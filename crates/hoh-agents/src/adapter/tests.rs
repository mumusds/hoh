//! The gate's result channel as the adapters read it (docs/adr/ADR-030.md decision 9, "闸门的结果通道"): the command
//! line and go line of a checked gate, and the refusal its record holds for the attempt's key.

use super::*;
use crate::compat::Agent;
use crate::probe::Exec;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

fn stamp(ino: u64) -> FileStamp {
    FileStamp {
        dev: 2049,
        ino,
        ctime_sec: 1_700_000_000,
        ctime_nsec: 5,
    }
}

fn expect() -> Expect {
    Expect {
        agent: Agent::Codex,
        identity: AgentIdentity {
            launcher_path: "/opt/bin/codex".to_owned(),
            launcher_sha256: "a".repeat(64),
            implementation_path: "/opt/lib/codex/vendor/codex".to_owned(),
            implementation_sha256: "b".repeat(64),
        },
        stamp: Stamp {
            launcher: stamp(11),
            implementation: stamp(12),
        },
        plan: Exec::Fd,
    }
}

fn status(dir: &std::path::Path, key: &str) -> GateStatus {
    GateStatus {
        path: dir.join("gate.jsonl"),
        key: key.to_owned(),
    }
}

#[test]
fn a_checked_gate_is_told_what_to_expect_and_its_key_only_on_its_go_line() {
    // Decision 9: `__gate --expect <agent> <four identity fields> --stamp <launcher> <implementation> --plan <fd|path>
    // --status <path> --` before the agent's program; the key is nowhere on the command line, only in `go <key>`. A
    // gate without them is today's: its arguments as given and `go`.
    let gate = GateCommand {
        program: "/hoh".into(),
        args: vec!["__gate".into()],
        env: Vec::new(),
        expect: Some(expect()),
        status: Some(status(std::path::Path::new("/logs"), KEY)),
    };
    let argv: Vec<String> = gate
        .argv()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let a = "a".repeat(64);
    let b = "b".repeat(64);
    assert_eq!(
        argv,
        [
            "__gate",
            "--expect",
            "codex",
            "/opt/bin/codex",
            a.as_str(),
            "/opt/lib/codex/vendor/codex",
            b.as_str(),
            "--stamp",
            "2049:11:1700000000:5",
            "2049:12:1700000000:5",
            "--plan",
            "fd",
            "--status",
            "/logs/gate.jsonl",
            "--"
        ]
    );
    assert!(!argv.iter().any(|a| a.contains(KEY)));
    assert!(!format!("{gate:?}").contains(KEY), "{gate:?}");
    assert_eq!(gate.go(), format!("go {KEY}\n").into_bytes());
    assert_eq!(stamp_of_text("2049:12:1700000000:5"), Some(stamp(12)));
    for bad in ["", "1:2:3", "1:2:3:4:5", "a:2:3:4", "1:2:3:-"] {
        assert_eq!(stamp_of_text(bad), None, "{bad}");
    }
    let unchecked = GateCommand {
        expect: None,
        status: None,
        ..gate
    };
    assert_eq!(unchecked.argv(), vec![std::ffi::OsString::from("__gate")]);
    assert_eq!(unchecked.go(), b"go\n");
}

#[test]
fn gate_refusal_takes_the_last_refused_line_with_its_key() -> R {
    // Decision 9 ("适配器读"): of the gate's record, the refused lines with this attempt's key count, the last one
    // when there are more; lines of another key, a refusal without a key, verified and exec_failed lines and lines
    // that do not parse are ignored.
    let dir = tempfile::tempdir()?;
    let lines = [
        serde_json::json!({"outcome": "verified", "plan": "E1", "implementation": {"path": "/x"}}).to_string(),
        serde_json::json!({"outcome": "refused", "key": KEY, "code": "COMPAT_UNVERIFIED", "item": "launcher_sha256",
                           "stamp_only": false, "expected": "e1", "seen": "s1"})
        .to_string(),
        "not json".to_owned(),
        serde_json::json!({"outcome": "refused", "key": OTHER, "code": "COMPAT_UNVERIFIED", "item": "other",
                           "stamp_only": false, "expected": "e", "seen": "s"})
        .to_string(),
        serde_json::json!({"outcome": "refused", "key": KEY, "code": "COMPAT_UNVERIFIED", "item": "stamp",
                           "stamp_only": true, "expected": {"ino": 1}, "seen": {"ino": 2}})
        .to_string(),
        serde_json::json!({"outcome": "refused", "code": "COMPAT_UNVERIFIED", "item": "nokey",
                           "stamp_only": false, "expected": "e", "seen": "s"})
        .to_string(),
        serde_json::json!({"outcome": "exec_failed", "errno": 2}).to_string(),
    ];
    std::fs::write(dir.path().join("gate.jsonl"), lines.join("\n") + "\n")?;
    let refusal = gate_refusal(&status(dir.path(), KEY)).ok_or("no refusal")?;
    assert_eq!(
        (
            refusal.item(),
            refusal.stamp_only(),
            refusal.expected(),
            refusal.seen()
        ),
        (
            "stamp",
            true,
            &serde_json::json!({"ino": 1}),
            &serde_json::json!({"ino": 2})
        )
    );
    let other = gate_refusal(&status(dir.path(), OTHER)).ok_or("no refusal")?;
    assert_eq!((other.item(), other.stamp_only()), ("other", false));
    Ok(())
}

