#!/usr/bin/env bash
# Build once; stdout is the executable Cargo named, diagnostics stay on stderr.
# The caller sets HOH_BUILD_COMMIT and the workspace cwd before invoking this helper.
set -Eeuo pipefail
if ! executable="$(cargo build --workspace --release --locked --message-format=json-render-diagnostics | python3 -c '
import json, os, sys
try:
    executables = []
    for line in sys.stdin:
        artifact = json.loads(line)
        if artifact.get("reason") != "compiler-artifact":
            continue
        target = artifact["target"]
        if target["name"] == "hoh" and target["kind"] == ["bin"]:
            executables.append(artifact.get("executable"))
    if len(executables) != 1:
        sys.exit(1)
    executable = executables[0]
    if not isinstance(executable, str) or not os.path.isabs(executable) or not os.path.isfile(executable):
        sys.exit(1)
    print(executable)
except (ValueError, KeyError, TypeError, AttributeError, OSError):
    sys.exit(1)
')"; then
  echo 'bundle: release build must succeed and name exactly one existing absolute executable for bin hoh; refusing to pack.' >&2
  exit 1
fi
printf '%s\n' "$executable"
