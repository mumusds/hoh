# 核心算法与可测试规则

所有公式是HoH设计启发式，不是论文已证明的最优算法。参数必须记录于run配置，不能在活跃run静默自改。提供参考实现用于测试思路，不替代Rust生产实现。

## 1. 就绪集与调度

先做Kahn拓扑检查；不存在的依赖/环使计划拒绝。eligible(task) = deps accepted AND current spec revision AND not leased AND role capability satisfied AND budget reservable。阻塞依赖不因高分强行运行。

排序分层：关键回归/数据完整性问题→解阻任务→普通任务；层内score=(4*user_value+3*unblock_value+3*risk_reduction+2*information_gain+age_bonus)/max(cost_estimate,1)。输入分值限定0..5，cost是粗粒度工作量而非虚假精确美元；无估计用中位桶。最后按created_seq,task_id稳定打破平局。age_bonus上限10，用于普通排序中的轻度老化；但有上限的老化不能保证不饥饿（分差超过10/cost时永远追不上）。因此另设max_wait_rounds（初值20，CR-002）：eligible但未被选的轮数达到上限的任务进入位于层0与层1之间的老化子层，子层内按首次eligible序号FIFO。这样只要层0为空，任一任务的等待轮数都有上界。

租约申请在同事务CAS revision；记录lease_owner/epoch/expiry。过期不能直接认为旧进程死：先检查/取消并fence旧结果。每个repo写槽=1；QA/Review读槽独立；Commander有保留槽。不要让排队并发过高吃完模型限额。

## 2. 双状态增量与净进展

A_t为工件，E_t为带版本的证据；下一计划读取(A_t,E_t,S_t)。引用论文机制 [S01]。净进展使用向量而不是单一得分：新增accepted AC、closed issues、reopened issues、新增critical/high、尚未消除的决策风险。只改README和多一次commit不自动算产品进展。

连续3轮（默认）accepted无增长、关键issue无下降且失败fingerprint重复→诊断。release稳定阶段不要求新功能；若总问题减少即为进展。规格范围变大时重设对照窗口并记录原因，不能假装进度倒退是系统失效。

## 3. Supervisor事件压缩

按problem fingerprint/spec revision聚合；一般窗口2秒、最大等待10秒（可调），严重完整性错误立即处理。每次取last_handled_seq之后的摘要和引用，超过上下文预算则按严重性/因果链选取，不丢关键版本。决策输出若expected_revision过期则重算，不能直接apply。

## 4. Context装箱

强制项：角色契约/目标/任务范围/spec+AC/候选身份/输出Schema，预算不足报CONTEXT_BUDGET_TOO_SMALL而不裁目标。剩余预算默认分配当前相关代码45%、证据35%、知识20%；按relevance、recency、causal relation排序，摘要代替原文但保留引用。非模型tokenizer时估算必须标estimated。被删细节保留检索索引。

QA context有偏差控制：先需求/AC/工件；后开发自测和建议。Project memory是帮助检索的经验，不是验收事实。

## 5. 重试与外部效果

本地错误修复最多3次/同一问题为初值；协议格式纠正最多2次；外部503/429使用Retry-After优先，否则full-jitter exponential，持久记录deadline/attempt_count/next_retry。预算耗尽停止派发。auth/unsupported是永久错误，不指数等待几天。

effect ledger阶段prepared→started→observed→committed；崩溃发生started之后先查询session/commit/远端对象，无法确认则UNKNOWN_EFFECT并交诊断。不承诺“消息去重=外部操作只执行一次”。PR创建先按run marker查找已有，再创建；发布同artifact摘要冲突拒绝。

## 6. 证据有效性

valid(e,c,s)要求：文件hash存在且一致，candidate/tree/spec/environment/test_plan对应，producer attempt真实关联，outcome完整，时间区间有效，测试运行前后工件未变。空observations或只有模型总结是INSUFFICIENT。对当前需求的旧证据可作历史，不计current accepted。

选择性回归：影响图闭包(修改模块→依赖→相关AC/TEST)。不确定影响时full suite；发布始终full required suite。不得仅按文件路径相同就重用旧证据。flaky保留第一次失败；重试成功单列，不自动满足strict profile。

## 7. 发布汇合

QA完成且Review完成且确定性checks有效且当前scope所有required AC覆盖→候选accepted。发布再检查部署/文档/真实适配/插件/soak。编译成功是必要非充分条件。合并/变基后commit变化需重新绑定验证，不能用PR头旧证据证明merge commit。

## 8. UI投影

project_seq是唯一回放顺序；事件处理器是纯函数(state,event)→state'。同event重复必须幂等，未知event显示diagnostic并保留，不使UI panic。实时token按约50ms合并绘制；底层事件持久速率与显示帧率分离。滚动位置用block_id+offset，而非可变绝对行号。

## 复杂度与验证

DAG检查O(V+E)，就绪堆O(log V)，历史查询带seq/task索引且分页。百万事件不得启动时全表反序列化。随机/属性测试覆盖排序稳定性、lease互斥、dedup、invalid evidence、replay等；参考Python模拟只验证不变量方向，Rust必须有同等实测。
