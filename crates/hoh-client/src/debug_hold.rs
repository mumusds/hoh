//! Debug builds only: the hold points the tests of a client's ownership of its processes use (docs/adr/ADR-031.md,
//! 设计修正 2), so each step of a race is waited for on a file, never on a sleep. With `HOH_DEBUG_HOLD_DIR=<dir>` in
//! the environment, the point `name` is held when `<dir>/<name>` exists: it writes `<dir>/<name>.reached` (this
//! process's pid) and waits while `<dir>/<name>` exists, at most 30 s; [`mark`] writes `<dir>/<name>` with a text.
//! Release builds have neither (like `HOH_DEBUG_TUI_PANIC`, ADR-014 decision 8): both are then empty functions.

/// The environment variable naming the directory of the hold points.
#[cfg(debug_assertions)]
const DIR_ENV: &str = "HOH_DEBUG_HOLD_DIR";

/// How long a held point waits at most.
#[cfg(debug_assertions)]
const LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Hold at the point `name` while its file exists (see the module).
pub fn at(name: &str) {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os(DIR_ENV) {
        let dir = std::path::Path::new(&dir);
        let gate = dir.join(name);
        if gate.exists() {
            write(
                &dir.join(format!("{name}.reached")),
                &std::process::id().to_string(),
            );
            let end = std::time::Instant::now() + LIMIT;
            while gate.exists() && std::time::Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = name;
}

/// Say that the point `name` was passed, with `text` (see the module).
pub fn mark(name: &str, text: &str) {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os(DIR_ENV) {
        write(&std::path::Path::new(&dir).join(name), text);
    }
    #[cfg(not(debug_assertions))]
    let _ = (name, text);
}

/// `text` written to `file` whole: to a temporary name first, then renamed.
#[cfg(debug_assertions)]
fn write(file: &std::path::Path, text: &str) {
    let mut temporary = file.as_os_str().to_owned();
    temporary.push(".tmp");
    if std::fs::write(&temporary, text).is_ok() {
        let _ = std::fs::rename(&temporary, file);
    }
}
