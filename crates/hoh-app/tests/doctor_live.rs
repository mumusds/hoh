//! TEST-006-03 against the installed CLIs (live; model calls). Run only by scripts/product/adapter-live.sh, which sets
//! HOH_LIVE_OUT and turns the verdict written there into the check's evidence. The name carries no TEST id, so the
//! integration check never reports it as skipped.
//!
//! The steps:
//! 1. Record each CLI's `--version` as it prints it.
//! 2. `hoh doctor --output json`: each version equals the native one, no item is verified, and every child command's
//!    argv is in the probe whitelist (ADR-015 decision 5).
//! 3. `hoh doctor --live --output json`: each agent that completed its minimal native turn is verified with a live
//!    attempt record (mode=live, a native session id); one not logged in is missing and the exit code is 3.
//!
//! pi not installed is NOT_RUN for pi. A run where no agent could be verified for want of a login is BLOCKED, never
//! PASS.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Whether a default-mode child argv (without the program) is one doctor may run.
fn whitelisted(args: &[String]) -> bool {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    matches!(
        a.as_slice(),
        ["--version"]
            | ["--help"]
            | ["login", "status"]
            | ["auth", "status", "--json"]
            | ["list", "--no-approve"]
            | ["app-server", "generate-json-schema", "--out", _]
            | ["auth", "check", "--provider", _, "--json", "--no-refresh"]
    ) || (a.len() >= 2
        && a.last() == Some(&"--help")
        && !a[..a.len() - 1].iter().any(|x| x.starts_with('-')))
}

fn doctor(args: &[&str]) -> R<(Option<i32>, Value)> {
    let out = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .arg("doctor")
        .args(args)
        .current_dir(tempfile::tempdir()?.path())
        .output()?;
    let report: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("{e}: {}", String::from_utf8_lossy(&out.stdout)))?;
    Ok((out.status.code(), report))
}

fn verdict(
    out: &std::path::Path,
    status: &str,
    detail: &str,
    sessions: &[Value],
    observations: &Value,
) -> R {
    std::fs::write(
        out.join("doctor-live-verdict.json"),
        serde_json::to_vec_pretty(
            &json!({"status": status, "detail": detail, "sessions": sessions,
                                          "observations": observations}),
        )?,
    )?;
    Ok(())
}

#[test]
#[ignore = "live: runs the installed CLIs through scripts/product/adapter-live.sh"]
fn doctor_live_versions_whitelist_and_verified_logins() -> R {
    let out = PathBuf::from(std::env::var_os("HOH_LIVE_OUT").ok_or("HOH_LIVE_OUT is not set")?);
    let mut native = serde_json::Map::new();
    for agent in ["codex", "claude", "pi"] {
        let line = Command::new(agent)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .map(str::to_owned)
            });
        native.insert(agent.to_owned(), json!(line));
    }
    let mut problems = Vec::new();
    let (code, default) = doctor(&["--output", "json"])?;
    for agent in ["codex", "claude", "pi"] {
        let item = &default["agents"][agent];
        if native[agent].is_null() {
            if item["installed"] != false {
                problems.push(format!(
                    "default: {agent} is not installed but doctor says {item}"
                ));
            }
            continue;
        }
        if item["version"] != native[agent] {
            problems.push(format!(
                "default: {agent} version {} but --version says {}",
                item["version"], native[agent]
            ));
        }
        if !matches!(
            item["auth_check"].as_str(),
            Some("not_verified" | "missing" | "unknown")
        ) {
            problems.push(format!(
                "default: {agent} auth_check {}",
                item["auth_check"]
            ));
        }
    }
    for run in default["probes"].as_array().into_iter().flatten() {
        let args: Vec<String> = run["argv"]
            .as_array()
            .into_iter()
            .flatten()
            .skip(1)
            .filter_map(|a| a.as_str().map(str::to_owned))
            .collect();
        if !whitelisted(&args) {
            problems.push(format!("default: {} is not a probe command", run["argv"]));
        }
    }
    let (live_code, live) = doctor(&["--live", "--output", "json"])?;
    let mut sessions = Vec::new();
    let mut missing = false;
    let mut verified = 0;
    for agent in ["codex", "claude", "pi"] {
        if native[agent].is_null() {
            continue;
        }
        let item = &live["agents"][agent];
        match item["auth_check"].as_str() {
            Some("verified") => {
                verified += 1;
                let record = &item["live"];
                let session = record["native_session_id"].as_str().unwrap_or_default();
                if record["mode"] != "live"
                    || session.is_empty()
                    || record["stored"]["state"] != "SETTLED"
                {
                    problems.push(format!(
                        "live: {agent} verified without a live attempt record: {record}"
                    ));
                }
                sessions.push(json!({"agent": agent, "cli_version": native[agent], "session_id": session,
                                     "resolved_model": record["stored"]["resolved_model"].as_str().unwrap_or("unknown")}));
            }
            Some("missing") => missing = true,
            other => problems.push(format!(
                "live: {agent} auth_check {other:?}: {}",
                item["live"]
            )),
        }
    }
    if missing && live_code != Some(3) {
        problems.push(format!(
            "live: an agent is missing but the exit code is {live_code:?}"
        ));
    }
    let observations = json!({"native_versions": native, "default_exit": code, "live_exit": live_code,
                              "default": default, "live": live});
    if problems.is_empty() && verified == 0 {
        verdict(
            &out,
            "BLOCKED",
            "no agent could be verified: none is logged in",
            &sessions,
            &observations,
        )?;
        return Ok(());
    }
    if problems.is_empty() {
        let pi = if native["pi"].is_null() {
            "; pi NOT_RUN (not installed)"
        } else {
            ""
        };
        verdict(
            &out,
            "PASS",
            &format!(
                "doctor: versions and probe whitelist hold; {verified} agent(s) verified live{pi}"
            ),
            &sessions,
            &observations,
        )?;
        Ok(())
    } else {
        verdict(&out, "FAIL", &problems.join("; "), &sessions, &observations)?;
        Err(problems.join("; ").into())
    }
}
