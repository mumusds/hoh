# CR-025 · 预算风险的定义与产生、交接期间的派发拒绝码、门禁拒绝在无 date-time 校验时判定

ID：CR-025
状态：PROPOSED
批准人：待两名独立设计审查判定后，由 CTO 依 CR-003 D1 预授权批准
日期：2026-11-16
基线：改受锁文件 `contracts/requirements.json`（REQ-042：新增行为 9 与决定 6）、`contracts/protocol.schema.json`（`$defs.RejectionReason`）、`tools/release_gate.py`（`validate_manifest`）；`python3 tools/gen_spec_views.py` 重新生成视图；批准后 `python3 tools/baseline.py relock --cr docs/cr/CR-025-budget-risk-role-binding-pending-gate-date-time.md`

本 CR 是 M2-T11 之前的后续项包 H（`.hoh-dev/evidence/followups-before-m2t11-2026-11-16.md` §1a 第 1、2 项，§2 的 M2T10R-750 门禁一半，§6 H）。CR-024（Commander 只读）仍在审查中，与本 CR 无交集，各自重新上锁。

## 背景

### 1. 预算风险没有定义，也没有产生方

REQ-021 行为 1 的重要事件集合含“budget 风险”，Commander 因它被唤醒：hoh-core 的 `supervise::classify` 把 `budget.risk` 判为 High（supervise.rs:111），监督步骤读取它（persistence/supervise.rs:25），唤醒的紧急程度为 2（wakes.rs:516）。但 REQ-042 没有定义什么是“预算风险”，也没有任何代码写 `budget.risk`，所以 REQ-021 的这一项永远不会发生。ADR-025 决定 1 写明这是规格缺口，唯一负责方是这一后续项（-264a、-360b、-458）。

### 2. 交接期间的派发被拒为“缺能力”

职位的新持有者欠着首个输入（交接包，ADR-024 决定 4）时，它的绑定不算 active（`decide::load_binding`，persistence/decide.rs:851-869）。同样不算 active 的还有：团队变更在等它的 checkpoint（ADR-022 决定 8）、staffing 为在途 Attempt 挂起的换绑（ADR-030 决定 5）、尚未授予会话。此时 Commander 派发该职位的另一个任务，被拒为 `ROLE_CAPABILITY_MISSING`，细节为 “role X has no active binding”（decision.rs:550-557）。

而 REQ-035 失败模式 3 把 `ROLE_CAPABILITY_MISSING` 定义为“目标角色缺能力”，并要求“不转派给缺能力 Agent”：这个职位并不缺能力，它的绑定正在按计划变更，变更自行结束。按码行事的 Commander 会把这当作缺能力（转派、改计划、请用户换 Agent），这些都是错的反应。ADR-024 决定 4 写明这一代价，并要 Commander 路径给出“稍后再派”的原因（M2T17R-352）；ADR-025 列为本路径的义务，但没有任何决定履行它。

### 3. 门禁在没有 date-time 校验时照常判定

`tools/release_gate.py` 以 `FormatChecker()` 构造校验器（:15、:35）。jsonschema 没有安装 requirements-dev.txt 的 format 附加包时，`FormatChecker` 不含 date-time 检查，一个 `started_at` 为 `2020-01-01 00:00:00+00:00` 这样非 RFC 3339 的记录会被判为有效（M2T10R-750，reports/reviews/M2.md 第 9 轮）。仿本 `scripts/product/traceability.py` 已在 7a71129 改为没有该检查时拒绝判定（退出 1）；门禁受锁，这一半留给细化 CR。

## 决定

### 1. 预算风险（REQ-042 的细化）

`contracts/requirements.json` 的 REQ-042，`behavior` 在第 8 条之后新增第 9 条：

- 旧：无（REQ-042 有 8 条行为）。
- 新：

  > 预算风险（REQ-021行为1的“budget 风险”）：已设上限的max_iterations、max_duration_hours与可执行的max_usd，其用量按停止判定的同一口径（已用加预留）首次达到上限的80%，或自Run创建起到deadline_at的时长首次过去80%时，Runtime在作出该判定的同一事务内写一条budget.risk{kind, used, limit, unit, threshold}；每个Run每种只写一次，重启后不重复。它只唤醒Commander，不改变任何预算的执行：停止派发仍只在用尽或deadline到达时发生（CR-025）。

`decisions` 在第 5 条之后新增第 6 条：

- 旧：无（REQ-042 有 5 条决定）。
- 新：

  > 预算风险的阈值80%为初值：在停止之前给Commander留出上限的五分之一（默认100次迭代中的20次、120小时中的24小时），用来重排剩余工作、缩小范围或请用户追加预算；更早的阈值在长Run中过早唤醒，更晚的留下的余量不够一次派发。价格未知的费用上限无法执行（BUDGET_UNENFORCEABLE），没有风险可报。kind取停止的名字（max_iterations、max_duration_hours、max_usd、deadline_at，与run.incomplete的stop相同），used与limit用账本的单位（unit：iterations、seconds、usd；deadline_at为自Run创建起的秒数）（CR-025）。

