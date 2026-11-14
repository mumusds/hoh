//! Staffing against docs/adr/ADR-030.md: TEST-020-01, the property test of K1 and decision 4's table, and the
//! whole-identity comparison (M2T15D-104).

use super::*;
use crate::retry::SplitMix64;
use serde_json::json;

/// The version every agent of the property test reports.
const VERSION: &str = "1.0.0";
/// The capabilities a role may require, sorted; bit i of a mask is `CAPS[i]`.
const CAPS: [&str; 2] = ["resume", "structured_events"];
const ALL_CAPS: u8 = 0b11;
const NAMES: [&str; 4] = ["a", "b", "c", "d"];

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn mask_names(mask: u8) -> Vec<&'static str> {
    CAPS.iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, c)| *c)
        .collect()
}

/// The identity of every agent of the property test, so an exclusion record applies by its agent's name or not at all.
fn identity() -> AgentIdentity {
    AgentIdentity {
        launcher_path: "/opt/agent/bin/agent".to_owned(),
        launcher_sha256: "1".repeat(64),
        implementation_path: "/opt/agent/lib/1.0.0/agent".to_owned(),
        implementation_sha256: "2".repeat(64),
    }
}

/// `identity()` under another implementation path: the same hashes, another identity.
fn moved() -> AgentIdentity {
    AgentIdentity {
        implementation_path: "/opt/agent/lib/1.0.0-copy/agent".to_owned(),
        ..identity()
    }
}

fn stamp() -> Stamp {
    let launcher = FileStamp {
        dev: 64_769,
        ino: 1,
        ctime_sec: 1_800_000_000,
        ctime_nsec: 0,
    };
    Stamp {
        launcher,
        implementation: FileStamp { ino: 2, ..launcher },
    }
}

/// The session capabilities of an agent that grants `granted` of the known capabilities.
fn capabilities(granted: &[&str]) -> Value {
    Value::Object(
        CAPS.iter()
            .map(|c| ((*c).to_owned(), Value::Bool(granted.contains(c))))
            .collect(),
    )
}

fn usable(version: &str, identity: AgentIdentity, granted: &[&str], compat: Compat) -> Fact {
    let session = capabilities(granted);
    let granted = crate::decision::granted_capabilities(&session);
    Fact::Usable(Box::new(Usable {
        version: version.to_owned(),
        identity,
        stamp: stamp(),
        generation: (1, 1),
        session,
        granted,
        compat,
    }))
}

#[test]
fn test_020_01_the_same_probe_table_selects_claude_every_time() {
    // AC-020-01: the same probe table and order choose the same agent every time; codex, outside the order, is never
    // evaluated.
    let order = strings(&["pi", "X", "claude"]);
    let both = ["structured_events", "resume"];
    let required = set(&both);
    let at = |tag: &str| AgentIdentity {
        launcher_path: format!("/opt/{tag}/bin/{tag}"),
        launcher_sha256: "a".repeat(64),
        implementation_path: format!("/opt/{tag}/lib/{tag}"),
        implementation_sha256: "b".repeat(64),
    };
    let mut facts = Facts::new();
    facts.insert(
        "pi".to_owned(),
        Fact::NotInstalled("pi: command not found".to_owned()),
    );
    facts.insert(
        "X".to_owned(),
        usable("x 1.0.0", at("X"), &["structured_events"], Compat::Verified),
    );
    facts.insert(
        "claude".to_owned(),
        usable(
            "2.1.318 (Claude Code)",
            at("claude"),
            &both,
            Compat::Verified,
        ),
    );
    facts.insert(
        "codex".to_owned(),
        usable("codex-cli 0.166.0", at("codex"), &both, Compat::Verified),
    );
    let excluded: Vec<Excluded> = Vec::new();
    let input = json!({"order": order, "required": required, "facts": facts, "excluded": excluded});
    println!("input: {input}");

    let mut outputs = BTreeSet::new();
    for _ in 0..100 {
        let selection = select(&order, &required, &facts, &excluded);
        assert_eq!(selection.chosen.as_deref(), Some("claude"));
        assert_eq!(selection.waiting_on, None);
        assert_eq!(
            selection.evaluated,
            [
                "pi:not_installed",
                "X:CAPABILITY_UNSUPPORTED:resume",
                "claude:usable"
            ]
        );
        assert!(!selection.evaluated.iter().any(|l| l.starts_with("codex:")));
        outputs.insert(serde_json::to_string(&selection).unwrap());
    }
    assert_eq!(outputs.len(), 1, "{outputs:?}");
    println!("output: {}", outputs.first().unwrap());
}

