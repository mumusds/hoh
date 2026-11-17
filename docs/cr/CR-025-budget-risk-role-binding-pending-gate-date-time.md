# CR-025 · 预算风险的定义与产生、交接期间的派发拒绝码、门禁拒绝在无 date-time 校验时判定

ID：CR-025
状态：PROPOSED
批准人：待两名独立设计审查判定后，由 CTO 依 CR-003 D1 预授权批准
日期：2026-11-16（第 2 稿：设计审查第 1 轮之后，见文末“审查记录”）
基线：改受锁文件 `contracts/requirements.json`（REQ-042：新增行为 9、决定 6 与 AC-042-07）、`contracts/acceptance-scenarios.json`（新增 TEST-042-07）、`contracts/protocol.schema.json`（`$defs.RejectionReason`）、`tools/release_gate.py`（`validate_manifest`）；`python3 tools/gen_spec_views.py` 重新生成视图；批准后 `python3 tools/baseline.py relock --cr docs/cr/CR-025-budget-risk-role-binding-pending-gate-date-time.md`

本 CR 是 M2-T11 之前的后续项包 H（`.hoh-dev/evidence/followups-before-m2t11-2026-11-16.md` §1a 第 1、2 项，§2 的 M2T10R-750 门禁一半，§6 H）。CR-024（Commander 只读）仍在审查中，与本 CR 无交集，各自重新上锁。

## 背景

### 1. 预算风险没有定义，也没有产生方

REQ-021 行为 1 的重要事件集合含“budget 风险”，Commander 因它被唤醒：hoh-core 的 `supervise::classify` 把 `budget.risk` 判为 High（supervise.rs:111），监督步骤读取它（persistence/supervise.rs:25），唤醒的紧急程度为 2（wakes.rs:516）。但 REQ-042 没有定义什么是“预算风险”，也没有任何代码写 `budget.risk`，所以 REQ-021 的这一项永远不会发生。ADR-025 决定 1 写明这是规格缺口，唯一负责方是这一后续项（-264a、-360b、-458）。

### 2. 交接期间的派发被拒为“缺能力”

`decide::load_binding`（persistence/decide.rs:829-872）给出的 `active` 是：绑定为 ACTIVE、已有会话、且下列四项都不成立：

- 团队变更在等它的 checkpoint（`team::pending`，ADR-022 决定 8，REQ-018）；
- staffing 为在途 Attempt 挂起了该职位的换绑（`staffing::pending_change`，原因为 Rebind；ADR-030 决定 5，REQ-020）；
- 该职位此刻 epoch 的 session.start 通知（携带交接包）尚未交付（`handover::first_input_pending`，ADR-024 决定 4，REQ-019）；
- 该职位的会话欠着首个输入，且已有 PREPARED、STARTING 或 RUNNING 的 Attempt（`handover::first_input_in_flight`，M2T17R-201）。

首个输入欠着、但该会话没有在途 Attempt 时，`active` 为真：下一次派发正是把交接包送给新持有者的那个 turn（ADR-024 决定 4），失败或取消之后的重试也是。

`active` 为假时，Commander 派发该职位的任务被拒为 `ROLE_CAPABILITY_MISSING`，细节为 “role X has no active binding”（decision.rs:550-557）。而 REQ-035 失败模式 3 把 `ROLE_CAPABILITY_MISSING` 定义为“目标角色缺能力”，并要求“不转派给缺能力 Agent”：在上述情形里，职位并不（或尚不能判定为）缺能力，它的绑定正在按计划变更，变更自行结束。按码行事的 Commander 会把这当作缺能力（转派、改计划、请用户换 Agent），这些都是错的反应。

ADR-025 决定 6、7 已写明这一拒绝不需要 Commander 再派：屏障解除之后，调度轮照常派发 READY 的 Task（rounds.rs 每轮重读绑定）。ADR-024 决定 4 要 Commander 路径给出“稍后再派”的原因（M2T17R-352），剩下的缺口只是结构化的拒绝原因：`decisions.reason_code` 仍写着“缺能力”。

### 3. 门禁在没有 date-time 校验时照常判定

