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

/// An installation like claude's in `dir`: `versions/A` and `versions/B` (another content), the launcher `bin/agent` a
/// link to `versions/A`, and a `gate.jsonl` beside them; the expectation of its look now.
fn installed(dir: &std::path::Path) -> R<Expect> {
    std::fs::create_dir_all(dir.join("versions"))?;
    std::fs::create_dir_all(dir.join("bin"))?;
    std::fs::write(dir.join("versions/A"), "#!/bin/sh\necho A\n")?;
    std::fs::write(dir.join("versions/B"), "#!/bin/sh\necho B\n")?;
    std::os::unix::fs::symlink(dir.join("versions/A"), dir.join("bin/agent"))?;
    std::fs::write(
        dir.join("gate.jsonl"),
        serde_json::json!({"outcome": "verified", "plan": "E2"}).to_string() + "\n",
    )?;
    let launcher = dir.join("bin/agent");
    let implementation = std::fs::canonicalize(dir.join("versions/A"))?;
    let sha = |path: &std::path::Path| crate::adapter::sha256_file(path);
    let stat = |path: &std::path::Path| -> R<FileStamp> {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::metadata(path)?;
        Ok(FileStamp {
            dev: m.dev(),
            ino: m.ino(),
            ctime_sec: m.ctime(),
            ctime_nsec: m.ctime_nsec(),
        })
    };
    Ok(Expect {
        agent: Agent::Claude,
        identity: AgentIdentity {
            launcher_path: launcher.display().to_string(),
            launcher_sha256: sha(&launcher)?,
            implementation_path: implementation.display().to_string(),
            implementation_sha256: sha(&implementation)?,
        },
        stamp: Stamp {
            launcher: stat(&launcher)?,
            implementation: stat(&implementation)?,
        },
        plan: Exec::Path,
    })
}

/// Replace `path` by a new file of `bytes` (a new inode, renamed into place).
fn replace(path: &std::path::Path, bytes: &[u8]) -> R {
    let staged = path.with_extension("staged");
    std::fs::write(&staged, bytes)?;
    std::fs::rename(&staged, path)?;
    Ok(())
}

