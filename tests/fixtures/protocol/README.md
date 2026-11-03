# Native protocol fixtures (M0-T04)

Replay inputs for adapter tests (REQ-024/025/026/027), recorded on the development VM against the
versions and binaries pinned in `compat-manifest.json`. Never edit them by hand. Re-record with

    python3 tests/fixtures/protocol/record.py [--agents claude,codex,pi]

which runs every case in a fresh temporary Git repository outside this checkout (removed afterwards),
strips the parent agent-session variables (`tools/agent_task.py` `PARENT_SESSION_VARS`) and stores the
exact stdin, raw stdout/stderr and `meta.json` (`valid`, `problems`, and `stopped`: how the process
ended). Each case has its own check of the outcome it claims (`VALIDATORS` in `record.py`; for example
only agent-written messages count, a steer must complete, a Pi turn must not end in error). An agent
version is recorded into a staging directory and replaces `<agent>/<version>/` in one rename only when
every case is valid and the binary did not change meanwhile; otherwise the published fixtures stay as
they were, the staging directory moves to `.hoh-dev/protocol-failed/` and the recorder exits 1 (also
when an agent is not logged in). `provenance.json` holds the binary and help hashes and the auth state
measured while recording; the manifest is rebuilt from those files and never re-measures binaries.
Children are stopped with bounded escalation (close stdin, SIGTERM, SIGKILL of the process group). Each
child's environment carries a token (`HOH_RECORDER_PROC`), so its descendants are found wherever they
went (other session or group, re-parented, stdio closed): those that end on their own within 5 s after
the CLI exited are recorded in `meta.json` `descendants_lingered`; those still running are killed and
the case is invalid, as is one whose process needed SIGKILL. A descendant that exec'd with a cleared
environment cannot be found this way. SIGINT/SIGTERM stop every child before the temporary directories
are removed, and the manifest is still rebuilt. Publishing swaps the directories atomically (`renameat2`).
Keep the previous version directory until the compat suite has been rerun.

Provenance limits: the operator's user-level agent configuration (plugins, hooks, account, rate-limit
state) applies to live runs and appears in the raw frames (local paths, plugin inventory, rate-limit
and overage metadata). No credentials are present. Fixtures stay byte-exact because adapter tests
compare raw bytes; the repository is private.

| Agent | Case | Status | What adapters must honor |
|---|---|---|---|
| claude 2.1.317 | `success` | live | `system:init` carries `session_id` and `model`; an undocumented `rate_limit_event` frame precedes `result` and must be kept as an unknown/auxiliary event |
| claude 2.1.317 | `resume` | live | `--resume <session_id>` continues the same session id |
| claude 2.1.317 | `max-turns-error` | live | `result` subtype `error_max_turns`, exit code 1 |
| claude 2.1.317 | `sigint-cancel` | live | SIGINT while a foreground tool runs yields `result` subtype `error_during_execution`, `is_error: true`, **exit code 0** — success must come from the result frame, never from the exit code. The CLI may exit before the cancelled tool's last child ends (`sleep 2` outlived it in 2 of 5 runs during M0-T06), so the adapter must reap the attempt's whole process tree |
| pi 1.5.0 | `rpc-prompt` | live | `prompt` response `disposition: "started"`; `agent_start` … `agent_end` → `agent_settled` |
| pi 1.5.0 | `rpc-resume` | live | a new process with the same `--session-dir` and `--session-id` reports the same `sessionId` and remembers the previous turn |
| pi 1.5.0 | `rpc-abort` | live | `abort` during streaming ends the assistant message with `stopReason: "aborted"` and still emits `agent_settled` |
| pi 1.5.0 | `rpc-no-provider` | live, reproducible | empty `PI_CODING_AGENT_DIR`, no key variables: `get_state` succeeds, `prompt` returns `success:false` "No API key found" → REJECTED / BLOCKED_AUTH, nothing to wait for |
| pi 1.5.0 | `rpc-invalid-model` | live | `set_model` with an unknown provider/model returns `success:false` "Model not found" |
| codex 0.166.0 | `exec-unauthenticated` | live, empty `CODEX_HOME` | `thread.started`, `turn.started`, repeated `error` frames and an `item.completed` error item, terminal `turn.failed`, exit code 1 |
| codex 0.166.0 | `exec-turn-failed` | live, logged in, `--model hoh-invalid-model` | the model is not available to the account: `item.completed` error item, `turn.started`, `error`, terminal `turn.failed`, exit code 1 (MODEL_UNAVAILABLE surface) |
| codex 0.166.0 | `exec` | live, logged in | `thread.started`, `turn.started`, `item.completed`, `turn.completed`, exit code 0 |
| codex 0.166.0 | `app-server-handshake` | live, logged in | `initialize` and `thread/start` results, then `thread/started`, `mcpServer/startupStatus/updated`, `account/updated` notifications |
| codex 0.166.0 | `app-server-turn` | live, logged in | `turn/start` result, `turn/started`, `item/started`, `item/agentMessage/delta`, `item/completed`, `turn/completed` with `turn.status: "completed"`; auxiliary `thread/tokenUsage/updated`, `account/rateLimits/updated` |
| codex 0.166.0 | `app-server-resume` | live, logged in | a new process `thread/resume`s the thread and the next turn recalls the previous token |
| codex 0.166.0 | `app-server-interrupt` | live, logged in | `turn/interrupt` during a command execution item returns a result, then `turn/completed` with `turn.status: "interrupted"` |
| codex 0.166.0 | `app-server-steer` | live, logged in | `turn/steer` with a stale `expectedTurnId` → JSON-RPC error `-32600` "expected active turn id … but found …"; with the active turn id → result, and the turn completes |
| codex 0.166.0 | `app-server-handshake-unauthenticated` | live, empty `CODEX_HOME` | `initialize` and `thread/start` still succeed (`thread/started`); stderr already logs a Responses WebSocket connection error |
| codex 0.166.0 | `app-server-schema/` | offline | JSON Schema from `codex app-server generate-json-schema` (tree hash in the manifest) |

Since M0-T06 every case is reproducible on each run: the unauthenticated surfaces use an empty
`CODEX_HOME` and the model-unavailable surface an invalid `--model`, so a re-recording never mixes cases
from different login states.
