//! The compat suite against docs/adr/ADR-030.md decision 10 ("PASS 取决于被测二进制", M2T15D-02, -50, -52) and the test
//! plan's "兼容套件" entries: the recorded versions pass (positive controls), and each counter-example changes one fact
//! of the tested binary that the probe alone does not see (the owner's request), or leaves the suite unable to decide.
//! The tested binaries are fake CLIs serving the recorded helps and schema export (tests/fixtures/protocol), changed
//! where a counter-example says.

use super::*;
use crate::compat::{CompatManifest, verdict};
use hoh_core::staffing::Compat;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

type R = Result<(), Box<dyn std::error::Error>>;

fn protocol() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/protocol")
}

fn recorded(rel: &str) -> String {
    std::fs::read_to_string(protocol().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// `text` with `from` replaced by `to`; `from` must be there, so a counter-example never silently equals the original.
fn changed(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "the recorded text has no {from:?}");
    text.replacen(from, to, 1)
}

/// A PATH sandbox of fake CLIs and the files they serve.
struct Bin {
    tmp: tempfile::TempDir,
}

impl Bin {
    fn new() -> Result<Bin, Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        std::fs::create_dir(tmp.path().join("bin"))?;
        Ok(Bin { tmp })
    }

    /// A file of the sandbox's own, outside its PATH.
    fn file(&self, name: &str, text: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = self.tmp.path().join(name);
        std::fs::write(&path, text)?;
        Ok(path)
    }

    fn script(&self, name: &str, script: &str) -> R {
        let path = self.tmp.path().join("bin").join(name);
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    fn env(&self) -> ProbeEnv {
        ProbeEnv {
            path: format!("{}:/usr/bin:/bin", self.tmp.path().join("bin").display()).into(),
            fixture_root: None,
            timeout: Duration::from_secs(30),
            log: None,
            deadline: None,
            cancel: None,
        }
    }

    /// A copy of the recorded 0.166.0 schema export, `edit` applied to it.
    fn export(&self, edit: impl FnOnce(&Path)) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let to = self.tmp.path().join("export");
        let status = std::process::Command::new("cp")
            .arg("-R")
            .arg(protocol().join("codex/0.166.0/app-server-schema"))
            .arg(&to)
            .status()?;
        assert!(status.success());
        edit(&to);
        Ok(to)
    }

    /// The recorded codex 0.166.0, its App Server's schema export served from `export`.
    fn codex(&self, export: &Path) -> R {
        let f = protocol().join("codex/0.166.0");
        self.script(
            "codex",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n \
                 exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
                 app-server) case \"$2\" in\n  --help) cat '{f}/help-app-server.txt';;\n  \
                 generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{export}/.' \"$4/\";;\n  \
                 *) exit 2;;\n esac;;\n *) echo \"unknown command $1\" >&2; exit 2;;\nesac\n",
                f = f.display(),
                export = export.display()
            ),
        )
    }

    /// The recorded codex 0.166.0 whose `--help` lists no app-server, so exec is its transport. Its `exec resume
    /// --help` (not recorded) is the recorded `exec --help`, which declares every option a resumed turn passes.
    fn codex_exec(&self) -> R {
        let f = protocol().join("codex/0.166.0");
        self.script(
            "codex",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n \
                 --help) grep -v '^  app-server ' '{f}/help.txt';;\n \
                 exec) case \"$2\" in\n  --help) cat '{f}/help-exec.txt';;\n  \
                 resume) [ \"$3\" = --help ] && cat '{f}/help-exec.txt';;\n  *) exit 2;;\n esac;;\n \
                 *) echo \"unknown command $1\" >&2; exit 2;;\nesac\n",
                f = f.display()
            ),
        )
    }

    /// A claude 2.1.318 whose `--help` is `help`.
    fn claude(&self, help: &str) -> R {
        let help = self.file("claude-help.txt", help)?;
        self.script(
            "claude",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n --version) echo '2.1.318 (Claude Code)';;\n --help) cat '{}';;\n \
                 *) exit 2;;\nesac\n",
                help.display()
            ),
        )
    }

    /// A pi 1.5.0 whose `--help` is `help`.
    fn pi(&self, help: &str) -> R {
        let help = self.file("pi-help.txt", help)?;
        self.script(
            "pi",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n --version) echo '1.5.0';;\n --help) cat '{}';;\n *) exit 2;;\nesac\n",
                help.display()
            ),
        )
    }

    /// The registry's look at `agent` (a probe) and the suite run on what it found.
    fn suite(
        &self,
        agent: Agent,
    ) -> Result<(AgentEntry, Result<SuiteResult, Discarded>), Box<dyn std::error::Error>> {
        let looked = probe::probe(agent, &self.env())?;
        let ran = run(agent, &looked, &self.env(), &ProbeStop::default());
        Ok((looked, ran))
    }
}

