# CR-043 · 决策依据与影响的生产者（C2）

ID：CR-043
状态：DRAFT
批准人：待独立 Review 与 CTO 按 CR-003 D1 审批
日期：2026-11-28
关联：CR-028 的 C2（§2“决策依据与影响”、D 表 C2 行、M4A-503、M4-T02 done_when 的 C2 句），REQ-010 AC-010-01 / TEST-010-01，CTO 决定 Q8（`.hoh-dev/evidence/m4-t02-prep/cto-decisions-2026-11-27.md`），M4-T02 增量清单（`inventory-2026-11-27.md` 第 101、118、184、284、304 行）；REQ-035、ADR-011 §3、ADR-031 决定 2、6、7

## 背景

C2：每项 decision 的依据来自已公开的 basis/reason/note 字段，影响来自已提交 effects，两者各带可追溯来源；任一缺失显示 unknown，unknown 不计 PASS。M4-T02 须逐类核对决策的生产者，缺依据或缺影响生产者的类别经 refinement CR 加法增加可选协议字段与真实生产接线，fixture 不捏造。Q8 定为先出六类对照表，再补缺。本 CR 是这一步：第一节是对照表，第二节是补齐的决定，第三节是受锁条款前后对照，第四节写明 M4-T02 S5 仍负责的部分。

## 一、六类决策的依据与影响（代码事实，基线 main `50d8fd60`）

**客户端读得到什么。** `GET /v1/events`（`crates/hoh-app/src/host/http.rs:95`）把每条已提交事件连同完整 payload 按 seq 发出（`event_json`，http.rs:1660-1671；只按 run_id/kinds 过滤）；`GET /v1/runs/{run_id}`（http.rs:1545）给投影，其中 `projection.questions` 是待答问题（http.rs:1614）。CR-028 §4 D3 的分类：`decision.applied`、`decision.rejected` 为 P,I（CR-028:386），`plan.imported` 为 I（CR-028:387），`question.asked`、`question.answered` 为 I（CR-028:393），`need_decision` 为 I（CR-028:405），`run.blocked`、`run.blocker_claimed`、`run.blocker_released` 为 P（CR-028:385）。每条事件的来源 id 是它的 seq 与 event_id；决策事件另名出 `decision_id`（decide 路径为消息 id，`crates/hoh-core/src/decision.rs:69`；Commander 输出路径为 `<attempt_id>/<message_id>`，`crates/hoh-core/src/persistence/outputs.rs:557`）。

**持久化。** 每条过了 schema 的决策，其 `protocol::encode` 后的 DecisionPayload 进 `decisions.payload_json`（`crates/hoh-core/src/persistence/decide.rs:179-180`、`:219-230`）；这是内部表，客户端不读。`decision.applied` 的 payload 原样带这份 DecisionPayload（键 `payload`，decide.rs:524-528，事件 decide.rs:608-616，event_id `<decision_id>/applied`，decide.rs:612），所以“依据字段公开”就是它在 `decision.applied.payload.payload.<字段>` 里。

状态记号：**有**＝字段与公开位置都在；**缺**＝没有字段；**有但不公开**＝已持久化，但客户端读的事件里没有这个键；**有但不可关联**＝已提交，但没有指向该决策的 id。

