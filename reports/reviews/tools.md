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

### Round 3 (focused on TR-02, fresh Claude session)

- Agent: `claude` 2.1.318, model claude-opus-5-5, session `9b570a03-f98d-4f22-9983-8f610c12b58e`, 375 s; attempt record sha256 `d3f6b0d3bfcc474058c2080f9e10cd6b5f6ed6e9078853da0571f267e27ebe16`, stdout sha256 `d05230c7ca924775912185142c76de03570bd55a5e36806ca4e90adb1f88b878`.
- Scope: `a8c7d57..8243300` and the record of rounds 1-2. The reviewer forced the empty-pidfile window (the probe waits 3 s between creating and writing its pidfile): with stop_all doing nothing the old code raised ValueError and left the scope running (2/2), the new code failed at the kernel check and left nothing (5/5); the real recorder passed 10/10; 60 unmutated runs and the test_record module passed. Round 2's EACCES result was 29/30, the one exception being the TR-02 case.
- Verdict: **PASS**, no findings.

## Session alert hooks: API failures and session ends recorded with a recovery point (PR #21, branch `tools/session-alerts`)

StopFailure and SessionEnd hooks (`scripts/dev/session-alert.sh`, logic in `session_alert.py`) write a recovery point to `.hoh-dev/session-alerts.jsonl`; `session-context.sh` shows what is new at the next start. A normal Stop has no hook. Alerts record and remind; they are not a continuation mechanism. Merged as 07b7233 (rebase; tree equal to the reviewed 7c6ac26) after green CI.

Real-entry verification (Claude Code 2.1.318, record `.hoh-dev/evidence/session-alerts-entry/README.md`). Historical, at f7dd0b4 (a pre-rebase commit no branch reaches; its tree is main's fedf1ce) plus the display fix (8c037cf), before round 3 added HOH_TOP_LEVEL_SESSION: a session carrying only HOH_BOOTSTRAP_PARENT (as `scripts/start.sh` set it then) got the SessionStart context and a SessionEnd record, a delegated one neither; an interactive session in a PTY against a local 529 server (throwaway config dir, fake key) recorded StopFailure with an OSC 9 notification, then SessionEnd; `claude -p` does not wait for StopFailure (its runner is not awaited), so headless API failures leave only SessionEnd; that run found an unavailable recovery part shown split into characters (fixed). On the merged hooks (run at 15:24 UTC on docs/process-continuity cbbab8b, whose hook files are identical to main 07b7233, for PR #22's review): a session whose HOH_BOOTSTRAP_PARENT equals HOH_TOP_LEVEL_SESSION (as `scripts/start.sh` sets them now) gets the context and a SessionEnd record; one whose parent is another session's id gets neither. StopFailure was not re-run: its own handling (error fields, OSC 9) did not change in round 3; the top-level gate and the 1 s collection limit it shares with SessionEnd were exercised by the SessionEnd re-run and by `tests/bundle/test_session_alerts.py`.

### Round 1 (fresh Claude session)

`claude` 2.1.318, session `f6057823-a76b-4f4a-b024-88519dc714a3`, 645 s; attempt record sha256 `edbcf6e77e8e0579007b3de72c7f11c501f02c041e4ebf8b2409cb0651a878f3`, stdout sha256 `ab58661a2a3c69037cb47c35fa4bd0eb9b7c8fc61d089532f3bce0d8e99c5039`. Candidate d17677d. Verdict **PASS** (2 medium, 4 low).

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| SAR-01 | medium | A top-level session started by `scripts/start.sh` carries HOH_BOOTSTRAP_PARENT and was skipped as delegated. | Fixed: delegated sessions carry HOH_DELEGATED_TASK=<role> (round 2), and the top-level session HOH_TOP_LEVEL_SESSION (round 3). |
| SAR-02 | medium | Any failing recovery-point collector (dangling evidence link, odd task-state, non-string message, 200 KB payload in the environment) lost the record and the notification. | Fixed: payload from stdin, each part collected on its own (`unavailable: <Exception>`), notification printed before the record. |
| SAR-03 | low | Seen-counter edge cases (more than 5 new alerts marked seen unshown; deleted/rotated file; partial line; shared counter). | Partly: an "older new alert(s) not shown" line; the rest deferred. |
| SAR-04 | low | ASCII filter, 300-char cut and 5-alert cap untested; newest-10 cut kept the oldest. | Fixed: tests; newest 10 kept. |
| SAR-05 | low | Background match missed interpreter spellings and `mutate*.py`. | Fixed. |
| SAR-06 | low | `error_details` unbounded; multi-line messages in the context. | Deferred (a 1000-char cap was added, untested). |

### Round 2 (focused on the fix diff, fresh Claude session)

`claude` 2.1.318, session `566b7b37-1159-4903-b9a3-17a5ee48511a`, 726 s; attempt record sha256 `a4d86c86ffc7142f329be47c1ffe8147ae76d9bf0468a681e1aa96a11ad1f01f`, stdout sha256 `7288ef61013dca20c6e2bf277904e07326e9fbe37a4ece57f5fc88268389fe34`. Candidate 90afced. Verdict **FAIL** (1 high, 1 medium, 3 low). SAR-01, -02, -04, -05 confirmed resolved.

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| SAR-07 | high | The new bridge test ran `--top-level` with the caller's HOH_BOOTSTRAP_PARENT, so the bundle failed in start.sh and delegated QA sessions. | Fixed: the bridge and setup tests strip the session markers; the bundle passes with HOH_BOOTSTRAP_PARENT set. |
| SAR-08 | medium | Hooks come from the `--cwd` checkout, the bridge from the caller's: an older bridge's reviewer was treated as a main session (this review got the context and wrote a record into the worktree, deleted by the developer). | Fixed: HOH_TOP_LEVEL_SESSION (set with HOH_BOOTSTRAP_PARENT by start.sh and `--top-level`, inherited); a session whose parent is not it is delegated whatever bridge started it. |
| SAR-09 | low | Three 3 s collector timeouts could reach the 10 s hook limit. | Fixed: 1 s each. |
| SAR-10 | low | Notification-before-record and the marker's value untested; an empty role accepted. | Fixed: tests; empty `--role` refused. |
| SAR-11 | low | docs/23 still named HOH_BOOTSTRAP_PARENT as the delegation mark. | Fixed. |

