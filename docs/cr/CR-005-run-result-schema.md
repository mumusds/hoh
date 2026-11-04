# CR-005 · 运行结果对象与 run.result 事件写入协议 schema

ID：CR-005
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权（独立 Review 会话 01a1a64d-e239-7760-8839-e1b80a0a70d3）
日期：2026-11-04
基线：HOH-BASELINE-1.1 revision 5 → revision 6

## 背景

REQ-004 的决定写明：“stream-json 终态事件 kind=`run.result` 与 json 结果字段集为确认值；M1-T06 实现时把 run.result 与 json 结果对象写入 protocol.schema.json（随该任务的 CR 重锁）”。字段集见 REQ-004 的行为第 1 条：json 模式 stdout 恰为 1 个对象，至少含 run_id、state、exit_code、blocked_reason、spec_revision、accepted、total、last_event_seq；stream-json 的最后一行是持久化终态事件 kind=`run.result`（含 state、exit_code）。accepted/total 的含义取自 docs/03 与 docs/12 的“已验收需求 n/m”：当前规格 revision 下已验收的需求数与需求总数。协议 schema 目前没有这两个定义，M2-T13 的端到端 AC（AC-004-02/03、AC-002-01）也就没有可校验的契约。

## 决定

| # | 内容 |
|---|---|
| F1 | 新增 `$defs.RunResult`：前台客户端等到的 Run 结果，Run 处于使客户端结束的状态。`run_id`（1–160 字符）、`state`（只取 COMPLETED/FAILED/BLOCKED/INCOMPLETE/CANCELLED/PAUSED）、`exit_code`（该状态在 REQ-004 退出码表中的码：0/1/3/4/5/6，逐一对应）、`blocked_reason`（state=BLOCKED 时必须是 BlockedReason，其他状态为 null）、`spec_revision`（≥1 的整数，尚无规格时为 null）、`accepted`、`total`（≥0 的整数）、`last_event_seq`（≥0 的整数）。全部字段必填，不允许其他字段。客户端停止等待（SIGINT 130、SIGTERM 143）或无法开始（host 不可用 69、配置错误 78）时不写 RunResult：json 模式 stdout 保持 0 字节（REQ-004 失败模式），中断时只在 stderr 提示 |
| F2 | 新增 `$defs.RunResultEvent`：同时满足 `$defs.Event`，`kind` 为常量 `run.result`，`payload` 为 RunResult，因此只能是持久化的终态事件。stream-json 的最后一行按它校验 |
| F3 | 产品侧：`hoh_core::protocol::RunResult`（带 SchemaDef，按 schema 编解码）；schema 语料新增 21 个用例（RunResult 15 个、RunResultEvent 6 个），由产品校验器与 Python jsonschema 分别判定且结论一致；Rust 测试核对 RunResult 的 state 枚举是 RunState 的子集、恰为 REQ-004 的六种结束状态，并逐项核对各自的退出码 |

## 是否符合 CR-003 D1 预授权

1. 只补齐缺失的定义：字段集、事件 kind 与退出码都已是 REQ-004 中的确认值，本 CR 把它们写成 schema。
2. 不删除任何需求或 AC，不改 required/degradable，不改 release_targets。
3. 不放宽任何门禁：新增的是输出约束，只会让校验更严格；没有阈值或证据规则变化。schema 只能校验结构与状态—退出码的对应；退出码是否与 Run 的真实终态一致仍由 CLI 的映射函数（TEST-004-01）与端到端 AC（M2-T13）判定。
4. 不改变 CONTRACT 中的架构与交互决定。

## 不变的内容

70 项需求、166 条 AC、场景、release-policy、其他 schema 定义与门禁工具均不变；baseline_id 仍为 HOH-BASELINE-1.1。

## 验证

`cargo test -p hoh-core --test protocol_roundtrip --test schema_corpus`（语料往返与产品校验器判定）、`bash scripts/check.sh --bundle`（Python jsonschema 复核同一语料、基线校验），`python3 tools/baseline.py relock --cr docs/cr/CR-005-run-result-schema.md` 之后 `python3 tools/baseline.py verify`。

## 审查记录

M1-T06 的三轮独立 Review 一并审查了本 CR 的补丁：第 1 轮（会话 01a1a628-71d5-7872-b71d-bb5df74bdaf2）指出 exit_code 只限 0–255（M1T06R-02），第 2 轮（会话 01a1a63a-2056-73c1-b486-211230e3d599）指出 RunResultEvent 接受非终态结果（M1T06R-05），两处均已按上文修正；第 3 轮（会话 01a1a64d-e239-7760-8839-e1b80a0a70d3）判 PASS，确认四项 D1 条件均满足，仅指出 F3 的验证范围表述（M1T06R-06，已改）。各轮记录见 reports/reviews/M1.md。