| 类别 | 依据：字段（schema / Rust） | 依据：公开位置 | 依据 | 影响：应用时已提交的事实 | 影响：客户端如何关联 | 影响 |
|---|---|---|---|---|---|---|
| dispatch | `note`，可选，无长度上限（`contracts/protocol.schema.json:1139`；`crates/hoh-core/src/protocol.rs:542-547`） | `decision.applied.payload.payload.note`（decide.rs:524-528） | 有（Commander 不写即缺，显示 unknown） | 同一事件的 `task{task_id, from: READY, to: LEASED}`、`attempt_id`、`lease{kind, epoch, expires_at}`（decide.rs:546-548）；Task 的 CAS（decide.rs:567）；PREPARED Attempt 行与租约（`lease_task`，decide.rs:297、:644）；派发消息进 outbox（decide.rs:549，不是事件） | 同一条 `decision.applied`（decision_id）；之后的 Attempt 事件按 `attempt_id` | 有 |
| transition_task | `reason`，可选，无上限（schema:1173；protocol.rs:548-553） | `decision.applied.payload.payload.reason` | 有（可缺） | `task{task_id, from, to}`（decide.rs:576）与 Task 的 CAS | 同一条 `decision.applied` | 有 |
| transition_run | `reason`，可选，无上限（schema:1215；protocol.rs:554-560） | `decision.applied.payload.payload.reason` | 有（可缺） | `run{from, to}`（decide.rs:583）与 Run 的迁移（decide.rs:602）；到 RUNNING 另有 outbox 的 run.started 通知（decide.rs:587，不是事件）；进入停留态/终态的提交以 `run.result` 结尾（ADR-031 决定 3，decide/tests.rs:1347 的用例） | 同一条 `decision.applied`；`run.result` 在同一提交 | 有 |
| request_user_decision | **没有依据字段**：只有 question_id、question、choices、requires_user（schema:1220-1267；protocol.rs:561-567）；`question` 是问题原文，不是依据 | — | **缺** | 提问（`person`，outputs.rs:603-662）：`run.blocker_claimed{NEED_DECISION, source: commander:<decision_id>}`（outputs.rs:614-622；`crates/hoh-core/src/persistence/blockers.rs:21`、`:236`），`question.asked{question_id: <decision_id>, commander_question_id, question, choices, requires_user}`（outputs.rs:627-642），`blockers::follow` 的 `run.blocked`（outputs.rs:644；blockers.rs:363），`supervisor.output_disposed{unsupported, decision_id}`（outputs.rs:658）；决策行留 PROPOSED（decide.rs:288）。答复（`crates/hoh-core/src/persistence/questions.rs:755-797`）：`question.answered{question_id, choice_id, answer, exit}`（questions.rs:725-730），`decision.applied{decision_id, kind, answered_by}`（questions.rs:777-782，**不带 payload**），`need_decision{answered, decision_id}`（questions.rs:783），释放声明后 `run.blocker_released` 与 `run.resumed`（questions.rs:794） | `question_id` = decision_id；声明来源 `commander:<decision_id>`；`need_decision.decision_id`；`decision.applied.decision_id` | 有 |
| create_tasks | **没有依据字段**：只有 kind、plan（schema:1268-1282）；`PlanProposal` 是 `additionalProperties: false`（schema:1306 起） | — | **缺** | `decision.applied` 只有 Run 的 Touch，无效果键（decide.rs:604）；同一事务紧随其后的 `plan.imported{run_id, tasks[{task_id, title, state, layer, created_seq, paths}]}`（decide.rs:340；`crates/hoh-core/src/persistence/plan.rs:311-312`）及 `spec::replace_after_import` 的事件（plan.rs:316） | `plan.imported` **不带 decision_id**（plan.rs:312），只能靠同一事务的相邻 seq 或 task_id 与提议重合去猜 | **有但不可关联** |
| record_assumption | `basis`，必填，minLength 1（schema:1298；protocol.rs:571-574）；无人值守时须为 `assumptions.<已列出的键>`（decision.rs:625-646） | `decision.applied.payload.payload.basis` | 有 | 只有它自己：`Effect::RecordAssumption` 是“只有事件与 Run revision”（decision.rs:460-461；ADR-011 §3 第 5 条），没有 Task/Run/计划效果；这条 `decision.applied` 的 event_id 是规格条目可引用的来源（`crates/hoh-core/src/persistence/spec_sources.rs:38`） | 事件本身 | 不需要新生产者：它按设计只记录（见第四节对显示的说明） |
| （任一类）被拒 | 提议内容与依据在 `decisions.payload_json` 与原始输出行 `supervisor.output.text`（outputs.rs:66）里；`decision.rejected`（decide.rs:709-744）只有 decision_id、kind、reason_code、detail、栅栏与 inputs，**不带提议** | 只在原始输出行的 JSON 文本里，不是公开字段 | **有但不公开** | 按设计没有效果：只写 decisions 行 REJECTED 与恰好一条 `decision.rejected`，不改 tasks/runs/outbox/attempts（ADR-011 §3 首段，`docs/adr/ADR-011.md:95`） | `decision.rejected.decision_id`、`output_id` | 不需要生产者：结果就是“未应用”加 reason_code |