/// `file` of an export, edited as JSON.
fn edit_json(export: &Path, file: &str, edit: impl FnOnce(&mut Value)) {
    let path = export.join(file);
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("the file")).expect("JSON");
    edit(&mut value);
    std::fs::write(&path, serde_json::to_vec(&value).expect("JSON")).expect("write");
}

/// The cases a run gives, in order.
fn names(result: &SuiteResult) -> Vec<&str> {
    result.cases.iter().map(|c| c.case.as_str()).collect()
}

fn the_cases() -> Vec<&'static str> {
    std::iter::once(PROBE_CASE)
        .chain(fixtures::CASES)
        .chain([LIVE_CASE, READ_ONLY_CASE])
        .collect()
}

/// The manifest holding `entry` for `agent`.
fn manifest(agent: Agent, entry: &AgentEntry) -> CompatManifest {
    CompatManifest::new([(agent, entry.clone())].into())
}

fn status(result: &SuiteResult, case: &str) -> SuiteStatus {
    result
        .case(case)
        .unwrap_or_else(|| panic!("no case {case}"))
        .status
}

/// The findings of `case`'s `part` with `truth`.
fn found<'a>(result: &'a SuiteResult, case: &str, part: Part, truth: Truth) -> Vec<&'a Finding> {
    result
        .case(case)
        .unwrap_or_else(|| panic!("no case {case}"))
        .of(part)
        .filter(|f| f.truth == truth)
        .collect()
}

/// Assert the run is one that earned `status`, entry and cases as the manifest records them.
fn assert_recorded(agent: Agent, looked: &AgentEntry, result: &SuiteResult, want: SuiteStatus) {
    let entry = &result.entry;
    assert_eq!(entry.suite.status, want, "{agent:?}: {:#?}", result.cases);
    assert_eq!(entry.suite.fixture_set_sha256, *SUITE_SET_SHA256);
    assert!(
        matches!(&entry.suite.run_at, Some(Some(at)) if !at.is_empty()),
        "{:?}",
        entry.suite.run_at
    );
    assert_eq!(names(result), the_cases());
    assert_eq!(
        entry.suite.cases,
        Some(
            result
                .cases
                .iter()
                .map(|c| SuiteCase {
                    case: c.case.clone(),
                    status: c.status,
                })
                .collect()
        )
    );
    assert_eq!(status(result, PROBE_CASE), SuiteStatus::Pass);
    assert_eq!(status(result, LIVE_CASE), SuiteStatus::NotRun);
    // The entry is the probe's, as the registry's look found it.
    assert_eq!(entry.identity(), looked.identity());
    assert_eq!(
        (
            &entry.cli_version,
            entry.transport,
            &entry.help_sha256,
            &entry.schema_sha256
        ),
        (
            &looked.cli_version,
            looked.transport,
            &looked.help_sha256,
            &looked.schema_sha256
        )
    );
    let document = manifest(agent, entry);
    let valid = hoh_core::schema::compat_manifest()
        .validate(&serde_json::to_value(&document).unwrap_or_default());
    assert!(valid.is_ok(), "{valid:?}");
}

