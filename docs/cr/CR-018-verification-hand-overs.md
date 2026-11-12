# CR-018 · 验证派发的移交：汇合写 ACCEPTED、规格位置与角色间路由

ID：CR-018
状态：PROPOSED
批准人：（待 ADR-028 设计审查判定后，由 CTO 依 CR-003 D1 预授权批准）
日期：2026-11-12
基线：只改 `contracts/development-tasks.json`（不在锁内）；不改受锁文件

## 背景

M2-T16 的 done_when 写有“候选的 VERIFYING/ACCEPTED/REJECTED 迁移”（CR-017）。ADR-028 的设计审查第 1 轮（M2T16D-01、-50、-54）指出：

- 判定候选 ACCEPTED 的汇合函数是 AC-054-05，属于 M2-T18；M2-T16 若另写一份 ACCEPTED 的判定，两处实现会分叉，而 M2-T18 的 done_when 里没有“在 barrier 关闭点写 ACCEPTED/REJECTED、门禁以 ACCEPTED 为准”。不改的话，ACCEPTED 的写者可能两头落空。
- ADR-025 决定 10 把当前 epoch 的 steer/query 路由与 M2T17R-154 移交给了 M2-T16，但角色间咨询（局部咨询、wait-for 环检测）属于 M3-T04（REQ-017、REQ-023）；只记在 PROGRESS 不够，应写进 M3-T04 的 done_when。
- 产品的规格还没有文件位置（`specs` 无路径列，规格导入属 M3-T02）。M2-T16 的冻结先用项目布局约定（`contracts/requirements.json`、`tests/check-plan.json`），由 M3-T02 替换为规格事实源的位置。

## 决定

`contracts/development-tasks.json`：

- M2-T16 的 done_when：把“候选的 VERIFYING/ACCEPTED/REJECTED 迁移”改为“候选的 VERIFYING 与 REJECTED 迁移、验证 barrier 的关闭与结论（ADR-028 决定 11）；验收门禁只认 ACCEPTED 的候选（ADR-028 决定 12），M2-T16 从不写 ACCEPTED”。
- M2-T18 的 done_when 加：“在验证 barrier 的关闭点调用汇合函数，按 AC-054-05 写候选的 ACCEPTED 或 REJECTED（带原状态条件，不覆盖 INVALID 与 STALE）；验收门禁以 ACCEPTED 为准（ADR-028 决定 11、12 的移交）；检查途中规格变化置 STALE 的规则与 M3-T02 一起实现（ADR-027 决定 5）”。
- M3-T02 的 done_when 加：“规格事实源的位置替换 M2-T16 冻结使用的约定路径（ADR-028 决定 4）；`spec.changed` 的产生，并把验证中的候选按 REQ-053 行为 7 置 STALE”。
- M3-T04 的 done_when 加：“steer/query 的产品路由与投递，含当前 epoch 的路由与哪些 Commander 消息还要核对接收方的 epoch（ADR-025 决定 10、M2T17R-154）；角色间咨询的 wait-for 环检测；投递 M2-T16 记为 `request.deferred` 的消息（ADR-028 决定 13）”。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets；每项工作都有明确的承接任务，没有落空。
- 不放宽任何检查：M2-T16 收紧了验收门禁（只认 ACCEPTED），M2-T18 接入汇合函数之前门禁一律拒绝。
- 不改 CONTRACT 的架构与交互决定；不改协议、SQL 或 schema。

## 是否属于 CR-003 D1 预授权

属于第 1 条“修正规格之间的不一致”（M2-T16 的 done_when 与 AC-054-05、M2-T18 范围的重叠；ADR-025 的移交与 M3-T04 范围的重叠），满足第 2 至 4 条（见“不变的部分”）。由独立审查判定后批准。

## 验证方式

ADR-028 的设计审查判定 CR-018 属细化且可以批准；批准后 `python3 tools/gen_spec_views.py` 重新生成 docs/18，bundle 自检通过。
