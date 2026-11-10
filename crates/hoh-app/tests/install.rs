//! Installing HoH (REQ-005, docs/adr/ADR-023.md): the binary's `__unit` entry and the installer's file layer, in a
//! temporary HOME with no user manager reachable (an empty XDG_RUNTIME_DIR). The unit's behaviour under systemd is
//! tested under a private user manager (`service.rs`), and the whole release bundle by TEST-005-06 (`deployment.rs`).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod installed;

use installed::{R, script, sha256, stage_bundle};
use std::collections::BTreeMap;
use std::ffi::OsStr;
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
fn the_unit_entry_gives_a_template_only_as_an_installed_binary_for_a_home_systemd_can_use() -> R {
    // ADR-023 design correction 1: the template names the installation's own instance files, found from where the
    // binary is; a binary not in an installation's version (a build's) gives none.
    let root = tempfile::tempdir()?;
    let out = hoh(
        &["__unit", "template"],
        &[("HOME", root.path())],
        root.path(),
    )?;
    assert_eq!(
        out.status.code(),
        Some(78),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8(out.stderr)?.contains("not an installed hoh"));
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let installed = install.data().join("hoh/current/bin/hoh");
    let out = hoh_at(
        &installed,
        &["__unit", "template"],
        &[("HOME", &install.home())],
    )?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let printed = String::from_utf8(out.stdout)?;
    assert_eq!(
        printed,
        hoh_app::unit::template(&install.home(), &install.data().join("hoh"))?
    );
    assert!(
        printed.lines().any(|l| l
            == format!(
                "EnvironmentFile={}/hoh/hosts/%i.env",
                install.data().display()
            )),
        "{printed}"
    );
    let quoted = install.root.path().join("a\"b");
    let out = hoh_at(&installed, &["__unit", "template"], &[("HOME", &quoted)])?;
    assert_eq!(out.status.code(), Some(78));
    assert!(out.stdout.is_empty());
    let out = hoh_at(&installed, &["__unit", "template"], &[])?;
    assert_eq!(out.status.code(), Some(78), "HOME unset");
    Ok(())
}

/// `program <args>` with only `vars` and a plain PATH.
fn hoh_at(program: &Path, args: &[&str], vars: &[(&str, &Path)]) -> R<Output> {
    let mut command = Command::new(program);
    command.args(args).env_clear().env("PATH", "/usr/bin:/bin");
    for (key, value) in vars {
        command.env(key, value);
    }
    Ok(command.output()?)
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
        self.run_with(bundle, args, &[])
    }

    /// As [`Install::run`], with `env` set over the clean environment.
    fn run_with(&self, bundle: &str, args: &[&str], env: &[(&str, &OsStr)]) -> R<Output> {
        self.run_script(
            &self.root.path().join(bundle).join("hoh/install.sh"),
            args,
            env,
        )
    }

    /// `script <args>` as [`Install::run_with`] runs a bundle's install.sh.
    fn run_script(&self, script: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> R<Output> {
        let mut command = Command::new("bash");
        command
            .arg(script)
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .env("HOME", self.home())
            .env("XDG_RUNTIME_DIR", self.root.path().join("r"))
            .env("USER", "tester")
            .env("PATH", format!("{}:/usr/bin:/bin", self.stubs().display()));
        for (name, value) in env {
            command.env(name, value);
        }
        Ok(command.output()?)
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
        hoh_app::unit::template(&install.home(), &install.data().join("hoh"))?
    );
    // Nothing else in the home: the command, the installers' lock (beside the installation, design correction 7), the
    // installation (with its lock, which every run of it holds, correction 2) and the template (with their
    // directories).
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
            PathBuf::from(".local/share/hoh/.lock"),
            PathBuf::from(".local/share/hoh/current"),
            PathBuf::from(".local/share/hoh.lock"),
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
fn a_user_that_cannot_be_told_is_refused_before_anything_is_written() -> R {
    // Design correction 11: `id -u` failing (a stand-in) cannot tell whether root runs this: 69, nothing written;
    // before, its empty answer was taken for another user's.
    let install = Install::new()?;
    script(
        &install.stubs().join("id"),
        "#!/bin/sh\necho 'id: failing' >&2\nexit 1\n",
    )?;
    for args in [&[][..], &["--uninstall"]] {
        let out = install.run("bundle", args)?;
        assert_eq!(out.status.code(), Some(69), "{args:?}: {}", text(&out));
        assert!(
            String::from_utf8(out.stderr.clone())?.contains("cannot tell which user"),
            "{args:?}: {}",
            text(&out)
        );
        assert_eq!(std::fs::read_dir(install.home())?.count(), 0, "{args:?}");
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
    // Every file and link is as before but the installers' lock, which stays (ADR-023 design correction 7); the
    // install's directories may stay, empty (as ~/.local/share/systemd/user).
    let files = |t: &BTreeMap<PathBuf, Entry>| -> BTreeMap<PathBuf, Entry> {
        t.iter()
            .filter(|(_, e)| e.kind != 'd')
            .map(|(p, e)| (p.clone(), e.clone()))
            .collect()
    };
    let mut kept = files(&after);
    assert!(
        kept.remove(Path::new(".local/share/hoh.lock"))
            .is_some_and(|e| e.kind == 'f' && e.size == 0),
        "the installers' lock stays, empty"
    );
    assert_eq!(kept, files(&before));
    for (path, entry) in &after {
        if !before.contains_key(path) && path != Path::new(".local/share/hoh.lock") {
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
    // Design correction 8 (M2T14R-550): in a usual home, its data directory there, an uninstall that finds nothing
    // installed writes nothing, not even the installers' lock.
    let install = Install::new()?;
    std::fs::create_dir_all(install.data())?;
    let before = tree(&install.home())?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(String::from_utf8(out.stdout)?, "not installed\n");
    assert_eq!(tree(&install.home())?, before);
    Ok(())
}

#[test]
fn a_bundle_without_its_binary_is_refused_before_anything_is_written() -> R {
    // Design correction 8 (M2T14R-550): the bundle's missing binary is one of the refusals before the installers' lock.
    let install = Install::new()?;
    std::fs::remove_file(install.root.path().join("bundle/hoh/bin/hoh"))?;
    let before = tree(&install.home())?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(64), "{}", text(&out));
    assert!(
        String::from_utf8(out.stderr.clone())?.contains("bin/hoh is missing"),
        "{}",
        text(&out)
    );
    assert_eq!(tree(&install.home())?, before);
    Ok(())
}

#[test]
fn a_home_the_unit_cannot_name_is_refused_before_any_installation_file_is_written() -> R {
    // Design correction 9 (M2T14R-601, -650): one rule for what a unit can name, the template generator's, applied by
    // the bundle's binary (`__unit check`) after the installers' lock and before anything of the installation is
    // written: a HOME that is not UTF-8 or has a control character, U+FEFF or a noncharacter is refused (78), leaving
    // only the installers' lock file. Before, the install wrote its version, `current` and the command link, then
    // failed at the template.
    use std::os::unix::ffi::OsStrExt as _;
    let install = Install::new()?;
    let names: Vec<&[u8]> = vec![
        b"h\tt",
        b"h\nn",
        b"h\x7f",
        "h\u{85}".as_bytes(),
        "h\u{feff}".as_bytes(),
        "h\u{fdd0}".as_bytes(),
        "h\u{fffe}".as_bytes(),
        b"h\xff",
    ];
    for name in names {
        let home = install.root.path().join(std::ffi::OsStr::from_bytes(name));
        std::fs::create_dir(&home)?;
        let out = Command::new("bash")
            .arg(install.root.path().join("bundle/hoh/install.sh"))
            .current_dir(install.root.path())
            .env_clear()
            .env("HOME", &home)
            .env("XDG_RUNTIME_DIR", install.root.path().join("r"))
            .env("USER", "tester")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", install.stubs().display()),
            )
            .output()?;
        assert_eq!(out.status.code(), Some(78), "{name:?}: {}", text(&out));
        let written: Vec<PathBuf> = tree(&home)?
            .into_iter()
            .filter(|(_, e)| e.kind != 'd')
            .map(|(p, _)| p)
            .collect();
        assert_eq!(
            written,
            [PathBuf::from(".local/share/hoh.lock")],
            "{name:?}: {}",
            text(&out)
        );
    }
    // The root the installed binary will name is its canonical path: a plain XDG_DATA_HOME that is a link to a
    // directory with a TAB is refused the same way (the lock file is written through the link, nothing else).
    let target = install.root.path().join("d\tx");
    std::fs::create_dir(&target)?;
    let data = install.root.path().join("data");
    std::os::unix::fs::symlink(&target, &data)?;
    let out = Command::new("bash")
        .arg(install.root.path().join("bundle/hoh/install.sh"))
        .current_dir(install.root.path())
        .env_clear()
        .env("HOME", install.home())
        .env("XDG_DATA_HOME", &data)
        .env("XDG_RUNTIME_DIR", install.root.path().join("r"))
        .env("USER", "tester")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", install.stubs().display()),
        )
        .output()?;
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    let written: Vec<PathBuf> = tree(&target)?
        .into_iter()
        .filter(|(_, e)| e.kind != 'd')
        .map(|(p, _)| p)
        .collect();
    assert_eq!(written, [PathBuf::from("hoh.lock")], "{}", text(&out));
    assert!(!install.home().join(".local/bin/hoh").is_symlink());
    Ok(())
}

#[test]
fn a_binary_that_fails_with_a_code_of_its_own_ends_the_install_with_the_installers() -> R {
    // Design correction 11 (M2T14R-851): hoh refusing or failing with a code install.sh gives no meaning to (a stand-in
    // binary: 101, a panic's; 1, a foreign command link's) ends the install with the step's own code, the binary's
    // named: 64 for the bundle's check, 69 for the installed binary's template; 78 is passed on as it is.
    let install = Install::new()?;
    let binary = install.root.path().join("bundle/hoh/bin/hoh");
    for (check, template, code, said) in [
        ("exit 101", "exit 0", 64, "(exit 101)"),
        ("exit 0", "exit 101", 69, "(exit 101)"),
        ("exit 0", "exit 1", 69, "(exit 1)"),
        ("exit 78", "exit 0", 78, "refused this home or root"),
        // Its own codes kept, with no code named (M2T14R-954).
        ("exit 69", "exit 0", 69, "refused this home or root"),
        ("exit 0", "exit 64", 64, "__unit template refused"),
    ] {
        std::fs::remove_file(&binary)?;
        script(
            &binary,
            &format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n'--version '*) printf 'hoh 0.0.0\\ncommit test\\n';;\n'__unit check') {check};;\n'__unit template') echo panicked >&2; {template};;\nesac\n"
            ),
        )?;
        let out = install.run("bundle", &[])?;
        assert_eq!(
            out.status.code(),
            Some(code),
            "{check} {template}: {}",
            text(&out)
        );
        let stderr = String::from_utf8(out.stderr.clone())?;
        assert!(stderr.contains(said), "{check} {template}: {stderr}");
        assert_eq!(
            stderr.contains("(exit "),
            said.starts_with("(exit"),
            "{check} {template}: {stderr}"
        );
    }
    Ok(())
}

#[test]
fn a_project_that_is_not_a_directory_is_refused_before_anything_is_written() -> R {
    // M2T14R-853: --project naming no directory needs no program to tell: refused (64) with the reason before
    // anything is written; before, the whole installation was written and `__unit enable` refused it afterwards.
    let install = Install::new()?;
    let before = tree(&install.home())?;
    let missing = install.root.path().join("no-such-project");
    let out = install.run("bundle", &["--project", missing.to_str().ok_or("path")?])?;
    assert_eq!(out.status.code(), Some(64), "{}", text(&out));
    assert!(
        String::from_utf8(out.stderr.clone())?.contains("is not a directory"),
        "{}",
        text(&out)
    );
    assert_eq!(tree(&install.home())?, before);
    Ok(())
}

