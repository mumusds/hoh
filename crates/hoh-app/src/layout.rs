//! The names a client shares with the host (docs/adr/ADR-031.md, 设计修正 2, A: the module boundary of CLT-1007): a
//! client's or doctor's source never refers to the `host`, `verify` or `gate` modules, so what both sides name lives
//! here, and the host re-exports it.

use std::path::{Path, PathBuf};

/// Set by the client on a host it starts directly, not in a delegated scope: why (docs/adr/ADR-021.md decision 2). The
/// host records it in host.json, so `hoh doctor` reports a running host's actual reason (CR-012, CR12-03).
pub const UNSCOPED_ENV: &str = "HOH_HOST_UNSCOPED";

/// Set by the client on a host it starts other than as its systemd unit: why (docs/adr/ADR-023.md decisions 6 and 7).
/// The host records it in host.json; without it, a host not run as its unit says so.
pub const UNSUPERVISED_ENV: &str = "HOH_HOST_UNSUPERVISED";

/// `id` as one path component: `%` and `/` escaped, so distinct ids stay distinct. A client reads an attempt's
/// native stderr where [`attempt_logs`] puts it (docs/adr/ADR-031.md decision 11).
pub fn component(id: &str) -> String {
    id.replace('%', "%25").replace('/', "%2F")
}

/// The directory of attempt `attempt_id`'s raw logs in the project at `project`: its agent's `*stderr.log` among them.
pub fn attempt_logs(project: &Path, attempt_id: &str) -> PathBuf {
    project.join(".hoh/attempts").join(component(attempt_id))
}
