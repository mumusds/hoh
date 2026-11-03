# 风险、遗漏项与关闭方式

|ID|风险/未知|处理与关闭证据|是否阻断现在写产品|
|---|---|---|---|
|R01|实际CLI版本/旗标变化|probe帮助/协议schema+live smoke，保存compat manifest|不阻断离线核心；阻断真实适配验收|
|R02|Pi仓库/包名迁移|按官方README解析来源并锁定，不盲升级已有安装|不阻断核心|
|R03|三插件共同hook兼容|组合测试、恢复/compact/无TUI测试|阻断插件完整验收|
|R04|QA/Reviewer与作者上下文污染|独立session与候选，先规格后自测摘要|阻断独立验收|
|R05|Claude中途steer能力差异|capability false时排队下轮；不伪装实时|不阻断规定的安全点steer|
|R06|外部副作用重复|outbox/effect ledger/reconcile与lost ACK测试|阻断生产可靠性|
|R07|模型误报完成|确定性证据、negative gate、真正代码覆盖审计|阻断生产发布|
|R08|规格变更造成完成率假象|revision与影响闭包，过期证据撤销current accepted|阻断追踪验收|
|R09|长日志/TUI卡死|分页、token合并、慢客户端测试、磁盘策略|阻断UX/NFR|
|R10|上游许可不一致|保留原许可、NOTICE、来源锁；本包不直接搬运第三方源码|代码复用前必须关闭|
|R11|hoh品牌/可执行名冲突|安装前检测which hoh，避免覆写未知二进制；发布前查仓库/包注册|不阻断本地实现，不宣称已商标审查|
|R12|安装器网络/代理失败|明确失败退出、无成功标记，可重跑，不无限retry|阻断完整preflight|
|R13|GitHub自审不满足规则|机器人required checks与独立AI会话；不假设同账户review可审批自己PR|只影响远程发布策略|
|R14|模型服务预算/认证不足|BLOCKED/INCOMPLETE，持久剩余任务；不拿mock替代live|不阻断离线实现|
|R15|未知的“完美”标准|70项需求/机器AC/真实验收；不提供无证据零缺陷保证|以可检验规格替代|
|R16|首次自举不具备成熟Runtime|外部Codex原生会话+持久任务板；M2后渐进接管|禁止自建外层无限循环|

## 本次审查已补充的设计

ACP命名冲突改HCP；queue ACK和任务完成分离；scope revision和role epoch fencing；transaction outbox而不是内存广播；UI回放high watermark；候选写入完整性；本地DB在线备份；模型思考不可观测时不编造；版本升级不污染活跃run；缺认证诚实阻塞；Plan/Auto与底座权限模式分开；发布合并后的新SHA重验。

关闭风险不是删除此行，而是添加实际测试结果、commit与日期；保留变更历史。