#[test]
fn a_root_that_cannot_be_resolved_is_refused_with_78_before_anything_is_written() -> R {
    // M2T14R-753 (design correction 10): an installation root that is a link to itself, or to nothing, cannot be
    // resolved, so the unit cannot name it: 78 with the reason, nothing written, whether or not this coreutils'
    // `readlink -m` fails on a loop (it does here, not on ubuntu-24.04). Before, `readlink -m` failing ended the
    // install with 1 (the code of a foreign command link) and no word.
    let install = Install::new()?;
    std::fs::create_dir_all(install.data())?;
    for target in ["hoh", "nowhere"] {
        let root = install.data().join("hoh");
        if root.is_symlink() {
            std::fs::remove_file(&root)?;
        }
        std::os::unix::fs::symlink(target, &root)?;
        let before = tree(&install.home())?;
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(78), "{target}: {}", text(&out));
        assert!(
            String::from_utf8(out.stderr.clone())?.contains("cannot be resolved"),
            "{target}: {}",
            text(&out)
        );
        assert_eq!(tree(&install.home())?, before, "{target}");
    }
    // `readlink -m` failing on a root that is no link (a stand-in) reaches the same refusal on any coreutils
    // (M2T14R-854: GNU's answers 0 on a loop, so the cases above reach it through the link check there).
    std::fs::remove_file(install.data().join("hoh"))?;
    script(
        &install.stubs().join("readlink"),
        "#!/bin/sh\ncase \"$1\" in -m) echo 'readlink: failing' >&2; exit 1;; esac\nexec /usr/bin/readlink \"$@\"\n",
    )?;
    let before = tree(&install.home())?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    assert!(
        String::from_utf8(out.stderr.clone())?.contains("cannot be resolved: readlink: failing"),
        "{}",
        text(&out)
    );
    assert_eq!(tree(&install.home())?, before);
    Ok(())
}

#[test]
fn a_bundle_whose_binary_does_not_run_is_refused_with_64_and_its_exit_code() -> R {
    // Design correction 9 (M2T14R-602, -651): the bundle's binary is asked its version on its own; any failure, or no
    // version named, is 64 with what it said, leaving only the installers' lock file. Before, a failing binary ended
    // the install with its own code and no word.
    let install = Install::new()?;
    let binary = install.root.path().join("bundle/hoh/bin/hoh");
    for (code, said) in [
        ("3", "exit 3"),
        ("126", "exit 126"),
        ("0", "does not name a version"),
    ] {
        std::fs::remove_file(&binary)?;
        script(
            &binary,
            &format!("#!/bin/sh\necho no-version\nexit {code}\n"),
        )?;
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(64), "{code}: {}", text(&out));
        let stderr = String::from_utf8(out.stderr.clone())?;
        assert!(stderr.contains(said), "{code}: {stderr}");
        let written: Vec<PathBuf> = tree(&install.home())?
            .into_iter()
            .filter(|(_, e)| e.kind != 'd')
            .map(|(p, _)| p)
            .collect();
        assert_eq!(written, [PathBuf::from(".local/share/hoh.lock")], "{code}");
    }
    Ok(())
}

#[test]
fn an_uninstall_whose_run_lock_another_holds_says_a_program_still_runs() -> R {
    // M2T14R-653: the run lock held shared by a process the /proc scan does not name (its program is not under the
    // root: this test's own) is the conflict: 69, the lock said held, and the user told to stop what runs; nothing is
    // removed, and the uninstall completes once the lock is let go.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let lock = std::fs::File::open(install.data().join("hoh/.lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockShared)?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains("this installation's lock is held: one of its programs still runs"),
        "{stderr}"
    );
    assert!(stderr.contains("stop what is named above"), "{stderr}");
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    drop(lock);
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    Ok(())
}

#[test]
fn an_uninstall_with_a_dangling_root_link_says_not_installed_and_writes_nothing() -> R {
    // M2T14R-654: the installer never makes the root a link; a dangling one is no installation: `not installed`, 0,
    // nothing written (before, the uninstall took the lock, wrote hoh.lock and said nothing).
    let install = Install::new()?;
    std::fs::create_dir_all(install.data())?;
    std::os::unix::fs::symlink(install.root.path().join("gone"), install.data().join("hoh"))?;
    let before = tree(&install.home())?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(String::from_utf8(out.stdout.clone())?, "not installed\n");
    assert_eq!(tree(&install.home())?, before);
    Ok(())
}

#[test]
fn an_uninstall_with_no_data_directory_takes_turns_with_a_first_install() -> R {
    // Design correction 8 (M2T14R-500): an uninstall that will act holds the installers' lock even when the data
    // directory does not exist yet. With a dangling link of this installation's as the only thing there, the uninstall
    // is held at its first removal (a stand-in rm); an install started then waits for it, and is whole once both have
    // ended. Before, the uninstall had no lock: the install ran to its end at once, and the uninstall then removed it.
    let install = Install::new()?;
    let link = install.home().join(".local/bin/hoh");
    std::fs::create_dir_all(link.parent().ok_or("no parent")?)?;
    std::os::unix::fs::symlink(install.data().join("hoh/current/bin/hoh"), &link)?;
    assert!(!install.data().exists());
    let (uninstalling, go) = paused_uninstall(&install)?;
    let mut installing = Command::new("bash")
        .arg(install.root.path().join("bundle/hoh/install.sh"))
        .current_dir(install.root.path())
        .env_clear()
        .env("HOME", install.home())
        .env("XDG_RUNTIME_DIR", install.root.path().join("r"))
        .env("USER", "tester")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", install.stubs().display()),
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    let waited = installing.try_wait()?.is_none();
    std::fs::write(&go, "")?;
    let removed = uninstalling.wait_with_output()?;
    let installed = installing.wait_with_output()?;
    assert!(
        waited,
        "the install did not wait for the uninstall\n{}\n{}",
        text(&removed),
        text(&installed)
    );
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed));
    assert_eq!(installed.status.code(), Some(0), "{}", text(&installed));
    let version = Command::new(&link).arg("--version").output()?;
    assert_eq!(
        version.status.code(),
        Some(0),
        "the install is not whole: {}\n{}",
        text(&version),
        text(&removed)
    );
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

#[test]
fn lingering_is_asked_for_only_with_linger() -> R {
    // REQ-005 behaviour 6 (M2T14R-55): logind is asked only when the user said --linger, once, for this user.
    let install = Install::new()?;
    let calls = install.root.path().join("loginctl-called");
    script(
        &install.stubs().join("loginctl"),
        &format!("#!/bin/sh\necho \"$@\" >> {}\nexit 0\n", calls.display()),
    )?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!calls.exists(), "loginctl called without --linger");
    let out = install.run("bundle", &["--linger"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(&calls)?, "enable-linger tester\n");
    assert!(!install.sudo_called());
    Ok(())
}

#[test]
fn a_root_the_unit_cannot_name_is_refused_before_anything_is_written() -> R {
    // M2T14R-101: EnvironmentFile= takes a glob, so a data directory with `[` (or `*`, `?`) cannot hold the instance
    // files the unit names; the install says so (78) and writes nothing.
    let install = Install::new()?;
    let data = install.home().join("data[1]");
    let script = install.root.path().join("bundle/hoh/install.sh");
    let out = Command::new("bash")
        .arg(script)
        .current_dir(install.root.path())
        .env_clear()
        .env("HOME", install.home())
        .env("XDG_DATA_HOME", &data)
        .env("XDG_RUNTIME_DIR", install.root.path().join("r"))
        .env("USER", "tester")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", install.stubs().display()),
        )
        .output()?;
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    assert!(String::from_utf8(out.stderr)?.contains("["));
    assert_eq!(std::fs::read_dir(install.home())?.count(), 0);
    Ok(())
}

#[test]
fn the_uninstall_says_what_it_removed_and_keeps_a_root_holding_other_files() -> R {
    // M2T14R-150: each removal is said as it happened; a file the installer did not write keeps the root, said so.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let root = install.data().join("hoh");
    std::fs::write(root.join("notes.txt"), "mine")?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(root.join("notes.txt"))?, "mine");
    for gone in ["versions", "current", "hosts", ".lock"] {
        assert!(
            !root.join(gone).exists() && !root.join(gone).is_symlink(),
            "{gone} left"
        );
    }
    let stdout = String::from_utf8(out.stdout.clone())?;
    let stderr = String::from_utf8(out.stderr.clone())?;
    let template = install.data().join("systemd/user/hoh-host@.service");
    let link = install.home().join(".local/bin/hoh");
    assert!(
        stdout.contains(&format!("removed {}\n", template.display())),
        "{}",
        text(&out)
    );
    assert!(
        stdout.contains(&format!("removed {}\n", link.display())),
        "{}",
        text(&out)
    );
    assert!(
        !stdout.contains(&format!("removed {}\n", root.display())),
        "{}",
        text(&out)
    );
    assert!(
        stderr.contains(&format!("kept {}", root.display())),
        "{}",
        text(&out)
    );
    Ok(())
}

#[test]
fn an_enabled_instance_with_no_user_manager_to_disable_it_keeps_the_uninstall_from_removing_anything()
-> R {
    // M2T14R-151: with no manager answering, an enabled instance cannot be disabled; removing the template would leave
    // it naming nothing, so the uninstall removes nothing (69). Its reason is the first pass's, not the final pass's
    // "enabled again" (design correction 9), which would refuse it too.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let wants = install
        .home()
        .join(".config/systemd/user/default.target.wants");
    std::fs::create_dir_all(&wants)?;
    let template = install.data().join("systemd/user/hoh-host@.service");
    std::os::unix::fs::symlink(&template, wants.join("hoh-host@0123456789abcdef.service"))?;
    let before = tree(&install.home())?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains(
            "hoh-host@0123456789abcdef.service is enabled, and no user systemd manager answers to disable it"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("enabled again"), "{stderr}");
    assert_eq!(tree(&install.home())?, before);
    Ok(())
}

#[test]
fn an_instance_a_requires_link_enables_with_no_user_manager_keeps_the_uninstall_from_removing_anything()
-> R {
    // Design correction 10 (M2T14R-700): with no manager to ask, an instance enabled by any link in the user's unit
    // configuration (here a .requires one, which a drop-in's RequiredBy= makes) refuses the uninstall as a .wants one
    // does; before, only .wants links were looked at and everything was removed.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let requires = install
        .home()
        .join(".config/systemd/user/default.target.requires");
    std::fs::create_dir_all(&requires)?;
    let template = install.data().join("systemd/user/hoh-host@.service");
    std::os::unix::fs::symlink(
        &template,
        requires.join("hoh-host@0123456789abcdef.service"),
    )?;
    let before = tree(&install.home())?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains(
            "hoh-host@0123456789abcdef.service is enabled, and no user systemd manager answers to disable it"
        ),
        "{stderr}"
    );
    assert_eq!(tree(&install.home())?, before);
    Ok(())
}

/// An installed home with no manager to ask, its instance `hoh-host@0123456789abcdef.service` enabled by a link that
/// `place` makes (given the home and the template); the uninstall must refuse (69), say `said`, remove nothing.
/// `place` gives the directories it made unreadable, which get their permissions back before the temporary root is
/// removed, whatever ends the test; the root is then gone (M2T14R-904).
fn an_uninstall_with_no_manager_refuses(
    place: impl Fn(&Path, &Path) -> R<Vec<PathBuf>>,
    said: &str,
) -> R<String> {
    let install = Install::new()?;
    let top = install.root.path().to_path_buf();
    let stderr = {
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        let template = install.data().join("systemd/user/hoh-host@.service");
        let _restore = installed::Restore(place(install.root.path(), &template)?);
        let out = install.run("bundle", &["--uninstall"])?;
        assert_eq!(out.status.code(), Some(69), "{}", text(&out));
        let stderr = String::from_utf8(out.stderr.clone())?;
        assert!(stderr.contains(said), "{stderr}");
        drop(_restore);
        assert!(install.data().join("hoh/current/bin/hoh").exists());
        assert!(template.exists());
        stderr
    };
    drop(install);
    assert!(!top.exists(), "{} left", top.display());
    Ok(stderr)
}