#[test]
fn fresh_compares_the_whole_identity() {
    // M2T15D-104: a session is the agent's only under the same version, capabilities and whole identity.
    let fact = usable(VERSION, identity(), &CAPS, Compat::Verified);
    let session = |identity| Session {
        agent_version: VERSION.to_owned(),
        capabilities: capabilities(&CAPS),
        identity,
    };
    assert!(fresh(&session(Some(identity())), &fact));
    assert!(
        !fresh(&session(Some(moved())), &fact),
        "only the implementation path differs"
    );
    let launcher_moved = AgentIdentity {
        launcher_path: "/usr/local/bin/agent".to_owned(),
        ..identity()
    };
    assert!(
        !fresh(&session(Some(launcher_moved)), &fact),
        "only the launcher path differs"
    );
    assert!(!fresh(&session(None), &fact));

    // As `team.session_bound` records it: the whole identity parses; one field missing or not a string is none.
    let recorded = serde_json::to_value(identity()).unwrap();
    assert_eq!(AgentIdentity::from_json(&recorded), Some(identity()));
    assert!(fresh(&session(AgentIdentity::from_json(&recorded)), &fact));
    for field in [
        "launcher_path",
        "launcher_sha256",
        "implementation_path",
        "implementation_sha256",
    ] {
        let mut partial = recorded.clone();
        partial.as_object_mut().unwrap().remove(field);
        assert_eq!(AgentIdentity::from_json(&partial), None, "{field} missing");
        assert!(!fresh(&session(AgentIdentity::from_json(&partial)), &fact));
        let mut wrong = recorded.clone();
        wrong[field] = json!(1);
        assert_eq!(
            AgentIdentity::from_json(&wrong),
            None,
            "{field} not a string"
        );
    }
    assert_eq!(AgentIdentity::from_json(&Value::Null), None);

    // The version and the capabilities count as well; a fact that is not usable keeps no session fresh.
    let older = Session {
        agent_version: "0.9.0".to_owned(),
        ..session(Some(identity()))
    };
    assert!(!fresh(&older, &fact));
    let fewer = Session {
        capabilities: capabilities(&["structured_events"]),
        ..session(Some(identity()))
    };
    assert!(!fresh(&fewer, &fact));
    assert!(!fresh(
        &session(Some(identity())),
        &Fact::LoginMissing("logged out".to_owned())
    ));

    // An exclusion record compares the whole identity too: the same hashes under another path do not apply.
    let mut facts = Facts::new();
    facts.insert("codex".to_owned(), fact);
    let record = |identity| Excluded {
        agent: "codex".to_owned(),
        agent_version: VERSION.to_owned(),
        identity,
        reason: Exclusion::ModelUnavailable,
    };
    let required = set(&CAPS);
    assert_eq!(
        verdict("codex", &required, &facts, &[record(moved())]),
        Verdict::Usable
    );
    assert_eq!(
        verdict("codex", &required, &facts, &[record(identity())]),
        Verdict::Excluded(Exclusion::ModelUnavailable)
    );
}

// The property test: every fact variant and exclusion record, every order and every required set.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmp {
    Verified,
    Pending,
    Failed,
    Undecided,
}

impl Cmp {
    const ALL: [Cmp; 4] = [Cmp::Verified, Cmp::Pending, Cmp::Failed, Cmp::Undecided];

    fn compat(self) -> Compat {
        match self {
            Cmp::Verified => Compat::Verified,
            Cmp::Pending => Compat::Pending("no manifest entry".to_owned()),
            Cmp::Failed => Compat::Failed("case thread_start failed".to_owned()),
            Cmp::Undecided => Compat::Undecided("the schema is not exported".to_owned()),
        }
    }
}

/// What the registry saw of one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Absent,
    NotInstalled,
    Unsupported,
    ProbeFailed,
    LoginMissing,
    /// Its compat and the mask of `CAPS` it grants.
    Usable(Cmp, u8),
}

