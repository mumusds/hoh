# 验收规范与完成条件

## 两个不同的完成

启动包完成：文档、契约、脚本与包测试有效，可供 AI 实施。产品完成：HoH 实现 v1 profile 的全部需求并通过真实验收。二者必须分别报告。v0.1 的 `INTERIM_RELEASED` 是第三种状态，不是产品完成。

## 发布 profile（contracts/release-policy.json）

|profile|范围|必需检查|通过后的状态|
|---|---|---|---|
|v0.1|`release_targets` 含 v0.1 的需求|除 web-e2e、plugin-live、soak 之外的检查|`INTERIM_RELEASED`|
|v1|全部 70 项需求|全部 14 项检查，含 soak|`COMPLETED`；存在合规降级时为 `COMPLETED_DEGRADED`|

## 产品 Definition of Done（v1）

- 每条 REQ 都有 AC 与 TEST 映射；每个 TEST 场景指定一个 `check_id`。
- 每条 AC 都有绑定当前 spec/candidate 的真实观察，并引用其场景要求的检查。
- profile 的全部检查完成；critical/high 问题为零；其他未解决问题列明，且不得破坏 required AC。
- 三 Agent 真实联调通过；三 Pi 插件真实联调通过，或按 CR-001 合规降级。
- TUI/Web/新 VM 安装与恢复实际通过；72h soak 达到门槛。

最终交付包括：完整源代码；Cargo.lock、package-lock、rust-toolchain；可执行的 Linux 发布包；Web 资产与资源包；使用、部署、升级、回滚文档；SBOM 与第三方 notice；测试原始记录；QA/Review 报告；证据清单与 release hash。

## 状态语义

- **PASS**：只用于有数据支撑的检查或 AC。
- **DEGRADED**：只允许出现在 release-policy 标为 degradable 的检查（plugin-live），以及 `release_class=degradable` 的需求（REQ-048/049/050）。DEGRADED 的检查行与 AC 观察都必须带 `degradation{reason, upstream_ref, diagnostic}`，诊断证据必须来自该检查的输出。引用了 DEGRADED 检查的 AC 不能是 PASS；AC 只有在其场景检查为 DEGRADED 时才能是 DEGRADED。
- **不满足要求的状态**：FAIL、BLOCKED、FLAKY、INVALID、UNKNOWN、NOT_RUN、SKIPPED 都不能满足要求。
- **其他约束**：空检查列表不允许 `all([])=true`；缺模型认证时不允许用 mock 顶替 live；成功退出码只是进程事实，不代表语义完成。

## 证据链（门禁逐项强制）

1. **候选**：`tools/candidate.py freeze` 绑定已提交的源码、环境清单和测试计划。测试计划必须是已提交到 Git 的检查计划 JSON（`contracts/check-plan.schema.json`），以保证它的命令经过审查。
2. **检查执行**：`tools/run_check.py --check-id X` 只执行计划中 X 的 argv；mode 和 cwd 由计划决定，执行前后都核对候选，并把声明的产物写入 `$HOH_EVIDENCE_DIR` 后哈希。门禁比对执行记录与计划的 command、mode、cwd 和产物，并要求：计划中的 mode 等于 policy 要求的 mode；记录由 run_check（或产品 hoh）以非 root 用户产生；各检查的执行目录互不重叠。
3. **live 检查**：必须产出 `live-sessions.json`（`contracts/live-session.schema.json`），列出实际使用的原生会话（agent、CLI 版本、session_id、解析出的模型），并绑定本检查和被测候选。经 `tools/agent_task.py` 运行时会自动追加。
4. **AC 观察**：`check_ids` 必须包含其场景要求的检查；`raw_evidence` 必须位于所引用检查的执行目录内（按解析后的真实路径判断，`..` 与符号链接不能越界），且至少一项来自场景检查本身。
5. **独立报告**：QA 与 Reviewer 的报告、原始会话和原生 attempt 记录都必须是 independent-qa / independent-review 检查的输出。attempt 记录需满足 `contracts/native-attempt.schema.json`，并且：role 一致、状态为已完成、candidate_id 为被测候选、session_ids 包含报告的 session_id 而不含开发会话、stdout 哈希等于原始会话。报告的 session_id 还必须出现在该检查的 `live-sessions.json` 中。报告中的 critical/high finding 会阻断发布。开发、QA、Review 三个会话互不相同。
6. **soak**：时间线必须是 soak 检查自己产出的 `timeline.jsonl`。每行都要符合 `contracts/soak-event.schema.json`，绑定被测 HoH 候选，按时间有序，且落在执行窗口内；live `iteration.accepted` 不少于 20 个且不重复；持续时长不少于 72h（取墙钟起止差与记录时长中较小者）。
7. **基线**：`contracts/baseline-lock.json` 锁定 CONTRACT、需求、场景、policy、全部 schema、本文件，以及门禁工具及其依赖（release_gate、candidate、run_check、common、baseline）。任何改动都需要 APPROVED 的 CR，并执行 `tools/baseline.py relock`。门禁本身也会校验 CR 记录的状态；已提交的 CR 不能再用 `--amend` 修改。

这是证据一致性门禁，不是软件正确性的数学证明。即使记录形式合法，仍要审核测试是否真正覆盖需求；禁止用 echo 成功模拟测试。它也不能防止拥有整个 VM 写权限的执行者刻意伪造全部材料。

## 发布结果

- **COMPLETED**：v1 profile 全部通过，最终产物身份一致。
- **COMPLETED_DEGRADED**：同上，但存在合规的插件降级，降级清单出现在门禁输出与发布说明中。
- **INTERIM_RELEASED**：v0.1 profile 通过。
- **BLOCKED**：缺关键外部前置；列出精确缺项与可执行的恢复动作。
- **INCOMPLETE**：预算或时间耗尽，或必要检查未通过；报告已完成、未完成、风险与续跑点。
- **FAILED**：不可恢复的运行失败或数据完整性异常；保存证据，不能包装为部分成功发布。

允许保留暂存的失败分支；不得删除失败记录来伪造零回归。发布后发现缺陷是正常的维护输入：本门禁只说明覆盖了哪些范围，不声称绝对无缺陷。