`tools/release_gate.py` 以 `FormatChecker()` 构造校验器（:15、:35）。jsonschema 没有安装 requirements-dev.txt 的 format 附加包时，`FormatChecker` 不含 date-time 检查，一个 `started_at` 为 `2020-01-01 00:00:00+00:00` 这样非 RFC 3339 的记录会被判为有效（M2T10R-750，reports/reviews/M2.md 第 9 轮）。仿本 `scripts/product/traceability.py` 已在 7a71129 改为没有该检查时拒绝判定（退出 1）；门禁受锁，这一半留给细化 CR。

## 决定

### 1. 预算风险（REQ-042 的细化）

`contracts/requirements.json` 的 REQ-042，`behavior` 在第 8 条之后新增第 9 条：

- 旧：无（REQ-042 有 8 条行为）。
- 新：

  > 预算风险（REQ-021行为1的“budget 风险”）：Runtime每次按预算判定停止时，对每种已设上限的预算也判定风险：max_iterations、max_duration_hours与价格已知的max_usd按停止的同一口径（已用加预留）达到上限的80%；deadline_at按墙钟，自Run创建起经过的时长达到创建至deadline_at时长的80%（该时长不为正时不判定）。成立、该预算的停止尚不成立、且该Run该种在该上限下还没有风险事件时，在同一事务内写一条budget.risk{kind, used, limit, unit, threshold}：threshold为0.8；used为判定所用的总量（已用加预留，deadline_at为已过去的墙钟秒数）；limit为同单位的上限（deadline_at为创建至deadline_at的秒数）。按水平判定：暂停期间越过的，在恢复后第一次判定时写。上限改变后按新上限重新判定。同一Run、同一种、同一上限只写一次，重启后不重复。它只唤醒Commander，不改变任何预算的执行：停止派发仍只在用尽或deadline到达时发生，也不计入重试次数（CR-025）。

`decisions` 在第 5 条之后新增第 6 条：

- 旧：无（REQ-042 有 5 条决定）。
- 新：

  > 预算风险的阈值80%为初值，是预警而不是保证：通常在停止之前还留有上限的约五分之一，供Commander重排剩余工作、向用户提出范围调整或请用户追加预算；一次迭代或一个Attempt的预留可能一步越过80%直到100%，那时只有停止，没有风险事件。更早的阈值在长Run中过早唤醒，更晚的留下的余量不够一次派发。价格未知的费用上限无法执行（BUDGET_UNENFORCEABLE），没有风险可报。kind取停止的名字（max_iterations、max_duration_hours、max_usd、deadline_at，与run.incomplete的stop相同）；unit为iterations、seconds或usd（max_duration_hours与deadline_at为seconds）。deadline_at与它的停止一样用墙钟；max_duration_hours只计Attempt的已用加预留，等待不计入（CR-025）。

`acceptance` 在 AC-042-06 之后新增 AC-042-07（加严，CR-003 D1 第 3 条）：

- 旧：无（REQ-042 有 AC-042-01..06）。
- 新：

  ```json
  {
    "id": "AC-042-07",
    "given": "Run Re设max_iterations=100，已用迭代79；Run Rf设deadline_at=创建时刻+100 s（墙钟）；FakeClock。",
    "when": "Re的预算判定一次；已用迭代到80后判定一次、再判定一次、重启（重新打开库）后再判定一次；Rf的墙钟到创建后79 s与80 s各判定一次。",
    "then": "Re在79时budget.risk为0条；到80时恰好1条budget.risk{kind:max_iterations, used:80, limit:100, unit:iterations, threshold:0.8}，之后的判定与重启后的判定都不再写；Rf在79 s时0条，80 s时恰好1条{kind:deadline_at, used:80, limit:100, unit:seconds, threshold:0.8}；这些判定前后，两个Run的state、派发数与attempt_count都不变。",
    "test_id": "TEST-042-07"
  }
  ```

`contracts/acceptance-scenarios.json` 的 `scenarios` 在 TEST-042-06 之后新增：

