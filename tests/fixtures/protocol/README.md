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
Every child, including version/help/schema/auth probes, runs in its own transient systemd user scope
(`systemd-run --user --scope`, i.e. its own cgroup v2), so the recorder needs a systemd user manager
(`loginctl enable-linger`) and refuses to run without one (exit 69). The kernel keeps every descendant
in that cgroup whatever it does (fork, setsid, closed stdio, non-dumpable), `cgroup.kill` kills all of
them at once and `cgroup.events` reports when none is left. Children are stopped with bounded
escalation: close stdin, SIGTERM to the process group, then kill the scope. Descendants still in the
scope after the CLI exited get 5 s to end by themselves (`meta.json` `descendants_lingered`); those the
kernel still lists as members when that period ends outlived it (`descendants_outlived`): they are
killed and the case is invalid, as is one whose process needed SIGKILL. The verdict is that single
observation (over the whole scope subtree, child cgroups included); the recorder never claims which
process it killed. A descendant that starts its own systemd unit (e.g. `systemd-run --user`) or moves
itself into a cgroup outside the scope is not tracked; agent CLIs are not known to do either.
Children get default SIGINT/SIGTERM/SIGHUP dispositions even when the recorder runs with SIGINT ignored.
SIGINT/SIGTERM/SIGHUP at any point of a run stop every child before the temporary directories are
removed (directories of a child that could not be confirmed gone are kept), and the manifest is still
rebuilt. Publishing swaps the directories atomically (`renameat2`).
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
| claude 2.1.317 | `sigint-cancel` | live | SIGINT while a foreground tool runs yields `result` subtype `error_during_execution`, `is_error: true`, **exit code 0** — success must come from the result frame, never from the exit code. The CLI may exit before the cancelled tool's last child ends (`sleep 2` outlived it in several M0-T06 runs; `meta.json` `descendants_lingered`), so the adapter must reap the attempt's whole process tree |
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

Claude Code updates itself on the development VM: 2.1.318 replaced 2.1.317 on 2026-11-04 and was recorded
with the same four cases (same frame types after SIGINT and in `success`, identical `--help`); the 2.1.317
directory is kept until the compat suite has been rerun. Re-record after every CLI update.

Since M0-T06 every case is reproducible on each run: the unauthenticated surfaces use an empty
`CODEX_HOME` and the model-unavailable surface an invalid `--model`, so a re-recording never mixes cases
from different login states.

## Provider faults (`_provider-faults/`, M2T07R-256)

What each CLI writes on the stream HoH reads when its provider answers 429, 503 or a usage limit, recorded with

    python3 tests/fixtures/protocol/record_faults.py [--agents claude,codex,pi]

against a fake provider (`FakeProvider`, 127.0.0.1) inside a sealed network namespace: the script re-executes itself
as `sudo -n unshare -n`, brings only `lo` up, returns to the calling user and refuses to start any CLI unless the
namespace has no other interface and no route out. Every run has a fresh HOME (and CODEX_HOME or
PI_CODING_AGENT_DIR) holding only the fake provider's configuration and a dummy key; no model, account or credential
is involved, and no live verification is claimed. Processes, scopes, staging and all-or-nothing publication are
`record.py`'s; each case keeps `stdin.txt`, `stdout.jsonl`, `stderr.log` and `meta.json` (`mode: fake-provider`, the
scenario the provider answered with, and its log of every request: time, path, status, headers sent). The leading `_`
keeps the directory out of `compat-manifest.json` and of the live recorder's cases. Validators: `VALIDATORS` in
`record_faults.py`; bundle test `tests/bundle/test_record_faults.py`.

| Agent | Case | What the stream shows |
|---|---|---|
| claude 2.1.325 | `fault-429-retry-after-long` | API key, 429 `retry-after: 120`: one request, no `system/api_retry`, assistant `error: "rate_limit"` and a failed result; the 120 s appears nowhere |
| claude 2.1.325 | `fault-429-retry-after-short` | 429 `retry-after: 3`, `CLAUDE_CODE_MAX_RETRIES=2`: two `system/api_retry` frames with `retry_delay_ms: 3000` (Claude waits itself), then the same failure with no value |
| claude 2.1.325 | `fault-503-retry-after-long` | 503 `overloaded_error`, `retry-after: 120`: one request, assistant `error: "server_error"`, no value |
| claude 2.1.325 | `fault-usage-limit` | OAuth (subscriber), 429 with a rejected 5-hour `anthropic-ratelimit-unified-*`: `rate_limit_event` `status: "rejected"`, `resetsAt`; assistant `api_error: "usage_limit_reached"` with `api_error_params.rate_limit_info.resetsAt` |
| codex 0.166.0 | `app-server-fault-429-retry-after` | 429 `retry-after: 2`: six requests 2 s apart, five `error` notifications `willRetry: true`, then `turn/completed` failed with `responseTooManyFailedAttempts {httpStatusCode: 429}`; no value |
| codex 0.166.0 | `app-server-fault-503-retry-after` | 503 `server_is_overloaded`, `retry-after: 2`: thirty requests 2 s apart, then `serverOverloaded`; no value |
| codex 0.166.0 | `app-server-fault-usage-limit` | 429 `usage_limit_reached` with `x-codex-*` window headers: `account/rateLimits/updated` (primary `usedPercent: 100`, `resetsAt`), then `usageLimitExceeded` |
| codex 0.166.0 | `exec-fault-usage-limit` | the same through `exec --json`: `turn.failed` with message text only |
| pi 1.5.0 | `rpc-fault-429-retry-after` | 429 `retry-after: 3`: pi's own `auto_retry_start` delays 2/4/8 s, then a text `errorMessage`; nothing structured |
| pi 1.6.0 | `rpc-fault-429-retry-after` | the same as 1.5.0: four requests, `auto_retry_start` 2/4/8 s, `auto_retry_end` `success: false`, then `agent_settled` `aborted: false` as the last record (PI16-01) |
| pi 1.6.0 | `rpc-fault-503-retry-after` | 503 `overloaded_error`, `retry-after: 3`: the same retries and end, a text `errorMessage` `503 …`; nothing structured |
| pi 1.6.0 | `rpc-fault-usage-limit` | 429 with a rejected 5-hour `anthropic-ratelimit-unified-*` (the claude case's answer): pi retries it as a rate limit (2/4/8 s) and ends the same way; no reset on the stream |
| pi 1.6.0 | `rpc-fault-billing-no-retry` | 400 `invalid_request_error` "credit balance is too low … Billing": one request, no `auto_retry_*` (pi's retry rule excludes billing), `agent_end`, then `agent_settled` |
