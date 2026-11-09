//! Installing HoH (REQ-005, docs/adr/ADR-023.md): the binary's `__unit` entry and the installer's file layer, in a
//! temporary HOME with no user manager reachable (an empty XDG_RUNTIME_DIR). The unit's behaviour under systemd is
//! tested under a private user manager (`systemd.rs`), and the whole release bundle by TEST-005-06 (`deployment.rs`).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod installed;

use installed::{R, script, sha256, stage_bundle};
use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `hoh <args>` with only `vars` in its environment, in `dir`.
fn hoh(args: &[&str], vars: &[(&str, &Path)], dir: &Path) -> R<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hoh"));
    command.args(args).current_dir(dir).env_clear();
    command.env("PATH", "/usr/bin:/bin");
    for (key, value) in vars {
        command.env(key, value);
    }
    Ok(command.output()?)
}

#[test]
fn the_unit_entry_refuses_a_wrong_command_line_with_64_and_is_in_no_help() -> R {
    let home = tempfile::tempdir()?;
    let vars = [("HOME", home.path())];
    for args in [
        &["__unit"][..],
        &["__unit", "x"],
        &["__unit", "template", "x"],
        &["__unit", "enable"],
        &["__unit", "enable", "a", "b"],
    ] {
        let out = hoh(args, &vars, home.path())?;
        assert_eq!(out.status.code(), Some(64), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    let missing = home.path().join("missing");
    let out = hoh(
        &["__unit", "enable", missing.to_str().ok_or("path")?],
        &vars,
        home.path(),
    )?;
    assert_eq!(out.status.code(), Some(64));
    for args in [&["--help"][..], &["help", "doctor"], &["attach", "--help"]] {
        let out = hoh(args, &vars, home.path())?;
        assert!(
            !String::from_utf8(out.stdout)?.contains("__unit"),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn the_unit_entry_prints_the_template_for_its_home_or_refuses_one_systemd_cannot_use() -> R {
    let root = tempfile::tempdir()?;
    let home = root.path().join("a home %1");
    std::fs::create_dir(&home)?;
    let out = hoh(&["__unit", "template"], &[("HOME", &home)], root.path())?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout)?;
    assert_eq!(text, hoh_app::unit::template(&home)?);
    let exec = format!(
        "ExecStart=\"{}/a home %%1/.local/bin/hoh\" __host ${{HOH_PROJECT}}",
        root.path().display()
    );
    assert!(text.lines().any(|line| line == exec), "{text}");
    let quoted = root.path().join("a\"b");
    let out = hoh(&["__unit", "template"], &[("HOME", &quoted)], root.path())?;
    assert_eq!(out.status.code(), Some(78));
    assert!(out.stdout.is_empty());
    let out = hoh(&["__unit", "template"], &[], root.path())?;
    assert_eq!(out.status.code(), Some(78), "HOME unset");
    Ok(())
}

#[test]
fn enabling_a_project_with_no_user_manager_to_ask_is_refused_with_69_and_writes_nothing() -> R {
    let root = tempfile::tempdir()?;
    let (home, runtime, project) = (
        root.path().join("h"),
        root.path().join("r"),
        root.path().join("p"),
    );
    for dir in [&home, &runtime, &project] {
        std::fs::create_dir(dir)?;
    }
    let vars = [
        ("HOME", home.as_path()),
        ("XDG_RUNTIME_DIR", runtime.as_path()),
    ];
    let out = hoh(
        &["__unit", "enable", project.to_str().ok_or("path")?],
        &vars,
        root.path(),
    )?;
    assert_eq!(
        out.status.code(),
        Some(69),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8(out.stderr)?.contains("does not answer"),
        "names why"
    );
    for dir in [&home, &runtime, &project] {
        assert_eq!(std::fs::read_dir(dir)?.count(), 0, "{}", dir.display());
    }
    Ok(())
}

/// What a path is, as compared before and after: kind, mode, size, modification time, link target, content digest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    kind: char,
    mode: u32,
    size: u64,
    mtime: (i64, i64),
    target: Option<PathBuf>,
    digest: Option<String>,
}

/// Every path under `dir` (relative), with what it is; equal before and after means nothing was written there.
fn tree(dir: &Path) -> R<BTreeMap<PathBuf, Entry>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(at) = todo.pop() {
        for item in std::fs::read_dir(&at)? {
            let path = item?.path();
            let meta = std::fs::symlink_metadata(&path)?;
            let kind = if meta.is_symlink() {
                'l'
            } else if meta.is_dir() {
                todo.push(path.clone());
                'd'
            } else {
                'f'
            };
            out.insert(
                path.strip_prefix(dir)?.to_path_buf(),
                Entry {
                    kind,
                    mode: meta.permissions().mode(),
                    size: if kind == 'd' { 0 } else { meta.len() },
                    mtime: (meta.mtime(), meta.mtime_nsec()),
                    target: (kind == 'l')
                        .then(|| std::fs::read_link(&path))
                        .transpose()?,
                    digest: (kind == 'f')
                        .then(|| std::fs::read(&path).map(|b| sha256(&b)))
                        .transpose()?,
                },
            );
        }
    }
    Ok(out)
}

/// A home to install into and a bundle to install from, laid out as `scripts/product/bundle.sh` lays it out (this
/// build's binary, the repository's install.sh), with a `sudo` that only records that it was called.
struct Install {
    root: tempfile::TempDir,
}

impl Install {
    fn new() -> R<Install> {
        let install = Install {
            root: tempfile::tempdir()?,
        };
        for dir in ["h", "r", "stubs"] {
            std::fs::create_dir(install.root.path().join(dir))?;
        }
        install.bundle("bundle", b"")?;
        script(
            &install.stubs().join("sudo"),
            &format!(
                "#!/bin/sh\necho \"$@\" >> {}/sudo-called\nexit 1\n",
                install.root.path().display()
            ),
        )?;
        Ok(install)
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn stubs(&self) -> PathBuf {
        self.root.path().join("stubs")
    }

    fn data(&self) -> PathBuf {
        self.home().join(".local/share")
    }

    /// Stage a bundle at `name/hoh`, its binary this build's with `extra` bytes after it (still runnable; another
    /// version by its digest).
    fn bundle(&self, name: &str, extra: &[u8]) -> R<PathBuf> {
        stage_bundle(&self.root.path().join(name), extra)
    }

    /// `<bundle>/install.sh <args>` as this user, in a clean environment with no user manager to ask.
    fn run(&self, bundle: &str, args: &[&str]) -> R<Output> {
        let script = self.root.path().join(bundle).join("hoh/install.sh");
        Ok(Command::new("bash")
            .arg(script)
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .env("HOME", self.home())
            .env("XDG_RUNTIME_DIR", self.root.path().join("r"))
            .env("USER", "tester")
            .env("PATH", format!("{}:/usr/bin:/bin", self.stubs().display()))
            .output()?)
    }

    fn sudo_called(&self) -> bool {
        self.root.path().join("sudo-called").exists()
    }
}

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The version directory a bundle's binary gets: `<semver>-<first 12 hex digits of its SHA-256>`.
fn version_of(binary: &Path) -> R<String> {
    let out = Command::new(binary).arg("--version").output()?;
    let semver = String::from_utf8(out.stdout)?
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("hoh "))
        .ok_or("no version")?
        .to_owned();
    let digest = sha256(&std::fs::read(binary)?);
    Ok(format!("{semver}-{}", &digest[..12]))
}

#[test]
fn an_install_lays_out_a_version_a_current_link_the_command_and_the_template() -> R {
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let stdout = String::from_utf8(out.stdout.clone())?;
    assert!(stdout.contains("installed hoh "), "{}", text(&out));
    assert!(
        stdout.lines().any(|l| l.starts_with("linger: ")),
        "{}",
        text(&out)
    );
    let name = version_of(&install.root.path().join("bundle/hoh/bin/hoh"))?;
    let root = install.data().join("hoh");
    let version = root.join("versions").join(&name);
    for file in [
        "bin/hoh",
        "install.sh",
        "LICENSE",
        "THIRD_PARTY_NOTICES.md",
        "resources/README.md",
    ] {
        assert!(version.join(file).is_file(), "{file}");
    }
    assert_ne!(
        std::fs::metadata(version.join("bin/hoh"))?
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(
        std::fs::read_link(root.join("current"))?,
        Path::new("versions").join(&name)
    );
    let command = install.home().join(".local/bin/hoh");
    assert_eq!(std::fs::read_link(&command)?, root.join("current/bin/hoh"));
    // AC-005-06: `command -v hoh` is $HOME/.local/bin/hoh, and it runs.
    let found = Command::new("sh")
        .args(["-c", "command -v hoh"])
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:/usr/bin:/bin",
                install.home().join(".local/bin").display()
            ),
        )
        .output()?;
    assert_eq!(
        String::from_utf8(found.stdout)?.trim_end(),
        command.to_str().ok_or("path")?
    );
    let version_out = Command::new(&command)
        .arg("--version")
        .env_clear()
        .output()?;
    assert!(String::from_utf8(version_out.stdout)?.starts_with("hoh "));
    let template = install.data().join("systemd/user/hoh-host@.service");
    assert_eq!(
        std::fs::read_to_string(&template)?,
        hoh_app::unit::template(&install.home())?
    );
    // Nothing else in the home: the command, the installation and the template (with their directories).
    let paths: Vec<PathBuf> = tree(&install.home())?
        .into_iter()
        .filter(|(_, e)| e.kind != 'd')
        .map(|(p, _)| p)
        .filter(|p| !p.starts_with(".local/share/hoh/versions"))
        .collect();
    assert_eq!(
        paths,
        [
            PathBuf::from(".local/bin/hoh"),
            PathBuf::from(".local/share/hoh/current"),
            PathBuf::from(".local/share/systemd/user/hoh-host@.service"),
        ]
    );
    assert_eq!(
        std::fs::read_dir(root.join("versions"))?.count(),
        1,
        "no temporary left"
    );
    assert!(!install.sudo_called());
    Ok(())
}

#[test]
fn installing_the_same_bundle_again_changes_nothing() -> R {
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let before = tree(&install.home())?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(tree(&install.home())?, before);
    Ok(())
}

#[test]
fn a_new_bundle_gets_a_version_of_its_own_and_current_moves_to_it() -> R {
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let old = version_of(&install.root.path().join("bundle/hoh/bin/hoh"))?;
    let template = install.data().join("systemd/user/hoh-host@.service");
    let template_before = tree(template.parent().ok_or("dir")?)?;
    let second = install.bundle("second", b"\0another build")?;
    let new = version_of(&second.join("bin/hoh"))?;
    assert_ne!(old, new);
    let out = install.run("second", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let root = install.data().join("hoh");
    // The old version stays (a running host and its agents use it until they restart); current names the new one.
    assert!(root.join("versions").join(&old).join("bin/hoh").is_file());
    assert!(root.join("versions").join(&new).join("bin/hoh").is_file());
    assert_eq!(
        std::fs::read_link(root.join("current"))?,
        Path::new("versions").join(&new)
    );
    assert_eq!(
        std::fs::read(install.home().join(".local/bin/hoh"))?,
        std::fs::read(second.join("bin/hoh"))?
    );
    // The same home gives the same template: not rewritten.
    assert_eq!(tree(template.parent().ok_or("dir")?)?, template_before);
    Ok(())
}

/// How a test puts something at a path.
type Make = fn(&Path) -> std::io::Result<()>;

#[test]
fn a_command_that_is_not_this_installations_is_never_replaced() -> R {
    let makes: [(&str, Make); 2] = [
        ("a file", |p| std::fs::write(p, "#!/bin/sh\necho mine\n")),
        ("a link elsewhere", |p| {
            std::os::unix::fs::symlink("/usr/bin/true", p)
        }),
    ];
    for (what, make) in makes {
        let install = Install::new()?;
        let command = install.home().join(".local/bin/hoh");
        std::fs::create_dir_all(command.parent().ok_or("dir")?)?;
        make(&command)?;
        let before = tree(&install.home())?;
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(1), "{what}: {}", text(&out));
        assert!(
            String::from_utf8(out.stderr)?.contains("not this installation's"),
            "{what}"
        );
        assert_eq!(tree(&install.home())?, before, "{what}");
    }
    Ok(())
}

#[test]
fn root_is_refused_before_anything_is_written() -> R {
    let install = Install::new()?;
    script(
        &install.stubs().join("id"),
        "#!/bin/sh\n[ \"$1\" = -u ] && echo 0 && exit 0\necho root\n",
    )?;
    for args in [&[][..], &["--uninstall"]] {
        let out = install.run("bundle", args)?;
        assert_eq!(out.status.code(), Some(77), "{args:?}: {}", text(&out));
        assert_eq!(std::fs::read_dir(install.home())?.count(), 0, "{args:?}");
    }
    assert!(!install.sudo_called());
    Ok(())
}

#[test]
fn the_uninstall_removes_what_the_install_wrote_and_nothing_else() -> R {
    let install = Install::new()?;
    let home = install.home();
    // The user's own files: configuration, a drop-in for the unit, another command, a project with its .hoh/.
    for (file, content) in [
        (".config/hoh/config.toml", "[hoh]\n"),
        (
            ".config/systemd/user/hoh-host@.service.d/override.conf",
            "[Service]\n",
        ),
        (".local/bin/other", "#!/bin/sh\n"),
        (".local/share/other/data", "x"),
        ("project/.hoh/state.db", "db"),
    ] {
        let at = home.join(file);
        std::fs::create_dir_all(at.parent().ok_or("dir")?)?;
        std::fs::write(at, content)?;
    }
    let before = tree(&home)?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        String::from_utf8(out.stderr)?.is_empty(),
        "a drop-in is not an override"
    );
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let after = tree(&home)?;
    // Every file and link is as before; the install's directories may stay, empty (as ~/.local/share/systemd/user).
    let files = |t: &BTreeMap<PathBuf, Entry>| -> BTreeMap<PathBuf, Entry> {
        t.iter()
            .filter(|(_, e)| e.kind != 'd')
            .map(|(p, e)| (p.clone(), e.clone()))
            .collect()
    };
    assert_eq!(files(&after), files(&before));
    for (path, entry) in &after {
        if !before.contains_key(path) {
            assert_eq!(entry.kind, 'd', "{}", path.display());
            assert!(
                [".local/share/systemd", ".local/share/systemd/user"]
                    .iter()
                    .any(|d| path == Path::new(d)),
                "{} left",
                path.display()
            );
        }
    }
    assert!(!install.sudo_called());
    Ok(())
}

#[test]
fn uninstalling_what_is_not_installed_does_nothing() -> R {
    let install = Install::new()?;
    let before = tree(&install.home())?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(String::from_utf8(out.stdout)?, "not installed\n");
    assert_eq!(tree(&install.home())?, before);
    Ok(())
}

#[test]
fn enabling_a_project_with_no_user_manager_or_a_wrong_command_line_writes_nothing() -> R {
    let install = Install::new()?;
    let project = install.root.path().join("p");
    std::fs::create_dir(&project)?;
    let p = project.to_str().ok_or("path")?;
    for (args, code) in [
        (&["--project", p][..], 69),
        (&["--project"], 64),
        (&["--uninstall", "--linger"], 64),
        (&["--uninstall", "--project", p], 64),
        (&["--bogus"], 64),
    ] {
        let out = install.run("bundle", args)?;
        assert_eq!(out.status.code(), Some(code), "{args:?}: {}", text(&out));
        assert_eq!(std::fs::read_dir(install.home())?.count(), 0, "{args:?}");
    }
    assert_eq!(std::fs::read_dir(&project)?.count(), 0);
    Ok(())
}
