use super::*;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;

#[test]
fn the_instance_is_the_first_16_hex_digits_of_the_sha256_of_the_path() {
    // Fixed vectors (python3 hashlib), so the installer, the client and a test computing it on their own agree.
    assert_eq!(instance(Path::new("/home/u/p")), "60ad706fe5335804");
    assert_eq!(instance(Path::new("/tmp/项目 x")), "857201194ecbb31a");
    assert_eq!(
        unit_name(&instance(Path::new("/home/u/p"))),
        "hoh-host@60ad706fe5335804.service"
    );
}

/// The `key=value` lines of `section` in a unit file, in order.
fn section<'a>(text: &'a str, name: &str) -> Vec<(&'a str, &'a str)> {
    let mut inside = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('[') {
            inside = line == format!("[{name}]");
        } else if inside && let Some((key, value)) = line.split_once('=') {
            out.push((key, value));
        }
    }
    out
}

#[test]
fn the_template_restarts_on_failure_within_its_limit_in_a_delegated_cgroup() {
    // REQ-005 behaviour 6 and decision 4, CR-011, ADR-023 decision 1.
    let text = template(Path::new("/home/u")).unwrap();
    assert_eq!(
        section(&text, "Unit"),
        [
            ("Description", "HoH host (project %i)"),
            ("StartLimitIntervalSec", "300"),
            ("StartLimitBurst", "5"),
        ]
    );
    assert_eq!(
        section(&text, "Service"),
        [
            ("Type", "exec"),
            ("EnvironmentFile", "%S/hoh/hosts/%i.env"),
            (
                "ExecStart",
                "\"/home/u/.local/bin/hoh\" __host ${HOH_PROJECT}"
            ),
            ("Restart", "on-failure"),
            ("RestartSec", "5"),
            ("RestartPreventExitStatus", "64 69 78"),
            ("Delegate", "yes"),
            ("KillMode", "control-group"),
            ("TimeoutStopSec", "15"),
            ("StandardInput", "null"),
            ("StandardOutput", "null"),
            ("StandardError", "append:%t/hoh-host@%i.stderr"),
        ]
    );
    assert_eq!(section(&text, "Install"), [("WantedBy", "default.target")]);
    // Every other line is a comment or a section header or empty.
    for line in text.lines() {
        assert!(
            line.is_empty() || line.starts_with('#') || line.starts_with('[') || line.contains('='),
            "{line:?}"
        );
    }
}

#[test]
fn a_percent_in_home_is_doubled_and_a_dollar_or_a_space_kept_as_it_is() {
    // Verified under a private manager (ADR-023 decision 1): `%` is a specifier, `$` in the program is not expanded.
    let text = template(Path::new("/home/a%b$c d~中")).unwrap();
    let exec = section(&text, "Service")
        .into_iter()
        .find(|(k, _)| *k == "ExecStart")
        .unwrap()
        .1;
    assert_eq!(
        exec,
        "\"/home/a%%b$c d~中/.local/bin/hoh\" __host ${HOH_PROJECT}"
    );
}

#[test]
fn a_home_systemd_cannot_run_a_program_in_gives_no_template() {
    // systemd refuses `"`, `\` and `'` in a program's path however escaped, and control characters anywhere.
    for home in [
        "/home/a\"b",
        "/home/a\\b",
        "/home/a'b",
        "/home/a\nb",
        "/home/a\tb",
        "/home/a\u{7f}b",
        "home/relative",
        "",
    ] {
        assert!(template(Path::new(home)).is_err(), "{home:?} was accepted");
    }
    let not_utf8 = Path::new(OsStr::from_bytes(b"/home/a\xffb"));
    assert!(template(not_utf8).is_err());
}

#[test]
fn the_environment_file_quotes_what_systemd_unquotes() {
    // Verified under a private manager (ADR-023 decision 2): this value reached `${HOH_PROJECT}` as one argument.
    let project = Path::new("/p/ we ird$x%y%i\"z\\w`b${HOME} ");
    let text = environment(project, OsStr::new("/usr/bin:/b$n")).unwrap();
    assert_eq!(
        text,
        "HOH_PROJECT=\"/p/ we ird\\$x%y%i\\\"z\\\\w\\`b\\${HOME} \"\nPATH=\"/usr/bin:/b\\$n\"\n"
    );
    assert_eq!(project_of(&text).as_deref(), Some(project));
}

#[test]
fn a_value_systemd_refuses_or_a_control_character_gives_no_environment_file() {
    let fine = OsStr::new("/usr/bin");
    for project in [
        "/p/a\u{feff}b",
        "/p/a\u{fdd0}b",
        "/p/a\u{fffe}b",
        "/p/a\u{10ffff}b",
        "/p/a\nb",
        "/p/a\rb",
        "/p/a\u{1b}b",
        "relative",
    ] {
        assert!(
            environment(Path::new(project), fine).is_err(),
            "{project:?} was accepted"
        );
    }
    assert!(environment(Path::new(OsStr::from_bytes(b"/p/\xff")), fine).is_err());
    assert!(environment(Path::new("/p"), OsStr::from_bytes(b"/usr/\xff")).is_err());
    assert!(environment(Path::new("/p"), OsStr::new("/usr/a\nb")).is_err());
}

