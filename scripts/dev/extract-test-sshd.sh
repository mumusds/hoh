#!/usr/bin/env bash
# Download the distribution openssh-server package and EXTRACT it for unprivileged loopback SSH tests. Never
# installs it: dpkg-deb -x runs no maintainer scripts, so no system sshd_config, unit or socket is created,
# enabled or started (CR4R-03). Run as the development user; needs apt lists, not root.
#   extract-test-sshd.sh <tools-dir>     prints only the extracted directory on stdout (diagnostics on stderr)
# Version: apt candidate of openssh-server, or HOH_TEST_SSHD_VERSION. OpenSSH >= 9.8 ships sshd-session and
# >= 10.0 also sshd-auth; older packages (Ubuntu 24.04: 9.6p1) have only sshd, so helpers are optional (CR5R-02).
# A failed or interrupted run leaves no download or partial extraction behind; temp dirs left by a run killed
# with SIGKILL are removed by the next run, cached or not (CR5R-03, CR6R-03). Not meant to run concurrently.
set -Eeuo pipefail
tools="${1:?usage: extract-test-sshd.sh <tools-dir>}"
version="${HOH_TEST_SSHD_VERSION:-$(apt-cache policy openssh-server | awk '$1=="Candidate:"{print $2}')}"
[[ -n "$version" && "$version" != "(none)" ]] || { echo 'openssh-server has no apt candidate' >&2; exit 69; }
base="$tools/openssh-server"
dest="$base/${version//:/_}"
dl="" partial=""
cleanup() { [[ -z "$dl" ]] || rm -rf -- "$dl"; [[ -z "$partial" ]] || rm -rf -- "$partial"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

usable() {  # usable <dir>: sshd present and no extracted program misses a shared library
  [[ -x "$1/usr/sbin/sshd" ]] || { echo "extracted openssh-server lacks usr/sbin/sshd" >&2; return 69; }
  local b
  for b in usr/sbin/sshd usr/lib/openssh/sshd-session usr/lib/openssh/sshd-auth; do
    [[ -e "$1/$b" ]] || continue
    [[ -x "$1/$b" ]] || { echo "$b is not executable" >&2; return 69; }
    if ldd "$1/$b" 2>/dev/null | grep -q 'not found'; then
      echo "$b: missing shared libraries:" >&2; ldd "$1/$b" | grep 'not found' >&2; return 69
    fi
  done
}

mkdir -p "$base"
# Leftovers of an earlier run killed before its trap ran (ours; this script is not run concurrently).
rm -rf -- "$base"/.download.* "$base"/.extract.*
if [[ -d "$dest" ]] && usable "$dest" 2>/dev/null; then printf '%s\n' "$dest"; exit 0; fi
rm -rf -- "$dest"  # incomplete earlier result
dl="$(mktemp -d "$base/.download.XXXXXX")"
partial="$(mktemp -d "$base/.extract.XXXXXX")"
(cd "$dl" && apt-get download -q "openssh-server=$version") >&2  # apt prints Get:/Fetched on stdout (CR6R-02)
debs=("$dl"/openssh-server_*.deb)
[[ -f "${debs[0]}" ]] || { echo "apt-get download produced no openssh-server package" >&2; exit 69; }
dpkg-deb -x "${debs[0]}" "$partial" >&2
usable "$partial" || exit 69
mv -T -- "$partial" "$dest"
partial=""
printf '%s\n' "$dest"
