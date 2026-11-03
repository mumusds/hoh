# Native protocol fixtures (M0-T04)

Replay inputs for adapter tests (REQ-024/025/026/027), recorded on the development VM against the
versions and binaries pinned in `compat-manifest.json`. Never edit them by hand. Re-record with

    python3 tests/fixtures/protocol/record.py [--agents claude,codex,pi]

which runs every case in a fresh temporary Git repository outside this checkout, strips the parent
agent-session variables (`tools/agent_task.py` `PARENT_SESSION_VARS`), stores the exact stdin, raw
stdout/stderr and `meta.json`, refuses cases that do not show what they claim (`valid`), and rewrites
the manifest. Keep the previous version directory until the compat suite has been rerun.

Provenance limits: the operator's user-level agent configuration (plugins, hooks, account, rate-limit
state) applies to live runs and appears in the raw frames (local paths, plugin inventory, rate-limit
and overage metadata). No credentials are present. Fixtures stay byte-exact because adapter tests
compare raw bytes; the repository is private.

| Agent | Case | Status | What adapters must honor |
|---|---|---|---|
| claude 2.1.317 | `success` | live | `system:init` carries `session_id` and `model`; an undocumented `rate_limit_event` frame precedes `result` and must be kept as an unknown/auxiliary event |
| claude 2.1.317 | `resume` | live | `--resume <session_id>` continues the same session id |
| claude 2.1.317 | `max-turns-error` | live | `result` subtype `error_max_turns`, exit code 1 |
| claude 2.1.317 | `sigint-cancel` | live | SIGINT while a foreground tool runs yields `result` subtype `error_during_execution`, `is_error: true`, **exit code 0** — success must come from the result frame, never from the exit code |
| pi 1.5.0 | `rpc-prompt` | live | `prompt` response `disposition: "started"`; `agent_start` … `agent_end` → `agent_settled` |
| pi 1.5.0 | `rpc-resume` | live | a new process with the same `--session-dir` and `--session-id` reports the same `sessionId` and remembers the previous turn |
| pi 1.5.0 | `rpc-abort` | live | `abort` during streaming ends the assistant message with `stopReason: "aborted"` and still emits `agent_settled` |
| pi 1.5.0 | `rpc-no-provider` | live, reproducible | empty `PI_CODING_AGENT_DIR`, no key variables: `get_state` succeeds, `prompt` returns `success:false` "No API key found" → REJECTED / BLOCKED_AUTH, nothing to wait for |
| pi 1.5.0 | `rpc-invalid-model` | live | `set_model` with an unknown provider/model returns `success:false` "Model not found" |
| codex 0.166.0 | `exec-unauthenticated` | live, no login | `thread.started`, `turn.started`, repeated `error` frames and an `item.completed` error item, terminal `turn.failed`, exit code 1 |
| codex 0.166.0 | `exec-turn-failed` | live, logged in | the configured model is not available to the account: `item.completed` error item, `turn.started`, `error`, terminal `turn.failed`, exit code 1 (MODEL_UNAVAILABLE surface) |
| codex 0.166.0 | `exec` | live, logged in | `thread.started`, `turn.started`, `item.completed`, `turn.completed`, exit code 0 |
| codex 0.166.0 | `app-server-handshake` | live, logged in | `initialize` and `thread/start` results, then `thread/started`, `mcpServer/startupStatus/updated`, `account/updated` notifications |
| codex 0.166.0 | `app-server-turn` | live, logged in | `turn/start` result, `turn/started`, `item/started`, `item/agentMessage/delta`, `item/completed`, `turn/completed` with `turn.status: "completed"`; auxiliary `thread/tokenUsage/updated`, `account/rateLimits/updated` |
| codex 0.166.0 | `app-server-resume` | live, logged in | a new process `thread/resume`s the thread and the next turn recalls the previous token |
| codex 0.166.0 | `app-server-interrupt` | live, logged in | `turn/interrupt` during a command execution item returns a result, then `turn/completed` with `turn.status: "interrupted"` |
| codex 0.166.0 | `app-server-steer` | live, logged in | `turn/steer` with a stale `expectedTurnId` → JSON-RPC error `-32600` "expected active turn id … but found …"; with the active turn id → result, and the turn completes |
| codex 0.166.0 | `app-server-handshake-unauthenticated` | live, no login | `initialize` and `thread/start` still succeed (`thread/started`); stderr already logs a Responses WebSocket connection error |
| codex 0.166.0 | `app-server-schema/` | offline | JSON Schema from `codex app-server generate-json-schema` (tree hash in the manifest) |

`exec-turn-failed` was recorded while the operator's Codex config named a model with the wrong case
(`gpt-6-Astra` instead of `gpt-6-astra`); it is kept as the real model-unavailable failure surface.
