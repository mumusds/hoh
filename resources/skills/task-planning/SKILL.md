---
name: hoh-task-planning
description: 可验证增量规划——Commander在候选判定、新issue或spec变更后，从就绪集中选出一个局部完整、可外部观察的增量，写成带REQ/AC、范围、预算与证据化完成条件的任务。
---
# 可验证增量规划

## 何时使用
- 上一增量的Candidate Gate有了结果；出现新的或reopened的issue；spec revision变化；里程碑开始。
- 净进展停滞，需要重规划。

## 输入
- 当前spec revision与REQ/AC；已accepted的AC与有效证据；open/reopened issue（含严重度与fingerprint）。
- 任务DAG（自举期为 `contracts/development-tasks.json`，产品期为Runtime任务表）、写租约占用、角色可用性、剩余预算。

## 步骤
1. 快照：accepted AC n/m、按严重度统计的open issue、因spec变化而失效的证据、剩余预算。
2. 就绪集：依赖已accepted、基于当前spec revision、未被租用、有具备能力的角色、预算可预留。先做拓扑检查；缺依赖或有环则拒绝计划。实现任务关联的需求仍为outline（spec_maturity）时不可派发，先派该里程碑的规格细化任务。
3. 排序：分层——关键回归/数据完整性→解阻任务→普通任务；层内score=(4×用户价值+3×解阻价值+3×风险降低+2×信息增益+age_bonus)/max(cost,1)，各输入取0..5，age_bonus≤10；平局按created_seq、task_id。
4. 选一个增量：局部完整、可外部观察、能在预算内完成；稳定期允许只修复、不加功能。
5. 写任务，每项必填：
   - [ ] id；goal（一句话、可观察的结果）
   - [ ] REQ/AC/TEST id；depends_on；owner role
   - [ ] 范围：允许修改的模块/文件，以及明确禁止触碰的部分
   - [ ] 不可破坏行为与回归集合
   - [ ] 输出契约（TaskResult及附加工件）与预算（token/时间）
   - [ ] done_when：以证据描述（哪些检查需在冻结候选上PASS），而不是“代码写完”
6. 验证计划：QA与Review各自范围；回归按影响闭包选择，影响不确定则全量。
7. 净进展核对：连续3轮accepted无增长、关键issue不降且失败fingerprint重复→不再派同类任务，转recovery诊断或重规划。
8. 以HCP Message kind=dispatch提交，带expected_revision与idempotency_key；冲突则基于新快照重算。

## 产出物与格式
- 任务条目（上述字段）；里程碑级计划用 `resources/templates/development-plan.md`（阶段与出口、任务及REQ/AC、依赖DAG与主写者、实现顺序与并行边界、风险与阻塞、每任务完成证据）。
- HCP dispatch Message；TaskResult（evidence引用计划文件，summary写所选增量与理由，issues列被推迟项与风险）。

## 完成判据
- 所选任务字段齐全，DAG无环；每个任务链接REQ/AC（纯基础设施任务写明服务于哪些REQ）。
- done_when可由QA独立判定；预算可预留；同一时刻只有一个写任务持有写槽。

## 常见错误
- 规定模型的思考步骤，却不约束可验证的结果。
- “实现模块X”式任务，没有可观察结果或AC。
- 用任务done比例冒充产品完成度；修改基线任务表状态当作进度。
- 忽略reopened issue，或沿用旧spec revision上已失效的证据。
- 一个任务跨越多个写者，或明显无法在预算内完成。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
