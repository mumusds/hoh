# Tooling reviews

Reviews of bootstrap tooling changes that belong to no task, through `tools/agent_task.py --role reviewer`, each in a fresh native session separate from the main development session. Raw transcripts stay in the ignored `.hoh-dev/attempts/`; this file keeps the verdicts, the findings and how each was handled.

## Grok as a bootstrap agent (branch `tools/grok-agent`)

Owner decision of 2026-11-05: Codex, Claude, Pi and Grok are full subscriptions with ample quota, to be planned to work together. Codex's "Selected model is at capacity" is temporary server congestion; another Codex model (`--model gpt-6.1-sol`) usually works.

### Round 1 (fresh Codex session)

- Agent: `codex` codex-cli 0.166.0, session `01a1ac42-6bee-7931-8b7c-3ce9b4bff243`, 779 s; attempt record sha256 `c94fac4c8c38b6d35e6d99b605089ebb62ed625b138ba062c208815a9f829a01`, stdout sha256 `3f7c8af0bb02d2605c24b70db7cd04ae5bcd44ed3d23327af6c4ee729f7dfd05`.
- Scope: `8f38ec8..06c9924`, all six changed files; the bundle in a copy (376 tests, one skip); fresh-directory mock bridge, report and schema checks; two real grok 1.0.70 turns without tools (a normal one and an unknown `--model`).
- Verdict: **FAIL** (2 high, 1 medium).

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| TG-01 | high | agent_task.py:155 — inside run_check a Grok session was written to `live-sessions.json` as `agent: grok`, which the locked `contracts/live-session.schema.json` (codex, claude, pi) and the release gate reject. | Fixed without touching the locked schema (that would need a CR, and AGENTS.md item 4 keeps the product at three agents): when `HOH_EVIDENCE_DIR` is set the bridge refuses with 64, before anything starts, any agent the schema does not accept. Docs say Grok is a bootstrap helper outside checks. The run_check live-sessions test now validates the file against the schema. |
| TG-02 | high | test_tools.py:109 — the Grok error case reused the first call's attempt directory and passed on the bridge's 64. | Fixed: its own test with a fresh directory, asserting `NATIVE_FAILED`, exit code 0, `terminal_observed` and `native_error_observed`. The fixture is the `error_during_execution` result re-observed from grok 1.0.70 with an unknown `--model`; the unfounded "capacity observed" comment is gone. |
| TG-03 | medium | agent_task.py:95 — for claude and grok an earlier non-empty result outlived a last empty one as the final text. | Fixed: the terminal result alone sets the final text. |

### Round 2 (focused, fresh Codex session)

- Agent: `codex` codex-cli 0.166.0, session `01a1ac52-37eb-7a13-b134-a9f52c5bee17`, 571 s; attempt record sha256 `8892388ac57fd50f53b9f1500a7f7650bd30e317b9c1367639d26cb814b559eb`, stdout sha256 `8808104adc2615272e8418ac2b5a02d2cffef8919b7acfa225c6d0fe60bedce4`.
- Scope: `06c9924..d27a76f` and the whole branch; TG-01 and TG-03 tests failing on the old code; 22 environment and dry-run cases; 10 recorded Claude streams parsed the same before and after; the bundle in a copy (379 tests, one skip).
- Verdict: **PASS**. TG-01..03 resolved.

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| TG-04 | low | CLAUDE.md:50 — the consent line listed Grok's quota for live checks and the soak too. | Fixed: Codex, Claude and Pi for reviews, QA, live checks and the soak; Grok for bootstrap reviews and QA outside checks. |

### Round 3 (focused on TG-04, fresh Claude session)

- Agent: `claude` 2.1.318 (Claude Code), session `e4afd374-3016-4a0b-8e03-ee33443503d6`, 60 s; attempt record sha256 `c0608a9d02ac5c40aea348cae61bb21225ee1033e0532cb744276cdc8fc0a23b`, stdout sha256 `5b6ccd465bc3c4247959879265b092dc4a442e2d17e5796288c1f465217ddfbd`.
- Scope: `d27a76f..e344de7` (CLAUDE.md), against CLAUDE.md:25, AGENTS.md item 3, docs/19, docs/22, the bridge's refusal and the locked schema; read only.
- Verdict: **PASS**, no findings.

Observed in use: a Grok review session (M1-T08 round 3) gave its report in an assistant message and then, as background commands ended, a few short closing messages; its final `result` held only the last of those. Under `--report-out` the bridge would refuse such a session (no `hoh-report` in the last result), which fails safe. Outside checks the report is read from the stream.

## The bundle self-test never writes into a check it runs inside (branch `tools/bundle-env`)

Found by the M1-T08 round-7 reviewer: with run_check's variables in the environment (inside a check, or a reviewer's shell), the NativeBridge bundle tests started the mock bridge with them inherited; the Grok tests met the bridge's in-check refusal and the other mock sessions were appended to that check's `live-sessions.json`. Fix `2e17782`: the NativeBridge environment drops the variables and `scripts/check.sh --bundle` runs the unittest discovery without them.

