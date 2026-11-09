//! What starting a turn of an agent needs from the agent itself: its help texts and, for Codex, the App Server's
//! schema (docs/adr/ADR-022.md decision 10). `hoh doctor --live` and the host's product executor read them the same
//! way, through the probe's bounded commands.

use crate::compat::Agent;
use crate::probe::{self, ProbeEnv};
use std::collections::BTreeSet;
use std::path::Path;

/// An agent's launch facts, read from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Facts {
    Claude {
        help: String,
    },
    Codex {
        /// `codex app-server --help`, when `codex --help` lists app-server.
        app_server_help: Option<String>,
        /// `codex exec --help` (empty without an exec subcommand: exec cannot start).
        exec_help: String,
        exec_resume_help: Option<String>,
        /// The App Server schema's notifications, requests and client notifications (empty when it cannot start
        /// on stdio, so its schema is not asked for).
        known: BTreeSet<String>,
        requests: BTreeSet<String>,
        client_notifications: BTreeSet<String>,
    },
    Pi {
        help: String,
    },
}

/// The stdout of `launcher args`, which must succeed.
pub fn stdout_of(launcher: &Path, args: &[&str], env: &ProbeEnv) -> Result<String, String> {
    let out = probe::run_status(launcher, args, env).map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "{} {} exited with {}",
            launcher.display(),
            args.join(" "),
            out.status
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Read `agent`'s launch facts from `launcher`.
pub fn read(agent: Agent, launcher: &Path, env: &ProbeEnv) -> Result<Facts, String> {
    Ok(match agent {
        Agent::Claude => Facts::Claude {
            help: stdout_of(launcher, &["--help"], env)?,
        },
        Agent::Codex => {
            // The transports `codex --help` lists, read as the probe reads them (M1R-03); app-server's schema names the
            // methods of both directions, and the adapter uses it only if they have everything a turn uses, as the probe
            // decides (M1T07R-15, M1R-04).
            let top = stdout_of(launcher, &["--help"], env)?;
            let listed = |name: &str| crate::adapter::lists_command(&top, name);
            let app_server_help = match listed("app-server") {
                true => Some(stdout_of(launcher, &["app-server", "--help"], env)?),
                false => None,
            };
            // An App Server that cannot start on stdio is not used whatever its schema says, so its schema is not asked
            // for; the adapter then takes exec, saying why (the probe's order, M1T07R-27, -29).
            let startable = app_server_help
                .as_deref()
                .filter(|help| crate::codex::app_server_launch_missing(help).is_none());
            let (known, requests, client_notifications) = match startable {
                None => Default::default(),
                Some(_) => {
                    // The probe's validated reader: a schema the probe refuses is refused here too (M1T07R-31).
                    let schema = probe::codex_schema(launcher, env).map_err(|e| e.to_string())?;
                    (
                        schema.notifications,
                        schema.requests,
                        schema.client_notifications,
                    )
                }
            };
            // Without an exec subcommand the exec fallback has no help, so it cannot start (codex::exec_args).
            let (exec_help, exec_resume_help) = match listed("exec") {
                true => (
                    stdout_of(launcher, &["exec", "--help"], env)?,
                    stdout_of(launcher, &["exec", "resume", "--help"], env).ok(),
                ),
                false => (String::new(), None),
            };
            Facts::Codex {
                app_server_help,
                exec_help,
                exec_resume_help,
                known,
                requests,
                client_notifications,
            }
        }
        Agent::Pi => Facts::Pi {
            help: stdout_of(launcher, &["--help"], env)?,
        },
    })
}
