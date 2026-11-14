//! `command_execution` as the probe measures it (docs/adr/ADR-030.md decision 12, M2T15D-06, -150, -200): each rule is
//! checked on the unmodified recorded artifacts (they must say true) and on counter-examples that each change one fact.

use super::*;
use crate::adapter::{has_word, lists_name, offers, option_text};
use serde_json::Value;

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

/// The recorded claude `--tools` example, as it ends the option's text.
const TOOLS_EXAMPLE: &str = "\"Bash,Edit,Read\").";

/// The recorded claude `--tools` declaration with its text.
const TOOLS_OPTION: &str =
    "  --tools <tools...>                    Specify the list of available tools from
                                        the built-in set. Use \"\" to disable all
                                        tools, \"default\" to use the default set,
                                        or specify tool names (e.g.
                                        \"Bash,Edit,Read\").
";

/// The recorded claude YOLO declaration with its text.
const YOLO_OPTION: &str = "  --dangerously-skip-permissions        Bypass all permission checks.
                                        Recommended only for sandboxes with no
                                        internet access.
";

#[test]
fn both_recorded_claude_helps_run_commands() {
    for version in ["2.1.317", "2.1.318"] {
        let help = recorded(&format!("claude/{version}/help.txt"));
        assert_eq!(claude_runs_commands(&help), Some(true), "{version}");
        // The example lists Bash only as a piece of `"Bash,Edit,Read").`: the whole-word rule of the second design
        // does not see it (M2T15D-150).
        let tools = option_text(&help, "--tools").expect("--tools is declared");
        assert!(lists_name(&tools, "Bash"), "{version}: {tools}");
        assert!(!has_word(&tools, "Bash"), "{version}: {tools}");
    }
}

#[test]
fn each_claude_counter_example_changes_one_fact() {
    let help = recorded("claude/2.1.318/help.txt");
    let example = |to: &str| changed(&help, TOOLS_EXAMPLE, to);
    for (case, text, expected) in [
        ("Edit,Read", example("\"Edit,Read\")."), Some(false)),
        (
            "BashOutput,Edit",
            example("\"BashOutput,Edit\")."),
            Some(false),
        ),
        ("space-separated", example("\"Bash Edit\")."), Some(false)),
        ("no YOLO flag", changed(&help, YOLO_OPTION, ""), Some(false)),
        ("no --tools", changed(&help, TOOLS_OPTION, ""), None),
    ] {
        assert_eq!(claude_runs_commands(&text), expected, "{case}");
    }
    assert!(!offers(
        &changed(&help, YOLO_OPTION, ""),
        crate::claude::YOLO_FLAG
    ));
    assert!(option_text(&changed(&help, TOOLS_OPTION, ""), "--tools").is_none());
    // Bash named only where `--restricted` describes the tools it removes: that text is no ground.
    let restricted_only = example("\"Read\").");
    let restricted = option_text(&restricted_only, "--restricted").expect("--restricted");
    assert!(lists_name(&restricted, "Bash"), "{restricted}");
    assert!(!restricted_only.contains("Bash,Edit,Read"));
    assert_eq!(claude_runs_commands(&restricted_only), Some(false));
}

#[test]
fn the_recorded_pi_help_lists_its_builtin_tools_trimmed_and_runs_commands() {
    let help = recorded("pi/1.5.0/help.txt");
    let tools = crate::pi::builtin_tools(&help).expect("the recorded help lists its tools");
    let names: std::collections::BTreeSet<&str> = tools.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "read",
            "bash",
            "powershell",
            "edit",
            "write",
            "grep",
            "find",
            "ls"
        ]
        .into_iter()
        .collect()
    );
    assert!(
        tools
            .iter()
            .all(|(name, about)| name.trim() == name && about.trim() == about),
        "{tools:?}"
    );
    assert_eq!(tools["bash"], "Execute bash commands");
    assert_eq!(
        tools["grep"],
        "Search file contents (read-only, off by default)"
    );
    assert_eq!(pi_runs_commands(&help), Some(true));
}

#[test]
fn each_pi_counter_example_changes_one_fact() {
    let help = recorded("pi/1.5.0/help.txt");
    let bash = "  bash       - Execute bash commands\n";
    for (case, text, expected) in [
        (
            "bash off by default",
            changed(
                &help,
                bash,
                "  bash       - Execute bash commands (off by default)\n",
            ),
            Some(false),
        ),
        ("no bash line", changed(&help, bash, ""), Some(false)),
        (
            "no section title",
            changed(&help, "Built-in Tool Names:\n", ""),
            None,
        ),
        (
            "an empty line before bash",
            changed(&help, bash, &format!("\n{bash}")),
            Some(false),
        ),
        (
            "off by default on a continued line",
            changed(
                &help,
                bash,
                &format!("{bash}               (off by default)\n"),
            ),
            Some(false),
        ),
        (
            "bash listed again off by default",
            changed(
                &help,
                bash,
                &format!("{bash}  bash       - Execute bash commands (off by default)\n"),
            ),
            Some(false),
        ),
    ] {
        assert_eq!(pi_runs_commands(&text), expected, "{case}");
    }
}