#[test]
fn the_recorded_versions_pass_the_suite() -> R {
    // Positive controls: the recorded codex 0.166.0 (help and schema export), claude 2.1.318 and pi 1.5.0; and the
    // recorded codex without its App Server, on exec.
    let bin = Bin::new()?;
    let export = bin.export(|_| {})?;
    bin.codex(&export)?;
    bin.claude(&recorded("claude/2.1.318/help.txt"))?;
    bin.pi(&recorded("pi/1.5.0/help.txt"))?;
    let exec = Bin::new()?;
    exec.codex_exec()?;
    for (bin, agent, transport) in [
        (&bin, Agent::Codex, Transport::AppServer),
        (&bin, Agent::Claude, Transport::StreamJson),
        (&bin, Agent::Pi, Transport::Rpc),
        (&exec, Agent::Codex, Transport::Exec),
    ] {
        let (looked, ran) = bin.suite(agent)?;
        assert_eq!(looked.transport, transport);
        let result = ran.map_err(|d| format!("{agent:?}: {d}"))?;
        assert_recorded(agent, &looked, &result, SuiteStatus::Pass);
        for case in fixtures::CASES {
            assert_eq!(status(&result, case), SuiteStatus::Pass, "{agent:?} {case}");
            let checked = result
                .case(case)
                .map_or(0, |c| c.of(Part::Requests).count());
            assert!(checked > 0, "{agent:?} {case}: no request checked");
        }
        assert_eq!(
            verdict(
                Some(&manifest(agent, &result.entry)),
                agent.as_str(),
                &looked,
                &SUITE_SET_SHA256
            ),
            Compat::Verified,
            "{agent:?}"
        );
    }
    // The App Server's checks: what each case sends, named, and the embedded notifications accepted by the recorded
    // ServerNotification.json.
    let (_, ran) = bin.suite(Agent::Codex)?;
    let result = ran?;
    for case in fixtures::CASES {
        let requests: Vec<&str> = found(&result, case, Part::Requests, Truth::Yes)
            .iter()
            .map(|f| f.detail.as_str())
            .collect();
        let thread = if case == "compaction" {
            "thread/resume"
        } else {
            "thread/start"
        };
        for method in ["initialize", "initialized", thread, "turn/start"] {
            assert!(
                requests.iter().any(|d| d.contains(method)),
                "{case}: {method} in {requests:#?}"
            );
        }
        assert!(
            !found(&result, case, Part::Frames, Truth::Yes).is_empty(),
            "{case}: frames accepted"
        );
    }
    let tool: Vec<&str> = found(&result, "tool", Part::Requests, Truth::Yes)
        .iter()
        .map(|f| f.detail.as_str())
        .collect();
    for sent in ["turn/steer"]
        .into_iter()
        .chain(crate::compat::TOOL_SERVER_REQUESTS.map(|(method, _)| method))
    {
        assert!(tool.iter().any(|d| d.contains(sent)), "{sent} in {tool:#?}");
    }
    Ok(())
}

