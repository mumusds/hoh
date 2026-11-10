//! The systemd user unit of a project's host (docs/adr/ADR-023.md decisions 1, 2 and 5): the instance a project
//! gets, the template the installer writes, the per-instance environment file that names the project, and the reading
//! of `systemctl show`. The installer (`hoh __unit`), the client and `hoh doctor` use this module and nothing else for
//! these.

use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The template's file name; an instance is `hoh-host@<instance>.service`.
pub const TEMPLATE: &str = "hoh-host@.service";

/// Why a path cannot be used in the unit or its environment file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unusable(pub String);

impl std::fmt::Display for Unusable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unusable {}

/// The instance of the project at `canonical` (its canonical path): the first 16 hex digits of the SHA-256 of its
/// bytes, as an attempt's cgroup is named.
pub fn instance(canonical: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    Sha256::digest(canonical.as_os_str().as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The unit of `instance`.
pub fn unit_name(instance: &str) -> String {
    format!("hoh-host@{instance}.service")
}

/// Whether systemd takes `c` in a unit or an environment file: no control character (systemd refuses them in a
/// command line), no byte order mark and no Unicode noncharacter (it refuses those in an environment file).
fn plain(c: char) -> bool {
    let n = u32::from(c);
    !c.is_control() && c != '\u{feff}' && !(0xfdd0..=0xfdef).contains(&n) && n & 0xfffe != 0xfffe
}

/// `path` as text systemd takes, or why not: UTF-8, absolute, and every character plain.
fn text_of<'a>(what: &str, path: &'a Path) -> Result<&'a str, Unusable> {
    let text = path
        .to_str()
        .ok_or_else(|| Unusable(format!("{what} is not UTF-8: {}", path.display())))?;
    if !path.is_absolute() {
        return Err(Unusable(format!("{what} is not absolute: {text:?}")));
    }
    if let Some(c) = text.chars().find(|c| !plain(*c)) {
        return Err(Unusable(format!(
            "{what} has {c:?}, which systemd does not take: {text:?}"
        )));
    }
    Ok(text)
}

/// `path` as a unit file takes it in ExecStart= (in double quotes) and EnvironmentFile= (the rest of the line), `%`
/// doubled, or why not: [`text_of`]'s rules, and no `"`, `\\` or `'` (systemd runs no program whose path has one,
/// however escaped).
fn unit_path(what: &str, path: &Path) -> Result<String, Unusable> {
    let text = text_of(what, path)?;
    if let Some(c) = text.chars().find(|c| matches!(c, '"' | '\\' | '\'')) {
        return Err(Unusable(format!(
            "{what} has {c:?}: systemd runs no program whose path has it ({text:?})"
        )));
    }
    Ok(text.replace('%', "%%"))
}

/// The root of the installation whose binary is `exe` (canonical): `<root>/versions/<version>/bin/hoh`; `None` for a
/// binary anywhere else (a build, a test's), which is not an installed hoh (ADR-023 design correction 1).
pub fn installation_root(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    let version = bin.parent()?;
    let versions = version.parent()?;
    let root = versions.parent()?;
    (exe.is_absolute()
        && exe.file_name()? == "hoh"
        && bin.file_name()? == "bin"
        && version.file_name().is_some()
        && versions.file_name()? == "versions"
        && root.file_name().is_some())
    .then(|| root.to_path_buf())
}

/// Hold the installation of the binary at `exe` (as `std::env::current_exe` reads it, " (deleted)" included) shared,
/// for as long as the returned file is open: every run of an installed hoh does, before anything else, and the
/// uninstall takes it exclusive before it removes anything (ADR-023 design corrections 2 and 3, M2T14R-100). `None`
/// for a binary not in an installation; an error when the installation is being removed: its lock held exclusive,
/// its lock file or root gone (the installer makes the file; it is never made here), a lock got on a file no longer
/// the one at its path (the uninstall removed it meanwhile), or this binary removed. The file is close-on-exec: the
/// agents a host starts do not hold it.
pub fn hold_installation(exe: &Path) -> Result<Option<std::fs::File>, Unusable> {
    hold_installation_as(exe, Path::new("/proc/self/exe"))
}

