#!/usr/bin/env bash
# Optional bootstrap entry; not a HoH product command and not an outer agent loop.
# Default (CR-001): the top-level session is the main developer (Claude); independent
# review runs in a separate session through tools/agent_task.py --role reviewer.
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
[[ ! -f "$ROOT/.hoh-dev/env.sh" ]] || source "$ROOT/.hoh-dev/env.sh"
AGENT=claude; HEADLESS=0; DRY=0
for arg in "$@"; do
 case "$arg" in codex|claude|pi) AGENT="$arg";; --headless) HEADLESS=1;; --dry-run) DRY=1;; -h|--help) echo 'start.sh [claude|codex|pi] [--headless] [--dry-run]'; exit 0;; *) echo "Unknown option: $arg" >&2; exit 64;; esac
done
cd "$ROOT"
if [[ -n "${HOH_BOOTSTRAP_PARENT:-}" ]]; then
 echo "Refusing: already inside bootstrap session ${HOH_BOOTSTRAP_PARENT}; do not start a second top-level session." >&2; exit 78
fi
ROLE=developer; [[ "$AGENT" != codex ]] || ROLE=commander
if [[ "$HEADLESS" == 1 ]]; then
 CMD=(python3 tools/agent_task.py --agent "$AGENT" --role "$ROLE" --top-level --prompt-file LAUNCH_PROMPT.md --timeout "${HOH_AGENT_TIMEOUT_SECONDS:-86400}")
 [[ "$DRY" == 0 ]] || CMD+=(--dry-run)
 exec "${CMD[@]}"
fi
case "$AGENT" in
 codex) CMD=(codex --dangerously-bypass-approvals-and-sandbox);;
 claude) CMD=(claude --dangerously-skip-permissions);;
 pi) CMD=(pi);;
esac
if [[ "$DRY" == 1 ]]; then printf 'DRY RUN: '; printf '%q ' "${CMD[@]}" '[HOH_TOP_LEVEL_BOOTSTRAP] <LAUNCH_PROMPT.md content>'; printf '\n'; exit 0; fi
command -v "$AGENT" >/dev/null || { echo "Run bootstrap first: missing $AGENT" >&2; exit 127; }
if [[ "$(id -u)" == 0 ]]; then echo 'Use a non-root VM development user; native agents run with bypass flags.' >&2; exit 78; fi
has_flag() { grep -Eq -- "(^|[^[:alnum:]_-])$1([^[:alnum:]_-]|\$)" <<<"$HELP"; }
HELP="$($AGENT --help 2>&1)" || { echo "$AGENT --help failed; cannot verify required flags" >&2; exit 78; }
if [[ "$AGENT" == pi ]]; then
 if has_flag --approve; then CMD+=(--approve); fi
else
 FLAG="${CMD[1]}"; has_flag "$FLAG" || { echo "Installed $AGENT does not support $FLAG" >&2; exit 78; }
fi
HOH_BOOTSTRAP_PARENT="interactive-$(date -u +%Y%m%dT%H%M%SZ)-$$" exec "${CMD[@]}" "$(printf '[HOH_TOP_LEVEL_BOOTSTRAP]\n%s' "$(cat LAUNCH_PROMPT.md)")"