结论：依据缺两类（request_user_decision、create_tasks），依据有但不公开一处（被拒的提议），影响有但不可关联一处（create_tasks 的 plan.imported）；另一处答复的 `decision.applied` 不带 payload，使 request_user_decision 应用后的读法与其余五类不同。

## 二、决定

本节的行号同第一节，指基线 `50d8fd60`；本分支的接线位置列在本节末尾。

1. **两个可选依据字段（协议，加法）。** `$defs.DecisionPayload` 的 `request_user_decision` 与 `create_tasks` 分支各增加可选 `basis`：字符串，1–4096 个字符（schema 的 minLength/maxLength 按 Unicode 字符计，`crates/hoh-core/src/schema.rs:390`）。含义：该决策公开的依据（证据路径与 sha256、取舍、影响范围的简述），不是隐藏推理，不是用户批准。必填字段不变，其余字段含义不变；没有 `basis` 的旧记录与旧输出照旧有效，读者显示 unknown。上限取 4096：UTF-8 至多 16 KiB，与 CR-028 §2.2 单公开事件正文 16 KiB 的内联界一致，与 CR-033 `paths` 同界。record_assumption 的 `basis` 及其无人值守校验不变，新字段不做 `[assumptions]` 校验。既有的 `note`/`reason`/`question` 不加上限：给它们加上限会让旧的有效消息变无效，不是加法；它们与新字段一样受一轮决策块 1 MiB/1000 行的总界（`crates/hoh-core/src/supervise.rs:225-226`）。
2. **Runtime 校验。** 校验就是既有的 schema 校验：Commander 输出进 `Proposal::from_message`（decision.rs:59-79），decide 入口再 `protocol::encode` 校验一次（decide.rs:179）。`basis` 为空、超过 4096 字符或不是字符串时整条输出处置为 `supervisor.output_disposed{invalid, PAYLOAD}`，不写 decisions 行、问题或声明，与其他字段的 schema 违例相同。
3. **持久化与公开。** Rust 镜像 `DecisionPayload::RequestUserDecision` 与 `CreateTasks` 增加 `basis: Option<String>`（缺省不序列化），随 `decisions.payload_json` 持久化。create_tasks 的依据经既有的 `decision.applied.payload.payload.basis` 公开。request_user_decision 的依据在提问时写进 `question.asked` 的 `basis` 键（只在有时写；与问题原文同一事件，客户端在读问题的地方读它）；答复时写的 `decision.applied` 增加 `payload`（该请求决策行里的 DecisionPayload），使六类决策应用后的依据都在 `decision.applied.payload.payload` 的同一位置。`projection.questions` 不变。
4. **被拒决策公开它的提议。** decide 写的 `decision.rejected` 增加 `payload`：Runtime 已校验的 DecisionPayload，与 `decision.applied` 的 `payload` 同一取值。被拒提议的内容与依据（note/reason/basis）因此可读；既有键与取值不变。Commander 旧 epoch 的非决策消息（`commander_message` 的 STALE_ROLE_EPOCH，decide.rs:1248 起）不是 DecisionPayload，不在此列，不变。
5. **create_tasks 的影响可关联。** 决策导入的 `plan.imported` 增加 `decision_id`（导入它的决策）。不经决策的 `Store::import_plan`（plan.rs:62）不写这个键。其余四类的影响本来就在同一条 `decision.applied` 里，或已由 decision_id 关联（request_user_decision 的 question_id、声明来源、need_decision.decision_id），不加字段。
6. **Commander 的产品指引。** `resources/prompts/commander.md` 的工作步骤 5 写明：依据只写进该决策的公开字段——dispatch的note、transition_task与transition_run的reason、record_assumption的basis、create_tasks与request_user_decision的可选basis（1–4096字符）；只写公开依据与证据引用，不写隐藏推理；不写即客户端显示 unknown。Commander 每个 turn 拿到的 DecisionPayload schema（`crates/hoh-core/src/context.rs:399` 的 output_schemas）来自同一份受锁 schema，所以字段与上限同时对它可见。
7. **不捏造。** 测试只经产品生产者（decide、Commander 输出路径、答复命令）产生这些字段；没有任何夹具写入依据或影响。旧记录（无 `basis`、无 `decision_id`、无 `payload`）不迁移、不补值。