/// [`hold_installation`] for a run whose running image is the file `image` names (`/proc/self/exe` for this process).
pub fn hold_installation_as(exe: &Path, image: &Path) -> Result<Option<std::fs::File>, Unusable> {
    let removed = exe.to_str().and_then(|p| p.strip_suffix(" (deleted)"));
    let named = removed.map_or_else(|| exe.to_path_buf(), PathBuf::from);
    let canonical = std::fs::canonicalize(&named).ok();
    let Some(root) = installation_root(canonical.as_deref().unwrap_or(&named)) else {
        return Ok(None);
    };
    let being_removed = |why: String| {
        Unusable(format!(
            "this installation ({}) is being uninstalled: {why}",
            root.display()
        ))
    };
    let Some(canonical) = canonical.filter(|_| removed.is_none()) else {
        return Err(being_removed(format!("{} was removed", named.display())));
    };
    let path = root.join(".lock");
    // Read-only: a shared flock needs no more (a 0400 lock is fine). Gone with the binary: the uninstall is removing
    // the installation; gone with the binary still there: the lock alone is missing (M2T14R-352); any other failure is
    // said as it is (M2T14R-250).
    let file = std::fs::File::open(&path).map_err(|e| {
        if e.kind() != std::io::ErrorKind::NotFound {
            Unusable(format!(
                "cannot open the installation's lock {}: {e}",
                path.display()
            ))
        } else if canonical.exists() {
            Unusable(format!(
                "the installation's lock {} is missing: run install.sh from the release bundle again",
                path.display()
            ))
        } else {
            being_removed(format!("its lock {} is gone", path.display()))
        }
    })?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockShared)
        .map_err(|e| being_removed(format!("its lock is held ({e})")))?;
    if !still_linked(&file, &path) {
        return Err(being_removed(
            "its lock was removed before it was held".to_owned(),
        ));
    }
    // The running image decides, not a path read earlier (design correction 6, M2T14R-350): it must still be the file
    // at its canonical path, linked; a binary removed (and maybe installed again at the same path) is not.
    if !same_file(image, &canonical) {
        return Err(being_removed(format!(
            "the binary this run started from ({}) was removed or replaced",
            canonical.display()
        )));
    }
    Ok(Some(file))
}

/// Whether the file `image` names (following it, as `/proc/self/exe` is followed to the running image) is linked and
/// is the file at `path`.
fn same_file(image: &Path, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::metadata(image), std::fs::metadata(path)) {
        (Ok(run), Ok(named)) => {
            run.nlink() > 0 && run.dev() == named.dev() && run.ino() == named.ino()
        }
        _ => false,
    }
}

/// Whether `file` is still the file at `path` (same device and inode, and linked): a lock got on a file the uninstall
/// removed or replaced meanwhile holds nothing.
pub fn still_linked(file: &std::fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(held), Ok(named)) => {
            held.nlink() > 0 && held.dev() == named.dev() && held.ino() == named.ino()
        }
        _ => false,
    }
}

/// The environment file of `instance` in the installation at `root`: the only one HoH writes for it.
pub fn instance_file(root: &Path, instance: &str) -> PathBuf {
    root.join("hosts").join(format!("{instance}.env"))
}

/// The template for the hoh installed in `home` (`<home>/.local/bin/hoh`) from the installation at `root`, or why
/// it cannot run from there.
///
/// The program is written whole in double quotes with `%` doubled (a specifier otherwise); `$` is not expanded in a
/// program's path. systemd refuses a program whose path has `"`, `\` or `'` however escaped, so such a home gets no
/// template (verified under a private manager, ADR-023 decision 1).
pub fn template(home: &Path, root: &Path) -> Result<String, Unusable> {
    let home = unit_path("the home directory", home)?;
    let root = unit_path("the installation's root", root)?;
    // EnvironmentFile= takes a glob: a root with one of its characters would name other files (M2T14R-101).
    if let Some(c) = root.chars().find(|c| matches!(c, '*' | '?' | '[')) {
        return Err(Unusable(format!(
            "the installation's root has {c:?}, which EnvironmentFile= reads as a glob ({root:?})"
        )));
    }
    let program = format!("{home}/.local/bin/hoh");
    let file = format!("{root}/hosts/%i.env");
    Ok(format!(
        "# Written by the HoH installer at each install, removed by `install.sh --uninstall`.\n\
         # Local changes belong in drop-ins: systemctl --user edit hoh-host@.service\n\
         [Unit]\n\
         Description=HoH host (project %i)\n\
         StartLimitIntervalSec=300\n\
         StartLimitBurst=5\n\
         \n\
         [Service]\n\
         Type=exec\n\
         EnvironmentFile={file}\n\
         ExecStart=\"{program}\" __host ${{HOH_PROJECT}}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         RestartPreventExitStatus=64 69 78\n\
         Delegate=yes\n\
         KillMode=control-group\n\
         TimeoutStopSec=15\n\
         StandardInput=null\n\
         StandardOutput=null\n\
         StandardError=append:%t/hoh-host@%i.stderr\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    ))
}

/// `value` in double quotes as systemd.exec(5) reads an environment file: `\`, `"`, `` ` `` and `$` escaped.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The environment file of `project`'s instance: the project for `${HOH_PROJECT}` and the PATH its host finds the
/// agents on (ADR-023 decision 2), or why they cannot be written so that systemd reads them back unchanged.
pub fn environment(project: &Path, path: &OsStr) -> Result<String, Unusable> {
    let project = text_of("the project's path", project)?;
    let path = path
        .to_str()
        .ok_or_else(|| Unusable("PATH is not UTF-8".to_owned()))?;
    if let Some(c) = path.chars().find(|c| !plain(*c)) {
        return Err(Unusable(format!(
            "PATH has {c:?}, which systemd does not take"
        )));
    }
    Ok(format!(
        "HOH_PROJECT={}\nPATH={}\n",
        quoted(project),
        quoted(path)
    ))
}

/// The project an environment file this module wrote names; `None` for anything else.
pub fn project_of(text: &str) -> Option<PathBuf> {
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix("HOH_PROJECT=\""))?
        .strip_suffix('"')?;
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(e @ ('\\' | '"' | '`' | '$')) => out.push(e),
                _ => return None,
            },
            '"' => return None,
            c => out.push(c),
        }
    }
    Some(PathBuf::from(out))
}

