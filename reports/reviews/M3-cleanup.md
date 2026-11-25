# M3 low-cleanup package: review records (lead)

Work list: `.hoh-dev/evidence/m3-lowcleanup/inventory.md` (64 open items, slices L1-L11). Each slice is one branch; frozen
commits are cited by hash here and by subject on main. Native records: `.hoh-dev/attempts/<attempt>/` in the commander's
checkout `/home/ai/dev/hoh`.

## L1 (spec index and acceptance view) and L6 (origin and migration)

Developer: Grok. Items: M3B-08, M3B-09, M3B-52, M3B-53, M3B-54, M3B-151, M3T04O-100, M3L-01, M3L-02, M3L-03, M3L-50,
FLK05-100, FLK05-101; each still applied and was fixed in its own commit (docs only for M3B-54 and M3L-03).

| Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|
| 1 | `f6b99857` | Codex codex-cli 0.171.0, `01a20c92-78d6-7310-b638-817657b1a18c` (`5b2af34b-2349-4f68-825b-dca377d813e8`) | PASS: M3LA-01 medium (the row-less binding's candidate.frozen subqueries scanned every Run's events) |
| 1 | `f6b99857` | Claude 2.1.331 (Claude Code), `bf76fd07-37b9-46ee-9ed1-0c2afc7c5362` (`f9dc4556-40fe-4da0-89cd-3500e289a01a`) | PASS: M3LA-50 low (bare CR rejected in prose lines too), M3LA-51 low (= M3LA-01) |

All three fixed red-first by Grok ("M3 cleanup: M3LA-01 M3LA-51 scope the row-less binding to the candidate run",
"M3 cleanup: M3LA-50 reject a bare CR only on value and structure lines") and checked by the lead: the Run predicate is
on both subqueries and a test asserts the plan uses `events_run_kind` with results unchanged; the CR rule now matches
ADR-033 with the old value-line negatives kept. The M3B-151 commit that narrows a "hold silence" assertion to exclude
this host's own reconcile line was examined by both reviewers; it still refuses every other `hoh host:` line.
Final checks: `bash scripts/dev/pre-pr.sh --no-deployment` all ok on `40ef7d40`.

## L10 (development tooling)

Developer: Claude; PR "M3 cleanup L10: development tooling (PREPR-01..04, BT-01, BT-02, CLN1S-b)". BT-03 is not done:
`tools/run_check.py` is locked, so queueing frozen checks on the machine test slot needs a CR (open item).

| Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|
| 1 | `cf6854e6` | Codex codex-cli 0.171.0, `01a20c86-68b4-7fd2-8fcf-3fbb0ed7762b` (`4bff0d01-b5ae-431e-83a1-0e11f96ca0c6`) | PASS: L10-01 medium (a too deeply nested hoh-report block failed the bridge with exit 1) |
| 1 | `cf6854e6` | Grok grok 1.0.92 (b06756fd6256), `01a20c86-7157-7db0-9e2c-dd22facd1e54` (`5b06a923-6fa7-4ff4-afb3-2fb50e77c4ad`) | PASS, no findings |

