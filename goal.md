# 最终目标

完整实现本目录定义的 HoH：Rust 原生长期自主软件工程 Harness，熟悉的 CLI/TUI、Web GUI、可配置专业 Agent 团队、规格/技能/提示词、双层编排、持久恢复和真实证据交付。

全部范围以 CONTRACT 与 `contracts/requirements.json` 为准，按依赖逐个完成 M0–M7，不把文档包、脚手架或模拟演示当成最终产品。

## 什么算完成

- **COMPLETED**：`python3 tools/release_gate.py --manifest <manifest>` 对 v1 profile 输出 `release_status=COMPLETED`。三个 Pi 插件按 CR-001 合规降级时为 `COMPLETED_DEGRADED`。
- 门禁通过意味着：70 项需求的全部 AC 在**同一个冻结候选**上有逐测试的 PASS 证据；14 项检查全部通过（含三种 Agent 的 live 会话、TUI PTY、Web e2e、恢复、新环境部署、72 小时 soak 与至少 20 个 live 迭代）；独立 QA 与独立 Review 是两个不同于开发者的全新会话，且没有 critical/high 问题。
- 交付物：源码与锁文件、可安装的 Linux 发布包、使用/部署/升级/回滚文档、SBOM 与第三方声明、测试原始记录、QA/Review 报告、证据清单与 release hash。
- **v0.1（INTERIM_RELEASED）是中间版本，不是完成。** 它在 M2 全部完成后发布（CR-003 D2），之后继续 M3。

## 路线与当前位置

| 阶段 | 出口 |
|---|---|
| M0 环境、样例、检查骨架 | 已完成；CR-003 修正任务图、门禁与 M2 规格（M0-T07） |
| M1 领域类型、进程底座、三适配器、CLI、doctor、TUI 外壳 | M1-T09 独立 QA+Review |
| M2 持久 Runtime、API、恢复、团队、监督、上下文、候选与 QA/Review | M2-T11 独立 QA+Review |
| v0.1 | V01-T02：`release_gate --profile v0.1` → INTERIM_RELEASED |
| M3 规格追踪、Prompt/Skill、Plan/Build/Auto | 先 M3-T01 细化规格（CR-003 D1 预授权） |
| M4 Workspace/Team/Focus/Timeline、终端兼容、容量 | M4-T04 |
| M5 证据/回归/门禁、备份、三 Pi 插件 | M5-T04 |
| M6 Web GUI、Git 交付、安装文档 | M6-T04 |
| M7 性能、交付真实性、72 h soak | M7-T04：`release_gate --profile v1` |

当前位置以 `.hoh-dev/PROGRESS.md` 与 `python3 tools/taskboard.py list` 为准，不以记忆或口头结论为准。

## 防止走偏的规则

1. **范围不缩。** 不删需求、不降门槛、不把 required 改为 degradable；做不到就报告 BLOCKED/INCOMPLETE 和原因。
2. **架构不换。** Rust 模块化单体；不重造第四个 Coding Agent；不加 Python 全局循环、微服务、消息队列或向量库；Shell 命令面只有 `hoh`、`hoh -p`、`attach`、`web`、`doctor`。
3. **一次一个写者。** 同一时间只有一个 writer 任务在运行；任务按台账依赖推进，不跳过、不并行改同一处。
4. **先规格后实现。** outline 需求先由该里程碑的 spec 任务细化（CR + relock），实现任务才能领取。
5. **证据说话。** 每个任务：失败测试 → 实现 → 冻结候选 → 计划内检查 → 独立 Review → 修复 → 合并。fixture/mock 结果永远不当成 live。
6. **不自己验收自己。** 开发会话不能充当 QA 或 Reviewer；审查会话不改代码。
7. **不停在半路。** 不在某个里程碑、MVP 或 v0.1 停工；只有真实外部阻塞（认证、额度、需要用户批准的范围变化）才停，停之前把不依赖它的工作做完并写清恢复点。
8. **每一步留痕。** 每个小步骤后更新 PROGRESS.md 与台账；会话结束或压缩前写下一步。

## 已知的外部条件

72 小时 soak 的墙钟时间（任何修复都要重跑）、三家订阅额度、新 VM 部署验证，以及超出 CR-003 D1 的规格变更都需要时间或用户参与。见 [docs/23 审计报告](docs/23-pre-m1-audit.md) 第 6 节。
