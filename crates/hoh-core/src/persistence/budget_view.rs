//! DC-T10: one transactional snapshot, one admission rule for dispatch, pace, recovery and supervision.
use super::{PersistError, sqlite_error};
use crate::budget::{Budget, Ledger, Stop};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub(super) enum Denied {
    Limit(Stop),
    Unenforceable(String),
    Consult(String),
}
impl Denied {
    pub(super) fn limit(&self) -> Option<Stop> {
        match self {
            Self::Limit(stop) => Some(*stop),
            _ => None,
        }
    }
}
impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Limit(stop) => f.write_str(stop.as_str()),
            Self::Unenforceable(reason) | Self::Consult(reason) => f.write_str(reason),
        }
    }
}

#[derive(Debug, Clone)]
struct Amount {
    ledger: Ledger,
    known: bool,
    usage_known: bool,
}
impl Amount {
    fn exhausted(&self, kind: &str, protected: f64) -> bool {
        let committed = self.ledger.used + self.ledger.reserved + protected;
        // An allocation creates exactly one attempt. Other kinds have no per-dispatch estimate in the ledger;
        // cost_estimate is an ordinal scheduler score, not seconds, tokens or dollars.
        let estimate = f64::from(kind == "attempts");
        self.ledger
            .limit
            .is_some_and(|limit| committed >= limit || committed + estimate > limit)
    }
}
#[derive(Debug)]
struct Hold {
    allocated: f64,
    consumed: Option<f64>,
    // Wall consumption already transfers out of the stored reservation; the other kinds settle on finish.
    transferred: f64,
}
impl Hold {
    fn unused(&self) -> f64 {
        self.consumed
            .map_or(self.allocated, |n| (self.allocated - n).max(0.0))
    }
}
#[derive(Debug)]
struct Consult {
    deadline: SystemTime,
    unfinished: bool,
    open: bool,
    amounts: BTreeMap<String, Amount>,
    holds: BTreeMap<String, Hold>,
}

pub(super) struct View {
    deadline: Option<SystemTime>,
    wall: SystemTime,
    // Only non-consult reservations here. Each consult's allocation and consumption remain separate below.
    amounts: BTreeMap<String, Amount>,
    consults: BTreeMap<String, Consult>,
}
impl View {
    fn unused(&self, kind: &str) -> f64 {
        self.consults
            .values()
            .filter(|c| c.open)
            .filter_map(|c| c.holds.get(kind))
            .map(Hold::unused)
            .sum()
    }
    /// A pure rule: consult holds protect against ordinary work, never against another consult.
    fn run_stop(&self, consult: bool) -> Option<Denied> {
        if self.deadline.is_some_and(|at| self.wall >= at) {
            return Some(Denied::Limit(Stop::Deadline));
        }
        for (kind, stop) in [
            ("iterations", Stop::Iterations),
            ("wall_seconds", Stop::Duration),
            ("usd", Stop::Cost),
            ("attempts", Stop::Attempts),
            ("tokens", Stop::Tokens),
        ] {
            let Some(amount) = self.amounts.get(kind) else {
                continue;
            };
            if amount.ledger.limit.is_none() {
                continue;
            }
            if kind == "usd" && !amount.known {
                return Some(Denied::Unenforceable(
                    "max_usd with an unknown price".into(),
                ));
            }
            if !amount.usage_known {
                return Some(Denied::Unenforceable(format!("{kind} with unknown usage")));
            }
            if amount.exhausted(kind, if consult { 0.0 } else { self.unused(kind) }) {
                return Some(Denied::Limit(stop));
            }
        }
        None
    }
    pub(super) fn consult_stop(&self, task: &str) -> Option<String> {
        let c = self.consults.get(task)?;
        if self.wall >= c.deadline {
            return Some("consult_deadline".into());
        }
        for (kind, amount) in &c.amounts {
            if amount.ledger.limit.is_none() {
                continue;
            }
            if kind == "usd" && !amount.known {
                return Some("consult_usd_price_unknown".into());
            }
            if !amount.usage_known {
                return Some(format!("consult_{kind}_usage_unknown"));
            }
            if amount.exhausted(kind, 0.0) {
                return Some(format!("consult_{kind}"));
            }
        }
        None
    }
    pub(super) fn task_stop(&self, task: &str) -> Option<Denied> {
        self.run_stop(self.consults.get(task).is_some_and(|c| c.open))
            .or_else(|| self.consult_stop(task).map(Denied::Consult))
    }
    /// The same answer for pace, resume, stall (e), wake allocation and wake availability.
    /// No task readiness/lease policy is inferred here: those remain independent admission gates.
    pub(super) fn any_stop(&self) -> Option<Denied> {
        let stop = self.run_stop(false)?;
        if self
            .consults
            .iter()
            .any(|(task, c)| c.open && c.unfinished && self.task_stop(task).is_none())
        {
            None
        } else {
            Some(stop)
        }
    }
    /// Budget-risk reporting uses the same snapshot with all unused holds included.
    pub(super) fn risk_budget(&self) -> Budget {
        let ledger = |kind: &str| {
            self.amounts.get(kind).map(|a| Ledger {
                reserved: a.ledger.reserved + self.unused(kind),
                ..a.ledger
            })
        };
        Budget {
            deadline_at: self.deadline,
            iterations: ledger("iterations"),
            wall_seconds: ledger("wall_seconds"),
            usd: ledger("usd").map(|l| (l, self.amounts["usd"].known)),
        }
    }
}