#[test]
fn the_recheck_compares_the_files_stamps_and_derives_the_identity_again_on_a_difference() -> R {
    // Decision 9 ("首个输入之前的再核对", -201) and the test plan's table: both files' (dev, ino, ctime) equal to
    // Expect.stamp → Same; the implementation replaced by another inode, only its ctime changed (chmod to the same
    // mode), appended to in place, deleted, the launcher's link retargeted to another file → each Changed, naming the
    // file (the implementation first); gate.jsonl deleted or rewritten changes nothing (it is not read). After a
    // difference the identity derived again decides: the same → stamp_only, another or none → not.
    use std::os::unix::fs::PermissionsExt as _;
    struct Case {
        name: &'static str,
        change: fn(&std::path::Path) -> R,
        file: Option<AgentFile>,
        same_ino: bool,
        missing: bool,
        stamp_only: bool,
    }
    let cases = [
        Case {
            name: "nothing changed",
            change: |_| Ok(()),
            file: None,
            same_ino: true,
            missing: false,
            stamp_only: false,
        },
        Case {
            name: "gate.jsonl deleted and rewritten",
            change: |dir| {
                std::fs::remove_file(dir.join("gate.jsonl"))?;
                std::fs::write(
                    dir.join("gate.jsonl"),
                    serde_json::json!({"outcome": "refused", "key": KEY, "code": "COMPAT_UNVERIFIED",
                                       "item": "stamp", "stamp_only": false})
                    .to_string()
                        + "\n",
                )?;
                Ok(())
            },
            file: None,
            same_ino: true,
            missing: false,
            stamp_only: false,
        },
        Case {
            name: "the implementation replaced by another content",
            change: |dir| replace(&dir.join("versions/A"), b"#!/bin/sh\necho other\n"),
            file: Some(AgentFile::Implementation),
            same_ino: false,
            missing: false,
            stamp_only: false,
        },
        Case {
            name: "the implementation replaced by the same content",
            change: |dir| replace(&dir.join("versions/A"), b"#!/bin/sh\necho A\n"),
            file: Some(AgentFile::Implementation),
            same_ino: false,
            missing: false,
            stamp_only: true,
        },
        Case {
            name: "chmod to the same mode",
            change: |dir| {
                let path = dir.join("versions/A");
                let mode = std::fs::metadata(&path)?.permissions().mode();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
                Ok(())
            },
            file: Some(AgentFile::Implementation),
            same_ino: true,
            missing: false,
            stamp_only: true,
        },
        Case {
            name: "appended to in place",
            change: |dir| {
                use std::io::Write as _;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(dir.join("versions/A"))?
                    .write_all(b"#\n")?;
                Ok(())
            },
            file: Some(AgentFile::Implementation),
            same_ino: true,
            missing: false,
            stamp_only: false,
        },
        Case {
            name: "the implementation deleted",
            change: |dir| Ok(std::fs::remove_file(dir.join("versions/A"))?),
            file: Some(AgentFile::Implementation),
            same_ino: false,
            missing: true,
            stamp_only: false,
        },
        Case {
            name: "the launcher's link retargeted",
            change: |dir| {
                std::os::unix::fs::symlink(dir.join("versions/B"), dir.join("bin/agent.new"))?;
                std::fs::rename(dir.join("bin/agent.new"), dir.join("bin/agent"))?;
                Ok(())
            },
            file: Some(AgentFile::Launcher),
            same_ino: false,
            missing: false,
            stamp_only: false,
        },
    ];
    for case in cases {
        let dir = tempfile::tempdir()?;
        let expect = installed(dir.path())?;
        assert_eq!(recheck(&expect), Recheck::Same, "{}: before", case.name);
        (case.change)(dir.path())?;
        let found = recheck(&expect).changed();
        assert_eq!(
            found.as_ref().map(Changed::file),
            case.file,
            "{}: {found:?}",
            case.name
        );
        let Some(changed) = found else {
            continue;
        };
        let expected = match changed.file() {
            AgentFile::Launcher => expect.stamp.launcher,
            AgentFile::Implementation => expect.stamp.implementation,
        };
        assert_eq!(changed.expected(), expected, "{}", case.name);
        assert_eq!(changed.seen().is_none(), case.missing, "{}", case.name);
        if let Some(seen) = changed.seen() {
            assert_eq!(seen.ino == expected.ino, case.same_ino, "{}", case.name);
            assert_ne!(seen, expected, "{}", case.name);
        }
        assert_eq!(changed.stamp_only(), case.stamp_only, "{}", case.name);
        let (key, payload) = changed.event();
        assert_eq!(key, "implementation-changed-before-input");
        assert_eq!(payload["reason"], "IMPLEMENTATION_CHANGED_BEFORE_INPUT");
        assert_eq!(payload["file"], changed.file().as_str());
        assert_eq!(payload["expected"], serde_json::json!(expected));
        if case.missing {
            assert_eq!(payload["seen"], "missing", "{}", case.name);
        }
    }
    Ok(())
}

#[test]
fn the_identity_derived_again_is_the_gates() -> R {
    // Decision 9 ("身份是否也变了"): the implementation resolved again the probe's way and both files hashed through
    // their descriptors: the expectation's identity while nothing changed, the other version's once the link names it,
    // none once the implementation it names is gone.
    let dir = tempfile::tempdir()?;
    let expect = installed(dir.path())?;
    assert_eq!(identity_now(&expect), Some(expect.identity.clone()));
    std::os::unix::fs::symlink(
        dir.path().join("versions/B"),
        dir.path().join("bin/agent.new"),
    )?;
    std::fs::rename(
        dir.path().join("bin/agent.new"),
        dir.path().join("bin/agent"),
    )?;
    let other = identity_now(&expect).ok_or("no identity")?;
    assert_eq!(
        other.implementation_path,
        std::fs::canonicalize(dir.path().join("versions/B"))?
            .display()
            .to_string()
    );
    assert_ne!(other, expect.identity);
    std::fs::remove_file(dir.path().join("versions/B"))?;
    assert_eq!(identity_now(&expect), None);
    Ok(())
}