```json
{
  "id": "TEST-042-07",
  "requirement_id": "REQ-042",
  "acceptance_id": "AC-042-07",
  "check_id": "unit",
  "fixture": "hoh-core预算判定（pace）+临时SQLite+FakeClock（含墙钟）+重启模拟（重新打开库）",
  "steps": ["建Re（max_iterations=100，已用79）并判定一次", "已用到80，判定两次", "重新打开库再判定一次", "建Rf（deadline_at=创建+100 s），墙钟到+79 s与+80 s各判定一次"],
  "pass_criteria": ["Re：79时budget.risk为0条，80时恰好1条且载荷为{kind:max_iterations, used:80, limit:100, unit:iterations, threshold:0.8}，再判定与重启后仍为1条", "Rf：+79 s为0条，+80 s恰好1条且载荷为{kind:deadline_at, used:80, limit:100, unit:seconds, threshold:0.8}", "每次判定前后两个Run的state、outbox派发数与attempt_count不变"],
  "observables": ["events导出（budget.risk的完整载荷）", "runs与tasks行", "FakeClock日志"]
}
```

这给 v0.1 与 v1 两档门禁加了同样的一项：REQ-042 的 `release_targets` 含两档，所以 `expected_ac` 都多出 AC-042-07；门禁要求 acceptance-scenarios 里有这个场景，`unit` 检查里有名字含 `test_042_07` 的测试，并且该检查的 `test-results.jsonl` 里有它的一条 PASS（release_gate.py:196-198 的规则，未改）。AC-042-07 归新的实现任务 M2-T21（见下文“不在锁内、随 CR 同步”）；已完成的 M2-T19 不改。

REQ-021 不改：它的行为 1 说“budget 风险”唤醒 Commander，现在由 REQ-042 行为 9 定义，事件名 `budget.risk` 即实现已在消费的那个。

### 2. 交接期间的派发拒绝码（RejectionReason 的补齐）

`contracts/protocol.schema.json` 的 `$defs.RejectionReason`：

- `enum` 旧：`["STALE_ROLE_EPOCH", "STALE_REVISION", "STALE_LEASE_EPOCH", "SPEC_MISMATCH", "CANDIDATE_MISMATCH", "ILLEGAL_TRANSITION", "DEPENDENCY_NOT_ACCEPTED", "BUDGET_NOT_RESERVABLE", "ROLE_CAPABILITY_MISSING", "EVIDENCE_GATE_UNMET", "SESSION_NOT_INDEPENDENT", "KEY_REUSE", "LEASE_UNAVAILABLE", "RETRY_NOT_DUE"]`
- `enum` 新：末尾加 `"ROLE_BINDING_PENDING"`，其余不变、次序不变。
- `description` 旧：`decisions.reason_code and Message kind=rejected payload.reason_code (CR-003). RETRY_NOT_DUE (CR-009): the task waits for its persisted next_retry_at (REQ-042).`
- `description` 新：`decisions.reason_code and Message kind=rejected payload.reason_code (CR-003). RETRY_NOT_DUE (CR-009): the task waits for its persisted next_retry_at (REQ-042). ROLE_BINDING_PENDING (CR-025): the target role's binding is ACTIVE and cannot be dispatched now only because one of these holds: (1) a team change of the role waits for its checkpoint (REQ-018); (2) staffing holds a rebind of the role pending for the attempt in flight (REQ-020); (3) the binding has no session yet; (4) the session.start notice of the role's epoch, which carries the handover package, is not delivered yet (REQ-019); (5) the role's session owes its first input (the session.start of its epoch was delivered to it and none of its attempts is SETTLED) and has an attempt PREPARED, STARTING or RUNNING. These are exactly the cases in which an ACTIVE binding could not be dispatched before this code; it names them and adds none. A session that owes its first input with no attempt in flight is dispatched: that dispatch is the turn that delivers the package. ROLE_CAPABILITY_MISSING stays for: no binding of the role; a BLOCKED binding; no role contract stating the capabilities the task needs, which takes precedence over (1) to (5); and a session lacking a capability the task needs, which takes precedence over (4) and (5), where that session is the one that will serve, and is not judged under (1) to (3), where the serving session is not settled yet. The task is not reassigned: it stays READY, and the scheduling round dispatches it once the change has completed, with no new decision.`

