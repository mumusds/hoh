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