#[test]
fn gate_refusal_is_none_without_a_refused_line_of_its_key() -> R {
    // Only lines that are not the attempt's keyed refusal, or no record at all: none (the adapter's own rules judge
    // the turn's end). A copy of the verified line with its outcome changed has no key.
    let dir = tempfile::tempdir()?;
    assert_eq!(gate_refusal(&status(dir.path(), KEY)), None, "no file");
    let lines = [
        serde_json::json!({"outcome": "verified", "plan": "E2"}).to_string(),
        serde_json::json!({"outcome": "refused", "plan": "E2"}).to_string(),
        serde_json::json!({"outcome": "refused", "key": OTHER, "code": "COMPAT_UNVERIFIED", "item": "x"})
            .to_string(),
        serde_json::json!({"outcome": "refused", "key": KEY.to_uppercase(), "item": "x"}).to_string(),
        serde_json::json!({"outcome": "exec_failed", "errno": 13, "key": KEY}).to_string(),
        "{\"outcome\": \"refused\", \"key\": \"".to_owned() + KEY,
    ];
    std::fs::write(dir.path().join("gate.jsonl"), lines.join("\n"))?;
    assert_eq!(gate_refusal(&status(dir.path(), KEY)), None);
    Ok(())
}

#[test]
fn a_gate_refusal_is_a_typed_compat_failure_with_its_diagnostic() -> R {
    // Decisions 6 and 9: the facts of a turn its gate refused are FAILED COMPAT_UNVERIFIED with a typed CompatRefusal
    // from the gate (stamp_only as recorded) and an adapter.diagnostic{GATE_REFUSED, item, expected, seen}; only a
    // turn that wrote nothing on stdout is looked at, and a gate without a status never is.
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("gate.jsonl"),
        serde_json::json!({"outcome": "refused", "key": KEY, "code": "COMPAT_UNVERIFIED",
                           "item": "implementation_sha256", "stamp_only": false, "expected": "e", "seen": "s"})
        .to_string()
            + "\n",
    )?;
    let gate = GateCommand {
        program: "/hoh".into(),
        args: Vec::new(),
        env: Vec::new(),
        expect: Some(expect()),
        status: Some(status(dir.path(), KEY)),
    };
    let mut report = crate::adapter::replayed_exit();
    report.stdout.bytes = 0;
    let earlier = vec![Recorded::Event {
        key: "k".to_owned(),
        kind: "adapter.diagnostic".to_owned(),
        payload: serde_json::json!({"reason": "stdin failed"}),
    }];
    let facts = refused_at_gate(&gate, "/logs", Some(&report), &earlier).ok_or("not refused")?;
    assert_eq!(facts.state, AttemptState::Failed);
    assert_eq!(facts.failure.as_deref(), Some("COMPAT_UNVERIFIED"));
    let compat = facts.compat.ok_or("untyped")?;
    assert_eq!(
        (compat.origin(), compat.stamp_only()),
        (Origin::Gate, false)
    );
    assert_eq!(
        facts.recorded,
        vec![
            earlier[0].clone(),
            Recorded::Event {
                key: "gate-refused".to_owned(),
                kind: "adapter.diagnostic".to_owned(),
                payload: serde_json::json!({"reason": "GATE_REFUSED", "item": "implementation_sha256",
                                            "expected": "e", "seen": "s"}),
            }
        ]
    );
    let mut wrote = report.clone();
    wrote.stdout.bytes = 1;
    assert_eq!(refused_at_gate(&gate, "/logs", Some(&wrote), &[]), None);
    assert_eq!(refused_at_gate(&gate, "/logs", None, &[]), None);
    let unchecked = GateCommand {
        status: None,
        ..gate.clone()
    };
    assert_eq!(
        refused_at_gate(&unchecked, "/logs", Some(&report), &[]),
        None
    );
    let other = GateCommand {
        status: Some(status(dir.path(), OTHER)),
        ..gate
    };
    assert_eq!(refused_at_gate(&other, "/logs", Some(&report), &[]), None);
    Ok(())
}