#[test]
fn a_project_is_read_back_only_from_a_file_this_module_wrote() {
    for text in [
        "",
        "PATH=\"/usr/bin\"\n",
        "HOH_PROJECT=/p\nPATH=\"/usr/bin\"\n",
        "HOH_PROJECT=\"/p\nPATH=\"/usr/bin\"\n",
        "HOH_PROJECT=\"/p\\q\"\nPATH=\"/usr/bin\"\n",
    ] {
        assert_eq!(project_of(text), None, "{text:?}");
    }
}

#[test]
fn show_output_is_split_at_the_first_equals_sign() {
    let parsed =
        show("LoadState=loaded\nExecStart={ path=/a=b ; argv[]=/a=b x }\nEmpty=\n\nnoequals\n");
    assert_eq!(parsed.get("LoadState").map(String::as_str), Some("loaded"));
    assert_eq!(
        parsed.get("ExecStart").map(String::as_str),
        Some("{ path=/a=b ; argv[]=/a=b x }")
    );
    assert_eq!(parsed.get("Empty").map(String::as_str), Some(""));
    assert_eq!(parsed.len(), 3);
}

#[test]
fn the_environment_file_is_the_one_systemd_names_and_reads_without_ignoring_errors() {
    assert_eq!(
        environment_file("/s/hoh/hosts/60ad706fe5335804.env (ignore_errors=no)"),
        Some(PathBuf::from("/s/hoh/hosts/60ad706fe5335804.env"))
    );
    assert_eq!(
        environment_file("/s/a b/x.env (ignore_errors=no)"),
        Some(PathBuf::from("/s/a b/x.env"))
    );
    for value in [
        "",
        "/s/x.env (ignore_errors=yes)",
        "relative.env (ignore_errors=no)",
        "/s/x.env",
    ] {
        assert_eq!(environment_file(value), None, "{value:?}");
    }
}

#[test]
fn exec_start_names_the_installed_program_only_by_its_exact_path() {
    let bin = Path::new("/home/u/.local/bin/hoh");
    assert!(runs(
        "{ path=/home/u/.local/bin/hoh ; argv[]=/home/u/.local/bin/hoh __host ${HOH_PROJECT} ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }",
        bin
    ));
    for value in [
        "",
        "{ path=/home/u/.local/bin/hoh2 ; argv[]=/home/u/.local/bin/hoh2 __host ${HOH_PROJECT} }",
        "{ path=/home/u/.local/bin/hoh ; argv[]=/home/u/.local/bin/hoh web }",
        "{ path=/other/hoh ; argv[]=/home/u/.local/bin/hoh __host ${HOH_PROJECT} }",
    ] {
        assert!(!runs(value, bin), "{value:?}");
    }
}

#[test]
fn the_environment_file_is_written_private_and_whole_and_never_over_another_project()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir()?;
    let file = root.path().join("state/hoh/hosts/60ad706fe5335804.env");
    let mine = Path::new("/home/u/p");
    let text = environment(mine, OsStr::new("/usr/bin"))?;
    write_environment(&file, mine, &text)?;
    assert_eq!(std::fs::read_to_string(&file)?, text);
    assert_eq!(
        std::fs::metadata(&file)?.permissions().mode() & 0o777,
        0o600
    );
    let hosts = file.parent().unwrap();
    assert_eq!(
        std::fs::metadata(hosts)?.permissions().mode() & 0o777,
        0o700
    );
    // The same project again (a new PATH) replaces it; nothing else is left in the directory.
    let again = environment(mine, OsStr::new("/opt/bin:/usr/bin"))?;
    write_environment(&file, mine, &again)?;
    assert_eq!(std::fs::read_to_string(&file)?, again);
    assert_eq!(std::fs::read_dir(hosts)?.count(), 1);
    // Another project with the same instance (a hash collision) is refused, the file unchanged.
    let other = Path::new("/home/u/q");
    let theirs = environment(other, OsStr::new("/usr/bin"))?;
    match write_environment(&file, other, &theirs) {
        Err(WriteError::Collision(named)) => assert_eq!(named, mine),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&file)?, again);
    // A file this module did not write (damaged) is replaced.
    std::fs::write(&file, "garbage")?;
    write_environment(&file, other, &theirs)?;
    assert_eq!(std::fs::read_to_string(&file)?, theirs);
    assert_eq!(std::fs::read_dir(hosts)?.count(), 1);
    Ok(())
}

#[test]
fn a_process_is_supervised_by_the_hosts_unit_only_when_its_cgroup_is_one() {
    // ADR-023 decision 7: /proc/self/cgroup's last segment names the unit.
    let unit = "hoh-host@60ad706fe5335804.service";
    assert_eq!(
        supervisor_of(&format!(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/{unit}\n"
        )),
        Some(unit.to_owned())
    );
    for text in [
        "",
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/hoh-host@60ad706fe5335804.service/attempt-1\n",
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/run-u12.scope\n",
        "0::/hoh-host@60AD706FE5335804.service\n",
        "0::/hoh-host@60ad706fe533580.service\n",
        "0::/hoh-host@.service\n",
        "1:name=systemd:/hoh-host@60ad706fe5335804.service\n",
    ] {
        assert_eq!(supervisor_of(text), None, "{text:?}");
    }
}