### Round 3 (focused on the fix diff, fresh Claude session)

`claude` 2.1.318, session `322832fb-d77f-481f-b7b2-4681a42c5e72`, 533 s; attempt record sha256 `42a7823fcc3d62a44f3c206b47fb18dd3f6aa704d39c43af78a9774706d33876`, stdout sha256 `2844e0f22e56c2db6b5409f5ad25798cccac350ec559d9b68ab2aa2b2e9f1a91`. Candidate 7c6ac26. Verdict **PASS** (3 low). SAR-07..11 resolved.

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| SAR-12 | low | The newest-10 test can fail while another bundle runs on the same machine (its decoys push ours out). | Deferred (follow-up). |
| SAR-13 | low | A claude started directly in a top-level session's shell inherits both markers and counts as main. | Deferred (follow-up). |
| SAR-14 | low | A top-level session started before the update has no HOH_TOP_LEVEL_SESSION and counts as delegated until restarted. | Deferred (follow-up; no such session was running). |

## Process rules written into docs/19 (PR #22, branch `docs/process-continuity`)

Owner instructions of 2026-11-06 (continuing and waiting, session identity, review convergence, helpers, layered verification, recovery) recorded in docs/19 and referenced from AGENTS.md, CLAUDE.md, RESUME_PROMPT.md and LAUNCH_PROMPT.md.

### Round 1 (fresh Claude session)

`claude` 2.1.318, session `6de7fff8-b97c-466a-8c03-e09a6c53a96a`, 469 s; attempt record sha256 `b1e242cfc1204038eecc6c08a86b784bba33a2499dc75a9dd33f06bc26fa54c9`, stdout sha256 `f211e80c19205f74e6e46575b2637aa27f4b65b78509fc12d52ad4d51ac5c680`. Candidate cbbab8b. Verdict **PASS** (1 medium, 7 low).

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| PRC-01 | medium | The PR #21 record presented the real-entry run (before round 3) as verification of the merged hooks, which classify its "HOH_BOOTSTRAP_PARENT only" session as delegated. | Fixed: labelled historical; re-run on the merged hooks (top-level and old-bridge child), recorded. |
| PRC-02 | low | LAUNCH_PROMPT lost the general "Codex unavailable -> fresh Claude" fallback. | Fixed. |
| PRC-03 | low | Waiting rules named only Claude Code mechanisms though Codex/Pi may be top-level. | Fixed: their native background mechanism, else foreground with a timeout. |
| PRC-04 | low | "Stop has no hook" read against /goal, itself a Stop hook. | Fixed: project settings add none; /goal's own check excepted. |
| PRC-05 | low | The historical-evidence recovery rule sat outside the sections RESUME_PROMPT names. | Fixed: moved into 恢复. |
| PRC-06 | low | Deferrals named only the git-ignored PROGRESS.md. | Fixed: dispositions live in the tracked review record; PROGRESS points to them. |
| PRC-07 | low | scripts/bootstrap.sh's PROGRESS template has no `## Next` heading (outside this diff). | Deferred; fixed later on branch `docs/followups`: the template has the docs/19 headings, checked by a bundle test. |
| PRC-08 | low | Duplication and wording (CLAUDE.md "process", step 4 "fix findings", a garbled sentence, "状态更新"). | Fixed those four; the repeated fallback/reconciliation pointers are kept as short references. |

### Round 2 (focused on the fix diff, fresh Claude session)

`claude` 2.1.318, session `66b658d6-3b48-4b7b-be5e-b6f883e9fddd`, 288 s; attempt record sha256 `7b90531816a9070d999c5b05f656702a9d2199c0d7b47a3848acf9a7cfdf40f4`, stdout sha256 `5c8f3d54a0e5a06b13e134768b933bf3b3ef06202699934977e6121cb0565b66`. Candidate 28b5307. Verdict **PASS** (3 low). PR #22 merged as 695d0ec after green CI; the lows were deferred and are fixed on branch `docs/followups`.

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| PRC-09 | low | The PR #21 record said StopFailure's path did not change in round 3, though round 3 added the top-level gate every event passes and cut the collection timeout to 1 s (shared parts covered by the SessionEnd re-run and bundle tests). | Fixed: the record says StopFailure's own handling did not change and names what covered the shared parts. |
| PRC-10 | low | The re-verification paragraph named main 07b7233 at ~16:00 UTC while the run was at cbbab8b at 15:24 UTC, and omitted each run's argv and environment. | Fixed: real time and commit (hooks identical to 07b7233) in the tracked record; argv and environment of both runs in the evidence README. |
| PRC-11 | low | The record named f7dd0b4, a pre-rebase commit no branch reaches. | Fixed: its main equivalents fedf1ce and 8c037cf are named too. |
