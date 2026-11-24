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