#[test]
fn a_link_where_the_user_unit_directory_is_a_link_keeps_the_uninstall_from_removing_anything() -> R
{
    // Design correction 11 (M2T14R-802): ~/.config/systemd/user is a link to another directory; the link enabling the
    // instance there is found (find follows the directory) and refuses as before correction 10.
    an_uninstall_with_no_manager_refuses(
        |root, template| {
            let real = root.join("elsewhere");
            std::fs::create_dir_all(real.join("default.target.wants"))?;
            std::os::unix::fs::symlink(
                template,
                real.join("default.target.wants/hoh-host@0123456789abcdef.service"),
            )?;
            std::fs::create_dir_all(root.join("h/.config/systemd"))?;
            std::os::unix::fs::symlink(&real, root.join("h/.config/systemd/user"))?;
            Ok(vec![])
        },
        "hoh-host@0123456789abcdef.service is enabled, and no user systemd manager answers to disable it",
    )
    .map(drop)
}

#[test]
fn a_link_in_a_wants_directory_that_is_a_link_keeps_the_uninstall_from_removing_anything() -> R {
    // Design correction 12 (M2T14R-950): default.target.wants itself a link to another directory; the link enabling
    // the instance there is read (systemd follows such a directory), and with no manager it refuses.
    an_uninstall_with_no_manager_refuses(
        |root, template| {
            let real = root.join("elsewhere-wants");
            std::fs::create_dir_all(&real)?;
            std::os::unix::fs::symlink(template, real.join("hoh-host@0123456789abcdef.service"))?;
            let user = root.join("h/.config/systemd/user");
            std::fs::create_dir_all(&user)?;
            std::os::unix::fs::symlink(&real, user.join("default.target.wants"))?;
            Ok(vec![])
        },
        "hoh-host@0123456789abcdef.service is enabled, and no user systemd manager answers to disable it",
    )
    .map(drop)
}

/// Move `dir` into `opaque` (made here), leave a link to it at `dir`, and make `opaque` unsearchable: the link is seen,
/// what it reaches is not. The directory to restore is returned.
fn linked_where_it_cannot_be_reached(dir: &Path, opaque: &Path) -> R<Vec<PathBuf>> {
    std::fs::create_dir_all(opaque)?;
    let name = dir.file_name().ok_or("a directory with a name")?;
    std::fs::rename(dir, opaque.join(name))?;
    std::os::unix::fs::symlink(opaque.join(name), dir)?;
    std::fs::set_permissions(opaque, std::fs::Permissions::from_mode(0o000))?;
    Ok(vec![opaque.to_path_buf()])
}

#[test]
fn a_unit_directory_linked_where_it_cannot_be_reached_keeps_the_uninstall_from_removing_anything()
-> R {
    // Design correction 13 (M2T14R-1000, -1050): ~/.config/systemd/user, or ~/.config/systemd on its way, a link into
    // a directory that cannot be searched, with a link enabling an instance in it. What the link reaches is neither
    // there nor not there; before, a link whose target `-e` did not see was taken for a link to nothing (or for no
    // directory on the way), and the uninstall removed all but the enabling link.
    for linked in [".config/systemd/user", ".config/systemd"] {
        let stderr = an_uninstall_with_no_manager_refuses(
            |root, template| {
                let wants = root.join("h/.config/systemd/user/default.target.wants");
                std::fs::create_dir_all(&wants)?;
                std::os::unix::fs::symlink(
                    template,
                    wants.join("hoh-host@0123456789abcdef.service"),
                )?;
                linked_where_it_cannot_be_reached(
                    &root.join("h").join(linked),
                    &root.join("opaque"),
                )
            },
            ".config/systemd/user is there, where a link may enable an instance",
        )?;
        assert!(stderr.contains("could not be told"), "{linked}: {stderr}");
    }
    Ok(())
}

#[test]
fn a_wants_directory_linked_where_it_cannot_be_reached_keeps_the_uninstall_from_removing_anything()
-> R {
    // Design correction 13 (M2T14R-1000): default.target.wants a link into a directory that cannot be searched; whether
    // it is a directory, and so may hold a link enabling an instance, cannot be told (before, `-d` said no and it was
    // skipped). The link's target is relative: it is read against the link's own directory.
    let stderr = an_uninstall_with_no_manager_refuses(
        |root, template| {
            let user = root.join("h/.config/systemd/user");
            let opaque = root.join("opaque");
            std::fs::create_dir_all(opaque.join("wants"))?;
            std::fs::create_dir_all(&user)?;
            std::os::unix::fs::symlink(
                template,
                opaque.join("wants/hoh-host@0123456789abcdef.service"),
            )?;
            std::os::unix::fs::symlink(
                "../../../../opaque/wants",
                user.join("default.target.wants"),
            )?;
            std::fs::set_permissions(&opaque, std::fs::Permissions::from_mode(0o000))?;
            Ok(vec![opaque])
        },
        "default.target.wants is a directory, where a link may enable an instance",
    )?;
    assert!(stderr.contains("could not be told"), "{stderr}");
    Ok(())
}

#[test]
fn a_command_directory_linked_where_it_cannot_be_reached_keeps_the_uninstall_from_removing_anything()
-> R {
    // Design correction 13 (M2T14R-1000, -1050): ~/.local/bin a link into a directory that cannot be searched hides
    // whether ~/.local/bin/hoh is there, and whose it is: the uninstall and the install refuse (69), nothing removed
    // or written (before, the uninstall took it for none and removed all but it; a later install wrote a version and
    // then failed with 74).
    let install = Install::new()?;
    let top = install.root.path().to_path_buf();
    {
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        let versions = install.data().join("hoh/versions");
        let before = std::fs::read_dir(&versions)?.count();
        let _restore = installed::Restore(linked_where_it_cannot_be_reached(
            &install.home().join(".local/bin"),
            &install.root.path().join("opaque"),
        )?);
        for args in [&["--uninstall"][..], &[]] {
            let out = install.run("bundle", args)?;
            assert_eq!(out.status.code(), Some(69), "{args:?}: {}", text(&out));
            let stderr = String::from_utf8(out.stderr.clone())?;
            assert!(
                stderr.contains(".local/bin/hoh is there"),
                "{args:?}: {stderr}"
            );
            assert!(install.data().join("hoh/current/bin/hoh").exists());
            assert_eq!(std::fs::read_dir(&versions)?.count(), before, "{args:?}");
        }
    }
    assert!(install.home().join(".local/bin/hoh").is_symlink());
    drop(install);
    assert!(!top.exists(), "{} left", top.display());
    Ok(())
}

#[test]
fn a_template_directory_linked_where_it_cannot_be_reached_keeps_the_uninstall_from_removing_anything()
-> R {
    // Design correction 13 (M2T14R-1000): the template's directory a link into a directory that cannot be searched;
    // whether the template is there cannot be told (before, it was taken for gone and left, naming a removed binary).
    let stderr = an_uninstall_with_no_manager_refuses(
        |root, template| {
            let dir = template.parent().ok_or("the template's directory")?;
            linked_where_it_cannot_be_reached(dir, &root.join("opaque"))
        },
        "hoh-host@.service or",
    )?;
    assert!(stderr.contains("could not be told"), "{stderr}");
    Ok(())
}

#[test]
fn links_in_upholds_and_dot_named_directories_keep_the_uninstall_from_removing_anything() -> R {
    // Design correction 13: UpheldBy= makes its links in a .upholds directory, and a unit's name may begin with a dot
    // (systemd's unit_name_is_valid); a link enabling an instance in either is read.
    for sub in ["default.target.upholds", ".early.target.wants"] {
        an_uninstall_with_no_manager_refuses(
            |root, template| {
                let at = root.join("h/.config/systemd/user").join(sub);
                std::fs::create_dir_all(&at)?;
                std::os::unix::fs::symlink(template, at.join("hoh-host@0123456789abcdef.service"))?;
                Ok(vec![])
            },
            "hoh-host@0123456789abcdef.service is enabled, and no user systemd manager answers to disable it",
        )?;
    }
    Ok(())
}

#[test]
fn a_drop_in_directory_that_cannot_be_read_does_not_keep_the_uninstall_from_completing() -> R {
    // M2T14R-1001 (design correction 13): systemd reads links that enable a unit only in .wants, .requires and .upholds
    // directories; another unit's drop-in directory holds none, so one that cannot be read keeps nothing from being
    // removed (before: 69, "cannot list").
    let install = Install::new()?;
    let top = install.root.path().to_path_buf();
    {
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        let drop_in = install
            .home()
            .join(".config/systemd/user/unrelated.service.d");
        std::fs::create_dir_all(&drop_in)?;
        std::fs::write(drop_in.join("local.conf"), "[Service]\nEnvironment=X=1\n")?;
        std::fs::set_permissions(&drop_in, std::fs::Permissions::from_mode(0o000))?;
        let _restore = installed::Restore(vec![drop_in]);
        let out = install.run("bundle", &["--uninstall"])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        assert!(!install.data().join("hoh").exists());
    }
    drop(install);
    assert!(!top.exists(), "{} left", top.display());
    Ok(())
}

#[test]
fn a_unit_directory_that_is_a_loop_is_not_there() -> R {
    // Design correction 13: links are followed by the installer one at a time, so a loop ends where the kernel's own
    // following ends (40 links): ~/.config/systemd/user a link to itself reaches nothing, and holds no link enabling an
    // instance; the uninstall completes.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let systemd = install.home().join(".config/systemd");
    std::fs::create_dir_all(&systemd)?;
    std::os::unix::fs::symlink("user", systemd.join("user"))?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    Ok(())
}

#[test]
fn a_link_on_the_way_that_cannot_be_read_keeps_the_uninstall_from_removing_anything() -> R {
    // Design correction 13: a link the installer follows itself is read; one that cannot be read (a stand-in readlink
    // failing for it alone) cannot be told for a link to nothing: 69, nothing removed. Without the stand-in, the same
    // link to nothing is not there, and the uninstall completes.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let systemd = install.home().join(".config/systemd");
    std::fs::create_dir_all(&systemd)?;
    std::os::unix::fs::symlink(install.root.path().join("nowhere"), systemd.join("user"))?;
    script(
        &install.stubs().join("readlink"),
        "#!/bin/sh\ncase \"$*\" in *.config/systemd/user) echo 'readlink: failing' >&2; exit 1;; esac\nexec /usr/bin/readlink \"$@\"\n",
    )?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains(".config/systemd/user is there, where a link may enable an instance"),
        "{stderr}"
    );
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    std::fs::remove_file(install.stubs().join("readlink"))?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    Ok(())
}

/// `path` with a newline after it.
fn with_newline(path: &Path) -> PathBuf {
    let mut path = path.as_os_str().to_owned();
    path.push("\n");
    PathBuf::from(path)
}

#[test]
fn a_config_home_whose_path_ends_with_a_newline_is_scanned_as_it_is() -> R {
    // M2T14R-1100 (design correction 14): XDG_CONFIG_HOME an absolute path ending with a newline, a link enabling an
    // instance in its systemd/user/default.target.wants: with no manager the uninstall refuses (69), nothing removed.
    // Before, a command substitution dropped the newline, the uninstall scanned another directory and exited 0.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let config = with_newline(&install.root.path().join("conf"));
    let wants = config.join("systemd/user/default.target.wants");
    std::fs::create_dir_all(&wants)?;
    std::os::unix::fs::symlink(
        install.data().join("systemd/user/hoh-host@.service"),
        wants.join("hoh-host@0123456789abcdef.service"),
    )?;
    let out = install.run_with(
        "bundle",
        &["--uninstall"],
        &[("XDG_CONFIG_HOME", config.as_os_str())],
    )?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains(
            "hoh-host@0123456789abcdef.service is enabled, and no user systemd manager answers"
        ),
        "{stderr}"
    );
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    Ok(())
}

