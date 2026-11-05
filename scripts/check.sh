#!/usr/bin/env bash
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
[[ ! -f "$ROOT/.hoh-dev/env.sh" ]] || source "$ROOT/.hoh-dev/env.sh"
cd "$ROOT"
export PYTHONDONTWRITEBYTECODE=1
case "${1:---bundle}" in
 --bundle)
  python3 tools/validate_bundle.py
  python3 tools/baseline.py verify
  # The self-test never writes into a check it runs inside: no run_check variable reaches its tools and scripts.
  env -u HOH_EVIDENCE_DIR -u HOH_CHECK_ID -u HOH_CANDIDATE_ID -u HOH_SPEC_SHA256 \
    python3 -m unittest discover -s tests/bundle -p 'test_*.py' -v
  ;;
 --reports)
  # Regenerates reports/ from this machine. Commit them only together with the code they describe.
  python3 tools/validate_bundle.py --output reports/bundle-validation.json >/dev/null
  python3 tools/test_bundle.py --output reports/tests-bundle.json
  python3 tools/preflight.py --output reports/environment-preflight.json >/dev/null || true
  ;;
 --product)
  [[ -f Cargo.toml ]] || { echo 'NOT_IMPLEMENTED: no Cargo workspace. Bundle checks cannot certify product acceptance.' >&2; exit 3; }
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --locked -- -D warnings
  cargo test --workspace --locked
  echo 'Only Rust checks ran. Release acceptance runs each check of the frozen check plan via tools/run_check.py, then tools/release_gate.py.' >&2
  [[ -n "${HOH_RELEASE_MANIFEST:-}" ]] || { echo 'Set HOH_RELEASE_MANIFEST to the complete version-bound release manifest.' >&2; exit 3; }
  python3 tools/release_gate.py --manifest "$HOH_RELEASE_MANIFEST"
  ;;
 *) echo 'Usage: scripts/check.sh --bundle | --reports | --product' >&2; exit 64;;
esac
