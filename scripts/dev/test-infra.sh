#!/usr/bin/env bash
# Development/test infrastructure for HoH checks (CR-003 D3). Idempotent; run as the non-root development
# user with working `sudo -n` in the dedicated VM. Not part of the HoH product.
#   strace          AC-006-02 (doctor never opens credential files)
#   sqlite3         external connection in AC-062-01, manual DB inspection
#   openssh-server  REQ-015/REQ-061 SSH and port-forward tests. Tests start their own unprivileged sshd
#                   (temp host key, 127.0.0.1, free port); the system ssh service is left as the owner set it
#                   (masked on this VM). The drop-in below only keeps a system sshd loopback-only if ever enabled.
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
APT=(strace sqlite3 openssh-server qemu-system-x86 qemu-utils cloud-image-utils shellcheck jq)
if [[ "$DRY" == 1 ]]; then
  printf 'DRY RUN: would install apt packages: %s\n' "${APT[*]}"
  printf 'DRY RUN: sshd ListenAddress 127.0.0.1, add %s to kvm, enable linger, playwright@%s install-deps chromium, cargo-deny@%s\n' "$USER" "$PLAYWRIGHT_VERSION" "$CARGO_DENY_VERSION"
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

# SSH for loopback tests only: never listen on external interfaces, keys only.
conf=/etc/ssh/sshd_config.d/50-hoh-test-loopback.conf
want=$'# Managed by hoh scripts/dev/test-infra.sh: loopback-only sshd for SSH/port-forward tests.\nListenAddress 127.0.0.1\nListenAddress ::1\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n'
if [[ "$(sudo -n cat "$conf" 2>/dev/null || true)" != "${want%$'\n'}" ]]; then
  printf '%s' "$want" | sudo -n tee "$conf" >/dev/null
  sudo -n sshd -t
  # Never enable or unmask the system service here; restart it only if the owner already runs it.
  if systemctl is-active --quiet ssh.service; then sudo -n systemctl restart ssh.service; fi
fi

getent group kvm | grep -qw "$USER" || sudo -n usermod -aG kvm "$USER"
[[ "$(loginctl show-user "$USER" -p Linger --value 2>/dev/null)" == yes ]] || sudo -n loginctl enable-linger "$USER"

npx --yes "playwright@${PLAYWRIGHT_VERSION}" install-deps chromium

if ! cargo deny --version 2>/dev/null | grep -q "$CARGO_DENY_VERSION"; then
  cargo install --locked "cargo-deny@${CARGO_DENY_VERSION}"
fi

python3 - "$ROOT/.hoh-dev/test-infra.json" "$PLAYWRIGHT_VERSION" <<'PY'
import json,subprocess,sys,datetime
def out(cmd):
    try:return subprocess.run(cmd,capture_output=True,text=True,timeout=20).stdout.strip().splitlines()[0]
    except Exception:return 'unknown'
pkgs={p:out(['dpkg-query','-W','-f=${Version}',p]) for p in ['strace','sqlite3','openssh-server','qemu-system-x86','qemu-utils','cloud-image-utils','shellcheck','jq']}
data={'schema_version':'1.0','at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'apt':pkgs,
      'cargo_deny':out(['cargo','deny','--version']),'playwright_install_deps':sys.argv[2],
      'kvm_group_configured':'kvm' in out(['id','-nG',subprocess.run(['id','-un'],capture_output=True,text=True).stdout.strip()]).split(),
      'linger':out(['loginctl','show-user',subprocess.run(['id','-un'],capture_output=True,text=True).stdout.strip(),'-p','Linger','--value']),
      'note':'kvm group membership applies to new login sessions; until then qemu uses sg kvm or falls back to TCG.'}
json.dump(data,open(sys.argv[1],'w'),indent=2);print(json.dumps(data,indent=2))
PY
