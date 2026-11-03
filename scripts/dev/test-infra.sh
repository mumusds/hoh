#!/usr/bin/env bash
# Development/test infrastructure for HoH checks (CR-003 D3). Idempotent; run as the non-root development
# user with working `sudo -n` in the dedicated VM. Not part of the HoH product.
#   strace          AC-006-02 (doctor never opens credential files)
#   sqlite3         external connection in AC-062-01, manual DB inspection
#   test sshd       REQ-015/REQ-061 SSH and port-forward tests. scripts/dev/extract-test-sshd.sh downloads the
#                   distribution openssh-server package and EXTRACTS it into .hoh-dev/tools/openssh-server/<version>/,
#                   never installs it: a package install creates /etc/ssh/sshd_config and enables/starts ssh.socket
#                   on a fresh VM (CR3R-03, CR4R-03). Only its library dependencies are installed. Each test starts
#                   its own unprivileged sshd with a temp config, host key, 127.0.0.1 and a free port, adding
#                   SshdSessionPath/SshdAuthPath only for the helpers the package ships (docs/22, CR5R-02).
#   qemu + kvm      fresh-VM deployment checks (REQ-057/066); user added to the kvm group
#   cloud-image-utils  cloud-init seed images for those VMs
#   Playwright deps system libraries for Chromium; browsers are installed by web/ (M6) from its own lockfile
#   cargo-deny      license/source/ban policy in the static check (deny.toml)
#   user linger     systemd --user services survive logout (AC-005-04, soak)
# Writes .hoh-dev/test-infra.json with the resolved versions.
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
CARGO_DENY_VERSION=0.20.2      # 2026-07-09, MSRV 1.88
PLAYWRIGHT_VERSION=1.64.1      # 2026-10-14; only its install-deps step is used here
DRY=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY=1;;
    -h|--help) echo 'test-infra.sh [--dry-run]'; exit 0;;
    *) echo "Unknown option: $arg" >&2; exit 64;;
  esac
done
APT=(strace sqlite3 qemu-system-x86 qemu-utils cloud-image-utils shellcheck jq)
# Library dependencies of the test sshd only (no SSH package, no service units).
mapfile -t SSH_LIBS < <(apt-cache depends --no-recommends --no-suggests openssh-server | awk '$1=="Depends:" && $2 ~ /^lib/ {print $2}' | sort -u)
APT+=("${SSH_LIBS[@]}")
if [[ "$DRY" == 1 ]]; then
  printf 'DRY RUN: would install apt packages: %s\n' "${APT[*]}"
  printf 'DRY RUN: extract openssh-server into .hoh-dev/tools (not installed; only a marked legacy hoh drop-in is removed, no ssh unit is touched); add %s to kvm, enable linger, playwright@%s install-deps chromium, cargo-deny@%s\n' "$USER" "$PLAYWRIGHT_VERSION" "$CARGO_DENY_VERSION"
  exit 0
fi
[[ "$(id -u)" != 0 ]] || { echo 'Run as the non-root development user.' >&2; exit 78; }
sudo -n true 2>/dev/null || { echo 'Needs working sudo -n in the dedicated VM.' >&2; exit 78; }
[[ -f "$ROOT/.hoh-dev/env.sh" ]] && source "$ROOT/.hoh-dev/env.sh"

missing=()
for p in "${APT[@]}"; do dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'install ok installed' || missing+=("$p"); done
if ((${#missing[@]})); then
  sudo -n apt-get update -q
  sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y -q "${missing[@]}"
fi

# An earlier revision of this script wrote a system sshd drop-in; remove only that file (identified by its
# marker line) and never restart, enable or unmask the system ssh service.
legacy=/etc/ssh/sshd_config.d/50-hoh-test-loopback.conf
if sudo -n test -f "$legacy" && sudo -n grep -q '^# Managed by hoh scripts/dev/test-infra.sh' "$legacy"; then
  sudo -n rm -f -- "$legacy"
fi
SSH_VERSION="${HOH_TEST_SSHD_VERSION:-$(apt-cache policy openssh-server | awk '$1=="Candidate:"{print $2}')}"
SSH_DIR="$(HOH_TEST_SSHD_VERSION="$SSH_VERSION" bash "$ROOT/scripts/dev/extract-test-sshd.sh" "$ROOT/.hoh-dev/tools")"
[[ -x "$SSH_DIR/usr/sbin/sshd" ]] || { printf 'test sshd not found at %q\n' "$SSH_DIR" >&2; exit 69; }

getent group kvm | grep -qw "$USER" || sudo -n usermod -aG kvm "$USER"
[[ "$(loginctl show-user "$USER" -p Linger --value 2>/dev/null)" == yes ]] || sudo -n loginctl enable-linger "$USER"

npx --yes "playwright@${PLAYWRIGHT_VERSION}" install-deps chromium

if ! cargo deny --version 2>/dev/null | grep -q "$CARGO_DENY_VERSION"; then
  cargo install --locked "cargo-deny@${CARGO_DENY_VERSION}"
fi

python3 - "$ROOT/.hoh-dev/test-infra.json" "$PLAYWRIGHT_VERSION" "$SSH_VERSION" "$SSH_DIR" <<'PY'
import json,subprocess,sys,datetime,hashlib,pathlib
def out(cmd):
    try:return subprocess.run(cmd,capture_output=True,text=True,timeout=20).stdout.strip().splitlines()[0]
    except Exception:return 'unknown'
pkgs={p:out(['dpkg-query','-W','-f=${Version}',p]) for p in ['strace','sqlite3','qemu-system-x86','qemu-utils','cloud-image-utils','shellcheck','jq']}
ssh=pathlib.Path(sys.argv[4]);bins={'sshd':'usr/sbin/sshd','sshd_session':'usr/lib/openssh/sshd-session','sshd_auth':'usr/lib/openssh/sshd-auth'}
# sshd-session (OpenSSH >= 9.8) and sshd-auth (>= 10.0) are absent from older packages: recorded as null.
entry=lambda f:{'path':str(f),'sha256':hashlib.sha256(f.read_bytes()).hexdigest()} if f.is_file() else None
test_sshd={'package':'openssh-server','version':sys.argv[3],'installed_as_system_package':False,'dir':str(ssh),
           **{k:entry(ssh/v) for k,v in bins.items()}}
if test_sshd['sshd'] is None:sys.exit(f'test sshd missing under {ssh}')
data={'schema_version':'1.1','at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'apt':pkgs,'test_sshd':test_sshd,
      'cargo_deny':out(['cargo','deny','--version']),'playwright_install_deps':sys.argv[2],
      'kvm_group_configured':'kvm' in out(['id','-nG',subprocess.run(['id','-un'],capture_output=True,text=True).stdout.strip()]).split(),
      'linger':out(['loginctl','show-user',subprocess.run(['id','-un'],capture_output=True,text=True).stdout.strip(),'-p','Linger','--value']),
      'note':'kvm group membership applies to new login sessions; until then qemu uses sg kvm or falls back to TCG.'}
json.dump(data,open(sys.argv[1],'w'),indent=2);print(json.dumps(data,indent=2))
PY
