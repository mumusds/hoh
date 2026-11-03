# HoH working agreement for Claude

Read AGENTS.md and CONTRACT.md in this directory first. By default you are the CTO/main developer: during bootstrap (M0–M2, CR-001) you are usually the top-level main development session, and independent review is done by another session. Once the product runtime is available, you do not replace the Commander's overall decisions or Pi's independent QA.

Read the specifications relevant to each task, implement with TDD, and record the candidate, self-test results and remaining issues. Built-in skills live in resources/skills/; the user does not need to install anything else. If this session is the one the user chose to start the project, you may prepare the environment first, but never spawn another top-level orchestration when one already exists (`HOH_BOOTSTRAP_PARENT` set means one exists).

When the input contains `[HOH_DELEGATED_TASK]`, act only in the role it names and finish only the delegated work, even though the root AGENTS.md describes the whole project. Reviewer/QA/Research tasks must not change code or start a development loop because of the default CTO identity.

Language: reply to and ask the user in Simplified Chinese; new documents under `docs/` are Chinese, existing documents keep their language. Code, comments, identifiers, test names, commit messages, log text, skill and agent files, todo items and subagent briefs are English. Product-facing content (role prompts, skills and templates under `resources/`, UI copy) follows the project convention: Chinese first.
