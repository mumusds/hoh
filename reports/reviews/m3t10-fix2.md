# M3-T10 second review follow-up

Baseline: `aab4a10729302620c07578f4293097016139dfb7`, branch `task/M3-T10`.
The delegated scope fixes M3K-100, M3K-101 and M3K-102 without pushing or advancing the global taskboard. Self-tests are diagnostic, not acceptance.

| Finding | Severity | Disposition |
| --- | --- | --- |
| M3K-100 | medium | Default consultations reserve only wall_seconds, attempts, tokens and usd. No candidate iteration is reserved or created. TEST-023-01 covers other work reaching 9/10 iterations while the consultation remains open. |
| M3K-101 | medium | Both dispatch paths credit only the target's unused, unreleased hold. Other reservations, consumed allocation, unknown pricing and Run/task deadlines still gate dispatch. Run completion and resume exclude unused consultation holds; actual exhaustion still stops the Run. Boundary tests include reopening the Store. ADR-037 records the accounting correction. |
| M3K-102 | low | Remove the extra Commander wake and its misleading AgentBlocked reason. Keep BLOCKED plus the parent Runtime diagnostic, with the precise stop in detail; no orphan NEED_DECISION marker. Deadline and budget exhaustion tests cover one-time settlement. |
| M3K-103 | low | Backlog for M3 low cleanup: existing task.consult lookups in consult/bounds.rs omit run_id and scan events by kind plus JSON task_id. Scope those reads by run_id or deterministic event identity; preserve the current behavior. New hold-credit reads are run-scoped, but the pre-existing lookups remain deferred. |

Source: `/home/ai/dev/hoh/.hoh-dev/evidence/m3t10-r2-review-claude.out`, native attempt `98499de1-d33e-45ad-accd-5cf3aba3729e`. The delegated brief records the complementary Grok PASS. No new reviewer or live agent was launched by this worker.

Evidence: `.hoh-dev/self-tests/m3t10-fix2/`. Baseline m3t10 tests passed (30 passed, 1 ignored helper). `red-100-boundary`, `red-101`, `red-101-pace` and `red-102` failed on their intended behavioral assertions (exit 101). `green-100`, `green-101` and `green-102` passed (exit 0). The final check logs and frozen candidate manifest live in the same evidence directory; these records do not assert independent acceptance.
