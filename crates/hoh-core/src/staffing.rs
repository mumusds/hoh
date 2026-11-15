//! Staffing (REQ-020, docs/adr/ADR-030.md decisions 2-4 and 6): what the registry knows about each agent, whether an
//! agent can fill a role, which agent of a role's order to take, whether a session is still the agent's, and what a
//! role's binding should become. Pure: no clock, randomness or hash map, so the same input gives the same output
//! byte for byte. The store writes the judgments (`persistence::staffing`, decision 5).

#[cfg(test)]
mod tests;

use crate::protocol::BlockedReason;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// What an installed agent is: its launcher and the implementation the launcher runs, each by path and content hash
/// (decision 2, M2T15D-104). Always compared as a whole: the same hashes under another path are another identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub launcher_path: String,
    pub launcher_sha256: String,
    pub implementation_path: String,
    pub implementation_sha256: String,
}

impl AgentIdentity {
    /// The identity recorded as `value` (such as `team.session_bound.identity`); `None` when it is not an object with
    /// the four fields as strings: a record without a whole identity has none (decision 2).
    pub fn from_json(value: &Value) -> Option<AgentIdentity> {
        serde_json::from_value(value.clone()).ok()
    }
}

/// One file's identity stamp as `stat` (following links) reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    pub dev: u64,
    pub ino: u64,
    pub ctime_sec: i64,
    pub ctime_nsec: i64,
}

/// The stamps of a look's launcher and implementation: not part of the identity, only "is it still that file" for
/// prepare, the gate and the rechecks (decision 2, M2T15D-201).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub launcher: FileStamp,
    pub implementation: FileStamp,
}

/// Whether the compatibility suite verified an agent's identity (decision 9), with why when it did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compat {
    Verified,
    /// Not run for this identity yet: waits.
    Pending(String),
    /// The suite failed: permanent.
    Failed(String),
    /// The suite ran and could not decide; the same identity would decide the same: permanent.
    Undecided(String),
}

/// What the registry's last look found about one agent (decision 2). An agent missing from [`Facts`] was not looked
/// at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fact {
    /// `ProbeError::NotFound`.
    NotInstalled(String),
    /// `ProbeError::Unsupported`, or its session capabilities could not be read.
    Unsupported(String),
    /// A probe that timed out, did not start or failed, or a stamp that could not be read: temporary.
    ProbeFailed(String),
    /// The login check says it is logged out, or a turn found the authentication missing.
    LoginMissing(String),
    /// Boxed: it is large (clippy::large_enum_variant).
    Usable(Box<Usable>),
}

/// What a look found of an agent that runs (decision 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usable {
    /// The CLI's version line.
    pub version: String,
    pub identity: AgentIdentity,
    pub stamp: Stamp,
    /// The look that found it: `(host_epoch, n)`.
    pub generation: (i64, u64),
    /// The session capabilities a session granted now would record (`sessions.capabilities_json`).
    pub session: Value,
    /// `decision::granted_capabilities(session)`.
    pub granted: BTreeSet<String>,
    pub compat: Compat,
}

/// Each looked-at agent's fact, by agent name.
pub type Facts = BTreeMap<String, Fact>;

/// What the host's registry knew of the agents it was asked to look at, read at one moment under its lock (decision
/// 1): each one's fact and its last actual look. One tick's staffing, pace, supervision and allocation share it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub facts: Facts,
    pub observations: BTreeMap<String, crate::decision::Observation>,
    /// What each agent's last look found, as the evidence of a Block or a Switch records it (decision 5): `{class,
    /// detail, cli_version, launcher_path, implementation_sha256, help_sha256, schema_sha256, capabilities, compat,
    /// observation: {host_epoch, n}}`; an agent not looked at has none.
    pub probes: BTreeMap<String, Value>,
}