### Round 1 (fresh Claude session)

- Agent: `claude` 2.1.318, session `a6b55032-7f1f-487c-bcf0-25d67234ed86`, 905 s; attempt record sha256 `0cdd8a74de29dbea93e283e77ba0d460c94b524ba14e2ae7bbad9acdaa77835f`, stdout sha256 `e91b03ca9f240369706147d1cd1955bb3f6d8a0cdc1578ac6f449fa6fea6bafc`.
- Scope: `31848a6..2e17782`; every reader of run_check's variables among the bundle tests, tools and product scripts; the new test against the old code; the bundle with and without the variables, in clones of both commits.
- Verdict: **PASS** (5 low).

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| TB-01 | low | check.sh:11, test_tools.py:12 — run_check sets six variables (also `HOH_CANDIDATE_FILE`, `HOH_CHECK_RECORD_ID`); the fix named four. | Fixed: `CHECK_VARIABLES` in `tools/agent_task.py` names all six (`tools/run_check.py` and `tools/common.py` are locked, so the list is not moved there); `test_the_check_variables_are_those_run_check_sets` keeps it equal to what run_check.py exports. |
| TB-02 | low | test_tools.py:120 — the new test ran Grok only, which the bridge refuses inside a check before writing anything, so it could not see a leak into live-sessions. | Fixed: the test also runs codex, which would record its session there. |
| TB-03 | low | check.sh:12 — the `env -u` line had no test. | Fixed: `test_the_bundle_self_test_unsets_every_check_variable` checks the bundle command unsets each of `CHECK_VARIABLES`. |
| TB-04 | low | agent_task.py:25 (outside the diff) — `child_env` passed the variables to real agent sessions, so a reviewer or QA session under a check that ran static.sh or cargo test would write into that check's evidence. | Fixed: `child_env` drops `CHECK_VARIABLES` (the bridge still records the session in the check), and the attempt record lists them in `env_removed`. Test `test_a_native_session_never_sees_the_check_it_runs_in`. |
| TB-05 | low | test_bundle.py:164 (outside the diff) — `test_offline_setup_idempotent` ran the copied bootstrap with the test's own working directory, so preflight's real `pi --version` let a user-level pi extension write `.pi-lens-probe-home/` into the repository. | Fixed: the scripts run in the copy (`cwd=self.root`). Not reproduced on this VM; the change makes it impossible by construction. |

Mutation check: dropping the strip from `child_env`, keeping a variable in the NativeBridge environment, removing one `-u` from check.sh and leaving a variable out of the list each make a new test fail (4/4).

### Round 2 (focused, fresh Codex session)

- Agent: `codex` codex-cli 0.166.0 with `--model gpt-6.1-sol`, session `01a1ad7e-54b0-7803-bba7-175aca0b9edb`, 464 s; attempt record sha256 `b695700e0e5dbeacdb80833c202086fcf01ec85728fdec99e9a799c70bb36e11`, stdout sha256 `3f2f59b8248c09ccc186996e5b9a4af68b45de94cff80ec4fe38fed231e852b5`.
- Scope: `2e17782..f3bd36d` and the whole branch; what stripping the variables from native sessions means for independent-qa/review, adapter-live, live-sessions.json, report binding and the release gate's provenance checks; the new tests against mutated copies.
- Verdict: **PASS** (2 low). TB-01..05 resolved.

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| TB-06 | low | test_tools.py:148 — the check.sh test only searched for the `-u` text: with the whole `env` command commented out it still passed. | Fixed: the test runs a copy of check.sh whose `python3` records its argv and environment, and asserts the unittest run has none of the six variables while the other steps keep them. |
| TB-07 | low | test_tools.py:143 — the regex over run_check.py read single-quoted keys only. | Fixed: the keys are read from run_check.py's syntax tree (the dict merged into `env`). Both reviewer mutants (the `env -u` removed, a double-quoted key added) now fail the tests. |

### Round 3 (focused on TB-06, TB-07, fresh Claude session)

- Agent: `claude` 2.1.318, session `370230f1-7020-4270-9201-6ec3423fd1da`, 612 s; attempt record sha256 `794dbdb51553547946b41d5c1c0648a2654d28db6a1b2d37ddf2a9a863b26858`, stdout sha256 `00de5c71c75fd01d764b5d4d230aa609fe880b504b778422b326e516dc46bc59`.
- Scope: `f3bd36d..a7dcb39`; the two guard tests against 13 mutants and a project venv on a path with a space.
- Verdict: **PASS** (1 medium, 1 low).

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| TB-08 | medium | test_tools.py:154 — the recorder's `#!/usr/bin/env -S <interpreter>` split an interpreter path with a space, so the bundle self-test failed (rc 127) in a checkout on such a path; a regression of `a7dcb39`. | Fixed: the recorder is a `.py` file started by a `/bin/sh` shim that execs the quoted interpreter and recorder. Verified with a venv on a path with a space. |
| TB-09 | low | test_tools.py:145 — reading constant keys from the syntax tree missed keys built otherwise (an f-string, `**`, a key set after the dict), and the disposition of TB-07 claimed "however they are spelt". | Fixed by a behavioural test instead: `CandidateAndChecks.test_the_check_variables_are_those_run_check_sets` runs a real check through run_check, with every outer `HOH_` variable removed, whose command records the `HOH_` variables it sees; they must be exactly `CHECK_VARIABLES`. An f-string key and a key set after the dict both fail it now. TB-07's disposition above overstated the syntax-tree test. |