判定的次序（决策规则，与锁定的 description 一致）。检查的位置不变：在 decision.rs 的 `RETRY_NOT_DUE` 之后、`BUDGET_NOT_RESERVABLE` 之前（ADR-020 决定 11）；它之前的验证轮（ILLEGAL_TRANSITION）、SPEC_MISMATCH、依赖与迁移检查都不变。`load_binding` 不再只给 `active`，而给出绑定的状态与上述五项各自是否成立；`dispatch` 依次判定：

| 次序 | 条件 | 拒绝码与细节 |
|---|---|---|
| 1 | 该职位没有绑定行 | `ROLE_CAPABILITY_MISSING`，“role X has no binding” |
| 2 | 绑定为 BLOCKED（`role_bindings.state` 只有 ACTIVE 与 BLOCKED） | `ROLE_CAPABILITY_MISSING`，“role X is blocked” |
| 3 | 没有职位契约给出该任务所需的能力（decision.rs:559-566 的分支） | `ROLE_CAPABILITY_MISSING`，原细节不变 |
| 4 | (1) 团队变更等 checkpoint、(2) 换绑挂起、(3) 没有会话，任一成立 | `ROLE_BINDING_PENDING`，细节写明哪一项（多项时都写）；能力不判定：将来服务的会话尚未确定 |
| 5 | 会话缺少所需能力（原能力子集检查） | `ROLE_CAPABILITY_MISSING`，原细节不变 |
| 6 | (4) 通知未交付、(5) 欠着首个输入且有在途 Attempt，任一成立 | `ROLE_BINDING_PENDING`，细节写明哪一项，例如 “role developer owes its first input (the handover package) and has an attempt in flight” |
| 7 | 都不成立 | 照原样继续（预算、槽位等） |

不扩大不可派发的集合：表中第 1 至 6 行的情形在本 CR 之前都已被拒（第 1、2、4、6 行的正是 `active` 为假的那些，第 3、5 行原本排在其后），所以任何一次派发被接受与否与之前相同，只是拒绝码在第 4、6 行变为 `ROLE_BINDING_PENDING`，第 1 至 3 行的细节更具体。(3) 与 (4) 可以同时成立（session.start 已写、绑定器尚未授予会话），按第 4 行判定。

不新增再派义务：这个码与其他非过期的拒绝一样只进历史与下一次唤醒的摘要（ADR-025 决定 6），不触发重新唤醒；任务保持 READY，屏障解除后由调度轮派发，不需要 Commander 另作决策（ADR-025 决定 6、7）。

为什么选新码，而不是“只改细节文本”或“记为接受的代价”：

- 只改细节文本（不需 CR）：`decisions.reason_code` 是 REQ-035 决定 2 要求持久化的结构化原因（“原因码不只写在 event 中”）。码本身仍说“缺能力”，与 REQ-035 失败模式 3 的定义不符，按码统计或按码行事都会错。细节文本只纠正了给人看的那一半。
- 记为接受的代价：ADR-024 决定 4 已把它记为代价，并要 Commander 路径给出原因，所以这样做不能履行该义务。
- 新码：与 CR-009 的 `RETRY_NOT_DUE` 同理。CR-009 补的是“现有的码说的是别的事、缺一个原因码”（`LEASE_UNAVAILABLE` 说租约，`BUDGET_NOT_RESERVABLE` 说预算），属 CR-003 D1 第 1 条的“补齐缺失的原因码”。这里只加一个枚举值，不改 REQ-035 的派发条件与其余码。

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

拒绝走门禁已有的失败路径：stderr 一行 `{"status": "FAIL", "error": "jsonschema cannot check date-time: install requirements-dev.txt"}`，退出 1，在核对基线和读取 manifest 所引用的任何证据之前（CLI 已读入 manifest 本身）。退出码与消息正文同仿本（`traceability: jsonschema cannot check date-time: install requirements-dev.txt`，退出 1），各自沿用自己的输出格式。`LOCK_REQUIRED` 与其余判定不变。

