# HoH autonomous development entry point

**First determine your execution level.** If the input starts with `[HOH_DELEGATED_TASK]`, you own only that subtask and the given role: do not run the project-wide bootstrap, do not read LAUNCH_PROMPT and start another top-level session, do not claim other tasks, and do not update global completion status. Return the task result and stop; QA/Reviewer never modify the source under test. The continuous-progress rules below apply only to the top-level session started by the user or marked `[HOH_TOP_LEVEL_BOOTSTRAP]`; all other project constraints still apply. If the environment variable `HOH_BOOTSTRAP_PARENT` is set, you are already inside a bootstrap session and must not start another top-level session.

1. Read CONTRACT.md (including CR-001) and docs/19-ai-development.md first; read the relevant requirements in contracts/requirements.json per task. These are adopted specifications; do not re-interview decisions that are already made.
2. This kit is not the finished HoH product. The goal is to implement every requirement whose `release_targets` contains v1. v0.1 is an interim usable release, not the end point.
3. Bootstrap phase (M0–M2): one main development session (Claude by default) plus an independent review session (Codex by default, via `tools/agent_task.py --role reviewer`). The product's default team is still Codex commander, Claude CTO/developer, Pi QA/researcher; roles are configurable.
4. Rust modular monolith; native agents first. No fourth coding agent and no outer infinite Python "continue" loop.
5. Shell surface is only `hoh`, `hoh -p`, `attach`, `web`, `doctor`; `/plan` lives in the TUI. CLI and TUI share semantics, not a one-to-one command list.
6. Run bootstrap, `scripts/check.sh --bundle` and preflight. Claim tasks with `tools/taskboard.py claim <id> --owner <session>` and work in small TDD/implement/test/review steps along the dependencies. Implementation tasks that depend on outline requirements are refused: run that milestone's spec task first.
7. Single writer (the taskboard allows only one running writer task) + frozen candidates + independent QA/review. Checks run only through the frozen check plan and `tools/run_check.py`. Instructions, code, specs and evidence are version-linked.
8. Keep and finish TUI/Web, role/prompt customization, built-in skills/templates and the three Pi plugins; never silently drop scope. If a plugin is unavailable upstream, degrade it explicitly per CR-001.
9. Never fabricate sources, model capabilities, execution results, costs or pass rates; keep mock/product/live/not_run strictly separate.
10. Never edit the baseline or required gates to get green. Baseline changes need a CR and `tools/baseline.py relock --cr`, within the user's authorization.
11. Update `.hoh-dev/PROGRESS.md` and the taskboard after every small step; save a recovery point before the session ends or compacts.
12. Do not stop at M1, an MVP or v0.1; unless there is a real external blocker, continue with the next task.
13. YOLO only in the isolated VM the user declared, as a non-root development user. No privacy/sandbox side projects, but keep engineering integrity.
14. Real acceptance must run at the end. Report COMPLETED only when `release_gate.py` passes the v1 profile (COMPLETED_DEGRADED when plugins are degraded); otherwise report INCOMPLETE/BLOCKED explicitly.