/// The exclusion record naming one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Record {
    None,
    Model,
    Capability,
    /// A model failure of the same version under `moved()`: does not apply.
    ModelMoved,
    /// A capability failure of an older version: does not apply.
    CapabilityOlder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Case {
    seen: Seen,
    record: Record,
}

const fn case(seen: Seen, record: Record) -> Case {
    Case { seen, record }
}

const ABSENT: Case = case(Seen::Absent, Record::None);

impl Case {
    /// Every fact variant (each compat value and granted mask) with every record: 105 cases.
    fn all() -> Vec<Case> {
        let mut seen = vec![
            Seen::Absent,
            Seen::NotInstalled,
            Seen::Unsupported,
            Seen::ProbeFailed,
            Seen::LoginMissing,
        ];
        for cmp in Cmp::ALL {
            for mask in 0..=ALL_CAPS {
                seen.push(Seen::Usable(cmp, mask));
            }
        }
        let records = [
            Record::None,
            Record::Model,
            Record::Capability,
            Record::ModelMoved,
            Record::CapabilityOlder,
        ];
        seen.iter()
            .flat_map(|s| records.iter().map(|r| case(*s, *r)))
            .collect()
    }

    /// One case of each outcome a selection tells apart, every fact variant and compat value among them.
    fn representatives() -> Vec<Case> {
        vec![
            ABSENT,
            case(Seen::NotInstalled, Record::None),
            case(Seen::Unsupported, Record::None),
            case(Seen::ProbeFailed, Record::None),
            case(Seen::LoginMissing, Record::None),
            case(Seen::Usable(Cmp::Verified, ALL_CAPS), Record::None),
            case(Seen::Usable(Cmp::Pending, ALL_CAPS), Record::None),
            case(Seen::Usable(Cmp::Failed, ALL_CAPS), Record::None),
            case(Seen::Usable(Cmp::Undecided, ALL_CAPS), Record::None),
            case(Seen::Usable(Cmp::Verified, 0b10), Record::None),
            case(Seen::Usable(Cmp::Verified, ALL_CAPS), Record::Model),
            case(Seen::Usable(Cmp::Verified, ALL_CAPS), Record::Capability),
            case(Seen::Usable(Cmp::Verified, ALL_CAPS), Record::ModelMoved),
            case(Seen::Usable(Cmp::Pending, 0b01), Record::CapabilityOlder),
        ]
    }
}

/// The facts and exclusion records of agents with these cases.
fn world(cases: &BTreeMap<String, Case>) -> (Facts, Vec<Excluded>) {
    let mut facts = Facts::new();
    let mut excluded = Vec::new();
    for (agent, case) in cases {
        let fact = match case.seen {
            Seen::Absent => None,
            Seen::NotInstalled => Some(Fact::NotInstalled(format!("{agent}: not found"))),
            Seen::Unsupported => Some(Fact::Unsupported(format!("{agent}: unsupported"))),
            Seen::ProbeFailed => Some(Fact::ProbeFailed(format!("{agent}: timed out"))),
            Seen::LoginMissing => Some(Fact::LoginMissing(format!("{agent}: logged out"))),
            Seen::Usable(cmp, mask) => {
                Some(usable(VERSION, identity(), &mask_names(mask), cmp.compat()))
            }
        };
        if let Some(fact) = fact {
            facts.insert(agent.clone(), fact);
        }
        let record = |agent_version: &str, identity, reason| Excluded {
            agent: agent.clone(),
            agent_version: agent_version.to_owned(),
            identity,
            reason,
        };
        match case.record {
            Record::None => {}
            Record::Model => {
                excluded.push(record(VERSION, identity(), Exclusion::ModelUnavailable))
            }
            Record::Capability => {
                excluded.push(record(VERSION, identity(), Exclusion::BlockedCapability));
            }
            Record::ModelMoved => {
                excluded.push(record(VERSION, moved(), Exclusion::ModelUnavailable))
            }
            Record::CapabilityOlder => {
                excluded.push(record("0.9.0", identity(), Exclusion::BlockedCapability));
            }
        }
    }
    (facts, excluded)
}