改 release_gate.py 的提交同时加入它的 bundle 测试（仿 tests/bundle/test_traceability.py:90：去掉 date-time 检查的 `FormatChecker` 下，门禁对一个合法的 manifest 退出 1，stderr 为上述 JSON 行；有检查时照常判定）。这条测试在插入 `need(...)` 之前先跑出红灯，红灯输出与“只装 jsonschema、不装 format 附加包的 venv 里实测门禁”的结果作为同一提交的证据。`scripts/check.sh --bundle` 不编译 Rust，所以重新上锁时 bundle 就已经罩住这一拒绝，不留一段没有回归测试的时间。

不在锁内、随视图重新生成：docs/02-functional-spec.md 与 docs/18（REQ-042 的行为 9、决定 6 与 AC-042-07）。

### 不在锁内、随 CR 同步

**`contracts/development-tasks.json`**

- 新增实现任务 M2-T21，固定此编号。它在 `tasks` 中插在 M2-T20 之后（列表顺序即领取优先级）；M2-T20 由 CR-024 新增，CR-024 尚未批准、main 上还没有它，所以本 CR 先插在 M2-T15 之后，CR-024 之后把 M2-T20 插在 M2-T15 与 M2-T21 之间：

  ```json
  {
   "id": "M2-T21",
   "milestone": "M2",
   "kind": "implement",
   "owner_role": "developer",
   "writer": true,
   "title": "CR-025 细化：预算风险（budget.risk）的产生、交接期间的派发拒绝码 ROLE_BINDING_PENDING、门禁在无 date-time 校验时拒绝判定",
   "depends_on": ["M2-T13", "M2-T19"],
   "requirements": ["REQ-042", "REQ-035", "REQ-021"],
   "acceptance": ["AC-042-07"],
   "done_when": "AC-042-07 通过；REQ-042 行为 9 与决定 6 的其余部分（各种预算的口径、deadline_at 的墙钟与不为正的跨度、按水平判定、每个上限一条、pace 没有别的写入时也提交、WAITING_EXTERNAL 与 BLOCKED 中照写、不计入重试、经 supervise 唤醒 Commander 与提交后崩溃的恢复）由 CR-025“影响”所列的 budget.rs、pace 与唤醒测试覆盖。ROLE_BINDING_PENDING 没有自己的 AC：由 CR-025“影响”所列的单元与 decide 测试覆盖（五项各自成立、首个派发与失败后重试的正对照、交叉与优先次序、被接受与否不变、屏障解除后调度轮派发），AC-035-01（TEST-035-01）的缺能力一例仍为 ROLE_CAPABILITY_MISSING。门禁的拒绝没有 AC：由与 release_gate.py 的修改同一提交的 bundle 测试（红灯先行）与无 format 附加包 venv 的实测覆盖，仿本一半已由 tests/bundle/test_traceability.py:90 覆盖。"
  }
  ```

  M2-T19 与 M2-T13 的 `acceptance` 是显式列表（AC-042-01/02/03/05/06 与 AC-042-04），所以 AC-042-07 只有 M2-T21 一个实现任务；REQ-035 与 REQ-021 列为它的需求，但不认领它们的 AC（显式的 `acceptance` 只含 AC-042-07）。AC-042-07 需要的 `unit` 检查由 M0-T03 交付，M0-T03 是 M2-T21 的祖先（经 M2-T19 或 M2-T13）；v0.1 档的 V01-T01 经 M2-T11 以 M2-T21 为祖先，v1 档的 M7-T04 经 V01-T02、V01-T01 与 M2-T11 以它为祖先（tools/validate_bundle.py 的任务图规则，已在模拟应用本 CR 的副本上核对，见“验证方式”）。
- M2-T11 的 `depends_on` 加 M2-T21。它的 `done_when` 已经要求对 M1/M2 每条 AC 给出 PASS/FAIL/BLOCKED/NOT_RUN，自然包括 AC-042-07。

## 影响