#[test]
fn a_refusal_before_the_first_input_is_typed_as_that_rechecks() -> R {
    // Decisions 6 and 9 (K9): the facts of a turn its re-check before the first input refused, judged FAILED
    // COMPAT_UNVERIFIED by its adapter, are typed BeforeInput, stamp_only as the re-check found it; another verdict, or
    // no re-check that found a change, stays untyped.
    let dir = tempfile::tempdir()?;
    let expect = installed(dir.path())?;
    std::fs::set_permissions(
        dir.path().join("versions/A"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )?;
    let stamp_only = recheck(&expect).changed().ok_or("no change")?;
    replace(&dir.path().join("versions/A"), b"other")?;
    let other = recheck(&expect).changed().ok_or("no change")?;
    assert!(stamp_only.stamp_only() && !other.stamp_only());
    let failed = || Facts {
        state: AttemptState::Failed,
        failure: Some("COMPAT_UNVERIFIED".to_owned()),
        ..Facts::incomplete("/logs".to_owned(), Vec::new())
    };
    for (changed, want) in [(&stamp_only, true), (&other, false)] {
        let compat = failed()
            .before_input(Some(changed))
            .compat
            .ok_or("untyped")?;
        assert_eq!(
            (compat.origin(), compat.stamp_only()),
            (Origin::BeforeInput, want)
        );
    }
    assert_eq!(failed().before_input(None).compat, None);
    let cancelled = Facts {
        state: AttemptState::Cancelled,
        failure: None,
        ..failed()
    };
    assert_eq!(cancelled.before_input(Some(&other)).compat, None);
    let other_failure = Facts {
        failure: Some("RAW_LOG_INCOMPLETE".to_owned()),
        ..failed()
    };
    assert_eq!(other_failure.before_input(Some(&other)).compat, None);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn the_in_turn_recheck_records_each_new_stamp_once() -> R {
    // Decision 9 ("turn 中的再核对（只记录）"): every re-check compares each file with what it was last seen as; a new
    // stamp is one IMPLEMENTATION_CHANGED_IN_TURN, the same one is not recorded again. The launcher's link retargeted
    // (its version kept) names the launcher; that version deleted names the implementation, seen missing. A launcher
    // that is the implementation's file seen changed with it is not named again (one change, one diagnostic).
    let dir = tempfile::tempdir()?;
    let expect = installed(dir.path())?;
    let mut watch = Watch::new(Some(expect.clone()));
    assert!(watch.check().is_empty());
    std::os::unix::fs::symlink(
        dir.path().join("versions/B"),
        dir.path().join("bin/agent.new"),
    )?;
    std::fs::rename(
        dir.path().join("bin/agent.new"),
        dir.path().join("bin/agent"),
    )?;
    let first = watch.check();
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].1["reason"], "IMPLEMENTATION_CHANGED_IN_TURN");
    assert_eq!(first[0].1["file"], "launcher");
    assert_eq!(
        first[0].1["expected"],
        serde_json::json!(expect.stamp.launcher)
    );
    assert!(watch.check().is_empty(), "the same change recorded again");
    std::fs::remove_file(dir.path().join("versions/A"))?;
    let second = watch.check();
    assert_eq!(second.len(), 1, "{second:?}");
    assert_eq!(
        (&second[0].1["file"], &second[0].1["seen"]),
        (
            &serde_json::json!("implementation"),
            &serde_json::json!("missing")
        )
    );
    assert_ne!(
        first[0].0, second[0].0,
        "two events of one attempt share a key"
    );
    assert!(watch.check().is_empty());
    // One file as launcher and implementation (a launcher that is its own implementation): replaced, one diagnostic.
    let single = tempfile::tempdir()?;
    installed(single.path())?;
    let mut alone = expect.clone();
    let path = std::fs::canonicalize(single.path().join("versions/A"))?;
    alone.identity.launcher_path = path.display().to_string();
    alone.identity.implementation_path = path.display().to_string();
    let stamp = {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::metadata(&path)?;
        FileStamp {
            dev: m.dev(),
            ino: m.ino(),
            ctime_sec: m.ctime(),
            ctime_nsec: m.ctime_nsec(),
        }
    };
    alone.stamp = Stamp {
        launcher: stamp,
        implementation: stamp,
    };
    let mut watch = Watch::new(Some(alone));
    replace(&path, b"other")?;
    let replaced = watch.check();
    assert_eq!(replaced.len(), 1, "{replaced:?}");
    assert_eq!(replaced[0].1["file"], "implementation");
    Ok(())
}

/// Which adapter event loop a test drives.
#[derive(Debug, Clone, Copy)]
enum Loop {
    Claude,
    CodexAppServer,
    CodexExec,
    Pi,
}

/// The test's look at the diagnostics a loop collects: each with the number of frames handed out when it was
/// collected.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<(usize, serde_json::Value)>>>;

/// A process whose every event is ready at its first poll, never pending (M2T15D-500): `total` frames, then its exit.
/// At frame `change_at` it moves the paused clock past [`RECHECK_EVERY`] and runs `change`; at frame `again_at` it moves
/// the clock again and changes nothing; at its last frame, it moves the clock and runs `last`.
struct AlwaysReady {
    handed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    total: usize,
    change_at: usize,
    again_at: usize,
    change: Box<dyn Fn() + Send + Sync>,
    last: Box<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for AlwaysReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AlwaysReady")
    }
}

