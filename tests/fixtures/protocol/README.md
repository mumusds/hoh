# Native protocol fixtures (M0-T04)

Recorded on the development VM on 2026-11-03 against the CLI versions in `compat-manifest.json`.
They are replay inputs for adapter tests (REQ-024/025/026/027). Never edit them by hand; re-record
when a CLI version changes and keep the old directory until the compat suite has been rerun.

Each case directory holds `meta.json` (exact argv, exit code, timestamp), raw `stdout.jsonl`,
`stderr.log` and, when a protocol is driven over stdin, `stdin.jsonl`. Claude was run with the
parent Claude Code session variables removed from the environment (see `tools/agent_task.py`).

| Agent | Case | Status | Observation that adapters must honor |
|---|---|---|---|
| claude 2.1.317 | `success` | live | `system:init` carries `session_id` and `model`; a `rate_limit_event` frame appears before `result` and must be kept as an unknown/auxiliary event |
| claude 2.1.317 | `resume` | live | `--resume <session_id>` continues the same session id |
| claude 2.1.317 | `max-turns-error` | live | `result` subtype `error_max_turns`, exit code 1 |
| claude 2.1.317 | `sigint-cancel` | live | SIGINT during a tool run ends with **exit code 0 and no `result` frame**; success must never be inferred from the exit code |
| pi 1.5.0 | `rpc-no-provider` | live (no provider) | `get_state` works without a model; `prompt` returns `success:false` "No API key found" → REJECTED / BLOCKED_AUTH, no `agent_settled` to wait for |
| codex 0.166.0 | `app-server-schema/` | offline | App Server JSON Schema from `codex app-server generate-json-schema`; live samples NOT_RUN because Codex is not logged in (BLOCKED_AUTH) |

Missing live samples (Codex exec/App Server turns, Pi turns with a provider) are external blockers,
not gaps to be filled with invented frames.
