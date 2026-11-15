//! Registering an installed agent's version in a project's compat manifest (docs/adr/ADR-030.md decision 10, "测试接缝"):
//! the product's own compat suite run on the agent found on a PATH, its result put in the manifest by the same
//! replacement a host's publication uses (the manifest the file holds with this agent's entry replaced, valid against
//! the locked schema, written to a temporary file, synced and renamed). What is written is what the suite earned, never
//! a forged PASS. Tests register this way before a host starts; while a host holds the project it refuses, the host
//! being the manifest's writer (K6).

use crate::host::agents::{self, PROBE_TIMEOUT};
use hoh_agents::compat::suite::{self, SuiteResult};
use hoh_agents::compat::{Agent, CompatManifest};
use hoh_agents::probe::{ProbeEnv, ProbeStop};
use hoh_core::compat_manifest::{self, Files};
use std::ffi::OsStr;
use std::path::Path;

/// Run the compat suite on `agent` as found on `path` (a PATH) and publish its entry in `project`'s manifest: the
/// suite's result, whatever its status; an error, and nothing written, when the run earned nothing (its files changed
/// meanwhile, its probe failed) or a host holds the project.
pub fn register(project: &Path, agent: Agent, path: &OsStr) -> Result<SuiteResult, String> {
    let state = project.join(".hoh");
    hoh_core::state_dir::ensure(&state)
        .map_err(|e| format!("cannot make {}: {e}", state.display()))?;
    // Held for the whole registration: a host does not start meanwhile, nor is one running. Released explicitly, so
    // a child another thread forked meanwhile does not keep it held for the next registration or the host's start.
    let lock = crate::host::DirLock::try_take(&state)
        .map_err(|e| format!("cannot open {}: {e}", state.display()))?
        .ok_or_else(|| format!("a host holds {}", state.display()))?;
    let stop = ProbeStop::default();
    let env = ProbeEnv {
        path: path.to_os_string(),
        fixture_root: None,
        timeout: PROBE_TIMEOUT,
        log: None,
        deadline: None,
        cancel: Some(stop.clone()),
    };
    let (entry, stamp) = agents::probed_entry(agent, &env)?;
    let result = suite::run(agent, &entry, &env, &stop).map_err(|e| e.to_string())?;
    let now = agents::stamp_now(
        Path::new(&entry.launcher_path),
        Path::new(&entry.implementation_path),
    );
    if now != Some(stamp) {
        return Err(format!(
            "{}'s files changed while its suite ran: nothing registered",
            agent.as_str()
        ));
    }
    // The manifest as a host would hold it: one that cannot be read or is not schema-valid is empty.
    let base = agents::read_manifest(&compat_manifest::path(project))
        .ok()
        .flatten()
        .map(|m: CompatManifest| serde_json::to_value(m))
        .transpose()
        .map_err(|e| e.to_string())?;
    let earned = serde_json::to_value(&result.entry).map_err(|e| e.to_string())?;
    let manifest = compat_manifest::replaced(
        base.as_ref(),
        agent.as_str(),
        &earned,
        &hoh_core::time::now_rfc3339(),
    )?;
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let replaced = compat_manifest::replace(
        &Files::of(project, &format!("register.{}", std::process::id())),
        &bytes,
    )?;
    drop(lock);
    if let compat_manifest::Replaced::Unsynced(why) = replaced {
        return Err(why);
    }
    Ok(result)
}