L10-01 fixed red-first by the lead ("M3 cleanup: L10-01 a too deeply nested hoh-report block is a bad block, not a
bridge failure"); pre-pr all ok on `b983d900`.

## L3 (input delivery)

Developer: Codex. Items: M3F-03a, M3F-04, M3F-05, M3F-06, M3H-53, M3H-54, M3H-55, M3H-56, M3I-01; all still applied and were
fixed (M3F-06 docs only). Two reviewers (concurrent delivery changed).

| Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|
| 1 | `d0f6f567` | Claude 2.1.331 (Claude Code), `72b9ffe9-5640-4cdc-9bca-6cca9146eefd` (`72f5137d-af9e-4a2f-8d72-b5ffca2eb412`) | PASS: M3LB-01 low (per-role delivery concurrency keyed by role name only: a held steer in one run delays the same-named role in another run; latency only) |
| 1 | `d0f6f567` | Grok grok 1.0.92 (b06756fd6256), `01a20e07-f05d-71b3-bf02-ac56a632625a` (`260b4b9c-8531-4cd1-9ec5-51337bda1835`) | FAIL: M3LB-50 high (Finish's stale and run-stopped branches dropped delivery_pending, so a pending turn's terminal reason depended on which transaction won against the sweep) |
| 2 (fix diff) | `e2650b86` | Grok grok 1.0.92 (b06756fd6256), `01a20e7c-929b-7972-a4ea-1af976868374` (`7f86ea99-4b11-4255-9225-b68fe34d47a1`) | PASS, M3LB-50 resolved, no findings |

M3LB-50 fixed red-first by a fresh Claude session ("M3 cleanup: M3LB-50 a pending delivery ends UNKNOWN_DELIVERY on every
terminal path"): a reserved turn ends UNKNOWN_DELIVERY whatever stopped it, in both orderings, for cancel and rebind; only a
receipt proving nothing reached the agent (NextAttempt, or an explicit native refusal: lead decision, the RPC error means
the steer had no effect) keeps the stale/stopped reason. The one changed expectation in
`test_017_02_end_before_stage_falls_back_but_epoch_change_before_ack_rejects` applies only to the reserved, acknowledged
input (it pinned the defect); the not-sent case keeps STALE_ROLE_EPOCH. Deferred low: M3LB-01 (M3 low backlog; ADR-036
records the per-role keying). Final checks: `bash scripts/dev/pre-pr.sh --no-deployment` all ok on `e2650b86`; after the
rebase onto main see the PR.
## L2 (spec commands) and CR-036

Developer: Codex. Items: M3B-06 (REJECT allowed after the CR's files reverted), M3B-07 (I/O errors while indexing a spec
answer 503 SPEC_SOURCE_UNAVAILABLE, retryable; a genuinely invalid index stays 422 SPEC_INVALID), M2R-501 (CR-029's outdated
"not relocked / stays DRAFT" text corrected in CR-036, CR-029 unchanged). CR-036 adds the missing reason code under CR-003
D1 item 1 (baseline revision 25); its review is recorded here because a relocked CR file cannot change after its commit.

| Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|
| 1 | `3c216c1f` | Pi 1.6.0, `01a20e7c-eee6-76ec-9151-ac662cd97941` (`de10131e-ba60-4cf0-8e5a-6f202cdb1b3b`) | PASS: M3LE-01 low (ENOENT on the spec root and "not a git repository" also answer the retryable 503), M3LE-02 low (a spawn_blocking JoinError still answers 503 code UNPROCESSABLE), M3LE-03 low (CR-036's own review table is empty: this section is that record) |

The CTO approves CR-036 under CR-003 D1 on this PASS: it only adds a missing reason code and corrects CR-029's status text; no
requirement or AC is removed, no gate, check or evidence rule is loosened, no CONTRACT decision changes. Deferred low:
M3LE-01, M3LE-02 (M3 low backlog). Final checks: `bash scripts/dev/pre-pr.sh --no-deployment` all ok on `3c216c1f`; after
the rebase onto main see the PR.

## L7 (process lifetime around checks), L8 (test hygiene), L9 and flakes FLK-08, FLK-09

Developer: Claude. Items fixed: M3B-201 (a verifier's check command waits at `hoh __check-gate` until it joined the
attempt's cgroup), M3J-250, M3F-03b, FLK34-100/101, M3C-01, M3C-02, CLN1S-a, M2QA-002, M2QA-003, FLK-08 (a host restart
refused by a killed host's child that still holds the start lock is retried for up to 5 s, only when the pid in host.lock is
gone), FLK-09 (a turn's BLOCKED_AUTH latch and its end commit exclude a tick's staffing); M3J-251 and M3J-R2-notes recorded as
residuals in ADR-035; M3J-52 dropped (a gated, never-opened process only reads stdin and exits 0 at EOF; it never touches
the run directory); FLK05-103 (0/120 under load) and DC4-DIAG-02 (not seen) recorded only. Two reviewers (process lifetime
and concurrency changed).

| Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|
| 1 | `660e1f2f` | Codex codex-cli 0.171.0, `01a20e06-8045-75f3-b579-dd7fa7147d83` (`e80e1129-f646-4f2a-849b-d8f20a8375b2`) | FAIL: M3LD-01 high (the runner dropped AttemptGroup::adopt's error and the gate opened anyway: a check could run outside the attempt's cgroup) |
| 1 | `660e1f2f` | Grok grok 1.0.92 (b06756fd6256), `01a20e06-8d97-7e11-a816-7f6a83ab577b` (`263c7c1f-6093-46c3-a41d-635b92271f0c`) | PASS: M3LD-50 medium (the turn's BLOCKED_AUTH latch was kept when hand() failed; staffing could block the binding first and the cause=turn event was lost) |
| 2 (fix diff) | `05910c80` | Codex codex-cli 0.171.0, `01a20ec4-d2c9-7120-9ddb-1a7983d476e5` (`5b5ef59c-a8dd-43d9-8112-f56a9760a7f6`) | FAIL on M3LD-100 (see below); M3LD-01, M3LD-50 and M3LD-L1 resolved |
| 2 (fix diff) | `05910c80` | Grok grok 1.0.92 (b06756fd6256), `01a20ec4-e126-7041-8422-92fce87c8cff` (`6df2b658-3a91-4c48-8a86-49f828f947f8`) | PASS, no findings |

Fixes by fresh Claude sessions, each red-first: "M3LD-01 a check command whose cgroup adoption failed never runs and is
recorded not started", "M3LD-50 a turn's end committed after staffing blocked its binding still writes the turn's block"
(the latch is kept: undoing it would let staffing unblock pi against AC-020-05), and "M3LD-L1 a check of a /proc-tracked
attempt runs under /proc tracking, not refused" (M3LD-L1, medium, found by the lead's check of the M3LD-01 fix: an attempt
degraded to /proc under CR-011 option A had every check recorded NOT_STARTED; the check now follows the attempt's recorded
tracking).

M3LD-100 (Codex round 2, reported high): an adoption failure is recorded as exit 127, the same dimension as a check that ran
and exited 127 itself, and every 127 labels FAIL/NOT_STARTED (inconclusive), so a check that ran, exited 127 and then passed
on a rerun is PASS rather than FLAKY. Lead ruling: pre-existing and by specification, not introduced by the fix diff:
ADR-027 (its run_check table: "command could not start" is 127, and 127 is an inconclusive FAIL/NOT_STARTED) and the label
in verification.rs (M2T10D-753) predate this branch, and the M3LD-01 fix records an unstarted command exactly as run_with's
spawn failure already does. Moved to the backlog as medium (it needs a check that really ran and exited 127 followed by a
passing rerun) with a recommended design correction: an execution record field for "started" independent of the exit
code. That changes ADR-027 and the locked run_check/release_gate semantics, so it goes through its own CR (next
verification task). Deferred low: none new. Final checks: `bash scripts/dev/pre-pr.sh --no-deployment` all ok on
`05910c80`; after the rebase onto main see the PR.
## L4 (resource bundles and prompts), L5 (consult and supervision), PI16-B2 and PI16-B3

Developer: Grok (fix rounds: Codex). Items fixed: M3E-02, M3G-01, M3G-51, M3G-52, M3G-54, M3G-100, M3G-101, M3K-03 (tests
only), M3K-04, M3K-06, M3K-07 (comment: CONSULTATION_OBSOLETE only reaches input.rejected, never a Message reason_code, so
no CR), M3K-103, PI16-B2, PI16-B3. M3G-53 kept as the documented residual of ADR-034 decision 9 (repack uses the current
bundle version; relaxing it would change AC-045-01's replay claim). Two reviewers in round 1.

| Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|
| 1 | `4eb78f66` | Codex codex-cli 0.171.0, `01a20e7c-3f66-7002-91b4-d611f816fc9d` (`82ebfd04-0059-44a1-9ba2-f0438a7f162c`) | PASS: M3LC-01 medium (bundle version cache not invalidated on release/format edits), M3LC-02 medium (old-pin compatibility checked after prepare had already refused as Decision), M3LC-03 medium (CARGO_MANIFEST_DIR dropped the product's HOME fallback), M3LC-05 medium (old-pin load errors other than one string were Transient), M3LC-04 low (settings diff used Bundle::current) |
| 1 | `4eb78f66` | Claude 2.1.331 (Claude Code), `797a2b1a-55d1-4ca7-9cfa-394c3017a616` (`9af39d0c-4321-42b7-90c8-047f204db459`) | PASS: M3LC-50 medium (= M3LC-05), M3LC-51 low (compatibility checked on the role's own template, not the resolved builtin), M3LC-52 low (ADR-012 PI16 note), M3LC-53 low (= M3LC-03), M3LC-54 low (restore Ok(false) kept a stale diff) |
| 2 (fix diff, after a conflict-free rebase onto `06332d8a`) | `6048a983` | Claude 2.1.331 (Claude Code), `50c06896-a542-4ac1-8ec7-d3c00a7b5312` (`d535a6e1-9642-4782-8180-4f7ec208ffcc`) | PASS, every round-1 finding resolved: M3LC-100 medium (only one test helper injected a global root; other unit, host and settings tests read the real ~/.config/hoh), M3LC-101 low |

Round 1 fixes (Codex, red-first): "M3LC-01 invalidate every bundle identity input", "M3LC-02 M3LC-05 M3LC-50 M3LC-51 classify
verified old pins before resolving prompts" (typed load errors: missing object, digest mismatch, decode failure and identity
mismatch stay Decision; only a loaded old pin the renderer cannot render is Transient, decided before template resolution on
the resolved builtin path), "M3LC-03 M3LC-53 restore XDG to HOME resolution", "M3LC-04 M3LC-54 refresh settings against the
host bundle", "M3LC-52 align Pi recording notes and doctor hints". Round 2 medium fixed red-first by Codex ("M3LC-100
M3LC-101 tests resolve prompts against an injected global root") and checked by the lead without another round: every
in-process ProductExecutor construction in the tests (product_prompts, world, host_stop, product) injects the World's
temporary root; the only uninjected call is the product's own HostConfig path. Deferred low: none. Final checks: `bash
scripts/dev/pre-pr.sh --no-deployment` all ok on `7ae6492b`; after the rebase onto main see the PR.

## PI16-B1 and FLK-10

Developer: Claude. PI16-B1: the pi 1.5.0 provider-fault set re-recorded with `record_faults.py` against the local fake
provider (no credentials, no model calls) from a private install whose launcher and package tree hashes equal the
2026-11-16 recording's: 429, 503, usage limit and billing, each ending with `agent_settled`; the bundle and adapter replay
tests now cover 1.5.0 and 1.6.0. FLK-10: the db_faults test armed its one-shot commit fault before the first run's staffing
commits had finished, so the host's own commit took it; it and two privileged neighbours now wait for
`staffing_deferred()` first (reproduced every time with a pause after arming, 0/240 under load after the fix).

| Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|
| 1 | `4f5a834d` | Codex codex-cli 0.171.0, `01a20f2f-0d85-7ff1-8080-e1367269bd11` (`396d93c3-8c61-4cfb-a4c5-8d23b4a104a6`) | PASS, no findings |

Final checks: `bash scripts/dev/pre-pr.sh --no-deployment` all ok on `4f5a834d`; after the rebase onto main see the PR.

## FLK-11 (the scripted writer's process-group exit)

Developer diagnostics from `7dec042f`, REQ-008 / AC-008-04, test
`test_008_04_m3l200_other_run_commits_between_draft_attempts`. The test's `ProductBetweenDrafts` executor reaped its
shell and sampled `group_empty(pid)` once. Under load that sample could still be false, even though Git had committed
successfully. The Runtime correctly recorded `runner.unreaped`, kept the attempt RUNNING and retained its writer
lease; the test then waited in vain for FAILED. This is a fixture race, not slow drafting or a product state defect.

The fixture now waits for the process-group exit before sending its outcome, as `world::Team::review` already does.
The 5 s bound limits that cleanup observation; it does not assert that a surviving group is gone. The original 20 s
assertion deadline, required FAILED state, real intervening Git commit, two drafts and no product-write violation
assertions are unchanged. Failure output now includes attempt and event diagnostics.

All loops held `/home/ai/dev/.hoh-test-slot.lock`, used the dedicated `m3flk-target`, and ran only scripted agents.
With 12 CPU burners and the entire drafting binary (`cargo test -p hoh-app --locked --test drafting -- --nocapture`),
the failure rate was **1/4 before, 0/8 after**. The red log shows `other-work/attempt-1` RUNNING, a successful Git exit,
and `runner.unreaped`. Two green samples needed the new barrier for 12.014 ms and 10.197 ms; the other six observed an
empty group immediately. The bound is not a longer timeout hiding the old failure: without the barrier the false
observation is final and 20 s of further polling cannot settle the attempt. Earlier runs with 6 burners (8 single-test
runs and 8 full-binary runs), and a 1 s pause between staffing and task creation (3 runs), did not reproduce it.

Evidence: `.hoh-dev/self-tests/m3flk/flk11-binary-heavy-before-{01..04}.log`,
`flk11-binary-heavy-after-{01..08}.log`, their `*-results.json` command/exit/duration/hash records, and
`flk11-green.patch` under the same directory. Self-tests, not acceptance. Similar single-sample helpers remain in
`tests/world/mod.rs` (`Team::hold`), `tests/s5/mod.rs`, `tests/tui_pty.rs`, `tests/m3t10.rs` and recovery fixtures;
these are unconfirmed out-of-scope risks, not failures reproduced here.

## FLK-12 (fault injection bound to the failed turn)

Developer diagnostics from `7dec042f`, REQ-020 / AC-020-05. The old `FailEnd` armed a one-shot fault at the auth latch,
consumed it in `before_job("Runner")`, and failed the next `before_commit`. A read-only Runner request (such as
`runner_waiting`) consumed the arm without committing; the following job reset `failing`, so the turn's end committed
normally and the test failed at "no commit failed". No product failure was reproduced: the test failed to inject its
intended fault.

A 700 ms pause immediately after arming reproduced this **8/8** with 6 CPU burners. Replacing the pause by an explicit
latch barrier, completing two real read-only Runner requests through the same store before releasing the end,
reproduced it **3/3**. The regression keeps that barrier. A SQLite trigger in this temporary test database now aborts
only the target attempt's update to FAILED; unrelated transactions and reads cannot consume it. The settle hook
asserts the end rolled back (the attempt is still RUNNING with no failure_reason), removes the trigger, and preserves
the original 700 ms hold before recovery. The existing exact `runner_error` count, `cause: "turn"`, auth fact, fallback
epoch/session and no-pi-unblock assertions remain. The developer renamed the test to carry `test_020_05`; review
M3FK-01 showed that would add a fault-injection fixture (different given, no `next_retry_at` assertion) to TEST-020-05's
traceability, so the lead restored its original unmapped name
`a_turns_auth_failure_whose_end_failed_to_commit_is_still_the_turns_block`.

With the same 6 CPU burners and read-request barrier the rate is **3/3 before, 0/20 after**. The complete
verification_dispatch binary also passed **33/33 twice** under 6 burners. A subsequent static check rejected `expect`
in the new helper methods; explicit Result handling and diagnostic assertions replace those calls without allowing
the lint or changing the fault/barrier. The final helper revision is rechecked before delivery.

Evidence under `.hoh-dev/self-tests/m3flk/`: `flk12-pause-before-*`, `flk12-barrier-red-*`, `flk12-barrier-green-*`,
`flk12-binary-regression-*`, `flk12-final-green-*`, `static-before-commits-*` and `static-fixed-*`; the patch files retain
the red reproductions, and each `*-results.json` records argv, exit, duration and raw-log digest. The green output
contains the actual injected SQLite error before recovery. These are scripted-agent product self-tests, not live
agent evidence or independent acceptance. No production source, AC, requirement, check plan, baseline or gate changed.

## M3-T05 pre-QA gaps G2, G4, G5

Run book: `.hoh-dev/evidence/m3t05-prep/runbook.md` section 4. G2 (Codex): the optional ignored pre-002 old-binary
diagnostic carried `test_008_02` in its name and wrote a TEST-008-02 SKIPPED line into the recovery check although
AC-008-02's scenario is drafting recovery; renamed so it maps to no TEST id ("M3 pre-QA: G2 unmap optional old-binary
diagnostic"); AC-008-02 stays covered by the two executed drafting-recovery tests, and refusing a newer schema by the
executed migration and db_faults tests named in its comment. G5 (Codex): AC-009-01 was covered at store level only; a host
test drives StartRun and a scripted Commander through the product host ("M3 pre-QA: G5 cover BUILD plan rejection through
host"); no product change. G4 (Claude): TEST-047-03 used one binary and never upgraded; `deployment.sh` now builds a second
release bundle v2 (developer contract and bundle version changed) and the new deployment test installs v1, upgrades to v2
under a running Run through the documented unit restart and checks per-attempt pins, contract digests and the drift report
("M3 pre-QA: G4 TEST-047-03 installs v1 and upgrades to a v2 release under a running Run"); ADR-034 carries a dated
correction of decision 12's single-binary fixture.

| Slice | Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|---|
| G2+G5 | 1 | `f49ed0d1` | Grok grok 1.0.92 (b06756fd6256), `01a20faa-f301-7682-9ca0-83eba96f5121` (`d2c6e881-64c5-48d3-bbb8-0b66287c66cd`) | PASS, no findings |
| G4 | 1 | `0b231a98` | Codex codex-cli 0.171.0, `01a20fab-8b8c-7840-8897-f3d9c0fb51f3` (`1f21d8f2-6cdf-4321-9b82-00c08caf5830`) | FAIL: M3PD-01 high (the three-way diff was checked against the product's own `three_way_diff`, so an empty-diff regression passed; the retained single-binary test likewise) |
| G4 | 2 (fix diff) | `5566c274` | Codex codex-cli 0.171.0, `01a20fd4-c3cf-72b3-b3b0-3d19cd4b603d` (`5bd964b9-e6ed-44c5-8dbd-be40acc3e74d`) | FAIL: M3PD-50 high (the hand-written diff applier accepted a trailing bare '+'), M3PD-51 medium (hunk ranges accepted '+1') |
| G4 | 3 (fix diff) | `5f38de52` | Codex codex-cli 0.171.0, `01a2101b-979c-7a12-9ab5-af874145111e` (`a08c2f53-5cb9-442e-9ba0-215670fab569`) | PASS, no findings |

Round 1 fix (Claude, red with empty and swapped diffs): "M3 pre-QA: M3PD-01 assert TEST-047-03 three-way diffs against an
independent oracle". Round 2 repeated the class (the diff oracle was not a strict independent expectation), so the lead wrote
design correction DC-PD before fixing: no parser; exact equality with an expected text the test spells out from the output
format (headers, one whole-file hunk, every old line '-', every new line '+'), after asserting the fixture inputs are non-empty,
newline-terminated and different, calling no hoh_core diff code. Fix 2 (Claude): "M3 pre-QA: M3PD-50 M3PD-51 compare TEST-047-03
three-way diffs with exact expected text", red through `deployment.sh` (exit 101) for empty, swapped, trailing '+' and signed-range
sabotage. Deferred low: M3PD-L1 `host/product/tests.rs` `test_047_03_drift_persists_three_way_object` (product check) still
computes its expectation with `three_way_diff` (the function has its own unit test). Final checks: `bash scripts/dev/pre-pr.sh`
all ok on `5f38de52` (G4) and `pre-pr --no-deployment` all ok on `f49ed0d1` (G2+G5); combined onto main `7dec042f`, whose
M3LC slice made `Bundle`'s fields private, the G4 test's `release_bundle` helper was adapted mechanically by the lead to
`set_release`/`retain_files`/`release()` ("M3 pre-QA: G4 use Bundle accessors after main made its fields private"; same reads,
same assertions); for the combined branch see the PR.

## FLK-11 and FLK-12 review

| Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|
| 1 | `558ce2f5` | Claude 2.1.331 (Claude Code), `d7623a41-b544-4632-b5f0-851cf3725a0c` (`383736bf-dc16-4f54-8685-b24a0da82f03`) | PASS: M3FK-01 low (the rename to `test_020_05_…` added a fault-injection fixture to TEST-020-05's traceability) |

M3FK-01 fixed by the lead ("M3 cleanup: M3FK-01 keep the FLK-12 test out of TEST-020-05"): the test's original unmapped name is
restored; nothing else changed. Deferred low: M3FK-L1 other fixtures that sample `group_empty()` once (`Team::hold`, s5,
tui_pty, m3t10, recovery fixtures), unreproduced risk, to the M3 low package.

## L11 lead decisions (2026-11-24)

- M3G-53: kept as the documented residual of ADR-034 d9 (no relaxation of the cross-version repack rule; AC-045-01 unchanged).
- M3K-07: resolved in L5 (comment; never reaches a Message reason_code; no CR).
- M3B-07: resolved by CR-036.
- M2R-501: resolved by CR-036's correction section.
- M2QF-02: accepted as documented (ADR-021 2026-11-21 note: SIGQUIT disposition depends on whether an agent turn started); no code.
- M2R-500-fu: recorded residual within the owner's CR-011 option A (proc mode is declared best-effort); nothing beyond it is
  proposed, so no owner question.
- M2R-1003: recorded residual: report independence is judged at intake; a later resume into a verifier's session is not
  re-judged. Recommended follow-up (not a gate change): re-judge at candidate acceptance in the next verification task (with
  M3LD-100's CR). Owner question only if that re-judgement would change a release rule.
- CLT-1260 (process-level memory measurement): moved to M4-T03 (observability and capacity), which owns resource measurement ACs.
- BT-03 (run_check.py queueing on the machine test slot): stays open; it changes a locked gate tool's behaviour without being a
  spec inconsistency, so it is not a CR-003 D1 refinement; asked of the owner in the next owner batch; meanwhile frozen checks
  run with the slot taken by the caller.

## CR-038 / M3LD-100 (execution record `started`)

Developer: Codex (CR draft and implementation), fix rounds by a fresh Claude session and a fresh Codex session. M3LD-100
(medium, pre-existing): exit 127 meant both "never started" and "ran and exited 127", all FAIL/NOT_STARTED, so a real 127
failure followed by a pass was PASS, not FLAKY. CR-038 adds a required `started` to the execution record: NOT_STARTED only
for started=false (exit 127, not timed out, no artifacts, duration and interval within 5 s, else INVALID); started=true with
127 is an ordinary FAIL; a record without the field is INVALID; pending facts written before the upgrade are recorded
RECORD_INVALID. Approved by the CTO under CR-003 D1 after round 3 ("M3 cleanup: approve CR-038 and relock baseline rev 26").

| Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|
| 1 | `6c9b89f2` | Claude 2.1.331 (Claude Code), `43d41e7d-38af-4cc9-965b-922e2fbabbfe` (`68f9d25a-2a3e-4b4b-9e53-3be4dd53dd31`) | PASS: M3LS-01 medium (the gate's exec-failure marker was a file the checked command could write, forging started=false), M3LS-02 low (pre-upgrade pending facts without `started` became Damaged), M3LS-03 low (docs) |
| 1 | `6c9b89f2` | Grok grok 1.0.92 (b06756fd6256), `01a21057-72cf-7102-9436-5ab4cda170c9` (`0eda5ebb-8d1d-49d4-a733-23de78b081ec`) | PASS: M3LS-50 medium (a signal during or right after Popen escaped the cleanup; the child group kept running), M3LS-51 medium (= M3LS-01) |
| 2 (fix diff) | `ca12180c` | Grok grok 1.0.92 (b06756fd6256), `01a2108d-b32c-7c21-b734-f8b638802e72` (`39c82cb7-5981-4bc7-ad95-8cf803c1a4f3`) | PASS: M3LS-100 medium (the inheritable report socket leaked into host spawns that bypassed the spawn lock) |
| 2 (fix diff) | `ca12180c` | Codex codex-cli 0.171.0, `01a2108d-b665-79b0-9d5f-71533b027599` (`20f539e3-bbf6-4dfc-b9c3-2de4181990bd`) | PASS: M3LS-150 medium (a signal between the two handler swaps left SIGINT replaced), M3LS-151 medium (only the first held signal was replayed), M3LS-152 medium (traceability fixtures lacked the newly imported release_gate.py) |
| 3 (fix diff) | `b2090e67` | Claude 2.1.331 (Claude Code), `4bf785f9-298f-4e4c-8f81-540256cf0b19` (`4a57525d-2770-4f69-997b-91df18335fb8`) | PASS: M3LS-200 low (an inherited SIG_IGN for SIGINT is replaced) |
| 3 (fix diff) | `b2090e67` | Grok grok 1.0.92 (b06756fd6256), `01a210cd-1393-7963-80af-93de306105f7` (`fc0bc90a-e863-4073-b57b-9698bc829493`) | PASS, no findings |

Round 1 fixes (Claude, red-first): "M3LS-01 M3LS-51 report gate exec failure through a close-on-exec descriptor", "M3LS-50 end
a check interrupted as it is spawned and record it started", "M3LS-02 record pending facts written without started as
RECORD_INVALID", "M3LS-03 document the interrupt and gate exec report corrections". Round 2 repeated both classes (a forgeable
not-started report; a signal window), so the lead wrote design corrections before fixing: DC-LS-A, no inheritable report
descriptor exists anywhere in the host at any time (the gate receives its report end with `go` over its private control
channel by SCM_RIGHTS with MSG_CMSG_CLOEXEC; the spawn lock, `inheriting()` and `--exec-error-fd` removed; any delivery failure
keeps started=true); DC-LS-B, handlers installed once by run_check only record signals, in order, and `execute` polls that
record at fixed points (no handler swapping, nothing raised from a handler). Fix round 2 (Codex, red-first): "M3LS-100 deliver
the gate exec report over SCM_RIGHTS close-on-exec", "M3LS-150 M3LS-151 record interrupts without swapping handlers in
execute", "M3LS-152 copy release_gate.py into traceability fixtures". Round-1 lesson: a DRAFT CR's bundle suite was reported
"red only on the unrelocked baseline" without checking; M3LS-152 hid behind it. Later rounds ran the bundle suite after a
relock in a scratch copy. Deferred low: M3LS-200 (M3 low package). Final checks: see the PR.

## FLK-13 (a concurrent spawn holds the gate's end of the control channel)

Developer diagnostics from `116e3585`, REQ-052 / AC-052-04, test
`test_052_04_delivery_receive_and_read_failures_do_not_prove_not_started`. Seen once in a parallel `cargo test --workspace`
(`channel.deliver().is_err()` false). Chain: `Channel::new` creates the control socketpair close-on-exec, the test drops the
gate's end and expects `deliver` to fail with the peer gone. Another test thread in the same binary that spawns a process
inside that window gives its child a copy of the gate's end, held until the child's exec; the peer is still open, so the
`go` packet is queued and `deliver` succeeds. No product defect: the test assumed that no other thread forks.

Reproduction (temporary, not committed): an ignored test in the same module runs the real test function 20000 times while
N threads spawn `true` in a loop, counting failures by panic location. **Before**: 1037, 1100 and 1067 failures in 20000
with 8 spawners (all of the last two at `assert!(channel.deliver().is_err())`), 0/20000 with no spawners. **After**: 0/20000
three times with 8 spawners, 0/20000 with 16 and 0/20000 with none. The test now waits, at most 10 s, until the control
socket reports POLLHUP (the kernel sets it only when the peer socket is released, i.e. every copy is closed, as when the
gate has exited) before calling `deliver`; on timeout the test fails. Its assertions are unchanged.

Product: the same window exists when the host spawns other processes while a gated check starts, and it stays conservative.
The workspace forbids `unsafe` and has no `pre_exec` or fork without exec, so a stray child holds copies of the control,
reader and writer ends only until its exec (or `_exit`) and never reads or writes them. Effects: if the gate died before
delivery, `deliver` may succeed instead of failing, so the "cannot deliver" stderr line and an already moot cancel are
missed, and the record still says started=true; a stray copy of the writer can delay EOF but cannot produce the exact
`exec_failed\n` report, and `exec_failed` treats EAGAIN like EOF; a stray copy of the control end delays the gate's EOF
only when the host abandons a gate it is already cancelling. No path turns a started command into NOT_STARTED.

Evidence under `.hoh-dev/self-tests/m3low2/`: `flk13-repro.patch` and `flk13-fix-plus-repro.patch` (harness on HEAD and
on the fix), `flk13-{before-01,before-02,before-03,before-nospawn,after-01,after-02,after-03,after-nospawn,after-16spawners}.log`
with their `*-result.json` (argv, binary digest, exit, duration, log digest); before-01 ran an earlier harness revision
without the panic-location count. Self-tests, not acceptance.

## M3 low package 2 (M3PD-L1, M3FK-L1, M3LS-200 with CR-039) and FLK-13 review

Developer: Claude (package and FLK-13, separate sessions). M3PD-L1: the packing test's drift diff is compared with an exact
expected text the test spells out ("M3 cleanup: M3PD-L1 spell out the expected three-way drift diff in the packing test", red
with an empty-diff sabotage). M3FK-L1: one shared bounded group-exit wait (`world::group_exit`, Team::review's 5 s bound)
replaces single `group_empty()` samples where a single false sample could make a fixture report a live group (Team::hold, s5,
tui_pty, m3t10, recovery start_cancel, drafting); sites asserting a product guarantee or a live group keep their single sample
("M3 cleanup: M3FK-L1 report scripted groups only after the shared group-exit barrier", "M3 cleanup: M3FK-L1 reach the
group-exit barrier from m3t10 through the world module"). M3LS-200: an inherited SIGINT ignore is kept by run_check ("M3
cleanup: M3LS-200 keep an inherited SIGINT ignore in install_interrupt_handlers"; tools/common.py is locked, so CR-039, approved
by the CTO under CR-003 D1 after review: "M3 cleanup: approve CR-039 and relock baseline rev 28").

| Slice | Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|---|
| package 2 + CR-039 | 1 | `116e3585` | Grok grok 1.0.92 (b06756fd6256), `01a21161-52c7-7013-8895-be194d0fc0ac` (`3fe8814f-a341-4b32-9b2d-e0e29144accf`) | PASS, no findings |
| FLK-13 | 1 | `1dc8b789` | Grok grok 1.0.92 (b06756fd6256), `01a211b9-ca15-7ab0-8843-95567c0c505c` (`7c927095-ecd1-4fba-990d-528f188ffc51`) | PASS, no findings |

BT-03 (run_check.py and the machine test slot), decided 2026-11-25 under the owner's delegation ("pending owner decisions take
the lead's recommendation"): tools/run_check.py stays unchanged (the slot is a development-machine convention that product
environments do not have); callers hold the slot (`flock /home/ai/dev/.hoh-test-slot.lock python3 tools/run_check.py …`), so
waiting for it is not part of a check's recorded duration; docs/19 records the rule. Deferred low: none new. Final checks: see
the PR.

## M3QA-018 (per-test result lines lost from check logs)

M3-T05 QA round 2, high, REQ-053: traceability reported TEST-062-01 NOT_RUN although it passed, and the v0.1 gate failed
nondeterministically. QA evidence: integration `stdout.log:9287` `hoh host: origin lookup returned none: stage=capture tcp
port=53426: tcp_match missingtest test_062_01_settings_busy_writer_responsive_reads ... ok`; the rerun lost another test the
same way (`integration-rerun1/stdout.log:7304`); deployment `stdout.log:7-8` `test
resource_deployment::test_047_03_bundle_pinned_until_checkpoint ... hoh host: verification reconciled host_epoch=3`, then `ok`
on the next line.

Chain: scripts/product/cargo-tests.sh, recovery.sh, tui-pty.sh and deployment.sh ran `cargo test … 2>&1 | tee "$log"` and
parsed `$log` with test_results.py. libtest's pretty formatter writes a result line in several flushed writes to stdout
(`test X ... `, then `ok`, then the newline); host processes the tests start inherit the test binary's stderr and write
their diagnostics with Rust's unbuffered stderr, the message and its newline in separate write(2) calls. With `2>&1` both
reach one pipe, so a diagnostic can land inside or just before a result line, which then no longer starts with `test ` or
ends in ` ... ok`; test_results.py (correctly) does not recognise it and the TEST stays NOT_RUN.

DC-QA018 (design correction). Wrong assumption: one log stream can carry both libtest's result lines and arbitrary
diagnostics of the processes under test. It cannot: line integrity holds only for a stream with one writer that writes whole
lines. Correction: the parsed log is libtest's stdout only. The scripts pipe only cargo's stdout into `tee "$log"` and leave
cargo's stderr (compiler output and errors, `Running …` headers, `error: test failed`, and every inherited child diagnostic)
on the script's own stderr, which tools/run_check.py records as the check's `stderr.log` beside `stdout.log`; nothing a
parser or the scripts' `test result:` / `^test … ok$` greps need is on stderr (libtest writes result lines, failure
sections `---- X stdout ----` and `test result:` summaries to stdout). Exit codes are unchanged: `set -o pipefail` with
`|| rc=$?` still yields cargo's status, so a build failure fails the check and its errors stay visible on stderr. A child
that inherits the test binary's *stdout* would still interleave, so the fix includes an audit for test-spawned processes
writing to an inherited fd 1 (recorded below). test_results.py does not guess at damaged lines: a lost line stays NOT_RUN.
static.sh merges clippy's stderr into `clippy.log`, which no parser reads for test results, so it stays as it is.

fd 1 audit. Static: no product `println!` on a host path (the direct `io::stdout()` writers are `hoh __wal-probe` and `hoh
__unit`, protocol outputs their parents pipe); the inherited-stdout spawns in crates/hoh-app/tests that were read start
quiet children (`sleep`, `sh -c 'read …'`, `tar -x`, `true`) or set stdout through a helper, and the deployment suite's
children are piped, nulled or write to a log. Dynamic: the whole `cargo test --workspace --locked --no-fail-fast` with stdout
and stderr in separate files (exit 0): its stdout held 2932 result lines, equal to the `test result:` totals, while 13829
`hoh host:` diagnostics went to stderr; every stdout line but one is libtest's or rustdoc's own. The exception was
`{"reasons":["worktree dirty: x"],"status":"refused"}` from `run_check_asks_for_its_check_and_ends_with_its_exit_code`
(crates/hoh-app/src/host/requests/tests.rs), whose second `run-check` helper inherited the test binary's stdout: it is
piped now and its answer asserted, like the first's. Evidence under `.hoh-dev/self-tests/qa018/` (`audit-workspace.*`,
`green-requests.*`, `qa018-classify.py`, `qa018-scan.py`, `red-bundle.log`, `green-bundle.log`, `pre-pr.log`).
Self-tests, not acceptance. Other merged streams: adapter-live.sh writes each live test's output with `2>&1` to its own
log, but its results come from the verdict files the tests write, never from that log; static.sh's `clippy.log` is not
parsed for test results. Both stay as they are.

### M3QA-018: fix review (fresh Pi session, attempt 3a30276d-ad69-40c1-8ce2-f1b3f5e7ce09)

- Scope: `ab3e92ce..7b5d1ff4` on the frozen worktree; bundle tests and shellcheck in a copy. Verdict: **PASS** (3 low).

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| M3R-1001 | low | No durable guard against the same root cause returning (a test child inheriting fd 1); the stdout classifier of the fd-1 audit lives only in `.hoh-dev`. A future offender gives a fail-closed NOT_RUN again, never a false PASS. | Deferred to the M4 low package: a test-Command helper with piped/null stdout by default, or the classifier as a bundle check. |
| M3R-1002 | low | The fd-1 audit's list of direct `io::stdout()` writers misses hoh-client's CLI output (`lib.rs:68`, `follow.rs:159`, `doctor.rs:468`); the conclusion holds since tests run the CLI via `.output()` or a PTY. | Noted here: CLI output belongs to the "parent pipes it" class. |
| M3R-1003 | low | recovery.sh's second run takes its pipe status from `systemd-run --user --scope`, not cargo (pre-existing, unchanged by this diff); whether systemd-run propagates cargo's non-zero exit is not evidenced. | Deferred to the M4 low package: show a failing cargo under the scope fails the check. |

### M3QA-005 / M3QA-006: scenario tests observe every Then clause (task/M3-qa005-006-scenario-tests)

- Findings (M3-T05 QA round 2, medium): the scenario-check tests of AC-008-01 (`tui_pty.rs` `test_008_01_running_plan_pause_resume_approval_review`) and AC-008-04 did not observe every Then clause.
- Fix (developer Claude session, attempt 3d0baeb6-b4d9-4f45-b899-c1b3fb78ce3c; test-only, no product change): `tui_pty.rs` now observes the PLAN safe point, PAUSE_REQUESTED before PAUSED, the return to RUNNING before drafting, `spec_review.opened` while RUNNING and the full Git write set; new `drafting.rs` `test_008_04_plan_rejects_writer_plan_and_draft_product_writes_block_with_their_paths` runs AC-008-04 end to end in the product host (Commander-bridge plan rejected, the drafting agent's own tracked/untracked/deleted writes, `plan.write_violation` with exactly those paths, the question listing them, no CR, no new revision). Eight reds shown by temporarily breaking the asserted product behaviour; equality assertions without a red are listed in the developer's per-clause table. pre-pr exit 0.
- Review (fresh Grok session, attempt a603096d-0d2a-4c5a-9c47-59a00577b9ed, session 01a214c2-d1f7-7de3-bff0-9b517ba038d1): **PASS**, no findings; every Then clause of both ACs checked against the asserting lines; the diff is test-only.

## M4 low package 1 (M3QA-003, M3LE-01, M3LE-02)

Developer: Grok, branch `task/M4-lowpkg1` from `072f9fa6`. Three lows carried from M3. M3QA-003 and M3LE-02 are fixed
red-first, each in its own commit. M3LE-01 is analysis only: contracts and behaviour are unchanged. The full
`bash scripts/dev/pre-pr.sh` follows this record and is not claimed here. The logs below are self-tests, not acceptance.

### M3QA-003

`scripts/product/traceability.py` read a relative `HOH_EVIDENCE_DIR` and passed it to the TEST-032-01 product step.
`crates/hoh-app/tests/trace.rs` `write_exports` writes those exports with the test binary's cwd, which cargo sets to
`crates/hoh-app`, so a relative path was `NotFound` there (Os error 2). A planned check is unaffected:
`tools/run_check.py` resolves the evidence directory to an absolute path before it starts the script.

The script owns the value. `evidence_directory` resolves a relative path once against the cwd of the process that read
it, and rewrites `HOH_EVIDENCE_DIR` only in that case, so the child sees the same directory. An absolute path is
returned unchanged and the variable is not rewritten, so the files a planned check records stay the same. Commit
`73083607` ("M3 cleanup: M3QA-003 resolve a relative HOH_EVIDENCE_DIR at the script"). ADR-039 DC-T07b notes the same
rule.

Red, before that change (`.hoh-dev/self-tests/m4low1/m3qa003-red.log`): 
`test_a_relative_evidence_directory_is_written_from_the_invocation` failed because the product-step stub could not
create `rel-evidence/marker` (`FileNotFoundError: [Errno 2] No such file or directory: 'rel-evidence/marker'`), and the
script reported the product step failed (1), TEST-032-01 FAIL. Green, after
(`.hoh-dev/self-tests/m4low1/m3qa003-green.log`): `python3 -m unittest discover -s tests/bundle -p test_trace_export.py`
ran 9 tests, 1 skipped, OK in 8.137s. The marker landed under the invocation directory, and the absolute-path
product-step test still passed.

### M3LE-02

`ApproveSpec` and `ProposeSpecChange` read the spec sources on `spawn_blocking`. A `JoinError` (the blocking thread
panicked or was cancelled) answered HTTP 503 with code `UNPROCESSABLE`, `retryable=true`, and `current_revision` null.
The approval-input path already maps the same join failure to `SpecReadError::unavailable`, and `store_error` already
maps `PersistError::SpecSourceUnavailable` to 503 `SPEC_SOURCE_UNAVAILABLE`. CR-036 decision 1 is that an unavailable
spec source is that retryable 503, with the DB_BUSY error shape (`code`, `message`, `retryable`, `current_revision`),
and nothing is written. Post-commit index publication (`publish_spec`) stays the existing retryable 503
`UNPROCESSABLE` (CR-036 decision 3) and was not changed.

There was no seam that produces a `JoinError` without a panic. `HostHooks::fail_spec_source_read` defaults to false
(`NoHostFaults` is unchanged). When a test returns true, the read panics inside that `spawn_blocking` closure; tokio
turns the panic into a `JoinError`, and the arm now goes through
`store_error(PersistError::SpecSourceUnavailable)`. Commit `39cd199f` ("M3 cleanup: M3LE-02 map a spec-source join
failure to SPEC_SOURCE_UNAVAILABLE").

The test name is `spec_source_join_failure_answers_spec_source_unavailable` and is not `test_NNN_MM`, so traceability
does not record a new TEST id (M3FK-01). It arms the hook for one read of both `ApproveSpec` and `ProposeSpecChange`,
and expects 503, `SPEC_SOURCE_UNAVAILABLE`, `retryable=true`, the CR-036 message prefix, `current_revision` equal to
the command's `expected_revision`, the four-field error object, an unchanged snapshot, no `spec.index_failed`, then
202 on the same command and a replay that does not write again.

Red, mapping still `UNPROCESSABLE` (`.hoh-dev/self-tests/m4low1/m3le02-red.log`, exit 101): the join panicked with
`injected spec-source join failure`, the body was `{"code":"UNPROCESSABLE","current_revision":null,"message":
"specification read stopped: task 26 panicked with message \"injected spec-source join failure\"","retryable":true}`,
and the assertion failed on the code. Green (`.hoh-dev/self-tests/m4low1/m3le02-green.log`, exit 0): that test passed
in 1.74s. The six `unavailable` tests in the same binary, including the new one and the existing 422 invalid-index
test, passed (`.hoh-dev/self-tests/m4low1/m3le02-unavailable-regression.log`, exit 0, 6 passed). Both runs were
`flock /home/ai/dev/.hoh-test-slot.lock cargo test --locked -p hoh-app --test spec_source` with the test name, then
with the filter `unavailable`.

### M3LE-01

No code or contract change. ENOENT on the spec root, and a workspace that is not a git repository, also answer the
retryable 503 `SPEC_SOURCE_UNAVAILABLE`, and they are not transient. The current mapping matches the approved wording.
Splitting the class needs its own CR.

What the code does. `host/spec.rs` `sources` calls `symlink_metadata` on `docs` and `docs/spec` and maps every I/O
error, including `NotFound`, to `SpecReadError::Unavailable`. If the path exists and is not a directory, that is an
`IndexError` ("expected a directory, not a link") and stays 422 `SPEC_INVALID`. A missing `docs/spec/index.json` is
not this case: `files` treats `NotFound` on the index as "no index yet". `files` then runs
`git ls-files -z -- docs/spec/index.json`. `control::git` discards stderr and, on a non-zero status, returns
`git <args>: <status>`. In a directory that is not a repository, `git ls-files` exits 128, so the client message is
the status line (`exit status: 128`), not git's stderr phrase "not a git repository". A failure to spawn `git` is the
same `Unavailable`. Both become `PersistError::SpecSourceUnavailable` and `store_error`'s retryable 503. Nothing is
written and `spec.index_failed` is not recorded. No test in this tree drives ENOENT on `docs` or `docs/spec`, or a
non-repository.

Which wording covers them. CR-036 decision 1: a Git enumeration failure or a filesystem read failure, on
`ApproveSpec`, `ProposeSpecChange`, or the read that answers a spec-approval question, is HTTP 503,
`retryable=true`; the command is not accepted; no CR, revision, or `spec.index_failed` is written. Decision 3 keeps
genuinely invalid specs on 422 `SPEC_INVALID`. ADR-033 §1 states the same split: enumeration or filesystem read
failures are `SpecReadError::Unavailable` and 503 `SPEC_SOURCE_UNAVAILABLE`; illegal syntax, links, and non-regular
files stay non-retryable 422. ENOENT on `docs` or `docs/spec`, exit 128 from `git ls-files`, and a missing `git`
binary are read or enumeration failures under that wording. The code follows it.

What a class split would be. Three classes, not one:

1. Transient unavailability: permission denied on a file that exists, I/O errors, and the `JoinError` fixed as
   M3LE-02. These stay retryable 503 `SPEC_SOURCE_UNAVAILABLE`.
2. The spec root is absent (`docs` or `docs/spec` does not exist). That is a permanent shape, closer to the existing
   "expected a directory" `IndexError` (422 `SPEC_INVALID`, non-retryable) than to a lock or a permissions blip.
3. The workspace is not a git repository (exit 128), or `git` cannot be spawned. That stays permanent until the
   repository or the binary exists. It is not invalid specification content, so 422 `SPEC_INVALID` would mis-label
   it the way M3B-07 rejected for I/O errors.

CR-036 ties `SPEC_SOURCE_UNAVAILABLE` to `retryable=true` (decision 1, and the client keeps exit 69 for 503). Setting
`retryable=false` on the same code would contradict that decision. A new reason code, or moving class 2 or 3 to 422,
changes what clients retry and touches the locked protocol schema, OpenAPI, ADR-033, and CR-036, so it needs its own
CR and a baseline relock if an `ErrorCode` is added. It is not a CR-003 D1 "add a missing code" change: the code
exists and the approved text already classifies these failures as retryable. This package does not implement the
split.

### M4 low package 1: review (fresh Claude session, attempt daf3a4f4-3ad0-4dfb-b20f-ddfb6abdd663)

- Scope: `072f9fa6..0e8c66d6` (M3QA-003, M3LE-02, M3LE-01 analysis). Verdict: **FAIL** (1 high, 1 low).

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| M4L1-001 | high | `tests/bundle/test_trace_export.py:171`: the M3QA-003 change created the ProductStep temp dirs under the hard-coded `/home/ai/dev`; on the CI runner (and any other host) setUp raises FileNotFoundError and the bundle job fails. | Fixed: tempfile's default (TMPDIR) again. Same class as M3L-101 / PR #108 (a Rust test naming the developer's /home), so per docs/19 the design correction comes first: **DC-M4L1-001**, the static machine-path scan, which covered only `*.rs` under `crates/`, now covers Rust, Python and shell sources under `crates`, `tests`, `tools` and `scripts`. Red: the scan finds the old line (exit 0 → static fails); green: no match (`.hoh-dev/self-tests/m4low1/m4l1-001-scan.log`). |
| M4L1-002 | low | `http.rs:984-997`: a JoinError still answers before the store, so Reject/replay/stale-revision/missing-run get 503 SPEC_SOURCE_UNAVAILABLE rather than their own answers (pre-existing shape; needs a blocking-thread panic; CR-036 holds). | Deferred to the M4 low package: map the JoinError to `files=Err(SpecReadError::unavailable(..))` and let the store decide, as inputs.rs does. |