- 预算风险（实现，批准并重新上锁之后，红灯先行）：
  - hoh-core `budget.rs` 加纯函数 `risks(&Budget, created_at: SystemTime, wall: SystemTime) -> Vec<Risk>`：用量类（迭代、时长、价格已知的费用）用账本的已用加预留，与 `budget::stop` 同口径；`Budget` 本身仍按 `context.now()` 装载（在途时长的口径不变）；deadline 只用 `wall`（`context.wall()`，与它的停止 `budget::stop(&budget, wall)` 相同），跨度为 `deadline_at - created_at` 的墙钟秒数（`runs.created_at`），跨度不为正时不判定；每种的停止已成立时不报该种。比较为 `used >= 0.8 * limit`。
  - pace（persistence/retry.rs）已在每个 tick 用 `budget_and_stop` 读取同一份预算。对每个成立的风险，若还没有 id 为 `child_id(run_id, "budget.risk/<kind>/<limit>")` 的事件（ids.rs，至多 160 字符），就在 pace 自己的事务里写它；pace 没有别的写入时也提交这一条。`<limit>` 是载荷中 limit 的十进制写法，所以上限改变后 id 不同，按新上限再报一次；同一上限的重复判定与重启都找到同一个 id，不再写。
  - 写在 pace 而不是记录用量的那几处（迭代计数在 `closing::iteration` 的事务里、Attempt 结束时的 wall 累计），原因有二：预留的运行时长与 deadline 随时钟变化，没有一次写入与之对应；而且停止判定也在 pace，风险与停止用的是同一份计算，不会说法不一。迭代的越线由 `iteration.completed` 的提交造成，下一个 tick 的 pace 写出风险，至多晚一个 tick。
  - pace 也在 WAITING_EXTERNAL、BLOCKED 与 Commander 阶段运行（retry.rs:618-628），这些状态下照样写；暂停中的 Run 不经 pace，恢复后第一个 tick 判定（按水平判定，暂停期间越过的阈值不会漏）。这些状态都不在停住集合 G 里，`budget.risk` 以普通的 Progress 意图写（ADR-031 更正 D）。
  - 风险事件不改 `attempt_count`、`next_retry_at`，不消耗 8 次重试，不改 Run 的 revision 与状态。
- 预算风险的测试：
  - budget.rs 单元测试：每种的 79%/80%；预留计入；价格未知的 usd 不报；未设上限不报；deadline 的墙钟比例，以及单调时钟与墙钟分开时（例如单调 70 s、墙钟 85 s）按墙钟判定；`deadline_at <= created_at` 不报；该种的停止已成立时不报；
  - pace 测试（含 TEST-042-07 即 `test_042_07`）：越线写一条且断言完整载荷；同一次 pace 中几种同时越线各写一条；再判定不再写；重启（重新打开库）后不重复；低于阈值不写；上限改变后按新上限再写一条；迭代经 `iteration.completed` 越线；暂停期间越过、恢复后写；WAITING_EXTERNAL 与 BLOCKED 中写，且 `attempt_count`、`next_retry_at` 不变；pace 没有别的写入时也提交；
  - 经 supervise 的 Commander 唤醒：`budget.risk` 产生 High 的触发器；仍可分配 Commander turn 时被唤醒，预算停止已成立时不另分配（监督的预算规则不变）；提交之后、supervise 消费之前崩溃，重启后照样被消费；
  - 价格已知的 usd 分支只有夹具测试（产品尚无价格表），不能当作实际费用的验证。