#[test]
fn a_root_whose_path_ends_with_a_newline_is_refused_as_it_is() -> R {
    // M2T14R-1100 (design correction 14): a root whose path, or whose resolved path, ends with a newline (XDG_DATA_HOME
    // so named; the root a link to a directory so named) is one the unit cannot take: 78, nothing of the installation
    // written (the installers' lock file only, as for every refusal by the bundle's binary). Before, the newline was
    // dropped: the install wrote into another directory, or checked another path, and exited 0.
    let install = Install::new()?;
    let data = with_newline(&install.root.path().join("data"));
    std::fs::create_dir_all(&data)?;
    let out = install.run_with("bundle", &[], &[("XDG_DATA_HOME", data.as_os_str())])?;
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    assert!(!install.root.path().join("data").exists());
    let written: Vec<_> = std::fs::read_dir(&data)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(written, ["hoh.lock"], "{}", text(&out));
    let real = with_newline(&install.root.path().join("real"));
    std::fs::create_dir_all(&real)?;
    std::fs::create_dir_all(install.data())?;
    std::os::unix::fs::symlink(&real, install.data().join("hoh"))?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    // The root is there, so its missing run lock is remade before any binary runs (install step 3): that alone.
    let written: Vec<_> = std::fs::read_dir(&real)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(written, [".lock"], "{}", text(&out));
    Ok(())
}

