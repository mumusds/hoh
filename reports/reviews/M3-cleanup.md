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
- CI (PR #131, product job) then failed the new `test_008_01` assertion: the last run state event before the draft's dispatch was the ResumeRun command's own `run.resumed` (its payload names the command, not the state), an order the assertion did not allow; locally a release's `run.resumed` to RUNNING came later. The lead's follow-up (`M3 cleanup: M3QA-005 the draft's dispatch may follow the ResumeRun's own run.resumed`) accepts either event and asserts `resume_state` RUNNING at the pause, so the run is still shown RUNNING at the dispatch; 3/3 local runs and CI green. Lesson: an event-order assertion lists every order the product may legitimately produce, from the code, not from one observed run.

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

### M4 low package 1: fix re-review (fresh Claude session, read-only)

- Scope: `0e8c66d6..904ba27e` (M4L1-001 fix, DC-M4L1-001 scan). Verdict: **PASS** (2 low).

| ID | Severity | Summary | Disposition |
|---|---|---|---|
| M4L1-100 | low | The scan misses unquoted shell paths, `/home/ai` without a trailing slash and other users' homes; `scripts/dev/pre-pr.sh:43` defaults the dev slot to `/home/ai/dev/.hoh-test-slot.lock` (dev-only, overridable by `HOH_TEST_SLOT`, not run in CI). | Deferred to the M4 low package: widen to `/home/<user>` with an explicit allowlist naming pre-pr.sh's slot default. |
| M4L1-101 | low | `.toml`, `.yml`, `.json` sources are not scanned. | Deferred to the M4 low package. |

## DC-M3CX-500: pipe reads must let control signals run

M3R-503 and M3CX-500 repeat the same defect class: an always-readable pipe consumes Tokio's cooperative budget;
a read-first biased select then polls its abandon watch only after that budget is exhausted. A supervisor that has
killed its group can still wait forever for a pipe held by an escaped descendant.

Design rule: **every loop that reads a pipe while racing an abandon, cancel or deadline signal yields after each
successful read, inside the read side.** Save bytes, counters, raw-log writes and retained tails before yielding so
that dropping the read future loses no consumed data. Drain loops check abandon first as well, including when the
previous read spent the last cooperative operation. EOF and errors terminate without another read. This is a bound
on reads/work between control polls, not a wall-clock bound on synchronous logging or scheduling.

Audit scope: all `select!`, `read_buf`, `read`, `read_line`, `read_until`, stream-next and process-output loops in
`crates/hoh-agents/src` and `crates/hoh-app/src`; the audit and deterministic cooperative-reader tests precede the fix.

| Audited loop / call chain | Result |
|---|---|
| `frame.rs::next` ordinary reads | Already saves state and yields after each successful read. |
| `frame.rs::skip_more` / `reduce_more` | Yield inside the read side also for the LF-bearing chunk; keep buffered input across dropped futures. |
| `process.rs::read_stdout` framing | Calls cancel-safe `FrameReader::next`; bounded channel backpressure is intentional. |
| `process.rs::read_stdout` refused-frame drain | Fixed: abandon first; count discarded bytes before read-side yield. |
| `process.rs::drain_stderr` | Fixed: abandon first; raw copy, byte count and tail saved before read-side yield. |
| `process.rs::supervise`, leader wait / drain grace | No pipe reads; signal, timer and stream joins. Stream joins rely on the fixed drains. |
| Codex App Server / exec, Claude, Pi driver selects | Consume bounded `Process::next` events, not pipes; cancel and due-watch are before output. |
| `hoh-app::verify::check` | Consumes `Process::next` events; its actual pipe readers are the fixed substrate. |
| `hoh-agents::probe::capture` / `Output::drain` | Synchronous nonblocking reads, no Tokio coop/select; explicit output cap and outer deadline/cancel checks. No cooperative starvation. |
| `hoh-app::host::control::git_run_limited` | Blocking reader thread joined after group termination; not a Tokio read/signal race. Escaped Git descendants holding stdout are a pre-existing separate lifetime risk, outside this CR. |
| Remaining hoh-app selects (`runner`, `inputs`, `control`, `mod`, `product`, `http`) | Timers, channels, joins or HTTP sockets; no direct pipe read loop. |
| Remaining read loops (probe/schema, adapter hashes; app gate, drafting, cgroup, product, untracked files) | Bounded regular-file/proc reads or test fixtures; not async pipe/control races. |

Synthetic cooperative readers reproduce both drain failures (512 total reads at the safety fuse), then assert abandon
within one drain read and preserved counters/tail. Frame cancellation/restart tests cover ordinary, skip and reduction
paths. Evidence: `.hoh-dev/self-tests/m3cxfrm/fix1-500-*.log`; all are self-tests, not acceptance.

### CR-041 round-1 fix disposition (M3CX)

Input: `.hoh-dev/self-tests/m3cxfrm/review1-findings.json`, candidate-3 / `62e8d689`.
The branch stays DRAFT and unrelocked; the owning commander retains independent review and approval.

| Finding | Disposition and diagnostic evidence under `.hoh-dev/self-tests/m3cxfrm/` |
|---|---|
| M3CX-500 medium | Fixed under DC-M3CX-500 above. `fix1-500-red.log`: both cooperative drains starved. `fix1-500-green.log`: 5 process tests pass; `fix1-500-frame-green-2.log`: all 8 cancellation/restart tests pass. |
| M3CX-001 medium | Fixed: only frames proven over the normal limit enter reduction, with bounded allocation transfer. `fix1-001-002-red.log`: 605,206-byte frame refused. `fix1-001-002-green-2.log`: all 28 frame tests pass, including exact limit and limit+1. The existing reduction hash/escape test now uses an actually oversized input to retain its original purpose under the corrected threshold. |
| M3CX-002 medium | Fixed by the same delayed-reduction boundary as M3CX-001; a separate commit records EOF/invalid-JSON/invalid-route regressions. The same red log shows small Partial/Line misclassified as TooLarge; the same green log verifies byte-exact legacy output. Codex verdict ranking is unchanged. |
| M3CX-003 low | Fixed: declaration checks return Truth; budget/depth exhaustion is Unknown. `fix1-003-005-red-corrected.log` shows No on an exhausted budget; `fix1-003-regression.log` passes the declared/absent/exhausted schema cases. |
| M3CX-004 low | CR-041 decision 5 documents full-raw replay versus online bounded items; documentation only, no behavioral red test. |
| M3CX-005 low | Fixed: only command_execution items carry aggregated_output. `fix1-003-005-red-corrected.log` shows the spurious field on agent_message; `fix1-005-regression.log` passes Codex unit regressions. |

All test results above are synthetic/product self-tests, not live sessions or independent acceptance. Failed diagnostics
are retained: the initial M3CX-005 test had a Recorded-enum fixture compile error (not a red); the first frame-yield
regression found a redundant yield and was corrected; the first threshold implementation exceeded the old interruption
work bound and rescanned earlier bytes, so it was superseded by bounded buffered processing and incremental LF search.
Its exact registered test process was stopped; `fix1-stopped-superseded-test.json` records identity. No gate, AC or check
plan was changed. Final scratch-only approval/relock, full pre-pr and candidate-4 evidence accompany the handoff.

Supplemental M3CX-001 check before candidate-4: the delayed-reduction transfer must diagnose every already-read
byte of the refused frame, including input not yet moved through the reducer when its structure budget fails.
`fix1-001-diagnostic-red.log` reproduces length 528,384 instead of 1,048,577. The fix consumes the remaining buffered
current-frame bytes into the diagnostic hash/length, stopping at LF and respecting CRLF. `fix1-001-diagnostic-green.log`
runs the complete frame regression, including the new length/hash/head assertion. The first scratch pre-pr passed
shellcheck/bootstrap/bundle/static/build; it was superseded and stopped only while waiting for the workspace slot
(recorded PID/start plus exact flock child). The final source receives the full pre-pr in `pre-pr-4-final.log`; the
requested `pre-pr-4.log` retains the first attempt and appends the final run, with both identities in `checks.jsonl`.

The reduction cancellation fixture also signals after the 1 MiB threshold, retaining its reduction-path coverage now that the first three reads belong to ordinary framing. The original early-cancel cases remain.

### CR-041 round 2: fix-diff re-review and approval

Scope `62e8d689..0e742bef` (frozen detached copy at `0e742bef`), two complementary fresh sessions:

| Reviewer, session (attempt) | Ids | Verdict |
|---|---|---|
| Claude Code 2.1.331, `963b5d08-f566-4fc3-9d61-3664151abb8a` (`93b6fc46-0b6d-45fa-83ce-79328d3de033`): protocol and frame semantics | M3CX-600..699 | PASS: M3CX-001..005 closed (4 of the 5 new frame tests fail on `62e8d689`; differential and drop-and-resume experiments over random chunking show byte-identical output for frames at or under the limit) |
| Grok 1.0.92, `01a2156e-b53f-7f62-8c08-9e3a5dfbdab5` (`502cfd1c-64c6-48a8-b35d-f582b266e2dd`): concurrency, cancellation, process lifetime | M3CX-700..799 | PASS: M3CX-500 closed; the pipe-read audit is complete (three `read_buf` in frame.rs, two `read` in process.rs) |

| Finding | Severity | Disposition |
|---|---|---|
| M3CX-600 | low | Fixed (`M3 cleanup: M3CX-600 the exhausted resume declaration test traces to TEST-024-05`): the M3CX-003 regression is named `test_024_05_*`. |
| M3CX-601 | low | Fixed with the approval: docs/13 states that only frames proven over 1 MiB enter reduction and smaller frames keep their Line/Partial semantics. |
| M3CX-700 | medium | Outside the fix diff (docs/19: backlog): `crates/hoh-app/src/host/control.rs` `git_run_limited` joins its blocking stdout reader without a deadline after killing the group; a hook or filter that leaves the group and keeps stdout open would stall the calling thread. M4 low/medium backlog. |
| M3CX-AUDIT-GIT-DRAIN | low | Same as M3CX-700 (developer's audit note). |
| M3CX-OUT-001 | — | The developer's scratch pre-pr failed once in `test_029_03` on a strace `???(` line: FLK-15, fixed on `task/M3-flk14-15`; not a frame defect. |

No critical/high in round 2; the lead approved CR-041 under CR-003 D1 and relocked the baseline to revision 30.

## M4 low package 2 (M3-T05 QA round 2 lows)

Developer diagnostics from `ab3e92ce` on branch `task/M4-lowpkg2-observation` (no product code changed; all self-test red/green
and `pre-pr.log` evidence under `.hoh-dev/self-tests/m4low2/`, scripted/product self-tests, not independent acceptance).

| Finding | AC / REQ | Scenario check | Disposition | Evidence (`file:line`, red-first sabotage, commit) |
|---|---|---|---|---|
| M3QA-007 | AC-008-02 / REQ-008 | `recovery` | Fixed | `crates/hoh-app/tests/recovery/drafting.rs:135-421` (`given_plan`, `recovery`): asserts all three phases in `recovery` (`runtime:no_spec` text and `[adopt, draft]` choices, `CREATED -> DISCOVERING -> SPECIFYING` order with zero attempt/effect in `DISCOVERING`, `spec_draft` attributes `cto/1/NULL/IMPLEMENTED`, `git init` effect owned by drafting attempt, `spec_approval` question with `spec_sha256`, `ApproveSpec{APPROVE_ONLY}`, `spec.changed{cause:"approved"}`, R1 returning to `SPECIFYING` with supervisor trigger, R2 `draft` adopting rev 1 with only R1's null-spec task). Red-first: removed `"draft"` from `no_spec` choices in `adoption.rs` (`m3qa-007-red.log` exit 101; reverted; `m3qa-007-green.log` exit 0). Commit `f9666061`. |
| M3QA-008 | AC-008-05 / REQ-008 | `integration` | Fixed | `crates/hoh-app/tests/drafting.rs:369-419, 777-842`: adds marked + user `hoh -p approve --resume <R> --detach` on first draft, asserts derived `commands` `ApproveSpec{decision:"APPROVE_ONLY", spec_sha256}` on both rev 1 and rev 2 approvals, asserts `details.reason == "marker"` on all CR-stage marked requests, asserts zero product-write dispatches, and runs a FakeCommander turn emitting an `approve_spec` output while verifying zero revisions are produced. Red-first: changed `Origin::Marker` reason to `"marker_broken"` in `origin.rs` (`m3qa-008-red.log` exit 101; reverted; `m3qa-008-green.log` exit 0). Commit `c94538f1`. |
| M3QA-009 | AC-009-02 / REQ-009 | `integration` | Fixed | `crates/hoh-app/tests/drafting.rs:702-775`: interactive AUTO reads both `question.asked{reason:"spec_approval"}` (matching the proposed CR's `spec_sha256` and naming `approve`) and `GET /v1/runs/{run}` `projection.questions`; all modes assert zero non-draft writer attempts and zero `schedule.round` dispatches across all tasks, not only `work`. Red-first: changed `RunReason::SpecApproval` to `"spec_approval_broken"` in `need.rs` (`m3qa-009-red.log` exit 101; reverted; `m3qa-009-green.log` exit 0). Commit `2e2ce32e`. |
| M3QA-010 | AC-017-04 / REQ-017 | `tui-pty` | Fixed | `crates/hoh-app/tests/s5/mod.rs:7-109, 292-462`: removes direct `store.team_checkpoint`, `store.route_input`, `store.settle_ended_inputs`, `store.enqueue_input`, and test-copied `OutboxRow`; holds one steer while queuing a real epoch-1 `outbox` row via HTTP `SendInput`, lets the host runner apply `ConfigureTeam` checkpoint to `codex:2` after T1 settles, lets the host delivery/sweep loop reject both epoch-1 inputs (`late-outbox` and `late`) as `REJECTED(STALE_ROLE_EPOCH)`, and verifies HTTP-submitted epoch-2 inputs stay `QUEUED:2`. Red-first: changed sweep `STALE_ROLE_EPOCH` in `inputs.rs:732` to `"STALE_BROKEN"` (`m3qa-010-red.log` exit 101; reverted; `m3qa-010-green.log` exit 0). Commits `203e02d0`, `c3a5d772`. |
| M3QA-011 | AC-023-03 / REQ-023 | `integration` | Fixed | `crates/hoh-app/tests/m3t10.rs:472-619, 833-945`: checks supervision and persisted wake records (`events` `supervisor.woken` severity `high` count == 1 and `supervisor_triggers` `HANDLED` count == 1) for both project A (`max=2`) and project B (`max=3`), asserts persisted `discussion.conclusion` event (`controversy_id`, `conclusion.adr`), and in the Commander test asserts persisted `supervisor.woken`, `supervisor_triggers`, `decisions` (`APPLIED`, `record_assumption`, ADR ref) and `decision.applied`. Red-first: changed `"discussion.conclusion"` to `"discussion.conclusion_broken"` in `consult.rs:214` (`m3qa-011-red.log` exit 101; reverted; `m3qa-011-green.log` exit 0). Commits `83e83fdd`, `c3a5d772`. |
| M3QA-012 | AC-023-04 / REQ-023 | `integration` | Fixed | `crates/hoh-app/tests/m3t10.rs:948-1244` (`test_023_04_consultation_conclusion_escalates_and_schedules_nothing_before_commander` + `schedule_round` in existing test): runs a real host consultation (`developer` -> `researcher` via `helpers.ask`) returning `"改公开接口的签名"` through the delivery loop, then `developer` submits `intent:"apply", decision_class:"public_api"` via `helpers.ask`, observing `ILLEGAL_TRANSITION`, persisted `need_decision`, High `supervisor.woken`, and `schedule_round` returning `dispatched: None` with zero post-escalation `schedule.round` dispatches. Red-first: changed `"commander_decision_required"` in `consult.rs:309` to `"commander_decision_broken"` (`m3qa-012-red.log` exit 101; reverted; `m3qa-012-green.log` exit 0). Commits `32973638`, `c3a5d772`. |
| M3QA-013 | AC-030-01 / REQ-030 | `integration` | Deferred | See deferral paragraph below (`crates/hoh-app/tests/convergence.rs:282-392`; `docs/adr/ADR-033.md:44`; `crates/hoh-core/src/specindex/parse.rs:199-207`). |
| M3QA-014 | AC-031-04 / REQ-031 | `integration` | Fixed | `crates/hoh-app/tests/spec_review.rs:201-664`: revision 2 in Project A is drafted by a real `spec_draft` task (`Team::planning_with_review`), and the test asserts reviewer session independence from both Commander and the rev-2 drafting session (plus native session ids); verifies neither `.hoh/objects` nor `artifacts` holds any native-attempt or report object for the `FAILED` review attempt; asserts `independent_reports` and `candidates` row counts remain unchanged across revision 3 and Project B. Red-first: allowed non-`Completed` `TaskResult` to export in `host/completion.rs:337` (`m3qa-014-red.log` exit 101; reverted; `m3qa-014-green.log` exit 0). Commit `3859a16b`. |
| M3QA-015 | AC-032-04 / REQ-032 | `integration` | Fixed | `crates/hoh-app/tests/trace.rs:150-340`: spawns the real product `hoh __host` process under `strace -f -e trace=open,openat,openat2,stat,newfstatat,statx,access,faccessat,faccessat2` while serving `GET /v1/runs/{run}` (`projection.trace`), positively asserts `strace` captured the host reading the frozen spec object (`index.sha256()`), and asserts zero traced syscalls touch `<project>/contracts/` or `"contracts/`. Red-first: added `std::fs::metadata(root.join("contracts/requirements.json"))` in `persistence/trace.rs:50` (`m3qa-015-red.log` exit 101; reverted; `m3qa-015-green.log` exit 0). Commits `345be099`, `c3a5d772`. |
| M3QA-016 | AC-040-01 / REQ-040 | `integration` | Fixed | `crates/hoh-app/tests/m3t10.rs:95-275`: asserts exact `vector` object (`accepted:0, closed_issues:0, reopened_issues:0, new_critical_high:1, decision_risks:0`), exact count == 1 for the High wake and persisted SQLite records (`supervisor.woken`, `supervisor_triggers` `HANDLED`), and replaces in-process close/reopen with a real `hoh __host` child killed via `SIGKILL` and restarted, verifying the restarted host reconciles round 5 without writing a second `no_progress` or wake. Red-first: changed `vector.new_critical_high` accumulation in `progress.rs:96` to `+ 99` (`m3qa-016-red.log` exit 101; reverted; `m3qa-016-green.log` exit 0). Commits `2bfa8582`, `c3a5d772`. |
| M3QA-017 | AC-047-02 / REQ-047 | `tui-pty` | Fixed | `crates/hoh-app/tests/tui_pty.rs:1962-2035`: reads the actual builtin `resources/prompts/developer.md`, asserts the settings diff screen renders the deleted builtin line (`-<first_builtin_line>`), and scrolls via PageDown (`\x1b[6~`) to assert the added effective project override line (`+CUSTOM SETTINGS CONTRACT`) appears on screen. Red-first: emptied body slices in `prompt::unified_diff` while keeping headers (`m3qa-017-red.log` exit 101; reverted; `m3qa-017-green.log` exit 0). Commits `15516cd9`, `c3a5d772`. |
| M3QA-019 | AC-033-04 / REQ-033 (G4) | `integration` | Fixed | `crates/hoh-app/tests/spec_source/tasks.rs:883-1135`: adds `test_033_04_product_places_writer_in_spec_mismatch_hold_then_approval_migrates_or_cancels`, where a real scheduled/started writer attempt prepares its rev-1 freeze, rev 2 is approved before completion commits, and `complete_writer_attempt` (`completion.rs:672-677`) places `T1` (`REQ-A`) and `T2` (`REQ-C`) into `BLOCKED(NEED_DECISION)` + `need_decision{reason:"spec_mismatch"}` without SQL seeding; also exercises `admission.rs:283` (`Refusal::SpecMismatch`) via `schedule_round`, then approves rev 3 to verify product migration of `T1` and cancellation/replacement (`T2 -> T3`). Red-first: disabled the `RejectionReason::SpecMismatch` branch in `persistence/completion.rs:672` (`m3qa-019-red.log` exit 101; reverted; `m3qa-019-green.log` exit 0). Commits `4c28b8f5`, `c3a5d772`. |

**Deferral — M3QA-013 (AC-030-01 / REQ-030):** Deferred. In `crates/hoh-app/tests/convergence.rs:282-392`, the test authors the
revised Markdown (`markdown(...)` at line 337) and verifies the five headings on those bytes, while `ProposeSpecChange` and
`ApproveSpec` validate the `source: question_id=<id>` linkage (whose negative path is proven by `test_030_03_unsourced_user_decision_spec_invalid`).
Making the product itself either author non-empty prose across all five sections from a user goal + scope answer or reject empty
prose sections is disproportionate here without changing product semantics or introducing a live-LLM fixture: per ADR-033
(`docs/adr/ADR-033.md:44`, `crates/hoh-core/src/specindex/parse.rs:199-207`, `crates/hoh-core/src/specindex/tests.rs:231-237`),
the `specindex` parser intentionally accepts empty `# 用户`/`# 范围`/`# 非目标` sections as valid grammar, and any offline scripted
`spec_draft` agent (`Team::planning`) still copies test-supplied Markdown bytes rather than drafting prose. Closing M3QA-013
completely would take either (a) a CR tightening ADR-033's `specindex` grammar to reject empty sections (plus a negative
integration test), or (b) an online live-agent drafting acceptance check where a real CTO agent authors the five sections from
the goal and answered scope question.

### Round 1 review fixes (M4L2-001, M4L2-100..103)

Review findings from `.hoh-dev/self-tests/m4low2/review1-findings.json` (no product code changed; self-test evidence under
`.hoh-dev/self-tests/m4low2/fix1-*`, not independent acceptance):

| Finding | Severity | Disposition | Evidence (`file:line`, red-first sabotage, commit) |
|---|---|---|---|
| M4L2-001 | high | Fixed | `crates/hoh-app/tests/spec_source/tasks.rs:878-885, 1085-1135` (`test_033_04_product_places_writer_in_spec_mismatch_hold_then_approval_migrates_or_cancels`, runs in AC-033-04's `integration` check) and `crates/hoh-core/src/persistence/admission/tests.rs:679-717` (`admit_refuses_stale_spec_task_for_every_caller`, `unit` check): removed the SQL-seeded `stale-rev1` shortcut and vacuous `schedule_round().dispatched.is_none()` claim. Reachability analysis: in `schedule_round` (`rounds.rs:59-140`), `candidates()` and `admit()` share one SQLite IMMEDIATE transaction where `Eligibility.current_spec` (`rounds.rs:486-488`, `schedule.rs:96-101`) already excludes stale-spec rows; in `decide` (`decide.rs:236`), `decision.rs:668` checks `task.spec_id != run.spec_id` before `snapshot.admission`; in `attempts::create` (`attempts.rs:395`, `By::Direct`), `admission.rs:283` (`Refusal::SpecMismatch`) is the sole guard refusing attempt creation on product-cancelled `T2` (whose `spec_id` remains rev 2 after rev 3 deletes `REQ-C`). `test_033_04_...` now calls `store.create_attempt` on product-cancelled `T2` (zero SQL seeding), asserts `PersistError::IllegalTransition` with the exact `SpecMismatch` `why` string and zero new attempts; the `hoh-core` unit test directly matches `Err(Refusal::SpecMismatch { .. })` for `By::Round`, `By::Commander`, `By::Direct`. Red-first: temporarily disabled `admission.rs:280-286` (`fix1-m4l2-001-red.log` exit 101 on both tests; reverted; `fix1-m4l2-001-green.log` exit 0). Commit `9e5a8362`. |
| M4L2-100 | low | Fixed | `crates/hoh-app/tests/s5/mod.rs:14-76, 349-455`: removed `hold-steer` and its 10 s `executor.steer` timeout dependency; pauses `developer`'s per-role dispatcher in `HostHooks::input_delivered` (`HoldDelivery` + `ReleaseOnDrop`) after `late` is staged so `late-outbox` stays `PENDING:1` across the `codex:2` rebind checkpoint with no steer timeout. Commit `67e0c9a5`. |
| M4L2-101 | low | Fixed | `crates/hoh-app/tests/s5/mod.rs:475-545`: replaced the fixed 500 ms sleep with a positive host-sweep barrier — enqueues epoch-2 `current-ask`/`current-steer` into `input_queue` (`QUEUED:2`), then enqueues epoch-1 `sweep-probe` (`ASK`, `cto`), rebinds `cto -> codex:2` via HTTP `ConfigureTeam`, waits for `sweep-probe` to reach `REJECTED` (`STALE_ROLE_EPOCH`) via `settle_ended_inputs`, and only then asserts `current-ask` and `current-steer` remain `QUEUED:2`. Red-first: widened `inputs.rs:709` sweep predicate to `>= 0` (`fix1-m4l2-101-red.log` exit 101; reverted; `fix1-m4l2-100-101-green.log` exit 0). Commit `67e0c9a5`. |
| M4L2-102 | low | Fixed | `crates/hoh-app/tests/trace.rs:311-329` and `crates/hoh-app/tests/m3t10.rs:278-295`: replaced unbounded `wait()` after `SIGTERM` with bounded `try_wait()` polling (15 s deadline), falling back to `SIGKILL` (process group + child) and failing the test on timeout. Commit `9094ef68`. |
| M4L2-103 | low | Fixed | `crates/hoh-app/tests/trace.rs:242-290, 338-348`: switched `strace` to `-e trace=%file`, added `tracer.0.try_wait()?` in the readiness loop, rejected `pid == own_pid` and asserted `host_pid != own_pid`, and matched relative `"contracts"` and `"/contracts\""` in addition to `prefix` and `"\"contracts/"`. Red-first: added `std::fs::read_link("contracts")` in `persistence/trace.rs:50` (`fix1-m4l2-103-red.log` exit 101 capturing `readlink("contracts", ...)`; reverted; `fix1-m4l2-102-103-green.log` exit 0). Commit `9094ef68`. |

### M4 low package 2: reviews

Developer: agy (Antigravity CLI, gemini-4-neon-high), package `ab3e92ce..9711eebe` and fix round `9711eebe..b42503b7`; test and record changes only.

| Round | Frozen | Reviewer, session (attempt) | Verdict |
|---|---|---|---|
| 1 | `9711eebe` | Grok 1.0.92, `01a21563-08bf-7970-8d09-86ea156d3cd7` (`4a894da7-2786-4a1a-a2c2-a5eb874af728`), M4L2-001..099: AC coverage | FAIL: M4L2-001 high (the AC-033-04 "SpecMismatch" assertion on an SQL-seeded stale task passed with the branch deleted, because eligibility filters the task first) |
| 1 | `9711eebe` | Claude Code 2.1.331, `a54d9334-45fa-4864-ade9-a2f03fcb0d67` (`f85eda8e-823f-4bc3-a8f2-838f936ed933`), M4L2-100..199: determinism, process lifetime | PASS: lows M4L2-100..103 (fixed in the fix round, table above) |
| 2 (fix diff) | `b42503b7` | Grok 1.0.92, `01a215a5-4a8e-7383-bcd1-8650d07c5717` (`3a2c5e96-cf60-4415-ad87-0056e3e86699`), M4L2-200..299 | PASS, no findings: with the spec-inequality branch removed in a copy both the hoh-core unit test and the integration test exit 101, 0 on the unchanged tree; the integration assertion claims only the direct create_attempt refusal, not a round dispatch |

M3QA-013 stays deferred (reason above). Deferred lows: none new.

## FLK-14 (the busy-writer test's outside lock ran on its own wall clock)

Developer diagnostics from `072f9fa6`, REQ-062 / AC-062-01, test `test_062_01_settings_busy_writer_responsive_reads`. Seen
twice in a parallel `cargo test --workspace`: in the M3-T02 fix2 continuation ("expected HTTP 503, observed 202", isolated
rerun passed, reports/reviews/M3.md) and on PR #128's pull_request run 45659710217 (`left: 202, right: 503`,
`{"command_id":"c-r2","revision":1,"run_id":"r2","status":"ACCEPTED"}`). The second occurrence of the same class, so per
docs/19 (审查收敛) a design correction came before the fix.

DC-FLK14 (design correction). Invariant the test needs: while the test expects the product to see the database locked, the
outside connection holds the lock, i.e. the release is ordered after the observation it is meant to precede. The old test did
not have it: the python locker took `BEGIN IMMEDIATE`, printed `locked` and released after a fixed `time.sleep(7.0)` timed
from the lock, while the test's busy window (5 s busy_timeout, 503 after 5 to 6 s) is timed from the command reaching SQLite.
Their order held only while the command reached SQLite less than 2 s after the lock was taken (7 s − 5 s). Lifetime of the
resource: the lock lived for a wall-clock interval chosen by the child, not for the test's observation. Correction: the
child holds the lock until the test writes a line to its stdin (or its stdin closes, when the test ends or panics); the test
releases the 7 s hold only once it has the command's reply and at least 7 s have passed since the command was sent, and the
2 s hold 2 s after the command was sent, after checking that the command is still waiting. Both AC holds stay at least as
long as the AC says, and no existing assertion changes. The same stdin-driven holder is already used by `db_faults.rs`, `tui_pty.rs`
(`WriteLock`) and `delivery.rs`.

DC-FLK14 revision (evidence after the first fix): the lag named above is not the only way, and on today's main not the
likely way, for the command to reach SQLite late. The host's input loop (`crates/hoh-app/src/host/inputs.rs`, since
"M3-T09 S3: deliver Commander inputs through the host outbox") calls `settle_ended_inputs` on every 200 ms poll; that is
`OutboxJob::InputSweep` on the single writer, and `inputs::sweep` opens `BEGIN IMMEDIATE` (`writer::begin`) even when it
finds nothing to settle (it then returns without a commit). While the outside connection holds the lock, the first sweep
after the lock waits the whole 5 s busy timeout on the writer thread (stderr `hoh host: input delivery: DB_BUSY: database
is locked`), and a command submitted after that sweep is queued behind it: it reaches SQLite about 5 s late. With the old
fixed 7 s hold it then got the lock at 7 s and committed, 202 after about 6.7 s; with the test-driven hold it gets 503 after
about 10 s. The command wins only if it reaches the writer before the next 200 ms poll, so a lag of a few to tens of
milliseconds between the lock and the command already makes the failure likely in proportion. The M3-T02 occurrence predates
the sweep (that branch did not contain the M3-T09 S3 commit), so there the lag itself is the candidate.

Chain on today's main: outside `BEGIN IMMEDIATE` → within 200 ms the host's idle input sweep enters `BEGIN IMMEDIATE` on the
writer and waits up to 5 s → the r2 PauseRun queued behind it → the sweep fails DB_BUSY at about 5 s → the command's own busy
wait starts at about 5 s → the locker commits at 7 s → the command commits: 202 ACCEPTED where the test expected 503.

Reproduction (diagnostic, not committed; each run under the test slot, `*-result.json` beside each log):
- r2 command sent 2.5 s after the lock (`diag-before.patch`, `fix-plus-diag.patch`): before, 202 (`before-diag.log`, the CI
  symptom, with the sweep's DB_BUSY line); after the test fix, `503 after 7.635978758s` (`after-diag.log`).
- r2 command sent 250 ms after the lock, just over one sweep period (`diag250-before.patch`, `diag250-fix.patch`): before,
  202 in 3/3 runs (`before-diag250-0{1,2,3}.log`), each with the sweep's DB_BUSY line; after the test fix, `503 after 9.83 s`,
  `9.82 s`, `9.84 s` in 3/3 (`after-diag250-0{1,2,3}.log`). Deterministic.
- No diagnostic, after the test fix: passed 3/3 (`after-0{1,2,3}.log`, 9.2 s each).

Status: the test fix (commit "M3 cleanup: FLK-14 the busy-writer test releases the outside lock itself") removes the release
race and reports the elapsed time, but it does not fix FLK-14 on today's main: the remaining cause is product behaviour,
and the unchanged 5 to 6 s assertion (AC-062-01: 503 DB_BUSY within 5 to 6 s while the outside connection holds the lock)
now reports it as `503 after ~10 s` instead of a 202. FLK-14 stays open, recorded as product issue FLK-14-P1 for the lead:
an idle host's background sweep takes the write lock and so delays a command's DB_BUSY by up to one busy timeout.
Recommendation (not implemented here: the brief scoped this task to the test, and the change is on the single-writer path):
the idle sweep must not take the write lock, e.g. `settle_ended_inputs` first asks a read-only connection whether anything
can be settled (an over-approximating `EXISTS` over the sweep's three selects; WAL reads take no write lock) and submits the
`BEGIN IMMEDIATE` job only then; regression: this test with the 250 ms diagnostic made permanent (the r2 command sent after
one sweep period) must give 503 within 5 to 6 s. More general, and a design question: any non-idle background job queued
ahead of a command stretches the command's DB_BUSY to a multiple of the busy timeout (REQ-062 "SQLITE_BUSY超过busy_timeout时
写命令返回503"); AC-062-01 covers only the idle host.

Not weakened: every assertion stays (PRAGMAs, read-only readers, 202 within 5 s for the 2 s hold, 503 `DB_BUSY` retryable
after 5 to 6 s with the Run unchanged for the 7 s hold, both GETs 200 within 500 ms, r1 PAUSED); added: the 2 s hold's
command must still be waiting when the lock is released (not checked before), the outside connection must exit 0, and the
503 message reports the elapsed time.

Same pattern elsewhere (grep `BEGIN IMMEDIATE` in tests): `crates/hoh-agents/tests/claude_adapter.rs` `hold_store` (1.5 s
from the lock) in `a_cancel_queued_with_the_exit_still_cancels` and in the cancel-after-result test near line 1028 relies on
the cancel arriving before the hold ends (after up to 2 s of reap polling, or 400 ms after `go`); a lagging trigger would let
the adapter's write through first. The same class, not seen in CI: low follow-up FLK-14-F1, not changed here (another crate,
outside this task). `crates/hoh-core/tests/delivery.rs:1860` (0.4 s hold) does not depend on the hold's length: the tick's job
is queued before the hold ends whatever the lag. `db_faults.rs`, `tui_pty.rs` and `delivery.rs:1623/2050` already release on
stdin.

Evidence under `.hoh-dev/self-tests/flk1415/flk14/` (patches, logs, `*-result.json` with argv, binary digest, exit, the run's
duration without the slot wait, log digest). Self-tests, not acceptance.

### FLK-14-P1 and DC-FLK14b (a write's wait for the lock is counted from its submission)

FLK-14 is the second flake of AC-062-01's busy test, and FLK-14-P1 is a class: any writer job queued ahead of a command
that is itself waiting for an outside lock stretches the command's DB_BUSY. Per docs/19 (审查收敛) a design correction
comes before the fix.

Chain (reproduced, `.hoh-dev/self-tests/flk14p1/red-host.log`): an outside `BEGIN IMMEDIATE` → within 200 ms the
host's idle input sweep (`settle_ended_inputs` → `OutboxJob::InputSweep`) runs `writer::begin` on the single writer,
and SQLite waits the connection's whole busy_timeout (5 s) for the lock → the PauseRun submitted 250 ms after the lock
is queued behind it → the sweep fails DB_BUSY at about 5.2 s → the command's own `BEGIN IMMEDIATE` then waits a fresh
5 s → 503 after 9.8 s, outside AC-062-01's 5 to 6 s.

DC-FLK14b. Wrong assumption: a request's wait for the write lock is the busy_timeout SQLite gives its own `BEGIN
IMMEDIATE`, so the writer's other work is cheap and never adds to a command's DB_BUSY time. It is not: the writer has one
connection and takes its jobs in order, so every job ahead of a command that waits for the same outside lock adds up to
one busy_timeout, and on an idle host the sweep (and, with a runner, Allocate and Stops) waits for the lock every
200 ms. Rule adopted: **a request waits for the write lock at most until one busy_timeout after it was submitted**
(submitted = before its caller waited for room in the queue), whatever was queued ahead of it. The writer records the
instant with each queued request (both lanes); `writer::begin` sets the connection's busy_timeout to what is left of
that (rounded up to whole ms; 0 once it is gone, so a free lock is still taken and a held one answers at once) for its
`BEGIN IMMEDIATE` and sets the configured value back right after, so `PRAGMA busy_timeout` still reads 5000 between
requests. It is one rule in one place and holds for every job, background or user, both lanes, without a per-job
classification: a command's DB_BUSY comes busy_timeout after it was sent however many jobs are ahead of it, and a
command whose lock frees in time still commits.

Alternatives not taken: (a) the read-first gate for the sweep (ask a reader whether anything can be settled, take the
writer only then) fixes the idle sweep only; Allocate, Stops and Renew still take the writer idle, and every per-run
step (Pace, Round, Supervise, Stall, VerificationReconcile) takes it on a host with runs, so the class stays. (b) A
short busy timeout for background jobs needs every job classified and makes one-shot background writes (an attempt's
result, its end) fail on a brief outside lock they used to outlast. (c) Commands ahead of background work in the queue
does not help: the job ahead is already inside SQLite's busy wait when the command arrives.

Consequences, accepted: a request that waited in the queue behind lock waiters has less of its own wait left (none once
busy_timeout has passed since it was submitted); it can then fail DB_BUSY although the outside lock goes a moment
later. Before, it would have answered DB_BUSY even later or committed after more than busy_timeout. Every caller
already handles a retryable `PersistError::Busy` (table below). With no outside lock nothing changes: the lock is free
and a 0 wait takes it. `commands::begin_host` (host start, the first writer job) and the migration in `Store::open`
(before the writer thread) do not go through `writer::begin` and keep the full busy_timeout: nothing is queued ahead
of them. The idle BEGINs themselves stay (efficiency, and a `hoh host: input delivery: DB_BUSY` line every 5 s while an
outside connection holds the lock): low follow-up FLK-14-L1, not needed by any AC.

Audit of the writer's background jobs (code read with file:line evidence by an Explore subagent, spot-checked:
inputs.rs:33-60, host/runner.rs:272/285/393-408/823; the rule above covers each without a per-job change):

| # | Job (loop, file) | Period / trigger | Takes the writer on an idle host | Busy handling | Fix |
|---|---|---|---|---|---|
| 1 | InputSweep (`settle_ended_inputs`, host/inputs.rs:33-60) | 200 ms poll and each watermark move | yes: submitted unconditionally, `inputs::sweep` begins before its selects | logged, next poll | writer deadline |
| 2 | Claim / Settle / Input / RouteInput / ChangeInput (delivery ticks, host/inputs.rs:61-91) | per poll, roles from a reader query | no (needs PENDING/DELIVERING inputs; a deferred row re-claims each poll, low FLK-14-L2) | tick ends, next poll | writer deadline |
| 3 | Runner Allocate (host/runner.rs tick) | 200 ms or watermark | yes, with a runner | tick ends, logged, next tick | writer deadline |
| 4 | Runner Stops | 200 ms or watermark | yes, with a runner | same | writer deadline |
| 5 | Runner Renew | every 30 s | yes, with a runner | same, retried next tick | writer deadline |
| 6 | Pace, Round, Mode, DraftStep/DraftDispatch/Readiness, spec review | per tick, per paced run | no runs: no; any waiting run: at least Pace + Round | tick ends | writer deadline |
| 7 | VerificationReconcile | per tick, per verified run | no | tick ends | writer deadline |
| 8 | Supervise / Dispose / StepFailed | per tick, per supervised run | no | Busy ends the tick | writer deadline |
| 9 | Stall | per tick, per run with spec and commander | no | tick ends | writer deadline |
| 10 | Checkpoint, Staff, SessionStart, Recovery, WithdrawTurn, Ack, lease-expiry settle | per tick, reader-gated | no | tick ends | writer deadline |
| 11 | Attempt steps (Received, Running, Effect, Consult, Request, Finished, completion, candidate, evidence, Watched) | per attempt event (one-shot) | no | error ends the attempt, then `settle(runner_error)`; else the lease expiry (row 10) settles it | writer deadline |
| 12 | Control PauseStep, Mode, Baseline (host/control.rs) | 200 ms tick | no (idle ticks only read) | tick ends, logged | writer deadline |
| 13 | BaselineProcessEnded | control tick, reader-gated | no | logged, next tick | writer deadline |
| 14 | BaselineProcess / BaselineTests (baseline task) | per confirmed run, one-shot | no | logged, task ends | writer deadline |
| 15 | CancelPass / FinishCancel (urgent lane) | 1 s per cancelling run | no | logged, again in 1 s | writer deadline (urgent lane too) |
| 16 | Suite Begin / Publish / End (host/suites.rs) | 200 ms after a registry refresh, when suites are pending | no | kept and retried each pass | writer deadline |
| 17 | BeginHost, startup Reconcile, sync_config | host start, one-shot | n/a | host does not start / logged | BeginHost not via `begin` (first job); rest writer deadline |
| 18 | Runner start: manifest, start recovery, verification start | one-shot | n/a | logged | writer deadline |
| 19 | unclaim_all at writer close | once | only with claims left | ignored | full busy_timeout (nothing after it) |

The writer also serves reads queued with the writes (`reconcile_scan`, `runner_waiting`, `session_starts`,
`running_attempts`): no `BEGIN IMMEDIATE`, so no lock wait of their own. Outside hoh-core nothing opens SQLite but the
clippy canary.

Red first, on the code before the fix: `test_062_01_a_queued_write_is_busy_one_timeout_after_its_submission` (hoh-core,
busy_timeout 2 s, a write queued 1 s after the one waiting for the lock): `DB_BUSY 3.006176727s after it was submitted`
(`red-core.log`); `test_062_01_busy_answer_not_delayed_by_background_sweep` (hoh-app host, both PauseRuns sent 250 ms
after the lock, one sweep period): `503 after 9.803797159s` with the sweep's `hoh host: input delivery: DB_BUSY` line
(`red-host.log`). Green after the fix (`green-core.log`, `green-host.log`), then both host tests 10/10 in a row on the
same binary under the test slot (`repeat-*.log`, `repeat-*-result.json`). The host test's body is shared with
`test_062_01_settings_busy_writer_responsive_reads` (delay 0); no assertion or window changed (5 to 6 s, 202 within
5 s, GETs within 500 ms, PRAGMAs 5000).

FLK-14-F1 (`crates/hoh-agents/tests/claude_adapter.rs`, `hold_store`): the same fixed-hold pattern as DC-FLK14 (the
outside lock lived 1.5 s on the child's clock while the test needs it held until its cancel is sent; the first test
sends the cancel after up to 2 s of reap polling, so a slow reap already crossed the hold). Not seen failing, but the
same class, so fixed like DC-FLK14: the holder releases on a line on its stdin (or its stdin closed), and both tests
release it right after `cancel.request()` (`f1-claude-adapter.log`). No assertion changed.

Evidence under `.hoh-dev/self-tests/flk14p1/`. Self-tests, not acceptance.

## FLK-15 (strace's `???` for a call it could not read)

Developer diagnostics from `072f9fa6`, REQ-029 / AC-029-03 (CR-037), test
`test_029_03_offline_instantiation_in_installed_release`. Seen once on PR #128's pull_request run 45659710217, deployment
job: `Error: "undecodable trace line: 8388  ???( <unfinished ...>"`.

What strace prints (source of strace 6.8, the Ubuntu 24.04 runner's package, and 6.19, this VM; the paths below are the
same in both): `syscall_entering_decode` prints `<name>(` with `tcp_sysent(tcp)->sys_name`, which is `stub_sysent`'s `"???"`
when `get_scno` returned an error. On x86_64 with PTRACE_GET_SYSCALL_INFO (Linux ≥ 5.3) the only such error is
`strace_get_syscall_info` failing, i.e. `ptrace(PTRACE_GET_SYSCALL_INFO)` failing at a syscall-entry stop. An unknown call
is qualified `QUAL_RAW | DEFAULT_QUAL_FLAGS` (traced), which is why it shows despite `-e trace=socket,close,connect`. The
call's end is printed by `print_event_exit` at the thread's PTRACE_EVENT_EXIT stop: `<... ??? resumed>) = ?` (or `???() = ?`
when no other line came between), `?` being strace's pseudo result for a call that did not return, and then
`+++ exited … +++`. Without that stop strace prints the thread's exit right after the call's `<unfinished ...>` entry.

Why such a call never ran (kernel v6.8 and master): every ptrace request but PTRACE_KILL/INTERRUPT goes through
`ptrace_check_attach`, which fails with ESRCH unless `ptrace_freeze_traced` succeeds, and that requires
`!__fatal_signal_pending(task)`. A tracee in a syscall-entry stop leaves the stop only when its tracer resumes it (strace had
not) or a fatal signal wakes it. So the request failed because a fatal signal (SIGKILL from `exit_group`, `execve`'s
de_thread or a kill) was pending. `ptrace_report_syscall` then returns `fatal_signal_pending(current)` (master: its negation
through `ptrace_report_syscall_permit_entry`), on which `syscall_trace_enter` aborts the call: "That must prevent normal
entry so no system call is made" (`ptrace_report_syscall_entry`). A `???` call therefore cannot be a connect that happened,
and it has no arguments to check.

Chain: the host stops (SIGTERM, `exit_group`) while another of its threads is at the entry stop of some call; the kill lands
before strace reads the stop; strace prints `???(`; the audit, which allowed only `socket(`, `close(` and `connect(` lines,
failed closed. The same exit also leaves named calls ending `= ?` or `= ? <unavailable>`, or `<unfinished ...>` followed
directly by the thread's exit, which the audit also rejected: the same flake, not seen in CI yet.

Reproduction: `spin.c` (8 threads looping `socket`/`close`, the main thread `_exit`s after 20 ms) under
`strace -f -e trace=socket,close,connect,execve -s 4096 -o <trace> ./spin`. First batch, 60 runs, the 46 traces with `???(`
kept (`flk15/traces/`, `repro-summary.txt`): 64 `???( <unfinished ...>`, each later `<... ??? resumed>) = ?`, one
`???() = ?`, 125 named calls ending `= ?` or `= ? <unavailable>`, every `= ?` line followed for its thread directly by `+++`;
32 of the 46 have a named call left `<unfinished ...>` at its thread's exit. Second batch, 60 runs, all kept
(`flk15/traces2/`): 46 with `???(`, 36 with an unfinished call at an exit, 60 with `= ?`. The CI line is the first form.

Fix (fails closed on everything else), in two steps:
1. "M3 cleanup: FLK-15 the connect audit accepts a call strace could not read only when its thread exits": `???(` is
   accepted only with no arguments and the pseudo result `?`; a call with the pseudo result `?` (named or `???`) puts its
   thread in "exiting", after which any line of that thread other than its `+++` exit fails the audit, as does a trace ending
   first; a line without a thread id cannot carry `?`; a named call with `?` is checked on its decoded arguments as before.
2. "M3 cleanup: FLK-15 a call left unfinished by its thread's exit is checked as one with result ?": the harness below found
   the third form (first-step audit: `t.48: trace ended with an unfinished syscall`; on the second batch `u.55`). At a thread's
   exit its pending call is checked as `<start>) = ?` would be: a connect still needs a local target or an owned SOCK_DIAG
   socket (strace prints all of connect's arguments at entry; a partial entry does not parse and fails), a socket authorizes
   nothing, a close drops the fd. Without a thread id an exit closes nothing. The per-call check moved into `check_call`.

How strace is attached is unchanged: an undecodable call is not a blind spot to close, because it never ran.

Runs (prebuilt test binaries under the test slot, `*-result.json` beside each log): red, the new tests on the old audit:
`connect_audit_accepts_a_syscall_cut_off_by_its_threads_exit` failed with the CI error, 8 others passed (`red.log`, exit
101); first step: 9 passed (`green.log`); final: 9 passed (`green2.log`), the audit accepts all 46 + 60 reproduction traces
with 0 connects (`repro-traces-green2.log`, `repro-traces2-green2.log`; uncommitted ignored harness
`repro-audit-harness.rs`), where the first step failed both batches (`repro-traces-v1.log`, `repro-traces2-v1.log`).

Not weakened: all existing audit tests pass unchanged, including the rejection of a netlink connect with result `?` and no
exit. Added tests reject `???` with arguments or a numeric result; `?` or an unfinished call without a thread id; `?` followed
by another call of its thread or by the trace end; an external connect with `?`, or unfinished at its thread's exit (also with
another thread's line between); a netlink connect, unfinished at exit, on no SOCK_DIAG socket; a netlink connect on an fd
only a `?` socket returned.

Evidence under `.hoh-dev/self-tests/flk1415/flk15/`. Self-tests, not acceptance.