- 拒绝码：
  - hoh-core `protocol.rs` 的 `RejectionReason` 加 `RoleBindingPending`；
  - `decide::load_binding` 给出绑定状态与五项条件，`decision::dispatch` 按上表判定；
  - schema 语料（`tests/fixtures/schema/corpus.json`）加正例与负例；
  - 单元与 decide 测试（红灯先行）：
    - 五项各单独成立时为 `ROLE_BINDING_PENDING`，细节写明是哪一项；其中“绑定 ACTIVE、没有会话、也没有 session.start”一例；
    - 正对照：首个输入欠着、没有在途 Attempt 时派发被接受（就是送交接包的那个 turn）；首个 turn 失败之后，欠着时的重试派发被接受；
    - 没有绑定行、BLOCKED 为 `ROLE_CAPABILITY_MISSING`；
    - 交叉：没有职位契约与任一项同时成立为 `ROLE_CAPABILITY_MISSING`；(4) 或 (5) 与会话缺能力同时成立为 `ROLE_CAPABILITY_MISSING`；(1)、(2) 或 (3) 与当前会话缺能力同时成立为 `ROLE_BINDING_PENDING`；(3) 与 (4) 同时成立为 `ROLE_BINDING_PENDING`；
    - 每一例被接受与否都与本 CR 之前相同（只比较拒绝码以外的结果）；
    - 屏障解除之后，同一 READY 任务由调度轮派发，不需要新的决策。
- 门禁：见决定 3，bundle 测试与 release_gate.py 的修改在同一提交，红灯在插入 `need(...)` 之前。
- ADR 与记录：ADR-025 决定 1 中 `budget.risk` 的“产生方”改为本 CR 与 pace；ADR-024 决定 4 的 M2T17R-352 改为“已由 CR-025 的 ROLE_BINDING_PENDING 履行”；PROGRESS 的三个后续项关闭；`contracts/development-tasks.json`（不在锁内）按上文新增 M2-T21，M2-T11 依赖它。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets；AC-042-07 是新增的验收（加严）。
- 预算风险不改变任何预算的执行：停止派发、INCOMPLETE、BUDGET_UNENFORCEABLE 与预留规则原样不变；它只多一个唤醒 Commander 的事件，REQ-021 的唤醒集合本来就含它。它不给 Commander 删减已接受的 required 范围的权限：范围的调整仍向用户提出。
- RejectionReason 只多一个值：现有码的含义与 REQ-035 的派发条件不变；不可派发的集合不变，任何派发被接受与否与之前相同；`ROLE_CAPABILITY_MISSING` 仍用于没有绑定、绑定被阻塞、没有职位契约与会话缺能力，只是不再用于上述五项按计划进行中的变更。
- 门禁只加严：能校验 date-time 时，判定与原来逐一相同；不能校验时拒绝判定，原来会照常判定。
- 不改 CONTRACT 的架构与交互决定；不改 SQL 或 openapi；`budget.risk` 的事件 kind 是自由字符串（`$defs.Event.kind`），不需改 schema。

## 是否属于 CR-003 D1 预授权

- 第 1 项属第 1 条“修正规格之间的不一致”：REQ-021 引用了 REQ-042 没有定义的“预算风险”。补上定义，使它可判定、可实现。AC-042-07 属第 3 条允许的加严。
- 第 2 项属第 1 条“补齐缺失的原因码”，与 CR-009 的 `RETRY_NOT_DUE` 同理，同时修正 REQ-035 失败模式 3 的“缺能力”与实际原因不符；派发条件不变。
- 第 3 项属第 3 条允许的加严：门禁在无法校验时拒绝判定。
- 满足第 2 至 4 条（见“不变的部分”）。
- 由两名独立设计审查判定是否属细化，判定之后批准。

## 验证方式

- 两名独立设计审查判定后批准。
- 批准之前已在一个副本上模拟应用本 CR（从 CR 正文取出全部受锁的新文本与 M2-T21，把副本里的 CR 标为 APPROVED 只为模拟）：`gen_spec_views` 之后 `validate_bundle` 只余“受锁文件已改、需重新上锁”一项，任务图与 AC 归属无误；`baseline.py relock --cr` 得到第 18 版，`validate_bundle` PASS（167 条 AC、59 个任务）；`scripts/check.sh --bundle` 退出 0（487 项，1 项跳过）。
- 改上述四个受锁文件（门禁的修改与它的 bundle 测试同一提交，红灯在先），`python3 tools/gen_spec_views.py`，`python3 tools/baseline.py relock --cr docs/cr/CR-025-budget-risk-role-binding-pending-gate-date-time.md`，`bash scripts/check.sh --bundle` 通过（含 schema 语料与 examples 对 protocol.schema.json 的校验、AC 与任务的归属检查）。
- 实现之后，“影响”中列出的测试都先红后绿：预算风险的单元、pace（含 `test_042_07`）与唤醒测试；拒绝码的单元与 decide 测试。
- `cargo test -p hoh-core -p hoh-agents`、`-p hoh-app`，以及 static.sh 通过。