impl AlwaysReady {
    /// Move the paused clock past the next re-check now, without waiting: `advance` moves it before its first await.
    fn past_the_next_recheck() {
        let _ = at_once(tokio::time::advance(
            RECHECK_EVERY + std::time::Duration::from_millis(1),
        ));
    }
}

impl crate::process::FakeEvents for AlwaysReady {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<crate::process::Event>> + Send + '_>,
    > {
        let n = self
            .handed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if n == self.change_at {
            AlwaysReady::past_the_next_recheck();
            (self.change)();
        }
        if n == self.again_at {
            AlwaysReady::past_the_next_recheck();
        }
        if n == self.total {
            AlwaysReady::past_the_next_recheck();
            (self.last)();
        }
        let event = match n {
            n if n <= self.total => Some(crate::process::Event::Frame(crate::frame::Frame::Line(
                br#"{"type":"hoh_test_frame"}"#.to_vec(),
            ))),
            n if n == self.total + 1 => {
                Some(crate::process::Event::Exit(Box::new(replayed_exit())))
            }
            _ => None,
        };
        Box::pin(std::future::ready(event))
    }
}

/// Drive `which`'s loop on an [`AlwaysReady`] process watching `expect` (none: no watch), `changes` making the
/// changes; its verdict and the IMPLEMENTATION_CHANGED_IN_TURN diagnostics it collected, each with the frames handed
/// out by then, and what the turn recorded of them.
async fn drive_always_ready(
    which: Loop,
    expect: Option<Expect>,
    change: Box<dyn Fn() + Send + Sync>,
    last: Box<dyn Fn() + Send + Sync>,
) -> R<(
    (AttemptState, Option<String>),
    Vec<(usize, serde_json::Value)>,
    Vec<serde_json::Value>,
)> {
    let handed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let events = AlwaysReady {
        handed: handed.clone(),
        total: 40,
        change_at: 10,
        again_at: 30,
        change,
        last,
    };
    let seen: Seen = std::sync::Arc::default();
    let into = seen.clone();
    let observer = Observer::new(
        "attempt",
        Some(Observe(std::sync::Arc::new(
            move |_: &str, d: &serde_json::Value| {
                if d["reason"] == CHANGED_IN_TURN
                    && let Ok(mut seen) = into.lock()
                {
                    seen.push((handed.load(std::sync::atomic::Ordering::SeqCst), d.clone()));
                }
            },
        ))),
    );
    let mut process = crate::process::Process::fake(4242, None, Box::new(events));
    let mut watch = Watch::new(expect);
    let (state, failure, recorded) = match which {
        Loop::Claude => crate::claude::drive_watched(&mut process, &mut watch, observer).await?,
        Loop::CodexAppServer => {
            crate::codex::drive_watched(false, &mut process, &mut watch, observer).await?
        }
        Loop::CodexExec => {
            crate::codex::drive_watched(true, &mut process, &mut watch, observer).await?
        }
        Loop::Pi => crate::pi::drive_watched(&mut process, &mut watch, observer).await?,
    };
    let kept = recorded
        .into_iter()
        .filter_map(|r| match r {
            Recorded::Event { payload, .. } if payload["reason"] == CHANGED_IN_TURN => {
                Some(payload)
            }
            _ => None,
        })
        .collect();
    let seen = seen.lock().map(|s| s.clone()).unwrap_or_default();
    Ok(((state, failure), seen, kept))
}