/// Why an environment file was not written.
#[derive(Debug)]
pub enum WriteError {
    /// It names this other project, whose instance is the same (a hash collision): nothing is overwritten.
    Collision(PathBuf),
    Io(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Collision(other) => write!(
                f,
                "its instance is already that of {} (the same hash): not overwritten",
                other.display()
            ),
            WriteError::Io(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for WriteError {}

/// Write `text`, the environment file of `project`, at `file` (in a directory only this user can enter, readable only
/// by this user), whole: a new file renamed over the old one, since systemd reads it again before each restart. A
/// file that names another project is left as it is.
pub fn write_environment(file: &Path, project: &Path, text: &str) -> Result<(), WriteError> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    let io = |what: &str, at: &Path, e: std::io::Error| {
        WriteError::Io(format!("cannot {what} {}: {e}", at.display()))
    };
    let dir = file
        .parent()
        .ok_or_else(|| WriteError::Io(format!("{} has no directory", file.display())))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| io("create", dir, e))?;
    match std::fs::read_to_string(file) {
        Ok(old) => {
            if let Some(other) = project_of(&old).filter(|p| p != project) {
                return Err(WriteError::Collision(other));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // Not text (so not ours) is replaced like any damaged file; another failure is reported.
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {}
        Err(e) => return Err(io("read", file, e)),
    }
    let mut name = file.as_os_str().to_owned();
    name.push(format!(".tmp.{}", std::process::id()));
    let temporary = PathBuf::from(name);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&temporary, file));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(io("write", file, e));
    }
    Ok(())
}

/// `systemctl show` output as properties, each line split at its first `=`.
pub fn show(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// Whether `systemctl show -p EnvironmentFiles` output (`text`, one line per file: a drop-in may add files or reset
/// them) names `file`, read without ignoring errors (so a missing file fails the start).
pub fn lists_environment_file(text: &str, file: &Path) -> bool {
    let wanted = format!("EnvironmentFiles={} (ignore_errors=no)", file.display());
    text.lines().any(|line| line == wanted)
}

/// Whether an `ExecStart=` property runs `program`'s host: its path exactly `program` and its arguments the
/// template's. Compared as a prefix, not split into fields, since a path may hold ` ; `.
pub fn runs(exec_start: &str, program: &Path) -> bool {
    let program = program.display();
    exec_start.starts_with(&format!(
        "{{ path={program} ; argv[]={program} __host ${{HOH_PROJECT}} ; "
    ))
}

/// The host's unit a process runs as, by its `/proc/self/cgroup` (`text`): the cgroup v2 line's last segment when it
/// is an instance of the template, else `None`.
pub fn supervisor_of(text: &str) -> Option<String> {
    let path = text.lines().find_map(|line| line.strip_prefix("0::"))?;
    let name = path.rsplit('/').next()?;
    let instance = name.strip_prefix("hoh-host@")?.strip_suffix(".service")?;
    (instance.len() == 16
        && instance
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    .then(|| name.to_owned())
}

#[cfg(test)]
mod tests;
