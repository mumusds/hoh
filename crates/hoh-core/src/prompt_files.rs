//! Override filesystem operations. Policy is in `prompt`; no host or database dependency.

use crate::prompt::{self, Error};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SAVES: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests;

/// The global HoH configuration root. Reading this does not create or modify a user directory.
/// Installed binaries and the TUI use this. A cargo test process that must not read `$HOME`
/// uses [`product_global_root`] (M3E-02).
pub fn global_root() -> Option<PathBuf> {
    global_root_for(
        None,
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// Resolve the global root from explicit inputs. A non-empty `xdg` wins. When `cargo_manifest`
/// is set and `xdg` is absent, the result is `None`: `$HOME/.config` is not a fallback.
/// When `cargo_manifest` is absent, the fallback is `$HOME/.config/hoh`, as [`global_root`].
pub fn global_root_for(
    cargo_manifest: Option<&std::ffi::OsStr>,
    xdg: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let xdg = xdg.filter(|value| !value.is_empty());
    if cargo_manifest.is_some() {
        return xdg.map(|root| PathBuf::from(root).join("hoh"));
    }
    xdg.map(PathBuf::from)
        .or_else(|| home.map(|path| PathBuf::from(path).join(".config")))
        .map(|root| root.join("hoh"))
}

/// The product host's global root. `cargo test` and `cargo run` set `CARGO_MANIFEST_DIR`; those
/// processes read `$XDG_CONFIG_HOME/hoh` only, and do not fall back to `$HOME/.config/hoh`.
/// An installed binary has no `CARGO_MANIFEST_DIR` and keeps [`global_root`]'s rule.
pub fn product_global_root() -> Option<PathBuf> {
    global_root_for(
        std::env::var_os("CARGO_MANIFEST_DIR").as_deref(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// Read each Attempt's directory chain afresh. A configured project file replaces the conventional project file.
pub fn load(
    root: &Path,
    global: Option<&Path>,
    run: &str,
    role: &str,
    configured: Option<&str>,
) -> Result<prompt::Resolved, Error> {
    load_with_bundle(
        root,
        global,
        run,
        role,
        configured,
        &crate::resource_bundle::Bundle::current(),
    )
}

pub fn load_with_bundle(
    root: &Path,
    global: Option<&Path>,
    run: &str,
    role: &str,
    configured: Option<&str>,
    bundle: &crate::resource_bundle::Bundle,
) -> Result<prompt::Resolved, Error> {
    use crate::config::{self, Contract};
    let builtin = configured
        .and_then(|p| p.strip_prefix(config::BUILTIN_PROMPT))
        .unwrap_or(role);
    let mut sources = Vec::new();
    if let Ok(text) = bundle.text(&format!("resources/prompts/{builtin}.md")) {
        sources.push(Contract {
            layer: "builtin",
            path: format!("resources/prompts/{builtin}.md"),
            text: text.to_owned(),
        });
    } else if configured.is_none()
        || configured.is_some_and(|p| p.starts_with(config::BUILTIN_PROMPT))
    {
        return Err(prompt::unavailable(format!(
            "{builtin} has no built-in role contract"
        )));
    }
    let role = crate::layout::component(role);
    if role.is_empty() || role == "." || role == ".." {
        return Err(prompt::unavailable("invalid role path component"));
    }
    let explicit = configured.filter(|p| !p.starts_with(config::BUILTIN_PROMPT));
    for layer in override_layers(root, global, run)? {
        let relative = format!("prompts/{role}.md");
        let source = if layer.layer == "project" && explicit.is_some() {
            let path = explicit.unwrap_or_default();
            Some(Contract {
                layer: "project",
                path: path.to_owned(),
                text: read_optional(root, path)?.ok_or_else(|| {
                    prompt::unavailable(format!("{path}: configured contract is missing"))
                })?,
            })
        } else {
            layer.read(&relative)?
        };
        if let Some(source) = source {
            sources.push(source);
        }
    }
    prompt::resolve(sources)
}

/// Shared directory chain for role contracts, skills and templates (ADR-034, ADR-040).
#[derive(Debug, Clone)]
pub struct OverrideLayer {
    pub layer: &'static str,
    root: PathBuf,
    prefix: String,
}

impl OverrideLayer {
    pub fn read(&self, relative: &str) -> Result<Option<crate::config::Contract>, Error> {
        let relative = format!("{}{relative}", self.prefix);
        Ok(
            read_optional(&self.root, &relative)?.map(|text| crate::config::Contract {
                layer: self.layer,
                path: if self.layer == "global" {
                    self.root.join(&relative).display().to_string()
                } else {
                    relative
                },
                text,
            }),
        )
    }
}

pub fn override_layers(
    root: &Path,
    global: Option<&Path>,
    run: &str,
) -> Result<Vec<OverrideLayer>, Error> {
    let run = crate::layout::component(run);
    if run.is_empty() || run == "." || run == ".." {
        return Err(prompt::unavailable("invalid run path component"));
    }
    let mut layers = Vec::new();
    if let Some(global) = global {
        layers.push(OverrideLayer {
            layer: "global",
            root: global.to_owned(),
            prefix: String::new(),
        });
    }
    layers.push(OverrideLayer {
        layer: "project",
        root: root.to_owned(),
        prefix: ".hoh/".into(),
    });
    layers.push(OverrideLayer {
        layer: "run",
        root: root.to_owned(),
        prefix: format!(".hoh/runs/{run}/"),
    });
    Ok(layers)
}

/// Missing is the only condition that means an absent layer. Check existing ancestors, including links, first.
fn optional_file(root: &Path, relative: &str) -> Result<Option<PathBuf>, Error> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(prompt::unavailable(
            "override path must be relative without ..",
        ));
    }
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(prompt::unavailable(e)),
        Ok(_) => (),
    }
    let root = root.canonicalize().map_err(prompt::unavailable)?;
    let mut path = root.clone();
    for component in relative.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(prompt::unavailable(e)),
            Ok(_) => (),
        }
        if !path
            .canonicalize()
            .map_err(prompt::unavailable)?
            .starts_with(&root)
        {
            return Err(prompt::unavailable(format!(
                "{} leads outside root",
                path.display()
            )));
        }
    }
    if !std::fs::metadata(&path)
        .map_err(prompt::unavailable)?
        .is_file()
    {
        return Err(prompt::unavailable(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    Ok(Some(path))
}

pub(crate) fn read_optional(root: &Path, relative: &str) -> Result<Option<String>, Error> {
    optional_file(root, relative)?
        .map(|path| {
            std::fs::read_to_string(&path)
                .map_err(|e| prompt::unavailable(format!("{}: {e}", path.display())))
        })
        .transpose()
}

/// References only: document text is never interpreted as evidence or template variables.
pub fn project_references(root: &Path) -> Result<serde_json::Value, Error> {
    use serde_json::json;
    let reference = |relative: &str| -> Result<Option<serde_json::Value>, Error> {
        optional_file(root, relative)?
            .map(|path| {
                let bytes = std::fs::read(&path).map_err(prompt::unavailable)?;
                Ok(json!({"path": relative, "sha256": crate::context::sha256_hex(&bytes)}))
            })
            .transpose()
    };
    let dir = root.join("docs/adr");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => Some(entries),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(prompt::unavailable(e)),
    };
    let mut paths = Vec::new();
    if let Some(entries) = entries {
        for entry in entries {
            let entry = entry.map_err(prompt::unavailable)?;
            if entry.path().extension().is_some_and(|e| e == "md") {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| prompt::unavailable("ADR path is not UTF-8"))?;
                paths.push(format!("docs/adr/{name}"));
            }
        }
    }
    paths.sort();
    let mut adrs = Vec::new();
    for path in paths {
        adrs.push(reference(&path)?.ok_or_else(|| {
            prompt::unavailable(format!("{path} disappeared while collecting references"))
        })?);
    }
    Ok(json!({"adrs": adrs, "conventions": reference(".hoh/conventions.md")?}))
}

/// Validate before opening any writable file; stage beside the destination and rename only after syncing.
/// A failed staged write is retained at the exact path named in the error, never mistaken for a saved override.
pub fn save_override(root: &Path, relative: &str, text: &str) -> Result<(), Error> {
    prompt::validate_size(text)?;
    prompt::validate_variables(text)?;
    let role = Path::new(relative)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let bundle = crate::resource_bundle::Bundle::current();
    if bundle.text(&format!("resources/prompts/{role}.md")).is_ok() {
        return save_override_based_on(root, relative, text, role, &bundle);
    }
    write_atomic(root, relative, text)
}

/// Save against a named builtin (also supports configured filenames). The sidecar binds both source and override
/// digests: a crash between the two renames is diagnosed as unknown base, never mistaken for a matching baseline.
pub fn save_override_based_on(
    root: &Path,
    relative: &str,
    text: &str,
    builtin: &str,
    bundle: &crate::resource_bundle::Bundle,
) -> Result<(), Error> {
    save_based_on(root, relative, text, builtin, bundle, &|| {})
}

fn save_based_on(
    root: &Path,
    relative: &str,
    text: &str,
    builtin: &str,
    bundle: &crate::resource_bundle::Bundle,
    after_base: &dyn Fn(),
) -> Result<(), Error> {
    prompt::validate_size(text)?;
    prompt::validate_variables(text)?;
    let base = bundle
        .text(&format!("resources/prompts/{builtin}.md"))
        .map_err(prompt::unavailable)?;
    let sidecar = serde_json::json!({"builtin": base, "builtin_sha256": crate::context::sha256_hex(base.as_bytes()),
        "resource_bundle_version": bundle.version(), "override_sha256": crate::context::sha256_hex(text.as_bytes())});
    // Preflight the destination before changing metadata. write_atomic repeats the check before its own rename.
    optional_file(root, relative)?;
    write_atomic(root, &format!("{relative}.base.json"), &sidecar.to_string())?;
    after_base();
    write_atomic(root, relative, text)
}

fn write_atomic(root: &Path, relative: &str, text: &str) -> Result<(), Error> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(prompt::unavailable(
            "override path must be relative without dot components",
        ));
    }
    let root = root.canonicalize().map_err(prompt::unavailable)?;
    let mut target = root.clone();
    for component in path.components() {
        target.push(component);
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(prompt::unavailable(
                    "override path contains a symbolic link",
                ));
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(prompt::unavailable(e)),
        }
    }
    if let Ok(meta) = std::fs::metadata(&target)
        && !meta.is_file()
    {
        return Err(prompt::unavailable("override is not a regular file"));
    }
    let parent = target
        .parent()
        .ok_or_else(|| prompt::unavailable("override has no parent"))?;
    std::fs::create_dir_all(parent).map_err(prompt::unavailable)?;
    if !parent
        .canonicalize()
        .map_err(prompt::unavailable)?
        .starts_with(&root)
    {
        return Err(prompt::unavailable("override parent leads outside root"));
    }
    let (staged, mut file) = loop {
        let staged = parent.join(format!(
            ".prompt-save-{}-{}",
            std::process::id(),
            SAVES.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::File::create_new(&staged) {
            Ok(file) => break (staged, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(prompt::unavailable(e)),
        }
    };
    let failure = |e: std::io::Error| prompt::unavailable(format!("{}: {e}", staged.display()));
    file.write_all(text.as_bytes()).map_err(failure)?;
    file.sync_all().map_err(failure)?;
    std::fs::rename(&staged, &target).map_err(failure)?;
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(prompt::unavailable)
}

/// Restore only the conventional project override owned by this product. No glob, recursive removal or configured
/// arbitrary file deletion. Directory handles and NOFOLLOW prevent a swapped ancestor from redirecting the unlink.
pub fn restore_project(root: &Path, role: &str, configured: Option<&str>) -> Result<bool, Error> {
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    if configured.is_some_and(|p| !p.starts_with(crate::config::BUILTIN_PROMPT)) {
        return Err(prompt::unavailable(
            "配置指定了文件；请先移除 prompt 配置再恢复约定路径",
        ));
    }
    if crate::config::builtin_contract(
        configured
            .and_then(|p| p.strip_prefix(crate::config::BUILTIN_PROMPT))
            .unwrap_or(role),
    )
    .is_none()
    {
        return Err(prompt::unavailable("该职位没有内置默认契约"));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = fs::open(root, flags, Mode::empty()).map_err(prompt::unavailable)?;
    for name in [".hoh", "prompts"] {
        dir = match fs::openat(&dir, name, flags, Mode::empty()) {
            Ok(next) => next,
            Err(rustix::io::Errno::NOENT) => return Ok(false),
            Err(e) => return Err(prompt::unavailable(e)),
        };
    }
    let name = format!("{}.md", crate::layout::component(role));
    let stat = match fs::statat(&dir, &name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(e) => return Err(prompt::unavailable(e)),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(prompt::unavailable("覆盖不是普通文件，未删除"));
    }
    fs::unlinkat(&dir, &name, AtFlags::empty()).map_err(prompt::unavailable)?;
    fs::fsync(&dir).map_err(prompt::unavailable)?;
    Ok(true)
}

/// Compare each override's saved base to the installed bundle, even while its Attempt still uses an older pin.
/// Missing or mismatched metadata is explicitly unknown; it never supplies invented old bytes to a three-way diff.
pub fn drift_reports(
    root: &Path,
    resolved: &prompt::Resolved,
    installed: &crate::resource_bundle::Bundle,
) -> Result<Vec<serde_json::Value>, Error> {
    use serde_json::{Value, json};
    let Some(builtin) = resolved.chain.iter().find(|s| s.layer == "builtin") else {
        return Ok(Vec::new());
    };
    let new = installed.text(&builtin.path).map_err(prompt::unavailable)?;
    let new_sha = crate::context::sha256_hex(new.as_bytes());
    let mut reports = Vec::new();
    for source in resolved.chain.iter().filter(|s| s.layer != "builtin") {
        let sidecar = format!("{}.base.json", source.path);
        let data = if source.layer == "global" {
            let path = Path::new(&sidecar);
            read_optional(
                path.parent()
                    .ok_or_else(|| prompt::unavailable("base has no parent"))?,
                path.file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| prompt::unavailable("invalid base filename"))?,
            )?
        } else {
            read_optional(root, &sidecar)?
        };
        let base: Option<Value> = data.and_then(|s| serde_json::from_str(&s).ok());
        let override_sha = crate::context::sha256_hex(source.text.as_bytes());
        let old = base.as_ref().and_then(|b| {
            let text = b["builtin"].as_str()?;
            (b["builtin_sha256"] == crate::context::sha256_hex(text.as_bytes())
                && b["override_sha256"] == override_sha
                && b["resource_bundle_version"].is_string())
            .then_some(text)
        });
        if old.is_some_and(|old| old == new) {
            continue;
        }
        reports.push(json!({"layer":source.layer, "path":source.path, "base_status":if old.is_some() {"known"} else {"missing_or_mismatched"},
            "base_resource_bundle_version":base.as_ref().and_then(|b|b["resource_bundle_version"].as_str()),
            "resource_bundle_version":installed.version(), "builtin_sha256":new_sha,
            "old_builtin_sha256":old.map(|s|crate::context::sha256_hex(s.as_bytes())), "override_sha256":override_sha,
            "diff":old.map(|old|prompt::three_way_diff(old,new,&source.text)),
            "current_diff":prompt::unified_diff(new,&source.text,"new-builtin","override")}));
    }
    Ok(reports)
}