REQ-021 不改：它的行为 1 说“budget 风险”唤醒 Commander，现在由 REQ-042 行为 9 定义，事件名 `budget.risk` 即实现已在消费的那个。

不新增 AC：预算风险是监督的一个输入，由下文的支撑测试覆盖。REQ-042 的 release_targets 含 v0.1，新 AC 会给 v0.1 增加一条门禁测试，这超出了补齐定义的范围。审查若认为需要，可以另加 AC-042-07（加严，仍属 D1）。

### 2. 交接期间的派发拒绝码（RejectionReason 的补齐）

`contracts/protocol.schema.json` 的 `$defs.RejectionReason`：

- `enum` 旧：`["STALE_ROLE_EPOCH", "STALE_REVISION", "STALE_LEASE_EPOCH", "SPEC_MISMATCH", "CANDIDATE_MISMATCH", "ILLEGAL_TRANSITION", "DEPENDENCY_NOT_ACCEPTED", "BUDGET_NOT_RESERVABLE", "ROLE_CAPABILITY_MISSING", "EVIDENCE_GATE_UNMET", "SESSION_NOT_INDEPENDENT", "KEY_REUSE", "LEASE_UNAVAILABLE", "RETRY_NOT_DUE"]`
- `enum` 新：末尾加 `"ROLE_BINDING_PENDING"`，其余不变、次序不变。
- `description` 旧：`decisions.reason_code and Message kind=rejected payload.reason_code (CR-003). RETRY_NOT_DUE (CR-009): the task waits for its persisted next_retry_at (REQ-042).`
- `description` 新：`decisions.reason_code and Message kind=rejected payload.reason_code (CR-003). RETRY_NOT_DUE (CR-009): the task waits for its persisted next_retry_at (REQ-042). ROLE_BINDING_PENDING (CR-025): the target role's binding is changing and the change ends by itself: a team change waits for its checkpoint (REQ-018), a rebind for the attempt in flight, or the new holder owes its first input, the handover package (REQ-019); no capability is missing, so the task is not reassigned; dispatch it again once the change has completed.`

实现时的划分（决策规则，不在锁内）：绑定存在、状态 ACTIVE，只因上述变更之一（含尚未授予会话）而不可派发时，拒绝码为 `ROLE_BINDING_PENDING`，细节写明是哪一种（例如 “role developer owes its first input (the handover package) until a turn of its new session completes”）。没有绑定、绑定不是 ACTIVE（BLOCKED、UNBOUND、已删除）、或会话缺少所需能力时，仍为 `ROLE_CAPABILITY_MISSING`。这个码不触发重新唤醒：ADR-025 决定 6 只为过期的决策重新触发；变更完成（首个输入付清、checkpoint 生效）照 ADR-025 决定 7 进入下一次唤醒的摘要，Commander 届时再派。

为什么选新码，而不是“只改细节文本”或“记为接受的代价”：

- 只改细节文本（不需 CR）：`decisions.reason_code` 是 REQ-035 决定 2 要求持久化的结构化原因（“原因码不只写在 event 中”）。码本身仍说“缺能力”，与 REQ-035 失败模式 3 的定义不符，按码统计或按码行事都会错。细节文本只纠正了给人看的那一半。
- 记为接受的代价：ADR-024 决定 4 已把它记为代价，并要 Commander 路径给出原因，所以这样做不能履行该义务。
- 新码：与 CR-009 的 `RETRY_NOT_DUE` 同理。CR-009 补的是“现有的码说的是别的事、缺一个原因码”（`LEASE_UNAVAILABLE` 说租约，`BUDGET_NOT_RESERVABLE` 说预算），属 CR-003 D1 第 1 条的“补齐缺失的原因码”。这里只加一个枚举值，不改 REQ-035 的条件与其余码。

### 3. 门禁在无 date-time 校验时拒绝判定（M2T10R-750 的门禁一半）

`tools/release_gate.py` 的 `validate_manifest`：

- 旧：

  ```python
  def validate_manifest(root: Path, manifest: dict) -> dict:
      root=root.resolve();lock=verify_baseline(root)
  ```

- 新：

  ```python
  def validate_manifest(root: Path, manifest: dict) -> dict:
      # Its schemas' times are date-time formats: a jsonschema without requirements-dev.txt's format extras would accept
      # any string there, so the gate refuses to judge rather than trust it (CR-025, M2T10R-750; as traceability.py does).
      need('date-time' in FormatChecker().checkers,'jsonschema cannot check date-time: install requirements-dev.txt')
      root=root.resolve();lock=verify_baseline(root)
  ```

拒绝走门禁已有的失败路径：stderr 一行 `{"status": "FAIL", "error": "jsonschema cannot check date-time: install requirements-dev.txt"}`，退出 1，在读任何证据之前。退出码与消息正文同仿本（`traceability: jsonschema cannot check date-time: install requirements-dev.txt`，退出 1），各自沿用自己的输出格式。`LOCK_REQUIRED` 与其余判定不变。

不在锁内、随视图重新生成：docs/02-functional-spec.md 与 docs/18（REQ-042 的行为 9 与决定 6）。