#[test]
fn a_command_link_whose_target_ends_with_a_newline_is_not_this_installations() -> R {
    // M2T14R-1101 (design correction 14): ~/.local/bin/hoh a link to this installation's current/bin/hoh with a newline
    // after it is another's: the install refuses (1, nothing written), and an uninstall keeps it. Before, a command
    // substitution dropped the newline: the install took it for its own (0) and the uninstall removed it.
    let install = Install::new()?;
    let bin = install.home().join(".local/bin");
    std::fs::create_dir_all(&bin)?;
    let target = with_newline(&install.data().join("hoh/current/bin/hoh"));
    std::os::unix::fs::symlink(&target, bin.join("hoh"))?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    std::fs::remove_file(bin.join("hoh"))?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    std::fs::remove_file(bin.join("hoh"))?;
    std::os::unix::fs::symlink(&target, bin.join("hoh"))?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    assert!(bin.join("hoh").is_symlink(), "another's link removed");
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains("it is not this installation's link"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn a_bundle_whose_directory_ends_with_a_newline_installs_from_it() -> R {
    // Design correction 14: the bundle's own directory is taken byte for byte too (before, `pwd -P` in a command
    // substitution lost its trailing newline, and the install said the bundle's bin/hoh was missing: 64).
    let install = Install::new()?;
    let dir = with_newline(&install.root.path().join("bundle/hoh"));
    std::fs::rename(install.root.path().join("bundle/hoh"), &dir)?;
    let out = install.run_script(&dir.join("install.sh"), &[], &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    // M2T14R-1250: its version is named by its binary's digest, not by what sha256sum prints for a file name with a
    // newline (an escaped name, so a backslash and 11 digits).
    let version = version_of(&dir.join("bin/hoh"))?;
    assert_eq!(
        std::fs::read_link(install.data().join("hoh/current"))?,
        Path::new("versions").join(&version),
        "{}",
        text(&out)
    );
    Ok(())
}

#[test]
fn a_reload_that_fails_after_the_removal_ends_the_uninstall_with_69() -> R {
    // M2T14R-1102 (design correction 14): the manager's daemon-reload failing once the template is removed (a
    // stand-in exiting 23) leaves the removed template loaded in the manager: the uninstall still removes the rest,
    // says each removal and the failed reload with what the manager said, and ends with 69 (before: 0), never saying
    // that nothing was removed.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    std::fs::create_dir_all(install.root.path().join("r/systemd"))?;
    let _socket =
        std::os::unix::net::UnixListener::bind(install.root.path().join("r/systemd/private"))?;
    script(
        &install.stubs().join("systemctl"),
        "#!/bin/sh\ncase \" $* \" in *\" daemon-reload \"*) echo manager-error >&2; exit 23;; esac\nexit 0\n",
    )?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let template = install.data().join("systemd/user/hoh-host@.service");
    let stdout = String::from_utf8(out.stdout.clone())?;
    assert!(
        stdout.contains(&format!("removed {}", template.display())),
        "{stdout}"
    );
    assert!(!install.data().join("hoh").exists() && !template.exists());
    assert!(!install.home().join(".local/bin/hoh").is_symlink());
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains("systemctl --user daemon-reload"),
        "{stderr}"
    );
    assert!(
        stderr.contains("exit 23") && stderr.contains("manager-error"),
        "{stderr}"
    );
    assert!(!stderr.contains("nothing was removed"), "{stderr}");
    Ok(())
}

#[test]
fn a_unit_directory_or_instance_directory_that_is_a_file_holds_nothing_to_refuse_for() -> R {
    // M2T14R-1103 (design correction 14): ~/.config/systemd/user a readable regular file is no directory: no link
    // enabling an instance can be in it, so with no manager the uninstall completes (before: 69, "cannot list"). The
    // same for the instances' files: R/hosts a regular file holds none (a stand-in manager that lists nothing).
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let systemd = install.home().join(".config/systemd");
    std::fs::create_dir_all(&systemd)?;
    std::fs::write(systemd.join("user"), "not a directory\n")?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let hosts = install.data().join("hoh/hosts");
    if hosts.is_dir() {
        std::fs::remove_dir_all(&hosts)?;
    }
    std::fs::write(&hosts, "not a directory\n")?;
    std::fs::create_dir_all(install.root.path().join("r/systemd"))?;
    let _socket =
        std::os::unix::net::UnixListener::bind(install.root.path().join("r/systemd/private"))?;
    script(&install.stubs().join("systemctl"), "#!/bin/sh\nexit 0\n")?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    Ok(())
}

#[test]
#[ignore = "a program of the installation: copied into its version and run by a test there"]
fn a_program_of_the_installation() {
    std::thread::sleep(std::time::Duration::from_secs(120));
}

/// A process ended when this is dropped, however the test ends.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_program_run_through_a_linked_data_directory_keeps_the_uninstall_from_removing_anything() -> R {
    // M2T14R-1150 (design correction 14): ~/.local/share a link to another directory, and a program of the installation
    // running (this test binary copied into its version, run through `current`) when the root's directory is removed by
    // hand. /proc names the program by its physical path, under the root's resolved path: the scan resolves the root
    // though it is not there, finds the program, and the uninstall removes nothing (69). Before, a root that was not
    // there was looked for by its own path only: the template and the command link were removed, exit 0.
    let install = Install::new()?;
    let real = install.root.path().join("datareal");
    std::fs::create_dir_all(&real)?;
    std::fs::create_dir_all(install.home().join(".local"))?;
    std::os::unix::fs::symlink(&real, install.data())?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let program = install.data().join("hoh/current/bin/program");
    // Made by another process, never one this process holds open for writing (ETXTBSY otherwise, `installed::executable`).
    installed::executable(&program, &[&std::env::current_exe()?])?;
    let running = Running(
        Command::new(&program)
            .args(["--exact", "a_program_of_the_installation", "--ignored"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    );
    let pid = running.0.id();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| exe.starts_with(&real)) {
        if std::time::Instant::now() > end {
            return Err("the program did not start".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::remove_dir_all(real.join("hoh"))?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(stderr.contains(&format!("pid {pid}")), "{stderr}");
    assert!(
        install
            .data()
            .join("systemd/user/hoh-host@.service")
            .exists()
    );
    assert!(install.home().join(".local/bin/hoh").is_symlink());
    drop(running);
    Ok(())
}

#[test]
fn a_program_run_through_a_root_whose_target_is_gone_keeps_the_uninstall_from_removing_anything()
-> R {
    // M2T14R-1251 (design correction 15): the root a link to <d>/inst, a program of the installation running, then <d>
    // removed with its contents. The root is resolved for the scan without any part of it having to exist (`readlink
    // -m`), so the program's physical path is found: 69 with its pid, template and link kept. Before, `readlink -f`
    // failed (<d> gone), the scan fell back to the root's own path, missed the program, and the uninstall exited 0.
    let install = Install::new()?;
    let target = install.root.path().join("d/inst");
    std::fs::create_dir_all(&target)?;
    std::fs::create_dir_all(install.data())?;
    std::os::unix::fs::symlink(&target, install.data().join("hoh"))?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let program = install.data().join("hoh/current/bin/program");
    installed::executable(&program, &[&std::env::current_exe()?])?;
    let running = Running(
        Command::new(&program)
            .args(["--exact", "a_program_of_the_installation", "--ignored"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    );
    let pid = running.0.id();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| exe.starts_with(&target))
    {
        if std::time::Instant::now() > end {
            return Err("the program did not start".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::remove_dir_all(install.root.path().join("d"))?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(stderr.contains(&format!("pid {pid}")), "{stderr}");
    assert!(
        install
            .data()
            .join("systemd/user/hoh-host@.service")
            .exists()
    );
    assert!(install.home().join(".local/bin/hoh").is_symlink());
    drop(running);
    Ok(())
}

#[test]
fn a_program_whose_path_cannot_be_read_keeps_the_uninstall_from_removing_anything() -> R {
    // M2T14R-1201 (design correction 15): a program of the installation running, and `readlink` of its
    // /proc/<pid>/exe failing (a stand-in, for that process alone). It is this user's and alive, so what it runs cannot
    // be told: 69, nothing removed. Before, `|| true` took it for an ended or another user's process, exit 0.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let program = install.data().join("hoh/current/bin/program");
    installed::executable(&program, &[&std::env::current_exe()?])?;
    let running = Running(
        Command::new(&program)
            .args(["--exact", "a_program_of_the_installation", "--ignored"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    );
    let pid = running.0.id();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| exe.ends_with("program"))
    {
        if std::time::Instant::now() > end {
            return Err("the program did not start".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    script(
        &install.stubs().join("readlink"),
        &format!(
            "#!/bin/sh\ncase \"$*\" in *\"/proc/{pid}/exe\") echo 'readlink: failing' >&2; exit 23;; esac\nexec /usr/bin/readlink \"$@\"\n"
        ),
    )?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains(&format!("cannot tell what process {pid} runs")),
        "{stderr}"
    );
    assert!(stderr.contains("could not be told"), "{stderr}");
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    drop(running);
    Ok(())
}

#[test]
fn a_process_the_kernel_says_runs_no_program_or_may_not_be_looked_at_does_not_refuse() -> R {
    // Design correction 16 (M2T14R-1350, -1351): a process whose program's path cannot be read is judged by the
    // kernel's own reason. "No such file or directory" (it has no address space: exiting, a zombie, a kernel thread,
    // or gone) and "Operation not permitted" (another user's under /proc's hidepid=1) are, like "Permission denied",
    // nothing the scan has to tell: a stand-in readlink gives each for an unrelated live process, and the uninstall
    // completes. Before, the first refused (69) unless the process was already Z or X, and so did the second.
    for reason in ["No such file or directory", "Operation not permitted"] {
        let install = Install::new()?;
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        let other = Running(
            Command::new("/bin/sh")
                .args(["-c", "exec sleep 60"])
                .stdout(std::process::Stdio::null())
                .spawn()?,
        );
        let pid = other.0.id();
        script(
            &install.stubs().join("readlink"),
            &format!(
                "#!/bin/sh\ncase \"$*\" in *\"/proc/{pid}/exe\") echo \"readlink: /proc/{pid}/exe: {reason}\" >&2; exit 1;; esac\nexec /usr/bin/readlink \"$@\"\n"
            ),
        )?;
        let out = install.run("bundle", &["--uninstall"])?;
        assert_eq!(out.status.code(), Some(0), "{reason}: {}", text(&out));
        assert!(!install.data().join("hoh").exists());
        drop(other);
    }
    Ok(())
}

#[test]
fn a_root_holding_only_a_dot_file_is_kept_and_said() -> R {
    // M2T14R-1354 (design correction 15 item 2): an entry whose name begins with a dot keeps the root: it is kept and
    // said, exit 0 (not taken for an rmdir that failed).
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    std::fs::write(install.data().join("hoh/.mine"), "")?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains("it holds files this installer did not write"),
        "{stderr}"
    );
    assert!(install.data().join("hoh/.mine").exists());
    Ok(())
}

#[test]
fn a_reload_and_a_root_that_both_fail_are_both_said_and_end_with_74() -> R {
    // M2T14R-1354 (design corrections 14 and 15): daemon-reload failing (a stand-in) and the emptied root not
    // removable (the data directory 0555): the rest is removed, both failures are said, and the uninstall ends with 74.
    let install = Install::new()?;
    let top = install.root.path().to_path_buf();
    {
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        std::fs::create_dir_all(install.root.path().join("r/systemd"))?;
        let _socket =
            std::os::unix::net::UnixListener::bind(install.root.path().join("r/systemd/private"))?;
        script(
            &install.stubs().join("systemctl"),
            "#!/bin/sh\ncase \" $* \" in *\" daemon-reload \"*) echo manager-error >&2; exit 23;; esac\nexit 0\n",
        )?;
        let data = install.data();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o555))?;
        let _restore = installed::Restore(vec![data.clone()]);
        let out = install.run("bundle", &["--uninstall"])?;
        assert_eq!(out.status.code(), Some(74), "{}", text(&out));
        let stderr = String::from_utf8(out.stderr.clone())?;
        assert!(
            stderr.contains("daemon-reload failed (exit 23): manager-error"),
            "{stderr}"
        );
        assert!(
            stderr.contains(&format!("cannot remove {}", data.join("hoh").display())),
            "{stderr}"
        );
    }
    drop(install);
    assert!(!top.exists(), "{} left", top.display());
    Ok(())
}

#[test]
fn a_final_pass_says_only_what_it_saw() -> R {
    // M2T14R-1352 (design correction 16): the final pass finds an instance not enabled but gets no answer about its
    // state (a stand-in manager whose second `show` fails); it stops and disables it and says that its state could not
    // be told, not that it was started again (nothing showed that).
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    std::fs::create_dir_all(install.data().join("hoh/hosts"))?;
    std::fs::write(install.data().join("hoh/hosts/abc.env"), "")?;
    std::fs::create_dir_all(install.root.path().join("r/systemd"))?;
    let _socket =
        std::os::unix::net::UnixListener::bind(install.root.path().join("r/systemd/private"))?;
    let count = install.root.path().join("shows");
    script(
        &install.stubs().join("systemctl"),
        &format!(
            "#!/bin/sh\ncase \" $* \" in *\" is-enabled \"*) echo disabled; exit 1;; *\" show \"*) n=$(cat {c} 2>/dev/null || echo 0); echo $((n+1)) > {c}; [ \"$n\" = 1 ] && exit 1; printf 'ActiveState=inactive\\nJob=\\n';; esac\nexit 0\n",
            c = count.display()
        ),
    )?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let stdout = String::from_utf8(out.stdout.clone())?;
    assert!(
        stdout.contains("stopped and disabled hoh-host@abc.service: its state could not be told while the uninstall ran"),
        "{stdout}"
    );
    assert!(!stdout.contains("started again"), "{stdout}");
    Ok(())
}

#[test]
fn a_zombie_of_this_user_does_not_keep_the_uninstall_from_completing() -> R {
    // Design correction 15: a process that has ended but is not yet reaped (a zombie: here a child of this test) has no
    // program path to read, and runs nothing: it is no reason to refuse.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let mut child = Command::new("/bin/true").spawn()?;
    let pid = child.id();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        if stat
            .rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z'))
        {
            break;
        }
        if std::time::Instant::now() > end {
            return Err("the child did not end".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = install.run("bundle", &["--uninstall"])?;
    let _ = child.wait();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    Ok(())
}

#[test]
fn an_instance_the_manager_says_still_runs_or_has_a_job_is_not_stopped() -> R {
    // Design correction 15 (M2T14R-1200): after a successful disable, an instance is stopped only when the manager says
    // inactive or failed with no job pending. A stand-in manager answers, once, "inactive" with a start job pending
    // (queued, not yet running), and once "active" with none: either is one that did not stop (69, said with its
    // state), nothing removed.
    for (state, job, said) in [
        ("inactive", "7", "ActiveState=inactive, job 7 pending"),
        ("active", "", "ActiveState=active, is-enabled"),
    ] {
        let install = Install::new()?;
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        std::fs::create_dir_all(install.data().join("hoh/hosts"))?;
        std::fs::write(install.data().join("hoh/hosts/abc.env"), "")?;
        std::fs::create_dir_all(install.root.path().join("r/systemd"))?;
        let _socket =
            std::os::unix::net::UnixListener::bind(install.root.path().join("r/systemd/private"))?;
        script(
            &install.stubs().join("systemctl"),
            &format!(
                "#!/bin/sh\ncase \" $* \" in *\" is-enabled \"*) echo disabled; exit 1;; *\" show \"*) printf 'Job=%s\\nActiveState=%s\\n' '{job}' '{state}';; esac\nexit 0\n"
            ),
        )?;
        let out = install.run("bundle", &["--uninstall"])?;
        assert_eq!(out.status.code(), Some(69), "{state}: {}", text(&out));
        let stderr = String::from_utf8(out.stderr.clone())?;
        assert!(
            stderr.contains(&format!(
                "could not stop and disable hoh-host@abc.service ({said}"
            )),
            "{stderr}"
        );
        assert!(install.data().join("hoh/current/bin/hoh").exists());
    }
    Ok(())
}

#[test]
fn a_root_that_is_not_there_and_cannot_be_resolved_is_looked_for_by_its_own_path() -> R {
    // Design corrections 14 and 15 (M2T14R-1052): the root a link to nothing and `readlink -m` failing for it (a
    // stand-in): nothing of it is there, so it is looked for by its own path, not taken for one that cannot be told;
    // the template and the command link are removed, exit 0.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let root = install.data().join("hoh");
    std::fs::remove_dir_all(&root)?;
    std::os::unix::fs::symlink(install.root.path().join("nowhere/at/all/hoh"), &root)?;
    script(
        &install.stubs().join("readlink"),
        "#!/bin/sh\ncase \"$1\" in -m) echo 'readlink: failing' >&2; exit 1;; esac\nexec /usr/bin/readlink \"$@\"\n",
    )?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        !install
            .data()
            .join("systemd/user/hoh-host@.service")
            .exists()
    );
    assert!(!install.home().join(".local/bin/hoh").is_symlink());
    Ok(())
}

#[test]
fn a_root_that_cannot_be_removed_ends_the_uninstall_with_74_saying_why() -> R {
    // M2T14R-1202 (design correction 15): the data directory not writable (0555; the installers' lock file in it
    // still is), so the emptied root cannot be removed: the rest is removed and said, then the failure with rmdir's own
    // words, exit 74. Before, any rmdir failure was said as "holds files this installer did not write", exit 0.
    let install = Install::new()?;
    let top = install.root.path().to_path_buf();
    {
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        let data = install.data();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o555))?;
        let _restore = installed::Restore(vec![data.clone()]);
        let out = install.run("bundle", &["--uninstall"])?;
        assert_eq!(out.status.code(), Some(74), "{}", text(&out));
        let stderr = String::from_utf8(out.stderr.clone())?;
        assert!(
            stderr.contains(&format!("cannot remove {}", data.join("hoh").display())),
            "{stderr}"
        );
        assert!(stderr.contains("Permission denied"), "{stderr}");
        assert!(!stderr.contains("did not write"), "{stderr}");
        assert!(!data.join("hoh/versions").exists() && !data.join("hoh/current").is_symlink());
    }
    drop(install);
    assert!(!top.exists(), "{} left", top.display());
    Ok(())
}

#[test]
fn a_regular_file_with_an_instances_name_enables_nothing() -> R {
    // M2T14R-1203 (design correction 15): systemd reads only symbolic links in a .wants directory ("is not a symlink,
    // ignoring"); a regular file named hoh-host@….service there enables nothing, and with no manager the uninstall
    // completes, the file left. Before, it was taken for an enabling link: 69.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let wants = install
        .home()
        .join(".config/systemd/user/default.target.wants");
    std::fs::create_dir_all(&wants)?;
    std::fs::write(wants.join("hoh-host@0123456789abcdef.service"), "")?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    assert!(wants.join("hoh-host@0123456789abcdef.service").is_file());
    Ok(())
}

#[test]
fn a_way_longer_than_the_kernel_follows_cannot_be_told() -> R {
    // M2T14R-1151 (design correction 14): ~/.config/systemd/user a link whose way has more than 40 links in all (a
    // chain of 39, then a link to a chain of 2), with a link enabling an instance at its end. The kernel's look at the
    // path fails (ELOOP), while each link the installer reads resolves on a fresh look; bash cannot tell that from a
    // change meanwhile, so it cannot be told: with no manager the uninstall refuses (69), nothing removed. Taken for
    // there, its directory would read as none and be skipped.
    let stderr = an_uninstall_with_no_manager_refuses(
        |root, template| {
            let chain = root.join("chain");
            let wants = chain.join("real/default.target.wants");
            std::fs::create_dir_all(&wants)?;
            std::os::unix::fs::symlink(template, wants.join("hoh-host@0123456789abcdef.service"))?;
            std::os::unix::fs::symlink(chain.join("real"), chain.join("e2"))?;
            std::os::unix::fs::symlink(chain.join("e2"), chain.join("e1"))?;
            std::fs::create_dir_all(chain.join("dir"))?;
            std::os::unix::fs::symlink(chain.join("e1"), chain.join("dir/u"))?;
            std::os::unix::fs::symlink(chain.join("dir"), chain.join("c39"))?;
            for n in (1..39).rev() {
                std::os::unix::fs::symlink(
                    chain.join(format!("c{}", n + 1)),
                    chain.join(format!("c{n}")),
                )?;
            }
            std::fs::create_dir_all(root.join("h/.config/systemd"))?;
            std::os::unix::fs::symlink(chain.join("c1/u"), root.join("h/.config/systemd/user"))?;
            Ok(vec![])
        },
        ".config/systemd/user is there, where a link may enable an instance",
    )?;
    assert!(stderr.contains("could not be told"), "{stderr}");
    Ok(())
}

#[test]
fn a_unit_directory_under_a_file_is_not_there() -> R {
    // M2T14R-1054 (design corrections 12 and 13): a path whose nearest ancestor that is there is no directory cannot
    // be there; ~/.config a regular file (mode 0644, which no one can search) holds no link enabling an instance, and
    // the uninstall completes.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let config = install.home().join(".config");
    if config.exists() {
        std::fs::remove_dir_all(&config)?;
    }
    std::fs::write(&config, "")?;
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644))?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    Ok(())
}

#[test]
fn a_root_that_is_a_link_to_nothing_leaves_the_rest_to_be_removed() -> R {
    // M2T14R-1052 (design correction 13): the root a link to nothing, the template and the command link left. Nothing
    // of the root is there, so nothing can run from it but by its own path, and nothing under it is removed; the
    // template and the command link are, and the uninstall ends with 0 (after correction 12 its `readlink -f` failed,
    // and the uninstall refused with 69).
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let root = install.data().join("hoh");
    std::fs::remove_dir_all(&root)?;
    std::os::unix::fs::symlink(install.root.path().join("nowhere/at/all/hoh"), &root)?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let stdout = String::from_utf8(out.stdout.clone())?;
    let template = install.data().join("systemd/user/hoh-host@.service");
    let bin_link = install.home().join(".local/bin/hoh");
    assert!(
        stdout.contains(&format!("removed {}", template.display())),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("removed {}", bin_link.display())),
        "{stdout}"
    );
    assert!(!template.exists() && !bin_link.is_symlink());
    Ok(())
}

#[test]
fn an_instance_whose_state_has_no_answer_after_its_disable_cannot_be_told() -> R {
    // M2T14R-1051 (design correction 13): the disable succeeds, then `show -p ActiveState` gives no answer (a stand-in
    // manager); whether the instance stopped cannot be told: 69, nothing removed, and nothing named to stop (before, it
    // was said as one that could not be stopped, and the user was told to stop it).
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    std::fs::create_dir_all(install.data().join("hoh/hosts"))?;
    std::fs::write(install.data().join("hoh/hosts/abc.env"), "")?;
    std::fs::create_dir_all(install.root.path().join("r/systemd"))?;
    let _socket =
        std::os::unix::net::UnixListener::bind(install.root.path().join("r/systemd/private"))?;
    script(
        &install.stubs().join("systemctl"),
        "#!/bin/sh\ncase \" $* \" in *\" is-enabled \"*) echo disabled; exit 1;; *\" show \"*) exit 1;; esac\nexit 0\n",
    )?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains("cannot tell whether hoh-host@abc.service stopped after its disable"),
        "{stderr}"
    );
    assert!(!stderr.contains("could not stop and disable"), "{stderr}");
    assert!(!stderr.contains("stop what is named above"), "{stderr}");
    assert!(stderr.contains("could not be told"), "{stderr}");
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    Ok(())
}

#[test]
fn a_unit_directory_that_cannot_be_read_keeps_the_uninstall_from_removing_anything() -> R {
    // Design correction 12: ~/.config/systemd/user itself unreadable (its parent searchable) cannot be listed; what
    // cannot be listed may enable an instance: 69, nothing removed.
    let stderr = an_uninstall_with_no_manager_refuses(
        |root, template| {
            let user = root.join("h/.config/systemd/user");
            let wants = user.join("default.target.wants");
            std::fs::create_dir_all(&wants)?;
            std::os::unix::fs::symlink(template, wants.join("hoh-host@0123456789abcdef.service"))?;
            std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o000))?;
            Ok(vec![user])
        },
        "cannot list",
    )?;
    assert!(stderr.contains("could not be told"), "{stderr}");
    Ok(())
}

#[test]
fn a_template_that_cannot_be_told_keeps_the_uninstall_from_removing_anything() -> R {
    // Design correction 12: what the uninstall removes is told before anything is; a template whose directory cannot
    // be searched (~/.local/share/systemd mode 000) cannot be told: 69, nothing removed (else the template would be
    // left, naming the removed installation, and the rest removed).
    let stderr = an_uninstall_with_no_manager_refuses(
        |root, _template| {
            let systemd = root.join("h/.local/share/systemd");
            std::fs::set_permissions(&systemd, std::fs::Permissions::from_mode(0o000))?;
            Ok(vec![systemd])
        },
        "cannot tell whether",
    )?;
    assert!(stderr.contains("could not be told"), "{stderr}");
    Ok(())
}

#[test]
fn a_runtime_link_with_no_manager_keeps_the_uninstall_from_removing_anything() -> R {
    // Design correction 11 (M2T14R-803): a link in $XDG_RUNTIME_DIR/systemd/user (`enable --runtime`) enables the
    // instance as well; with no manager to disable it, it refuses.
    an_uninstall_with_no_manager_refuses(
        |root, template| {
            let wants = root.join("r/systemd/user/default.target.wants");
            std::fs::create_dir_all(&wants)?;
            std::os::unix::fs::symlink(template, wants.join("hoh-host@0123456789abcdef.service"))?;
            Ok(vec![])
        },
        "hoh-host@0123456789abcdef.service is enabled, and no user systemd manager answers to disable it",
    )
    .map(drop)
}

#[test]
fn a_unit_directory_that_cannot_be_listed_keeps_the_uninstall_from_removing_anything() -> R {
    // Design correction 11 (M2T14R-801): what cannot be listed may enable an instance: a .requires directory this user
    // cannot read refuses the uninstall (69, nothing removed), it is not taken for one with no link in it.
    let stderr = an_uninstall_with_no_manager_refuses(
        |root, template| {
            let requires = root.join("h/.config/systemd/user/default.target.requires");
            std::fs::create_dir_all(&requires)?;
            std::os::unix::fs::symlink(
                template,
                requires.join("hoh-host@0123456789abcdef.service"),
            )?;
            std::fs::set_permissions(&requires, std::fs::Permissions::from_mode(0o000))?;
            Ok(vec![requires])
        },
        "cannot list",
    )?;
    // What cannot be told is said so; nothing is named to stop (design correction 11).
    assert!(stderr.contains("could not be told"), "{stderr}");
    assert!(!stderr.contains("stop what is named above"), "{stderr}");
    Ok(())
}

#[test]
fn a_unit_directory_under_one_that_cannot_be_searched_keeps_the_uninstall_from_removing_anything()
-> R {
    // Design correction 12 (M2T14R-902): a unit directory whose parent cannot be searched is neither there nor not
    // there; it cannot be told, so the uninstall refuses (69, nothing removed), and says so.
    let stderr = an_uninstall_with_no_manager_refuses(
        |root, template| {
            let wants = root.join("h/.config/systemd/user/default.target.wants");
            std::fs::create_dir_all(&wants)?;
            std::os::unix::fs::symlink(template, wants.join("hoh-host@0123456789abcdef.service"))?;
            let systemd = root.join("h/.config/systemd");
            std::fs::set_permissions(&systemd, std::fs::Permissions::from_mode(0o000))?;
            Ok(vec![systemd])
        },
        "cannot tell",
    )?;
    assert!(stderr.contains("could not be told"), "{stderr}");
    Ok(())
}

#[test]
fn a_command_link_in_a_directory_that_cannot_be_searched_is_not_taken_for_anothers() -> R {
    // Design correction 12 (M2T14R-903): ~/.local/bin that cannot be searched hides whether ~/.local/bin/hoh is this
    // installation's link: the uninstall and the install refuse (69), nothing removed; before, the uninstall removed
    // everything but the link.
    let install = Install::new()?;
    let top = install.root.path().to_path_buf();
    {
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        let bin = install.home().join(".local/bin");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o000))?;
        let _restore = installed::Restore(vec![bin]);
        for args in [&["--uninstall"][..], &[]] {
            let out = install.run("bundle", args)?;
            assert_eq!(out.status.code(), Some(69), "{args:?}: {}", text(&out));
            let stderr = String::from_utf8(out.stderr.clone())?;
            assert!(stderr.contains("cannot tell"), "{args:?}: {stderr}");
            assert!(install.data().join("hoh/current/bin/hoh").exists());
        }
    }
    assert!(install.home().join(".local/bin/hoh").is_symlink());
    drop(install);
    assert!(!top.exists(), "{} left", top.display());
    Ok(())
}

