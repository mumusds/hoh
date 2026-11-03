#!/usr/bin/env bash
# Development environment only; this is NOT the HoH product runtime.
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
DRY=0; INSTALL=1; PLUGINS=1; YOLO=0
for arg in "$@"; do
  case "$arg" in
    --yolo) YOLO=1;;
    --dry-run) DRY=1;;
    --skip-install) INSTALL=0;;
    --skip-plugins) PLUGINS=0;;
    -h|--help) printf '%s\n' 'bootstrap.sh --yolo [--dry-run] [--skip-install] [--skip-plugins]' 'Run in a dedicated Linux VM as a non-root development user. Dry run writes nothing.'; exit 0;;
    *) printf 'Unknown option: %s\n' "$arg" >&2; exit 64;;
  esac
done
[[ "$YOLO" == 1 ]] || { echo 'Pass --yolo to acknowledge the dedicated-VM unattended development mode.' >&2; exit 64; }
command -v python3 >/dev/null || { echo 'Python 3.11+ is needed to bootstrap. Install python3 first.' >&2; exit 78; }
python3 -c 'import sys; assert sys.version_info >= (3,11), "Python 3.11+ required"'
if [[ "$DRY" == 1 ]]; then
  printf 'DRY RUN (no writes/downloads)\nProject: %s\n' "$ROOT"
  printf 'Install missing dependencies: %s\nPi plugin installation: %s\n' "$INSTALL" "$PLUGINS"
  printf '%s\n' 'Keep existing agents, authentication, and model defaults.' 'Resolve native CLI help/versions; record locks; do not upgrade active runtimes.' 'Prepare isolated development venv and project-local skill entries.' 'No background agent loop and no claim of product acceptance.'
  exit 0
fi
if [[ "$(id -u)" == 0 ]]; then
  echo 'Use a normal development user in the VM, not root: native agents run with bypass flags and Claude rejects root.' >&2
  exit 78
fi
DEV="$ROOT/.hoh-dev"; mkdir -p "$DEV" "$DEV/downloads" "$DEV/runtime" "$DEV/tooling" "$DEV/attempts"
export PATH="$DEV/tooling/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
if [[ -d "$DEV/runtime/node/bin" ]]; then export PATH="$DEV/runtime/node/bin:$PATH"; fi
# env.sh is regenerated only from paths; never write credentials here.
python3 - "$ROOT" <<'ENV'
import pathlib,sys,shlex
r=pathlib.Path(sys.argv[1]);d=r/'.hoh-dev';paths=[d/'venv/bin',d/'runtime/node/bin',d/'tooling/bin',pathlib.Path.home()/'.local/bin',pathlib.Path.home()/'.cargo/bin']
env=shlex.quote(str(r/'.env'))
(d/'env.sh').write_text('# Source this in each new development shell. No secrets are written here.\nexport HOH_PROJECT_ROOT='+shlex.quote(str(r))+'\nexport PATH='+shlex.quote(':'.join(map(str,paths)))+':"$PATH"\n'
  '# Optional user overrides (see .env.example); .env is gitignored.\nif [ -f '+env+' ]; then set -a; . '+env+'; set +a; fi\n',encoding='utf-8')
ENV
if [[ "$INSTALL" == 1 ]]; then
  [[ "$(uname -s)" == Linux ]] || { echo 'Automatic installation currently supports Linux only.' >&2; exit 78; }
  MISSING=()
  for tool in curl git cc make pkg-config tar xz; do command -v "$tool" >/dev/null || MISSING+=("$tool"); done
  python3 -c 'import venv,ensurepip' >/dev/null 2>&1 || MISSING+=(python3-venv)
  if ((${#MISSING[@]})); then
    command -v apt-get >/dev/null || { echo "Missing ${MISSING[*]}; auto-install requires apt-get (Ubuntu/Debian)." >&2; exit 78; }
    sudo -n true || { echo 'Missing system packages require working sudo -n in this dedicated VM.' >&2; exit 78; }
    sudo -n apt-get update
    sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y build-essential pkg-config libssl-dev ca-certificates curl git python3-venv xz-utils unzip jq
  fi
  fetch() { curl --fail --location --retry 3 --connect-timeout 20 --max-time 180 "$1" --output "$2"; }
  NODE_OK=0
  if command -v node >/dev/null; then
    if node -e 'const [a,b]=process.versions.node.split(".").map(Number);process.exit((a>22||(a===22&&b>=19))?0:1)' ; then NODE_OK=1; fi
  fi
  if [[ "$NODE_OK" == 0 ]]; then
    fetch https://nodejs.org/dist/index.json "$DEV/downloads/node-index.json"
    NODE_VERSION="$(python3 - "$DEV/downloads/node-index.json" <<'NODE'
import json,sys
items=json.load(open(sys.argv[1]));valid=[i for i in items if i.get('lts') and int(i['version'][1:].split('.')[0])>=24]
if not valid:raise SystemExit('No supported Node LTS resolved; review official releases.')
print(max(valid,key=lambda i:tuple(map(int,i['version'][1:].split('.'))))['version'])
NODE
)"
    case "$(uname -m)" in x86_64) ARCH=x64;; aarch64) ARCH=arm64;; *) echo 'Unsupported Node architecture.' >&2; exit 78;; esac
    TAR="node-${NODE_VERSION}-linux-${ARCH}.tar.xz"
    fetch "https://nodejs.org/dist/$NODE_VERSION/$TAR" "$DEV/downloads/$TAR"
    fetch "https://nodejs.org/dist/$NODE_VERSION/SHASUMS256.txt" "$DEV/downloads/node-SHASUMS256.txt"
    python3 - "$DEV/downloads/$TAR" "$DEV/downloads/node-SHASUMS256.txt" <<'VERIFY'
