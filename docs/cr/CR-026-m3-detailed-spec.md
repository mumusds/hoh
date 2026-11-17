# CR-026 · M3 的 16 项需求细化为 detailed，与 M3 实现任务的拆分

ID：CR-026
状态：DRAFT
批准人：（待定。预期为“CTO 依 CR-003 D1 预授权与所有者 2026-11-17 对 C22 的决定 (a)（独立设计审查会话 …）”；见“所有者的决定”与“是否属于 CR-003 D1”）
日期：2026-11-17（第 3 稿：设计审查第 2 轮之后，并记入所有者 2026-11-17 对 C22 的决定 (a)，见文末“审查记录”；M3-T01 的准备稿，领取时按当时的 main 重新核对）
基线：改受锁文件 `contracts/requirements.json`（M3 的 16 项需求由 outline 细化为 detailed；REQ-018 的配置键表增加三项）、`contracts/acceptance-scenarios.json`（这 16 项的场景）、`contracts/protocol.schema.json`（`$defs.ErrorCode`、`$defs.RejectionReason`、`$defs.BlockedReason` 补齐 8 个原因码）；新增受锁文件 `contracts/spec-review-report.schema.json`（独立规格审查的报告，CR26D-101）与 `contracts/sql/002_spec_drafting.sql`（C22 的决定 (a)：首次批准之前的起草 Task 可以没有规格）；不在锁内、随 CR 一起改：`contracts/development-tasks.json`（M3 实现任务的拆分）、`scripts/product/persistence-whitelist.json`（迁移 002 的条目）。`config/hoh.example.toml` 不在本 CR 中改（CR26D-103：三个新键随加载器的支持在 M3-T06、M3-T09、M3-T10 中加入）。`python3 tools/gen_spec_views.py` 重新生成 docs/02 与 docs/18；批准后 `python3 tools/baseline.py relock --cr docs/cr/CR-026-m3-detailed-spec.md --add contracts/spec-review-report.schema.json --add contracts/sql/002_spec_drafting.sql`

本 CR 是 M3-T01（“细化 M3 需求为 detailed：每项 2–5 条可判定 AC 与场景，写 CR 并 relock 基线”）的准备稿。M3-T01 依赖 V01-T02，尚未领取；本稿不改任何受锁文件，下文的片段已在仓库之外的副本上应用并通过 `tools/validate_bundle.py` 与 bundle 自检，并在同一副本上运行了 TEST-018-01 与迁移 002 的探针（见“验证方式”）。依据是 M3 准备清单 `.hoh-dev/evidence/m3-prep/inventory.md`（写于 16ff3e1），已按 main 715e2d0 复核，更正见附录 A；本稿引用的源文件在 715e2d0 与 main c874460 之间只有 `crates/hoh-core/src/persistence/mod.rs` 第 1371 行之后新增了 `runner_watched`，引用的行号不变。

## 背景

M3 的 16 项需求（REQ-007、008、009、017、023、028、029、030、031、032、033、034、040、045、046、047）都是 `spec_maturity=outline`、`required`、`release_targets=[v1]`、P0：每项只有 summary 与一条 AC，AC 的 given/when/then 是占位（如 “when: 同 CommandHandler 的集成测试”，“then” 照抄 summary），没有 behavior、failure_modes、decisions、doc_refs；场景的 fixture 都是“待该里程碑 spec 任务细化”。CONTRACT 第 17 条要求 outline 在实现前细化为 detailed，不得删范围、降门槛。

M3-T01 的 done_when 还要求：按每个实现任务至多 2 项需求、8 条 AC 拆分本里程碑的实现任务（更新 development-tasks.json 的 requirements/acceptance/checks/depends_on，validate_bundle 的归属规则通过）；每条 AC 只分给依赖闭包已提供其所需能力的任务（CR3R-02）；REQ-039 按任务范围选代码的上下文来源分给一个实现任务（CR-016）；CR-018 交给 M3-T02、M3-T04 的各项分给一个实现任务并写进 done_when（M2T16D-1050）。

## 所有者已定的事项（2026-11-17，照录，不再讨论）

准备清单 §4 的六个所有者问题，所有者于 2026-11-17 都选了保守默认；第 2 稿之后提出的 C22（首次批准之前的起草），所有者于 2026-11-17 选了 (a)。本 CR 按下表写入需求：

| ID | 决定 | 写入 |
|---|---|---|
| O1 | 只有用户批准规格，HoH 从不代用户批准；无人值守的 Run 停在批准之前 | REQ-008 行为 1、4 与 AC-008-02、-05；REQ-009 行为 2 与 AC-009-02；REQ-030 决定 2；REQ-034 行为 3、4 |
| O2 | 无人值守只假设项目配置中列出的默认值；产品方向、范围、非目标一律阻塞 | REQ-030 行为 4 与 AC-030-02；REQ-018 的配置键表加 `[assumptions]`（三类为保留名） |
| O3 | 已有仓库：碰到用户未提交的文件先阻塞询问；发现不自动运行项目的构建与测试 | REQ-028 行为 3、4 与 AC-028-02、-03 |
| O4 | 用户规格是 docs/spec/ 下的 Markdown，固定标题与 id 行；HoH 生成 JSON 索引 | REQ-034 行为 1–5 与 AC-034-01；REQ-029 行为 6 |
| O5 | Skill 不写进原生 Agent 的用户目录，只注入或写进会话自己的目录 | REQ-046 行为 1、失败模式 3 与 AC-046-02 |
| O6 | 任何规格修订使全部验收失效，在新的候选上重验 | REQ-033 行为 4 与 AC-033-03；REQ-032 行为 5 与 AC-032-03 |
| C22 | 首次批准之前的起草取 (a)：Plan 对话、由 Agent 起草，起草 Task 的 spec_id 为空，经一个受守护的 SQL 迁移（只有起草 Task 可以为空）；草稿只经用户的 ApproveSpec 成为修订 1；Q1(a) 与 `-p` 停在 no_spec 不变 | 第 2 节；迁移 002；REQ-008 行为 2–4 与 AC-008-02、-05；REQ-028 行为 1、5 与 AC-028-04；REQ-029 行为 7 与 AC-029-03 |

## 决定

### 1. 共同约定（16 项共用，各需求的行为里引用）

- **规格根与格式**（O4）：用户项目的规格是 `docs/spec/*.md`；固定一级标题为用户、范围、非目标、决策、开放问题、需求；需求为 `## REQ-<id> <标题>`（可带 class 行：required 或 degradable，缺省 required），验收为 `### AC-<req>-<nn>`，带 given、when、then、check 四个 id 行；决策条目带来源（question_id、input_id 或假设的事件 id），开放问题条目带“阻塞：是/否”。精确语法在 M3-T02 的 ADR 中定，内置模板（REQ-029）给出实例。
- **冻结形式与规格摘要**（CR26D-02、-102）：多份 Markdown 的冻结形式是 HoH 生成并提交的 `docs/spec/index.json`：规范 JSON（UTF-8、LF、键按字典序、两空格缩进、末尾一个换行），含 `generated` 标记、`files`（每份 Markdown 的路径与 sha256，按路径排序）、`requirements`（与 `contracts/requirements.json` 的需求对象同形；每条 AC 带 `test_id`；`release_class` 取 class 行，`release_targets` 恒为 `["project"]`：用户项目只有一个发布目标）与 `scenarios`（每条 AC 一项 `{id, requirement_id, acceptance_id, check_id}`，`check_id` 取该 AC 的 check 行）。**规格摘要就是 index.json 字节的 sha256**：任一 Markdown 文件的任一字节变化都改变它，scenarios 也在其中。`specs.sha256`、ApproveSpec 与 ProposeSpecChange 的 `spec_sha256`、`candidate.spec_sha256` 都是这个值；`candidate.spec` 是指向 index.json 的单个 FileRef；`acceptance_criteria.check_id` 与追踪的 TEST→check 都取自 scenarios（REQ-032 行为 6）。冻结时 HoH 从该提交的 Markdown 重新生成索引，要求与提交中的 index.json 逐字节相同。用户项目不需要 `contracts/` 目录；受锁的候选契约不改：`contracts/candidate.schema.json` 的 `spec` 本就是单个 FileRef。
- **已批准修订**：经用户的 ApproveSpec 批准（批准问题的 AnswerQuestion approve 经同一对端判定后转为 ApproveSpec，O1）、sha256 为该修订规格摘要的 specs 行；specs 只有已批准修订（M2T13D-59，ADR-031 决定 1，所有者 2026-11-16 的 Q1(a)），只有已批准修订被采用、派发、冻结与验收引用。项目的首个规格经批准成为修订 1（`spec.changed{cause:"approved"}`），之后的修订经 CR（REQ-033，`spec.changed{cause:"changed", cr_id}`）。受锁的 ApproveSpec 载荷照旧（`spec_sha256`、`decision`、`note`），ProposeSpecChange 照旧（`reason`、`spec_sha256`）；`decision` 的三值语义见 REQ-008 行为 1。
- **只由用户批准**（O1，CR26D-06、-105、-152）：host 按连接对端的进程身份判定，对端或它的任一祖先带 Attempt 标记、对端在任一 Attempt 的 cgroup 中、或对端无法确认，都以 403 SPEC_APPROVAL_NOT_USER 拒绝（REQ-008 行为 1）。Commander 的 DecisionPayload 没有批准一类，它的输出永远不产生修订。剩余风险（同用户进程脱掉标记、离开 cgroup 并脱离 Attempt 的祖先链）与 CR-024 的 R1–R7 一样写入 docs/12。
- **独立规格审查**（CR26D-05、-101）：对象是已批准修订而不是候选。报告用新的受锁契约 `contracts/spec-review-report.schema.json`（第 5 节），审查的开启与收取是事件 `spec_review.opened`、`spec_review.reported`；不进 `independent_reports`（`candidate_id` 必填、外键到 candidates），不开验证轮次（`reports::intake` 经 `round_of_task` 找候选），也不为规格造候选（REQ-031 行为 3）。
- **新事件**（`$defs.Event.kind` 是开放的字符串，不需 CR；名称以 M3 各任务的 ADR 为准，AC 中出现的为定名）：`spec.index_failed`、`spec.generated_drift`、`spec.drafting`、`spec_review.opened`、`spec_review.reported`、`plan.write_violation`、`task.migrated`、`task.replaced`、`readiness.passed`、`run.baseline`、`run.mode_change_pending`、`run.mode_changed`、`input.delivered`、`no_progress`（ADR-025 决定 1 已列为唤醒种类，产生方在 M3-T10）。新的 Run 级问题原因：`spec_approval`（REQ-008 行为 4）。
- **check**：AC 只用 v1 档已运行、且由 M3 实现任务的祖先交付的检查：unit、integration、tui-pty、recovery、deployment、adapter-live、traceability（M0-T03、M1-T03、M1-T08、M2-T05、M2-T10、M2-T14 交付）。没有 AC 用 web-e2e、plugin-live、soak（它们由 M5–M7 的任务交付）。
- **可配置性照旧兑现**（CR26D-10、-56、-103）：docs/03 的“Shift+Tab 循环（配置可改）”与 docs/06 的“MOA 最多两轮……为默认可调上限”都保留：REQ-018 的键表增加 `[ui] mode_cycle` 与 `[supervision] max_discussion_rounds`，AC-007-01 与 AC-023-03 各覆盖一个非缺省值。三个新键（含 `[assumptions]`）进入 `config/hoh.example.toml` 的时间，是加载器接受它们的同一提交（第 4 节）。

### 2. 首次批准之前的起草（C22：所有者 2026-11-17 选 (a)；CR26D-01、-50、-100、-150、-151）

**要解决的**：一个还没有已批准规格的项目里，Plan 的首次起草在哪里发生——由谁起草、怎样派发、完成、取消与恢复，Run 在起草与批准前后处于什么状态，第一次批准时身份怎样移动——同时守住 StartRun 的 `runtime:no_spec`（所有者 2026-11-16 的 Q1(a)）、O1（只有用户批准）、M2T13D-59（specs 只有已批准修订）与 CONTRACT 第 4 条（Plan 对话、生成规格，用户批准的是这份规格）。

**受锁与已确认的约束**（715e2d0）：

- `contracts/sql/001_initial.sql`：`specs` 没有批准列，`revision>0`、`UNIQUE(project_id, revision)`；`runs.spec_id` 可空（受锁注释：“NULL until SPECIFYING produces the first spec revision”）；`tasks.spec_id NOT NULL`、`attempts.task_id NOT NULL`、`leases.attempt_id NOT NULL`、`effects.attempt_id NOT NULL`；`change_requests.base_spec_id NOT NULL`。受锁注释给出的演进方式是“new numbered migration + user_version bump + round-trip/rollback tests”。
- 采用取最新一行（persistence/adoption.rs:32，`ORDER BY revision DESC LIMIT 1`），所以任何 specs 行都会被当成已批准规格采用。
- 没有规格的 Run 不被监督：监督步骤把到期的触发器延后为 `supervisor.deferred{NO_SPEC}`（persistence/supervise.rs:913-929），唤醒分配只选 `spec_id IS NOT NULL` 的 Run（persistence/wakes.rs:849），停滞探测也只看有规格的 Run（persistence/runner.rs:563-567）。调度轮只在 RUNNING、VERIFYING 派发（persistence/rounds.rs:40）。运行器的派发通知接受 SPECIFYING 中的 PREPARED Attempt（persistence/runner.rs:445-462）。
- Run 的边是 CREATED→DISCOVERING→SPECIFYING（lifecycle.rs:157-183），没有 SPECIFYING 回到 DISCOVERING 的边；今天没有代码把 Run 推进到 DISCOVERING；no_spec 的 adopt 出口在仍没有规格时保持问题、Run 留在 BLOCKED（questions.rs:672-679）。

**所有者的决定**（2026-11-17）：取 (a)。首次批准之前的起草作为 spec_id 为空的 Task 运行，经一个受守护的 SQL 迁移，只有起草 Task 可以没有规格；Plan 对话，由 Agent 起草规格；草稿只经用户的 ApproveSpec 成为修订 1；Q1(a) 与 `-p` 停在 no_spec 不变。

**未采用**：(b) 起草不是 Task（例如 Commander 的监督 turn）——不可行：runner 的每个 Attempt 都要 Task，绕过 runner 则失去租约、取消阶梯、终结证明与 kill -9 恢复；(c) 不经 Agent 的模板骨架先由用户批准为修订 1（第 2 稿的推荐）——改 CONTRACT 第 4 条的交互（CR26D-150），批准前的写入也没有租约与效果台账的归属（CR26D-100）；(d) 首个规格由用户自己写——空项目里 Plan 不能生成规格，是范围缩减。

**(a) 端到端**：

1. **没有已批准规格**（阶段一）：StartRun（任何模式）照旧以 `runtime:no_spec` 阻塞：BLOCKED(NEED_DECISION)、resume_state=CREATED、`runs.spec_id` 为空，没有 Task 与 Attempt；`-p` 以 3 退出（Q1(a) 不变）。no_spec 问题的选项从 `[adopt]` 变为 `[adopt, draft]`。
2. **draft**（阶段二）：用户以 `draft` 答复（TUI，或 `hoh -p draft --resume <run_id>`）。项目此时已有已批准修订，则 draft 与 adopt 相同（采用最新修订）；否则答复释放 no_spec 声明，Run 回到 CREATED（resume_state），`runs.spec_id` 仍为空。Runtime 推进常规阶段（CONTRACT 第 6 条“常规阶段自动流转”，ADR-031 交给 M3 的 :497）：CREATED→DISCOVERING，做只读的项目发现并写 `run.baseline`（REQ-028 行为 1：先于该 Run 的任何写入），再 DISCOVERING→SPECIFYING；进入 SPECIFYING 的同一事务建首个起草 Task 并写 `spec.drafting{run_id, task_id}`。
3. **起草 Task**：`kind='spec_draft'`、职位 cto、writer、`spec_id` 为空、没有 `task_requirements`（不经 `create_tasks` 与 `plan::check`）。它按 Runtime 规则类派发，与监督 Task 同形（persistence/wakes.rs:1041 的 LEASED Task 加 PREPARED Attempt 与运行器通知），但持项目的**写者**租约：同一项目的两个起草 Run 由写者租约串行。首个起草 Attempt 的准备阶段在非 git 目录中本地 `git init`（无远程，REQ-028 行为 5），随后实例化规格骨架与有效档的模板（REQ-029 行为 7，M3-T08），都记为该 Attempt 的 effect（效果台账，REQ-041）。取消阶梯、终结证明、工作区记录与 kill -9 之后的 reconcile 同其他写 Task。首次批准之前 Run 没有规格，Commander 不被唤醒、停滞探测不看它（上面的约束照旧），起草由看门狗（ADR-022 决定 19）与墙钟期限约束；起草 Agent 需要用户回答时以 NEED_DECISION 的问题停下（Run 为 BLOCKED、resume_state=SPECIFYING）。
4. **完成**：起草 Task 的完成不冻结候选、不要求 AC（host 的完成按 kind 分支，先于写 Task 的“没有 AC 即 no_acceptance_criteria”，persistence/completion.rs:560）。Runtime 先以 Git 核对写集（REQ-008 行为 5：docs/spec/、docs/、.hoh/），再要求起草结果已提交（未提交时与冻结拒绝一样请它更正），并从该提交的 Markdown 重新生成索引、与提交中的 index.json 逐字节相同。通过后 Task 为 IMPLEMENTED，Runtime 以 Run 级问题 `spec_approval{spec_sha256, git_commit}` 请用户批准：Run 为 BLOCKED(NEED_DECISION)、resume_state=SPECIFYING；此时 specs 仍没有行。
5. **批准**（阶段三）：修订只经用户的 ApproveSpec 产生。用户在 TUI 或经 API 提交 ApproveSpec；以 AnswerQuestion 答复 `approve` 时，host 先做同一对端判定，再把它转为该用户的 ApproveSpec 命令（commands 中记为 ApproveSpec，spec_sha256 取问题中的值，幂等键由答复命令派生）。项目还没有已批准修订时，一个事务写修订 1（`spec.changed{cause:"approved"}`、requirements 与 acceptance_criteria 行）、使该 Run 采用它（`runs.spec_id`）、起草 Task 为 ACCEPTED、释放问题、Run 回到 SPECIFYING；此后 Commander 被唤醒，独立规格审查、readiness 与任务 DAG 照 REQ-031 进行。项目已有已批准修订时（例如另一个起草 Run 先被批准），该 Run 先采用当前修订，起草结果以 CR 提出（REQ-033，base 为当前修订）。REJECT（或其他答复）使起草 Task 回到 READY，下一个 Attempt 以 note 为输入继续起草。之后的起草 Task（PLAN、AUTO 中改规格）的 `spec_id` 为当前已批准修订，完成时提出 CR。
6. **无人值守**：`hoh -p` 在阶段一以 3 退出；`hoh -p draft --resume <run_id>` 跟随起草直到 `spec_approval` 问题，以 3 退出，永不自批；用户随后在 TUI 中批准，或以 `hoh -p approve --resume <run_id>` 答复（转为 ApproveSpec；对端判定照旧：从 Attempt 内运行的这条命令被拒）。
7. **取消与恢复**：CancelRun 走起草 Attempt 的取消阶梯，起草 Task 为 CANCELLED、Run 为 CANCELLED，已写的规格文件留在工作树与提交中，不还原。host 被 kill -9 时，重启后起草 Attempt 照 REQ-041 reconcile：不重复建起草 Task（`spec.drafting` 按 run 去重），已记录的 effect（git init、模板实例化）不重复执行；批准问题与 specs 都是持久的，重启前后不变。
8. **保证**：Q1(a) 的 no_spec 与 `-p` 的 3 不变；没有 specs 行先于用户批准（M2T13D-59）；只有用户批准（O1）；规格由 Plan 的起草 Agent 在对话中生成、用户批准的就是它（CONTRACT 第 4 条）；没有规格的 Task 只能是起草 Task，且只在项目还没有已批准修订时（迁移 002 的 CHECK 与触发器）。AC-008-02 端到端观察三个阶段与 kill -9；AC-008-05 观察 O1 的反例与正例；AC-028-04 观察基线先于写入与首个提交的路径边界；AC-029-03 观察离线实例化与 `-p` 停在批准问题。

**迁移 002**（`contracts/sql/002_spec_drafting.sql`，新增并受锁）：

```sql
-- Migration 2 (CR-026, C22 (a)): before a project's first approved specification revision, its drafting Task has no
-- specification (spec_id NULL); every other Task keeps one. tasks gains kind: 'spec_draft' for the Runtime's drafting
-- Tasks, 'work' for every other Task and every existing row. A NOT NULL cannot be dropped in place, so tasks is rebuilt
-- by the documented table-rebuild steps; the migration runner runs every migration with foreign_keys off and commits
-- it only if PRAGMA foreign_key_check returns no row (docs/cr/CR-026-m3-detailed-spec.md, section 2).
BEGIN;
CREATE TABLE tasks_new(task_id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(run_id), iteration_id TEXT REFERENCES iterations(iteration_id), spec_id TEXT REFERENCES specs(spec_id), state TEXT NOT NULL CHECK(state IN ('PENDING','READY','LEASED','RUNNING','IMPLEMENTED','VERIFYING','ACCEPTED','BLOCKED','CANCELLED')), revision INTEGER NOT NULL DEFAULT 0 CHECK(revision>=0), role TEXT NOT NULL, writer INTEGER NOT NULL DEFAULT 0 CHECK(writer IN (0,1)), created_seq INTEGER NOT NULL, user_value INTEGER NOT NULL DEFAULT 0 CHECK(user_value BETWEEN 0 AND 5), unblock_value INTEGER NOT NULL DEFAULT 0 CHECK(unblock_value BETWEEN 0 AND 5), risk_reduction INTEGER NOT NULL DEFAULT 0 CHECK(risk_reduction BETWEEN 0 AND 5), information_gain INTEGER NOT NULL DEFAULT 0 CHECK(information_gain BETWEEN 0 AND 5), cost_estimate INTEGER NOT NULL DEFAULT 3 CHECK(cost_estimate BETWEEN 1 AND 5), layer INTEGER NOT NULL DEFAULT 2 CHECK(layer IN (0,1,2)), wait_rounds INTEGER NOT NULL DEFAULT 0 CHECK(wait_rounds>=0), first_eligible_seq INTEGER, attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count>=0), next_retry_at TEXT, blocked_reason TEXT, kind TEXT NOT NULL DEFAULT 'work' CHECK(kind IN ('work','spec_draft')), CHECK(spec_id IS NOT NULL OR kind='spec_draft'), CHECK(kind<>'spec_draft' OR writer=1));
INSERT INTO tasks_new(task_id, run_id, iteration_id, spec_id, state, revision, role, writer, created_seq, user_value, unblock_value, risk_reduction, information_gain, cost_estimate, layer, wait_rounds, first_eligible_seq, attempt_count, next_retry_at, blocked_reason, kind) SELECT task_id, run_id, iteration_id, spec_id, state, revision, role, writer, created_seq, user_value, unblock_value, risk_reduction, information_gain, cost_estimate, layer, wait_rounds, first_eligible_seq, attempt_count, next_retry_at, blocked_reason, 'work' FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;
CREATE INDEX tasks_run_state ON tasks(run_id,state,created_seq);
CREATE INDEX tasks_retry ON tasks(next_retry_at) WHERE next_retry_at IS NOT NULL;
-- A Task may lack a specification only while its project has no approved revision.
CREATE TRIGGER tasks_spec_draft_insert BEFORE INSERT ON tasks WHEN NEW.spec_id IS NULL AND EXISTS(SELECT 1 FROM specs s JOIN runs r ON r.project_id=s.project_id WHERE r.run_id=NEW.run_id) BEGIN SELECT RAISE(ABORT,'SPEC_DRAFT: the project has an approved specification'); END;
-- A Task keeps its kind and whether it has a specification; a non-NULL spec_id may move to a newer revision (CR-018).
CREATE TRIGGER tasks_spec_draft_update BEFORE UPDATE OF spec_id, kind ON tasks WHEN NEW.kind IS NOT OLD.kind OR ((OLD.spec_id IS NULL) <> (NEW.spec_id IS NULL)) BEGIN SELECT RAISE(ABORT,'SPEC_DRAFT: a task keeps its kind and whether it has a specification'); END;
INSERT INTO schema_migrations VALUES(2,'2026-11-17T00:00:00Z');
PRAGMA user_version = 2;
COMMIT;
```


迁移运行器（`crates/hoh-core/src/persistence/mod.rs:653-689`，不在锁内）随之改变，属 M3-T04：对每个迁移，`BEGIN IMMEDIATE` 之前 `PRAGMA foreign_keys=OFF`，脚本之后、`COMMIT` 之前执行 `PRAGMA foreign_key_check` 且必须无行（否则回滚为 Migration 错误），之后无论成败恢复 `foreign_keys=ON` 并核对它为 1。原因：SQL 不能原地去掉 NOT NULL，只能按文档的重建步骤重建 tasks；今天的运行器在外键开启的事务里执行，`DROP TABLE tasks` 会因 attempts 等子表的行以 “FOREIGN KEY constraint failed” 失败（探针 `.hoh-dev/evidence/m3-prep/cr026/draft3/migration-probe.json` 的 `fk_on_runner`）。按上述运行器执行时，探针（Python 的 SQLite 3.46.1；产品捆绑的版本由 M3-T04 的迁移测试覆盖）观察到：各表行与值不变、新列 `kind` 全为 `work`、`integrity_check` 为 ok、`foreign_key_check` 为空、两个索引与两个触发器都在、子表的外键仍指向 tasks 且照常执行；守护：`kind='work'` 而 `spec_id` 为空被 CHECK 拒，`spec_draft` 而非写者被 CHECK 拒，项目已有 specs 行时插入 `spec_id` 为空的 Task 被触发器拒，`spec_id` 在空与非空之间变化或 `kind` 变化被触发器拒，非空的 `spec_id` 改为更新的修订（CR-018 的 Task 迁移）照常；没有规格的起草 Task 可以有 Attempt、写者租约与 effect。

**今天按 `tasks.spec_id` 非空写的代码路径**（715e2d0 上对全部非测试 Rust 源中同时提到 tasks 与 spec_id 的 SQL 文本的检索，加上它们的 Rust 侧解码；M3-T04 逐条处理）：

| 位置 | 今天的假设 | (a) 下的处理 |
|---|---|---|
| `persistence/decide.rs:764-786` `load_task`，解码进 `decision.rs:213` 的 `TaskView.spec_id: String`，`decision.rs:580` 的 SPEC_MISMATCH 判定 | 非空（空值解码失败） | 改为 `Option<String>`；Commander 的决策点名起草 Task 时拒绝（起草 Task 只由 Runtime 推进） |
| `persistence/rounds.rs:323-366` 调度轮候选扫描（`t.spec_id` 解码为 String），`:401` 的 `current_spec` | 非空 | 扫描只取 `kind='work'`（起草 Task 永不经调度轮），解码为 Option |
| 插入：`persistence/plan.rs:242`、`verification.rs:215`、`wakes.rs:1041`、`writer.rs:1154` | 传入非空 spec_id | 不变（`kind` 取缺省 `work`）；起草 Task 由新的插入路径建 |
| 经 `task_requirements` 的连接：`persistence/completion.rs:549-550`、`context.rs:123-124`、`verification.rs:90`、`:352-353`、`closing.rs:824`、`join.rs:312`、`snapshot.rs:175` | 用 `task_requirements.spec_id`，不读 `tasks.spec_id` | 起草 Task 没有行，结果为空；写 Task 的完成在 `completion.rs:560` 会以 no_acceptance_criteria 阻塞，所以起草 Task 的完成在 host 中先分支（第 4 步） |
| Run 级的规格过滤：`wakes.rs:849`、`supervise.rs:913-929`、`runner.rs:563-567` | Run 有规格才分配唤醒、才监督、才探测停滞 | 有意不变：首次批准之前不监督（第 3 步） |
| `context.rs:29-42`（Run 的规格经 LEFT JOIN 读为 Option） | 已处理空值 | 不变：起草 Attempt 的提示没有规格一节 |
| `persistence/mod.rs:629-647` `allowed_values`（取第一个含 `CREATE TABLE tasks(` 的迁移） | 只有迁移 1 | 迁移 002 建的是 `tasks_new`，取到的仍是 001 的 state 列表，与 002 相同；不变 |
| `persistence/mod.rs:653-689` `migrate` | 外键开启的事务内执行 | 按上文改；`persistence/tests.rs` 中按 user_version=1 断言的测试（:109、:178、:207 等）随最新版本变为 2 而更新 |