#[test]
fn links_that_are_not_hohs_in_the_unit_directories_do_not_stop_the_uninstall() -> R {
    // M2T14R-950 (design correction 12): the enabling links are read where systemd reads them, one level down, by name,
    // no link's target looked at: a link to itself and a link to nothing, neither HoH's, at the top and in a .wants
    // directory, keep nothing from being removed (before, `find -L` failed on them: 69, "cannot list").
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let user = install.home().join(".config/systemd/user");
    let wants = user.join("default.target.wants");
    std::fs::create_dir_all(&wants)?;
    for at in [&user, &wants] {
        std::os::unix::fs::symlink("stale.service", at.join("stale.service"))?;
        std::os::unix::fs::symlink("/nowhere", at.join("gone.service"))?;
    }
    // Design correction 13: a name systemd would read links in, itself a link to itself or to nothing, is followed
    // and seen not there; it holds no link enabling an instance.
    std::os::unix::fs::symlink("loop.target.wants", user.join("loop.target.wants"))?;
    std::os::unix::fs::symlink("/nowhere", user.join("gone.target.requires"))?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    assert!(
        wants.join("stale.service").is_symlink(),
        "the user's link removed"
    );
    Ok(())
}

#[test]
fn a_root_that_cannot_be_resolved_for_the_scan_keeps_the_uninstall_from_removing_anything() -> R {
    // M2T14R-951: the /proc scan names programs by the root's resolved path; `readlink -m` failing (a stand-in) cannot
    // tell where they would run from: 69, nothing removed (before, the root's own path was used and the scan could miss
    // a program run through a link).
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    script(
        &install.stubs().join("readlink"),
        "#!/bin/sh\ncase \"$1\" in -m) echo 'readlink: failing' >&2; exit 1;; esac\nexec /usr/bin/readlink \"$@\"\n",
    )?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(stderr.contains("cannot resolve"), "{stderr}");
    assert!(stderr.contains("could not be told"), "{stderr}");
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    Ok(())
}

#[test]
fn a_command_link_that_cannot_be_read_is_not_taken_for_anothers() -> R {
    // Design correction 11 (M2T14R-804): ~/.local/bin/hoh that cannot be read (a stand-in readlink failing for it
    // alone) is neither this installation's nor another's: the uninstall removes nothing (69, "cannot read"), and the
    // install refuses too; before, the uninstall removed everything but the link and called it another's.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    script(
        &install.stubs().join("readlink"),
        "#!/bin/sh\nfor last; do :; done\ncase \"$1 $last\" in --\\ */.local/bin/hoh) echo 'readlink: Input/output error' >&2; exit 23;; esac\nexec /usr/bin/readlink \"$@\"\n",
    )?;
    for args in [&["--uninstall"][..], &[]] {
        let out = install.run("bundle", args)?;
        assert_eq!(out.status.code(), Some(69), "{args:?}: {}", text(&out));
        let stderr = String::from_utf8(out.stderr.clone())?;
        assert!(stderr.contains("cannot read"), "{args:?}: {stderr}");
        assert!(
            !stderr.contains("not this installation's link"),
            "{args:?}: {stderr}"
        );
        assert!(
            !stderr.contains("stop what is named above"),
            "{args:?}: {stderr}"
        );
        assert!(install.data().join("hoh/current/bin/hoh").exists());
        assert!(install.home().join(".local/bin/hoh").is_symlink());
    }
    Ok(())
}

#[test]
fn the_install_says_its_version_with_no_command_that_could_fail_in_its_words() -> R {
    // Design correction 11 (M2T14R-805): the closing words are made in the installer's own shell (no head or tr in a
    // command substitution whose failure printf would hide): with a head and a tr that fail, the install still names
    // the version it installed.
    let install = Install::new()?;
    for tool in ["head", "tr"] {
        script(
            &install.stubs().join(tool),
            "#!/bin/sh\necho 'failing' >&2\nexit 23\n",
        )?;
    }
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let stdout = String::from_utf8(out.stdout.clone())?;
    assert!(stdout.contains("installed hoh 0."), "{stdout}");
    Ok(())
}

#[test]
fn a_manager_that_fails_during_the_install_is_said_and_ends_it_with_69() -> R {
    // Design correction 10 (M2T14R-701): `systemctl --user daemon-reload` or `show` failing during the install (a
    // stand-in exiting 23) ends it with 69, saying which and what the manager said; before, set -e ended it with 23
    // and no word of its own.
    let install = Install::new()?;
    std::fs::create_dir_all(install.root.path().join("r/systemd"))?;
    let _socket =
        std::os::unix::net::UnixListener::bind(install.root.path().join("r/systemd/private"))?;
    let template = install.data().join("systemd/user/hoh-host@.service");
    for failing in ["daemon-reload", "show"] {
        script(
            &install.stubs().join("systemctl"),
            &format!(
                "#!/bin/sh\ncase \" $* \" in *\" {failing} \"*) echo manager-error >&2; exit 23;; *\" show \"*) printf 'LoadState=loaded\\nFragmentPath=%s\\n' '{t}';; esac\nexit 0\n",
                t = template.display()
            ),
        )?;
        if template.exists() {
            std::fs::remove_file(&template)?;
        }
        let out = install.run("bundle", &[])?;
        assert_eq!(out.status.code(), Some(69), "{failing}: {}", text(&out));
        let stderr = String::from_utf8(out.stderr.clone())?;
        assert!(
            stderr.contains(&format!("systemctl --user {failing}")),
            "{stderr}"
        );
        assert!(stderr.contains("exit 23"), "{stderr}");
        assert!(stderr.contains("manager-error"), "{stderr}");
    }
    Ok(())
}

