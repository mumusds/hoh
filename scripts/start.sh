#!/usr/bin/env bash
# Optional bootstrap entry; not a HoH product command and not an outer agent loop.
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
[[ ! -f "$ROOT/.hoh-dev/env.sh" ]] || source "$ROOT/.hoh-dev/env.sh"
AGENT=codex; HEADLESS=0; DRY=0
for arg in "$@"; do
 case "$arg" in codex|claude|pi) AGENT="$arg";; --headless) HEADLESS=1;; --dry-run) DRY=1;; -h|--help) echo 'start.sh [codex|claude|pi] [--headless] [--dry-run]'; exit 0;; *) echo "Unknown option: $arg" >&2; exit 64;; esac
done
cd "$ROOT"
if [[ "$HEADLESS" == 1 ]]; then
 CMD=(python3 tools/agent_task.py --agent "$AGENT" --role commander --top-level --prompt-file LAUNCH_PROMPT.md --timeout "${HOH_AGENT_TIMEOUT_SECONDS:-86400}")
 [[ "$DRY" == 0 ]] || CMD+=(--dry-run)
 exec "${CMD[@]}"
fi
case "$AGENT" in
 codex) CMD=(codex --dangerously-bypass-approvals-and-sandbox);;
 claude) CMD=(claude --dangerously-skip-permissions);;
 pi) CMD=(pi);;
esac
if [[ "$DRY" == 1 ]]; then printf 'DRY RUN: '; printf '%q ' "${CMD[@]}" '<LAUNCH_PROMPT.md content>'; printf '\n'; exit 0; fi
command -v "$AGENT" >/dev/null || { echo "Run bootstrap first: missing $AGENT" >&2; exit 127; }
if [[ "$AGENT" == claude && "$(id -u)" == 0 ]]; then echo 'Use a non-root VM development user.' >&2; exit 78; fi
HELP="$($AGENT --help)"
if [[ "$AGENT" == pi ]]; then
 if [[ "$HELP" == *'--approve'* ]]; then CMD+=(--approve); fi
else
 FLAG="${CMD[1]}"; [[ "$HELP" == *"$FLAG"* ]] || { echo "Installed $AGENT does not support $FLAG" >&2; exit 78; }
fi
exec "${CMD[@]}" "$(cat LAUNCH_PROMPT.md)"
