# CR-020 · 验收投影与独立报告的移交：RunResult 的计数、规则 16 的 profile 与遗留 Issue 的确认

ID：CR-020
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权（ADR-029 设计审查第 9 轮 Claude 3e0b2eb8-df86-43a1-8ce7-a575920f16be 判 PASS；第 10 轮 Codex 01a1d2ea-e3ed-7272-8fda-e1fcf69984ac 判 PASS、设计已收敛、CR-020 满足 CR-003 D1 的四项条件、可由 CTO 批准）
日期：2026-11-12
基线：只改 `contracts/development-tasks.json`（不在锁内）；不改受锁文件

## 背景

ADR-029（M2-T18：验收投影、独立报告的入库与候选汇合）的设计审查（M2T18D-58、-155、-161、-153、-259）指出几项工作落在别的任务里，只写在 ADR 里不够，需要写进承接任务的 done_when（承接须有契约载体，M2T16D-1050）：

- 受锁 RunResult 的 `accepted` 与 `remaining_requirements` 按需求计。ADR-029 的快照按 AC 计（`passing`、`accepted`、`total`），二者不能混用；RunResult 属于 M2-T13（`hoh -p` 的结果），它应当用 ADR-029 决定 12 的 `requirements_passed` 求，所以 M2-T13 依赖 M2-T18。
- M2-T13 解除汇合级的 NEED_DECISION（flaky、join_gate_refused、重新验证用尽）时，出口只能是重新验证或返工：二者都产生新的栅栏编号，汇合按自己的事件判幂等（ADR-029 J9），若在同一编号下回到 VERIFYING，Task 会停住。
- 原则一的规则 16（profile 的 live agents 须有会话）只在发布门禁给出 profile 时生效。产品的候选门禁没有 profile；由 M5-T02 的发布门禁传入（ADR-029 决定 14）。
- 确认修复要验证者的正向观察（ADR-029 决定 10）。其 Task 已取消、不再有后续轮次的源留下的 OPEN critical/high Issue 没有人再确认，release_gate.py 的 `required_zero_issue_severities` 会阻断发布；M5-T02 的全范围验证应逐条确认它们，并给决策出口。
- M5-T01 拆分 M5-T02 时，要把这些项与 CR-018 的项一起分给一个实现任务。

## 决定

`contracts/development-tasks.json`：

- M2-T13 的 depends_on 加 `M2-T18`；done_when 加：“受锁 RunResult 的 accepted 与 remaining_requirements 用 ADR-029 决定 12 的 `requirements_passed` 求（按需求计，不用快照按 AC 的计数）；解除汇合级 NEED_DECISION 的出口：join_gate_refused 与 reverify_exhausted 为重新验证（开下一轮）或返工，flaky 为要求新提交的返工（同内容守卫也看汇合的 flaky；REQ-055 的 flaky 处置属 M5-T02），每种出口都产生新的栅栏编号（ADR-029 J9）（CR-020）”。
- M5-T02 的 done_when 加：“发布门禁调用 ADR-029 的展示投影时传入 profile（原则一规则 16，只核对 outcome 为 PASS 的证据产生的 PASS 行，`RowFact.live_agents` 取证据的 live-sessions）；全范围验证也逐条确认 Run 中仍 OPEN/REOPENED 的 critical/high Issue（含其 Task 已取消、不再有后续轮次的），未确认的阻断发布；决策出口只有重新验证、返工或取消而阻断保持，任何豁免须所有者批准（CR-018“RESOLVED 须有验证者的观察”）；全范围验证对遗留 Issue 的确认按 ADR-029 决定 10 的同一条目规则写 absent 事实、在同一事务调用 `issues::settle_all`，验收失败的 Issue 由它写出的行按同一状态函数解决；发布导出的 open_issues 与全范围验证的待确认集合在导出的读快照里直接对 ADR-029 决定 10 的状态函数求值，不依赖物化的状态列；REQ-055 的 flaky 处置解除汇合级 flaky 时产生新的栅栏编号（ADR-029 J9）（CR-020）”。
- M5-T01 的 done_when 把“拆分时把 CR-018 交给 M5-T02 的各项分给一个实现任务”改为“拆分时把 CR-018 与 CR-020 交给 M5-T02 的各项分给一个实现任务”。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets；每项工作都有明确的承接任务。
- 不放宽任何检查或门禁：RunResult 按需求计、规则 16 只在给出 profile 时更严、遗留 Issue 未确认即阻断发布。
- 不改 CONTRACT 的架构与交互决定；不改协议、SQL、schema 或 openapi。

## 是否属于 CR-003 D1 预授权

属于第 1 条“修正规格之间的不一致”（ADR-029 的设计与 M2-T13、M5-T01、M5-T02 的 done_when 之间的承接缺口），满足第 2 至 4 条（见“不变的部分”）。由独立审查判定后批准。

## 验证方式

ADR-029 的设计审查判定 CR-020 属细化且可以批准；批准后 `python3 tools/gen_spec_views.py` 重新生成生成视图，`bash scripts/check.sh --bundle` 通过（validate_bundle 的依赖与归属规则）。
