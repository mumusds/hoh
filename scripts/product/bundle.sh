#!/usr/bin/env bash
# The release bundle (CR-003, docs/adr/ADR-023.md decision 9) at $1: hoh/bin/hoh (Cargo's release binary), LICENSE,
# THIRD_PARTY_NOTICES.md, resources/, install.sh and, once there is a Web GUI, web/; a reproducible tar (fixed order,
# owner and mtime). Used by the build check (build.sh) and the deployment check (deployment.sh).
set -Eeuo pipefail
out="${1:?usage: bundle.sh <output.tar.gz>}"
[[ "$out" == /* ]] || out="$PWD/$out"
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
# Keep the identity exported before cargo ran; do not replace it with a later Git observation.
expected="${HOH_BUILD_COMMIT-unknown}"
if [[ ! -v HOH_BUILD_COMMIT ]]; then
  echo 'bundle: HOH_BUILD_COMMIT is unset; requiring commit unknown (ADR-023 decision 8).' >&2
elif [[ ! "$expected" =~ ^[0-9a-f]{40}(-dirty)?$ ]]; then
  echo "bundle: invalid HOH_BUILD_COMMIT: $expected" >&2
  exit 1
elif [[ "$expected" == *-dirty ]]; then
  echo "bundle: packing a dirty build; requiring commit $expected (ADR-023 decision 8)." >&2
fi
# Cargo resolves both CARGO_TARGET_DIR and build.target-dir, including relative paths.
# A metadata failure must not fall back to a possibly stale default target directory.
if ! metadata="$(cargo metadata --format-version 1 --no-deps --locked)"; then
  echo 'bundle: cargo metadata failed; refusing to pack.' >&2
  exit 1
fi
if ! target="$(printf '%s\n' "$metadata" | python3 -c '
import json, os, sys
try:
    target = json.load(sys.stdin)["target_directory"]
except (ValueError, KeyError, TypeError):
    sys.exit(1)
if not isinstance(target, str) or not os.path.isabs(target):
    sys.exit(1)
print(target)
' 2>/dev/null)"; then
  echo 'bundle: Cargo gave no absolute target_directory; refusing to pack.' >&2
  exit 1
fi
# A build target triple puts the binary under <target>/<triple>/release (BTD-01). The product scripts never set one;
# CARGO_BUILD_TARGET is honoured, and a build.target in the repository's Cargo configuration is refused rather than
# guessed at.
if grep -Eqs '^[[:space:]]*target[[:space:]]*=' .cargo/config.toml .cargo/config; then
  echo 'bundle: a build target in .cargo/config is not supported; refusing to pack.' >&2
  exit 1
fi
release="$target/${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/hoh/bin"
install -m 0755 "$release/hoh" "$stage/hoh/bin/hoh"
# Verify the staged bytes that will enter the archive, before opening the output file.
if ! version="$("$stage/hoh/bin/hoh" --version)"; then
  echo 'bundle: staged hoh --version failed; refusing to pack it.' >&2
  exit 1
fi
commit_line="$(printf '%s\n' "$version" | sed -n '2p')"
if [[ "$commit_line" != "commit $expected" ]]; then
  echo "bundle: commit mismatch: expected 'commit $expected', got '$commit_line'; refusing to pack it." >&2
  exit 1
fi
cp LICENSE THIRD_PARTY_NOTICES.md "$stage/hoh/"
[[ ! -d resources ]] || cp -R resources "$stage/hoh/resources"
[[ ! -f scripts/install.sh ]] || install -m 0755 scripts/install.sh "$stage/hoh/install.sh"
[[ ! -d web/dist ]] || cp -R web/dist "$stage/hoh/web"
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C "$stage" -cf - hoh | gzip -n >"$out"