import hashlib,pathlib,sys
p=pathlib.Path(sys.argv[1]);rows=[l.split() for l in pathlib.Path(sys.argv[2]).read_text().splitlines()]
expected=[r[0] for r in rows if len(r)==2 and r[1]==p.name]
if len(expected)!=1 or hashlib.sha256(p.read_bytes()).hexdigest()!=expected[0]:raise SystemExit('Node checksum verification failed')
VERIFY
    mkdir -p "$DEV/runtime/node.new"
    tar -xJf "$DEV/downloads/$TAR" --strip-components=1 -C "$DEV/runtime/node.new"
    [[ ! -d "$DEV/runtime/node" ]] || mv "$DEV/runtime/node" "$DEV/runtime/node.old.$(date +%s)"
    mv "$DEV/runtime/node.new" "$DEV/runtime/node"
    export PATH="$DEV/runtime/node/bin:$PATH"
  fi
  if ! command -v rustup >/dev/null; then
    fetch https://sh.rustup.rs "$DEV/downloads/rustup-init.sh"
    # PATH comes from .hoh-dev/env.sh; do not edit the user's shell profile.
    sh "$DEV/downloads/rustup-init.sh" -y --no-modify-path --profile minimal --default-toolchain stable
  fi
  if [[ ! -f "$DEV/rust-version.lock" ]]; then
    rustup toolchain install stable --profile minimal --component rustfmt --component clippy
    rustc +stable --version | awk '{print $2}' > "$DEV/rust-version.lock"
  fi
  RUST_VERSION="$(cat "$DEV/rust-version.lock")"
  rustup toolchain install "$RUST_VERSION" --profile minimal --component rustfmt --component clippy
  # Do not overwrite an implementation team's existing pinned toolchain.
  if [[ ! -f "$ROOT/rust-toolchain.toml" ]]; then
    printf '[toolchain]\nchannel = "%s"\nprofile = "minimal"\ncomponents = ["rustfmt", "clippy"]\n' "$RUST_VERSION" > "$ROOT/rust-toolchain.toml"
  fi
  python3 -m venv "$DEV/venv"
  "$DEV/venv/bin/python" -m pip install -r "$ROOT/requirements-dev.txt"
  "$DEV/venv/bin/python" -m pip freeze > "$DEV/python-lock.txt"
  # Pin installations, preserve already available native CLIs.
  install_npm_cli() {
    local bin="$1" package="$2" name="$3" version
    if command -v "$bin" >/dev/null; then printf 'Keeping existing %s: %s\n' "$bin" "$(command -v "$bin")"; return; fi
    if [[ -f "$DEV/$name-version.lock" ]]; then version="$(cat "$DEV/$name-version.lock")"; else
      version="$(npm view "$package" version)"; printf '%s\n' "$version" > "$DEV/$name-version.lock"
    fi
    npm install --global --prefix "$DEV/tooling" "$package@$version"
  }
  install_npm_cli codex '@openai/codex' codex
  if ! command -v claude >/dev/null; then
    fetch https://claude.ai/install.sh "$DEV/downloads/claude-install.sh"
    bash "$DEV/downloads/claude-install.sh" stable
  fi
  PI_PACKAGE="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["native_pi_package"])' "$ROOT/config/plugins.json")"
  install_npm_cli pi "$PI_PACKAGE" pi
  if [[ "$PLUGINS" == 1 ]]; then
    # Plugins are degradable (CR-001): record DEGRADED in .hoh-dev/plugin-lock.json and keep going.
    python3 "$ROOT/tools/install_pi_plugins.py" || echo 'Pi plugins DEGRADED; see .hoh-dev/plugin-lock.json. Core development continues; release reports must show DEGRADED.' >&2
  fi
else
  printf '%s\n' 'Offline preparation only: no toolchain, package, native agent or plugin installation attempted.'
fi
python3 - "$ROOT" <<'STATE'
import json,pathlib,sys,datetime
r=pathlib.Path(sys.argv[1]);d=r/'.hoh-dev'
if not (d/'PROGRESS.md').exists():
 (d/'PROGRESS.md').write_text('# HoH development checkpoint\n\nStage: M0\nStatus: NOT_IMPLEMENTED\nNext: run bundle checks, inspect preflight, follow contracts/development-tasks.json.\n\nNo live integration or product acceptance has been claimed.\n',encoding='utf-8')
(d/'bootstrap-state.json').write_text(json.dumps({'prepared_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'root':str(r),'state':'ENVIRONMENT_PREPARED_NOT_ACCEPTED'},indent=2)+'\n')
STATE
source "$DEV/env.sh"
python3 "$ROOT/tools/preflight.py" --output "$DEV/preflight.json" || {
  echo 'Environment preparation completed with missing capabilities. Inspect .hoh-dev/preflight.json; live tests remain blocked.' >&2
  exit 3
}
printf '%s\n' 'Environment and native help checks passed; authentication and live behavior are NOT proven.' 'Next: source .hoh-dev/env.sh; bash scripts/check.sh --bundle; follow LAUNCH_PROMPT.md.'