#[test]
fn an_unexpected_failure_inside_a_command_substitution_is_said_once_with_its_own_code() -> R {
    // Design correction 10: the ERR trap speaks only in the installer's own shell. A command failing inside a command
    // substitution the installer does not handle (a stand-in sha256sum) ends that subshell with its own code, and the
    // installer then says the assignment failed with that code, once, and ends with 74.
    let install = Install::new()?;
    script(
        &install.stubs().join("sha256sum"),
        "#!/bin/sh\necho 'sha256sum: read error' >&2\nexit 1\n",
    )?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(74), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert_eq!(stderr.matches("failed unexpectedly").count(), 1, "{stderr}");
    assert!(stderr.contains("sha256sum"), "{stderr}");
    assert!(stderr.contains("failed unexpectedly (exit 1,"), "{stderr}");
    Ok(())
}

#[test]
fn an_unexpected_failure_is_said_with_its_command_and_ends_the_install_with_74() -> R {
    // Design correction 10: a command whose failure install.sh does not handle on its own (a stand-in cp: no space
    // left) is said with its command, exit code and line, and ends the install with 74; before, set -e ended it with
    // cp's 1, the code of a foreign ~/.local/bin/hoh, and no word of its own.
    let install = Install::new()?;
    script(
        &install.stubs().join("cp"),
        "#!/bin/sh\necho 'cp: error writing: No space left on device' >&2\nexit 1\n",
    )?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(74), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(stderr.contains("cp -R"), "{stderr}");
    assert!(stderr.contains("failed unexpectedly (exit 1"), "{stderr}");
    assert!(!install.home().join(".local/bin/hoh").is_symlink());
    Ok(())
}

#[test]
fn the_uninstall_keeps_a_command_that_is_no_longer_its_link_and_says_so() -> R {
    // M2T14R-150: a ~/.local/bin/hoh the user replaced after the install is not removed, nor said to be.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let link = install.home().join(".local/bin/hoh");
    std::fs::remove_file(&link)?;
    std::fs::write(&link, "#!/bin/sh\necho mine\n")?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(&link)?, "#!/bin/sh\necho mine\n");
    assert!(
        !String::from_utf8(out.stdout.clone())?.contains(&format!("removed {}", link.display())),
        "{}",
        text(&out)
    );
    assert!(
        String::from_utf8(out.stderr.clone())?.contains(&format!("kept {}", link.display())),
        "{}",
        text(&out)
    );
    Ok(())
}

/// The uninstall started in the background, paused before its first removal by a stand-in `rm` until `go` exists.
fn paused_uninstall(install: &Install) -> R<(std::process::Child, PathBuf)> {
    let paused = install.root.path().join("rm-paused");
    let go = install.root.path().join("rm-go");
    script(
        &install.stubs().join("rm"),
        &format!(
            "#!/bin/sh\nif [ ! -e {p} ]; then : > {p}; i=0; while [ ! -e {g} ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done; fi\nexec /usr/bin/rm \"$@\"\n",
            p = paused.display(),
            g = go.display()
        ),
    )?;
    let child = Command::new("bash")
        .arg(install.root.path().join("bundle/hoh/install.sh"))
        .arg("--uninstall")
        .current_dir(install.root.path())
        .env_clear()
        .env("HOME", install.home())
        .env("XDG_RUNTIME_DIR", install.root.path().join("r"))
        .env("USER", "tester")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", install.stubs().display()),
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !paused.exists() {
        if std::time::Instant::now() > end {
            return Err("the uninstall did not pause".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok((child, go))
}

/// `hoh --version` from the installed command under strace, its first `syscall` held 4 s at entry; the strace and
/// the pid of hoh once it runs.
fn held_at(install: &Install, syscall: &str) -> R<(std::process::Child, u32)> {
    held_for(install, syscall, 4)
}

/// [`held_at`], held `seconds`.
fn held_for(install: &Install, syscall: &str, seconds: u32) -> R<(std::process::Child, u32)> {
    under_strace(
        install,
        &[
            "-e".to_owned(),
            format!("trace={syscall}"),
            "-e".to_owned(),
            format!(
                "inject={syscall}:delay_enter={}:when=1",
                seconds * 1_000_000
            ),
        ],
    )
}

/// `hoh --version` held `seconds` at its opening of the installation's lock (after it read its own path).
fn held_opening_the_lock(install: &Install, seconds: u32) -> R<(std::process::Child, u32)> {
    under_strace(
        install,
        &[
            "-P".to_owned(),
            install.data().join("hoh/.lock").display().to_string(),
            "-e".to_owned(),
            "trace=openat".to_owned(),
            "-e".to_owned(),
            format!("inject=openat:delay_enter={}:when=1", seconds * 1_000_000),
        ],
    )
}

/// `hoh --version` from the installed command under strace with `held` (its injection); the strace and the pid of
/// hoh once it runs the installed binary.
fn under_strace(install: &Install, held: &[String]) -> R<(std::process::Child, u32)> {
    let strace = Command::new("strace")
        .args(["-f", "-qq", "-o", "/dev/null"])
        .args(held)
        .arg(install.home().join(".local/bin/hoh"))
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("precondition: strace ({e})"))?;
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let children = std::fs::read_to_string(format!(
            "/proc/{}/task/{}/children",
            strace.id(),
            strace.id()
        ))
        .unwrap_or_default();
        // strace may first fork a helper of its own (to probe ptrace): hoh is the child running a binary of the
        // installation.
        let root = install.data().join("hoh");
        if let Some(pid) = children
            .split_whitespace()
            .filter_map(|p| p.parse::<u32>().ok())
            .find(|p| {
                std::fs::read_link(format!("/proc/{p}/exe")).is_ok_and(|exe| exe.starts_with(&root))
            })
        {
            return Ok((strace, pid));
        }
        if std::time::Instant::now() > end {
            return Err("strace started nothing".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn a_run_that_gets_its_lock_only_after_the_uninstall_removed_it_does_not_run_on() -> R {
    // ADR-023 design correction 3 (round 3): a run that opened the lock while the uninstall removed the installation
    // and got its shared lock only after the uninstall ended (flock held 4 s by strace) holds an old file: refused.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let (uninstall, go) = paused_uninstall(&install)?;
    let (strace, pid) = held_at(&install, "flock")?;
    let lock = install.data().join("hoh/.lock");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !std::fs::read_dir(format!("/proc/{pid}/fd"))?
        .flatten()
        .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|t| t == lock))
    {
        if std::time::Instant::now() > end {
            return Err("hoh never opened the lock".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::fs::write(&go, "")?;
    let done = uninstall.wait_with_output()?;
    assert_eq!(done.status.code(), Some(0), "{}", text(&done));
    let run = strace.wait_with_output()?;
    assert_eq!(run.status.code(), Some(69), "{}", text(&run));
    let said = String::from_utf8(run.stderr)?;
    // Its own reason, not another refusal met first (M2T14R-353).
    assert!(
        said.contains("being uninstalled")
            && said.contains("its lock was removed before it was held"),
        "{said}"
    );
    Ok(())
}

#[test]
fn a_run_whose_binary_the_uninstall_removed_before_it_looked_does_not_run_on() -> R {
    // Design correction 3 (round 3): a run held before it reads its own path (readlink of /proc/self/exe, 4 s) while
    // the uninstall removes its binary reads " (deleted)": refused, not taken for a binary outside an installation.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let (uninstall, go) = paused_uninstall(&install)?;
    let (strace, _pid) = held_at(&install, "readlink")?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    std::fs::write(&go, "")?;
    let done = uninstall.wait_with_output()?;
    assert_eq!(done.status.code(), Some(0), "{}", text(&done));
    let run = strace.wait_with_output()?;
    assert_eq!(run.status.code(), Some(69), "{}", text(&run));
    let said = String::from_utf8(run.stderr)?;
    // Its own reason, not another refusal met first (M2T14R-353).
    assert!(
        said.contains("being uninstalled")
            && said.contains("was removed")
            && !said.contains("or replaced"),
        "{said}"
    );
    Ok(())
}

#[test]
fn a_run_between_its_exec_and_its_lock_keeps_the_uninstall_from_removing_anything() -> R {
    // M2T14R-252: a run of the installed binary held (4 s, strace) before it reads its own path, so before its lock:
    // the uninstall gets the lock but finds the run by its binary and removes nothing (69); the scan is a defence of
    // its own, not only a report.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let (strace, pid) = held_at(&install, "readlink")?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    assert!(
        String::from_utf8(out.stderr.clone())?.contains(&format!("pid {pid}")),
        "{}",
        text(&out)
    );
    assert!(install.home().join(".local/bin/hoh").exists());
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    let run = strace.wait_with_output()?;
    assert_eq!(run.status.code(), Some(0), "{}", text(&run));
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    Ok(())
}

/// Install, pause an uninstall before its first removal, start `hoh --version` held at its first `syscall` (8 s),
/// let it reach that point (`ready`), then let the uninstall finish and install the same bundle again: the held run,
/// let go now, finds an installation at the same paths, but not the one it was started from.
fn held_across_a_reinstall(
    start: fn(&Install) -> R<(std::process::Child, u32)>,
    ready: fn(&Install, u32) -> R,
) -> R<Output> {
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let (uninstall, go) = paused_uninstall(&install)?;
    let (strace, pid) = start(&install)?;
    ready(&install, pid)?;
    std::fs::write(&go, "")?;
    let done = uninstall.wait_with_output()?;
    assert_eq!(done.status.code(), Some(0), "{}", text(&done));
    let again = install.run("bundle", &[])?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again));
    Ok(strace.wait_with_output()?)
}

#[test]
fn a_run_holding_a_removed_lock_does_not_run_on_after_a_reinstall() -> R {
    // ADR-023 design correction 3: the run opened the lock the uninstall then removed; the same bundle is installed
    // again (a new lock and a binary at the same paths) before its flock goes on: the lock it holds is not the
    // installation's (red without the inode check: it ran on).
    let run = held_across_a_reinstall(
        |install| held_for(install, "flock", 8),
        |install, pid| {
            let lock = install.data().join("hoh/.lock");
            let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !std::fs::read_dir(format!("/proc/{pid}/fd"))?
                .flatten()
                .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|t| t == lock))
            {
                if std::time::Instant::now() > end {
                    return Err("hoh never opened the lock".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(())
        },
    )?;
    assert_eq!(run.status.code(), Some(69), "{}", text(&run));
    let said = String::from_utf8(run.stderr)?;
    // Its own reason, not another refusal met first (M2T14R-353).
    assert!(
        said.contains("being uninstalled")
            && said.contains("its lock was removed before it was held"),
        "{said}"
    );
    Ok(())
}

#[test]
fn a_run_of_a_removed_binary_does_not_run_on_after_a_reinstall() -> R {
    // Design correction 3: the run's binary was removed (a binary at the same path again after the reinstall) before
    // it read its own path: it reads " (deleted)" and is refused (red without that check: it ran on).
    let run = held_across_a_reinstall(
        |install| held_for(install, "readlink", 8),
        |_, _| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            Ok(())
        },
    )?;
    assert_eq!(run.status.code(), Some(69), "{}", text(&run));
    let said = String::from_utf8(run.stderr)?;
    // Its own reason, not another refusal met first (M2T14R-353).
    assert!(
        said.contains("being uninstalled")
            && said.contains("was removed")
            && !said.contains("or replaced"),
        "{said}"
    );
    Ok(())
}

#[test]
fn a_run_that_read_its_path_before_an_uninstall_and_a_reinstall_does_not_run_on() -> R {
    // ADR-023 design correction 6 (M2T14R-350): the run read and resolved its own path, then (held 8 s at its opening
    // of the lock) the uninstall removed its binary and the same bundle was installed again: the new lock and a binary
    // at its path pass every path check, but its running image is the removed one: refused (red when the binary is
    // judged by its path).
    let run = held_across_a_reinstall(
        |install| held_opening_the_lock(install, 8),
        |_, _| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            Ok(())
        },
    )?;
    assert_eq!(run.status.code(), Some(69), "{}", text(&run));
    let said = String::from_utf8(run.stderr)?;
    assert!(said.contains("removed or replaced"), "{said}");
    Ok(())
}

#[test]
fn an_install_and_an_uninstall_take_turns() -> R {
    // Design correction 5 (M2T14R-301): an uninstall started while an install is under way (paused before it
    // publishes the unit template) waits for it; the install ends complete, naming its version, and the uninstall
    // then removes it whole.
    let install = Install::new()?;
    let paused = install.root.path().join("mv-paused");
    let go = install.root.path().join("mv-go");
    script(
        &install.stubs().join("mv"),
        &format!(
            "#!/bin/sh\nfor last; do :; done\ncase \"$last\" in *hoh-host@.service) if [ ! -e {p} ]; then : > {p}; i=0; while [ ! -e {g} ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done; fi;; esac\nexec /usr/bin/mv \"$@\"\n",
            p = paused.display(),
            g = go.display()
        ),
    )?;
    let env = |command: &mut Command| {
        command
            .current_dir(install.root.path())
            .env_clear()
            .env("HOME", install.home())
            .env("XDG_RUNTIME_DIR", install.root.path().join("r"))
            .env("USER", "tester")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", install.stubs().display()),
            )
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    };
    let script_path = install.root.path().join("bundle/hoh/install.sh");
    let mut first = Command::new("bash");
    first.arg(&script_path);
    env(&mut first);
    let installing = first.spawn()?;
    let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !paused.exists() {
        if std::time::Instant::now() > end {
            return Err("the install did not pause".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut second = Command::new("bash");
    second.arg(&script_path).arg("--uninstall");
    env(&mut second);
    let mut uninstalling = second.spawn()?;
    // It waits: still running a second later.
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert!(
        uninstalling.try_wait()?.is_none(),
        "the uninstall did not wait for the install"
    );
    // The installers' lock is a file of its own: $HOME itself stays free for a client starting a host in a project
    // there (it locks its project's directory), M2T14R-450.
    let home = std::fs::File::open(install.home())?;
    rustix::fs::flock(&home, rustix::fs::FlockOperation::NonBlockingLockExclusive)?;
    drop(home);
    std::fs::write(&go, "")?;
    let installed = installing.wait_with_output()?;
    assert_eq!(installed.status.code(), Some(0), "{}", text(&installed));
    assert!(
        String::from_utf8(installed.stdout.clone())?.contains("installed hoh "),
        "{}",
        text(&installed)
    );
    let removed = uninstalling.wait_with_output()?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed));
    for gone in [
        install.home().join(".local/bin/hoh"),
        install.data().join("hoh"),
        install.data().join("systemd/user/hoh-host@.service"),
    ] {
        assert!(
            !gone.exists() && !gone.is_symlink(),
            "{} left",
            gone.display()
        );
    }
    Ok(())
}

#[test]
fn an_uninstall_whose_lock_changed_while_it_took_it_removes_nothing() -> R {
    // Design correction 5 (M2T14R-300): a stand-in flock replaces the installation's lock just before the uninstall
    // takes it: the lock it holds is not the file at its path, so it removes nothing (69).
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let lock = install.data().join("hoh/.lock");
    script(
        &install.stubs().join("flock"),
        &format!(
            "#!/bin/sh\ncase \" $* \" in *\" 9 \"*) /usr/bin/rm -f {l}; : > {l};; esac\nexec /usr/bin/flock \"$@\"\n",
            l = lock.display()
        ),
    )?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(stderr.contains("lock changed"), "{stderr}");
    assert!(!stderr.contains("still runs"), "{stderr}");
    assert!(!stderr.contains("stop what is named above"), "{stderr}");
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    std::fs::remove_file(install.stubs().join("flock"))?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    Ok(())
}

#[test]
fn a_lock_made_read_only_or_lost_is_said_so_and_mended_by_the_installer() -> R {
    // M2T14R-351, -352: a 0400 lock: runs hold it and the uninstall takes it (read-only); a lost lock with the
    // installation in place: a run says the lock is missing (not an uninstall), and the installed copy of install.sh
    // makes it again.
    use std::os::unix::fs::PermissionsExt as _;
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let lock = install.data().join("hoh/.lock");
    let command = install.home().join(".local/bin/hoh");
    let version = || {
        Command::new(&command)
            .arg("--version")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
    };
    std::fs::remove_file(&lock)?;
    let out = version()?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let said = String::from_utf8(out.stderr.clone())?;
    assert!(
        said.contains("is missing") && !said.contains("being uninstalled"),
        "{said}"
    );
    let again = Command::new("bash")
        .arg(install.data().join("hoh/current/install.sh"))
        .current_dir(install.root.path())
        .env_clear()
        .env("HOME", install.home())
        .env("XDG_RUNTIME_DIR", install.root.path().join("r"))
        .env("USER", "tester")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", install.stubs().display()),
        )
        .output()?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again));
    assert_eq!(version()?.status.code(), Some(0));
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o400))?;
    assert_eq!(version()?.status.code(), Some(0));
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!install.data().join("hoh").exists());
    Ok(())
}