fn rows(
    tx: &Transaction<'_>,
    scope: &str,
    id: &str,
) -> Result<BTreeMap<String, Amount>, PersistError> {
    let mut q = tx.prepare("SELECT kind,limit_value,used_value,reserved_value,price_known FROM budgets WHERE scope_kind=?1 AND scope_id=?2 ORDER BY kind").map_err(sqlite_error)?;
    q.query_map(params![scope, id], |r| {
        Ok((
            r.get(0)?,
            Amount {
                ledger: Ledger {
                    limit: r.get(1)?,
                    used: r.get(2)?,
                    reserved: r.get(3)?,
                },
                known: r.get(4)?,
                usage_known: true,
            },
        ))
    })
    .map_err(sqlite_error)?
    .collect::<Result<_, _>>()
    .map_err(sqlite_error)
}

/// Read-only snapshot in the caller's transaction. No gate may credit/release holds in the database.
pub(super) fn load(
    tx: &Transaction<'_>,
    run: &str,
    now: SystemTime,
    wall: SystemTime,
) -> Result<View, PersistError> {
    let deadline: Option<String> = tx
        .query_row(
            "SELECT deadline_at FROM runs WHERE run_id=?1",
            params![run],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    let parse = |text: &str| {
        crate::time::parse_rfc3339(text).ok_or_else(|| {
            PersistError::IntegrityViolation(format!("invalid budget deadline: {text}"))
        })
    };
    let mut view = View {
        deadline: deadline.as_deref().map(parse).transpose()?,
        wall,
        amounts: rows(tx, "run", run)?,
        consults: BTreeMap::new(),
    };
    let mut q = tx.prepare("SELECT e.payload_json,t.state,
        NOT EXISTS(SELECT 1 FROM events d WHERE d.run_id=e.run_id AND d.kind='task.consult_finished' AND json_extract(d.payload_json,'$.task_id')=t.task_id)
        FROM events e JOIN tasks t ON t.task_id=json_extract(e.payload_json,'$.task_id') AND t.run_id=e.run_id
        WHERE e.run_id=?1 AND e.kind='task.consult' ORDER BY e.seq").map_err(sqlite_error)?;
    for row in q
        .query_map(params![run], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })
        .map_err(sqlite_error)?
    {
        let (meta, state, open) = row.map_err(sqlite_error)?;
        let meta: Value = serde_json::from_str(&meta)
            .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
        let task = meta["task_id"]
            .as_str()
            .ok_or_else(|| PersistError::IntegrityViolation("consult task missing".into()))?;
        let mut amounts = rows(tx, "task", task)?;
        let mut holds = BTreeMap::new();
        for (kind, amount) in &mut amounts {
            let persisted = amount.ledger.used;
            let used = super::consult::consumed(tx, task, kind, persisted, now)?;
            amount.usage_known = used.is_some();
            amount.ledger.used = used.unwrap_or(persisted);
            if let Some(allocated) = meta["reserved_budget"][kind].as_f64() {
                holds.insert(
                    kind.clone(),
                    Hold {
                        allocated,
                        consumed: used,
                        transferred: if kind == "wall_seconds" {
                            persisted
                        } else {
                            0.0
                        },
                    },
                );
            }
        }
        view.consults.insert(
            task.into(),
            Consult {
                deadline: parse(meta["deadline"].as_str().ok_or_else(|| {
                    PersistError::IntegrityViolation("consult deadline missing".into())
                })?)?,
                // A retry or recoverable staffing block is not a budget stop. Ended children awaiting
                // settlement still own their holds, but cannot be a way forward themselves.
                unfinished: !matches!(state.as_str(), "IMPLEMENTED" | "ACCEPTED" | "CANCELLED"),
                open,
                amounts,
                holds,
            },
        );
    }
    // Split the stored reservation into ordinary reservations and per-task holds before applying any policy.
    for (kind, amount) in &mut view.amounts {
        let mut held = 0.0;
        for c in view.consults.values() {
            if c.open
                && let Some(h) = c.holds.get(kind)
            {
                held += (h.allocated - h.transferred).max(0.0);
            }
            // Only duration and candidate iterations transfer into the Run row. The other kinds are read
            // from task/attempt consumption, including finished consults: releasing a hold never refunds usage.
            if matches!(kind.as_str(), "attempts" | "tokens" | "usd")
                && let Some(consumed) = c.amounts.get(kind)
            {
                amount.usage_known &= consumed.usage_known;
                amount.ledger.used += consumed.ledger.used;
            }
        }
        amount.ledger.reserved = (amount.ledger.reserved - held).max(0.0);
    }
    if let Some(amount) = view.amounts.get_mut("wall_seconds") {
        let mut q = tx.prepare("SELECT a.attempt_id,a.started_at FROM attempts a JOIN tasks t USING(task_id) WHERE t.run_id=?1 AND a.state IN ('STARTING','RUNNING') AND a.started_at IS NOT NULL").map_err(sqlite_error)?;
        for row in q
            .query_map(params![run], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sqlite_error)?
        {
            let (attempt, started) = row.map_err(sqlite_error)?;
            if let Some(started) = crate::time::parse_rfc3339(&started) {
                amount.ledger.used +=
                    super::retry::ran(started, now, super::retry::observed_end(tx, &attempt)?)
                        as f64;
            }
        }
    }
    Ok(view)
}
