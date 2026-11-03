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
| codex 0.166.0 | `exec-unauthenticated` | live, no login | `thread.started`, `turn.started`, repeated `error` frames, exit code 1 |
| codex 0.166.0 | `app-server-handshake-unauthenticated` | live, no login | `initialize` and `thread/start` succeed without login (`thread/started` notification); failures surface only once a turn runs |
| codex 0.166.0 | `app-server-schema/` | offline | JSON Schema from `codex app-server generate-json-schema` (tree hash in the manifest) |

Codex authenticated turns (exec and App Server `turn/start`, steer, interrupt) are NOT_RUN until
`codex login` is done (task M0-T05). They are an external blocker, not a gap to fill with invented frames.