/// The capabilities every role needs, and those a role following `contract`'s role contract needs too (REQ-020
/// decision 1; docs/adr/ADR-022.md decision 10, docs/adr/ADR-030.md decision 12): structured events always, resume
/// for the commander and the reviewer, command execution (as the probe measures it) for the developer and qa.
/// Staffing judges a binding by them (decision 3) and the host's dispatch asks them of a session
/// (`host::agents::required`).
pub fn required(contract: Option<&str>) -> BTreeSet<String> {
    let mut needs: BTreeSet<String> = ["structured_events".to_owned()].into();
    match contract {
        Some("commander" | "reviewer") => {
            needs.insert("resume".to_owned());
        }
        Some("developer" | "qa") => {
            needs.insert("command_execution".to_owned());
        }
        _ => {}
    }
    needs
}

/// What a turn's failure excludes (decision 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Exclusion {
    ModelUnavailable,
    BlockedCapability,
}

/// A `team.binding_excluded` record: a turn of `agent` at `agent_version` and `identity` failed for `reason`. It
/// applies only while the agent's fact has that very version and identity (decision 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    pub agent: String,
    pub agent_version: String,
    pub identity: AgentIdentity,
    pub reason: Exclusion,
}

/// Whether a verdict ends the search for an agent (decision 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Usable,
    /// Temporary: the role waits for this agent; no fallback is evaluated.
    Waiting,
    /// The agent cannot fill the role as things are: the next agent of the order is evaluated.
    Permanent,
}

/// Whether one agent can fill a role (decision 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    NotLookedAt,
    NotInstalled,
    Unsupported,
    ProbeFailed,
    BlockedAuth,
    /// The suite failed or could not decide.
    CompatUnverified,
    /// A turn's failure excluded this version and identity.
    Excluded(Exclusion),
    /// The capabilities the role requires and the agent lacks, sorted.
    CapabilityUnsupported(Vec<String>),
    CompatPending,
    Usable,
}

impl Verdict {
    /// The verdict's code: `not_installed`, `BLOCKED_AUTH`, `CAPABILITY_UNSUPPORTED:resume`, ….
    pub fn code(&self) -> String {
        match self {
            Verdict::NotLookedAt => "not_looked_at".to_owned(),
            Verdict::NotInstalled => "not_installed".to_owned(),
            Verdict::Unsupported => "unsupported".to_owned(),
            Verdict::ProbeFailed => "probe_failed".to_owned(),
            Verdict::BlockedAuth => "BLOCKED_AUTH".to_owned(),
            Verdict::CompatUnverified => "COMPAT_UNVERIFIED".to_owned(),
            Verdict::Excluded(Exclusion::ModelUnavailable) => "MODEL_UNAVAILABLE".to_owned(),
            Verdict::Excluded(Exclusion::BlockedCapability) => "BLOCKED_CAPABILITY".to_owned(),
            Verdict::CapabilityUnsupported(missing) => {
                format!("CAPABILITY_UNSUPPORTED:{}", missing.join(","))
            }
            Verdict::CompatPending => "compat_pending".to_owned(),
            Verdict::Usable => "usable".to_owned(),
        }
    }

    /// Its line `<agent>:<code>`, as `evaluated` and `missing` list it.
    pub fn line(&self, agent: &str) -> String {
        format!("{agent}:{}", self.code())
    }

    /// The blocked reason it stands for; `None` for usable.
    pub fn reason(&self) -> Option<BlockedReason> {
        Some(match self {
            Verdict::NotLookedAt
            | Verdict::NotInstalled
            | Verdict::Unsupported
            | Verdict::ProbeFailed => BlockedReason::AgentUnavailable,
            Verdict::BlockedAuth => BlockedReason::BlockedAuth,
            Verdict::CompatUnverified | Verdict::CompatPending => BlockedReason::CompatUnverified,
            Verdict::Excluded(Exclusion::ModelUnavailable) => BlockedReason::ModelUnavailable,
            Verdict::Excluded(Exclusion::BlockedCapability) | Verdict::CapabilityUnsupported(_) => {
                BlockedReason::BlockedCapability
            }
            Verdict::Usable => return None,
        })
    }

    pub fn kind(&self) -> Kind {
        match self {
            Verdict::Usable => Kind::Usable,
            Verdict::NotLookedAt | Verdict::ProbeFailed | Verdict::CompatPending => Kind::Waiting,
            Verdict::NotInstalled
            | Verdict::Unsupported
            | Verdict::BlockedAuth
            | Verdict::CompatUnverified
            | Verdict::Excluded(_)
            | Verdict::CapabilityUnsupported(_) => Kind::Permanent,
        }
    }
}