#[tokio::test(start_paused = true)]
async fn an_event_loop_whose_frames_are_always_ready_still_rechecks_on_time() -> R {
    // M2T15D-400, -500 (decision 9, "turn 中的再核对"): each adapter's loop driven on a paused clock by a process whose
    // every frame is ready at once. The clock moved past RECHECK_EVERY and the implementation replaced at frame 10: the
    // diagnostic is collected before frame 11 is read; the clock moved again at frame 30 with nothing changed: nothing
    // is recorded again; at the last frame the clock moved and the launcher's link retargeted: its diagnostic is
    // collected before the exit event is read. The verdict is the one of the same loop with nothing changed, and what
    // the turn recorded is what was collected. Red by the mutation "the re-check moved back to a timer branch after the
    // frames": no diagnostic at all.
    for which in [
        Loop::Claude,
        Loop::CodexAppServer,
        Loop::CodexExec,
        Loop::Pi,
    ] {
        let dir = tempfile::tempdir()?;
        let expect = installed(dir.path())?;
        let root = dir.path().to_path_buf();
        let at = root.clone();
        let (verdict, seen, kept) = drive_always_ready(
            which,
            Some(expect.clone()),
            Box::new(move || {
                let _ = replace(&at.join("versions/A"), b"#!/bin/sh\necho C\n");
            }),
            Box::new(move || {
                let _ =
                    std::os::unix::fs::symlink(root.join("versions/B"), root.join("bin/agent.new"));
                let _ = std::fs::rename(root.join("bin/agent.new"), root.join("bin/agent"));
            }),
        )
        .await?;
        let files: Vec<(usize, &serde_json::Value)> =
            seen.iter().map(|(n, d)| (*n, &d["file"])).collect();
        assert_eq!(
            files,
            [
                (10, &serde_json::json!("implementation")),
                (40, &serde_json::json!("launcher"))
            ],
            "{which:?}: {seen:?}"
        );
        assert_ne!(
            seen[0].1["seen"]["ino"], seen[0].1["expected"]["ino"],
            "{which:?}"
        );
        assert_eq!(
            kept,
            seen.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>(),
            "{which:?}"
        );
        let unchanged = tempfile::tempdir()?;
        let control = installed(unchanged.path())?;
        let (control_verdict, control_seen, _) =
            drive_always_ready(which, Some(control), Box::new(|| {}), Box::new(|| {})).await?;
        assert_eq!(control_seen, Vec::new(), "{which:?}");
        assert_eq!(verdict, control_verdict, "{which:?}: the outcome changed");
        let (unwatched, _, _) =
            drive_always_ready(which, None, Box::new(|| {}), Box::new(|| {})).await?;
        assert_eq!(verdict, unwatched, "{which:?}: the outcome changed");
    }
    Ok(())
}

#[test]
fn a_gate_refusal_is_shown_to_the_observer_as_its_diagnostic_is_collected() -> R {
    // Decision 9 (M2T15D-401): the executor's test hook sees each diagnostic a turn collects; a gate's refusal is judged
    // once the turn ended, and its GATE_REFUSED is shown then; diagnostics the loop collected were shown already and are
    // not shown again, and a turn of no observer shows nothing.
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("gate.jsonl"),
        serde_json::json!({"outcome": "refused", "key": KEY, "code": "COMPAT_UNVERIFIED",
                           "item": "stamp", "stamp_only": true, "expected": "e", "seen": "s"})
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
        key: "stdin-failed".to_owned(),
        kind: "adapter.diagnostic".to_owned(),
        payload: serde_json::json!({"reason": "stdin failed"}),
    }];
    let seen: std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>> =
        std::sync::Arc::default();
    let into = seen.clone();
    let observer = Observer::new(
        "attempt-1",
        Some(Observe(std::sync::Arc::new(
            move |a: &str, d: &serde_json::Value| {
                if let Ok(mut seen) = into.lock() {
                    seen.push((a.to_owned(), d.clone()));
                }
            },
        ))),
    );
    let facts = refused_at_gate(&gate, "/logs", Some(&report), &earlier).ok_or("not refused")?;
    let shown = observer.refused(facts.clone());
    assert_eq!(shown, facts);
    let seen = seen.lock().map(|s| s.clone()).unwrap_or_default();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].0, "attempt-1");
    assert_eq!(seen[0].1["reason"], "GATE_REFUSED");
    assert_eq!(Observer::default().refused(facts.clone()), facts);
    Ok(())
}

/// What a [`Timed`] process does as one of its events is handed out.
type Then = Option<Box<dyn FnOnce() + Send + Sync>>;

/// One step of a [`Timed`] process: when, on the paused clock, the event it hands out (none: only its [`Then`] runs).
type Step = (tokio::time::Instant, Option<crate::process::Event>, Then);

/// A process whose steps come at set times of the paused clock, in order: each taken once its time came, its [`Then`]
/// run, its event handed out; nothing after the last. A wait the event loop drops for another branch takes nothing.
struct Timed(std::collections::VecDeque<Step>);

impl std::fmt::Debug for Timed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Timed")
    }
}

impl crate::process::FakeEvents for Timed {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<crate::process::Event>> + Send + '_>,
    > {
        Box::pin(async move {
            loop {
                let at = self.0.front()?.0;
                tokio::time::sleep_until(at).await;
                let (_, event, then) = self.0.pop_front()?;
                if let Some(then) = then {
                    then();
                }
                if event.is_some() {
                    return event;
                }
            }
        })
    }
}

/// The handshake timeout of a [`codex_facts_timed`] turn.
const HANDSHAKE: std::time::Duration = std::time::Duration::from_secs(1);

/// A step of a [`Timed`] script: its time in milliseconds from the start, its event, its [`Then`].
type At = (u64, Option<crate::process::Event>, Then);