## 审查记录

- 设计审查第 1 轮，冻结提交 1d10d44：
  - Codex（会话 01a1e6dc-f79b-7ba1-b683-6f3c4992c399，attempt 7f25dc0b-e822-4550-8b8b-0615942b13af）：FAIL，CR25D-01 high，-02 至 -05 medium。
  - Grok（会话 01a1e6dc-f341-72f3-82d8-7109f0e663df，attempt e01cb72d-0384-4c87-84f8-d26e99af912b）：FAIL，CR25D-50 high，-51 至 -55 medium，-56 low。
  - 两位都判三项都可按 D1 由 CTO 批准，第 2 项须先收窄锁定的条件。
  - CR25D-01 = CR25D-50（high）：首个输入欠着被整段写成不可派发，会拒掉送交接包的那个 turn，交接永远完成不了。已改：锁定的 description 只列出今天使 `active` 为假的五项，写明欠着而没有在途 Attempt 时照常派发，不扩大不可派发的集合；测试加正对照（首个派发、失败后的重试）。
  - CR25D-02、-51（medium）：锁定的 description 缺 ACTIVE 前提、无会话一项与仍为 `ROLE_CAPABILITY_MISSING` 的情形，没有职位契约的分支与重叠时的优先次序未写，且列了 SQL 里没有的 UNBOUND 与“已删除”。已改：description 写入全部条件与优先次序，决定 2 给出判定次序表，去掉 UNBOUND 与“已删除”；测试加无会话一例与交叉例。
  - CR25D-03（medium）：背景说没有决定履行这项义务、决定说 Commander 届时再派，与 ADR-025 决定 6、7 不符。已改：背景写明 ADR-025 已规定由调度轮在屏障解除后派发，剩下的缺口只是拒绝码；决定写明不新增再派义务；测试加“解除后无需新决策即派发”。
  - CR25D-04、-52（medium）：deadline 的时钟、比较与非正跨度未定义。已改：deadline 用墙钟，跨度为 runs.created_at 至 deadline_at 的墙钟秒，比较为 `used >= 0.8 * limit`，跨度不为正不判定；其余用量按账本口径；按水平判定（暂停期间越过的恢复后写），WAITING_EXTERNAL、BLOCKED 中照写，不计入重试；测试加时钟分开、非正跨度等例。
  - CR25D-05（medium）：载荷的单位不能唯一判定。已改：threshold 为比例 0.8，used 为判定所用总量（含预留；deadline 为已过墙钟秒），limit 同单位（deadline 为跨度秒）；测试断言完整载荷，含同次多种越线。
  - CR25D-53（medium）：每种只写一次使上限改变后不再警告，字面 id 可能超过 160 字符。已改：每个 (Run, 种, 上限) 一条，id 用 `child_id`，pace 没有别的写入时也提交。
  - CR25D-54（medium）：建议加 AC-042-07。已加（D1 加严），并写明它给 v0.1 与 v1 门禁带来的要求；它归新的实现任务 M2-T21（第 2 稿曾写 M2-T19，按 CTO 的决定改为新任务：已完成任务的验收不改）。
  - CR25D-55（medium）：门禁的 bundle 测试排在实现之后，上锁到实现之间没有回归测试。已改：bundle 测试与 release_gate.py 的修改同一提交，红灯在插入 `need(...)` 之前，无附加包 venv 的实测作为该提交的证据。
  - CR25D-56（low）：写明检查位置不变。已写入决定 2。
  - 另按 Codex 的说明：决定 6 的“缩小范围”改为“向用户提出范围调整”，并写明 80% 是预警而非保证；门禁拒绝的时机写为“读取 manifest 所引用的证据之前”；测试加“提交之后、supervise 消费之前崩溃”，并区分记录风险事件与实际分配 Commander turn。
