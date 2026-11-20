#!/usr/bin/env bash
# The release bundle (CR-003, docs/adr/ADR-023.md decision 9) at $1: hoh/bin/hoh (Cargo's executable at $2), LICENSE,
# THIRD_PARTY_NOTICES.md, resources/, install.sh and, once there is a Web GUI, web/; a reproducible tar (fixed order,
# owner and mtime). Used by the build check (build.sh) and the deployment check (deployment.sh).
set -Eeuo pipefail
if [[ "$#" -ne 2 || -z "${1:-}" || -z "${2:-}" ]]; then
  echo 'bundle: usage: bundle.sh <output.tar.gz> <executable>' >&2
  exit 64
fi
out="$1"
executable="$2"
if [[ "$executable" != /* || ! -f "$executable" ]]; then
  echo "bundle: executable must be an existing absolute file: $executable" >&2
  exit 1
fi
[[ "$out" == /* ]] || out="$PWD/$out"
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
# Keep the identity exported before cargo ran; do not replace it with a later Git observation.
expected="${HOH_BUILD_COMMIT-unknown}"
if [[ -v HOH_BUILD_COMMIT && ! "$expected" =~ ^[0-9a-f]{40}(-dirty)?$ ]]; then
  echo "bundle: invalid HOH_BUILD_COMMIT: $expected" >&2
  exit 1
fi
stage="$(mktemp -d)"
archive=""
trap 'rm -rf -- "$stage"; if [[ -n "$archive" ]]; then rm -f -- "$archive"; fi' EXIT
mkdir -p "$stage/hoh/bin"
install -m 0755 "$executable" "$stage/hoh/bin/hoh"
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
if [[ ! -v HOH_BUILD_COMMIT ]]; then
  echo 'bundle: HOH_BUILD_COMMIT is unset; requiring commit unknown (ADR-023 decision 8).' >&2
elif [[ "$expected" == *-dirty ]]; then
  echo "bundle: packing a dirty build; requiring commit $expected (ADR-023 decision 8)." >&2
fi
cp LICENSE THIRD_PARTY_NOTICES.md "$stage/hoh/"
[[ ! -d resources ]] || cp -R resources "$stage/hoh/resources"
[[ ! -f scripts/install.sh ]] || install -m 0755 scripts/install.sh "$stage/hoh/install.sh"
[[ ! -d web/dist ]] || cp -R web/dist "$stage/hoh/web"
# Keep any existing bundle intact even if tar/gzip fails; rename on the output filesystem.
archive="$(mktemp "$out.XXXXXX")"
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C "$stage" -cf - hoh | gzip -n >"$archive"
mv -fT -- "$archive" "$out"