/// Decision 3's rules restated over a case: the code, kind and blocked reason its verdict must have.
fn expected(case: Case, required: u8) -> (String, Kind, Option<BlockedReason>) {
    use BlockedReason as R;
    let unavailable = |code: &str, kind| (code.to_owned(), kind, Some(R::AgentUnavailable));
    let (cmp, granted) = match case.seen {
        Seen::Absent => return unavailable("not_looked_at", Kind::Waiting),
        Seen::NotInstalled => return unavailable("not_installed", Kind::Permanent),
        Seen::Unsupported => return unavailable("unsupported", Kind::Permanent),
        Seen::ProbeFailed => return unavailable("probe_failed", Kind::Waiting),
        Seen::LoginMissing => {
            return (
                "BLOCKED_AUTH".to_owned(),
                Kind::Permanent,
                Some(R::BlockedAuth),
            );
        }
        Seen::Usable(cmp, granted) => (cmp, granted),
    };
    if matches!(cmp, Cmp::Failed | Cmp::Undecided) {
        return (
            "COMPAT_UNVERIFIED".to_owned(),
            Kind::Permanent,
            Some(R::CompatUnverified),
        );
    }
    match case.record {
        Record::Model => {
            return (
                "MODEL_UNAVAILABLE".to_owned(),
                Kind::Permanent,
                Some(R::ModelUnavailable),
            );
        }
        Record::Capability => {
            return (
                "BLOCKED_CAPABILITY".to_owned(),
                Kind::Permanent,
                Some(R::BlockedCapability),
            );
        }
        Record::None | Record::ModelMoved | Record::CapabilityOlder => {}
    }
    let missing = mask_names(required & !granted);
    if !missing.is_empty() {
        return (
            format!("CAPABILITY_UNSUPPORTED:{}", missing.join(",")),
            Kind::Permanent,
            Some(R::BlockedCapability),
        );
    }
    if cmp == Cmp::Pending {
        return (
            "compat_pending".to_owned(),
            Kind::Waiting,
            Some(R::CompatUnverified),
        );
    }
    ("usable".to_owned(), Kind::Usable, None)
}

/// Decision 3's selection restated over cases.
fn expected_selection(order: &[String], cases: &BTreeMap<String, Case>, required: u8) -> Selection {
    let mut evaluated = Vec::new();
    for agent in order {
        let (code, kind, _) = expected(cases.get(agent).copied().unwrap_or(ABSENT), required);
        evaluated.push(format!("{agent}:{code}"));
        let found = Some(agent.clone());
        match kind {
            Kind::Usable => {
                return Selection {
                    chosen: found,
                    waiting_on: None,
                    evaluated,
                };
            }
            Kind::Waiting => {
                return Selection {
                    chosen: None,
                    waiting_on: found,
                    evaluated,
                };
            }
            Kind::Permanent => {}
        }
    }
    Selection {
        chosen: None,
        waiting_on: None,
        evaluated,
    }
}

/// Every order over `NAMES`: the permutations of every subset, 65.
fn orders() -> Vec<Vec<String>> {
    let mut all = vec![Vec::new()];
    let mut shorter: Vec<Vec<String>> = vec![Vec::new()];
    for _ in NAMES {
        let mut longer = Vec::new();
        for order in &shorter {
            for name in NAMES {
                if !order.iter().any(|n| n == name) {
                    let mut next = order.clone();
                    next.push(name.to_owned());
                    longer.push(next);
                }
            }
        }
        all.extend(longer.iter().cloned());
        shorter = longer;
    }
    all
}

fn required_set(mask: u8) -> BTreeSet<String> {
    set(&mask_names(mask))
}