## 影响

- 预算风险（实现，批准并重新上锁之后，红灯先行）：
  - hoh-core `budget.rs` 加纯函数 `risks(&Budget, created_at, now) -> Vec<Risk>`，口径与 `budget::stop` 相同：已用加预留；价格未知的 usd 不算；deadline 按 created_at 到 deadline_at 的时长。
  - pace（persistence/retry.rs）已在每个 tick 用 `budget_and_stop` 读取同一份预算。它对尚未写过的种类，在 pace 自己的提交里写 `budget.risk`。事件 id 为 `<run_id>/budget.risk/<kind>`，每个 Run 每种只有一条，重启后也只有一条。
  - 写在 pace 而不是记录用量的那几处（迭代计数、Attempt 结束时的 wall 累计），原因有二：预留的运行时长与 deadline 随时钟变化，没有一次写入与之对应；而且停止判定也在 pace，风险与停止用的是同一份计算，不会说法不一。
  - 迭代的越线由 `iteration.completed` 的提交造成，下一个 tick 的 pace 写出风险，至多晚一个 tick。
  - 暂停中的 Run 不经 pace，恢复之后的第一个 tick 再判定。
- 预算风险的测试：
  - budget.rs 单元测试：每种的 79%/80%、预留计入、价格未知的 usd 不报、deadline 的时长比例、未设上限不报；
  - pace 测试：越线写一条，再越线不再写，重启后不重复，低于阈值不写，迭代经 `iteration.completed` 越线；
  - 经 supervise 的 Commander 唤醒测试：`budget.risk` 产生 High 的触发器并唤醒 Commander。
- 拒绝码：
  - hoh-core `protocol.rs` 的 `RejectionReason` 加 `RoleBindingPending`；
  - `decide::load_binding` 给出不 active 的原因，`decision.rs` 按上文划分；
  - schema 语料（`tests/fixtures/schema/corpus.json`）加正例与负例；
  - 单元与 decide 测试（红灯先行）：首个输入欠着、checkpoint 挂起、换绑挂起各一例为 `ROLE_BINDING_PENDING`，绑定 BLOCKED 仍为 `ROLE_CAPABILITY_MISSING`。
- 门禁：
  - bundle 测试（红灯先行），仿 tests/bundle/test_traceability.py:90 的做法：去掉 date-time 检查的 `FormatChecker` 下，门禁对一个合法的 manifest 拒绝判定，退出 1 并给出上述消息；有检查时照常。
  - 另在一个只装 jsonschema、不装 format 附加包的 venv 里实测一次，作为红灯证据。
- ADR 与记录：ADR-025 决定 1 中 `budget.risk` 的“产生方”改为本 CR 与 pace；ADR-024 决定 4 的 M2T17R-352 改为“已由 CR-025 的 ROLE_BINDING_PENDING 履行”；PROGRESS 的三个后续项关闭。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 预算风险不改变任何预算的执行：停止派发、INCOMPLETE、BUDGET_UNENFORCEABLE 与预留规则原样不变；它只多一个唤醒 Commander 的事件，REQ-021 的唤醒集合本来就含它。
- RejectionReason 只多一个值：现有码的含义与 REQ-035 的派发条件不变；`ROLE_CAPABILITY_MISSING` 仍用于缺能力、没有绑定或绑定被阻塞的情形，只是不再用于按计划进行中的变更。
- 门禁只加严：能校验 date-time 时，判定与原来逐一相同；不能校验时拒绝判定，原来会照常判定。
- 不改 CONTRACT 的架构与交互决定；不改 SQL 或 openapi；`budget.risk` 的事件 kind 是自由字符串（`$defs.Event.kind`），不需改 schema。

## 是否属于 CR-003 D1 预授权

- 第 1 项属第 1 条“修正规格之间的不一致”：REQ-021 引用了 REQ-042 没有定义的“预算风险”。补上定义，使它可判定、可实现。
- 第 2 项属第 1 条“补齐缺失的原因码”，与 CR-009 的 `RETRY_NOT_DUE` 同理，同时修正 REQ-035 失败模式 3 的“缺能力”与实际原因不符。
- 第 3 项属第 3 条允许的加严：门禁在无法校验时拒绝判定。
- 满足第 2 至 4 条（见“不变的部分”）。
- 由两名独立设计审查判定是否属细化，判定之后批准。

## 验证方式

- 两名独立设计审查判定后批准。
- 改上述三个受锁文件，`python3 tools/gen_spec_views.py`，`python3 tools/baseline.py relock --cr docs/cr/CR-025-budget-risk-role-binding-pending-gate-date-time.md`，`bash scripts/check.sh --bundle` 通过（含 schema 语料与 examples 对 protocol.schema.json 的校验）。
- 实现之后，“影响”中列出的测试都先红后绿：预算风险的单元、pace 与唤醒测试；拒绝码的单元与 decide 测试；门禁的 bundle 测试与无附加包 venv 的实测。
- `cargo test -p hoh-core -p hoh-agents`、`-p hoh-app`，以及 static.sh 通过。
