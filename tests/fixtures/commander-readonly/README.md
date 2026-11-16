# Commander read-only profile: recording spike (ADR-032 slice S0, design correction 3)

Live recordings of the Commander's read-only profile on the installed agents, made before CR-024 is locked and before
the TEST-021-07 judge is written. A row of ADR-032's evidence table is valid only if it cites one of these recordings.

- `record_spike.py` makes a run. It is built on `tests/fixtures/protocol/record.py`'s machinery: every child runs in its
  own systemd user scope, the parent agent-session variables are stripped and the raw stdin, stdout and stderr are kept
  byte for byte. Each session gets a fresh project outside every checkout, with the four regions of ADR-032 decision 8
  (P's work tree, P/.git/, P/.hoh/ and the private working directory W), every cell seeded, a generated shell driver
  whose region paths are constants (it refuses to run when a region is missing), the exact patches and, for pi, an
  observer extension that reports the active tools and their sources. The regions are snapshotted before and after
  every session.
- For every codex case the recorder also copies the thread's rollout file into the case directory as `rollout.jsonl`:
  the App Server response's `thread.path` (marked [UNSTABLE] in the 0.166.0 schema), for exec the one file under
  `CODEX_HOME/sessions` named with the thread id. It is the only record of a resumed thread's patch calls and their
  outputs. `record_spike.py --collect-rollouts <run>` copied them for the two runs below after the fact (so the
  app-ro-new copy already holds the resumed turn too).
- The raw recordings stay local, under `.hoh-dev/evidence/commander-readonly/spike/<run>/`: they carry local paths,
  plugin lists, rate-limit and plan metadata, and the rollout files account identifiers. They are never committed.
- `excerpt.py` makes the committed `excerpts/` (ADR-032 design correction 4). It keeps only the lines the evidence
  table reads, and **every kept line is the raw line with some values replaced**: keys, nesting, array lengths and the
  JSON type of every value are the source's. It replaces the run's temporary paths with `<P>`, `<W>`, `<T>` and the
  home directory with `<HOME>`, every uuid with `<uuid-N>` (one numbering for all the runs given, so a resumed session
  keeps its placeholder), every value under a key named like an account, user, e-mail, plan, organisation, credential,
  token, rate limit, request id, device, socket or provider base URL with `"<redacted>"` (numbers with 0), and every
  value listing the user's plugins, skills, slash commands, agents or memory paths with `"<redacted>"`. A string in
  which the model listed its tools (it names the user's connectors, `mcp__…`) becomes a marker; in pi's observer
  report the user's extensions' tool names become `<extension-tool-N>`. For codex it keeps raw tool calls and their
  outputs only, never raw messages, which may carry instructions. The excerpts are scanned for e-mail addresses, keys
  and tokens; a hit writes nothing.
- Every excerpt's first line is a header: the source file and its sha256, and for every following line its line
  index in the source and the sha256 of its key skeleton. `excerpt_check.py` compares each line's skeleton with the
  recorded one and, where the raw source is present locally with the recorded sha256, with the source line's
  skeleton; it also compares each `.meta.json`'s `snapshot_diff` with the raw snapshot's. The bundle self-test runs
  it on the recorded skeletons (tests/bundle/test_commander_readonly.py).
- `.meta.json` files are summaries, not excerpts: the case's argv, exit, notes, driver text and token hash, and the
  raw snapshot's `diff` object (values redacted) with the snapshot's sha256.
- Regenerate with `python3 tests/fixtures/commander-readonly/excerpt.py` followed by every run directory under
  `.hoh-dev/evidence/commander-readonly/spike/` in order (the raw runs are on the machine that recorded them), then
  run `excerpt_check.py`.

## Run 20261116T180821Z

Versions: claude 2.1.325, codex-cli 0.166.0, pi 1.5.0.

| Agent | Case | What the recording shows |
|---|---|---|
| claude | ro-new, ro-resumed | init frame `tools` exactly Glob, Grep, Read; `mcp_servers` empty; `permissionMode` dontAsk; one Read of TOKEN.txt; no write attempt possible; snapshot unchanged |
| claude | ro-plus-write | `Write` added to `--tools` but not pre-approved: the call is refused with a `system` frame of subtype `permission_denied` (`decision_reason_type` mode), a `tool_result` with `is_error` true, and an entry `{tool_name, tool_use_id, tool_input}` in the result's `permission_denials`; snapshot unchanged |
| claude | control | full access: Bash runs the driver (every line `exit=0`) and Write creates the file; the snapshot shows every change |
| codex | app-ro-new | thread/start result `sandbox.type` readOnly; the driver runs as a commandExecution item whose output has twelve `Read-only file system` lines; the twelve patches and the extra write create **no fileChange item**; each refusal is visible only as a raw `custom_tool_call_output` (experimental raw events) with the text `patch rejected: writing is blocked by read-only sandbox; rejected by user approval settings`; snapshot unchanged |
| codex | app-ro-resumed | thread/resume (which offers no raw events) result `sandbox.type` readOnly; the driver's twelve refusals are visible; the patch attempts leave **no trace at all** in the stream; snapshot unchanged |
| codex | app-control | full access: the driver's lines are all `exit=0`, and each patch is a fileChange item `completed` with `kind.type` add, update or delete and a diff; the snapshot shows every change |
| codex | exec-ro-new | exec, read-only: the driver's refusals are visible as a `command_execution` item; the patch attempts leave no trace in the stream; snapshot unchanged |
| codex | rollouts (`*.rollout.excerpt.jsonl`) | every turn has a `turn_context` with `approval_policy` never and `sandbox_policy.type` read-only (danger-full-access in the controls); every patch attempt of the new **and the resumed** turn, and of the exec thread, is a `response_item` `custom_tool_call` (name `exec`, input `tools.apply_patch("<the exact patch>")`) followed by a `custom_tool_call_output` with the same `call_id` whose text is the read-only refusal; the controls' outputs are `Script completed` |
| pi | ro-new, ro-resumed | the observer reports active tools exactly read, grep, find, ls, all builtin; one read of TOKEN.txt; the snapshot shows pi's own session file under `.hoh/sessions/` **and files the user-installed pi-lens extension writes into W (`.pi-lens-probe-home/*.log`)** |
| pi | ro-plus-write | `write` added to `--tools`: pi executes it (`tool_execution_end` `isError` false, "Successfully wrote"); pi has no refusal for a tool in its active set |
| pi | control | full access (default tools): the model (glm-5.3) declined the task as a suspected probe, so no write happened; the extension again wrote into W |

## Run 20261116T201950Z (`--set errors`)

Ordinary operation errors, recorded as the judge's negative fixtures, and pi's read-only profile with `--no-extensions`.
The prompt asks for: `cat` of a missing file; a patch updating a missing file and a patch whose old line does not
match; an edit whose old text does not match; a write under a regular file (`TOKEN.txt/hoh-cell`); a read of a missing
file. Every snapshot except pi's is unchanged.

| Agent | Case | What the recording shows |
|---|---|---|
| claude | ro-ordinary-error | Read of a missing file: `tool_result` `is_error` true ("File does not exist."), no `permission_denied` frame, `permission_denials` empty |
| claude | control-ordinary-error | full access: Bash `cat` exit 1, Edit "String to replace not found", Write `ENOTDIR`: each `is_error` true, none with a `permission_denied` frame, `permission_denials` empty |
| codex | app-ro-ordinary-error | read-only: both bad patches and the bad edit give `apply_patch verification failed: …` (checked before the sandbox), no fileChange item; the add under a regular file gives the read-only refusal text; the `cat` is a commandExecution with exit 1 and `No such file or directory` |
| codex | app-control-ordinary-error | full access: the same `apply_patch verification failed: …` texts without a fileChange item; the add under a regular file is a fileChange item with status `failed` and the output `Failed to write file` |
| pi | ordinary-error | `edit` and `write` added: `tool_execution_end` `isError` true for the edit ("Could not find the exact text"), the write (`EEXIST … mkdir`) and the read of a missing file (`ENOENT`); the extension again wrote into W |
| pi | ro-no-extensions | read-only profile plus `--no-extensions`: the explicitly loaded observer still reports exactly read, grep, find, ls, all builtin; the snapshot shows only pi's session file, nothing in W |

## Run 20261116T212808Z (`--set noext`)

pi with the read-only argv of ADR-032 v5 (`--tools read,grep,find,ls --no-mcp --no-extensions`, plus the observer by
`-e`), a new session and the same session resumed in a new process. From this run on, the recorder creates pi's
session directory and its parents before the baseline snapshot, as the product's pi adapter does before it starts pi
(crates/hoh-agents/src/pi.rs, `create_dir_all` of the launch's `session_dir`); the earlier pi recordings were taken
without that step, so their snapshots also show the new `P/.hoh/sessions` and `P/.hoh/sessions/s1` directories.

| Agent | Case | What the recording shows |
|---|---|---|
| pi | noext-new | the observer reports exactly read, grep, find, ls, all builtin; one read of TOKEN.txt; the final message has the decision block with the token; the snapshot shows only pi's session file inside `P/.hoh/sessions/s1/` |
| pi | noext-resumed | the same report; the same session file changed, nothing else |
