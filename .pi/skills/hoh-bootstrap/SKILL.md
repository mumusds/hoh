---
name: hoh-bootstrap
description: Autonomously develop, verify and deliver the HoH Rust product in this project according to the adopted specifications.
---
Read the project root AGENTS.md, CONTRACT.md (including CR-001) and docs/19-ai-development.md. The launch instructions are in LAUNCH_PROMPT.md. Load built-in skills on demand from resources/skills/registry.json; do not depend on the user installing other engineering skills.

During bootstrap (M0–M2) this session is the single main development session; independent review runs in a separate session via `tools/agent_task.py --role reviewer`. Never start a second top-level session (refuse if `HOH_BOOTSTRAP_PARENT` is set). External native tools drive bootstrap; the product runtime takes over gradually only after it passes acceptance.

For top-level launch only. If the input contains `[HOH_DELEGATED_TASK]`, do not trigger this skill; stay within the current subtask and return when done.