#[test]
fn a_probe_that_passes_with_a_schema_the_adapters_requests_violate_fails_the_suite() -> R {
    // The owner's counter-example: the recorded 0.166.0 schema with only `danger-full-access` taken out of
    // `SandboxMode` (methods and envelopes unchanged). The probe opens `params`, so it does not see it; the suite does.
    let bin = Bin::new()?;
    let export = bin.export(|dir| {
        edit_json(dir, "ClientRequest.json", |v| {
            let modes = v["definitions"]["SandboxMode"]["enum"]
                .as_array_mut()
                .expect("SandboxMode.enum");
            let before = modes.len();
            modes.retain(|m| m != "danger-full-access");
            assert_eq!(modes.len(), before - 1);
        });
    })?;
    bin.codex(&export)?;
    let looked = probe::probe(Agent::Codex, &bin.env())?;
    assert_eq!(looked.transport, Transport::AppServer);
    let (looked, ran) = bin.suite(Agent::Codex)?;
    let result = ran?;
    assert_recorded(Agent::Codex, &looked, &result, SuiteStatus::Fail);
    assert_eq!(status(&result, "success"), SuiteStatus::Fail);
    let refused = found(&result, "success", Part::Requests, Truth::No);
    assert!(
        refused.iter().any(|f| f.detail.contains("thread/start")),
        "{refused:#?}"
    );
    assert!(
        matches!(
            verdict(
                Some(&manifest(Agent::Codex, &result.entry)),
                "codex",
                &looked,
                &SUITE_SET_SHA256
            ),
            Compat::Failed(_)
        ),
        "a failed suite"
    );
    Ok(())
}

#[test]
fn a_turn_start_with_a_new_required_property_fails_the_suite() -> R {
    let bin = Bin::new()?;
    let export = bin.export(|dir| {
        edit_json(dir, "ClientRequest.json", |v| {
            let params = &mut v["definitions"]["TurnStartParams"];
            params["properties"]["hohRequiredNow"] = serde_json::json!({"type": "string"});
            params["required"]
                .as_array_mut()
                .expect("TurnStartParams.required")
                .push(Value::from("hohRequiredNow"));
        });
    })?;
    bin.codex(&export)?;
    let (looked, ran) = bin.suite(Agent::Codex)?;
    assert_eq!(looked.transport, Transport::AppServer);
    let result = ran?;
    assert_recorded(Agent::Codex, &looked, &result, SuiteStatus::Fail);
    for case in fixtures::CASES {
        assert_eq!(status(&result, case), SuiteStatus::Fail, "{case}");
        let refused = found(&result, case, Part::Requests, Truth::No);
        assert!(
            refused.iter().all(|f| f.detail.contains("turn/start")) && !refused.is_empty(),
            "{case}: {refused:#?}"
        );
    }
    Ok(())
}

#[test]
fn a_turn_completed_of_another_status_set_fails_frame_acceptance() -> R {
    // The turn's status values change (`completed` is `succeeded` now): the version no longer sends the shape the
    // adapter reads, though every request still conforms and the probe sees nothing.
    let bin = Bin::new()?;
    let export = bin.export(|dir| {
        edit_json(dir, "ServerNotification.json", |v| {
            let statuses = v["definitions"]["TurnStatus"]["enum"]
                .as_array_mut()
                .expect("TurnStatus.enum");
            let completed = statuses
                .iter_mut()
                .find(|s| *s == "completed")
                .expect("completed");
            *completed = Value::from("succeeded");
        });
    })?;
    bin.codex(&export)?;
    let (looked, ran) = bin.suite(Agent::Codex)?;
    assert_eq!(looked.transport, Transport::AppServer);
    let result = ran?;
    assert_recorded(Agent::Codex, &looked, &result, SuiteStatus::Fail);
    assert_eq!(status(&result, "success"), SuiteStatus::Fail);
    let refused = found(&result, "success", Part::Frames, Truth::No);
    assert!(
        !refused.is_empty() && refused.iter().all(|f| f.detail.contains("turn/completed")),
        "{refused:#?}"
    );
    assert!(found(&result, "success", Part::Requests, Truth::No).is_empty());
    assert!(found(&result, "success", Part::Replay, Truth::No).is_empty());
    // A turn that ends interrupted or failed still has the shape the adapter reads.
    assert_eq!(status(&result, "cancel"), SuiteStatus::Pass);
    assert_eq!(status(&result, "failure"), SuiteStatus::Pass);
    Ok(())
}

