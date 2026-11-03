# 系统架构

## 架构决定

Rust 模块化单体，第一版三个crate：hoh-core（领域/状态/调度/验证）、hoh-agents（原生适配/协议/进程）、hoh-app（CLI/TUI/API/运行组合）。Web在web/。对外只有产品二进制hoh；内部daemon模式是隐藏实现入口，不列为用户需记忆命令。

每项目一个Engine host，拥有SQLite writer actor和执行租约；多个TUI/Web连接它。进程锁+DB epoch防止两个host启动同项目。不同项目可独立host；v1全局跨项目Agent并发限制由轻量本机broker或启动上限实现，不建分布式调度。

```text
CLI/TUI/Web → CommandService → Engine host
                              ├─ Specification + Team + Prompt
                              ├─ Workflow + Supervisor + Scheduler
                              ├─ AgentAdapter → native Codex/Claude/Pi
                              ├─ Verification + Git workspace
                              └─ SQLite transactions + outbox + artifact store
Events → query projections → TUI/Web/SSE/reports
```

## 模块边界

Specification 管Git规格语义、readiness和CR；Orchestration 管依赖/任务策略/双层编排；Execution 管attempt/进程/会话；Verification 管固定candidate与证据；Persistence 管事务/日志/对象/备份。各模块只经类型接口改变状态，禁止UI或Agent直接update表。

接口按ports定义，测试使用FakeAgent/FakeClock/FakeGit，不把Fake路径合入生产成功判定。Rust错误分DomainConflict/CapabilityUnsupported/TransientExternal/PermanentExternal/IntegrityViolation/BudgetExceeded/Cancelled。

## 技术选型与取舍

Tokio做异步I/O和子进程；同步rusqlite放专用writer线程，不阻塞Tokio worker。SQLite WAL本地存储，FK+busy_timeout，单写者策略 [S21]。Axum HTTP/SSE [S25]；本地控制优先Unix socket，Web桥只服务同项目。Git CLI使用argv数组，不经过shell拼接；尽量保留用户Git语义。

Ratatui/Crossterm只实现HoH界面，不嵌套三个第三方全屏TUI。原生agent通过机器接口输出可观察event。Web使用React/TypeScript/Vite，OpenAPI生成客户端，样式尽量小，不复制DSH整个插件系统。

schema JSON/SQL为规范；Rust类型与Schema单源生成或CI对比，不允许双手维护无限漂移。固定Cargo.lock、package-lock.json与rust-toolchain.toml；初次解析真实可用版本，禁止文档里捏造最新crate号。

## 后台生命周期

hoh启动连接host；不存在则启动独立session的host并等待ready。host先获得项目锁、读取schema、reconcile未完成attempt，再接受写命令。daemon不是靠TUI线程存活。客户端detach只退订。VM长期服务由systemd用户服务托管；logout持续运行需user linger，不能声称仅nohup保证重启恢复。

单机runtime用队列和SQLite outbox就足够。消息采用at-least-once投递+effect去重，不承诺外部副作用exactly-once。进程在发命令后未收到ACK就崩溃，必须查询会话、commit和effect marker后决定，不直接再跑可能重复的发布/迁移。

## 软件工件

project repo源码属于用户；运行状态放项目.hoh/（默认gitignore）；大日志/视频/截图按内容寻址对象目录存储。候选使用Git提交+tree+环境/规格/测试计划摘要。QA运行目录与写目录分离是为了版本稳定，不是本项目要求的操作系统权限沙箱。运行前后核对树；不一致→INVALID。

## 可替换性

Model不是Role，Role不是进程。Commander可更换而状态不丢；底座CLI不支持某能力时capability=false，走显式有限降级。Codex exec可作为早期或兼容降级，但不假称具有App Server实时steer。HoH Prompt与底座不可见原生system prompt是不同层。

## 扩展边界

未来可增加外部ACP客户端接口；内部HCP不可冒充兼容ACP [S27]。不将MCP当完整跨Agent状态机；必要自定义工具仅桥接“发消息/查事实/提交报告”等能力。第三方Agent的代码编辑、原生工具、原生压缩由它自己负责。
