#!/usr/bin/env bash
# Check "static": formatting and lints with warnings denied.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
cargo fmt --all -- --check
# AC-062-05: clippy.toml's disallowed-methods keep SQLite inside hoh-core::persistence. Nothing from the environment may
# change lint levels or the clippy configuration (M2T01R-24): rustflags and compiler wrappers are cleared, and the
# repository's clippy.toml is the configuration.
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS CARGO_BUILD_RUSTC RUSTC CLIPPY_ARGS
# An empty wrapper variable overrides a wrapper set in any cargo configuration; such configuration is also refused by
# the scan below (M2T01R-30).
export RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=''
for var in $(compgen -e); do
  if [[ "$var" =~ ^CARGO_TARGET_.*_RUSTFLAGS$ || "$var" =~ ^CARGO_PROFILE_ ]]; then unset "$var"; fi
done
export CLIPPY_CONF_DIR="$PWD"
logs="$(mktemp -d)"; trap 'rm -rf "$logs"' EXIT
# The members declare no features (the scan refuses them), so the dev and the release build are every build
# (M2T01R-32, -35, -39: code under cfg(not(debug_assertions)) is compiled in the release profile only).
cargo clippy --workspace --all-targets --locked -- -D warnings 2>&1 | tee "$logs/clippy.log"
cargo clippy --workspace --all-targets --release --locked -- -D warnings 2>&1 | tee -a "$logs/clippy.log"
# An entry that does not resolve is only a warning of clippy's configuration, so it fails here.
if grep -q 'does not refer to a reachable function' "$logs/clippy.log"; then
  echo 'static: a clippy.toml disallowed-methods entry does not resolve' >&2
  exit 1
fi
# The canary calls every disallowed method outside the Persistence module; it must fail with exactly those refusals.
# Whatever turns the boundary off where this check runs (cargo configuration, lint tables, caps) shows up here.
if cargo clippy -p hoh-core --lib --locked -- -D warnings --cfg hoh_clippy_canary > "$logs/canary.log" 2>&1; then
  echo 'static: the clippy canary compiled: the AC-062-05 boundary is not in force' >&2
fi
# Dependency policy (deny.toml): licenses, banned crates and sources; advisories need the network and run in CI.
cargo deny --locked check licenses bans sources
# AC-062-05 scan, including the canary's verdict; under run_check its TEST-062-05 result goes to test-results.jsonl
# (CR-003 per-test evidence).
scan=(python3 scripts/product/persistence_scan.py --canary-log "$logs/canary.log")
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then scan+=(--out "$HOH_EVIDENCE_DIR/test-results.jsonl"); fi
"${scan[@]}"
# No source names a machine-specific home directory: tests that created their temporary directories under one
# developer's /home path passed on that machine and failed in CI (M3L-101, PR #108 in Rust; M4L1-001 in a bundle test,
# the same class again, so the scan covers Rust, Python and shell sources alike); use tempfile's defaults / TMPDIR.
found=0; grep -rnE "tempdir_in\\(\"/home|[\"']/home/ai/" --include='*.rs' --include='*.py' --include='*.sh' \
  crates tests tools scripts || found=$?
if [[ "$found" == 0 ]]; then
  echo 'static: a source names a machine-specific /home path; use tempfile defaults' >&2
  exit 1
elif [[ "$found" != 1 ]]; then
  echo "static: the machine-path scan failed (grep exit $found)" >&2
  exit 1
fi
# docs/adr/ADR-031.md, 设计修正 2, E: the client crate does not depend on the host's, every place that starts a process
# or makes the plain spawner is listed with its owner, and every direct stderr write outside the host is listed.
python3 scripts/product/spawn_scan.py