**与 2026-11-16 未采用的 Q1(b) 的区别**（Q1(b) 原文与所有者的取舍见 `.hoh-dev/evidence/m3-prep/cr026/draft2/q1b.md`）：

| 方面 | Q1(b)（2026-11-16 未采用） | C22 (a) |
|---|---|---|
| 谁触发 | host，每个 Run（含 `-p`）自动 | 用户在 no_spec 问题上答复 `draft` |
| 何时有 specs 行 | 立即（host 写） | 只在用户以 ApproveSpec 批准起草结果之后 |
| `-p` | 不再停在 no_spec | 照旧以 3 停在 no_spec；draft 之后停在批准问题 |
| 修订 1 的内容 | 目标摘要、无需求 | Plan 起草 Agent 生成、用户批准的规格 |
| 修订 1 有无 AC | 无（completion 以 no_acceptance_criteria 阻塞） | 由起草与用户的批准决定；readiness 照常判定（缺 AC 即不就绪） |
| 经 Agent 生成吗 | 否 | 是（Plan 对话） |
| 受锁 SQL | 不改 | 迁移 002 |

### 3. 补齐的原因码（`contracts/protocol.schema.json`，CR-003 D1 第 1 条）

追加到各自的 `enum` 末尾（旧：无），`$defs.BlockedReason.description` 末尾追加一句（旧：无）：

```json
{
  "ErrorCode": [
    "SPEC_CONFLICT",
    "SPEC_INVALID",
    "SPEC_APPROVAL_NOT_USER",
    "PROMPT_VARIABLE_MISSING",
    "PROMPT_TOO_LONG"
  ],
  "RejectionReason": [
    "CONSULT_CYCLE",
    "DISCUSSION_LIMIT"
  ],
  "BlockedReason": [
    "READINESS_NOT_MET"
  ]
}
```

`$defs.BlockedReason.description` 末尾追加：

> READINESS_NOT_MET (CR-026) is a task's blocked_reason only: the task's requirement or its dependency closure is not ready (REQ-031); it never blocks a run.


| 码 | 枚举 | 用处 |
|---|---|---|
| SPEC_CONFLICT | ErrorCode（409） | ApproveSpec 的摘要与重新生成的索引不同，或 CR 的 base 不是当前已批准修订（REQ-034、REQ-033） |
| SPEC_INVALID | ErrorCode（422） | 提议或送批的规格无法解析、index.json 与 Markdown 不一致（冻结时），或标为用户决定的条目没有来源（REQ-034、REQ-030） |
| SPEC_APPROVAL_NOT_USER | ErrorCode（403） | 批准来自 Attempt 的进程、它的后代、Attempt cgroup 中的进程，或对端无法确认（REQ-008，O1） |
| PROMPT_VARIABLE_MISSING | ErrorCode | 模板引用白名单之外的变量，Attempt 不启动（REQ-045） |
| PROMPT_TOO_LONG | ErrorCode | 职位契约覆盖超过 32 KiB，保存即拒（REQ-045） |
| CONSULT_CYCLE | RejectionReason | 角色间咨询成环（REQ-023） |
| DISCUSSION_LIMIT | RejectionReason | 超过讨论轮数上限（REQ-023） |
| READINESS_NOT_MET | BlockedReason（只用于 Task） | Task 的需求或依赖闭包不就绪（REQ-031、REQ-009） |

### 4. REQ-018 的配置键表

`contracts/requirements.json` 的 REQ-018，`decisions` 末尾追加三条（旧：无；REQ-018 的“完整配置键表（未知键一律拒绝）”由 CR-003、CR-007 给出，本条只增加键，不改已有键与缺省值；`project.document_profile` 的缺省仍为 full）：

> （CR-026）[assumptions] 表：键为决定类名（ASCII 小写字母、数字与下划线），值为字符串或字符串数组，是无人值守时允许假设的默认值（REQ-030，所有者 2026-11-17 的决定 O2）；product_direction、scope、non_goals 为保留名，出现即配置被拒（这三类永不假设）；config/hoh.example.toml 中为空表（与加载器的支持在同一提交加入，见 M3-T06），空表不产生任何假设；表外的未知键照旧拒绝。
>
> （CR-026）[ui] mode_cycle：Shift+Tab 循环的模式顺序（docs/03 的“配置可改”），缺省 ["PLAN", "BUILD", "AUTO"]，合法值为 2 或 3 个互不相同的 PLAN、BUILD、AUTO 组成的数组，其他值使配置被拒（REQ-007）；example 中的缺省值与加载器的支持在同一提交加入（M3-T09）。
>
> （CR-026）[supervision] max_discussion_rounds：同一争议的意见轮数上限（docs/06 的“默认可调上限”），缺省 2，合法值为 1–5 的整数，其他值使配置被拒（REQ-023）；example 中的缺省值与加载器的支持在同一提交加入（M3-T10）。

**本 CR 不改 `config/hoh.example.toml`**（CR26D-103）。今天的加载器（`crates/hoh-core/src/config.rs`）拒绝未知键，AC-018-01 要求“无配置等于 example”：在加载器接受新键之前把它们写进 example，会使 TEST-018-01 失败、使复制官方 example 的用户无法启动。所以每个键的解析、合法值校验、缺省值与进入 Run 配置快照的支持，与它在 example 中的条目在同一提交加入，并在该提交上重跑 TEST-018-01：`[assumptions]` 在 M3-T06，`[ui] mode_cycle` 在 M3-T09，`[supervision] max_discussion_rounds` 在 M3-T10（三个任务的 done_when）。M3-T01 relock 之后、这三个任务之前，REQ-018 的键表列出的是目标，example 与加载器都不变，TEST-018-01 照旧通过（“验证方式”第 4 条在应用本稿的副本上运行了它）。

### 5. 新增的受锁契约

1. `contracts/spec-review-report.schema.json`（独立规格审查的报告，REQ-031 行为 3；`relock --add`）。它与 `contracts/independent-report.schema.json` 同形，只把 `candidate_id` 换成规格修订的 `spec_revision`、`spec_sha256` 与 `git_commit`，`role` 只能是 reviewer；`independent-report.schema.json` 不改：

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "urn:hoh:spec-review-report:1.0",
  "description": "Independent review of an approved specification revision, not of a candidate (REQ-031 behaviour 3, CR-026): bound to the revision's id, digest and specification commit; collected as spec_review.reported, never as an independent_reports row.",
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "schema_version": {
      "const": "1.0"
    },
    "role": {
      "const": "reviewer"
    },
    "session_id": {
      "type": "string",
      "minLength": 1
    },
    "spec_revision": {
      "type": "integer",
      "minimum": 1
    },
    "spec_sha256": {
      "type": "string",
      "pattern": "^[a-f0-9]{64}$"
    },
    "git_commit": {
      "type": "string",
      "pattern": "^([a-f0-9]{40}|[a-f0-9]{64})$"
    },
    "status": {
      "enum": [
        "PASS",
        "FAIL",
        "UNKNOWN"
      ]
    },
    "scope": {
      "type": "array",
      "minItems": 1,
      "items": {
        "type": "string",
        "minLength": 1
      }
    },
    "findings": {
      "type": "array",
      "items": {
        "$ref": "#/$defs/Finding"
      }
    },
    "raw_session": {
      "$ref": "#/$defs/FileRef"
    },
    "native_attempt": {
      "$ref": "#/$defs/FileRef",
      "description": "Attempt record of the native agent run that produced this report. Its session_ids must contain session_id."
    }
  },
  "required": [
    "schema_version",
    "role",
    "session_id",
    "spec_revision",
    "spec_sha256",
    "git_commit",
    "status",
    "scope",
    "findings",
    "raw_session",
    "native_attempt"
  ],
  "$defs": {
    "FileRef": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "path",
        "sha256"
      ],
      "properties": {
        "path": {
          "type": "string",
          "minLength": 1,
          "pattern": "^(?!/)"
        },
        "sha256": {
          "type": "string",
          "pattern": "^[a-f0-9]{64}$"
        }
      }
    },
    "Finding": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "id",
        "severity",
        "summary"
      ],
      "properties": {
        "id": {
          "type": "string",
          "minLength": 1
        },
        "severity": {
          "enum": [
            "critical",
            "high",
            "medium",
            "low"
          ]
        },
        "summary": {
          "type": "string",
          "minLength": 1
        },
        "evidence": {
          "type": "array",
          "items": {
            "$ref": "#/$defs/FileRef"
          }
        }
      }
    }
  }
}
```


2. `contracts/sql/002_spec_drafting.sql`（C22 的决定 (a)；全文见第 2 节；`relock --add`）。
3. `scripts/product/persistence-whitelist.json`（不在锁内）：迁移 002 含 SQL 写语句，persistence_scan 的第 6 项要求它在白名单中，与 001 的条目同理：

`files` 中加入（按键排序插入）：

```json
{
 "contracts/sql/002_spec_drafting.sql": "the schema's migration 2 (CR-026, C22 (a)); embedded and run only by the Persistence module (MIGRATIONS, from M3-T04)"
}
```


### 6. 各需求的细化

每项给出替换后的整个需求对象与它的全部场景。AC 的 id 与 TEST 的 id 按 validate_bundle 的规则依次编号；每项原有的 AC-xxx-01 保留 id、改写为可判定的内容（原文的覆盖关系见“不变的部分”）。

#### REQ-034 文档事实源（4 条 AC）

`contracts/requirements.json` 中 REQ-034 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-034-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-034",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "文档事实源",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "文件是语义事实源，DB是索引/状态；冲突可检测；生成页不产生第二权威源",
  "behavior": [
    "用户项目的规格是 Git 工作树中规格根 docs/spec/ 下的 Markdown 文件（O4）：固定一级标题（用户、范围、非目标、决策、开放问题、需求），需求为“## REQ-<id> <标题>”，验收为“### AC-<req>-<nn>”并带 given、when、then、check 四个 id 行；精确语法由 M3-T02 的 ADR 规定，HoH 内置模板给出（REQ-029）。",
    "规格的冻结形式是生成的规格索引 docs/spec/index.json（随规格一起提交、被 Git 跟踪）：由 HoH 从规格根的 Markdown 文件确定性生成的规范 JSON（UTF-8、LF、键按字典序、两空格缩进、末尾一个换行），含 generated 标记、files（每份 Markdown 文件的路径与 sha256，按路径排序）、requirements（与 contracts/requirements.json 中需求对象同形的需求与验收；每条 AC 带 test_id=TEST-<req>-<nn>；追踪读取的 release_class 取需求的 class 行，缺省 required，release_targets 恒为 [\"project\"]：用户项目只有一个发布目标）与 scenarios（每条 AC 一项 {id: 它的 test_id, requirement_id, acceptance_id, check_id}，check_id 取该 AC 的 check 行，按 id 排序）。规格摘要就是这个文件字节的 sha256；任一 Markdown 文件的任一字节变化都改变 files 中的摘要，从而改变规格摘要。",
    "已批准修订是经 ApproveSpec 批准、sha256 为该修订规格摘要的 specs 行；specs 只有已批准修订（M2T13D-59，ADR-031 决定 1），只有已批准修订被采用、派发、冻结与验收引用。DB 只是索引与状态：已批准修订的 requirements、acceptance_criteria 行由 HoH 从该修订的索引写入，acceptance_criteria.check_id 取 scenarios 中该 AC 的 check_id。",
    "ApproveSpec 的 spec_sha256 必须等于 HoH 在提交时从工作树重新生成的索引的 sha256，且工作树中已跟踪的 index.json 与之逐字节相同；不等即冲突（SPEC_CONFLICT），什么也不写。",
    "冻结与验证以 index.json 为候选的 spec 文件（candidate.spec 为单个 FileRef，candidate.spec_sha256 为它的 sha256）：冻结时 HoH 从该提交的 Markdown 重新生成索引并要求与提交中的 index.json 逐字节相同，否则拒绝冻结（SPEC_INVALID）；traceability 从它的 requirements 读需求、从它的 scenarios 读 TEST→check（REQ-032）。它替换 M2 的约定路径 contracts/requirements.json（ADR-028 决定 4，CR-018），用户项目不需要 contracts/ 目录。",
    "索引在提议与批准时构建；Markdown 无法解析时返回 422 SPEC_INVALID 并写 spec.index_failed{path, line, error}，不写 change_requests 也不写新修订，派发依据仍是上一已批准修订，禁止基于半更新的索引派发。",
    "index.json 等生成页带“生成，勿改”标记与来源摘要，不是事实源（Markdown 才是）；生成时发现它被人工修改，写 spec.generated_drift{path} 并按 Markdown 重新生成。"
  ],
  "failure_modes": [
    "ApproveSpec 的 spec_sha256 与重新生成的索引不同，或已跟踪的 index.json 与之不同，返回 409 SPEC_CONFLICT，details 含 expected 与 actual 两个摘要，specs、requirements、acceptance_criteria 与 runs 都不变。",
    "Markdown 无法解析时，索引不变（仍为上一已批准修订），写 spec.index_failed，不派发依据未批准内容的任务。",
    "提交中的 index.json 与其 Markdown 不一致时拒绝冻结（SPEC_INVALID），没有候选。",
    "规格根不存在或没有已批准修订时，项目视为没有规格（StartRun 以 runtime:no_spec 阻塞，runs.spec_id 为空，ADR-031 决定 1），不读任何约定路径。"
  ],
  "doc_refs": [
    "docs/05-data-api.md",
    "docs/21-spec-governance.md",
    "docs/15-prompts-skills-memory.md",
    "docs/adr/ADR-028.md",
    "docs/adr/ADR-031.md",
    "contracts/candidate.schema.json",
    "contracts/sql/001_initial.sql"
  ],
  "acceptance": [
    {
      "id": "AC-034-01",
      "given": "夹具项目 docs/spec/ 下两份 Markdown 规格共 3 条 REQ、7 条 AC，与 HoH 生成的 docs/spec/index.json 一起提交于 C1；项目尚无已批准修订",
      "when": "用户以 ApproveSpec{spec_sha256=index.json 的 sha256, decision:APPROVE_ONLY} 批准，随后冻结一个候选",
      "then": "specs 中有 revision=1 的已批准修订，sha256 等于 index.json 字节的 sha256，index.json 的 files 列出两份 Markdown 的路径与 sha256；requirements 3 行、acceptance_criteria 7 行且 id 与文件一致；index.json 的 scenarios 有 7 项，每项的 check_id 等于对应 AC 的 check 行，acceptance_criteria.check_id 与之逐条相同；candidate.spec 为 {path:docs/spec/index.json, sha256} 且 candidate.spec_sha256 与之相同；冻结进程未打开 contracts/requirements.json",
      "test_id": "TEST-034-01"
    },
    {
      "id": "AC-034-02",
      "given": "已批准修订 1（摘要 S1）；用户把一份 Markdown 改动后重新生成索引（摘要 S2），未提出变更",
      "when": "以 ApproveSpec{spec_sha256=S1} 提交批准",
      "then": "返回 409，code=SPEC_CONFLICT，details 含 expected=S1 与 actual=S2；specs、requirements、acceptance_criteria 与 runs 行逐值不变；工作树文件不变",
      "test_id": "TEST-034-02"
    },
    {
      "id": "AC-034-03",
      "given": "已批准修订 1 的 Run R 处于 RUNNING、有 READY 的 Task；用户把一份规格文件改坏（AC 缺 then 行）",
      "when": "以 ProposeSpecChange 提议，调度轮再运行三轮；随后修好文件，经 ProposeSpecChange 与 ApproveSpec 批准",
      "then": "坏文件的提议返回 422 SPEC_INVALID、写 spec.index_failed{path, line, error}，没有 change_requests 行与新的已批准修订，三轮内派发依据仍是修订 1；修好后经 CR 得到修订 2 的已批准修订与重建的索引",
      "test_id": "TEST-034-03"
    },
    {
      "id": "AC-034-04",
      "given": "已批准修订 1，已跟踪的 docs/spec/index.json 带“生成，勿改”标记与来源摘要",
      "when": "用户手工改 index.json 中一条 AC 的文字并提交，随后先冻结一个候选、再触发生成",
      "then": "冻结被拒（SPEC_INVALID，details 指出 index.json 与 Markdown 不一致），没有候选；生成时写 spec.generated_drift{path}，index.json 恢复为按 Markdown 生成的内容；specs、requirements、acceptance_criteria 不变",
      "test_id": "TEST-034-04"
    }
  ],
  "decisions": [
    "规格格式与位置取所有者 2026-11-17 的决定 O4：Markdown 放 docs/spec/，固定标题与 id 行，HoH 生成 JSON 索引（CR-026）。",
    "冻结形式取生成的 index.json：多份 Markdown 在候选中由一个 FileRef 表示，候选、验证与追踪的受锁契约都不需要改（candidate.schema.json 的 spec 仍是单个 FileRef；索引的 requirements 与 contracts/requirements.json 同形）（CR-026，CR26D-02）。",
    "索引带 scenarios（TEST→check）：acceptance_criteria.check_id 与追踪都从它读，候选仍只有一个 spec FileRef，spec_sha256 覆盖它（CR-026，CR26D-102）。",
    "冲突以新码 SPEC_CONFLICT（409）报告，不复用 CONFLICT：后者指命令 id 冲突（CR-026，C1）。",
    "生成页漂移时以 Markdown 为准覆盖并记录，不尝试合并人工修改（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-034 的场景（旧：TEST-034-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-034-01",
    "requirement_id": "REQ-034",
    "acceptance_id": "AC-034-01",
    "check_id": "integration",
    "fixture": "临时 git 项目（docs/spec/ 两份 Markdown 与生成的 index.json）+ 产品 host + ApproveSpec + 冻结",
    "steps": [
      "提交规格文件与索引",
      "ApproveSpec{spec_sha256=索引摘要, decision:APPROVE_ONLY}",
      "冻结候选",
      "记录冻结进程打开的文件"
    ],
    "pass_criteria": [
      "specs 1 行且 sha256 等于 index.json 的 sha256",
      "index.json 的 files 列出两份 Markdown",
      "requirements 3 行、acceptance_criteria 7 行",
      "scenarios 7 项且 check_id 与 AC 的 check 行及 acceptance_criteria.check_id 相同",
      "candidate.spec 与 spec_sha256 指向 index.json",
      "未打开 contracts/requirements.json"
    ],
    "observables": [
      "specs/requirements/acceptance_criteria 导出",
      "index.json 内容",
      "candidate.frozen 载荷",
      "冻结进程的文件访问记录",
      "判定结果"
    ]
  },
  {
    "id": "TEST-034-02",
    "requirement_id": "REQ-034",
    "acceptance_id": "AC-034-02",
    "check_id": "integration",
    "fixture": "临时 git 项目 + 产品 host，已批准修订 1",
    "steps": [
      "改一份 Markdown 并重新生成索引",
      "ApproveSpec{spec_sha256=S1}"
    ],
    "pass_criteria": [
      "HTTP 409 且 code=SPEC_CONFLICT，details 含两个摘要",
      "四张表逐值不变",
      "工作树文件不变"
    ],
    "observables": [
      "HTTP 状态与错误体",
      "表导出前后对比",
      "文件摘要"
    ]
  },
  {
    "id": "TEST-034-03",
    "requirement_id": "REQ-034",
    "acceptance_id": "AC-034-03",
    "check_id": "integration",
    "fixture": "临时 git 项目 + 产品 host + FakeAgent，Run R RUNNING、Task READY",
    "steps": [
      "改坏一份规格文件并 ProposeSpecChange",
      "运行三轮调度",
      "修好并经 CR 批准"
    ],
    "pass_criteria": [
      "提议返回 422 SPEC_INVALID",
      "spec.index_failed 一条且载荷含 path、line、error",
      "无 change_requests 行与新修订",
      "三轮派发依据为修订 1",
      "修好后修订 2 与新索引"
    ],
    "observables": [
      "HTTP 状态与错误体",
      "events导出",
      "specs 与 change_requests 行",
      "调度轮派发记录",
      "判定结果"
    ]
  },
  {
    "id": "TEST-034-04",
    "requirement_id": "REQ-034",
    "acceptance_id": "AC-034-04",
    "check_id": "unit",
    "fixture": "规格生成器与冻结校验 + 临时 git 项目",
    "steps": [
      "手工改 index.json 并提交",
      "冻结",
      "生成"
    ],
    "pass_criteria": [
      "冻结被拒为 SPEC_INVALID 且无候选",
      "spec.generated_drift 一条",
      "index.json 恢复并带来源摘要与标记",
      "三张表不变"
    ],
    "observables": [
      "冻结结果",
      "生成文件内容",
      "漂移诊断",
      "判定结果"
    ]
  }
]
```

#### REQ-033 规格变更（4 条 AC）

