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