/// K1 for one input: the same selection byte for byte each time; the first usable agent of the order with every one
/// before it permanent; `evaluated` a prefix of the order, its lines the verdicts' lines.
fn check_k1(order: &[String], required: u8, cases: &BTreeMap<String, Case>) {
    let (facts, excluded) = world(cases);
    let wanted = required_set(required);
    let selection = select(order, &wanted, &facts, &excluded);
    let again = select(order, &wanted, &facts, &excluded);
    assert_eq!(
        serde_json::to_vec(&selection).unwrap(),
        serde_json::to_vec(&again).unwrap()
    );
    let context =
        || format!("order {order:?}, required {required:#b}, cases {cases:?}: {selection:?}");
    let n = selection.evaluated.len();
    assert!(n <= order.len(), "{}", context());
    for (line, agent) in selection.evaluated.iter().zip(order) {
        assert!(line.starts_with(&format!("{agent}:")), "{}", context());
    }
    let kinds: Vec<Kind> = order[..n]
        .iter()
        .map(|a| expected(cases.get(a).copied().unwrap_or(ABSENT), required).1)
        .collect();
    let before_last_permanent = kinds.iter().rev().skip(1).all(|k| *k == Kind::Permanent);
    match (&selection.chosen, &selection.waiting_on) {
        (Some(chosen), None) => {
            assert_eq!(Some(chosen), order[..n].last(), "{}", context());
            assert_eq!(kinds.last(), Some(&Kind::Usable), "{}", context());
            assert!(before_last_permanent, "{}", context());
        }
        (None, Some(waiting)) => {
            assert_eq!(Some(waiting), order[..n].last(), "{}", context());
            assert_eq!(kinds.last(), Some(&Kind::Waiting), "{}", context());
            assert!(before_last_permanent, "{}", context());
        }
        (None, None) => {
            assert_eq!(n, order.len(), "{}", context());
            assert!(kinds.iter().all(|k| *k == Kind::Permanent), "{}", context());
        }
        (Some(_), Some(_)) => panic!("both chosen and waiting: {}", context()),
    }
    assert_eq!(
        selection,
        expected_selection(order, cases, required),
        "{}",
        context()
    );
}

#[test]
fn a_verdict_is_the_first_rule_that_holds() {
    // Decision 3's ordered rules, its codes and reasons, for every fact variant and record; the other agents' records
    // carry the same version and identity and still never apply to this one.
    let all = Case::all();
    assert_eq!(all.len(), 105);
    for (i, this) in all.iter().enumerate() {
        let cases: BTreeMap<String, Case> = [
            ("a", *this),
            ("b", all[(i + 1) % all.len()]),
            ("c", all[(i * 7 + 3) % all.len()]),
            ("d", all[(i * 31 + 5) % all.len()]),
        ]
        .into_iter()
        .map(|(n, c)| (n.to_owned(), c))
        .collect();
        let (facts, excluded) = world(&cases);
        for required in 0..=ALL_CAPS {
            for (agent, case) in &cases {
                let v = verdict(agent, &required_set(required), &facts, &excluded);
                let (code, kind, reason) = expected(*case, required);
                assert_eq!(
                    (v.code(), v.kind(), v.reason()),
                    (code.clone(), kind, reason),
                    "{agent} {case:?} required {required:#b}"
                );
                assert_eq!(v.line(agent), format!("{agent}:{code}"));
            }
        }
    }
}

#[test]
fn select_takes_the_first_usable_agent_of_its_order() {
    // K1 over every order and required set: each order position takes every representative case (the orders of a
    // length taken in turn), the agents outside the order are usable or waiting, so evaluating one would show; then
    // whole tables drawn from all 105 cases per agent against all 65 orders.
    let representatives = Case::representatives();
    let outside = [
        case(Seen::Usable(Cmp::Verified, ALL_CAPS), Record::None),
        case(Seen::ProbeFailed, Record::None),
        case(Seen::Usable(Cmp::Pending, ALL_CAPS), Record::None),
    ];
    let orders = orders();
    assert_eq!(orders.len(), 65);
    let mut covered = BTreeSet::new();
    for len in 0..=NAMES.len() {
        let of_len: Vec<&Vec<String>> = orders.iter().filter(|o| o.len() == len).collect();
        let patterns = representatives.len().pow(u32::try_from(len).unwrap());
        for pattern in 0..patterns {
            let order = of_len[pattern % of_len.len()];
            covered.insert(order.clone());
            let mut cases = BTreeMap::new();
            let mut digits = pattern;
            for agent in order {
                cases.insert(
                    agent.clone(),
                    representatives[digits % representatives.len()],
                );
                digits /= representatives.len();
            }
            for (i, name) in NAMES.iter().enumerate() {
                cases
                    .entry((*name).to_owned())
                    .or_insert(outside[(pattern + i) % outside.len()]);
            }
            for required in 0..=ALL_CAPS {
                check_k1(order, required, &cases);
            }
        }
    }
    assert_eq!(covered.len(), orders.len());

    let all = Case::all();
    let mut draw = SplitMix64::new(0x0020_0001);
    for _ in 0..200 {
        let cases: BTreeMap<String, Case> = NAMES
            .iter()
            .map(|n| {
                let i = usize::try_from(draw.next_u64() % all.len() as u64).unwrap();
                ((*n).to_owned(), all[i])
            })
            .collect();
        for order in &orders {
            for required in 0..=ALL_CAPS {
                check_k1(order, required, &cases);
            }
        }
    }
}