`contracts/requirements.json` 中 REQ-033 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-033-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-033",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "规格变更",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "生成 CR、revision、影响闭包；受影响验收过期，不静默沿用旧PASS",
  "behavior": [
    "规格变更都走 CR：ProposeSpecChange（或 CHANGE 输入、或起草 Task 的完成，见 REQ-008）以当前已批准修订为 base，写一条 change_requests 行（PROPOSED），reason 为提议理由，impact_json 列出影响闭包；批准之前规格、Run 的 spec_id 与任务都不变。",
    "影响闭包按显式映射计算：被改动的 REQ（增删改）→ 其 AC → 其 TEST/check → 以 task_requirements 关联的 Task；不按文件路径猜测（C2）。",
    "批准（ApproveSpec，只由用户，O1）写新修订（revision+1）、CR 为 APPLIED、result_spec_id 指向新行，写 spec.changed{cause:\"changed\", from, to, cr_id}；REJECT 把 CR 置为 REJECTED，什么也不改。",
    "任何修订都使该 Run 全部验收结果失效（O6）：新修订下没有任何 accepted，直到在新冻结的候选上重新验收；旧结果保留为历史，显示为 STALE，不计入当前进度与完成门禁。规格摘要是 index.json 的 sha256（REQ-034），任一 Markdown 文件的任一字节变化都产生新修订与新摘要。",
    "验证进行中规格变化时分两步：批准事务内，验证中的候选写 candidate.stale{candidate_id, reason:\"spec_changed\"}、它的独立报告翻转为 STALE，并对其验证 Attempt 请求取消（cancel_requested）；轮次只在这些验证进程（含其 run-check 子进程）经取消阶梯证明已结束之后，以 superseded 关闭恰好一次（ADR-028 决定 11，closing.rs 的关轮规则）。取消之后到达的验证输出只记录为迟到，不计入任何结果；host 在其间崩溃时，重启的 reconcile 先证明进程已结束，再恰好一次关轮。",
    "影响闭包内、以 SPEC_MISMATCH 阻塞的写 Task，批准后迁移：仍有对应 REQ 的，task_requirements 改映射到新修订的 REQ 并写 task.migrated；对应 REQ 被删除的，Task 取消、由 Commander 计划的替代 Task 承接并写 task.replaced{from, to}。任何 Task 都不再在旧修订下派发。"
  ],
  "failure_modes": [
    "批准前，规格、runs.spec_id、tasks 与 acceptance_results 不变。",
    "以修订 n 的证据判修订 n+1 的 AC 时，投影与完成门禁（EVIDENCE_GATE_UNMET）都不计为 accepted，显示 STALE；已验收计数的分母随修订变化并显示 spec revision。",
    "CR 的 base 不是当前已批准修订时（期间已有别的批准），ApproveSpec 返回 409 SPEC_CONFLICT，CR 保持 PROPOSED。",
    "验证进程尚未证明结束时轮次不关闭；它们的迟到输出不改变任何验收结果。",
    "被删 REQ 的写 Task 没有替代时，它保持 CANCELLED，不在旧修订下重新派发。"
  ],
  "doc_refs": [
    "docs/21-spec-governance.md",
    "docs/07-algorithms.md",
    "docs/06-orchestration.md",
    "docs/03-ux.md",
    "docs/adr/ADR-028.md",
    "docs/adr/ADR-021.md",
    "docs/10-acceptance.md",
    "contracts/sql/001_initial.sql"
  ],
  "acceptance": [
    {
      "id": "AC-033-01",
      "given": "已批准修订 1 含 REQ-A（AC-A-01、AC-A-02，各有 TEST 并由 Task TA 关联）与 REQ-B；用户把 REQ-A 的 AC-A-02 改写",
      "when": "以 ProposeSpecChange{reason, spec_sha256=新摘要} 提议，查看 CR，然后以 ApproveSpec{decision:APPROVE_ONLY} 批准",
      "then": "批准前 change_requests 一行 PROPOSED、base_spec_id=修订 1、impact_json 恰含 REQ-A、AC-A-01..02、它们的 TEST 与 TA，不含 REQ-B 的任何项，specs 与 runs.spec_id 不变；批准后 specs 新增修订 2、CR 为 APPLIED 且 result_spec_id=修订 2、写 spec.changed{cause:\"changed\", from:1, to:2}",
      "test_id": "TEST-033-01"
    },
    {
      "id": "AC-033-02",
      "given": "Run R 的候选 C 在修订 1 下 VERIFYING：QA 的验证 Attempt 正在跑一个需时 60 s 的 run-check，Review 的验证 Attempt 正在运行",
      "when": "批准修订 2；QA 的假 Agent 在取消之后仍写出一份 PASS 报告；再在另一次运行中，于批准提交之后、验证进程结束之前 kill -9 host 并重启",
      "then": "批准事务内写 candidate.stale{candidate_id:C, reason:\"spec_changed\"}、C 的独立报告为 STALE、两个验证 Attempt 为 cancel_requested，轮次此时仍未关闭；两个验证进程与 run-check 子进程经取消阶梯证明结束之后，轮次以 superseded 关闭恰好一次；迟到的报告不产生任何 accepted；重启的一次也在 reconcile 证明进程结束后恰好一次关轮；修订 2 下 accepted AC 数为 0",
      "test_id": "TEST-033-02"
    },
    {
      "id": "AC-033-03",
      "given": "修订 1 下 REQ-A、REQ-B 的全部 AC 已 PASS 并 accepted；修订 2 与修订 1 只差 REQ-A 所在 Markdown 文件中的一个字符",
      "when": "批准修订 2，读 /v1/runs/{id}/acceptance 与 Run 的投影，随后 Commander 以 transition_run 申请 COMPLETED",
      "then": "修订 2 的摘要（index.json 的 sha256）与修订 1 不同；两者都显示修订 2 的全部 AC（含未改动的 REQ-B）为未验收，旧结果标 STALE 并可查；已验收计数为 0/修订 2 的 AC 总数并显示 spec revision 2；完成申请被拒（EVIDENCE_GATE_UNMET），Run 不是 COMPLETED",
      "test_id": "TEST-033-03"
    },
    {
      "id": "AC-033-04",
      "given": "修订 1 下写 Task T1（REQ-A）与 T2（REQ-C）以 SPEC_MISMATCH 阻塞；修订 2 改写 REQ-A、删除 REQ-C",
      "when": "批准修订 2，Commander 为 REQ-C 的去留计划替代 Task T3，调度轮运行",
      "then": "T1 的 task_requirements 改指修订 2 的 REQ-A 并写 task.migrated，T1 可派发；T2 为 CANCELLED 并写 task.replaced{from:T2, to:T3}；之后没有任何 Task 以修订 1 的 spec_id 派发",
      "test_id": "TEST-033-04"
    }
  ],
  "decisions": [
    "证据失效的粒度取所有者 2026-11-17 的决定 O6：任何修订使全部验收失效；影响闭包只决定要做的工作，不决定保留哪些 PASS；验收在新冻结的候选上重跑全部 required 检查（与 docs/07 §6 的发布全量一致）（CR-026）。",
    "影响闭包按 REQ→AC→TEST→TASK 的显式映射，不按路径（CR-026，C2）。",
    "CR 的记录是 change_requests 行；提议的规格内容就是工作树中的规格文件与索引，以摘要锁定，不另写 CR 文件（CR-026，C3）。",
    "验证中的规格变化先失效与请求取消、后关轮，关轮等进程结束证明（ADR-028 决定 11）（CR-026，CR26D-03）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-033 的场景（旧：TEST-033-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-033-01",
    "requirement_id": "REQ-033",
    "acceptance_id": "AC-033-01",
    "check_id": "integration",
    "fixture": "临时 git 项目 + 产品 host，修订 1 含 REQ-A/REQ-B 与关联 Task",
    "steps": [
      "改写 AC-A-02 并 ProposeSpecChange",
      "读 change_requests",
      "ApproveSpec{decision:APPROVE_ONLY}"
    ],
    "pass_criteria": [
      "批准前 CR 为 PROPOSED，impact_json 恰为 REQ-A 的闭包",
      "批准前 specs 与 runs.spec_id 不变",
      "批准后修订 2、CR APPLIED、spec.changed{cause:changed}"
    ],
    "observables": [
      "change_requests 行",
      "specs 行",
      "events导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-033-02",
    "requirement_id": "REQ-033",
    "acceptance_id": "AC-033-02",
    "check_id": "recovery",
    "fixture": "产品 host 进程 + FakeAgent 验证者（一个 60 s 的 run-check）+ kill -9 与重启",
    "steps": [
      "批准修订 2",
      "让 QA 假 Agent 在取消后写 PASS 报告",
      "另一次运行在批准后、进程结束前 kill -9 host 并重启"
    ],
    "pass_criteria": [
      "批准事务内 candidate.stale{reason:spec_changed}、报告 STALE、两个 Attempt cancel_requested，轮次未关",
      "进程与 run-check 子进程结束证明之后轮次 superseded 恰一次",
      "迟到报告无 accepted",
      "重启后 reconcile 证明结束再恰一次关轮",
      "修订 2 下 accepted 为 0"
    ],
    "observables": [
      "events导出（含时间顺序）",
      "rounds/attempts/independent_reports 行",
      "进程结束证明",
      "判定结果"
    ]
  },
  {
    "id": "TEST-033-03",
    "requirement_id": "REQ-033",
    "acceptance_id": "AC-033-03",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander：修订 1 全部 accepted 后批准只差一个字符的修订 2",
    "steps": [
      "批准修订 2",
      "读 acceptance API 与投影",
      "Commander 申请 COMPLETED"
    ],
    "pass_criteria": [
      "两个修订的摘要不同",
      "修订 2 的全部 AC 未验收、旧结果 STALE",
      "计数为 0/总数并显示 revision 2",
      "完成申请被拒（EVIDENCE_GATE_UNMET），Run 不是 COMPLETED"
    ],
    "observables": [
      "specs 行",
      "acceptance API 响应",
      "投影",
      "decisions 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-033-04",
    "requirement_id": "REQ-033",
    "acceptance_id": "AC-033-04",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander，T1/T2 以 SPEC_MISMATCH 阻塞",
    "steps": [
      "批准修订 2（改 REQ-A、删 REQ-C）",
      "Commander 计划 T3",
      "运行调度轮"
    ],
    "pass_criteria": [
      "T1 映射到修订 2 并 task.migrated，可派发",
      "T2 CANCELLED 并 task.replaced{from:T2,to:T3}",
      "无修订 1 的派发"
    ],
    "observables": [
      "tasks 与 task_requirements 行",
      "events导出",
      "调度轮派发记录",
      "判定结果"
    ]
  }
]
```

#### REQ-031 Readiness Gate（5 条 AC）

`contracts/requirements.json` 中 REQ-031 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-031-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-031",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "Readiness Gate",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "必要 AC 可观察、依赖可达、关键决定确定；仅阻断当前受影响范围",
  "behavior": [
    "Readiness 是已批准规格修订、它的独立规格审查与当前计划的纯函数，由 Runtime 自动判定（C5），结果为 READY 或 NOT_READY{reasons}；Commander 不能覆盖判定结果。",
    "不就绪的原因：范围内 required REQ 没有 AC（missing_ac）；AC 缺 then 或 check（unobservable）；计划的依赖不存在或成环（DAG_MISSING_DEPENDENCY、DAG_CYCLE）；开放问题中标为阻塞的未决（undecided，标记方式见 C4）；该修订没有可用的独立规格审查（spec_review）。",
    "独立规格审查（docs/06 规格循环：write→independent spec review→readiness→task DAG）：Run 采用一个已批准修订，而该修订既没有 CURRENT 的审查报告也没有进行中的审查时，Runtime 在这个 Run 中为它建一个 reviewer 职位的只读审查 Task（spec_id 为该修订，不引用 REQ），写 spec_review.opened{review_id, run_id, spec_id, spec_revision, spec_sha256, git_commit, task_id}（git_commit 是该修订的规格提交：从它的 Markdown 重新生成的索引的 sha256 等于 specs.sha256），在 SPECIFYING 中按 Runtime 规则类派发，持读者租约，在该提交的只读检出中运行。报告用新的受锁契约 contracts/spec-review-report.schema.json：对象是规格修订而不是候选，没有 candidate_id，带 spec_revision、spec_sha256 与 git_commit；它不进 independent_reports（候选外键必填）、不开验证轮次，也不为规格造候选。Runtime 收取报告时核对 schema、spec_sha256 与 git_commit 等于 spec_review.opened 中的值、会话在 sessions 中且不同于起草该修订的任何会话（以 spec.drafting 标记、结果成为该修订的 Task 的 Attempt）与 Commander 的会话（REQ-053 的会话独立），登记报告与原始会话为 artifacts，写 spec_review.reported{review_id, status, state:CURRENT 或 REJECTED, reasons, session_id, attempt_id, report, raw_session}（report 与 raw_session 为 path 与 sha256）。审查按修订而不按 Run：readiness 的 spec_review 只在该修订最新的 CURRENT 报告为 PASS 时满足；FAIL、UNKNOWN、缺失、REJECTED（会话不独立、提交或摘要不符）或绑定别的修订都使 readiness 为 NOT_READY(spec_review)。审查 Task 是普通的读 Task：取消、租约与崩溃恢复同其他读 Task；它终结而没有被收取的报告时，Runtime 为同一修订再开一次审查（新的 review_id）。用户批准（O1）不替代它。",
    "只阻断受影响的范围：一个 Task 的 readiness 只看它的 REQ 及其依赖闭包；不就绪的 Task 为 BLOCKED（READINESS_NOT_MET，details 列出原因与对应 id），其他就绪的 Task 照常派发，Run 不因此整体停下。",
    "标为非阻塞的开放问题不阻断 readiness，列入 Run 的开放问题清单（投影与 Timeline 可查）。",
    "Run 只在 readiness 通过后从 SPECIFYING 进 READY，并写 readiness.passed{spec_revision}；规格修订变化时对受影响范围重新判定（REQ-033），并对新修订重新做独立规格审查。"
  ],
  "failure_modes": [
    "范围内 required REQ 缺 AC 时，该 REQ 的 Task 为 BLOCKED（READINESS_NOT_MET，details.reasons 含 missing_ac 与 REQ id），没有该 Task 的 Attempt。",
    "计划依赖成环时，计划被拒（DAG_CYCLE），不导入任何 Task。",
    "Commander 在 readiness 未过时提出 transition_run 到 READY，决策 REJECTED（ILLEGAL_TRANSITION，details 写 readiness 原因），Run 不变。",
    "独立规格审查为 FAIL、UNKNOWN、缺失、非独立会话或绑定旧修订时，readiness 为 NOT_READY(spec_review)，Run 不进 READY。"
  ],
  "doc_refs": [
    "docs/06-orchestration.md",
    "docs/07-algorithms.md",
    "docs/adr/ADR-017.md",
    "docs/adr/ADR-028.md",
    "docs/adr/ADR-031.md"
  ],
  "acceptance": [
    {
      "id": "AC-031-01",
      "given": "五个夹具（规格加计划，独立规格审查均为同修订独立会话的 PASS）：齐全；required REQ 缺 AC；AC 缺 then；依赖成环；开放问题中一条阻塞问题未决",
      "when": "对每个夹具调用 readiness 判定",
      "then": "齐全的为 READY；其余依次为 NOT_READY 且 reasons 恰为 missing_ac、unobservable、DAG_CYCLE、undecided，并各自给出对应的 REQ、AC 或问题 id",
      "test_id": "TEST-031-01"
    },
    {
      "id": "AC-031-02",
      "given": "已批准修订中需求组 A（齐全）与 B（一条 required REQ 缺 AC），规格审查 PASS，计划含 A 的 Task TA 与 B 的 Task TB，Run 处于 RUNNING",
      "when": "运行三轮调度",
      "then": "TA 被派发；TB 为 BLOCKED、blocked_reason=READINESS_NOT_MET 且 details.reasons 含 missing_ac 与该 REQ id，TB 没有 Attempt；Run 仍为 RUNNING",
      "test_id": "TEST-031-02"
    },
    {
      "id": "AC-031-03",
      "given": "规格的开放问题中有一条标为“阻塞：否”的未决问题，其余齐全，规格审查 PASS",
      "when": "调用 readiness 判定并读 Run 的投影",
      "then": "判定为 READY；该问题出现在 Run 的开放问题清单中，带其 id 与“非阻塞”标记",
      "test_id": "TEST-031-03"
    },
    {
      "id": "AC-031-04",
      "given": "Run R 处于 SPECIFYING，已批准修订 1 不就绪（missing_ac）",
      "when": "Commander 提出 transition_run 到 READY；随后用户批准补齐 AC 的修订 2，Runtime 派发它的规格审查，假 reviewer 在独立会话中给出 PASS",
      "then": "第一次决策 REJECTED（ILLEGAL_TRANSITION，details 含 missing_ac），R 仍为 SPECIFYING；修订 2 批准后 Runtime 写恰一条 spec_review.opened（spec_revision=2，git_commit 为修订 2 的规格提交）并派发恰一个 reviewer 的规格审查 Attempt，其会话不同于 Commander 与起草修订 2 的会话；报告经 spec-review-report 契约收取为 spec_review.reported{status:PASS, state:CURRENT}，带报告与原始会话的 path 与 sha256，independent_reports 与 candidates 没有新行；PASS 之前 R 不进 READY，之后写 readiness.passed{spec_revision:2}，R 进入 READY",
      "test_id": "TEST-031-04"
    },
    {
      "id": "AC-031-05",
      "given": "已批准修订 2 齐全；六份规格审查记录：同修订独立会话 PASS；同修订 FAIL；同修订 UNKNOWN；没有记录；起草修订 2 的会话出具的 PASS；绑定修订 1 的 PASS",
      "when": "对每份记录各调用一次 readiness 判定",
      "then": "只有第一份得到 READY；其余依次为 NOT_READY 且 reasons 含 spec_review，details 依次为 fail、unknown、missing、not_independent、stale_revision",
      "test_id": "TEST-031-05"
    }
  ],
  "decisions": [
    "readiness 由 Runtime 自动判定，Commander 只能补规格或计划（CR-026，C5）。",
    "阻塞性的标记写在规格的开放问题条目里（每条带“阻塞：是/否”），缺省为阻塞（CR-026，C4）。",
    "独立规格审查由 reviewer 职位在独立会话中做，属 readiness 的输入；对象是规格修订：报告用新的受锁契约 spec-review-report.schema.json，审查的开启与收取是事件（spec_review.opened、spec_review.reported），不用 independent_reports 与验证轮次（二者的候选外键与 round_of_task 只适用于候选），也不为规格造候选（CR-026，CR26D-05、-101）。",
    "新码 READINESS_NOT_MET 为 BlockedReason（Task 的 blocked_reason）（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-031 的场景（旧：TEST-031-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-031-01",
    "requirement_id": "REQ-031",
    "acceptance_id": "AC-031-01",
    "check_id": "unit",
    "fixture": "hoh-core readiness 纯函数 + 五个规格与计划夹具（规格审查 PASS）",
    "steps": [
      "对五个夹具各判定一次"
    ],
    "pass_criteria": [
      "齐全者 READY",
      "其余四个的 reasons 恰为 missing_ac、unobservable、DAG_CYCLE、undecided 并含对应 id"
    ],
    "observables": [
      "判定返回值",
      "夹具清单",
      "判定结果"
    ]
  },
  {
    "id": "TEST-031-02",
    "requirement_id": "REQ-031",
    "acceptance_id": "AC-031-02",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent，修订含 A（齐全）与 B（缺 AC）",
    "steps": [
      "运行三轮调度"
    ],
    "pass_criteria": [
      "TA 被派发",
      "TB BLOCKED 且 READINESS_NOT_MET，details 含 missing_ac 与 REQ id",
      "TB 无 Attempt",
      "Run 仍 RUNNING"
    ],
    "observables": [
      "tasks 与 attempts 行",
      "调度轮派发记录",
      "events导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-031-03",
    "requirement_id": "REQ-031",
    "acceptance_id": "AC-031-03",
    "check_id": "unit",
    "fixture": "readiness 纯函数 + Run 投影的开放问题清单",
    "steps": [
      "判定一次",
      "读投影"
    ],
    "pass_criteria": [
      "READY",
      "清单含该问题 id 与非阻塞标记"
    ],
    "observables": [
      "判定返回值",
      "投影",
      "判定结果"
    ]
  },
  {
    "id": "TEST-031-04",
    "requirement_id": "REQ-031",
    "acceptance_id": "AC-031-04",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander + 假 reviewer，Run SPECIFYING",
    "steps": [
      "Commander 提出到 READY",
      "用户批准修订 2",
      "假 reviewer 在独立会话中给出 PASS"
    ],
    "pass_criteria": [
      "首个决策 REJECTED 且 details 含 missing_ac",
      "恰一条 spec_review.opened 与一个规格审查 Attempt，会话独立",
      "spec_review.reported 为 PASS、CURRENT 且带报告与原始会话引用",
      "independent_reports 与 candidates 无新行",
      "PASS 之前不进 READY",
      "readiness.passed{spec_revision:2} 后 Run 为 READY"
    ],
    "observables": [
      "decisions 行",
      "attempts 与 sessions 行",
      "events导出",
      "independent_reports 与 candidates 行数",
      "runs 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-031-05",
    "requirement_id": "REQ-031",
    "acceptance_id": "AC-031-05",
    "check_id": "unit",
    "fixture": "readiness 纯函数 + 六份规格审查记录夹具",
    "steps": [
      "逐份判定"
    ],
    "pass_criteria": [
      "只有同修订独立会话 PASS 为 READY",
      "其余 reasons 含 spec_review 且 details 依次为 fail、unknown、missing、not_independent、stale_revision"
    ],
    "observables": [
      "判定返回值",
      "夹具清单",
      "判定结果"
    ]
  }
]
```

#### REQ-030 规格收敛（3 条 AC）

`contracts/requirements.json` 中 REQ-030 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-030-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-030",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "规格收敛",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "记录用户/范围/非目标/决策/开放问题；必要不确定性不伪造用户答案",
  "behavior": [
    "规格记录五类内容（REQ-034 的固定标题）：用户、范围、非目标、决策、开放问题；每类非空或明写“无”。",
    "每条决策注明来源：用户答复（question_id）、用户的交互输入（input_id），或假设（record_assumption 的事件 id）；没有来源的决策不允许。",
    "交互运行中，需要用户选择时以 request_user_decision 提问并等待答复，答复按 question_id 记录，不被记成假设。",
    "无人值守（-p）时只能假设项目配置 [assumptions] 中列出的默认值（O2）：每个假设以 record_assumption 记录，basis 写配置键；basis 不是 [assumptions] 中列出的键的 record_assumption 被拒（决策 REJECTED），不写假设；产品方向、范围、非目标与未列出的必需决定一律不假设，Run 以 NEED_DECISION 阻塞并持久化问题。[assumptions] 中以 product_direction、scope、non_goals 为名的键使配置被拒（与未知键同一路径），即使列出也不会被假设；config/hoh.example.toml 的 [assumptions] 为空表，空表不产生任何假设。",
    "规格提议与送批（ProposeSpecChange、ApproveSpec）时 Runtime 校验来源：标为用户决定而在 DB 中找不到对应 AnswerQuestion 或交互输入的条目，使其被拒（SPEC_INVALID，details 列出无来源的条目）。",
    "假设台账是事件的投影（record_assumption 的应用事件），不另建表（C6）。"
  ],
  "failure_modes": [
    "无人值守遇到产品方向这类关键未知时，Run=BLOCKED(NEED_DECISION)，问题持久化，-p 以 3 退出并列出问题，不写任何假设。",
    "把假设写成用户决定的规格被拒（SPEC_INVALID），没有新的已批准修订。",
    "配置 [assumptions] 未列出的决定类，无人值守时不假设，照样阻塞；列出 product_direction、scope 或 non_goals 的配置被拒。"
  ],
  "doc_refs": [
    "docs/06-orchestration.md",
    "docs/15-prompts-skills-memory.md",
    "docs/01-prd.md",
    "docs/21-spec-governance.md",
    "config/hoh.example.toml",
    "docs/adr/ADR-031.md"
  ],
  "acceptance": [
    {
      "id": "AC-030-01",
      "given": "交互运行，目标“做一个记账工具”，FakeCommander 脚本提出一个范围问题，用户答复其选项",
      "when": "起草完成，规格经提议送批",
      "then": "规格五个标题下各有内容或“无”；决策中来自答复的条目引用其 question_id，DB 中有对应的 AnswerQuestion 事件；批准通过",
      "test_id": "TEST-030-01"
    },
    {
      "id": "AC-030-02",
      "given": "无人值守（-p）的 AUTO Run。项目 A 有已批准修订 1（没有语言与产品方向的决定），配置 [assumptions] 列出 language=\"Rust\"；FakeCommander 补规格时依次提出 record_assumption{basis: assumptions.language}、record_assumption{basis: assumptions.product_direction}，再以 request_user_decision 问产品方向。项目 B、C、D 的配置 [assumptions] 分别列出 product_direction、scope、non_goals",
      "when": "四个项目各以 hoh -p 运行至停止",
      "then": "A：语言以 record_assumption 记录且 basis 含 assumptions.language；产品方向的 record_assumption 被拒（REJECTED，details 指出 product_direction 不是 [assumptions] 中列出的键），没有该假设；Run=BLOCKED(NEED_DECISION)、产品方向的问题持久化、-p 以 3 退出并列出该问题；没有新的已批准修订。B、C、D：配置都被拒，各自指出键路径 assumptions.product_direction、assumptions.scope、assumptions.non_goals，没有任何假设与派发，-p 以配置错误退出",
      "test_id": "TEST-030-02"
    },
    {
      "id": "AC-030-03",
      "given": "送批的规格中一条决策标为用户决定，引用的 question_id 在 DB 中没有答复（实为假设）",
      "when": "以 ProposeSpecChange 提议",
      "then": "返回 422，code=SPEC_INVALID，details 列出该条目及其缺失的来源；没有 change_requests 行与新的已批准修订",
      "test_id": "TEST-030-03"
    }
  ],
  "decisions": [
    "无人值守可假设的范围取所有者 2026-11-17 的决定 O2：只有项目配置中列出的默认值；产品方向、范围与非目标一律阻塞，配置中也不能为它们给默认值（CR-026，CR26D-52）。",
    "批准只由用户（O1），无人值守的 Run 停在批准之前（CR-026）。",
    "假设台账由事件投影，不新增表（CR-026，C6）；五节模板属 REQ-029 的模板（C7）。",
    "新码 SPEC_INVALID（ErrorCode，422）报告提议或送批规格的结构或来源问题（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-030 的场景（旧：TEST-030-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-030-01",
    "requirement_id": "REQ-030",
    "acceptance_id": "AC-030-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander 脚本 + 用户答复（交互）",
    "steps": [
      "提交目标",
      "答复范围问题",
      "提议并送批"
    ],
    "pass_criteria": [
      "五节齐全",
      "答复来源的决策引用 question_id 且 DB 有 AnswerQuestion",
      "批准通过"
    ],
    "observables": [
      "规格文件",
      "events导出",
      "specs 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-030-02",
    "requirement_id": "REQ-030",
    "acceptance_id": "AC-030-02",
    "check_id": "integration",
    "fixture": "hoh -p + 产品 host + FakeCommander，四个项目：A 有已批准修订 1 与 [assumptions] language；B、C、D 各列出一个保留名",
    "steps": [
      "项目 A 运行至停止",
      "项目 B、C、D 各运行至停止"
    ],
    "pass_criteria": [
      "A：basis 含 assumptions.language",
      "A：product_direction 的 record_assumption 被拒且无该假设",
      "A：BLOCKED(NEED_DECISION)、问题持久化、退出码 3",
      "A：没有新的已批准修订",
      "B、C、D：配置被拒并各自指出保留名的键路径，无假设与派发"
    ],
    "observables": [
      "events导出",
      "runs 行",
      "退出码与 stderr",
      "判定结果"
    ]
  },
  {
    "id": "TEST-030-03",
    "requirement_id": "REQ-030",
    "acceptance_id": "AC-030-03",
    "check_id": "integration",
    "fixture": "产品 host，规格含无来源的用户决定",
    "steps": [
      "ProposeSpecChange"
    ],
    "pass_criteria": [
      "HTTP 422 且 code=SPEC_INVALID，details 列出该条目",
      "无 change_requests 行与新修订"
    ],
    "observables": [
      "HTTP 状态与错误体",
      "change_requests 与 specs 行",
      "判定结果"
    ]
  }
]
```

#### REQ-032 需求追踪（4 条 AC）

`contracts/requirements.json` 中 REQ-032 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-032-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-032",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "需求追踪",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "REQ→TASK→candidate→TEST→EVIDENCE→acceptance 可双向查找",
  "behavior": [
    "产品为用户项目维护追踪链 REQ→AC→TASK→candidate→TEST→EVIDENCE→acceptance 结果，任一节点可双向查到完整链（C8：产品功能；本仓库的 traceability 工具以仿本校验产品的导出）。",
    "断链报为 broken 并指出缺哪一环：TEST 没有证据、证据没有候选、AC 没有 TEST、Task 没有 REQ。",
    "孤立验收报为 orphan：PASS 绑定的不是当前候选或当前规格修订。",
    "API 与 DB 一致：/v1/runs/{id}/acceptance 为每条 AC 给出候选、TEST 与证据引用，/v1/runs/{id}/tasks 为每个 Task 给出 REQ；产品不另造第二份追踪事实。",
    "规格修订变化后（O6），旧修订下的全部验收节点标为 stale（不只受影响的链），与 REQ-033 的验收失效一致；历史可查，不静默沿用。",
    "追踪的输入全部取自候选的 spec 文件：需求取它的 requirements，TEST→check 取它的 scenarios（用户项目的 docs/spec/index.json，REQ-034 行为 2），用户项目以发布目标 project 为档；scripts/product/traceability.py 的 World 与产品的追踪导出用同一读取。只有 spec 文件没有 scenarios 键时（HoH 自身仓库的 contracts/requirements.json 布局）才读 contracts/acceptance-scenarios.json；没有 contracts/ 目录的项目照常批准、冻结与追踪。"
  ],
  "failure_modes": [
    "证据被删或缺候选时，追踪报告列为 broken 并指明缺失环节，完成门禁不计该 AC 为 accepted。",
    "AC 的 PASS 绑定旧候选或旧修订时，报为 orphan，不计入当前进度。",
    "spec 文件的 scenarios 缺某个 TEST 或它的 check 时，该 TEST 报为 broken（缺 check），不回退去读项目的 contracts/。"
  ],
  "doc_refs": [
    "docs/10-acceptance.md",
    "docs/05-data-api.md",
    "docs/adr/ADR-027.md",
    "docs/adr/ADR-029.md",
    "docs/cr/CR-017-same-content-refreeze-in-one-run.md"
  ],
  "acceptance": [
    {
      "id": "AC-032-01",
      "given": "夹具项目经产品运行得到的追踪导出：2 条 REQ、4 条 AC、3 个 Task、1 个候选；测试再删去一条 TEST 的证据，并把一条 AC 的 PASS 改绑旧候选",
      "when": "以 traceability 检查运行 tests/bundle 中名称含 test_032_01 的测试，它调用产品的导出并读取追踪报告",
      "then": "任一节点（REQ、AC、Task、候选、TEST、证据、结果）都能正反两个方向查到完整链；没有证据的 TEST 列在 broken 中并指出缺 evidence；绑定旧候选的 PASS 列在 orphan 中；其余链完整",
      "test_id": "TEST-032-01"
    },
    {
      "id": "AC-032-02",
      "given": "产品 host 的 Run 有 3 个 Task 与 4 条 AC 的验证结果",
      "when": "读取 /v1/runs/{id}/acceptance 与 /v1/runs/{id}/tasks 的全部分页",
      "then": "每条 AC 的候选、TEST 与证据引用，以及每个 Task 的 REQ，与直接查询 DB 的结果逐项相等",
      "test_id": "TEST-032-02"
    },
    {
      "id": "AC-032-03",
      "given": "修订 1 的追踪链完整，REQ-A 与 REQ-B 都已验收；批准只改写 REQ-A 的修订 2",
      "when": "再次导出追踪报告",
      "then": "REQ-A 与未改动的 REQ-B 两条链上的验收节点都标为 stale，历史结果仍可查；修订 2 下没有节点沿用修订 1 的 PASS",
      "test_id": "TEST-032-03"
    },
    {
      "id": "AC-032-04",
      "given": "一个没有 contracts/ 目录的夹具 git 项目：docs/spec/ 下两份 Markdown 共 2 条 REQ、3 条 AC（check 行依次为 unit、unit、integration）与生成的 index.json，tests/check-plan.json 列出 unit 与 integration；项目还没有已批准修订",
      "when": "用户以 ApproveSpec 批准，经产品冻结一个候选，随后对该候选运行追踪（scripts/product/traceability.py 的 World，档 project），并读取产品对同一候选的追踪导出",
      "then": "候选的 spec 为指向 docs/spec/index.json 的单个 FileRef；World.tests() 恰得到 3 个 TEST，check 依次为 unit、unit、integration，等于 index.json 的 scenarios，也等于 acceptance_criteria.check_id；没有成员证据时 3 个 TEST 都是 NOT_RUN 而不是错误；追踪与导出期间没有打开项目根下 contracts/ 中的任何路径；产品导出的 TEST→check 与 World 的逐项相同",
      "test_id": "TEST-032-04"
    }
  ],
  "decisions": [
    "REQ-032 是产品功能；TEST-032-01 的测试在 tests/bundle、名称含 test_032_01，以 traceability 检查校验产品当前导出的追踪报告（运行产品导出，再注入缺证据与旧候选），不以手写的导出代替（CR-017，CR-026，C8）。",
    "M3 的追踪入口是 API（Page 的 items 为任意对象，ADR-029）与 TUI 的状态摘要；Web 视图属 M6（CR-026，C9）。",
    "追踪输入随冻结形式：用户项目的 TEST→check 只来自候选 spec 文件 index.json 的 scenarios，contracts/acceptance-scenarios.json 只用于没有 scenarios 键的 spec 文件（CR-026，CR26D-102）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-032 的场景（旧：TEST-032-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-032-01",
    "requirement_id": "REQ-032",
    "acceptance_id": "AC-032-01",
    "check_id": "traceability",
    "fixture": "tests/bundle 的 test_032_01 + 产品当前导出（注入缺证据与旧候选）",
    "steps": [
      "运行产品导出",
      "注入两处缺陷",
      "逐节点正反查询",
      "检查 broken 与 orphan"
    ],
    "pass_criteria": [
      "每个节点正反可达",
      "无证据的 TEST 在 broken 中且指出缺 evidence",
      "绑定旧候选的 PASS 在 orphan 中"
    ],
    "observables": [
      "追踪报告",
      "test-results.jsonl",
      "判定结果"
    ]
  },
  {
    "id": "TEST-032-02",
    "requirement_id": "REQ-032",
    "acceptance_id": "AC-032-02",
    "check_id": "integration",
    "fixture": "产品 host + 有结果的 Run",
    "steps": [
      "读两个 API 的全部分页",
      "直接查 DB"
    ],
    "pass_criteria": [
      "两份清单逐项相等"
    ],
    "observables": [
      "API 响应",
      "DB 查询结果",
      "判定结果"
    ]
  },
  {
    "id": "TEST-032-03",
    "requirement_id": "REQ-032",
    "acceptance_id": "AC-032-03",
    "check_id": "integration",
    "fixture": "产品 host，批准只改 REQ-A 的修订 2",
    "steps": [
      "导出追踪报告"
    ],
    "pass_criteria": [
      "REQ-A 与 REQ-B 的验收节点都 stale",
      "历史可查",
      "无沿用修订 1 的 PASS"
    ],
    "observables": [
      "追踪报告",
      "acceptance_results",
      "判定结果"
    ]
  },
  {
    "id": "TEST-032-04",
    "requirement_id": "REQ-032",
    "acceptance_id": "AC-032-04",
    "check_id": "integration",
    "fixture": "没有 contracts/ 目录的临时 git 项目（docs/spec/ 与 tests/check-plan.json）+ 产品 host + 冻结 + 追踪 World",
    "steps": [
      "ApproveSpec",
      "冻结候选",
      "运行 World.tests() 与追踪报告",
      "读产品追踪导出",
      "记录打开的文件"
    ],
    "pass_criteria": [
      "candidate.spec 为指向 index.json 的单个 FileRef",
      "3 个 TEST 的 check 依次为 unit、unit、integration 且等于 scenarios 与 acceptance_criteria.check_id",
      "无证据时为 NOT_RUN",
      "未打开项目根下 contracts/ 的路径",
      "产品导出与 World 逐项相同"
    ],
    "observables": [
      "candidate.frozen 载荷",
      "World.tests() 结果",
      "追踪报告",
      "文件访问记录",
      "判定结果"
    ]
  }
]
```

#### REQ-028 项目发现（4 条 AC）

`contracts/requirements.json` 中 REQ-028 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-028-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-028",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "项目发现",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "建立基线，保留脏文件，不重写架构、不强制初始化远程",
  "behavior": [
    "Run 的 CREATED→DISCOVERING→SPECIFYING 由 Runtime 推进（CONTRACT 第 6 条“常规阶段自动流转”，ADR-031 给 M3 的移交）：CREATED 的 Run 已采用规格，或已以 draft 进入起草（REQ-008）时，Runtime 把它置为 DISCOVERING 并做项目发现，写 run.baseline{head, tree, dirty:[{path, sha256}], untracked:[{path, sha256}], build_commands, test_commands, docs}，之后才进入 SPECIFYING；run.baseline 先于该 Run 的任何写入（git init、Attempt、effect）。Commander 的 transition_run 不能让 Run 在 run.baseline 之前离开 DISCOVERING（ILLEGAL_TRANSITION）。基线随 Run 记录（C11）；非 git 目录的 head 与 tree 为 null。",
    "发现只读：不改写、不 stash、不 reset 工作树，不新建提交，不改远程配置；git status --porcelain、脏文件与未跟踪文件的摘要在发现前后逐字相同。",
    "构建与测试命令按常见约定检测（Cargo.toml、package.json 的 scripts、Makefile、pyproject.toml），检测不到记 unknown（C10）；发现不自动运行项目的构建与测试（O3）；用户确认后运行一次，结果按命令记入基线，之后的回归以基线为对照。",
    "写 Task 的路径范围（REQ-031 的任务范围）含有用户未提交修改的文件时，HoH 不碰这些文件：Task 为 BLOCKED（NEED_DECISION），问题列出这些文件（O3）；这些文件的摘要在 Run 全程不变。",
    "Greenfield（空目录或非 git 目录）：HoH 只在首个起草 Attempt 的准备阶段（REQ-008：持该 Attempt 的写者租约，记为它的 effect）本地 git init，不加远程、不推送；首个提交只含 docs/spec/、docs/ 与 .hoh/ 下的路径（起草 Task 的写集核对）；已是 git 仓库时不重复初始化。",
    "已有文档按常见位置自动发现（README、docs/、ADR 目录），列入基线的 docs。",
    "不重写架构：发现不创建任何写 Task；Brownfield 的计划中每个 Task 都必须引用用户目标范围内（当前已批准修订中）至少一条 REQ，不引用任何 REQ 的 Task（如重构既有模块）被拒（REQ-031、REQ-009）。"
  ],
  "failure_modes": [
    "发现过程中任何写操作都不发生：git status --porcelain 与文件摘要不变，git remote -v 不变，提交数不变。",
    "写 Task 涉及脏文件时，Task=BLOCKED(NEED_DECISION)，问题列出文件路径，文件摘要不变。",
    "不引用已批准 REQ 的计划 Task 被拒，没有被导入。",
    "基线已失败的测试不被计为 HoH 引入的回归。"
  ],
  "doc_refs": [
    "docs/01-prd.md",
    "docs/05-data-api.md",
    "docs/12-user-guide.md",
    "docs/adr/ADR-021.md"
  ],
  "acceptance": [
    {
      "id": "AC-028-01",
      "given": "夹具 git 仓库：有代码、测试、README 与 docs/，两个已修改文件、一个未跟踪文件，一个远程",
      "when": "以新 Run 执行项目发现",
      "then": "写 run.baseline，含 head、tree、两个脏文件与一个未跟踪文件的路径与 sha256、检测到的 build_commands 与 test_commands、README 与 docs/；发现前后 git status --porcelain 逐字相同、脏与未跟踪文件的摘要不变、提交数与 git remote -v 不变；发现期间没有创建写 Task",
      "test_id": "TEST-028-01"
    },
    {
      "id": "AC-028-02",
      "given": "基线检测到测试命令 cargo test，其中一个测试本来失败；用户确认运行测试",
      "when": "发现运行测试一次，随后一个候选使同一测试仍失败",
      "then": "基线按命令记录 pass/fail 且含该失败测试；该候选的回归判定不把它算作 HoH 引入的回归；用户确认前没有运行任何项目命令",
      "test_id": "TEST-028-02"
    },
    {
      "id": "AC-028-03",
      "given": "夹具仓库中文件 F 有用户未提交的修改；已批准修订含 REQ-A；Commander 先提出计划 P1：写 Task T（引用 REQ-A，路径范围含 F）与 Task U（不引用任何 REQ，重构既有模块）；再提出只含 T 的计划 P2",
      "when": "Runtime 依次处理 P1 与 P2，并运行调度轮",
      "then": "P1 整体被拒（details 指出 U 没有引用已批准 REQ），T 与 U 都没有导入，tasks 与 task_requirements 不变；P2 被接受、T 被导入；T 为 BLOCKED(NEED_DECISION)，问题列出 F；没有 T 的写 Attempt；F 的 sha256 在 Run 全程不变，没有 stash 或 reset",
      "test_id": "TEST-028-03"
    },
    {
      "id": "AC-028-04",
      "given": "一个空目录与一个已有提交的 git 仓库，两者都没有已批准规格；cto 的假 Agent 起草时只写 docs/spec/ 并提交",
      "when": "各启动一个 Run，用户以 draft 答复 runtime:no_spec 问题，起草 Task 完成",
      "then": "两个 Run 的 run.baseline 都先于该 Run 的任何 Attempt 与 effect 记录（空目录的 head 为 null）；空目录在起草 Attempt 的准备阶段被本地 git init（该 Attempt 的 effect），没有远程，首个提交只含 docs/spec/、docs/ 与 .hoh/ 下的路径；已有仓库没有被重新初始化，提交历史保留，新提交同样只含这三处路径",
      "test_id": "TEST-028-04"
    }
  ],
  "decisions": [
    "已有仓库的做法取所有者 2026-11-17 的决定 O3：碰到未提交文件先阻塞询问；发现不自动运行构建与测试（CR-026）。",
    "基线随 Run 记录（run.baseline），不同 Run 可以有不同的基线（CR-026，C11）。",
    "命令检测按常见约定，检测不到为 unknown，不猜测（CR-026，C10）。",
    "规格阶段之前的 CREATED→DISCOVERING→SPECIFYING 由 Runtime 推进，基线先于任何写入（CR-026，CR26D-151）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-028 的场景（旧：TEST-028-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-028-01",
    "requirement_id": "REQ-028",
    "acceptance_id": "AC-028-01",
    "check_id": "integration",
    "fixture": "夹具 git 仓库（脏文件、未跟踪文件、远程）+ 产品 host",
    "steps": [
      "记录 git 状态与文件摘要",
      "执行发现",
      "再次记录"
    ],
    "pass_criteria": [
      "run.baseline 字段齐全且列出脏与未跟踪文件",
      "前后 git 状态、摘要、提交数与 remote 逐字相同",
      "发现期间无写 Task"
    ],
    "observables": [
      "events导出",
      "git 状态快照",
      "文件摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-028-02",
    "requirement_id": "REQ-028",
    "acceptance_id": "AC-028-02",
    "check_id": "integration",
    "fixture": "夹具仓库（一个失败测试）+ 产品 host + 用户确认",
    "steps": [
      "确认前检查命令运行记录",
      "确认后运行测试",
      "候选回归判定"
    ],
    "pass_criteria": [
      "确认前无项目命令运行",
      "基线记录 pass/fail 含失败测试",
      "回归判定不含该测试"
    ],
    "observables": [
      "run.baseline",
      "命令运行记录",
      "回归判定",
      "判定结果"
    ]
  },
  {
    "id": "TEST-028-03",
    "requirement_id": "REQ-028",
    "acceptance_id": "AC-028-03",
    "check_id": "integration",
    "fixture": "夹具仓库（F 有未提交修改）+ 产品 host + FakeCommander 的两个计划（P1：T 与 U；P2：只有 T）",
    "steps": [
      "处理 P1",
      "处理 P2",
      "运行调度轮至 T"
    ],
    "pass_criteria": [
      "P1 整体被拒且 details 指出 U 未引用 REQ，零导入",
      "P2 接受、T 导入",
      "T BLOCKED(NEED_DECISION) 且问题列出 F",
      "无 T 的写 Attempt",
      "F 摘要全程不变"
    ],
    "observables": [
      "decisions 与 tasks 行",
      "attempts 行",
      "问题",
      "F 的摘要序列",
      "判定结果"
    ]
  },
  {
    "id": "TEST-028-04",
    "requirement_id": "REQ-028",
    "acceptance_id": "AC-028-04",
    "check_id": "integration",
    "fixture": "空目录 + 已有 git 仓库 + 产品 host + cto 假 Agent + 用户以 draft 答复",
    "steps": [
      "各启动 Run",
      "以 draft 答复",
      "起草完成后检查基线、effect 与提交"
    ],
    "pass_criteria": [
      "run.baseline 先于任何 Attempt 与 effect",
      "空目录在起草 Attempt 的准备阶段本地初始化（effect）且无 remote",
      "首个提交只含三处路径",
      "已有仓库未重新初始化且新提交只含三处路径"
    ],
    "observables": [
      "events导出",
      "effects 行",
      "git 配置与提交清单",
      "remote",
      "判定结果"
    ]
  }
]
```

#### REQ-045 四层Prompt版本（4 条 AC）

`contracts/requirements.json` 中 REQ-045 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-045-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-045",
  "category": "PROMPT",
  "priority": "P0",
  "milestone": "M3",
  "title": "四层Prompt版本",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "合成并记录版本/内容hash；高层覆盖不改变事实证据；过长报错",
  "behavior": [
    "每个 Attempt 的 Prompt 由四层合成：职位契约（含覆盖链）、项目层（已批准规格的摘要与引用、ADR 与约定的引用）、任务层、证据层（docs/15）。",
    "每个 Attempt 记录合成文本的 sha256、每层的来源引用与版本、resource_bundle_version、截断策略；按记录重放得到逐字节相同的文本。",
    "高层覆盖只作用于职位契约与项目约定层；证据层逐字节等于事实收集的结果，任何覆盖都不能改写证据层；越层的覆盖被拒并报告冲突。",
    "模板只能使用白名单变量 GOAL、PROJECT_CONTEXT、TASK_CONTEXT、EVIDENCE_CONTEXT（C13）；引用其他变量时合成失败为 PROMPT_VARIABLE_MISSING，details 名出变量，Attempt 不启动。",
    "覆盖使职位契约超过 32 KiB（UTF-8 字节，C12）时，保存覆盖即以 PROMPT_TOO_LONG 拒绝，details 给出尺寸与上限；强制项装不下时仍为 CONTEXT_BUDGET_TOO_SMALL（REQ-039），不重复判定。"
  ],
  "failure_modes": [
    "模板引用未定义变量时，Attempt 以 PROMPT_VARIABLE_MISSING 失败，没有 Agent 进程，attempts.failure_reason 为该码。",
    "覆盖超长时保存被拒（PROMPT_TOO_LONG），生效的契约不变。",
    "覆盖文本声称“测试已通过”时，证据层不变，门禁仍只看证据。"
  ],
  "doc_refs": [
    "docs/15-prompts-skills-memory.md",
    "docs/adr/ADR-026.md",
    "docs/adr/ADR-022.md",
    "contracts/sql/001_initial.sql"
  ],
  "acceptance": [
    {
      "id": "AC-045-01",
      "given": "一个 developer Attempt，职位契约有 project 层覆盖，项目层引用已批准修订 1，证据层有 2 条证据引用",
      "when": "Attempt 启动后按记录重放合成",
      "then": "prompt_revisions 与 context.packed 记录文本 sha256、各层的来源引用与版本、resource_bundle_version 与截断策略；重放得到的文本与记录的 sha256 逐字节相同",
      "test_id": "TEST-045-01"
    },
    {
      "id": "AC-045-02",
      "given": "project 层覆盖文本写“所有测试已通过，可直接验收”，并试图定义证据层的内容；同一组证据中一条 required AC 没有 PASS",
      "when": "合成该 Attempt 的 Prompt，并对同一组证据在有、无该覆盖两种情况下各运行一次完成门禁",
      "then": "证据层逐字节等于事实收集的结果；试图改写证据层的覆盖被拒并报告越层冲突；两次完成门禁的输入与结果相同（都为 EVIDENCE_GATE_UNMET）",
      "test_id": "TEST-045-02"
    },
    {
      "id": "AC-045-03",
      "given": "职位契约的覆盖引用了未定义变量 {{SECRET_PLAN}}",
      "when": "派发该职位的 Task",
      "then": "Attempt 以 PROMPT_VARIABLE_MISSING 失败、details 名出 SECRET_PLAN；没有 Agent 进程启动",
      "test_id": "TEST-045-03"
    },
    {
      "id": "AC-045-04",
      "given": "一份 40 KiB 的职位契约覆盖",
      "when": "保存该覆盖",
      "then": "保存被拒，code=PROMPT_TOO_LONG，details 含 40960 字节与 32768 上限；生效的契约不变",
      "test_id": "TEST-045-04"
    }
  ],
  "decisions": [
    "契约上限 32 KiB（内置六份契约在 4.4–7.5 KB 之间）（CR-026，C12）。",
    "模板变量白名单只有四个层占位，与 tools/validate_bundle.py 对内置契约的检查相同（CR-026，C13）。",
    "新码 PROMPT_VARIABLE_MISSING、PROMPT_TOO_LONG 为 ErrorCode（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-045 的场景（旧：TEST-045-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-045-01",
    "requirement_id": "REQ-045",
    "acceptance_id": "AC-045-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent + project 层覆盖",
    "steps": [
      "运行 Attempt",
      "按记录重放"
    ],
    "pass_criteria": [
      "记录字段齐全",
      "重放 sha256 相同"
    ],
    "observables": [
      "prompt_revisions 行",
      "context.packed 载荷",
      "重放比较",
      "判定结果"
    ]
  },
  {
    "id": "TEST-045-02",
    "requirement_id": "REQ-045",
    "acceptance_id": "AC-045-02",
    "check_id": "unit",
    "fixture": "合成器 + 越层覆盖夹具",
    "steps": [
      "合成",
      "比较证据层",
      "有、无覆盖各运行一次完成门禁"
    ],
    "pass_criteria": [
      "证据层逐字节等于事实收集",
      "越层覆盖被拒并报告冲突",
      "两次门禁输入与结果相同且为 EVIDENCE_GATE_UNMET"
    ],
    "observables": [
      "合成文本",
      "冲突诊断",
      "判定结果"
    ]
  },
  {
    "id": "TEST-045-03",
    "requirement_id": "REQ-045",
    "acceptance_id": "AC-045-03",
    "check_id": "integration",
    "fixture": "产品 host + 含未定义变量的覆盖",
    "steps": [
      "派发 Task"
    ],
    "pass_criteria": [
      "Attempt 失败为 PROMPT_VARIABLE_MISSING 且 details 名出变量",
      "无 Agent 进程"
    ],
    "observables": [
      "attempts 行",
      "进程记录",
      "判定结果"
    ]
  },
  {
    "id": "TEST-045-04",
    "requirement_id": "REQ-045",
    "acceptance_id": "AC-045-04",
    "check_id": "unit",
    "fixture": "覆盖保存校验",
    "steps": [
      "保存 40 KiB 覆盖"
    ],
    "pass_criteria": [
      "拒绝为 PROMPT_TOO_LONG 并给出尺寸与上限",
      "生效契约不变"
    ],
    "observables": [
      "保存结果与错误体",
      "判定结果"
    ]
  }
]
```

#### REQ-047 自定义与升级（3 条 AC）

`contracts/requirements.json` 中 REQ-047 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-047-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-047",
  "category": "PROMPT",
  "priority": "P0",
  "milestone": "M3",
  "title": "自定义与升级",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "内置/global/project/run按序；显示diff；运行锁定旧版本直到checkpoint",
  "behavior": [
    "资源（职位契约、Skill、模板）的覆盖按 builtin→global→project→run 的顺序生效，后者优先；global 层在 $XDG_CONFIG_HOME/hoh/（缺省 ~/.config/hoh/），project 层在 .hoh/，run 层随 Run 的快照（C15）。",
    "每个 Attempt 记录覆盖链：各层的路径与内容摘要（prompt_revisions 的 layer 与 parent_revision）。",
    "用户可查看 builtin 与生效版本的统一 diff，“恢复默认”删除该层覆盖；任何编辑都不改安装的资源文件。",
    "资源包升级只给新 Attempt：Run 已开始的职位在其下一个 checkpoint（ADR-022 决定 8，C14）之前仍用旧版本；每个 Attempt 记其 resource_bundle_version。",
    "基于旧内置版本的覆盖在新版本内置已变时标为漂移，给出三方 diff（旧内置、新内置、覆盖）。"
  ],
  "failure_modes": [
    "删除 run 层覆盖后，下一个 Attempt 落到 project 层，覆盖链随之变化。",
    "编辑与恢复默认之后，安装目录的资源文件摘要不变。",
    "升级后在 checkpoint 之前的 Attempt 仍记旧的 resource_bundle_version。"
  ],
  "doc_refs": [
    "docs/15-prompts-skills-memory.md",
    "docs/03-ux.md",
    "docs/12-user-guide.md",
    "docs/adr/ADR-022.md"
  ],
  "acceptance": [
    {
      "id": "AC-047-01",
      "given": "developer 契约在 global、project、run 三层都有覆盖",
      "when": "运行一个 Attempt；删掉 run 层覆盖后再运行一个",
      "then": "第一个 Attempt 用 run 层，覆盖链依次记录 builtin、global、project、run 的路径与摘要；第二个用 project 层，链中没有 run 层",
      "test_id": "TEST-047-01"
    },
    {
      "id": "AC-047-02",
      "given": "developer 契约有 project 层覆盖",
      "when": "在 TUI 的设置页打开该契约的 diff，随后执行恢复默认",
      "then": "diff 为 builtin 与生效版本的统一 diff；恢复默认后 project 层覆盖文件被删除，下一 Attempt 用 builtin；安装目录中资源文件的 sha256 在全程不变",
      "test_id": "TEST-047-02"
    },
    {
      "id": "AC-047-03",
      "given": "Run R 以资源包 v1 开始，developer 契约有基于 v1 的 project 层覆盖",
      "when": "R 运行中把 HoH 升级到 developer 内置契约已变的 v2",
      "then": "R 的 developer 在下一个 checkpoint 之前的 Attempt 记 resource_bundle_version=v1，之后记 v2；该覆盖被标为漂移并给出三方 diff",
      "test_id": "TEST-047-03"
    }
  ],
  "decisions": [
    "global 层位置为 $XDG_CONFIG_HOME/hoh/（CR-026，C15）。",
    "资源升级的生效点取 ADR-022 决定 8 的职位 checkpoint，与团队变更一致（CR-026，C14）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-047 的场景（旧：TEST-047-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-047-01",
    "requirement_id": "REQ-047",
    "acceptance_id": "AC-047-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent + 三层覆盖（临时 XDG_CONFIG_HOME）",
    "steps": [
      "运行 Attempt",
      "删 run 层",
      "再运行"
    ],
    "pass_criteria": [
      "第一个用 run 层且链有四层",
      "第二个用 project 层且链无 run 层"
    ],
    "observables": [
      "prompt_revisions 行",
      "合成文本摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-047-02",
    "requirement_id": "REQ-047",
    "acceptance_id": "AC-047-02",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI 设置页 + 产品 host + project 层覆盖 + 安装目录",
    "steps": [
      "打开 diff",
      "恢复默认",
      "运行下一 Attempt"
    ],
    "pass_criteria": [
      "统一 diff 正确",
      "覆盖文件删除且下一 Attempt 用 builtin",
      "安装文件摘要不变"
    ],
    "observables": [
      "diff 文本",
      "覆盖文件",
      "安装文件摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-047-03",
    "requirement_id": "REQ-047",
    "acceptance_id": "AC-047-03",
    "check_id": "deployment",
    "fixture": "安装 v1 + 运行中升级到 v2 的发布包",
    "steps": [
      "v1 下开始 Run",
      "升级到 v2",
      "跨 checkpoint 运行"
    ],
    "pass_criteria": [
      "checkpoint 前 v1、之后 v2",
      "覆盖标漂移并给出三方 diff"
    ],
    "observables": [
      "attempts.resource_bundle_version",
      "漂移报告",
      "判定结果"
    ]
  }
]
```

#### REQ-029 内置模板（3 条 AC）

`contracts/requirements.json` 中 REQ-029 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-029-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-029",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "内置模板",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "产品/需求/功能/UX/架构/开发/测试/验收/部署/使用规范齐全；按规模选用",
  "behavior": [
    "内置模板覆盖十类规范：产品（product-definition）、需求（prd）、功能（feature）、UX（ux）、架构（architecture）、开发（development-plan）、测试（test-plan）、验收（acceptance）、部署（deployment）、使用（user-guide），按 lite、standard、full 三档组织（resources/templates/profiles.json）；full 含全部十类，lite 与 standard 是按规模的子集。",
    "模板随产品分发（嵌入二进制，计入 resource_bundle_version，C17）；无网络、无外部工具也能实例化；hoh doctor 列出模板包版本。",
    "实例化填实全部必填占位（不残留占位语法），并写 docs/21 的元数据：id、title、revision、status、owners、related_requirements、source_refs。",
    "档取 project.document_profile 的有效值：项目配置写了就用它，没写就用配置的缺省值 full（REQ-018，config/hoh.example.toml 不变）；实例化记录所用的档与来源（project 或 default）（C16）。",
    "改档时只补缺的文档，不覆盖用户已编辑的文件。",
    "规格模板（docs/spec/ 的骨架）含 REQ-034 的固定标题（用户、范围、非目标、决策、开放问题、需求），供 REQ-030 的五类内容与 REQ-008 的起草流程使用（C7）。",
    "实例化的产品入口是首次批准之前的起草（REQ-008）：起草 Task 的首个 Attempt 开始之前（git init 之后），Runtime 持该 Attempt 的写者租约，把规格模板以 Run 的目标文本实例化到 docs/spec/，并按有效档把 profiles.json 所列的全部模板实例化到 docs/（每个模板一份、文件名以模板 id 开头；full 档含十类 product-definition、prd、feature、ux、architecture、development-plan、test-plan、acceptance、deployment、user-guide，另有 api-data、adr、contributing、changelog、change-request、milestone-report），整体记为该 Attempt 的一个 effect；已存在的文件不覆盖；这些路径都在 Plan 的写集内，由起草 Agent 填写并随规格提交。"
  ],
  "failure_modes": [
    "实例化结果残留占位语法或缺元数据字段时，判为失败并列出文件与位置。",
    "改档时已编辑的文件内容不变。",
    "离线环境实例化不访问网络。"
  ],
  "doc_refs": [
    "docs/21-spec-governance.md",
    "docs/15-prompts-skills-memory.md",
    "docs/00-product-definition.md",
    "config/hoh.example.toml",
    "resources/templates/profiles.json"
  ],
  "acceptance": [
    {
      "id": "AC-029-01",
      "given": "三个空的夹具项目，project.document_profile 分别为 lite、standard、full",
      "when": "各实例化一次",
      "then": "full 的生成文件覆盖十类：产品、需求、功能、UX、架构、开发、测试、验收、部署、使用（各至少一份，按 profiles.json 的映射）；lite 与 standard 各生成 profiles.json 所列的全部模板；生成文件中没有残留的占位语法；每份都有 id、title、revision、status、owners、related_requirements、source_refs",
      "test_id": "TEST-029-01"
    },
    {
      "id": "AC-029-02",
      "given": "两个项目：一个的项目配置没有 document_profile，一个先以 lite 实例化后用户编辑了其中一份文件，再把档改为 full",
      "when": "各实例化一次",
      "then": "未写档的项目用 full，并记录档为 full、来源为 default；改档的只新增 full 比 lite 多出的文档，已编辑文件的 sha256 不变",
      "test_id": "TEST-029-02"
    },
    {
      "id": "AC-029-03",
      "given": "安装的发布包，测试环境不提供网络，HOME 与 XDG 目录为测试私有、没有运行中的 host；一个新的空项目，cto 职位绑定到随测试安装的假 Agent（它不改 docs/ 下的模板，只提交 docs/spec/ 后完成）",
      "when": "执行 hoh doctor；以 hoh -p 启动 Run（host 由此启动），以 strace -f -p 附着该 host 的全部线程（pid 取自 host 的运行记录），再在 strace -f 下以 hoh -p draft --resume <run_id> 选择起草",
      "then": "doctor 输出列出模板包版本；起草 Attempt 开始时 docs/spec/ 有规格骨架，docs/ 下有 full 档 profiles.json 所列的全部模板文件，其中十类 product-definition、prd、feature、ux、architecture、development-plan、test-plan、acceptance、deployment、user-guide 各一份且没有残留占位语法；两个 strace 的全部记录（客户端、host 与 host 的子进程）中 connect 的目标只有本地地址（AF_UNIX 或回环）；第二个 hoh -p 停在批准问题并以 3 退出",
      "test_id": "TEST-029-03"
    }
  ],
  "decisions": [
    "未在项目配置中指定档时用配置缺省 full，不改变 REQ-018 的缺省值，并记录来源 default（CR-026，C16，CR26D-09）。",
    "模板嵌入二进制，与职位契约、schema 同一资源包（CR-026，C17）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-029 的场景（旧：TEST-029-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-029-01",
    "requirement_id": "REQ-029",
    "acceptance_id": "AC-029-01",
    "check_id": "unit",
    "fixture": "三个临时项目 + 三档实例化",
    "steps": [
      "实例化",
      "按十类核对 full",
      "扫描占位",
      "校验元数据"
    ],
    "pass_criteria": [
      "full 覆盖十类",
      "lite 与 standard 齐所列模板",
      "无残留占位",
      "元数据字段齐全"
    ],
    "observables": [
      "生成文件清单",
      "占位扫描结果",
      "判定结果"
    ]
  },
  {
    "id": "TEST-029-02",
    "requirement_id": "REQ-029",
    "acceptance_id": "AC-029-02",
    "check_id": "integration",
    "fixture": "两个临时项目（未写档；lite 后改 full）",
    "steps": [
      "实例化",
      "比较已编辑文件摘要"
    ],
    "pass_criteria": [
      "未写档用 full 且记录来源 default",
      "只补缺的文档",
      "已编辑文件不变"
    ],
    "observables": [
      "生成清单",
      "选择记录",
      "文件摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-029-03",
    "requirement_id": "REQ-029",
    "acceptance_id": "AC-029-03",
    "check_id": "deployment",
    "fixture": "发布包安装 + 私有 HOME/XDG + 随测试安装的假 cto Agent + strace -f（附着 host 与跟踪客户端）+ 新空项目",
    "steps": [
      "hoh doctor",
      "hoh -p 启动 Run 与 host",
      "strace -f -p 附着 host",
      "在 strace -f 下 hoh -p draft --resume"
    ],
    "pass_criteria": [
      "doctor 列出模板包版本",
      "docs/spec/ 骨架与 full 档全部模板（十类各一份）且无占位",
      "客户端、host 与其子进程的 connect 只到本地地址",
      "第二个 -p 以 3 退出"
    ],
    "observables": [
      "doctor 输出",
      "生成清单",
      "两份 strace 的 connect 记录",
      "退出码",
      "判定结果"
    ]
  }
]
```

#### REQ-046 内置Skill（3 条 AC）

`contracts/requirements.json` 中 REQ-046 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-046-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-046",
  "category": "PROMPT",
  "priority": "P0",
  "milestone": "M3",
  "title": "内置Skill",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "内置可用无需用户手装；只加载相关技能；原生Skill不可用时内容注入",
  "behavior": [
    "13 个内置 Skill（resources/skills/registry.json）随产品分发，不要求用户安装，也不写进原生 Agent 的用户目录（O5）。",
    "每个 Attempt 收到 Skill 短索引，只含 roles 含该职位、phases 含当前阶段的 Skill（ADR-026 的短索引）。",
    "Skill 正文按阶段与任务注入（C18）：任务的计划条目或当前阶段要求的 Skill 正文作为知识类内容注入，计入 REQ-039 的知识份额，并记入 prompt 修订；已在原生会话目录中的不重复注入。",
    "注册校验：每个 Skill 有最小字段（docs/15）；依赖闭包可解析且无环；entry 存在；未知 phase 被拒。项目层自定义 Skill 照此校验，不能注册调度器或绕过门禁。"
  ],
  "failure_modes": [
    "没有网络与用户安装的 Skill 时，内置 Skill 照常可用。",
    "registry 依赖成环、缺 entry 或 phase 未知时，注册被拒并给出原因，生效的 Skill 集不变。",
    "Skill 不写入 ~/.claude、pi 扩展目录等原生用户目录；这些目录在 Run 前后的文件摘要不变。"
  ],
  "doc_refs": [
    "docs/15-prompts-skills-memory.md",
    "docs/adr/ADR-026.md",
    "docs/23-pre-m1-audit.md",
    "resources/skills/registry.json"
  ],
  "acceptance": [
    {
      "id": "AC-046-01",
      "given": "无网络，没有任何用户安装的 Skill；实现阶段的 developer Task 要求 tdd",
      "when": "运行该 Attempt，读假 Agent 记下的输入",
      "then": "短索引只含 roles 含 developer 且 phases 含实现阶段的 Skill；输入含 tdd 的正文；prompt 修订记录 tdd 的来源与版本",
      "test_id": "TEST-046-01"
    },
    {
      "id": "AC-046-02",
      "given": "绑定的 Agent 没有原生 Skill 机制；另一个 Agent 的会话目录中已有 tdd",
      "when": "各运行一个要求 tdd 的 Attempt",
      "then": "无原生机制的输入以知识类内容注入 tdd 正文并计入知识份额；会话目录已有 tdd 的输入不重复注入；运行前后 ~/.claude 与 pi 扩展目录的文件摘要不变",
      "test_id": "TEST-046-02"
    },
    {
      "id": "AC-046-03",
      "given": "四份 registry 夹具：合法；依赖成环；entry 缺失；phase 未知；另一份项目层 Skill 声称注册调度器",
      "when": "逐一注册",
      "then": "合法的通过；其余各被拒并给出原因（环成员、缺失路径、未知 phase、不允许的能力），生效的 Skill 集不变",
      "test_id": "TEST-046-03"
    }
  ],
  "decisions": [
    "Skill 的落点取所有者 2026-11-17 的决定 O5：不写原生 Agent 的用户目录，只注入或写进会话自己的目录（CR-026）。",
    "正文注入按阶段与任务，不由 Agent 自行请求（CR-026，C18）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-046 的场景（旧：TEST-046-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-046-01",
    "requirement_id": "REQ-046",
    "acceptance_id": "AC-046-01",
    "check_id": "integration",
    "fixture": "产品 host + 假 Agent（记录输入）+ 无网络、无用户 Skill",
    "steps": [
      "运行要求 tdd 的 developer Attempt",
      "读输入记录"
    ],
    "pass_criteria": [
      "短索引按职位与阶段过滤",
      "输入含 tdd 正文",
      "prompt 修订记录来源"
    ],
    "observables": [
      "假 Agent 输入",
      "prompt_revisions",
      "判定结果"
    ]
  },
  {
    "id": "TEST-046-02",
    "requirement_id": "REQ-046",
    "acceptance_id": "AC-046-02",
    "check_id": "integration",
    "fixture": "两个假 Agent（无原生机制；会话目录已有 tdd）+ 临时 HOME",
    "steps": [
      "各运行一个 Attempt",
      "比较原生目录摘要"
    ],
    "pass_criteria": [
      "注入计入知识份额",
      "不重复注入",
      "原生目录摘要不变"
    ],
    "observables": [
      "假 Agent 输入",
      "context.packed",
      "目录摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-046-03",
    "requirement_id": "REQ-046",
    "acceptance_id": "AC-046-03",
    "check_id": "unit",
    "fixture": "registry 校验 + 五份夹具",
    "steps": [
      "逐一注册"
    ],
    "pass_criteria": [
      "合法通过",
      "其余各被拒并给出原因",
      "生效集不变"
    ],
    "observables": [
      "校验结果",
      "判定结果"
    ]
  }
]
```

#### REQ-007 Slash 与统一操作（3 条 AC）

`contracts/requirements.json` 中 REQ-007 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-007-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-007",
  "category": "UX",
  "priority": "P0",
  "milestone": "M3",
  "title": "Slash 与统一操作",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "两种入口得到同一 SetMode 行为；Shell 不需要对称子命令",
  "behavior": [
    "TUI 的 /plan、/build、/auto 与 Shift+Tab 都提交同一 SetMode 命令（CommandService 的规范化意图）；HTTP 的 SetMode 得到形状相同的事件；Shell 没有对称子命令（CONTRACT 第 3 条）。",
    "Shift+Tab 按配置 [ui] mode_cycle 的顺序循环（docs/03 的“配置可改”）：缺省为 [\"PLAN\", \"BUILD\", \"AUTO\"]，合法值是 2 或 3 个互不相同的模式名（PLAN、BUILD、AUTO）组成的数组，非法值使配置被拒（REQ-018 的同一路径）（C21）；附着 Run 时作用于该 Run，未附着时只改下一个由 TUI 启动的 Run 的模式（C20）。",
    "模式变更在安全点生效：有运行中的写 Attempt 时先写 run.mode_change_pending{from, to}、不再派发新的写 Attempt，该 Attempt 到终态后写 run.mode_changed{from, to}；没有时立即写 run.mode_changed。",
    "/ 打开模糊选择器，列出高频命令（/plan /build /auto /team /model /status /resume /help）；未知命令显示“未知命令”且不提交任何命令。",
    "/plan 等 HoH 命令永不转给底座 Agent；转给底座的斜杠须显式写目标（/agent <role> <text>，C19），没有目标时不转。"
  ],
  "failure_modes": [
    "未知斜杠命令不产生命令、事件或 Agent 输入。",
    "运行中切换时，待生效期间没有新的写派发。",
    "SetMode 的 expected_revision 过期时返回 409 STALE_REVISION，模式不变。"
  ],
  "doc_refs": [
    "docs/03-ux.md",
    "docs/12-user-guide.md",
    "CONTRACT.md",
    "docs/adr/ADR-031.md"
  ],
  "acceptance": [
    {
      "id": "AC-007-01",
      "given": "TUI（PTY）附着一个 AUTO 的 Run，另一个客户端经 HTTP 可提交命令；另一个项目的配置为 [ui] mode_cycle = [\"PLAN\", \"AUTO\"]",
      "when": "第一个项目的 TUI 中依次输入 /plan、按两次 Shift+Tab，随后经 HTTP 提交 SetMode{mode:PLAN}；第二个项目的 TUI 附着一个 PLAN 的 Run 后按两次 Shift+Tab",
      "then": "第一个项目四次各产生恰好一条 SetMode 命令与一条 run.mode_changed{from, to}，依次为 AUTO→PLAN、PLAN→BUILD、BUILD→AUTO、AUTO→PLAN；/plan 与 HTTP 的两条命令除 id 外 payload 相同；顶栏显示当前模式；第二个项目依次为 PLAN→AUTO、AUTO→PLAN，没有经过 BUILD",
      "test_id": "TEST-007-01"
    },
    {
      "id": "AC-007-02",
      "given": "BUILD 的 Run 中一个写 Attempt 正在运行，另有 READY 的写 Task",
      "when": "TUI 中输入 /plan，随后该 Attempt 结束",
      "then": "/plan 返回 202 并写 run.mode_change_pending{from:BUILD, to:PLAN}；Attempt 结束前没有新的写派发；结束后写 run.mode_changed 且 runs.mode=PLAN",
      "test_id": "TEST-007-02"
    },
    {
      "id": "AC-007-03",
      "given": "TUI 附着 Run，绑定的 Agent 为假 Agent（记录输入）",
      "when": "输入 /，再输入 /foo，再输入 /plan，最后输入 /agent developer /compact",
      "then": "/ 列出八个高频命令；/foo 显示“未知命令”且没有命令提交；/plan 不出现在任何 Agent 输入中；/agent 的文本以 developer 为目标投递，未写目标的转发不发生",
      "test_id": "TEST-007-03"
    }
  ],
  "decisions": [
    "Shift+Tab 的顺序由 [ui] mode_cycle 配置，缺省 PLAN→BUILD→AUTO（CR-026，C21，CR26D-56）。",
    "未附着时 /plan 不改项目默认（project.default_mode），只改下一个 Run（CR-026，C20）。",
    "转给底座的语法为 /agent <role> <text>（CR-026，C19）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-007 的场景（旧：TEST-007-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-007-01",
    "requirement_id": "REQ-007",
    "acceptance_id": "AC-007-01",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI + 产品 host + HTTP 客户端",
    "steps": [
      "/plan",
      "两次 Shift+Tab",
      "HTTP SetMode",
      "第二个项目（mode_cycle 为 PLAN、AUTO）两次 Shift+Tab"
    ],
    "pass_criteria": [
      "四条 SetMode 与四条 run.mode_changed 依次为 AUTO→PLAN、PLAN→BUILD、BUILD→AUTO、AUTO→PLAN",
      "/plan 与 HTTP 的 payload 相同",
      "顶栏模式正确",
      "第二个项目依次为 PLAN→AUTO、AUTO→PLAN"
    ],
    "observables": [
      "命令记录",
      "events导出",
      "屏幕快照",
      "判定结果"
    ]
  },
  {
    "id": "TEST-007-02",
    "requirement_id": "REQ-007",
    "acceptance_id": "AC-007-02",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent 写 Attempt 运行中",
    "steps": [
      "提交 /plan 对应的 SetMode",
      "结束 Attempt"
    ],
    "pass_criteria": [
      "202 且 run.mode_change_pending",
      "期间无写派发",
      "结束后 run.mode_changed 且 runs.mode=PLAN"
    ],
    "observables": [
      "events导出",
      "调度轮派发记录",
      "runs 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-007-03",
    "requirement_id": "REQ-007",
    "acceptance_id": "AC-007-03",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI + 假 Agent 输入记录",
    "steps": [
      "/",
      "/foo",
      "/plan",
      "/agent developer /compact"
    ],
    "pass_criteria": [
      "选择器列八个命令",
      "/foo 无命令",
      "/plan 不进 Agent 输入",
      "/agent 以 developer 为目标"
    ],
    "observables": [
      "屏幕快照",
      "命令记录",
      "假 Agent 输入",
      "判定结果"
    ]
  }
]
```

#### REQ-008 Plan 互动与文档（5 条 AC）

`contracts/requirements.json` 中 REQ-008 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-008-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-008",
  "category": "UX",
  "priority": "P0",
  "milestone": "M3",
  "title": "Plan 互动与文档",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "产生版本化规格，允许提问，未批准实施不修改产品源码",
  "behavior": [
    "规格只经用户批准成为修订（O1）：ApproveSpec 只接受来自用户客户端的请求。host 以连接对端的进程身份判定（Unix socket 的对端凭据；TCP 由本机连接表找到对端进程）：对端进程或它的任一祖先带任何 Attempt 的标记（HOH_ATTEMPT_ID），或对端处于任何 Attempt 的 cgroup 中，即以 403 SPEC_APPROVAL_NOT_USER 拒绝（details.reason 依次为 marker、ancestor_marker、attempt_cgroup），什么也不写；Agent 工具调用里开的 shell 是 Attempt 的后代，同样被拒。对端无法确认（找不到对端进程、/proc 不可读、对端属于另一个 uid）时同样拒绝（details.reason=origin_unknown）：同用户的服务或端口转发来的请求不被当作用户批准。Commander 没有批准规格的决策（DecisionPayload 不含此类），它的任何输出都不产生修订。剩余风险（脱掉标记、离开 cgroup 并脱离 Attempt 祖先链的同用户进程）写入 docs/12。ApproveSpec 的 decision：APPROVE_ONLY 批准、模式不变；APPROVE_AND_BUILD 批准并把该 Run 的模式置为 BUILD（同 SetMode，在安全点生效，REQ-007）；REJECT 不产生修订。",
    "没有已批准规格时（所有者 2026-11-17 对 C22 的决定 (a)，见 CR-026 第 2 节）：StartRun 照旧以 runtime:no_spec 阻塞（BLOCKED(NEED_DECISION)，resume_state=CREATED），runs.spec_id 为空，没有 Task 与 Attempt，-p 以 3 退出（所有者 2026-11-16 的 Q1(a)）。no_spec 问题在 adopt 之外另有选项 draft：项目此时已有已批准修订，则 draft 与 adopt 相同；否则答复释放 no_spec 声明，Run 回到 CREATED，由 Runtime 经只读的 DISCOVERING（REQ-028：run.baseline 先于任何写入）进入 SPECIFYING，spec_id 仍为空，并在进入 SPECIFYING 的同一事务建首个起草 Task。",
    "起草 Task：由 Runtime 建，kind=spec_draft，职位 cto，writer，不引用 REQ（没有 task_requirements，不经 create_tasks 与 plan::check），以 spec.drafting{run_id, task_id} 标记；项目还没有任何已批准修订时它的 spec_id 为空（迁移 002：只有起草 Task 可以没有规格，且只在项目还没有已批准修订时），之后建的起草 Task 的 spec_id 为当前已批准修订。它在 SPECIFYING 中按 Runtime 规则类派发（不经调度轮；与监督 Task 同形：LEASED 的 Task、PREPARED 的 Attempt 与运行器通知），持项目的写者租约，所以同一项目的两个起草 Run 依次进行；取消阶梯、终结证明、工作区记录、效果台账与 kill -9 之后的 reconcile 同其他写 Task（REQ-041、REQ-043、REQ-019 行为 6）。首个起草 Attempt 的准备阶段在非 git 目录中本地 git init（REQ-028），记为该 Attempt 的 effect。首次批准之前 Run 没有规格，Commander 不被唤醒（监督照旧以 NO_SPEC 延后），起草由 Runtime 规则、看门狗（ADR-022 决定 19）与墙钟期限约束；起草 Agent 需要用户回答时以 NEED_DECISION 的问题停下（Run 为 BLOCKED、resume_state=SPECIFYING）。",
    "起草 Task 的完成不冻结候选、不要求 AC：Runtime 先核对写集（行为 5），再要求起草结果已提交（未提交时与写 Task 的冻结拒绝一样请它更正），并从该提交的 Markdown 重新生成索引、与提交中的 index.json 逐字节相同（REQ-034）。通过后 Task 为 IMPLEMENTED，Runtime 以 Run 级问题 spec_approval{spec_sha256, git_commit} 请用户批准（Run 为 BLOCKED(NEED_DECISION)、resume_state=SPECIFYING；选项 approve，其他答复为 REJECT，答复文本为 note）。修订只经用户的 ApproveSpec 产生：以 AnswerQuestion 答复 approve 时，host 先做同一对端判定，再把它转为该用户的 ApproveSpec 命令（commands 中记为 ApproveSpec，spec_sha256 取问题中的值，幂等键由答复命令派生），由它写修订。项目还没有已批准修订时，批准在一个事务里写修订 1（spec.changed{cause:\"approved\"}）、使该 Run 采用它（runs.spec_id）、起草 Task 为 ACCEPTED、Run 回到 SPECIFYING，此后 Commander 照常被唤醒；项目已有已批准修订时（例如另一个起草 Run 先得到批准），该 Run 先采用当前修订，起草结果以 CR 提出（REQ-033，base 为当前修订）。REJECT 使起草 Task 回到 READY，下一个 Attempt 以 note 为输入继续起草。无人值守的 -p 停在这个问题并以 3 退出，永不自批。",
    "Plan 的写集是 docs/spec/、docs/ 与 .hoh/：起草 Task 完成时 Runtime 以 Git 核对它开始以来的写集（Attempt 开始时的提交到完成时的提交之间的改动，加上已跟踪的修改与删除、未跟踪的新文件）；任一路径在写集之外时，写 plan.write_violation{task_id, paths}，不送批也不提出 CR，Task 为 BLOCKED（NEED_DECISION）并以问题列出这些路径；HoH 不替用户还原它们。PLAN 中 Commander 计划写产品源文件的 Task（writer 且不是起草 Task）被拒（ILLEGAL_TRANSITION，details 写“PLAN 不实施”）。",
    "有已批准修订后，PLAN 与 AUTO 中的起草同样由起草 Task 承担（spec_id 为当前已批准修订），由 Commander 照常监督；完成时以工作树中的规格提出 CR（change_requests PROPOSED，REQ-033），并以问题请用户批准。",
    "Plan 中允许提问：Commander 的 request_user_decision 成为 question.asked，TUI 显示选项；待答期间 Run 为 BLOCKED（NEED_DECISION）、resume_state=SPECIFYING，答复后回到 SPECIFYING；答复按 question_id 记录，不被记成假设。",
    "未批准不实施：ProposeSpecChange 未批准时 Run 的 spec_id 不变；切到 BUILD 也无写派发（readiness 未过，REQ-031、REQ-009），既有阻塞不被清空。"
  ],
  "failure_modes": [
    "Attempt 的进程、它的后代（含 Agent 工具调用里的 shell）或 Attempt cgroup 中的进程提交 ApproveSpec（或以 AnswerQuestion 答复批准问题）时返回 403 SPEC_APPROVAL_NOT_USER，对端无法确认时同样拒绝；没有新修订、没有采用、没有产品写派发。",
    "起草 Task 写了写集之外的路径（已跟踪的修改、删除或未跟踪的新文件）时，写 plan.write_violation，不送批也不提出 CR，Task 不前进。",
    "没有已批准规格时 StartRun 阻塞在 runtime:no_spec，runs.spec_id 为空，没有 Task 与 Attempt；draft 之前不派发任何 Agent turn；用户批准之前 specs 没有行。",
    "host 在起草 Attempt 运行中崩溃时，重启后照 REQ-041 reconcile：不重复建起草 Task，已记录的 effect（git init、模板实例化）不重复执行，specs 仍没有行。",
    "答复前 Run 为 BLOCKED(NEED_DECISION)，resume_state=SPECIFYING，不进入 READY。"
  ],
  "doc_refs": [
    "CONTRACT.md",
    "docs/03-ux.md",
    "docs/06-orchestration.md",
    "docs/15-prompts-skills-memory.md",
    "docs/12-user-guide.md",
    "docs/adr/ADR-031.md",
    "docs/adr/ADR-025.md",
    "contracts/sql/002_spec_drafting.sql"
  ],
  "acceptance": [
    {
      "id": "AC-008-01",
      "given": "PLAN 的 Run 采用了已批准修订 1；cto 的假 Agent 起草修订 2 的内容，用户经 TUI 交互答复 Commander 的问题",
      "when": "从起草 Task 派发运行到 CR 提出、用户在 TUI 中批准",
      "then": "Git 记录的写集（含未跟踪与删除）只含 docs/spec/、docs/ 与 .hoh/ 下的路径；没有写产品源文件的派发；产品源文件的摘要不变；批准后有修订 2",
      "test_id": "TEST-008-01"
    },
    {
      "id": "AC-008-02",
      "given": "一个空目录，没有已批准规格；cto 的假 Agent 起草时先写 docs/spec/ 并提交，再等待 30 s 才报告完成",
      "when": "StartRun（PLAN）两次，得到 R1、R2；用户以 draft 答复 R1 的 no_spec 问题；起草 Attempt 运行中 kill -9 host 并重启；起草完成后用户以 ApproveSpec{decision:APPROVE_ONLY} 批准；随后以 draft 答复 R2 的 no_spec 问题",
      "then": "阶段一：R1、R2 都为 BLOCKED(NEED_DECISION)、问题 runtime:no_spec，spec_id 为空，没有 Task 与 Attempt。阶段二：draft 之后 R1 的状态事件依次为 CREATED、DISCOVERING、SPECIFYING，DISCOVERING 期间 R1 没有 Attempt 与 effect，SPECIFYING 中 spec_id 仍为空；恰有一个起草 Task（kind=spec_draft、spec_id 为空、职位 cto、writer、没有 task_requirements）与一条 spec.drafting；git init 恰一次，为该 Attempt 的 effect，没有远程；kill -9 与重启之后没有第二个起草 Task、git init 没有再执行，Attempt 按 REQ-041 reconcile；完成时没有候选，起草 Task 为 IMPLEMENTED，R1 为 BLOCKED(NEED_DECISION)、resume_state=SPECIFYING、问题 spec_approval 带 spec_sha256；此时 specs 没有行，R2 仍阻塞在 no_spec。阶段三：批准写 revision=1 的已批准修订（spec.changed{cause:\"approved\"}），R1 的 spec_id 为它，起草 Task 为 ACCEPTED，R1 回到 SPECIFYING 并有它的监督触发器；R2 的 draft 答复等同 adopt：R2 采用修订 1，没有建 spec_id 为空的 Task",
      "test_id": "TEST-008-02"
    },
    {
      "id": "AC-008-03",
      "given": "PLAN 的 Run 在 SPECIFYING，Commander 发出 request_user_decision（两个选项）",
      "when": "TUI 显示后等待 10 s，随后用户选择第二个选项",
      "then": "TUI 显示问题与两个选项；等待期间 Run 为 BLOCKED(NEED_DECISION) 且 resume_state=SPECIFYING；答复后 Run 回到 SPECIFYING，AnswerQuestion 事件带 question_id 与 choice，假设台账中没有该条",
      "test_id": "TEST-008-03"
    },
    {
      "id": "AC-008-04",
      "given": "PLAN 的 Run 采用了修订 1；Commander 的计划含一个 developer 的写 Task；cto 的起草假 Agent 修改一个已跟踪的产品源文件、新建一个未跟踪的产品源文件、删除一个产品源文件",
      "when": "Runtime 处理该计划，起草 Task 完成",
      "then": "计划被拒（ILLEGAL_TRANSITION，details 含“PLAN 不实施”），没有导入 Task；起草 Task 的完成写 plan.write_violation，paths 恰为这三个路径，不提出 CR，Task 为 BLOCKED(NEED_DECISION) 且问题列出这三个路径；没有新的已批准修订",
      "test_id": "TEST-008-04"
    },
    {
      "id": "AC-008-05",
      "given": "PLAN 的 Run R 的首次起草已完成、等待批准（项目还没有已批准修订）；host 在受委托的 cgroup 中运行；起草 Attempt 的假 Agent 进程 P 带 Attempt 标记并读到 host.json 的 token；FakeCommander 在输出中要求批准",
      "when": "P 经 TCP 与 Unix socket 各提交一次 ApproveSpec，并以 AnswerQuestion 答复批准问题 approve；P 启动的 shell Q 清除自身环境中的 HOH_ATTEMPT_ID 后提交 ApproveSpec（祖先 P 带标记）；一个自身与祖先都不带标记的进程 Z 被放入该 Attempt 的 cgroup 后提交 ApproveSpec；不属于任何 Attempt、不在任何 Attempt cgroup 中的用户客户端提交 ApproveSpec；之后在 CR 待批时 P 再提交一次，用户客户端随后批准该 CR",
      "then": "P 的四次请求、Q 与 Z 的请求都返回 403 SPEC_APPROVAL_NOT_USER，details.reason 对 P 为 marker、对 Q 为 ancestor_marker、对 Z 为 attempt_cgroup；这些请求之后没有新的已批准修订、没有采用、没有产品写派发；FakeCommander 的输出没有产生修订；用户客户端的两次批准分别产生修订 1 与修订 2",
      "test_id": "TEST-008-05"
    }
  ],
  "decisions": [
    "批准权取所有者 2026-11-17 的决定 O1：只有用户批准规格；对端进程判定（标记、祖先标记、cgroup，无法确认即拒）是 host 能做到的执行，剩余风险写入用户指南（CR-026，CR26D-06、-105、-152）。",
    "首次批准之前的起草取所有者 2026-11-17 对 C22 的决定 (a)：Plan 对话、由 Agent 起草，起草是 spec_id 为空的起草 Task，受锁 SQL 增加迁移 002（只有起草 Task 可以没有规格，且只在项目还没有已批准修订时）；草稿只经用户的 ApproveSpec 成为修订 1；Q1(a) 与 M2T13D-59 不变（CR-026，CR26D-01、-50、-100、-150、-151）。",
    "Plan 的写集为 docs/spec/、docs/ 与 .hoh/；原型工件也只能放在 docs/ 下（CR-026，C23）。",
    "ApproveSpec 的 APPROVE_AND_BUILD 等于批准加 SetMode(BUILD)；批准问题的 AnswerQuestion approve 经同一对端判定后转为用户的 ApproveSpec 命令（CR-026）。",
    "新码 SPEC_APPROVAL_NOT_USER（ErrorCode，403）（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-008 的场景（旧：TEST-008-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-008-01",
    "requirement_id": "REQ-008",
    "acceptance_id": "AC-008-01",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI + 产品 host + FakeCommander + cto 假 Agent（交互答复）",
    "steps": [
      "派发起草 Task",
      "交互答复",
      "TUI 批准 CR",
      "比较 Git 写集与文件摘要"
    ],
    "pass_criteria": [
      "写集（含未跟踪与删除）只含 docs/spec/、docs/、.hoh/",
      "无写产品源文件的派发",
      "产品源文件摘要不变",
      "批准后有修订 2"
    ],
    "observables": [
      "git status 与 diff 的路径集合",
      "attempts 行",
      "文件摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-008-02",
    "requirement_id": "REQ-008",
    "acceptance_id": "AC-008-02",
    "check_id": "recovery",
    "fixture": "空目录 + 产品 host 进程（kill -9 与重启）+ cto 假 Agent + 用户答复与批准",
    "steps": [
      "StartRun 两次",
      "以 draft 答复 R1",
      "起草 Attempt 运行中 kill -9 并重启 host",
      "起草完成后 ApproveSpec",
      "以 draft 答复 R2"
    ],
    "pass_criteria": [
      "阶段一：两个 Run 阻塞在 runtime:no_spec，spec_id 为空，无 Task 与 Attempt",
      "阶段二：R1 经 CREATED、DISCOVERING（期间无 Attempt 与 effect）到 SPECIFYING 且 spec_id 为空",
      "恰一个 spec_id 为空的起草 Task 与一条 spec.drafting；git init 恰一次且为 effect",
      "重启后无第二个起草 Task、无重复 effect",
      "完成时无候选，问题 spec_approval，specs 无行，R2 仍阻塞",
      "阶段三：修订 1 与 spec.changed{cause:approved}，R1 采用并回到 SPECIFYING，有监督触发器",
      "R2 的 draft 等同 adopt，无 spec_id 为空的 Task"
    ],
    "observables": [
      "runs、tasks、attempts、effects、specs 行",
      "events导出",
      "git 配置与提交清单",
      "判定结果"
    ]
  },
  {
    "id": "TEST-008-03",
    "requirement_id": "REQ-008",
    "acceptance_id": "AC-008-03",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI + FakeCommander 提问",
    "steps": [
      "显示问题",
      "等待 10 s",
      "选第二项"
    ],
    "pass_criteria": [
      "TUI 显示问题与选项",
      "等待期间 BLOCKED(NEED_DECISION) 且 resume_state=SPECIFYING",
      "答复后回到 SPECIFYING 且记录 question_id 与 choice",
      "台账无该条"
    ],
    "observables": [
      "屏幕快照",
      "events导出",
      "runs 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-008-04",
    "requirement_id": "REQ-008",
    "acceptance_id": "AC-008-04",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander 计划 + 越界写的 cto 假 Agent",
    "steps": [
      "提交含 developer 写 Task 的计划",
      "起草 Task 修改、新建、删除产品源文件后完成"
    ],
    "pass_criteria": [
      "计划被拒且 details 含“PLAN 不实施”",
      "plan.write_violation 的 paths 恰为三个路径",
      "不提出 CR，Task BLOCKED(NEED_DECISION) 且问题列出路径",
      "没有新的已批准修订"
    ],
    "observables": [
      "decisions 与 tasks 行",
      "events导出",
      "change_requests 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-008-05",
    "requirement_id": "REQ-008",
    "acceptance_id": "AC-008-05",
    "check_id": "integration",
    "fixture": "受委托 cgroup 中的产品 host + 带 Attempt 标记的假 Agent 进程 P + P 的子 shell Q + 被放入 Attempt cgroup 的进程 Z + FakeCommander + 用户客户端",
    "steps": [
      "P 经 TCP、Unix socket 提交 ApproveSpec 并以 AnswerQuestion 答复 approve",
      "Q 清除标记后提交",
      "Z 在 Attempt cgroup 中提交",
      "用户客户端批准",
      "CR 待批时 P 再提交，用户批准 CR"
    ],
    "pass_criteria": [
      "P 的四次请求为 403 且 reason=marker",
      "Q 为 403 且 reason=ancestor_marker",
      "Z 为 403 且 reason=attempt_cgroup",
      "这些请求后无新修订、无采用、无产品写派发",
      "FakeCommander 输出不产生修订",
      "用户两次批准产生修订 1 与 2"
    ],
    "observables": [
      "HTTP 状态与错误体",
      "specs 与 runs 行",
      "attempts 行与 cgroup 成员",
      "判定结果"
    ]
  }
]
```

#### REQ-009 Build 与 Auto（3 条 AC）

`contracts/requirements.json` 中 REQ-009 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-009-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-009",
  "category": "UX",
  "priority": "P0",
  "milestone": "M3",
  "title": "Build 与 Auto",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "Build 遵守既定规格；Auto 可补规划；缺必需规格均不越过 readiness",
  "behavior": [
    "BUILD 遵守既定规格：Commander 的 create_tasks 只能引用当前已批准修订中的 REQ；BUILD 中不起草规格，需要改规格时提示用户切到 PLAN 或提 CR。",
    "AUTO 可补规划：规格缺必需项时 Runtime 派发起草 Task（REQ-008，写集同 PLAN），补充只经用户批准生效（O1）：交互时以问题请用户批准，无人值守时 Run 以 NEED_DECISION 停在批准前（-p 以 3 退出）；批准前无写产品源文件的派发，也没有新的已批准修订。",
    "BUILD 在缺必需规格时不自行补规格，相关 Task 为 BLOCKED（READINESS_NOT_MET）并在问题中提示可切到 AUTO 或 PLAN（C24）。",
    "模式与生命周期正交：PAUSED、BLOCKED 中可改模式，恢复时按新模式；PLAN→BUILD 只在规格已批准且 readiness 已过时才有写派发；BUILD/AUTO→PLAN 在安全点生效（REQ-007）。"
  ],
  "failure_modes": [
    "BUILD 中引用未批准 REQ 的 create_tasks 被拒（ILLEGAL_TRANSITION，details 写“BUILD 不改规格，用 /plan 或 CR”），task_requirements 不变。",
    "AUTO 的补充未批准时，没有写产品源文件的派发，也没有新的已批准修订。",
    "无人值守的 AUTO 缺规格时停在批准前（NEED_DECISION），不自批。"
  ],
  "doc_refs": [
    "docs/03-ux.md",
    "docs/06-orchestration.md",
    "CONTRACT.md",
    "docs/adr/ADR-031.md"
  ],
  "acceptance": [
    {
      "id": "AC-009-01",
      "given": "BUILD 的 Run，已批准修订含 REQ-A；Commander 先提出 create_tasks，一个 Task 引用 REQ-A，另一个引用不存在于修订的 REQ-Z；再提出只含引用 REQ-A 的 Task 的 create_tasks",
      "when": "Runtime 依次处理这两个决策",
      "then": "第一个决策 REJECTED（ILLEGAL_TRANSITION，details 含“BUILD 不改规格，用 /plan 或 CR”与 REQ-Z），没有导入任何 Task；第二个决策 APPLIED，导入引用 REQ-A 的 Task 且 task_requirements 恰为 REQ-A",
      "test_id": "TEST-009-01"
    },
    {
      "id": "AC-009-02",
      "given": "三个 Run 在同一缺 AC 的已批准修订上：一个交互的 AUTO、一个无人值守（-p）的 AUTO、一个 BUILD",
      "when": "三者各运行至停止派发",
      "then": "两个 AUTO 都派发起草 Task 并提出 CR，没有新的已批准修订、没有写产品源文件的派发；交互的以问题请用户批准；无人值守的 Run=BLOCKED(NEED_DECISION) 且 -p 以 3 退出；BUILD 不起草，相关 Task 为 BLOCKED(READINESS_NOT_MET) 且问题提示可切到 AUTO 或 PLAN",
      "test_id": "TEST-009-02"
    },
    {
      "id": "AC-009-03",
      "given": "模式状态机的全部 (状态, 模式, 目标模式) 组合表",
      "when": "逐项调用模式迁移判定",
      "then": "与表逐项一致：PAUSED 与 BLOCKED 中可改模式且恢复后按新模式；PLAN→BUILD 只在规格已批准且 readiness 已过时允许写派发；BUILD/AUTO→PLAN 有运行中写 Attempt 时为待生效",
      "test_id": "TEST-009-03"
    }
  ],
  "decisions": [
    "BUILD 缺规格时阻塞而不自动转 AUTO（CR-026，C24）。",
    "AUTO 的补充规格同样只由用户批准（O1）（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-009 的场景（旧：TEST-009-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-009-01",
    "requirement_id": "REQ-009",
    "acceptance_id": "AC-009-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander，BUILD 的 Run",
    "steps": [
      "提交含 REQ-Z 的 create_tasks",
      "提交只含 REQ-A 的 create_tasks"
    ],
    "pass_criteria": [
      "第一个 REJECTED 且 details 含提示与 REQ-Z，无 Task 导入",
      "第二个 APPLIED 且 task_requirements 恰为 REQ-A"
    ],
    "observables": [
      "decisions 行",
      "tasks 与 task_requirements 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-009-02",
    "requirement_id": "REQ-009",
    "acceptance_id": "AC-009-02",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander + cto 假 Agent + hoh -p，同一缺 AC 的修订",
    "steps": [
      "交互 AUTO、无人值守 AUTO 与 BUILD 各运行至停止派发"
    ],
    "pass_criteria": [
      "两个 AUTO 派发起草 Task 并提出 CR，无新修订、无产品写派发",
      "交互的请用户批准",
      "无人值守的 BLOCKED(NEED_DECISION) 且退出码 3",
      "BUILD 的 Task BLOCKED(READINESS_NOT_MET) 且问题提示切换"
    ],
    "observables": [
      "attempts、tasks 与 change_requests 行",
      "问题",
      "退出码",
      "判定结果"
    ]
  },
  {
    "id": "TEST-009-03",
    "requirement_id": "REQ-009",
    "acceptance_id": "AC-009-03",
    "check_id": "unit",
    "fixture": "hoh-core 模式迁移判定 + 组合表",
    "steps": [
      "逐项判定"
    ],
    "pass_criteria": [
      "与表逐项一致"
    ],
    "observables": [
      "判定表",
      "判定结果"
    ]
  }
]
```

#### REQ-017 运行中对话（5 条 AC）

`contracts/requirements.json` 中 REQ-017 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-017-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-017",
  "category": "UX",
  "priority": "P0",
  "milestone": "M3",
  "title": "运行中对话",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "Ask 不改任务；steer 有生效边界；CR 改规格；控制可独立于模型执行",
  "behavior": [
    "输入按种类路由（docs/03-ux.md）：普通输入默认给 Commander，@role 是稳定目标；ASK 走只读查询；STEER 记录生效边界；CHANGE 写 CR（REQ-033）；CONTROL 转为对应的控制命令（PauseRun、ResumeRun、CancelRun，C26）。输入状态为 QUEUED→DELIVERED→APPLIED|REJECTED。",
    "ASK 不改任务：回答作为消息返回，tasks 与 runs 的 revision 不变，不新增工作 Attempt；给非 Commander 职位的 ASK 不打断其运行中的 turn，在下一个安全点或经只读查询回答（C25）。",
    "STEER 的生效边界：绑定 Agent 支持 turn 内引导（mid_turn_steer）时在当前 turn 内投递，否则排到该职位的下一个 Attempt；事件 input.delivered{boundary:\"turn\"|\"next_attempt\"} 写明边界；新持有者的首个输入付清之前不投递（M2T17R-153）。",
    "@role 解析：一个 Agent 绑定多个职位时按当前任务的职位解析并显示实际目标，含糊时弹出选择器；投递只给当前 epoch 的绑定，换绑后迟到的投递被拒（STALE_ROLE_EPOCH，M2T17R-154）。",
    "投递循环（ADR-018 决定 4）接入 host：QUEUED 的输入至少投递一次，以 (consumer, message_id) 去重；consume 的 KeyReuse 计为一次拒绝，不重试到死信（M2T17R-254）；领取永不取验证交接（ADR-028 决定 9）；host 重启后未 ACK 的照常重投；接管之后的 query/steer 路由取代 deferred 回应，历史上 deferred 的不补投（ADR-028 决定 13）。",
    "控制可独立于模型执行：PauseRun、CancelRun 在 Commander 与全部 Agent 不可用时照常生效（AC-043-04 已判，本需求不重复）。"
  ],
  "failure_modes": [
    "ASK 之后 tasks 与 runs 的 revision 不变，没有新工作 Attempt。",
    "换绑后发往旧 epoch 的投递为 REJECTED（STALE_ROLE_EPOCH）。",
    "host 在投递后、ACK 前崩溃，重启后重投且只应用一次。",
    "CHANGE 在批准前不改规格与任务。"
  ],
  "doc_refs": [
    "docs/03-ux.md",
    "docs/06-orchestration.md",
    "docs/12-user-guide.md",
    "docs/adr/ADR-018.md",
    "docs/adr/ADR-024.md",
    "docs/adr/ADR-025.md",
    "docs/adr/ADR-028.md"
  ],
  "acceptance": [
    {
      "id": "AC-017-01",
      "given": "Run R 中 Task T1 运行，FakeCommander 可回答",
      "when": "向默认目标发 ASK“进度如何”",
      "then": "input_queue 中该输入依次为 QUEUED、DELIVERED、APPLIED 并带回答消息；全部 tasks 与 runs 的 revision 不变；没有新的工作 Attempt",
      "test_id": "TEST-017-01"
    },
    {
      "id": "AC-017-02",
      "given": "两个 Run 的 developer Attempt 运行中：一个绑定真实的 Codex CLI（App Server，支持 mid_turn_steer），另一个绑定不支持的假 Agent",
      "when": "各经产品 host 的 SendInput{input_kind:STEER, target_role:developer} 发一条引导",
      "then": "Codex 的一条经 host 的投递链在当前 turn 内到达（适配器的 steer 请求在该 turn 结束前发出），写 input.delivered{boundary:\"turn\"}；假 Agent 的一条在下一个 Attempt 的输入中收到，写 input.delivered{boundary:\"next_attempt\"}；两者都只投递一次",
      "test_id": "TEST-017-02"
    },
    {
      "id": "AC-017-03",
      "given": "已批准修订 1 的 Run，Task 运行中；Commander 与全部 Agent 都不可用（假 CLI 不响应）",
      "when": "发 CHANGE“把导出格式改为 CSV”，再发 CONTROL“暂停”",
      "then": "CHANGE 新增一条 PROPOSED 的 change_requests（REQ-033），批准前 specs、runs.spec_id 与 tasks 的状态不变；CONTROL 转为一条 PauseRun 命令，Run 经 PAUSE_REQUESTED 进入 PAUSED；两者都没有任何模型 turn 参与",
      "test_id": "TEST-017-03"
    },
    {
      "id": "AC-017-04",
      "given": "Agent claude 绑定 cto 与 developer；当前运行的是 developer 的 Task；随后 developer 换绑到 codex（epoch+1）",
      "when": "TUI 输入“@claude 先写测试”，随后向换绑前的 epoch 投递一条迟到的消息",
      "then": "消息栏显示实际目标 developer，input_queue 的 target_role=developer；迟到的投递为 REJECTED(STALE_ROLE_EPOCH)",
      "test_id": "TEST-017-04"
    },
    {
      "id": "AC-017-05",
      "given": "投递循环接入 host；三条 QUEUED 输入 M1、M2、M3，其中 M3 的 consume 会以 KeyReuse 失败；outbox 中有一条验证交接",
      "when": "投递一轮，在 M2 已投递、未 ACK 时 kill -9 host 并重启",
      "then": "按 message_id 逐条：M1 应用 1 次；M2 的传输投递 2 次（重启前后）而业务应用 1 次（receipts 中 (consumer, M2) 恰一行）；M3 应用 0 次、记一次拒绝且不进死信；验证交接没有被领取",
      "test_id": "TEST-017-05"
    }
  ],
  "decisions": [
    "ASK 不打断运行中的 turn（docs/12：问进度不会打断编码）（CR-026，C25）。",
    "CONTROL 输入即转为对应的控制命令，不经模型（CR-026，C26）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-017 的场景（旧：TEST-017-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-017-01",
    "requirement_id": "REQ-017",
    "acceptance_id": "AC-017-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander，T1 运行中",
    "steps": [
      "发 ASK"
    ],
    "pass_criteria": [
      "输入状态依次 QUEUED、DELIVERED、APPLIED 并带回答",
      "revision 不变",
      "无新工作 Attempt"
    ],
    "observables": [
      "input_queue 状态序列",
      "tasks/runs revision",
      "attempts 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-017-02",
    "requirement_id": "REQ-017",
    "acceptance_id": "AC-017-02",
    "check_id": "adapter-live",
    "fixture": "产品 host + 真实 Codex CLI（App Server，mid_turn_steer）+ 不支持的假 Agent（标为 fallback 夹具）",
    "steps": [
      "经 SendInput 各发 STEER"
    ],
    "pass_criteria": [
      "Codex 的经 host 投递链在当前 turn 内到达，boundary=turn",
      "假 Agent 的 boundary=next_attempt",
      "各只投递一次"
    ],
    "observables": [
      "适配器事件",
      "events导出",
      "假 Agent 输入",
      "判定结果"
    ]
  },
  {
    "id": "TEST-017-03",
    "requirement_id": "REQ-017",
    "acceptance_id": "AC-017-03",
    "check_id": "integration",
    "fixture": "产品 host，修订 1 的 Run，假 CLI 不响应",
    "steps": [
      "发 CHANGE",
      "发 CONTROL 暂停"
    ],
    "pass_criteria": [
      "一条 PROPOSED 的 change_requests",
      "specs、spec_id 与任务状态不变",
      "CONTROL 成为 PauseRun 且 Run 进入 PAUSED",
      "无模型 turn"
    ],
    "observables": [
      "change_requests 行",
      "commands 与 runs 行",
      "attempts 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-017-04",
    "requirement_id": "REQ-017",
    "acceptance_id": "AC-017-04",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI + 产品 host，claude 绑定两个职位，换绑",
    "steps": [
      "@claude 输入",
      "迟到投递"
    ],
    "pass_criteria": [
      "显示实际目标 developer 且 target_role=developer",
      "迟到投递 REJECTED(STALE_ROLE_EPOCH)"
    ],
    "observables": [
      "屏幕快照",
      "input_queue 行",
      "events导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-017-05",
    "requirement_id": "REQ-017",
    "acceptance_id": "AC-017-05",
    "check_id": "recovery",
    "fixture": "产品 host 进程 + 投递循环 + kill -9 + 重启",
    "steps": [
      "投递一轮",
      "已投递未 ACK 时 kill -9",
      "重启"
    ],
    "pass_criteria": [
      "M1 应用 1 次",
      "M2 传输 2 次、应用 1 次",
      "M3 应用 0 次、拒绝 1 次、无死信",
      "验证交接未被领取"
    ],
    "observables": [
      "outbox 与 input_queue",
      "receipts",
      "events导出",
      "判定结果"
    ]
  }
]
```

#### REQ-023 局部咨询与升级（4 条 AC）

`contracts/requirements.json` 中 REQ-023 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-023-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-023",
  "category": "TEAM",
  "priority": "P0",
  "milestone": "M3",
  "title": "局部咨询与升级",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "局部咨询不绕过全局任务账本；重大决定升级 Commander；讨论有轮数上限",
  "behavior": [
    "角色间咨询经 Router 成为全局账本中的子 Task（parent、role、预算、deadline），出现在 /v1/runs/{id}/tasks，结果回到请求方；没有账本之外的工作。",
    "wait-for 图检测环：A 等 B 的回答时 B 向 A 提问，后一请求被拒（CONSULT_CYCLE，details 列出环成员）；QA 请 Developer 改当前候选被拒并转为 Issue（ADR-028 决定 13）。",
    "讨论有轮数上限：同一争议的意见轮数上限取配置 [supervision] max_discussion_rounds（缺省 2，合法值 1–5 的整数，非法值使配置被拒；docs/06 的“默认可调上限”），超过上限的请求被拒（DISCUSSION_LIMIT），Commander 以 need_decision 被唤醒；没有新证据时提前停止；结论产出 Decision 或 ADR 引用。",
    "重大决定升级：咨询结论触及规格、ADR、公开接口或门禁时（C27），参与职位不自行应用，升级为 Commander 的 need_decision；规格变更再走 CR（REQ-033）。"
  ],
  "failure_modes": [
    "咨询成环时后一请求被拒（CONSULT_CYCLE），不建子 Task，不死锁。",
    "第三轮讨论被拒（DISCUSSION_LIMIT），Commander 被唤醒一次。",
    "改当前候选的请求被拒并开 Issue，不派发修改。"
  ],
  "doc_refs": [
    "docs/06-orchestration.md",
    "docs/adr/ADR-028.md",
    "docs/adr/ADR-025.md",
    "resources/prompts/researcher.md"
  ],
  "acceptance": [
    {
      "id": "AC-023-01",
      "given": "developer 的 Task 运行中，researcher 绑定可用",
      "when": "developer 请求 researcher 研究一个问题",
      "then": "Router 建一个子 Task（parent=该 Task、role=researcher、带预算与 deadline），出现在 /v1/runs/{id}/tasks；researcher 的结果作为消息回到 developer；Attempt 全部属于账本中的 Task",
      "test_id": "TEST-023-01"
    },
    {
      "id": "AC-023-02",
      "given": "developer 正等待 researcher 的回答；QA 正在验证当前候选",
      "when": "researcher 向 developer 提问；QA 请求 developer 修改当前候选",
      "then": "researcher 的请求被拒，reason=CONSULT_CYCLE 且 details 列出 developer 与 researcher，没有新子 Task；QA 的请求被拒并开一条 Issue，没有修改派发",
      "test_id": "TEST-023-02"
    },
    {
      "id": "AC-023-03",
      "given": "两个项目中各有一个架构争议：项目 A 用缺省配置，项目 B 配置 [supervision] max_discussion_rounds = 3",
      "when": "A 的参与者在两轮意见之后发起第三轮；B 的参与者发起第三轮，之后发起第四轮",
      "then": "A 的第三轮被拒（DISCUSSION_LIMIT），Commander 以 need_decision 被唤醒恰一次，唤醒摘要含两轮意见的引用，之后的结论记录为 Decision 或 ADR 引用；B 的第三轮被接受，第四轮被拒（DISCUSSION_LIMIT）并唤醒恰一次",
      "test_id": "TEST-023-03"
    },
    {
      "id": "AC-023-04",
      "given": "researcher 与 developer 的咨询结论是“改公开接口的签名”",
      "when": "参与职位尝试直接应用该结论",
      "then": "应用被拒，结论升级为 Commander 的 need_decision；没有 Task 在升级处理前按该结论派发",
      "test_id": "TEST-023-04"
    }
  ],
  "decisions": [
    "重大决定为触及规格、ADR、公开接口或门禁者（CR-026，C27）。",
    "讨论轮数上限由 [supervision] max_discussion_rounds 配置，缺省 2（docs/06 的默认可调上限）（CR-026，C28，CR26D-10）。",
    "新码 CONSULT_CYCLE、DISCUSSION_LIMIT 为 RejectionReason（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-023 的场景（旧：TEST-023-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-023-01",
    "requirement_id": "REQ-023",
    "acceptance_id": "AC-023-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent（developer、researcher）",
    "steps": [
      "developer 请求研究"
    ],
    "pass_criteria": [
      "子 Task 带 parent、role、预算、deadline 并出现在 tasks API",
      "结果回到 developer",
      "无账本外 Attempt"
    ],
    "observables": [
      "tasks 行与父子关系",
      "消息",
      "attempts 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-023-02",
    "requirement_id": "REQ-023",
    "acceptance_id": "AC-023-02",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent，developer 等待中，QA 验证中",
    "steps": [
      "researcher 提问 developer",
      "QA 请求改候选"
    ],
    "pass_criteria": [
      "CONSULT_CYCLE 且 details 列出环成员",
      "无新子 Task",
      "QA 请求被拒并开 Issue"
    ],
    "observables": [
      "请求状态与错误体",
      "issues 行",
      "tasks 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-023-03",
    "requirement_id": "REQ-023",
    "acceptance_id": "AC-023-03",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander，两个项目（缺省；max_discussion_rounds=3）",
    "steps": [
      "A 发起第三轮",
      "B 发起第三轮与第四轮"
    ],
    "pass_criteria": [
      "A 第三轮 DISCUSSION_LIMIT 且唤醒恰一次、摘要含两轮引用",
      "结论为 Decision 或 ADR 引用",
      "B 第三轮接受、第四轮 DISCUSSION_LIMIT 且唤醒恰一次"
    ],
    "observables": [
      "轮次计数",
      "唤醒记录",
      "decisions 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-023-04",
    "requirement_id": "REQ-023",
    "acceptance_id": "AC-023-04",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent 咨询结论",
    "steps": [
      "尝试应用"
    ],
    "pass_criteria": [
      "应用被拒并升级 need_decision",
      "处理前无派发"
    ],
    "observables": [
      "events导出",
      "decisions 行",
      "调度轮派发记录",
      "判定结果"
    ]
  }
]
```

#### REQ-040 停滞与诊断（4 条 AC）

`contracts/requirements.json` 中 REQ-040 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-040-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-040",
  "category": "WORK",
  "priority": "P0",
  "milestone": "M3",
  "title": "停滞与诊断",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "按问题指纹/状态变化而非commit计数诊断，升级/研究/改计划",
  "behavior": [
    "净进展是向量（docs/07 §2）：新增 accepted AC、关闭的 issue、重开的 issue、新增 critical/high、未消除的决策风险；判定不读提交数。",
    "连续 3 轮验证 accepted 不增、critical/high 不降、且失败 fingerprint 重复时，写恰好一条 no_progress{window:3, fingerprints, vector, last_progress_seq}，以 High 唤醒 Commander。",
    "窗口重设：一轮出现关闭的 issue 或 accepted 增加即重设；规格新增 REQ 时重设并在事件中写原因；发布稳定阶段问题总数下降即为进展。",
    "no_progress 的唤醒带诊断（重复的 fingerprint、最后一次进展的 seq、建议：升级、研究或改计划）；Commander 的回答可建研究 Task 或改计划；Runtime 不自动循环“继续”。",
    "no_progress 管“有推进而无净进展”，ADR-031 更正 C 的 STALLED 管“没有任何推进”；同一窗口内两者不对同一指纹重复唤醒（C29）。"
  ],
  "failure_modes": [
    "只改 README 或 docs 的三轮候选、提交数增加而 accepted 不增，照样判为无进展。",
    "同一窗口不写第二条 no_progress，重启后不重复。",
    "STALLED 已为同一状况唤醒时不再写 no_progress。"
  ],
  "doc_refs": [
    "docs/07-algorithms.md",
    "docs/06-orchestration.md",
    "docs/adr/ADR-031.md",
    "docs/adr/ADR-025.md",
    "docs/adr/ADR-029.md"
  ],
  "acceptance": [
    {
      "id": "AC-040-01",
      "given": "脚本化的验证结果：连续 3 轮同一失败 fingerprint F 复现，accepted AC 不增，critical/high 不降",
      "when": "Runtime 处理第 3 轮的结果，随后再处理一轮同样的结果并重启 host",
      "then": "第 3 轮后恰写一条 no_progress{window:3, fingerprints:[F], vector, last_progress_seq} 并以 High 唤醒 Commander；第 4 轮与重启后不再写第二条",
      "test_id": "TEST-040-01"
    },
    {
      "id": "AC-040-02",
      "given": "三轮候选只改 README 与 docs/，提交数各增加，accepted 不增",
      "when": "对三轮结果调用进展向量函数与判定",
      "then": "判定为无进展；进展向量中没有提交数分量，提交数变化不改变判定",
      "test_id": "TEST-040-02"
    },
    {
      "id": "AC-040-03",
      "given": "四个结果序列：第 2 轮关闭一个 issue；第 2 轮 accepted 增加；第 2 轮规格新增一条 REQ；发布稳定阶段问题总数从 5 降到 3",
      "when": "对每个序列逐轮判定",
      "then": "前两个序列在第 2 轮重设窗口、第 3 轮不判无进展；第三个序列重设并在事件中写原因“规格新增 REQ”；第四个序列判为进展",
      "test_id": "TEST-040-03"
    },
    {
      "id": "AC-040-04",
      "given": "两个 Run 各有一次 no_progress 唤醒 Commander：Run X 的 FakeCommander 以改计划回答（取消重复失败的 Task、以 create_tasks 建替代 Task），Run Y 的以建研究 Task 回答",
      "when": "两个 Commander 各处理其唤醒",
      "then": "两份唤醒摘要都含重复的 fingerprint、最后一次进展的 seq，以及升级、研究、改计划三项建议；X 中原 Task 为 CANCELLED、替代 Task 被导入；Y 中出现 role=researcher 的新 Task；两个 Run 中 Runtime 都没有自行派发“继续”的 Commander turn",
      "test_id": "TEST-040-04"
    }
  ],
  "decisions": [
    "窗口 3 轮为初值（docs/07 §2），固定写在 Runtime（CR-026）。",
    "no_progress 与 STALLED 分工如上，同一状况只唤醒一次（CR-026，C29）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-040 的场景（旧：TEST-040-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-040-01",
    "requirement_id": "REQ-040",
    "acceptance_id": "AC-040-01",
    "check_id": "integration",
    "fixture": "产品 host + 脚本化验证结果 + 重启",
    "steps": [
      "处理三轮结果",
      "处理第 4 轮",
      "重启"
    ],
    "pass_criteria": [
      "恰一条 no_progress 且载荷字段齐全",
      "High 唤醒",
      "第 4 轮与重启后无第二条"
    ],
    "observables": [
      "events导出",
      "唤醒记录",
      "判定结果"
    ]
  },
  {
    "id": "TEST-040-02",
    "requirement_id": "REQ-040",
    "acceptance_id": "AC-040-02",
    "check_id": "unit",
    "fixture": "进展向量函数 + 三轮只改文档的结果",
    "steps": [
      "计算向量并判定"
    ],
    "pass_criteria": [
      "判定无进展",
      "向量无提交数分量"
    ],
    "observables": [
      "向量",
      "判定结果"
    ]
  },
  {
    "id": "TEST-040-03",
    "requirement_id": "REQ-040",
    "acceptance_id": "AC-040-03",
    "check_id": "unit",
    "fixture": "进展判定 + 四个结果序列",
    "steps": [
      "逐轮判定"
    ],
    "pass_criteria": [
      "前两个第 2 轮重设",
      "第三个重设并写原因",
      "第四个判为进展"
    ],
    "observables": [
      "判定序列",
      "事件载荷",
      "判定结果"
    ]
  },
  {
    "id": "TEST-040-04",
    "requirement_id": "REQ-040",
    "acceptance_id": "AC-040-04",
    "check_id": "integration",
    "fixture": "产品 host + 两个 FakeCommander（改计划；建研究 Task）",
    "steps": [
      "两个 Run 各处理唤醒"
    ],
    "pass_criteria": [
      "摘要含 fingerprint、最后进展 seq 与三项建议",
      "X 的原 Task CANCELLED 且替代 Task 导入",
      "Y 出现 researcher 的新 Task",
      "无自动“继续” turn"
    ],
    "observables": [
      "唤醒摘要",
      "tasks 行",
      "attempts 行",
      "判定结果"
    ]
  }
]
```


### 7. M3 实现任务的拆分（`contracts/development-tasks.json`）

M3-T02、M3-T03、M3-T04 保留 id 作为各组的第一个任务，新增 M3-T06 至 M3-T10；M3-T01（spec）与 M3-T05（verify）不变，只是 M3-T05 的 `depends_on` 改为全部八个实现任务。每个实现任务至多 2 项需求、8 条 AC：

| 任务 | 需求（AC 数） | AC | depends_on |
|---|---|---|---|
| M3-T02 | REQ-034（4）、REQ-033（4） | 8 | M3-T01 |
| M3-T06 | REQ-031（5）、REQ-030（3） | 8 | M3-T02 |
| M3-T03 | REQ-045（4）、REQ-047（3） | 7 | M3-T02 |
| M3-T09 | REQ-007（3）、REQ-017（5） | 8 | M3-T02 |
| M3-T04 | REQ-008（5）、REQ-009（3） | 8 | M3-T06、M3-T09 |
| M3-T08 | REQ-029（3）、REQ-046（3） | 6 | M3-T04、M3-T03 |
| M3-T07 | REQ-032（4）、REQ-028（4） | 8 | M3-T04、M3-T06 |
| M3-T10 | REQ-023（4）、REQ-040（4） | 8 | M3-T09 |
| 合计 | 16 项 | 61 |  |


替换后的八个任务（M3-T02、M3-T03、M3-T04 的旧条目由下文取代；新条目插在 M3-T05 之前）：

```json
[
  {
    "id": "M3-T02",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "文档事实源与规格变更（规格位置与冻结形式、索引、CR、影响闭包、证据失效）",
    "depends_on": [
      "M3-T01"
    ],
    "requirements": [
      "REQ-034",
      "REQ-033"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。规格变化置 STALE 时写 candidate.stale{candidate_id, reason}（ADR-028 决定 11 的持久取代事实）；规格事实源的位置替换 M2-T16 冻结使用的约定路径（ADR-028 决定 4），冻结形式为 docs/spec/index.json，索引带 scenarios（TEST→check，CR-026）；以 SPEC_MISMATCH 被挡住的写 Task 经规格变更流程放行；spec.changed 的产生，并把验证中的候选按 REQ-053 行为 7 置 STALE；旧规格下的开发 Task 的迁移或替代（含 task_requirements 的需求映射，使它们在新规格下可派发）（CR-018）。规格位置不再指向 contracts/requirements.json 后，删去 persistence_scan 的对应条目（CR-019）。"
  },
  {
    "id": "M3-T06",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Readiness Gate（含独立规格审查）与规格收敛（假设台账、来源校验）",
    "depends_on": [
      "M3-T02"
    ],
    "requirements": [
      "REQ-031",
      "REQ-030"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。按任务范围选代码的上下文来源（REQ-039，ADR-026 决定 12）在本任务实现：Task 的路径范围随计划与 readiness 出现（CR-016），M3-T07 的脏文件判定使用它。独立规格审查（审查 Task 的建立与派发、contracts/spec-review-report.schema.json 的报告收取、spec_review.opened 与 spec_review.reported）在本任务实现（CR-026，CR26D-05、-101）。配置键 [assumptions] 的加载器支持（解析、键名与值的校验、保留名拒绝、缺省空表、进入 Run 的配置快照）与 config/hoh.example.toml 中的空表在同一提交加入，并在该提交上重跑 TEST-018-01（cargo test -p hoh-core test_018_01）（CR-026，CR26D-103）。"
  },
  {
    "id": "M3-T03",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "四层Prompt版本、自定义与升级",
    "depends_on": [
      "M3-T02"
    ],
    "requirements": [
      "REQ-045",
      "REQ-047"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。"
  },
  {
    "id": "M3-T09",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Slash 统一操作与运行中对话（投递循环接入 host）",
    "depends_on": [
      "M3-T02"
    ],
    "requirements": [
      "REQ-007",
      "REQ-017"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。steer/query 的产品路由与投递：ADR-018 决定 4 的投递循环接入 host 与它的消费者（领取永不取验证交接，ADR-028 决定 9），当前 epoch 的路由，M2T17R-153（新持有者首个输入之前不投递）、M2T17R-154（哪些 Commander 消息还要核对接收方的 epoch）、M2T17R-254（KeyReuse 计为一次拒绝、不重试到死信）（ADR-025 决定 10）；接管之后的 query/steer 路由（取代 M2-T16 的 deferred 回应），历史上 deferred 的消息不补投（ADR-028 决定 13）（CR-018）。用户输入交给 Commander（ADR-025 决定 1）。配置键 [ui] mode_cycle 的加载器支持（解析、合法值、缺省、快照）与 config/hoh.example.toml 中的缺省值在同一提交加入，并在该提交上重跑 TEST-018-01（CR-026，CR26D-103）。改动进程与并发，按 docs/19 由两名互补审查者审查。"
  },
  {
    "id": "M3-T04",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Plan 与 Build/Auto（首次批准之前的起草、起草 Task、批准者判定）",
    "depends_on": [
      "M3-T06",
      "M3-T09"
    ],
    "requirements": [
      "REQ-008",
      "REQ-009"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。所有者 2026-11-17 对 C22 的决定 (a) 在本任务实现（CR-026 第 2 节）：迁移 002（contracts/sql/002_spec_drafting.sql，persistence 白名单的条目已由 M3-T01 加入）加入 MIGRATIONS；迁移运行器对每个迁移在 BEGIN IMMEDIATE 之前关闭 foreign_keys，COMMIT 之前执行 PRAGMA foreign_key_check 且必须无行，之后无论成败恢复 foreign_keys=ON 并核对；迁移的往返与回滚测试（受锁 SQL 的“Evolution”注释）：有数据的 v1 库迁到 v2 后各表行数与值不变、foreign_key_check 为空、两个索引与两个触发器都在，迁移中途失败回滚到 v1，旧二进制拒绝 v2 库（SchemaTooNew）；SQL 层的负例：kind=work 而 spec_id 为空被 CHECK 拒，项目已有已批准修订时插入 spec_id 为空的 Task 被触发器拒，spec_id 在空与非空之间变化或 kind 变化被触发器拒。CR-026 第 2 节清单中按 tasks.spec_id 非空写的代码路径在本任务改为处理空值。起草 Task（Runtime 建、SPECIFYING 中派发、完成即送批或提出 CR）、Runtime 推进 CREATED→DISCOVERING→SPECIFYING（发现的内容 run.baseline 与离开 DISCOVERING 的条件由 M3-T07 补上）、首个起草 Attempt 准备阶段的 git init、批准问题 spec_approval、Plan 写集的核对与 ApproveSpec 的对端进程判定在本任务实现（CR26D-06、-11、-105）；对端判定另有单元测试：找不到对端、/proc 不可读、对端属于另一个 uid 都拒绝为 origin_unknown。"
  },
  {
    "id": "M3-T08",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "内置模板与内置Skill（含首次起草时的模板实例化）",
    "depends_on": [
      "M3-T04",
      "M3-T03"
    ],
    "requirements": [
      "REQ-029",
      "REQ-046"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。Skill 正文按需加载与短索引（REQ-039 行为 7 的前半，ADR-026）在本任务实现。首个起草 Attempt 开始之前的模板实例化（规格骨架与有效档的全部模板，记为该 Attempt 的 effect，不覆盖已有文件）在本任务实现（CR-026 的 C22 决定 (a)，CR26D-154）。"
  },
  {
    "id": "M3-T07",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "需求追踪与项目发现",
    "depends_on": [
      "M3-T04",
      "M3-T06"
    ],
    "requirements": [
      "REQ-032",
      "REQ-028"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。TEST-032-01 的测试在 tests/bundle、名称含 test_032_01（traceability 自测据此定位），运行产品当前的导出（CR-017，ADR-027 的移交，CR-026）。追踪读取候选 spec 文件的 scenarios：scripts/product/traceability.py 的 World.tests() 与产品的追踪导出用同一规则，只有没有 scenarios 键的 spec 文件才读 contracts/acceptance-scenarios.json（CR-026，CR26D-102）。DISCOVERING 中的项目发现与 run.baseline、离开 DISCOVERING 的条件（基线之后）在本任务实现，接上 M3-T04 的阶段推进（CR26D-151）。"
  },
  {
    "id": "M3-T10",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "局部咨询与停滞诊断",
    "depends_on": [
      "M3-T09"
    ],
    "requirements": [
      "REQ-023",
      "REQ-040"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。角色间咨询的 wait-for 环检测（CR-018）；no_progress、milestone_ready 与 release_request 的产生方（ADR-025 决定 1；后两者没有 M3 的 AC，其定义在本任务的 ADR 中给出，不改变 REQ-021 的唤醒集合）。配置键 [supervision] max_discussion_rounds 的加载器支持（解析、合法值、缺省、快照）与 config/hoh.example.toml 中的缺省值在同一提交加入，并在该提交上重跑 TEST-018-01（CR-026，CR26D-103）。"
  }
]
```


M3-T05 的 `depends_on`：

- 旧：`["M3-T02", "M3-T03", "M3-T04"]`
- 新：`["M3-T02", "M3-T03", "M3-T04", "M3-T06", "M3-T07", "M3-T08", "M3-T09", "M3-T10"]`

拆分的理由：

- M3-T02 先行：规格的位置、冻结形式（含 scenarios）、索引与修订是其余各组的前提。它承接 CR-018 给 M3-T02 的全部项与 CR-019 的条目删除。
- M3-T06（readiness、独立规格审查与收敛）承接 REQ-039 按任务范围选代码（CR-016）：Task 的路径范围随计划与 readiness 出现；独立规格审查的派发与报告收取也在这里（CR26D-05、-101）；`[assumptions]` 的加载器支持与 example 在这里（CR26D-103）。
- M3-T03（四层 Prompt 与覆盖）依赖 M3-T02：项目层引用已批准规格。
- M3-T09（SetMode、斜杠与运行中对话）依赖 M3-T02（CHANGE 写 CR）；承接 CR-018 给 M3-T04 的投递项，是进程与并发改动，按 docs/19 用两名互补审查者；`[ui] mode_cycle` 在这里。
- M3-T04（Plan 与 Build/Auto）依赖 M3-T06（readiness 与规格审查）与 M3-T09（SetMode 的安全点，APPROVE_AND_BUILD）；C22 (a) 的迁移 002 与运行器、起草 Task、阶段推进、git init、批准问题、写集核对与批准者判定都在这里（第 2 节，CR26D-06、-11、-105）。第 2 稿依赖 M3-T08 的 draft 入口，第 3 稿反过来：模板实例化接在起草 Task 上，所以 M3-T08 依赖 M3-T04。
- M3-T08（模板与 Skill）依赖 M3-T04（首个起草 Attempt 的准备阶段是模板的产品入口，CR26D-154）与 M3-T03（模板与 Skill 也按四层覆盖）；承接 ADR-026 的 Skill 短索引。
- M3-T07（追踪与发现）依赖 M3-T04（阶段推进与起草，AC-028-01、-04）与 M3-T06（AC-028-03 的脏文件判定用 Task 的路径范围，CR26D-04）；承接 CR-017 的 TEST-032-01 位置与追踪对 scenarios 的读取（CR26D-102），以及 run.baseline 与离开 DISCOVERING 的条件（CR26D-151）。
- M3-T10（咨询与停滞诊断）依赖 M3-T09（咨询经投递循环路由）；承接 wait-for 环检测与 no_progress 等的产生方；`[supervision] max_discussion_rounds` 在这里。

### 8. 每条 AC 的依赖闭包（CR3R-02，CR26D-04）

下表按每条 AC 实际用到的能力列出提供者（“本任务”或它的祖先）。表中点名的每个 M3 任务都已由生成器核对为该 AC 所属任务本身或其祖先；所需检查的交付者（validate_bundle 的归属规则）全在 M3-T01 的祖先中：

| 任务 | AC | 检查 | 实际用到的能力与提供者（“本任务”或其祖先） |
|---|---|---|---|
| M3-T02 | AC-034-01 | integration | ApproveSpec 的受理与 index.json 的生成（本任务）；冻结（M2-T10、M2-T16）；产品 host 与 HTTP（M2-T12） |
| M3-T02 | AC-034-02 | integration | ApproveSpec 的摘要核对（本任务）；HTTP 错误体（M2-T12） |
| M3-T02 | AC-034-03 | integration | ProposeSpecChange 与索引构建（本任务）；调度轮与 FakeAgent（M2-T03、M2-T07） |
| M3-T02 | AC-034-04 | unit | 索引生成器与冻结校验（本任务） |
| M3-T02 | AC-033-01 | integration | change_requests 与影响闭包、ApproveSpec（本任务） |
| M3-T02 | AC-033-02 | recovery | 验证派发、报告与关轮（M2-T16、M2-T18）；取消阶梯与终结证明（M2-T06）；kill -9 恢复（M2-T05）；规格变化的失效（本任务） |
| M3-T02 | AC-033-03 | integration | acceptance API 与投影（M2-T18）；完成门禁（M2-T13）；规格修订与失效（本任务） |
| M3-T02 | AC-033-04 | integration | task_requirements 与 SPEC_MISMATCH（M2-T16）；Commander 的 create_tasks（M2-T13）；迁移（本任务） |
| M3-T06 | AC-031-01 | unit | readiness 纯函数（本任务）；dag::validate（M2-T03） |
| M3-T06 | AC-031-02 | integration | 调度轮（M2-T03）；Task 的 BLOCKED 声明（M2-T19）；readiness（本任务） |
| M3-T06 | AC-031-03 | unit | readiness 与 Run 投影（本任务、M2-T18） |
| M3-T06 | AC-031-04 | integration | Commander 决策（M2-T08、M2-T13）；规格批准（M3-T02）；规格审查 Task 的建立与派发、spec-review-report 的收取与会话独立判定（本任务，REQ-053 的会话规则） |
| M3-T06 | AC-031-05 | unit | readiness 的规格审查判定（本任务） |
| M3-T06 | AC-030-01 | integration | 问题与答复（M2-T13）；规格提议与批准（M3-T02）；来源校验（本任务） |
| M3-T06 | AC-030-02 | integration | hoh -p 的退出码（M2-T13）；Commander 的 record_assumption 与 request_user_decision（M2-T08、M2-T13）；[assumptions] 的加载器支持、basis 校验与保留名拒绝（本任务，REQ-018 的加载器 M2-T07） |
| M3-T06 | AC-030-03 | integration | ProposeSpecChange（M3-T02）；来源校验（本任务） |
| M3-T03 | AC-045-01 | integration | 装箱与 prompt_revisions（M2-T09）；四层来源记录（本任务） |
| M3-T03 | AC-045-02 | unit | 合成器（本任务）；完成门禁（M2-T13） |
| M3-T03 | AC-045-03 | integration | 产品执行器的启动拒绝（M2-T07）；变量白名单（本任务） |
| M3-T03 | AC-045-04 | unit | 覆盖保存校验（本任务） |
| M3-T03 | AC-047-01 | integration | 覆盖链与 global 层（本任务）；产品执行器（M2-T07） |
| M3-T03 | AC-047-02 | tui-pty | TUI 设置页（M1-T08、M2-T13）；diff 与恢复默认（本任务） |
| M3-T03 | AC-047-03 | deployment | 发布包与安装（M2-T14）；职位 checkpoint（M2-T07）；资源包版本与漂移（本任务） |
| M3-T09 | AC-007-01 | tui-pty | TUI 与 PTY（M1-T08、M2-T13）；SetMode 的受理与 [ui] mode_cycle 的加载器支持（本任务，加载器 M2-T07） |
| M3-T09 | AC-007-02 | integration | 调度轮与运行中的写 Attempt（M2-T03、M2-T07）；模式的安全点（本任务） |
| M3-T09 | AC-007-03 | tui-pty | TUI 的斜杠与选择器（M2-T13）；投递（本任务） |
| M3-T09 | AC-017-01 | integration | SendInput 与 input_queue（M2-T13）；投递循环（本任务） |
| M3-T09 | AC-017-02 | adapter-live | Codex App Server 的 steer（M1-T04，runner 模式 M2-T07）；adapter-live 检查（M1-T03）；投递链（本任务） |
| M3-T09 | AC-017-03 | integration | change_requests（M3-T02）；控制面（M2-T06）；CONTROL 的转换（本任务） |
| M3-T09 | AC-017-04 | tui-pty | TUI（M2-T13）；role_epoch 与换绑（M2-T07、M2-T17）；@role 解析（本任务） |
| M3-T09 | AC-017-05 | recovery | outbox、receipts 与恢复（M2-T04、M2-T05）；投递循环接入 host（本任务） |
| M3-T04 | AC-008-01 | tui-pty | TUI（M2-T13）；起草 Task 与写集核对（本任务）；CR 与批准（M3-T02） |
| M3-T04 | AC-008-02 | recovery | runtime:no_spec 与答复（M2-T13）；ApproveSpec（M3-T02）；draft 选项、阶段推进、迁移 002、起草 Task 与批准问题（本任务）；kill -9 与 reconcile（M2-T05、M2-T06）；Commander 唤醒（M2-T08） |
| M3-T04 | AC-008-03 | tui-pty | 问题与答复、BLOCKED 声明（M2-T13、M2-T19）；TUI（M2-T13） |
| M3-T04 | AC-008-04 | integration | Commander 计划（M2-T13）；起草 Task 的写集核对（本任务） |
| M3-T04 | AC-008-05 | integration | ApproveSpec（M3-T02）；起草 Task 与批准问题、对端进程判定（标记、祖先、cgroup）（本任务）；Attempt 标记与受委托 cgroup（M2-T06） |
| M3-T04 | AC-009-01 | integration | Commander 的 create_tasks（M2-T13）；BUILD 的规格约束（本任务） |
| M3-T04 | AC-009-02 | integration | 起草 Task 与批准问题（本任务）；readiness（M3-T06）；hoh -p 的退出码（M2-T13） |
| M3-T04 | AC-009-03 | unit | 模式迁移判定（本任务，SetMode 来自 M3-T09） |
| M3-T08 | AC-029-01 | unit | 模板包（本任务） |
| M3-T08 | AC-029-02 | integration | 配置的 document_profile 与其来源（本任务，M2-T07 的加载器） |
| M3-T08 | AC-029-03 | deployment | 发布包、安装与 doctor（M2-T14、M1-T07）；hoh -p --resume 的答复（M2-T13）；draft 选项与起草 Task、批准问题（M3-T04）；起草前的模板实例化（本任务） |
| M3-T08 | AC-046-01 | integration | Prompt 装箱与知识份额（M2-T09）；Skill 短索引与注入（本任务） |
| M3-T08 | AC-046-02 | integration | 四层合成与修订记录（M3-T03）；Skill 注入（本任务） |
| M3-T08 | AC-046-03 | unit | registry 校验（本任务） |
| M3-T07 | AC-032-01 | traceability | traceability 检查（M2-T10）；产品追踪导出（本任务）；验收结果（M2-T18） |
| M3-T07 | AC-032-02 | integration | acceptance 与 tasks API（M2-T18）；导出（本任务） |
| M3-T07 | AC-032-03 | integration | 规格修订与失效（M3-T02）；导出（本任务） |
| M3-T07 | AC-032-04 | integration | ApproveSpec、index.json 的 scenarios 与冻结（M3-T02）；追踪对 scenarios 的读取与产品导出（本任务） |
| M3-T07 | AC-028-01 | integration | Runtime 推进 CREATED→DISCOVERING（M3-T04）；只读 git 帮手（M2-T06）；基线（本任务） |
| M3-T07 | AC-028-02 | integration | 用户确认（M2-T13 的问题与答复）；基线（本任务） |
| M3-T07 | AC-028-03 | integration | Task 的路径范围（M3-T06）；计划导入（M2-T13）；脏文件判定（本任务） |
| M3-T07 | AC-028-04 | integration | draft 选项、起草 Task 与准备阶段的 git init（M3-T04）；run.baseline 与它的次序（本任务） |
| M3-T10 | AC-023-01 | integration | 子 Task 与 Router（本任务）；投递（M3-T09） |
| M3-T10 | AC-023-02 | integration | wait-for 图（本任务，Reader::wait_for 来自 M2-T16）；验证者请求的拒绝与 Issue（M2-T16） |
| M3-T10 | AC-023-03 | integration | 讨论轮数与 [supervision] max_discussion_rounds 的加载器支持（本任务，加载器 M2-T07）；Commander 唤醒（M2-T08） |
| M3-T10 | AC-023-04 | integration | 升级为 need_decision（本任务、M2-T08） |
| M3-T10 | AC-040-01 | integration | 验证结果与指纹（M2-T18）；no_progress（本任务）；唤醒（M2-T08）；重启（M2-T06） |
| M3-T10 | AC-040-02 | unit | 进展向量（本任务） |
| M3-T10 | AC-040-03 | unit | 进展判定（本任务）；规格修订（M3-T02） |
| M3-T10 | AC-040-04 | integration | Commander 的取消与 create_tasks（M2-T13）；唤醒摘要（本任务） |


## CTO 的问题（C1–C35）：提议与理由，**待 CTO 决定**

下列提议已按“推荐”写进上文的需求；CTO 改变任一项时，相应需求的文字随之修改后再送设计审查。第 2 稿改了 C16、C21、C28；第 3 稿记入所有者对 C22 的决定，新增 C30–C35（标“第 3 稿”）。

| ID | 问题 | 提议 | 理由 |
|---|---|---|---|
| C1 | SPEC_CONFLICT 是新码还是 CONFLICT 的 details | 新码，409 | CONFLICT 已指命令 id 冲突；同码两义会让客户端误判 |
| C2 | 影响闭包的算法 | 按 REQ→AC→TEST→TASK 的显式映射，不按路径 | 映射都在库中（requirements、acceptance_criteria、task_requirements），可判定 |
| C3 | CR 是否另写文件进规格根 | 不另写；CR 是 change_requests 行，提议的内容就是工作树中的规格与索引，以摘要锁定 | 避免规格之外的第二份文本，符合 REQ-034 |
| C4 | 关键决定与非阻塞未知的标记 | 规格开放问题条目带“阻塞：是/否”，缺省为阻塞 | 写在事实源里，readiness 是纯函数；缺省阻塞是保守的一侧 |
| C5 | readiness 由谁判定 | Runtime 自动判定，Commander 不能覆盖；独立规格审查是它的输入 | docs/06 的固定工程约束与规格循环；可测 |
| C6 | 假设台账的载体 | record_assumption 事件的投影，不新增表 | 不改受锁 SQL；事件已有 basis |
| C7 | 五节的模板归属 | REQ-029 的规格模板 | 模板是 REQ-029 的产物，REQ-030 只校验内容 |
| C8 | REQ-032 是产品功能还是本仓库的工具 | 产品功能；tests/bundle 的 test_032_01 运行产品当前导出并以 traceability 检查校验 | 同时满足 summary 与 CR-017 的测试位置 |
| C9 | 追踪的产品入口 | M3 为 API 与 TUI 的状态摘要；Web 视图属 M6 | M3 的任务闭包里没有 web-e2e |
| C10 | 构建与测试命令的检测 | 按 Cargo.toml、package.json 的 scripts、Makefile、pyproject.toml 等约定；检测不到为 unknown | 不猜；unknown 如实显示 |
| C11 | 基线随 Run 还是随项目 | 随 Run（run.baseline） | 工作树在 Run 之间会变 |
| C12 | 契约长度上限 | 32 KiB（UTF-8 字节） | 内置六份为 4.4–7.5 KB，留四倍余量；字节可判定 |
| C13 | 模板变量 | 白名单只有 GOAL、PROJECT_CONTEXT、TASK_CONTEXT、EVIDENCE_CONTEXT | 与 validate_bundle 对内置契约的检查一致 |
| C14 | 资源升级的生效点 | ADR-022 决定 8 的职位 checkpoint | 与团队变更同一机制 |
| C15 | global 层位置 | `$XDG_CONFIG_HOME/hoh/`（缺省 `~/.config/hoh/`） | 用户级配置的惯例；不在项目里 |
| C16（第 2 稿） | 未指定档时的选档 | 用配置缺省 full（REQ-018 与 example 不变），记录档与来源（project 或 default） | 第 1 稿的“缺省 standard”会改 REQ-018 的缺省与 AC-018-01（CR26D-09）；保持缺省不变是不改行为的一侧 |
| C17 | 模板的分发方式 | 嵌入二进制，计入 resource_bundle_version | 离线可用 |
| C18 | Skill 正文的注入时机 | 按阶段与任务注入，不由 Agent 自行请求 | 可判定、可复现；记入 prompt 修订 |
| C19 | 转给底座的斜杠语法 | `/agent <role> <text>` | 目标显式；/plan 等 HoH 命令永不转发 |
| C20 | 未附着时 `/plan` 的作用对象 | 只改下一个由 TUI 启动的 Run，不改 project.default_mode | 不让一次按键改项目配置 |
| C21（第 2 稿） | Shift+Tab 的顺序 | `[ui] mode_cycle` 配置，缺省 PLAN→BUILD→AUTO，合法值为 2–3 个互不相同的模式；AC-007-01 覆盖非缺省顺序 | docs/03 写“配置可改”；第 1 稿钉死顺序是静默的范围缩减（CR26D-56） |
| C22（第 3 稿） | 没有已批准规格时 Plan 的首次起草 | **所有者 2026-11-17 已决定：(a)**（起草是 spec_id 为空的 Task，迁移 002 守护） | 不再由 CTO 决定；(a) 保留 CONTRACT 第 4 条的交互、Q1(a)、M2T13D-59 与 O1，并让起草得到 Task 的全部执行保证 |
| C23 | 原型工件的允许路径 | 只在 docs/ 下（与 docs/spec/、.hoh/ 共同构成 Plan 的写集） | 写集可由 Git 核对 |
| C24 | BUILD 缺规格时 | 阻塞（READINESS_NOT_MET），问题提示可切到 AUTO 或 PLAN，不自动转 AUTO | 模式由用户选；不越过 readiness |
| C25 | ASK 是否打断运行中的 turn | 不打断；在下一个安全点或经只读查询回答 | docs/12：问进度不会打断编码 |
| C26 | CONTROL 输入与控制命令 | CONTROL 即转为 PauseRun、ResumeRun 或 CancelRun，不经模型 | 控制面独立于模型（AC-043-04 已判） |
| C27 | 何为重大决定 | 触及规格、ADR、公开接口或门禁者 | 可判定，覆盖会改变用户可见行为的决定 |
| C28（第 2 稿） | MOA 轮数是否可配置 | `[supervision] max_discussion_rounds`，缺省 2，合法值 1–5；AC-023-03 覆盖非缺省值 | docs/06 写“默认可调上限”；第 1 稿固定两轮并留给以后是范围缩减（CR26D-10） |
| C29 | no_progress 与 STALLED 的分工 | STALLED 管“没有任何推进”，no_progress 管“有推进而无净进展”；同一状况只唤醒一次 | 避免两套探测对同一状况两次唤醒 |
| C30（第 3 稿） | 独立规格审查的报告走哪条路 | 新受锁契约 spec-review-report.schema.json，事件 spec_review.opened / reported；不用 independent_reports、不开验证轮次、不造候选 | 对象是规格修订；independent_reports 的候选外键与 round_of_task 只适用于候选；“规格候选”还要 test plan 与环境，新项目在首次批准时没有，也会被当成产品候选参与 STALE 与发布（CR26D-101） |
| C31（第 3 稿） | 用户项目的 TEST→check 与发布目标 | index.json 带 scenarios；release_targets 恒为 ["project"]，release_class 取 class 行（缺省 required） | 一个 FileRef 冻结全部追踪输入；用户项目不需要 contracts/ 目录（CR26D-102） |
| C32（第 3 稿） | 新配置键何时进入 example | 与加载器的支持同一提交，在用它的任务中，并重跑 TEST-018-01 | 先进 example 会使 AC-018-01 失败（CR26D-103） |
| C33（第 3 稿） | 批准问题能否以 AnswerQuestion 答复 | 能：approve 经同一对端判定后转为该用户的 ApproveSpec 命令，修订仍只由 ApproveSpec 写；approve 之外的答复为 REJECT | 用户在 `-p` 与 TUI 中都有一条答复路径；判定不因入口而异，不留绕过 O1 的口子；守住所有者“只经用户的 ApproveSpec 成为修订 1” |
| C34（第 3 稿） | CREATED→DISCOVERING→SPECIFYING 由谁推进 | Runtime（常规阶段自动流转），run.baseline 之前不离开 DISCOVERING；Commander 的 transition_run 受同一条件 | CONTRACT 第 6 条与 ADR-031 的移交；首次批准之前没有 Commander 可以推进（CR26D-151） |
| C35（第 3 稿） | 对端无法确认时 | 拒绝（origin_unknown），不当作用户批准 | O1 是“只有用户”；无法确认的请求不能证明来自用户（CR26D-105） |

## 所有者的决定（C22）

2026-11-17，CTO 向所有者提出 C22（“没有已批准规格的项目，Plan 怎样生成第一份规格？”，备选 (a) 起草作为 Task 运行、(c) 先批准模板骨架、(d) 第一份规格由用户自己写；附 Q1(b) 原文与对照），所有者选 **(a)**：首次批准之前的起草作为 spec_id 为空的 Task 运行，经一个受守护的 SQL 迁移，只有起草 Task 可以没有规格；Plan 对话，由 Agent 起草规格；草稿只经用户的 ApproveSpec 成为修订 1；Q1(a) 与 `-p` 停在 no_spec 不变。本 CR 的第 2 节、迁移 002、REQ-008 行为 2–4 与 AC-008-02、-05 按此写成。

CTO 认为在 CR-003 D1 之内、未另请所有者确认的一项：新增受锁契约 `contracts/spec-review-report.schema.json`（第 5 节；只增不改，不放宽任何检查，使 docs/06 的独立规格审查有合法的报告路径）。

## 不变的部分

- 16 项需求的 id、category、priority、milestone、title、summary、required_release、release_class（required）与 release_targets（v1）都不变；没有删除任何需求或 AC。
- 每项原有的 AC-xxx-01 保留 id，改写为可判定的内容。原 AC 的 then 照抄 summary；除下表点名由既有 AC 承接的条款外，summary 的每一项仍由该需求自己的 AC 判定，没有被缩小：

| 需求 | summary 的各项 → 判定它的 AC |
|---|---|
| REQ-007 | 两种入口得到同一 SetMode 行为 → 007-01；Shell 不需要对称子命令 → 由既有 AC 承接（REQ-001：`hoh plan`、`hoh mode` 以 64 退出，M1-T06） |
| REQ-008 | 版本化规格 → 008-02；允许提问 → 008-03；未批准实施不修改产品源码 → 008-01、008-04、008-05 |
| REQ-009 | Build 遵守既定规格 → 009-01；Auto 可补规划 → 009-02；缺必需规格均不越过 readiness → 009-02、009-03 |
| REQ-017 | Ask 不改任务 → 017-01；steer 有生效边界 → 017-02；CR 改规格、控制可独立于模型执行 → 017-03 |
| REQ-023 | 不绕过全局任务账本 → 023-01；重大决定升级 → 023-04；讨论有轮数上限（含可调）→ 023-03（环检测 023-02） |
| REQ-028 | 建立基线 → 028-01、028-02、028-04；保留脏文件 → 028-01、028-03；不重写架构 → 028-03（含 U 的计划整体被拒）与 028-01；不强制初始化远程 → 028-04 |
| REQ-029 | 十类规范齐全 → 029-01（逐类点名）、029-03（产品入口写出十类）；按规模选用 → 029-02（离线 029-03） |
| REQ-030 | 记录五类 → 030-01；不伪造用户答案 → 030-02、030-03 |
| REQ-031 | AC 可观察、依赖可达、关键决定确定 → 031-01；仅阻断受影响范围 → 031-02；（独立规格审查 031-04、031-05） |
| REQ-032 | 双向查找 → 032-01、032-02（修订后全部 stale 032-03；没有 contracts/ 的项目 032-04） |
| REQ-033 | CR、revision、影响闭包 → 033-01；受影响验收过期、不沿用旧 PASS → 033-02、033-03 |
| REQ-034 | 文件为事实源、DB 为索引 → 034-01；冲突可检测 → 034-02；生成页不是第二权威源 → 034-04 |
| REQ-040 | 按指纹与状态变化、不按 commit 计数 → 040-01、040-02；升级、研究、改计划 → 040-04 |
| REQ-045 | 记录版本与 hash → 045-01；高层覆盖不改证据 → 045-02；过长报错 → 045-04（缺变量 045-03） |
| REQ-046 | 无需手装、只加载相关 → 046-01；原生不可用时注入 → 046-02 |
| REQ-047 | 按序覆盖 → 047-01；显示 diff → 047-02；运行锁定旧版本直到 checkpoint → 047-03 |

- 十六个 TEST-xxx-01 中有十四个的 check 不变。改了的两个都是换到更完整的检查，不是放宽：TEST-045-01 与 TEST-040-01 由 unit 改为 integration（它们现在要经 host 记录与重放 Prompt、要重启 host），这两项需求的其余 AC 仍有 unit 检查。integration 与 unit 同在 v1 与 v0.1 档的必跑检查中。
- 不改：CONTRACT、`docs/10-acceptance.md`（第 17 行要求观察绑定当前 spec，与 O6 一致）、`docs/03-ux.md` 与 `docs/06-orchestration.md`（它们的可配置承诺由新键兑现）、`contracts/release-policy.json`、`tools/release_gate.py` 与其余门禁工具、`contracts/openapi.json`、`contracts/sql/001_initial.sql`（只新增迁移 002）、`contracts/candidate.schema.json`、`contracts/independent-report.schema.json`、各 schema 中除三个原因码枚举之外的部分、`config/hoh.example.toml`。没有门禁阈值、检查或证据规则被放宽；O6 使证据规则更严；独立规格审查成为 readiness 的输入（加严）。
- 非 M3 的需求只有 REQ-018 增加三条决定（三个配置键），它的行为与 AC 不变；`project.document_profile` 的缺省仍为 full。

## 是否属于 CR-003 D1

1. 把 16 项 outline 细化为 detailed，补齐 8 个原因码与 REQ-018 键表中缺的三项，新增独立规格审查的报告契约（第 1 条；新契约只增不改）。
2. 不删需求或 AC；required、release_class、release_targets 不变；docs/03、docs/06 承诺的可配置性保留（第 2 条）。
3. 不放宽任何门禁、检查或证据规则；两个 TEST 换到更完整的检查；O6 加严；独立规格审查成为 readiness 的输入（加严）（第 3 条）。
4. C22 不在 D1 之内（(a) 改受锁 SQL 的语义：没有规格的 Task），已由所有者于 2026-11-17 决定取 (a)；迁移 002 是这一决定的执行。其余部分不改 CONTRACT 的架构与交互决定：/plan、Shift+Tab、Plan/Build/Auto、`-p` 与 attach 照 CONTRACT 第 3、4 条；规格批准权（O1）与证据失效（O6）是所有者的决定。CR-024（Commander 只读，所有者 2026-11-17 批准）已改 CONTRACT 决定 7，本 CR 与之一致：起草由 cto 职位的写 Task 承担，Commander 不写文件。

故本 CR 在设计审查 PASS 后可由 CTO 依 D1 批准，C22 与迁移 002 以所有者 2026-11-17 的决定为据。

## 验证方式

本稿的片段由一份数据源生成，并在仓库之外的副本上机械验证（不改工作树的受锁文件）：

1. `git archive` 取本稿所在提交的树到副本，按本稿应用：替换 16 项需求与它们的场景，追加 REQ-018 的三条决定与 8 个原因码，写入 `contracts/spec-review-report.schema.json` 与 `contracts/sql/002_spec_drafting.sql`，在 persistence 白名单中加入迁移 002，替换 M3 的任务并改 M3-T05 的依赖；`config/hoh.example.toml` 不动。
2. 在副本中运行 `python3 tools/gen_spec_views.py`、以本 CR（副本中标为 APPROVED）运行 `python3 tools/baseline.py relock --add contracts/spec-review-report.schema.json --add contracts/sql/002_spec_drafting.sql`，再运行 `python3 tools/validate_bundle.py` 与 `bash scripts/check.sh --bundle`。
3. 生成器另核对第 8 节表中点名的每个 M3 任务都是该 AC 所属任务本身或其祖先。
4. 在同一副本上运行 TEST-018-01（`cargo test -p hoh-core --locked test_018_01`，bundle 自检不运行它），与 persistence_scan。
5. 迁移 002 的探针（第 2 节）：在有数据的 v1 库上按指定的运行器执行，并对照今天的运行器。

结果（本稿提交前的最后一次运行）：

- `python3 tools/gen_spec_views.py`：退出码 0
- `python3 tools/baseline.py relock --cr docs/cr/CR-026-m3-detailed-spec.md --add contracts/spec-review-report.schema.json --add contracts/sql/002_spec_drafting.sql`：退出码 0
- `python3 tools/validate_bundle.py`：退出码 0
- validate_bundle：status=PASS，errors=0；counts：requirements=70、detailed_requirements=43（原 27）、acceptance=212（原 167）、development_tasks=64（原 59）
- 在同一副本上运行 `bash scripts/check.sh --bundle`：退出码 0；Ran 502 tests；OK (skipped=2)
- 第 8 节的闭包核对：0 处不符
- `cargo test -p hoh-core --locked test_018_01`（副本中 config/hoh.example.toml 未改）：退出码 0；test config::tests::test_018_01_no_config_and_the_example_give_the_same_effective_config ... ok；test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 809 filtered out
- `python3 scripts/product/persistence_scan.py --root .`（副本中白名单含迁移 002 的条目）：退出码 0；去掉该条目时同一扫描为 FAIL（unlisted-file：contracts/sql/002_spec_drafting.sql）
- 迁移 002 的探针（`migration_probe.py`，SQLite 3.46.1）：退出码 0；今天的运行器：failed and rolled back: FOREIGN KEY constraint failed（user_version 仍为 1）；指定的运行器：user_version 2、各表行与值不变 True、integrity ok、foreign_key_check 0 行、索引 tasks_retry、tasks_run_state、触发器 tasks_spec_draft_insert、tasks_spec_draft_update；守护：work_task_without_spec refused；draft_without_spec_project_has_revision refused；draft_reader refused；draft_without_spec_project_without_revision accepted；attempt_of_draft_task accepted；writer_lease_of_draft_attempt accepted；effect_of_draft_attempt accepted；draft_null_to_spec refused；draft_kind_change refused；work_spec_to_null refused；work_kind_change refused；work_spec_to_newer_revision accepted；draft_with_spec accepted；fk_still_enforced refused


批准时的步骤（M3-T01）：按 docs/21 的“基线变更的机械步骤”在工作树中应用同样的片段（含 persistence 白名单的条目），`gen_spec_views`、`baseline.py relock --cr docs/cr/CR-026-m3-detailed-spec.md` 带上述两个 `--add`、`bash scripts/check.sh --bundle`、TEST-018-01，提交。

## 审查记录

设计审查第 1 轮，在 `ef0b174`（第 1 稿）：Codex 判 FAIL（CR26D-01..06 high，-07..-11 medium），Grok 判 FAIL（CR26D-50 high，-51..-56 medium，-57、-58 low）。文本：`.hoh-dev/evidence/m3-prep/cr026/review-cr026-d1-codex.md`、`review-cr026-d1-grok.md`。第 2 稿的处置：

| ID | 级别 | 摘要 | 处置 |
|---|---|---|---|
| CR26D-01、-50 | high | C22 的零摘要锚没有闭合的起草路径，并且会被采用为规格，改写 Q1(a) 与 M2T13D-59 | 锚撤回；第 2 稿推荐 (c)；第 3 稿：所有者 2026-11-17 选 (a)，第 2 节给出端到端设计（见第 2 轮） |
| CR26D-02 | high | 多文件规格没有冻结形式 | 冻结形式为生成并提交的 index.json，规格摘要为它的 sha256，candidate.spec 为单个 FileRef（共同约定、REQ-034 行为 2、5）；AC-034-01 观察冻结，AC-033-03 观察一个字符的变化改变摘要并使全部证据 STALE，AC-034-04 观察 index.json 与 Markdown 不一致时拒绝冻结 |
| CR26D-03 | high | AC-033-02 在验证进程仍运行时关轮 | 分为批准事务内的失效与取消请求、进程结束证明之后恰一次 superseded 关轮；场景改为 recovery：运行中的 run-check、取消后的迟到报告、批准后崩溃再恢复 |
| CR26D-04 | high | M3-T07 缺 M3-T06 的路径范围 | M3-T07 依赖 M3-T06；第 8 节按每条 AC 实际用到的能力重列提供者，生成器核对它们都是祖先 |
| CR26D-05 | high | 缺独立规格审查 | REQ-031 行为 3、AC-031-04、AC-031-05；由 M3-T06 承担；用户批准不替代它；报告路径见第 2 轮 CR26D-101 |
| CR26D-06、-51 | high、medium | O1 没有反例 | AC-008-05 与 AC-009-02；第 3 稿按 CR26D-105、-152 补祖先与 cgroup |
| CR26D-07 | medium | 待答问题期间的生命周期 | REQ-008 行为 7 与 AC-008-03：BLOCKED(NEED_DECISION)、resume_state=SPECIFYING，答复后回到 SPECIFYING |
| CR26D-08 | medium | AC-017-05 的应用次数自相矛盾 | 按 message_id 逐条：M1 应用 1；M2 传输 2、应用 1；M3 应用 0、拒绝 1、无死信 |
| CR26D-09 | medium | 缺省档与 REQ-018 的 full 冲突 | 缺省仍为 full，记录档与来源（C16，AC-029-02） |
| CR26D-10、-56 | medium | 可配置承诺被移出 | `[supervision] max_discussion_rounds` 与 `[ui] mode_cycle` 进入 REQ-018 键表；AC-023-03、AC-007-01 覆盖非缺省值；进入 example 的时间见 CR26D-103 |
| CR26D-11 | medium | Plan 写集没有负例 | REQ-008 行为 5 与 AC-008-04 |
| CR26D-52 | medium | [assumptions] 可为三类保留决定给默认 | 三个保留名出现即配置被拒；第 3 稿 AC-030-02 三个名字各一个项目 |
| CR26D-53 | medium | AC-029-01 没点名十类 | AC-029-01 与 TEST-029-01 逐类点名；第 3 稿 AC-029-03 也点名 |
| CR26D-54 | medium | “不重写架构”与首提交路径无边界 | AC-028-03、AC-028-04 |
| CR26D-55 | medium | AC-040-04 的建议未限定 | 摘要须列出升级、研究、改计划三项，并分别观察改计划与研究两条路径 |
| CR26D-57 | low | REQ-032 的 stale 只限受影响链 | REQ-032 行为 5 与 AC-032-03 改为全部 stale |
| CR26D-58 | low | “不变”一节的措辞 | 改为“除下表点名由既有 AC 承接的条款外” |

设计审查第 2 轮，在 `dbc76fe`（第 2 稿）：Codex 判 FAIL（CR26D-100..103 high，-104..106 medium；第 1 轮 9 项闭合，CR26D-01、-05 仍开放），Grok 判 FAIL（CR26D-150、-151 high，-152..154 medium）。文本：`.hoh-dev/evidence/m3-prep/cr026/review-cr026-d2-codex.md`、`review-cr026-d2-grok.md`；Codex 的探针在 `.hoh-dev/evidence/m3-prep/cr026/review-d2-codex/`。第 3 稿的处置：

| ID | 级别 | 摘要 | 处置 |
|---|---|---|---|
| CR26D-150 | high | (c) 改 CONTRACT 第 4 条的交互，是所有者问题 | C22 交所有者决定（问题附 Q1(b) 原文与对照）；所有者 2026-11-17 选 (a)，(c)、(d) 记为未采用；“所有者的决定”一节与 D1 一节第 4 条照录 |
| CR26D-100 | high | (c) 的批准前写入没有租约与效果台账的归属，崩溃、重复答复、跨 Run 互斥未定义 | (a) 下起草是 Task：写者租约（跨 Run 串行）、git init 与模板实例化记为 Attempt 的 effect、取消与 reconcile 同其他写 Task，`spec.drafting` 按 run 去重；AC-008-02 在起草 Attempt 中 kill -9 并断言不重复建 Task、不重复 effect；(c) 未采用 |
| CR26D-151 | high | draft 前后的 Run 状态，DISCOVERING 与基线 | 第 2 节第 1–5 步：draft 之前 BLOCKED(no_spec)；draft 之后 Runtime 推进 CREATED→DISCOVERING（run.baseline 先于任何写入，REQ-028 行为 1，C34）→SPECIFYING（spec_id 空）；批准问题期间 BLOCKED、resume_state=SPECIFYING；批准后采用并回到 SPECIFYING。AC-008-02 观察三段，AC-028-04 观察基线先于 Attempt 与 effect |
| CR26D-101 | high | 独立规格审查没有合法的报告路径 | 新受锁契约 spec-review-report.schema.json（第 5 节）；事件 spec_review.opened / reported；不进 independent_reports、不开验证轮次、不造候选（REQ-031 行为 3、决定 3，C30）；AC-031-04 观察收取，且 independent_reports 与 candidates 无新行 |
| CR26D-102 | high | 追踪的 TEST→check 输入 | index.json 带 scenarios（REQ-034 行为 2、决定 3）；acceptance_criteria.check_id 与追踪都从它读，contracts/acceptance-scenarios.json 只用于没有 scenarios 键的 spec 文件（REQ-032 行为 6、失败模式 3、决定 3，C31）；新 AC-032-04：没有 contracts/ 目录的项目批准→冻结→追踪；归属 M3-T07（读取）与 M3-T02（索引） |
| CR26D-103 | high | 新配置键先于加载器进入 example，TEST-018-01 失败 | 本 CR 不改 example；三键的加载器支持与 example 在 M3-T06、M3-T09、M3-T10 各自同一提交加入并重跑 TEST-018-01（第 4 节、三个任务的 done_when、REQ-018 的三条决定，C32）；应用本稿的副本上 TEST-018-01 通过（“验证方式”） |
| CR26D-104 | medium | AC-028-03 要求同一计划部分导入 | 先提交含 U 的 P1：整体被拒、零导入；再提交只含 T 的 P2 测脏文件保护 |
| CR26D-105、-152 | medium | O1 只测了直接带标记的进程 | REQ-008 行为 1：祖先标记、cgroup、对端无法确认（origin_unknown，C35）都拒，Agent 工具调用里的 shell 按后代拒；AC-008-05 加 Q（只有祖先带标记）、Z（只在 Attempt cgroup 中）、P 的 AnswerQuestion，以及不属于任何 Attempt 的用户客户端正例；origin_unknown 由 M3-T04 的单元测试覆盖（done_when） |
| CR26D-106 | medium | 离线实例化的 strace 没有跟踪 host | AC-029-03：私有 HOME/XDG、没有既有 host；以 strace -f -p 附着执行实例化的 host（含子进程），客户端也在 strace -f 下 |
| CR26D-153 | medium | AC-030-02 没有已批准修订，B 只钉一个保留名 | 项目 A 有已批准修订 1，产品方向的 record_assumption 被拒；B、C、D 各列一个保留名，都被拒 |
| CR26D-154 | medium | 产品入口不写十类 | REQ-029 行为 7：首个起草 Attempt 开始前按有效档实例化 profiles.json 所列全部模板（full 的十类逐一点名）；AC-029-03 点名十类 |

## 附录 A：准备清单的复核（按 main 715e2d0）

准备清单写于 16ff3e1。其后合入的是 retry-after（PR #57、#58）与 CR-025 / M2-T21；M2-T13、M2-T15 在 16ff3e1 之前已合入；CR-024 已由所有者批准，但尚未合入 main（分支 task/commander-readonly）。复核方法：从清单中取出全部 190 处 `path:line` 引用，用 `git diff -U0 16ff3e1 715e2d0` 的块把旧行号映射到新行号，并逐一比较两边的行文本；含糊的文件名按上下文定到唯一文件。

**行号**：190 处引用中，161 处在 715e2d0 上行号与文本都不变；13 处文件名含糊（只写了 `supervise.rs`、`control.rs`、`tui.rs` 之类），按上下文定为唯一文件后，除下表最后一行外文本与行号都不变；下列引用的文本不变、行号移动，以新行号为准：

| 清单中的引用 | 715e2d0 上的位置 | 该行 |
|---|---|---|
| `app_server.rs:292` | `crates/hoh-agents/src/codex/app_server.rs:380` | fn send_steer(&mut self, s: Steer) { |
| `crates/hoh-core/src/decision.rs:412` | `crates/hoh-core/src/decision.rs:459` | SpecMismatch, |
| `decide.rs:922` | `crates/hoh-core/src/persistence/decide.rs:938` | pub(super) fn requirements_passed( |
| `decision.rs:462` | `crates/hoh-core/src/decision.rs:509` | DecisionPayload::CreateTasks { .. } => Ok(Effect::CreateTask |
| `decision.rs:653` | `crates/hoh-core/src/decision.rs:726` | fn transition_run( |
| `docs/adr/ADR-022.md:85` | `docs/adr/ADR-022.md:87` | - `project.document_profile`：M3-T03（REQ-029）； |
| `docs/adr/ADR-030.md:553` | `docs/adr/ADR-030.md:555` | - 只是可供读取、不构成义务的：TUI（M3）显示团队视图的 `missing` 与 `pending.cause`；C |
| `host/product.rs:424` | `crates/hoh-app/src/host/product.rs:425` | fn pack( |
| `host/product.rs:509-513` | `crates/hoh-app/src/host/product.rs:510-514` | ) |
| `host/runner.rs:1071` | `crates/hoh-app/src/host/runner.rs:1075` | held.item(store.detect_stall(run, pending.clone(), at()).awa |
| `host/runner.rs:1216` | `crates/hoh-app/src/host/runner.rs:1220` | // A writer's workspace at its start: its HEAD, the base of  |
| `persistence/retry.rs:949` | `crates/hoh-core/src/persistence/retry.rs:1014` | pub const REVERIFY_LIMIT: u32 = 2; |
| `wakes.rs:515` | `crates/hoh-core/src/persistence/wakes.rs:528` | \| "no_progress" |
| `retry.rs:955-956`（§3 REQ-040） | `crates/hoh-core/src/persistence/retry.rs:1020-1021` | Repair::Local { .. } => "repair.local",（含糊的文件名定为 persistence/retry.rs，行已移动） |

含糊文件名的归属（文本与行号不变）：`supervise.rs:24、:28、:913` 为 crates/hoh-core/src/persistence/supervise.rs，`supervise.rs:111` 为 crates/hoh-core/src/supervise.rs；`control.rs:608` 为 crates/hoh-app/src/host/control.rs；`tui.rs:225、:552` 为 crates/hoh-app/src/tui.rs；`cli.rs:206-208` 为 crates/hoh-app/src/cli.rs；`delivery.rs:1、:231` 为 crates/hoh-core/src/delivery.rs；`verification.rs:1004` 为 crates/hoh-core/src/persistence/verification.rs；`completion.rs:806` 为 crates/hoh-core/src/persistence/completion.rs。

**“M2 已建”的判断**（逐条重查，715e2d0）：

- 仍然成立：`spec.changed` 唯一的产生方是采用（persistence/adoption.rs:51）；`candidate.stale` 唯一的产生方是 evidence_unrecoverable；specs 唯一的插入是 doctor 的 scratch_task（persistence/writer.rs:1144）；change_requests、requirements、acceptance_criteria 在产品里没有写者；SetMode、ProposeSpecChange、ApproveSpec 仍回 422 CAPABILITY_UNSUPPORTED（host/http.rs:857-864）；`Dispatcher` 仍只在 hoh-core 的测试中构造；`no_progress`、`milestone_ready`、`release_request` 仍没有产生方；Codex 的 runner 模式仍关闭 turn 内 steer（codex.rs:854）。
- 已变：`budget.risk` 现在有产生方（M2-T21，persistence/retry.rs:933，CR-025）；准备清单 §2 的 ADR-025 决定 1 一行据此只剩 `no_progress`、`milestone_ready`、`release_request` 与用户输入交给 M3：本 CR 把前三者的产生方写进 M3-T10 的 done_when（后两者没有 M3 的 AC），把用户输入交给 Commander 写进 M3-T09 的 done_when。
- 已变：§2 的 M2T17R-352 一行写“包 H / CR-025（修订中）”，CR-025 已 APPLIED（relock 第 18 版）：交接期间派发的“稍后再派”原因是 `ROLE_BINDING_PENDING`；M3-T09 的投递与它一致。§1.4 的 ErrorCode 清单不变，RejectionReason 多了 ROLE_BINDING_PENDING。
- 已变：CR-024（Commander 只读）已由所有者于 2026-11-17 批准，尚未合入 main。准备清单风险 4 写“Commander 只读（CR-024，审查中）”，现应读为已批准：Plan 的规格由 cto 职位的写 Task 起草（REQ-008 行为 1），Commander 不写文件。
- §2 中“ADR-024 决定 4；M2T17R-153、-154、-254”与“ADR-031 移交 :497、:498”的内容在 715e2d0 上不变。


