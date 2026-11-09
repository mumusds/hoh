# shellcheck shell=bash
# Sourced by the product scripts before cargo (ADR-023 decision 8): the commit `hoh --version` names.
# HOH_BUILD_COMMIT is HEAD's full hash, with `-dirty` when a tracked file differs from it; outside a Git checkout it is
# left unset, and the binary says `commit unknown`. cargo rebuilds hoh-app when the value changes (option_env!).
if hoh_head="$(git rev-parse --verify --quiet HEAD 2>/dev/null)"; then
  if git diff --quiet HEAD -- 2>/dev/null; then
    export HOH_BUILD_COMMIT="$hoh_head"
  else
    export HOH_BUILD_COMMIT="$hoh_head-dirty"
  fi
else
  unset HOH_BUILD_COMMIT
fi
unset hoh_head
