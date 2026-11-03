# 长期运行、消息可靠性与恢复

## 运行不变量

I1 每repo最多一个有效代码写租约。
I2 state+event+outbox同事务。
I3 重复消息不重复提交逻辑状态。
I4 旧role_epoch/expected_revision不能推进当前任务。
I5 candidate变化即失效其当前验收。
I6 已ack的持久状态可从DB重放恢复；未确认的外部效果先核对。
I7 UI不可用不影响host；Commander不可用不会阻止cancel/保存状态。
I8 失败历史append-only，不靠覆盖成PASS清理问题。

## 启动reconcile

获取本机项目锁→DB版本/完整性检查→递增host_epoch→找到LEASED/RUNNING attempts→查询进程/会话/effect→可确认完成则观察结果并入库→确认未执行才能重投→无法确认进入UNKNOWN_EFFECT诊断。旧host即使恢复也因epoch写入被拒。

PID可能复用，记录boot_id、进程启动标识、session/attempt；仅用PID存在不能证明旧Agent仍在运行。不能对不属于本attempt的复用PID kill。

## 事务发件箱

dispatcher读取pending outbox并记录delivery attempt；ACK不直接标任务done。投递可重复，收件方按message_id/command key去重；工具副作用用effect ledger二次核对。消费端写收据与状态转换一个事务。回调失败进入dead-letter并显示，不无限隐藏重试。

## 取消与暂停

Pause请求立即持久并阻止后续派发，等待当前安全点；Cancel优先队列直接发原生interrupt/进程组信号。进入CANCELLED前收集工作区hash/日志，不能把未完成候选当verified。超时不等于已取消，显示CANCELLING并升级kill。

## 外部服务

网络5xx/429使用持久next_retry和jitter；鉴权缺失直接BLOCKED_AUTH；上下文溢出可原生compact后恢复但不得重新生成同副作用；CLI不兼容BLOCKED_CAPABILITY。重启不清retry计数或预算。工程deadline到达输出INCOMPLETE保留余项。

## 数据与磁盘

每次大对象写入先容量预检；低空间停止新执行但保持可读取/取消。对象临时文件恢复扫描并清理孤立未登记对象；已引用证据不自动删。WAL checkpoint按大小/空闲时机执行；busy时不阻塞整个UI。backup使用一致性API，迁移前后integrity_check。

## 版本变化与用户修改

用户编辑规格产生revision invalidation事件；新任务前建立新索引；当前attempt固定旧规格可完成但其结果标stale，Commander决定是否可迁移/重验，不能直接验收新需求。用户修改写工作区检测后停止派发并保存分支，不执行破坏性reset。

## 时间与测量

持久last_progress、last_heartbeat不同：进程心跳不代表工作有进展。模型沉默但工具仍运行时状态显示tool running。定时器使用单调时钟，恢复后重新计算持久UTC期限。72h测量必须记录中断区间与provider可用区间，拒绝手填数字冒充。
