---
name: hoh-recovery
description: 故障诊断与恢复——attempt崩溃、超时、卡住，主机重启，UNKNOWN_EFFECT或净进展停滞时，先保全证据并核对外部效果，再在resume/retry/repair/rollback/BLOCKED中选择并追加留痕。
---
# 故障诊断与恢复

## 何时使用
- attempt崩溃、超时、无终态事件，或进程疑似悬挂/成为孤儿。
- 主机或会话重启后的reconcile；租约过期；出现UNKNOWN_EFFECT。
- 同一失败fingerprint重复出现，或净进展停滞（默认连续3轮）。

## 输入
- attempt记录、effect ledger、outbox、lease/epoch、事件序列。
- 日志与stdout/stderr、工作区状态（git status/diff）、进程信息（boot_id、进程启动标识、进程组）。
- 历史决策与失败fingerprint、重试计数与剩余预算。

## 步骤
1. 保全：把日志、`git status` 与 `git diff` 输出、相关DB记录复制到证据目录；不reset、不清理工作区、不删除失败候选。
2. 分类故障（选一个主类）：
   - 外部暂时性：429、503、网络中断
   - 永久：认证缺失、能力不支持、CLI不兼容
   - 协议：无效JSON、超大帧、缺字段
   - 本地缺陷：编译或测试失败
   - 资源：磁盘满、权限只读、SQLite busy
   - 进程：悬挂、孤儿、PID复用
   - 数据完整性：hash不符、半提交
   - 身份过期：spec revision、epoch或候选已变化
   - 无进展：fingerprint重复、空转
3. 核对外部效果：对ledger中started但未observed的效果，按session、commit、远端对象的run marker查询。确认已发生→记observed；确认未发生→可重投；无法确认→UNKNOWN_EFFECT，禁止盲目重跑。
4. 核对进程归属：用boot_id+启动标识+attempt判断，不能只看PID；取消时向正确的进程组发信号并wait，超时再升级kill，确认无孤儿。
5. 选择动作：
   | 条件 | 动作 |
   |---|---|
   | 原生会话可续且状态一致 | resume |
   | 暂时性错误且效果确认未发生 | retry：优先Retry-After，否则full-jitter指数退避；持久化next_retry与次数 |
   | 本地缺陷，同fingerprint修复未满3次 | repair：派Developer并附诊断 |
   | 协议错误，纠正未满2次 | 要求按契约重新输出 |
   | 候选不可修或回归严重 | rollback到最后accepted候选：用新commit/分支实现，不破坏用户改动 |
   | 认证/能力缺失、UNKNOWN_EFFECT、或超出上限 | BLOCKED：写精确缺项与恢复动作 |
6. 记录决策：追加一条新Decision（引用证据、选择理由、被拒动作），不覆盖旧历史；重试计数与预算在重启后不清零。
7. 验证恢复：重跑原失败的检查或reconcile查询，确认不变量成立（单写租约、无重复副作用、无被冒用的旧证据）。

## 产出物与格式
- 诊断报告：故障类别、时间线（事件seq）、证据路径、效果核对结果、所选动作与理由、验证结果。
- 必要时提交HCP Message kind=decision（带expected_revision与idempotency_key）。
- TaskResult：evidence引用诊断报告与日志；status：COMPLETED=已恢复并验证；BLOCKED=需外部前提；FAILED=数据完整性异常无法自动恢复；INCOMPLETE=预算耗尽；issues写残留风险。

## 完成判据
- 故障已分类；每个相关外部效果的状态明确（observed / 未发生 / UNKNOWN_EFFECT）。
- 动作已执行并验证，或明确BLOCKED并附恢复动作。
- 历史记录与失败证据完整保留。

## 常见错误
- 未核对效果就重跑，造成重复提交或重复PR。
- 只凭PID判断进程存活，或kill了被复用的PID。
- 用 `git reset --hard` 或清理工作区抹掉用户改动与证据。
- 重启后清零重试计数或预算。
- 把被取消或超时的候选当作已验证。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