#[test]
fn a_claude_help_without_the_yolo_flag_fails_the_suite() -> R {
    // The help still has what `interface_missing` asks for, so the probe succeeds; the adapter's builder refuses the
    // product's YOLO launch.
    let help = changed(
        &recorded("claude/2.1.318/help.txt"),
        "  --dangerously-skip-permissions        Bypass all permission checks.\n",
        "",
    );
    assert!(!crate::adapter::offers(&help, crate::claude::YOLO_FLAG));
    assert_eq!(crate::claude::interface_missing(&help), None);
    let bin = Bin::new()?;
    bin.claude(&help)?;
    let (looked, ran) = bin.suite(Agent::Claude)?;
    assert_eq!(looked.transport, Transport::StreamJson);
    let result = ran?;
    assert_recorded(Agent::Claude, &looked, &result, SuiteStatus::Fail);
    for case in fixtures::CASES {
        assert_eq!(status(&result, case), SuiteStatus::Fail, "{case}");
        let refused = found(&result, case, Part::Requests, Truth::No);
        assert!(
            !refused.is_empty()
                && refused
                    .iter()
                    .all(|f| f.detail.contains(crate::claude::YOLO_FLAG)),
            "{case}: {refused:#?}"
        );
    }
    Ok(())
}

#[test]
fn a_pi_help_without_model_fails_its_model_variants() -> R {
    let help = recorded("pi/1.5.0/help.txt");
    let model = help
        .lines()
        .find(|l| l.starts_with("  --model "))
        .expect("the recorded help declares --model");
    let help = changed(&help, &format!("{model}\n"), "");
    assert!(!crate::adapter::offers(&help, "--model"));
    assert_eq!(crate::pi::interface_missing(&help), None);
    let bin = Bin::new()?;
    bin.pi(&help)?;
    let (looked, ran) = bin.suite(Agent::Pi)?;
    assert_eq!(looked.transport, Transport::Rpc);
    let result = ran?;
    assert_recorded(Agent::Pi, &looked, &result, SuiteStatus::Fail);
    for case in fixtures::CASES {
        assert_eq!(status(&result, case), SuiteStatus::Fail, "{case}");
        for finding in result
            .case(case)
            .map(|c| c.of(Part::Requests))
            .into_iter()
            .flatten()
        {
            let with_model = finding.detail.contains("-model");
            let want = if with_model { Truth::No } else { Truth::Yes };
            assert_eq!(finding.truth, want, "{case}: {finding:?}");
        }
        assert!(!found(&result, case, Part::Requests, Truth::Yes).is_empty());
        assert!(!found(&result, case, Part::Requests, Truth::No).is_empty());
    }
    Ok(())
}

#[test]
fn a_thread_start_nested_beyond_the_validators_depth_is_undecided() -> R {
    // `ThreadStartParams` reached through a chain of 70 references: the validator gives `Unknown` past 64 levels. The
    // cases that send thread/start are NOT_RUN, the suite NOT_RUN with `run_at` set, and the verdict Undecided (it is
    // never a PASS).
    let bin = Bin::new()?;
    let export = bin.export(|dir| {
        edit_json(dir, "ClientRequest.json", |v| {
            let definitions = v["definitions"].as_object_mut().expect("definitions");
            let original = definitions
                .remove("ThreadStartParams")
                .expect("ThreadStartParams");
            let link = |n: usize| serde_json::json!({"$ref": format!("#/definitions/HohDeep{n}")});
            definitions.insert("ThreadStartParams".to_owned(), link(1));
            for n in 1..70 {
                definitions.insert(format!("HohDeep{n}"), link(n + 1));
            }
            definitions.insert("HohDeep70".to_owned(), original);
        });
    })?;
    bin.codex(&export)?;
    let (looked, ran) = bin.suite(Agent::Codex)?;
    assert_eq!(looked.transport, Transport::AppServer);
    let result = ran?;
    assert_recorded(Agent::Codex, &looked, &result, SuiteStatus::NotRun);
    for case in fixtures::CASES {
        assert!(
            found(&result, case, Part::Requests, Truth::No).is_empty(),
            "{case}"
        );
        if case == "compaction" {
            assert_eq!(status(&result, case), SuiteStatus::Pass, "thread/resume");
            continue;
        }
        assert_eq!(status(&result, case), SuiteStatus::NotRun, "{case}");
        let unknown = found(&result, case, Part::Requests, Truth::Unknown);
        assert!(
            !unknown.is_empty() && unknown.iter().all(|f| f.detail.contains("thread/start")),
            "{case}: {unknown:#?}"
        );
    }
    assert!(
        matches!(
            verdict(
                Some(&manifest(Agent::Codex, &result.entry)),
                "codex",
                &looked,
                &SUITE_SET_SHA256
            ),
            Compat::Undecided(_)
        ),
        "undecided"
    );
    Ok(())
}