/// The bound session against the bound agent's fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    None,
    Fresh,
    VersionStale,
    CapabilityStale,
    IdentityStale,
    NoIdentity,
}

impl Held {
    const ALL: [Held; 6] = [
        Held::None,
        Held::Fresh,
        Held::VersionStale,
        Held::CapabilityStale,
        Held::IdentityStale,
        Held::NoIdentity,
    ];

    /// The session, made against the fact a case has (a fact that is not usable is taken as granting every capability).
    fn session(self, case: Case) -> Option<Session> {
        let granted = match case.seen {
            Seen::Usable(_, mask) => mask,
            _ => ALL_CAPS,
        };
        let caps = capabilities(&mask_names(granted));
        let session = |version: &str, capabilities, identity| {
            Some(Session {
                agent_version: version.to_owned(),
                capabilities,
                identity,
            })
        };
        match self {
            Held::None => None,
            Held::Fresh => session(VERSION, caps, Some(identity())),
            Held::VersionStale => session("0.9.0", caps, Some(identity())),
            Held::CapabilityStale => {
                let mut more = caps;
                more["command_execution"] = json!(true);
                session(VERSION, more, Some(identity()))
            }
            Held::IdentityStale => session(VERSION, caps, Some(moved())),
            Held::NoIdentity => session(VERSION, caps, None),
        }
    }
}

/// Decision 4's table restated: what `staffed` (bound to an agent with `cases[agent]`, its session `held`) should
/// become.
fn expected_judgment(staffed: &Staffed, held: Held, cases: &BTreeMap<String, Case>) -> Judgment {
    let case = cases.get(&staffed.agent).copied().unwrap_or(ABSENT);
    let (code, kind, reason) = expected(case, ALL_CAPS);
    match kind {
        Kind::Usable => match held {
            Held::None => Judgment::Grant,
            Held::Fresh if staffed.state == BindingState::Blocked => Judgment::Unblock,
            Held::Fresh => Judgment::Nothing,
            Held::VersionStale | Held::CapabilityStale | Held::IdentityStale | Held::NoIdentity => {
                Judgment::Rebind {
                    model: staffed.model.clone(),
                }
            }
        },
        Kind::Waiting if code == "probe_failed" => Judgment::Block {
            reason: BlockedReason::AgentUnavailable,
            missing: vec![format!("{}:probe_failed", staffed.agent)],
        },
        Kind::Waiting => Judgment::Nothing,
        Kind::Permanent => {
            let s = expected_selection(&staffed.order, cases, ALL_CAPS);
            if let Some(to) = s.chosen {
                let model = if to == staffed.preferred {
                    staffed.preferred_model.clone()
                } else {
                    None
                };
                Judgment::Switch {
                    to,
                    model,
                    evaluated: s.evaluated,
                }
            } else if s.waiting_on.is_some()
                && staffed.state == BindingState::Active
                && held == Held::None
            {
                Judgment::Nothing
            } else {
                Judgment::Block {
                    reason: reason.unwrap(),
                    missing: s.evaluated,
                }
            }
        }
    }
}