/// A codex App Server frame handed out at `ms` milliseconds.
fn frame_at(ms: u64, frame: serde_json::Value, then: Then) -> At {
    (
        ms,
        Some(crate::process::Event::Frame(crate::frame::Frame::Line(
            frame.to_string().into_bytes(),
        ))),
        then,
    )
}

/// The process's exit at `ms` milliseconds, its streams complete, `stdout_bytes` read from its stdout.
fn exit_at(ms: u64, stdout_bytes: u64) -> At {
    let mut report = replayed_exit();
    report.stdout.bytes = stdout_bytes;
    (
        ms,
        Some(crate::process::Event::Exit(Box::new(report))),
        None,
    )
}

/// `then` run at `ms` milliseconds, no event handed out.
fn act_at(ms: u64, then: Box<dyn FnOnce() + Send + Sync>) -> At {
    (ms, None, Some(then))
}

/// The facts of a codex App Server turn of the runner's mode (its handshake timeout [`HANDSHAKE`], its native cancel
/// `native`) driven on a [`Timed`] process run by `script`, watching `expect`, its gate `gate`; its stdin a pipe nobody
/// reads. With them, when (from the start) the adapter asked to end the process, if it did.
async fn codex_facts_timed(
    script: Vec<At>,
    expect: Option<Expect>,
    gate: &GateCommand,
    native: &tokio::sync::Notify,
) -> R<(Facts, Option<std::time::Duration>)> {
    let start = tokio::time::Instant::now();
    let events = Timed(
        script
            .into_iter()
            .map(|(ms, event, then)| (start + std::time::Duration::from_millis(ms), event, then))
            .collect(),
    );
    let (stdin, unread) = tokio::net::unix::pipe::pipe()?;
    let mut process = crate::process::Process::fake(4242, Some(stdin), Box::new(events));
    // The paused clock stands still while tasks can run, so this is the time the cancel was asked for.
    let mut cancels = process.cancels();
    let asked = tokio::spawn(async move {
        cancels.changed().await.ok()?;
        Some(tokio::time::Instant::now() - start)
    });
    let mut watch = Watch::new(expect);
    let facts = crate::codex::app_server_facts_watched(
        &mut process,
        &mut watch,
        Observer::default(),
        gate,
        HANDSHAKE,
        native,
    )
    .await;
    drop(unread);
    // Its control dropped, a cancel never asked for ends the wait.
    drop(process);
    Ok((facts, asked.await?))
}

/// The term grace of a [`codex_facts_timed`] turn: the default escalation's.
fn term_grace() -> std::time::Duration {
    crate::codex::Turn::default_escalation().term_grace
}

/// Whether the adapter asked to end the process `asked` after the start at `ms` milliseconds plus the term grace (within
/// the clock's millisecond).
fn ended_after_the_term_grace(asked: Option<std::time::Duration>, ms: u64) -> bool {
    let due = std::time::Duration::from_millis(ms) + term_grace();
    asked.is_some_and(|at| at >= due && at <= due + std::time::Duration::from_millis(1))
}

/// A gate with no expectation and no status: today's `go`.
fn plain_gate() -> GateCommand {
    GateCommand {
        program: "/hoh".into(),
        args: Vec::new(),
        env: Vec::new(),
        expect: None,
        status: None,
    }
}