#[test]
fn an_uninstall_that_cannot_tell_its_lock_removes_nothing() -> R {
    // M2T14R-400: a stat that fails (a stand-in) leaves the uninstall unable to tell whether the lock it holds is the
    // installation's: it removes nothing (69), and completes once it can tell.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    script(
        &install.stubs().join("stat"),
        "#!/bin/sh\necho 'stat: cannot statx' >&2\nexit 1\n",
    )?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(stderr.contains("cannot tell"), "{stderr}");
    assert!(!stderr.contains("still runs"), "{stderr}");
    assert!(!stderr.contains("stop what is named above"), "{stderr}");
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    assert!(install.home().join(".local/bin/hoh").is_symlink());
    std::fs::remove_file(install.stubs().join("stat"))?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    Ok(())
}

#[test]
fn an_installers_lock_that_cannot_be_had_is_told_from_one_held_by_another() -> R {
    // M2T14R-401 (ADR-023 design corrections 7 and 8): the installers' lock $XDG_DATA_HOME/hoh.lock: a failure to lock at all
    // (a stand-in flock: EBADF) is said as it is, at once; a wait that ended at its limit (the stand-in's conflict
    // code) says the lock stayed held; neither says the other. Neither writes anything but the lock file itself.
    let install = Install::new()?;
    for (code, said, not_said) in [
        (
            "echo 'flock: 8: Bad file descriptor' >&2; exit 65",
            "cannot lock",
            "stayed locked",
        ),
        (
            "case \" $* \" in *\" -E 75 \"*) exit 75;; *) exit 1;; esac",
            "stayed locked",
            "cannot lock",
        ),
    ] {
        script(
            &install.stubs().join("flock"),
            &format!(
                "#!/bin/sh\ncase \" $* \" in *\" 8 \"*) {code};; esac\nexec /usr/bin/flock \"$@\"\n"
            ),
        )?;
        let started = std::time::Instant::now();
        let out = install.run("bundle", &[])?;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{code}"
        );
        assert_eq!(out.status.code(), Some(69), "{code}: {}", text(&out));
        let stderr = String::from_utf8(out.stderr.clone())?;
        assert!(stderr.contains(said), "{code}: {stderr}");
        assert!(!stderr.contains(not_said), "{code}: {stderr}");
        let written: Vec<PathBuf> = tree(&install.home())?
            .into_iter()
            .filter(|(_, e)| e.kind != 'd')
            .map(|(p, _)| p)
            .collect();
        assert_eq!(written, [PathBuf::from(".local/share/hoh.lock")], "{code}");
    }
    std::fs::remove_file(install.stubs().join("flock"))?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    Ok(())
}

#[test]
fn an_install_whose_installed_binary_does_not_run_fails() -> R {
    // M2T14R-301, -455: the last look at the installed binary (its version) failing fails the install (69), and it
    // does not say it installed: a stand-in mv removes the binary as the template is published.
    let install = Install::new()?;
    let binary = install.data().join("hoh/current/bin/hoh");
    script(
        &install.stubs().join("mv"),
        &format!(
            "#!/bin/sh\nfor last; do :; done\ncase \"$last\" in *hoh-host@.service) /usr/bin/rm -f \"$(readlink -f {b})\";; esac\nexec /usr/bin/mv \"$@\"\n",
            b = binary.display()
        ),
    )?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    assert!(
        String::from_utf8(out.stderr.clone())?.contains("does not run"),
        "{}",
        text(&out)
    );
    assert!(
        !String::from_utf8(out.stdout.clone())?.contains("installed hoh"),
        "{}",
        text(&out)
    );
    Ok(())
}

#[test]
fn an_uninstall_whose_lock_cannot_be_had_says_so_and_not_that_a_program_runs() -> R {
    // Design correction 8 (M2T14R-501, -551): the run lock's flock failing other than by a conflict (a stand-in: EIO,
    // exit 74) is said as it is, with flock's message: nothing is removed (69), and no program is said to run.
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    script(
        &install.stubs().join("flock"),
        "#!/bin/sh\ncase \" $* \" in *\" 9 \"*) echo 'flock: 9: Input/output error' >&2; exit 74;; esac\nexec /usr/bin/flock \"$@\"\n",
    )?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(stderr.contains("cannot lock"), "{stderr}");
    assert!(stderr.contains("Input/output error"), "{stderr}");
    assert!(!stderr.contains("still runs"), "{stderr}");
    assert!(!stderr.contains("stop what is named above"), "{stderr}");
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    std::fs::remove_file(install.stubs().join("flock"))?;
    let out = install.run("bundle", &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    Ok(())
}

#[test]
fn an_installers_lock_that_cannot_be_opened_is_said_so() -> R {
    // M2T14R-452, -552: the installers' lock this user cannot open (a directory in its place): 69 and the reason, not
    // bash's own message and exit 1; nothing else is written.
    let install = Install::new()?;
    std::fs::create_dir_all(install.data().join("hoh.lock"))?;
    let before = tree(&install.home())?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    assert!(
        String::from_utf8(out.stderr.clone())?.contains("cannot open the installer's lock"),
        "{}",
        text(&out)
    );
    assert_eq!(tree(&install.home())?, before);
    Ok(())
}

#[test]
fn an_uninstall_that_cannot_open_the_installations_lock_says_so() -> R {
    // M2T14R-453, -455: a lock this user cannot open: the uninstall removes nothing (69) and says why, without saying
    // that a program still runs.
    use std::os::unix::fs::PermissionsExt as _;
    let install = Install::new()?;
    let out = install.run("bundle", &[])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let lock = install.data().join("hoh/.lock");
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o000))?;
    let out = install.run("bundle", &["--uninstall"])?;
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600))?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr.clone())?;
    assert!(
        stderr.contains("cannot open this installation's lock"),
        "{stderr}"
    );
    assert!(!stderr.contains("still runs"), "{stderr}");
    assert!(!stderr.contains("stop what is named above"), "{stderr}");
    assert!(install.data().join("hoh/current/bin/hoh").exists());
    Ok(())
}
