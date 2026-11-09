#!/usr/bin/env bash
# The release bundle (CR-003, docs/adr/ADR-023.md decision 9) at $1: hoh/bin/hoh (target/release/hoh), LICENSE,
# THIRD_PARTY_NOTICES.md, resources/, install.sh and, once there is a Web GUI, web/; a reproducible tar (fixed order,
# owner and mtime). Used by the build check (build.sh) and the deployment check (deployment.sh).
set -Eeuo pipefail
out="${1:?usage: bundle.sh <output.tar.gz>}"
[[ "$out" == /* ]] || out="$PWD/$out"
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/hoh/bin"
install -m 0755 target/release/hoh "$stage/hoh/bin/hoh"
cp LICENSE THIRD_PARTY_NOTICES.md "$stage/hoh/"
[[ ! -d resources ]] || cp -R resources "$stage/hoh/resources"
[[ ! -f scripts/install.sh ]] || install -m 0755 scripts/install.sh "$stage/hoh/install.sh"
[[ ! -d web/dist ]] || cp -R web/dist "$stage/hoh/web"
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C "$stage" -cf - hoh | gzip -n >"$out"
