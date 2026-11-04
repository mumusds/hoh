#!/usr/bin/env bash
# Check "static": formatting and lints with warnings denied.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
cargo fmt --all -- --check
# AC-062-05: clippy.toml's disallowed-methods keep SQLite inside hoh-core::persistence. Nothing from the environment may
# change lint levels or the clippy configuration (M2T01R-24): rustflags and compiler wrappers are cleared, and the
# repository's clippy.toml is the configuration.
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER \
  RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CLIPPY_ARGS
for var in $(compgen -e); do
  if [[ "$var" =~ ^CARGO_TARGET_.*_RUSTFLAGS$ ]]; then unset "$var"; fi
done
export CLIPPY_CONF_DIR="$PWD"
logs="$(mktemp -d)"; trap 'rm -rf "$logs"' EXIT
cargo clippy --workspace --all-targets --locked -- -D warnings 2>&1 | tee "$logs/clippy.log"
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
