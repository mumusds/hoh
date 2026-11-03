#!/usr/bin/env bash
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
[[ ! -f "$ROOT/.hoh-dev/env.sh" ]] || source "$ROOT/.hoh-dev/env.sh"
cd "$ROOT"
case "${1:---bundle}" in
 --bundle)
  python3 tools/validate_bundle.py
  PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/bundle -p 'test_*.py' -v
  ;;
 --product)
  [[ -f Cargo.toml ]] || { echo 'NOT_IMPLEMENTED: no Cargo workspace. Bundle checks cannot certify product acceptance.' >&2; exit 3; }
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --locked -- -D warnings
  cargo test --workspace --locked
  echo 'Only Rust checks ran. Final acceptance ALSO requires live agents/plugins, Web, PTY, deployment, recovery and soak evidence.' >&2
  [[ -n "${HOH_RELEASE_MANIFEST:-}" ]] || { echo 'Set HOH_RELEASE_MANIFEST to the complete version-bound release manifest.' >&2; exit 3; }
  python3 tools/release_gate.py --manifest "$HOH_RELEASE_MANIFEST"
  ;;
 *) echo 'Usage: scripts/check.sh --bundle | --product' >&2; exit 64;;
esac