接线位置（本分支）：schema `contracts/protocol.schema.json:1266`（request_user_decision.basis）、`:1287`（create_tasks.basis）；Rust 镜像 `crates/hoh-core/src/protocol.rs:569`、`:575`；`question.asked.basis` `crates/hoh-core/src/persistence/outputs.rs:638`；答复的 `decision.applied.payload` `crates/hoh-core/src/persistence/questions.rs:784`；`decision.rejected.payload` `crates/hoh-core/src/persistence/decide.rs:729`；`plan.imported.decision_id` `crates/hoh-core/src/persistence/decide.rs:341` → `crates/hoh-core/src/persistence/plan.rs:309`（`Store::import_plan` 传 None，plan.rs:62）；Commander 契约 `resources/prompts/commander.md:39`。

## 三、受锁条款（前 → 后）

只改 `contracts/protocol.schema.json`，两处加法：

1. `$defs.DecisionPayload.oneOf[3]`（`request_user_decision`）的 `properties`：
   - 前：`kind`、`question_id`、`question`、`choices`、`requires_user`，`required` = `[kind, question_id, question, requires_user]`。
   - 后：另有 `basis: {"description": "CR-043: …", "type": "string", "minLength": 1, "maxLength": 4096}`；`required` 不变，`additionalProperties: false` 不变。
2. `$defs.DecisionPayload.oneOf[4]`（`create_tasks`）的 `properties`：
   - 前：`kind`、`plan`，`required` = `[kind, plan]`。
   - 后：另有同形的 `basis`（description 写明它是该计划决策的依据，不是 PlanProposal 的字段）；`required` 不变，`additionalProperties: false` 不变；`$defs.PlanProposal` 不变。

需求、AC、场景、检查计划、发布策略、门禁、SQL 迁移、OpenAPI 都不变；AC 数不变。`requirements.json` 不改：REQ-010 的 C2 behavior 已写明本义务，REQ-035 决定 4 列出的六类不变。事件 payload 是协议的自由对象，`question.asked`、`decision.applied`、`decision.rejected`、`plan.imported` 增加的键不进受锁文件，记入 ADR-011 与 ADR-031 的更正说明。

## 四、M4-T02 S5 仍负责