/// Whether `agent` can fill a role that requires `required`, by the first rule that holds (decision 3).
pub fn verdict(
    agent: &str,
    required: &BTreeSet<String>,
    facts: &Facts,
    excluded: &[Excluded],
) -> Verdict {
    let Some(fact) = facts.get(agent) else {
        return Verdict::NotLookedAt;
    };
    let usable = match fact {
        Fact::NotInstalled(_) => return Verdict::NotInstalled,
        Fact::Unsupported(_) => return Verdict::Unsupported,
        Fact::ProbeFailed(_) => return Verdict::ProbeFailed,
        Fact::LoginMissing(_) => return Verdict::BlockedAuth,
        Fact::Usable(usable) => usable,
    };
    if matches!(usable.compat, Compat::Failed(_) | Compat::Undecided(_)) {
        return Verdict::CompatUnverified;
    }
    if let Some(record) = excluded.iter().find(|r| {
        r.agent == agent && r.agent_version == usable.version && r.identity == usable.identity
    }) {
        return Verdict::Excluded(record.reason);
    }
    let missing: Vec<String> = required.difference(&usable.granted).cloned().collect();
    if !missing.is_empty() {
        return Verdict::CapabilityUnsupported(missing);
    }
    match usable.compat {
        Compat::Pending(_) => Verdict::CompatPending,
        _ => Verdict::Usable,
    }
}

/// The agents of an order evaluated in turn (decision 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// The first usable agent, every one before it permanent.
    pub chosen: Option<String>,
    /// The first waiting agent, every one before it permanent.
    pub waiting_on: Option<String>,
    /// The line of each agent evaluated, in order.
    pub evaluated: Vec<String>,
}

/// Evaluate `order` until an agent is usable or waiting (decision 3); an agent not in `order` is never evaluated.
pub fn select(
    order: &[String],
    required: &BTreeSet<String>,
    facts: &Facts,
    excluded: &[Excluded],
) -> Selection {
    let mut selection = Selection {
        chosen: None,
        waiting_on: None,
        evaluated: Vec::new(),
    };
    for agent in order {
        let v = verdict(agent, required, facts, excluded);
        selection.evaluated.push(v.line(agent));
        match v.kind() {
            Kind::Usable => {
                selection.chosen = Some(agent.clone());
                break;
            }
            Kind::Waiting => {
                selection.waiting_on = Some(agent.clone());
                break;
            }
            Kind::Permanent => {}
        }
    }
    selection
}

/// A binding's session as the store recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// `sessions.agent_version`.
    pub agent_version: String,
    /// `sessions.capabilities_json`.
    pub capabilities: Value,
    /// The identity it was granted under (`team.session_bound.identity`); `None` when the record has none.
    pub identity: Option<AgentIdentity>,
}

/// Whether `session` is still the agent's as `fact` sees it: the same version, session capabilities and whole
/// identity (decision 4). A session without an identity, or a fact that is not usable, is never fresh.
pub fn fresh(session: &Session, fact: &Fact) -> bool {
    match fact {
        Fact::Usable(usable) => {
            session.agent_version == usable.version
                && session.capabilities == usable.session
                && session.identity.as_ref() == Some(&usable.identity)
        }
        _ => false,
    }
}

/// The vacancy set V (decision 7): the reasons a binding is BLOCKED for while its role waits for an agent; a session
/// granted ends them.
pub const VACANCY: [BlockedReason; 5] = [
    BlockedReason::AgentUnavailable,
    BlockedReason::BlockedAuth,
    BlockedReason::BlockedCapability,
    BlockedReason::ModelUnavailable,
    BlockedReason::CompatUnverified,
];

/// `role_bindings.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingState {
    Active,
    Blocked,
}

/// Why a change staffing wrote waits (decision 5): another agent of the order, or a new session of the same agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    Fallback,
    Rebind,
}

