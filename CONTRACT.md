# HoH 产品约束基线 v1.1

本文是用户确认的基线（v1.0 + [CR-001](docs/cr/CR-001-kit-hardening.md)）。优先级：用户后续明确指令 > 经批准的基线变更记录（`docs/cr/`）> 本文 > contracts 中的机器契约 > 详细设计 > 示例。发现冲突先记录并按较严格、可验证的行为实现，不擅自删除功能。

## 不可偏离的决定

1. 名称暂定 HoH，产物二进制名 `hoh`；基于 Rust 的自主软件工程 Harness，不重造第四个 Coding Agent。
2. Rust 模块化单体；Tokio、SQLite、本地 Git、Ratatui/Crossterm、Axum；Web 使用 TypeScript/React。Pi 扩展保持原生 TypeScript。第一版不加 Python 全局 Agent 循环、微服务、Kafka、向量数据库或另一全局编排框架。
3. `hoh` 进入 TUI；`hoh -p "任务"` 才是非交互入口；必要子命令只保留 `attach`、`web`、`doctor`。禁止 `hoh plan`、`hoh mode plan`、`hoh run` 等命令膨胀。
4. Plan/Build/Auto 是 HoH 行为模式；`/plan` 与 Shift+Tab 在 TUI 切换；Plan 对话、生成规格、不自动实施产品代码。非交互自动规划不是跳过规格。
5. 默认 Codex Commander/Reviewer、Claude CTO/Developer、Pi QA/Research；职位、运行时、模型、会话分离；用户可配置角色、模型、提示词与团队。更换在 checkpoint 交接。
6. Commander 事件驱动持续监督和决策；Rust 决定性运行时控制状态、派发、恢复与证据。常规阶段自动流转，不让 Commander 转发每条工具日志。
7. 单一主写者；候选版本冻结；QA 与独立 Reviewer 对同版本并行；新版本不得沿用旧版本验收。YOLO VM 不要求额外权限沙箱或隐私专项；仍保留版本完整性与证据约束。
8. Workspace 默认单主视图；Team 可选分屏；Focus 与三级 Timeline；可展开块、可点击证据、丰富响应式 statusline；进度基于已验收需求而非模型估计百分比。
9. 上下文采用四层 Prompt + 按需引用；Prompt/Skill/模板可编辑、版本化、内置、锁定；不全量互传会话。
10. 内置文档规范和原创可运行等效 Skill；不要求用户额外手工安装文档 Skill。三个 Pi 插件属于 v1 范围，自动解析/安装/诊断；不可偷偷省略或伪称兼容。插件真实集成（REQ-048/049/050）的发布类为 degradable：上游不可用或不兼容时，以带诊断证据与上游引用的 DEGRADED 交付，发布状态为 `COMPLETED_DEGRADED`；安装、诊断与显式降级本身（REQ-051）仍是 required（CR-001）。
11. 插件职责：alpertarhan/pi-smart-compact 管压缩连续性，pi-hermes-memory 管长期经验，@juicesharp/rpiv-todo 管 Pi 局部任务。HoH 管全局任务与证据；禁止双全局账本。
12. 支持 Greenfield/Brownfield、规格准备、需求变更、任务 DAG、SDD/TDD、QA、Review、修复、回归、发布与交付；文档随着真实实现更新。
13. TUI/Web 是客户端，关闭客户端不停止已接受的后台任务；明确的 pause/cancel 才控制执行；退出码和 JSON 通道明确。
14. 可靠失败是产品能力：缺认证、无模型、无工具、证据不足、预算耗尽均不能报告 COMPLETED。
15. 首发支持 Ubuntu 24.04/26.04 x86_64 VM；其他平台不得无测试宣称支持；Web 完整界面及三种 Agent 都属于 v1 完整范围，不可在 M1 就称最终完成。
16. 以 `contracts/requirements.json` 中 `release_targets` 含 `v1` 的全部需求和 `contracts/release-policy.json` 的 `v1` profile 为最终交付清单；阶段优先级不是删减许可。`v0.1` 是中间可用版本（发布状态 `INTERIM_RELEASED`），不是完成，也不得据此停工（CR-001）。
17. 规格滚动细化：`spec_maturity=outline` 的需求必须在其里程碑实现前由 spec 任务细化为 detailed（可判定 AC + 测试场景 + CR + 基线重锁）；细化不得删减范围或降低门槛。
18. 自举期（产品 Runtime 未通过 M2 之前）使用一个主开发会话加独立 Review 会话；Codex 指挥与 Pi QA 在 Runtime 可用后接入。这不改变第 5 条的产品默认团队（CR-001）。

## 本包是什么

这是可审计的开发启动包、规格和验证工具，不是已经实现的 HoH 产品。没有伪造可用的 `hoh` 占位程序。
包自测通过只表示文档/机器契约/脚本/参考逻辑通过本地检查，不能替代 HoH 实现后的真实三 Agent 联调、终端/浏览器测试和跨天验收。