- **渲染。** Workspace 的决策行按第一节逐类取依据：dispatch→`note`，transition_task/transition_run→`reason`，record_assumption→`basis`，create_tasks→`basis`（均在 `decision.applied.payload.payload` 或 `decision.rejected.payload.payload`），request_user_decision→`question.asked.basis`（应用后另在 `decision.applied.payload.payload.basis`）；影响取第一节“影响”列的事件与键，带各自 seq/event_id 作来源；被拒决策显示 reason_code。不把提议当已发生效果（被拒的 `payload` 是提议，不是影响）。
- **unknown 的显示。** 键不存在（Commander 没写可选依据；本 CR 之前的旧记录没有 `basis`、`decision_id`、`payload`）即 unknown，不计 PASS，不按相邻 seq 或 task_id 重合去补关联，不写占位依据或“无影响”之类的推断。record_assumption 没有 Task/Run/计划效果、被拒决策没有效果：S5 按 C2 定它们的影响列显示（显示该事实本身，或 unknown），但不得算作“带 impact”的正例。
- **TEST-010-01 的否定例。** “缺 basis/reason/note 的决策”与“缺已提交 effects 的决策”各一，经真实生产者产生并保留来源：前者可用不带可选依据的 dispatch 或 create_tasks；后者从真实生产者给出的事实里选（例如尚未答复的 request_user_decision——决策行 PROPOSED，没有 `decision.applied`；或 record_assumption 的“只记录”），选择与断言（显示 unknown、不计 PASS、已有项保留真实来源）属 S5 与 TEST-010-01。本 CR 不写 `test_010_01` 测试，不改客户端代码。

## 五、不变的部分

既有六类的必填字段、含义与拒绝码不变；`note`/`reason`/`question` 不加上限；record_assumption 的 basis 校验不变；`decision.applied`/`decision.rejected` 既有键不变，`commander_message` 的拒绝事件不变；`projection.questions`、OpenAPI、SQL 不变。不改 CONTRACT 的架构、角色分工与交互决定（依据是 Commander 已有输出里的可选数据，不是新的用户交互）。不放宽任何门禁、检查或证据规则。

## 六、是否属于 CR-003 D1

是：(1) 补齐缺失的字段（C2 与 M4-T02 done_when 已要求的依据字段与影响关联），修正规格之间的不一致（C2 要求每项决策有公开依据与可追溯影响，而协议对 request_user_decision、create_tasks 没有依据字段，plan.imported 不能关联到决策）；(2) 不删需求或 AC，不改 required/degradable 或 release_targets；(3) 不放宽门禁、检查或证据规则：新字段可选，unknown 不计 PASS 的规则不变，fixture 不捏造；(4) 不改 CONTRACT 的架构与交互决定。

## 七、验证方式

- 先红后绿，测试以所证事实命名（不用 `test_010_01`，它的 then 证据属 M4-T02）：schema 语料新增四例（两例有效、两例无效）；`protocol_roundtrip` 的 `a_decision_basis_is_optional_and_one_to_4096_characters`；context 的 `the_commander_is_shown_the_optional_basis_of_a_request_and_of_a_plan`；decide 的 `a_plans_basis_is_published_with_its_decision_and_its_import_names_the_decision`、`a_rejected_decision_publishes_the_proposal_it_refused_with_its_basis`；Commander 输出路径的 `a_decision_whose_basis_is_out_of_its_bound_is_an_invalid_output_and_records_no_decision`、`a_commanders_request_basis_is_asked_with_its_question_and_published_when_its_answer_applies_it`；既有 `a_commanders_question_blocks_the_run_and_its_answer_wakes_the_next_turn_with_it` 加断言：没写依据时 `question.asked` 没有 `basis` 键。既有用例只有一处随类型改：decide 测试里对 `CreateTasks { plan }` 的模式加 `..`（断言不变）。红/绿记录在 `.hoh-dev/self-tests/cr043/`。
- 分支：`cargo clippy --workspace --all-targets --locked -- -D warnings`、完整 `cargo test --workspace --locked`（改协议，docs/24 §5）、`bash scripts/check.sh --bundle`（DRAFT 分支的 baseline 不符如实报告，不改基线消除）、前台 `bash scripts/dev/pre-pr.sh`。
- 本分支不 relock，CR 保持 DRAFT；只在 `mktemp -d -p /home/ai/dev` 建的本任务 scratch 副本中临时写 APPROVED、`python3 tools/baseline.py relock --cr CR-043`、`bash scripts/check.sh --bundle`，记录退出码后删除副本（spec 视图不变，`python3 tools/gen_spec_views.py` 只核对无差异）。独立 Review 与批准由指挥安排。

## 审查记录

（待独立 Review。）