#[test]
fn the_recorded_codex_exec_help_runs_commands_and_one_without_the_yolo_flag_does_not() {
    let help = recorded("codex/0.166.0/help-exec.txt");
    assert_eq!(exec_runs_commands(&help), Some(true));
    let without = changed(
        &help,
        "      --dangerously-bypass-approvals-and-sandbox\n",
        "",
    );
    assert!(!offers(&without, crate::codex::YOLO_FLAG));
    assert_eq!(exec_runs_commands(&without), Some(false));
}

fn schema_dir() -> PathBuf {
    protocol().join("codex/0.166.0/app-server-schema")
}

/// Every file of the recorded export, as the probe reads them.
fn export() -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).expect("read the export") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                let bytes = std::fs::read(&path).expect("read a file");
                out.push((path, bytes));
            }
        }
    }
    let mut files = Vec::new();
    walk(&schema_dir(), &mut files);
    files.sort();
    files
}

fn file(files: &[(PathBuf, Vec<u8>)], name: &str) -> Option<Value> {
    files
        .iter()
        .find(|(path, _)| path == &schema_dir().join(name))
        .map(|(_, bytes)| serde_json::from_slice(bytes).expect("JSON"))
}

/// The export with `name` replaced by `edit` of it.
fn edited(name: &str, edit: impl FnOnce(&mut Value)) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = export();
    let mut value = file(&files, name).expect("the file is there");
    edit(&mut value);
    let entry = files
        .iter_mut()
        .find(|(path, _)| path == &schema_dir().join(name))
        .expect("the file is there");
    entry.1 = serde_json::to_vec(&value).expect("serialize");
    files
}

/// The probe's reading of `files` as the export: its `command_execution`.
fn read(files: &[(PathBuf, Vec<u8>)]) -> Result<Option<bool>, String> {
    read_schema(
        &schema_dir(),
        files,
        &Work::new(VALIDATION_BUDGET, None),
        &Work::new(u64::MAX, None),
    )
    .map(|schema| schema.command_execution)
}

/// Both files' judgement as [`app_server_runs_commands`] makes it.
fn judged(files: &[(PathBuf, Vec<u8>)]) -> Option<bool> {
    app_server_runs_commands(
        file(files, "ClientRequest.json").as_ref(),
        file(files, "ServerNotification.json").as_ref(),
        &Work::new(VALIDATION_BUDGET, None),
    )
}

#[test]
fn the_recorded_app_server_schema_runs_commands() {
    let files = export();
    assert_eq!(read(&files), Ok(Some(true)));
    assert_eq!(judged(&files), Some(true));
    // What the positive control rests on: the sandbox HoH asks for, and a thread item that is a command.
    let requests = file(&files, "ClientRequest.json").expect("ClientRequest.json");
    assert!(
        requests["definitions"]["SandboxMode"]["enum"]
            .as_array()
            .is_some_and(|modes| modes.contains(&Value::from("danger-full-access")))
    );
}

#[test]
fn each_app_server_counter_example_changes_one_fact() {
    let no_full_access = edited("ClientRequest.json", |v| {
        let modes = v["definitions"]["SandboxMode"]["enum"]
            .as_array_mut()
            .expect("SandboxMode.enum");
        let before = modes.len();
        modes.retain(|m| m != "danger-full-access");
        assert_eq!(modes.len(), before - 1);
    });
    assert_eq!(read(&no_full_access), Ok(Some(false)));
    assert_eq!(judged(&no_full_access), Some(false));

    let no_command_item = edited("ServerNotification.json", |v| {
        let items = v["definitions"]["ThreadItem"]["oneOf"]
            .as_array_mut()
            .expect("ThreadItem.oneOf");
        let before = items.len();
        items
            .retain(|i| i["properties"]["type"]["enum"] != serde_json::json!(["commandExecution"]));
        assert_eq!(items.len(), before - 1);
    });
    assert_eq!(read(&no_command_item), Ok(Some(false)));
    assert_eq!(judged(&no_command_item), Some(false));

    // An export without ServerNotification.json cannot tell; the probe refuses such an export whole.
    let files: Vec<_> = export()
        .into_iter()
        .filter(|(path, _)| path != &schema_dir().join("ServerNotification.json"))
        .collect();
    assert_eq!(judged(&files), None);
    assert!(read(&files).is_err_and(|e| e.contains("ServerNotification.json is missing")));

    // Work cut short decides nothing.
    let files = export();
    assert_eq!(
        app_server_runs_commands(
            file(&files, "ClientRequest.json").as_ref(),
            file(&files, "ServerNotification.json").as_ref(),
            &Work::new(0, None),
        ),
        None
    );
}

#[test]
fn every_entry_carries_command_execution() {
    assert!(CAPABILITIES.contains(&"command_execution"));
}