#[test]
fn an_export_without_a_needed_response_schema_cannot_decide_the_tool_case() -> R {
    // The headless answer to a file change's approval has no schema to be checked against in this export.
    let bin = Bin::new()?;
    let export = bin.export(|dir| {
        std::fs::remove_file(dir.join("FileChangeRequestApprovalResponse.json")).expect("remove");
    })?;
    bin.codex(&export)?;
    let (looked, ran) = bin.suite(Agent::Codex)?;
    let result = ran?;
    assert_recorded(Agent::Codex, &looked, &result, SuiteStatus::NotRun);
    assert_eq!(status(&result, "tool"), SuiteStatus::NotRun);
    let unknown = found(&result, "tool", Part::Requests, Truth::Unknown);
    assert!(
        !unknown.is_empty()
            && unknown
                .iter()
                .all(|f| f.detail.contains("item/fileChange/requestApproval")),
        "{unknown:#?}"
    );
    for case in fixtures::CASES.iter().filter(|c| **c != "tool") {
        assert_eq!(status(&result, case), SuiteStatus::Pass, "{case}");
    }
    Ok(())
}

#[test]
fn a_run_whose_probe_finds_another_entry_or_that_is_stopped_is_discarded() -> R {
    let bin = Bin::new()?;
    bin.pi(&recorded("pi/1.5.0/help.txt"))?;
    let looked = probe::probe(Agent::Pi, &bin.env())?;
    let mut other = looked.clone();
    other.help_sha256 = "0".repeat(64);
    let discarded = run(Agent::Pi, &other, &bin.env(), &ProbeStop::default());
    assert!(
        matches!(&discarded, Err(Discarded(why)) if why.contains("help_sha256")),
        "{discarded:?}"
    );
    let mut moved = looked.clone();
    moved.implementation_path = "/elsewhere/pi".to_owned();
    let discarded = run(Agent::Pi, &moved, &bin.env(), &ProbeStop::default());
    assert!(
        matches!(&discarded, Err(Discarded(why)) if why.contains("identity")),
        "{discarded:?}"
    );
    let stop = ProbeStop::default();
    stop.stop();
    let stopped = run(Agent::Pi, &looked, &bin.env(), &stop);
    assert!(matches!(stopped, Err(Discarded(_))), "{stopped:?}");
    assert!(run(Agent::Pi, &looked, &bin.env(), &ProbeStop::default()).is_ok());
    Ok(())
}

/// The read_only case of `result` and the entry's read_only_workspace.
fn read_only(result: &SuiteResult) -> (SuiteStatus, Option<bool>) {
    (
        status(result, READ_ONLY_CASE),
        result
            .entry
            .capabilities
            .get("read_only_workspace")
            .copied()
            .flatten(),
    )
}

