#!/usr/bin/env bash
# Check "build": release build of everything that exists so far.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
cargo build --workspace --release --locked
if [[ -f web/package.json ]]; then
  npm ci --prefix web
  npm run build --prefix web
else
  # M6-T02 (Web GUI) must turn a missing web/ into a failure once the Web GUI is in scope.
  echo 'build: web/ not present yet (Web GUI arrives in M6); Rust workspace built.' >&2
fi
# Release bundle for the release gate (CR-003): only under tools/run_check.py. Reproducible tar (fixed order, owner, mtime).
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then
  stage="$(mktemp -d)"; trap 'rm -rf "$stage"' EXIT
  mkdir -p "$stage/hoh/bin"
  install -m 0755 target/release/hoh "$stage/hoh/bin/hoh"
  cp LICENSE THIRD_PARTY_NOTICES.md "$stage/hoh/"
  [[ ! -d resources ]] || cp -R resources "$stage/hoh/resources"
  [[ ! -f scripts/install.sh ]] || install -m 0755 scripts/install.sh "$stage/hoh/install.sh"
  [[ ! -d web/dist ]] || cp -R web/dist "$stage/hoh/web"
  tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C "$stage" -cf - hoh | gzip -n > "$HOH_EVIDENCE_DIR/hoh-release.tar.gz"
  echo "build: release bundle $(sha256sum "$HOH_EVIDENCE_DIR/hoh-release.tar.gz" | cut -c1-16)…" >&2
fi
