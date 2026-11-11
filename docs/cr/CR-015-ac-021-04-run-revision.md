# CR-015 · AC-021-04 以 Run 的 revision 叙述过期决策

ID：CR-015
状态：WITHDRAWN（2026-11-11，设计审查第 2 轮：Codex M2T08D-107、Claude M2T08D-160 均判其放宽检查；改由 ADR-025 第三版决定 6 的 Task 快照栅栏，AC-021-04 按原文成立，不需要 CR）
批准人：（待定）
日期：2026-11-11
基线：修改受锁文件 `contracts/requirements.json`（REQ-021 的 AC-021-04）与 `contracts/acceptance-scenarios.json`（TEST-021-04），重新上锁

## 背景

决策的 `expected_revision` 在规格里有两种叙述。AC-035-01、AC-035-02（REQ-035）以 Run 的 revision 判定：Run revision=7 时 expected_revision=6 的提议被拒为 revision 过期；同一个派发提议原样再提交时，因为第一次已推进 Run 的 revision，第二次得到 STALE_REVISION。ADR-011 第 89 行与 `decision::validate` 照此实现（expected_revision ≠ runs.revision → STALE_REVISION）。AC-021-04 却以 Task 的 revision 叙述：触发器“基于 task revision=7 生成”，Runtime 把“该 task 推进到 revision=8”，expected_revision:7 的决策过期，expected_revision:8 的决策 APPLIED。产品中 runner 对 Task 的推进（LEASED→RUNNING、RUNNING→IMPLEMENTED）不推进 Run 的 revision，所以两种叙述在同一个决策入口上不能同时成立（M2-T08 设计审查 M2T08D-06、-56）。schema 与需求的行为条款没有规定 expected_revision 对应哪一个 revision。

## 决定

1. 决策的 `expected_revision` 是 Run 的 revision（与 ADR-011、AC-035-01/02 一致）。AC-021-04 改为以 Run 的 revision 叙述，Runtime 的推进为一次调度轮的租约（它在同一事务里推进 Task 与 Run 的 revision），其余不变：

   - given：触发器 K 在 Run revision=7（K 所关注的 task 也为 revision=7）时生成；Commander 作答前，Runtime 以一次调度轮的租约把该 task 推进到 revision=8，Run 的 revision 随之为 8。
   - when：FakeCommander 返回 decision{expected_revision:7, role_epoch:当前值}；对随后的重新触发返回 expected_revision:8 的决策。
   - then：第一条 decision 为 REJECTED(STALE_REVISION)，task revision 仍为 8 且无工作派发；恰好产生 1 个新触发器，其摘要基于 Run revision 8；第二条 decision 为 APPLIED。

2. TEST-021-04 的步骤与通过标准相应改写：步骤“推进 task 与 Run 到 revision 8（一次调度轮的租约）”；通过标准“decision#1 处理后工作 Task 的派发增量=0（重新触发产生的 Commander 唤醒通知不计）”“新触发器数=1，摘要中 Run revision=8”，其余不变。

## 影响

- `contracts/requirements.json`：REQ-021 `acceptance[3]`（AC-021-04）的 given、then。
- `contracts/acceptance-scenarios.json`：TEST-021-04 的 steps 与 pass_criteria。
- 重新生成 `docs/02`、`docs/18`，重新上锁。
- docs/adr/ADR-025 决定 6。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 不放宽任何检查：过期决策仍被拒、仍不改变 Task、仍恰好重新触发一次；第二条决策仍须 APPLIED。
- 不改 CONTRACT 的架构与交互决定；不改协议 schema。

## 是否属于 CR-003 D1 预授权

属于第 1 条“修正规格之间的不一致”；满足第 2 至 4 条（见“不变的部分”）。由独立审查判定后批准。

## 验证方式

- TEST-021-04（integration）：Run 与 Task 都在 revision 7，一次调度轮租约把两者推进到 8，第一条决策 STALE_REVISION 且无工作派发，恰好一个新触发器、摘要中 Run revision=8，第二条决策 APPLIED。
- AC-035-01、-02 不变并照常通过。
