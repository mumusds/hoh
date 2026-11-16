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
- The raw recordings stay local, under `.hoh-dev/evidence/commander-readonly/spike/<run>/`: they carry local paths,
  plugin lists and rate-limit and plan metadata. They are never committed.
- `excerpt.py` makes the committed `excerpts/`. It keeps only the frames the evidence table reads, replaces the run's
  temporary paths with `<P>`, `<W>`, `<T>` and the home directory with `<HOME>`, replaces every uuid with `<uuid-N>`
  (one numbering for all the runs given, so a resumed session keeps its placeholder) and drops every field named like
  an account, user, e-mail, plan, organisation, credential, rate limit or uuid. For codex it keeps only raw tool calls
  and their outputs, never raw messages, which may carry instructions; an output in which the model listed its tools is
  replaced by a marker, because the list names the user's connectors. For pi the observer's report keeps the builtin
  tools and replaces the names of the user's extensions and their tools by placeholders. The first line of every
  excerpt names its source file and that file's sha256, and says it is an excerpt. The excerpts are scanned for e-mail
  addresses, keys and tokens; a hit writes nothing.
- Regenerate with `python3 tests/fixtures/commander-readonly/excerpt.py .hoh-dev/evidence/commander-readonly/spike/20261116T180821Z
  .hoh-dev/evidence/commander-readonly/spike/20261116T201950Z` (the raw runs are on the machine that recorded them).

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
