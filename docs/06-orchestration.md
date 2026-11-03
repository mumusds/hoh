# 编排系统规范

## 固定工程约束与动态智能

Commander决定目标/优先级/研究/重新规划；Runtime校验合法性、排队、执行、恢复、验收。常规candidate_ready自动派QA+Review，无需LLM反复“批准下一步”；异常、里程碑和策略变化才调用Commander。Commander不能以自然语言绕过证据门禁。

## 规格循环

Discover→choose Lite/Standard/Full→resolve current decisions→write PRD/AC/design→independent spec review→readiness→task DAG。Plan中真人选择必须真实发生；非交互按已授权假设处理且记录，不假扮用户。完整产品方向未知则BLOCKED。已采纳的本HoH规格不应被AI重新采访一遍。

## 工程循环

选择一个coherent increment→fresh plan引用spec/candidate→Developer baseline→TDD/实现/自测→candidate提交和hash→QA/Review barrier→记录Issue/验收→Commander必要决策→修复或下一增量。稳定阶段允许只修复；不机械每轮加功能。QA和Review即使一方失败，仍收集另一方可用证据；取消高成本剩余检查可配置，但未运行不能算通过。

## 默认团队与会话

Commander=Codex；CTO/Developer=Claude；QA/Research=Pi；Reviewer=独立Codex session。可以使用同产品多个角色，但必须防止复用有自评偏见的开发会话作为独立验收。只读研究可并行；v1仅一个代码写租约。不同worktree并行开发属于后续增强，不自动开启。

## Commander监督

重要事件集合：need_decision、critical/high issue、no_progress、budget风险、角色失效、规格变更、milestone_ready、release_request。去重键(run,problem_fingerprint,spec_revision)，合并同一事务风暴；紧急事件绕过普通去抖。持久触发器带last_handled_seq，重启不重发旧指令。

Balanced默认：轮次/重要事件唤醒。Efficient减少常规检查；Intensive增加高风险task边界检查。不是定时无限“继续”。Commander不可用时已授权低风险动作可完成，遇新重大决策则BLOCKED；不要无限等待假装有人盯。

**Run 级 BLOCKED 的含义（CR-003）**：BLOCKED 只停 Commander 决策类派发（新的实现任务）；已运行的 Attempt 继续到安全点，Runtime 自动规则（candidate_ready→QA/Review、已调度重试、pause/cancel、持久化）照常执行。`runs.blocked_reason` 是当前阻塞集合里优先级最高的一项（BLOCKED_AUTH > COMMANDER_UNAVAILABLE > BUDGET_UNENFORCEABLE > BLOCKED_CAPABILITY > NEED_DECISION），完整集合在 `blocked_reasons_json`；集合清空且没有 pause/cancel 请求时回到 resume_state。只有在没有其他可派发 Task、且至少一个 Task 在等待外部（next_retry_at 未到）时，Run 才是 WAITING_EXTERNAL。

## 协作与上下文

角色间咨询经Router生成子task或request，带预算、parent、deadline。没有独立工具接口时用规范化结构化输出/文件提交桥；不假设所有Agent都有相同自定义工具。Runtime在安全点接收请求，再dispatch；Codex有实际steer能力时可走原生通道。

Handoff package含goal、spec/AC、current candidate、必要代码/ADR引用、已知问题、任务边界、输出Schema、剩余预算。QA初始以规格和固定工件为主，不先灌输开发者“全部通过”；自测报告可用作诊断而非裁决。禁止互传完整数小时会话。

## 指令与角色交接

队列状态QUEUED→DELIVERED→APPLIED/REJECTED；ACK仅到达。每个角色epoch随binding改动递增，旧epoch返回的指令只能存历史，不能改变项目。交接等待writer安全点；紧急中断标unknown effect并reconcile。新Commander读取当前规格/决策/问题/待投递而不是旧对话全文。

## 死锁与会议

咨询wait-for graph检测环；若QA等Developer改当前candidate则拒绝，改成Issue→下一候选。角色自身不能同步等待自己占有的唯一执行槽。预留监督/取消通道，不让所有token/进程槽被开发占满。

MOA最多两轮意见+一次决策为默认可调上限；最终生成Decision/ADR和行动，没有新证据即停止讨论并选择可回滚方案或标阻塞。不得以“多Agent都同意”代替执行证据。

## 三类门禁

Readiness决定是否有足够规格；Candidate Gate决定当前增量是否验收；Release Gate决定完整目标是否交付。每类读取自己的冻结revision。用户修改Prompt不能改变事实判定；修改质量profile需明确scope/CR并保留历史，不允许AI为通过测试降低要求。

## 本产品的自举开发

HoH尚不存在时（M0–M2），使用一个外部主开发会话（默认Claude）按本包任务板小步开发，并经单次原生桥派发独立Review会话（默认Codex）；v0.1发布时QA与Review为两个不同的独立会话（CR-001）。先做运行底座，不搭额外无限Python循环。达到M2之后才接入Codex指挥与Pi QA，把局部工作迁入新HoH，并保留外部监督/回滚路径；不能让未经测试的新harness自行签收自己。