/// Each event a turn recorded as (kind, its payload's reason).
fn kinds(recorded: &[Recorded]) -> Vec<(String, serde_json::Value)> {
    recorded
        .iter()
        .filter_map(|r| match r {
            Recorded::Event { kind, payload, .. } => {
                Some((kind.clone(), payload["reason"].clone()))
            }
            Recorded::NativeSession(_) => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_refusal_before_the_first_input_outlasts_the_handshake_deadline() -> R {
    // M2T15R-02 (decision 9, "首个输入之前的再核对"; decision 15): initialize is answered just before the handshake
    // deadline, the implementation replaced as the answer comes, so the re-check refuses and stdin is closed; the process
    // exits only after the deadline. The deadline no longer applies once initialize was answered: the facts are the
    // re-check's typed refusal, never a move to exec (no `degraded`, which marks the implementation exec_only). Red
    // before the fix: the handshake timer set the degrade after the refusal and `conclude` preferred it.
    let dir = tempfile::tempdir()?;
    let expect = installed(dir.path())?;
    let at = dir.path().join("versions/A");
    let (facts, _) = codex_facts_timed(
        vec![
            frame_at(
                900,
                serde_json::json!({"id": 0, "result": {"userAgent": "codex"}}),
                Some(Box::new(move || {
                    let _ = replace(&at, b"#!/bin/sh\necho C\n");
                })),
            ),
            exit_at(1500, 64),
        ],
        Some(expect),
        &plain_gate(),
        &tokio::sync::Notify::new(),
    )
    .await?;
    assert_eq!(
        (
            facts.state,
            facts.failure.as_deref(),
            facts.degraded.as_deref()
        ),
        (AttemptState::Failed, Some(COMPAT_UNVERIFIED), None),
        "{facts:?}"
    );
    let compat = facts.compat.ok_or("untyped")?;
    assert_eq!(
        (compat.origin(), compat.stamp_only()),
        (Origin::BeforeInput, false)
    );
    let recorded = kinds(&facts.recorded);
    assert!(
        recorded.contains(&(
            "adapter.diagnostic".to_owned(),
            serde_json::json!(CHANGED_BEFORE_INPUT)
        )),
        "{recorded:?}"
    );
    assert!(
        !recorded.iter().any(|(kind, _)| kind == "adapter.degraded"),
        "{recorded:?}"
    );
    Ok(())
}

/// M2T15R-02, a refused handshake request in the runner's mode: codex answers `refused` with an error at 900 ms after
/// `before` was answered, and would exit by itself only at 7 s. The turn fails with `failure`, never moves to exec, and
/// the adapter itself ends the process once the term grace from the refusal is over (stdin closed at once, then the
/// end the turn's escalation plans), as after the re-check before the first input: a refusal is the turn's own end, and
/// no cancel of the host's follows it.
async fn refused_in_the_runners_mode(
    before: Vec<At>,
    refused: serde_json::Value,
    failure: &str,
) -> R {
    let mut script = before;
    script.push(frame_at(900, refused, None));
    script.push(exit_at(7000, 64));
    let (facts, asked) =
        codex_facts_timed(script, None, &plain_gate(), &tokio::sync::Notify::new()).await?;
    assert_eq!(
        (
            facts.state,
            facts.failure.as_deref(),
            facts.degraded.as_deref()
        ),
        (AttemptState::Failed, Some(failure), None),
        "{facts:?}"
    );
    assert!(
        ended_after_the_term_grace(asked, 900),
        "{failure}: the adapter asked to end the process at {asked:?}, not the term grace after the refusal"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_initialize_refused_in_the_runners_mode_is_ended_after_the_term_grace() -> R {
    // Initialize refused with an error other than "method not found", just before the handshake deadline. Red at
    // 689abe8: the handshake timer's degrade won (CAPABILITY_UNSUPPORTED) and its end came at the deadline plus the term
    // grace; red with only M2T15R-02's first fix: no end at all (the stop only closed stdin).
    refused_in_the_runners_mode(
        Vec::new(),
        serde_json::json!({"id": 0, "error": {"code": -32000, "message": "boom"}}),
        "initialize: boom",
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_thread_start_refused_in_the_runners_mode_is_ended_after_the_term_grace() -> R {
    // Red at 689abe8 and before this fix: no end at all (the stop only closed stdin, and no timer covers the thread).
    refused_in_the_runners_mode(
        vec![frame_at(
            100,
            serde_json::json!({"id": 0, "result": {}}),
            None,
        )],
        serde_json::json!({"id": 1, "error": {"code": -32000, "message": "no thread"}}),
        "thread/start: no thread",
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_turn_start_refused_in_the_runners_mode_is_ended_after_the_term_grace() -> R {
    // Red at 689abe8 and before this fix: no end at all (the stop only closed stdin, and no timer covers the turn's start).
    refused_in_the_runners_mode(
        vec![
            frame_at(100, serde_json::json!({"id": 0, "result": {}}), None),
            frame_at(
                200,
                serde_json::json!({"id": 1, "result": {"thread": {"id": "thread-1"}, "model": "m"}}),
                None,
            ),
        ],
        serde_json::json!({"id": 2, "error": {"code": -32000, "message": "no turn"}}),
        "turn/start: no turn",
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_native_cancel_before_initialize_is_answered_is_left_to_the_host() -> R {
    // M2T15R-02, the handshake timer once initialize no longer waits: a native cancel at 500 ms closes stdin (the host's
    // ladder ends the process, ADR-022 decision 14); codex would exit by itself only at 7 s. The handshake deadline (1 s)
    // no longer applies, so the adapter asks no end of its own and nothing degrades: the turn is CANCELLED. Red with the
    // 689abe8 timer guard: the timer's degrade closed gently at 1 s and the adapter ended the process at 6 s.
    let native = std::sync::Arc::new(tokio::sync::Notify::new());
    let cancel = native.clone();
    let (facts, asked) = codex_facts_timed(
        vec![
            act_at(500, Box::new(move || cancel.notify_one())),
            exit_at(7000, 0),
        ],
        None,
        &plain_gate(),
        &native,
    )
    .await?;
    assert_eq!(
        (
            facts.state,
            facts.failure.as_deref(),
            facts.degraded.as_deref()
        ),
        (AttemptState::Cancelled, None, None),
        "{facts:?}"
    );
    assert_eq!(
        asked, None,
        "the adapter escalated a cancel the host's ladder ends"
    );
    Ok(())
}

/// M2T15R-03 (decision 9, "turn 中的再核对": its diagnostics are written with the turn's end): the implementation is
/// replaced before the turn starts, so the re-check while it runs records IMPLEMENTATION_CHANGED_IN_TURN at its first
/// tick; then initialize gets `answer` at 300 ms (none: no answer within the handshake timeout). The facts move the next
/// attempt to exec and keep that diagnostic before the turn's adapter.degraded. Red before the fix: only
/// adapter.degraded.
async fn degraded_after_an_in_turn_change(answer: Option<serde_json::Value>) -> R {
    let dir = tempfile::tempdir()?;
    let expect = installed(dir.path())?;
    replace(&dir.path().join("versions/A"), b"#!/bin/sh\necho C\n")?;
    let mut script: Vec<_> = answer.map(|a| frame_at(300, a, None)).into_iter().collect();
    script.push(exit_at(1500, 64));
    let (facts, _) = codex_facts_timed(
        script,
        Some(expect),
        &plain_gate(),
        &tokio::sync::Notify::new(),
    )
    .await?;
    assert_eq!(
        (facts.state, facts.failure.as_deref()),
        (AttemptState::Failed, Some("CAPABILITY_UNSUPPORTED")),
        "{facts:?}"
    );
    assert!(facts.degraded.is_some(), "{facts:?}");
    assert_eq!(
        kinds(&facts.recorded)
            .into_iter()
            .map(|(kind, reason)| (kind, reason.as_str().map(str::to_owned)))
            .collect::<Vec<_>>(),
        [
            (
                "adapter.diagnostic".to_owned(),
                Some(CHANGED_IN_TURN.to_owned())
            ),
            ("adapter.degraded".to_owned(), facts.degraded.clone()),
        ]
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_turn_degraded_by_method_not_found_keeps_what_it_recorded() -> R {
    degraded_after_an_in_turn_change(Some(
        serde_json::json!({"id": 0, "error": {"code": -32601, "message": "Method not found"}}),
    ))
    .await
}

#[tokio::test(start_paused = true)]
async fn a_turn_degraded_by_the_handshake_timeout_keeps_what_it_recorded() -> R {
    degraded_after_an_in_turn_change(None).await
}

#[tokio::test(start_paused = true)]
async fn a_gate_refusal_outranks_a_handshake_that_timed_out() -> R {
    // M2T15R-02, the same flaw on the gate's side (decision 9, "适配器读"): the gate takes longer than the handshake
    // timeout and refuses the agent, which never wrote a frame; the process exits after the deadline. Its keyed refusal
    // decides (COMPAT_UNVERIFIED from the gate), not a move to exec. Red before the fix: CAPABILITY_UNSUPPORTED, degraded.
    let dir = tempfile::tempdir()?;
    std::fs::write(
        dir.path().join("gate.jsonl"),
        serde_json::json!({"outcome": "refused", "key": KEY, "code": "COMPAT_UNVERIFIED",
                           "item": "implementation_sha256", "stamp_only": false, "expected": "e", "seen": "s"})
        .to_string()
            + "\n",
    )?;
    let gate = GateCommand {
        expect: Some(expect()),
        status: Some(status(dir.path(), KEY)),
        ..plain_gate()
    };
    let (facts, _) = codex_facts_timed(
        vec![exit_at(1500, 0)],
        None,
        &gate,
        &tokio::sync::Notify::new(),
    )
    .await?;
    assert_eq!(
        (
            facts.state,
            facts.failure.as_deref(),
            facts.degraded.as_deref()
        ),
        (AttemptState::Failed, Some(COMPAT_UNVERIFIED), None),
        "{facts:?}"
    );
    let compat = facts.compat.ok_or("untyped")?;
    assert_eq!(
        (compat.origin(), compat.stamp_only()),
        (Origin::Gate, false)
    );
    Ok(())
}