#[test]
fn judge_follows_decision_four() {
    // Decision 4's table over (the bound agent's verdict × its session: none, fresh, version-, capability- or
    // identity-stale, no identity × state × live × the selection), the bound agent "a" in every order that holds it,
    // the other agents usable, waiting or permanent; with stickiness (a usable agent is never switched away from) and
    // the model rule (the configured model only for the configured agent). Undecided falls back as Failed does.
    let others = [
        ABSENT,
        case(Seen::LoginMissing, Record::None),
        case(Seen::Usable(Cmp::Verified, ALL_CAPS), Record::None),
        case(Seen::Usable(Cmp::Pending, ALL_CAPS), Record::None),
    ];
    let states = [
        (BindingState::Active, None),
        (BindingState::Blocked, Some(BlockedReason::AgentUnavailable)),
    ];
    let mut seen = BTreeSet::new();
    let orders: Vec<Vec<String>> = orders()
        .into_iter()
        .filter(|o| o.len() <= 3 && o.iter().any(|n| n == "a"))
        .collect();
    for (o, order) in orders.iter().enumerate() {
        let rest: Vec<&String> = order.iter().filter(|n| *n != "a").collect();
        let patterns = others.len().pow(u32::try_from(rest.len()).unwrap());
        for pattern in 0..patterns {
            let mut digits = pattern;
            let mut placed = BTreeMap::new();
            for agent in &rest {
                placed.insert((*agent).clone(), others[digits % others.len()]);
                digits /= others.len();
            }
            // The configured agent is the order's head, or (a hand-written row put first) its second.
            let preferred = order[(o + pattern) % order.len().min(2)].clone();
            for bound in Case::representatives() {
                let mut cases = placed.clone();
                cases.insert("a".to_owned(), bound);
                let (facts, excluded) = world(&cases);
                for held in Held::ALL {
                    for (state, blocked_reason) in states {
                        let mut judged = Vec::new();
                        for live in [false, true] {
                            let staffed = Staffed {
                                agent: "a".to_owned(),
                                model: Some("bound-model".to_owned()),
                                preferred: preferred.clone(),
                                preferred_model: Some("configured-model".to_owned()),
                                state,
                                blocked_reason,
                                session: held.session(bound),
                                live,
                                order: order.clone(),
                                required: required_set(ALL_CAPS),
                            };
                            let judgment = judge(&staffed, &facts, &excluded);
                            assert_eq!(
                                judgment,
                                expected_judgment(&staffed, held, &cases),
                                "{staffed:?} cases {cases:?}"
                            );
                            let bound_kind = expected(bound, ALL_CAPS).1;
                            if bound_kind == Kind::Usable {
                                assert!(
                                    !matches!(judgment, Judgment::Switch { .. }),
                                    "a usable bound agent is kept"
                                );
                            }
                            if let Seen::Usable(Cmp::Undecided, mask) = bound.seen {
                                let mut failed = cases.clone();
                                failed.insert(
                                    "a".to_owned(),
                                    case(Seen::Usable(Cmp::Failed, mask), bound.record),
                                );
                                let (facts, excluded) = world(&failed);
                                assert_eq!(judge(&staffed, &facts, &excluded), judgment);
                            }
                            seen.insert(match &judgment {
                                Judgment::Nothing if bound_kind == Kind::Permanent => {
                                    "nothing while a candidate waits".to_owned()
                                }
                                Judgment::Nothing => "nothing".to_owned(),
                                Judgment::Grant => "grant".to_owned(),
                                Judgment::Unblock => "unblock".to_owned(),
                                Judgment::Rebind { .. } => "rebind".to_owned(),
                                Judgment::Block { .. } if bound_kind == Kind::Waiting => {
                                    "block on probe_failed".to_owned()
                                }
                                Judgment::Block { .. } => "block".to_owned(),
                                Judgment::Switch { to, model, .. } => {
                                    let to = if *to == preferred {
                                        "configured"
                                    } else {
                                        "other"
                                    };
                                    format!("switch to the {to} agent with {model:?}")
                                }
                            });
                            judged.push(judgment);
                        }
                        assert_eq!(judged[0], judged[1], "live does not change the judgment");
                    }
                }
            }
        }
    }
    // Every row of the table was reached, and both sides of the model rule.
    for row in [
        "nothing",
        "nothing while a candidate waits",
        "grant",
        "unblock",
        "rebind",
        "block on probe_failed",
        "block",
        "switch to the configured agent with Some(\"configured-model\")",
        "switch to the other agent with None",
    ] {
        assert!(seen.contains(row), "{row} never judged: {seen:?}");
    }
}