#[test]
fn the_read_only_case_decides_read_only_workspace_and_never_the_suite() -> R {
    // ADR-032 decision 4 ("按档的符合性"): the recorded versions take the read-only profile; codex on exec never does
    // (decision 10 (A)); a help or schema without one read-only fact fails the read_only case alone, so the suite's
    // status, the gate of every role, stays PASS while read_only_workspace is false.
    let bin = Bin::new()?;
    bin.codex(&bin.export(|_| {})?)?;
    bin.claude(&recorded("claude/2.1.318/help.txt"))?;
    bin.pi(&recorded("pi/1.5.0/help.txt"))?;
    for agent in [Agent::Codex, Agent::Claude, Agent::Pi] {
        let result = bin.suite(agent)?.1.map_err(|d| format!("{agent:?}: {d}"))?;
        assert_eq!(result.entry.suite.status, SuiteStatus::Pass, "{agent:?}");
        assert_eq!(
            read_only(&result),
            (SuiteStatus::Pass, Some(true)),
            "{agent:?}: {:#?}",
            result.case(READ_ONLY_CASE)
        );
        // Every read-only variant was built and checked.
        assert!(
            found(&result, READ_ONLY_CASE, Part::Requests, Truth::Yes).len() >= 4,
            "{agent:?}"
        );
    }
    let exec = Bin::new()?;
    exec.codex_exec()?;
    let result = exec.suite(Agent::Codex)?.1?;
    assert_eq!(result.entry.suite.status, SuiteStatus::Pass);
    assert_eq!(read_only(&result), (SuiteStatus::Fail, Some(false)));
    assert!(
        found(&result, READ_ONLY_CASE, Part::Requests, Truth::No)
            .iter()
            .all(|f| f.detail.contains("refuses") && f.detail.contains("read-only")),
        "{:#?}",
        result.case(READ_ONLY_CASE)
    );
    // One fact missing: not supported (the builder refuses), named.
    for (agent, help, missing) in [
        (
            Agent::Claude,
            changed(
                &recorded("claude/2.1.318/help.txt"),
                "  --strict-mcp-config   ",
                "  --strict-mcp-cfg      ",
            ),
            "--strict-mcp-config",
        ),
        (
            Agent::Pi,
            changed(
                &recorded("pi/1.5.0/help.txt"),
                "  --no-extensions, -ne ",
                "  --no-extensionz, -nz ",
            ),
            "--no-extensions",
        ),
    ] {
        let bin = Bin::new()?;
        match agent {
            Agent::Claude => bin.claude(&help)?,
            _ => bin.pi(&help)?,
        }
        let result = bin.suite(agent)?.1?;
        assert_eq!(result.entry.suite.status, SuiteStatus::Pass, "{agent:?}");
        for case in fixtures::CASES {
            assert_eq!(status(&result, case), SuiteStatus::Pass, "{agent:?} {case}");
        }
        assert_eq!(
            read_only(&result),
            (SuiteStatus::Fail, Some(false)),
            "{agent:?}"
        );
        let refused = found(&result, READ_ONLY_CASE, Part::Requests, Truth::No);
        assert!(
            refused.len() == 4
                && refused
                    .iter()
                    .all(|f| f.detail.contains("refuses") && f.detail.contains(missing)),
            "{agent:?}: {refused:#?}"
        );
    }
    // The schema without read-only among SandboxMode's values: the read-only requests do not conform.
    let bin = Bin::new()?;
    bin.codex(&bin.export(|export| {
        edit_json(export, "ClientRequest.json", |v| {
            let modes = v["definitions"]["SandboxMode"]["enum"]
                .as_array_mut()
                .expect("SandboxMode.enum");
            modes.retain(|m| m != "read-only");
        })
    })?)?;
    let result = bin.suite(Agent::Codex)?.1?;
    assert_eq!(result.entry.suite.status, SuiteStatus::Pass);
    assert_eq!(read_only(&result), (SuiteStatus::Fail, Some(false)));
    assert!(
        found(&result, READ_ONLY_CASE, Part::Requests, Truth::No)
            .iter()
            .any(|f| f.detail.contains("thread/")),
        "{:#?}",
        result.case(READ_ONLY_CASE)
    );
    Ok(())
}