### Round 4 (focused on TB-08, TB-09, fresh Codex session)

- Agent: `codex` codex-cli 0.166.0 with `--model gpt-6.1-sol`, session `01a1ad98-3ad1-7ca2-be52-76dc10fe2949`, 311 s; attempt record sha256 `37915746d525bf9c13509001f59ec768171b80e5efa380e2a29bb75807647fa9`, stdout sha256 `0d6809cd3a018129e2d477d362207cffb3e2f5bafdd87296bd65244b1bec4811`.
- Scope: `a7dcb39..9438f58`; both guard tests against their regressions, machine dependence (spaced or odd interpreter paths, outer `HOH_` variables, PATH), the restoring of `os.environ`, the new PLAN entry's effect on the other CandidateAndChecks tests.
- Verdict: **PASS**, no findings.

## The recorder SIGTERM test judges a leak by the kernel, not by systemd's listing (branch `tools/record-scope-listing`)

`tests/bundle/test_record.py` `Interrupted.test_sigterm_while_constructing_the_version_probe` failed in about 4% of runs (1/40 and 5/120 on this VM; the follow-up from the M1-T08 round-8 review and M1-T09). Root cause, measured: the recorder had already seen the probe's scope empty in the kernel (`cgroup.events` populated 0, or the cgroup gone), but systemd removes the transient unit a few ms after it sees the cgroup empty (3-5 ms here), so `systemctl --user list-units` still listed it as `loaded active running`. The test now requires every listed scope of its own to be empty in the kernel at once, and gives the listing a bounded 5 s wait to drop it. 0 failures in 200 runs; mutants (stop_all does nothing; Proc does not register its child) still fail at the kernel check, leaving nothing behind.

### Round 1 (fresh Grok session)

- Agent: `grok` 1.0.70, model grok-4.7, session `01a1ae2b-11f2-7421-a180-a3b903765ed2`, 1300 s; attempt record sha256 `0088ec9c603f7359d291ca96daf1a6be8190c7b6df9939dc52fe6f3974231530`, stdout sha256 `a81d058f5904fcf1c643a50120c84e20ec91eacc4fd56968f007749ba43bfbf4`.
- Scope: `ca76e82..a099387`. The reviewer measured the race itself (79/80 listings still running right after the cgroup emptied; median 7.7 ms, max 13.4 ms to the unit's removal), ran the test 80 times, and three mutants (stop_all no-op, no registration, a kill of the leader only with `populated()` always false), all failing.
- Verdict: **PASS** (1 low).

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| TR-01 | low | test_record.py:545 — once the kernel check had seen a process in the scope, the test still waited the full 5 s before killing it and stopping the unit, and an OSError other than ENOENT/ENODEV from `members()` skipped the cleanup. | Fixed in `a8c7d57`: sampling and waiting run in `try`, the wait ends once a scope holds a process, and the `finally` stops every unit seen and a probe still alive. |

### Round 2 (focused on TR-01, fresh Claude session)

- Agent: `claude` 2.1.318, model claude-opus-5-5, session `a793510e-bf45-46c5-ad61-b580084ce882`, 442 s; attempt record sha256 `df742feaff07066013217e653180fe652cadd66613ff97e27c1bbd77e79a978f`, stdout sha256 `9da7f2248b36e5af80318582dffee0e3d99e6e520b3b1f798fec2839a52d67a1`.
- Scope: `a099387..a8c7d57`, in `git archive` copies: 90 unmutated runs; the stop_all no-op mutant now fails in about 200 ms instead of after the 5 s wait; with `members()` raising EACCES the units seen are still stopped.
- Verdict: **PASS** (1 low).

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| TR-02 | low | test_record.py:554 — the `finally` read `probe.pid` before stopping the units; the probe creates the file before writing it, so against a defective recorder an empty file raised ValueError (no-op mutant 1/40, 1/8, 1/10), the units were not stopped and the error hid the kernel assertion. Present before this branch too. | Fixed: an empty pidfile counts as not yet written, and the units are stopped in an inner `finally` whatever the pidfile read does. 60 unmutated runs pass; the no-op mutant fails at the kernel assertion in 20/20 runs, never with ValueError, leaving nothing behind. |