impl Cause {
    /// As `team.binding_change_pending.cause` and `team.binding_applied.cause` say it.
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Fallback => "fallback",
            Cause::Rebind => "rebind",
        }
    }
}

/// A switch or a rebind staffing wrote as pending while the role had an attempt in flight (decision 5): the epoch's
/// latest staffing event is its `team.binding_change_pending`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChange {
    pub cause: Cause,
    /// The agent the change binds.
    pub to: String,
}

/// One binding as staffing judges it (decision 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staffed {
    pub run_id: String,
    pub role: String,
    pub role_epoch: i64,
    /// The bound agent.
    pub agent: String,
    /// The bound model; `None`: the agent's own default.
    pub model: Option<String>,
    /// The agent and model the role's configuration names (its revision's).
    pub preferred: String,
    pub preferred_model: Option<String>,
    pub state: BindingState,
    pub blocked_reason: Option<BlockedReason>,
    /// `role_bindings.missing_json`: the lines its last block named.
    pub missing: Vec<String>,
    pub session: Option<Session>,
    /// The role has an attempt in flight (PREPARED, STARTING or RUNNING). It does not change the judgment; the store
    /// turns a switch or rebind into a pending change while it holds (decision 5).
    pub live: bool,
    /// The agent, then its fallbacks, in the order they are tried.
    pub order: Vec<String>,
    /// The capabilities the role requires.
    pub required: BTreeSet<String>,
    /// The exclusion records of the binding's run, role and revision (decision 6).
    pub excluded: Vec<Excluded>,
    /// The change the epoch's latest staffing event holds pending, if it is one (decision 5).
    pub pending: Option<PendingChange>,
}

/// What a binding should become (decision 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judgment {
    Nothing,
    /// Grant a session (BLOCKED becomes ACTIVE).
    Grant,
    Unblock,
    /// The same agent and model, the next epoch and a new session.
    Rebind {
        model: Option<String>,
    },
    Block {
        reason: BlockedReason,
        missing: Vec<String>,
    },
    /// Bind `to` with `model`; `evaluated` is the selection's lines, a prefix of the order.
    Switch {
        to: String,
        model: Option<String>,
        evaluated: Vec<String>,
    },
}

/// Decision 4's table for `staffed` under `facts` and `excluded`. A usable bound agent is kept (stickiness: no
/// selection is made); a permanent one is replaced by the first usable agent of the order from its head.
pub fn judge(staffed: &Staffed, facts: &Facts, excluded: &[Excluded]) -> Judgment {
    let c = &staffed.agent;
    let v = verdict(c, &staffed.required, facts, excluded);
    match v.kind() {
        Kind::Usable => match (&staffed.session, facts.get(c)) {
            (None, _) => Judgment::Grant,
            (Some(session), Some(fact)) if fresh(session, fact) => match staffed.state {
                BindingState::Blocked => Judgment::Unblock,
                BindingState::Active => Judgment::Nothing,
            },
            (Some(_), _) => Judgment::Rebind {
                model: staffed.model.clone(),
            },
        },
        Kind::Waiting => match v {
            Verdict::ProbeFailed => Judgment::Block {
                reason: BlockedReason::AgentUnavailable,
                missing: vec![v.line(c)],
            },
            _ => Judgment::Nothing,
        },
        Kind::Permanent => {
            let s = select(&staffed.order, &staffed.required, facts, excluded);
            if let Some(to) = s.chosen {
                let model = if to == staffed.preferred {
                    staffed.preferred_model.clone()
                } else {
                    None
                };
                return Judgment::Switch {
                    to,
                    model,
                    evaluated: s.evaluated,
                };
            }
            // At start-up the bound agent may be looked at before its candidates: an ACTIVE binding without a session
            // is not dispatched anyway, so it waits instead of being blocked and then switched.
            if s.waiting_on.is_some()
                && staffed.state == BindingState::Active
                && staffed.session.is_none()
            {
                return Judgment::Nothing;
            }
            match v.reason() {
                Some(reason) => Judgment::Block {
                    reason,
                    missing: s.evaluated,
                },
                // A permanent verdict always has a reason.
                None => Judgment::Nothing,
            }
        }
    }
}
