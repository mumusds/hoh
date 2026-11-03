# HoH 自主软件工程：设计与AI开发启动包 v1.1

**这是一套用于实现HoH的规格、资源和可执行开发工具，不是已经编译完成的HoH产品。** 所有70项产品需求都保留在 v1 交付范围；模板自测或模拟Agent通过，不能据此宣布产品验收完成。

[快速开始](START_HERE.md) · [完整启动提示词](LAUNCH_PROMPT.md) · [约束基线](CONTRACT.md) · [CR-001](docs/cr/CR-001-kit-hardening.md) · [验证报告](reports/REVIEW.md) · [研究来源](research/SOURCES.md)

## 用户操作

在专用Linux虚拟机中，以普通（非root）开发用户进入 `hoh/`，打开已登录的 Claude Code（自举期主开发会话，推荐）或 Codex/Pi，将下面一段交给它：

> 读取当前目录AGENTS.md、CONTRACT.md和LAUNCH_PROMPT.md，按完整启动指令执行HoH自主开发。环境是专用隔离VM，允许YOLO安装、开发、测试与修复。不要再次讨论已确定的产品范围，不要只做MVP，不要伪造验收。现在从环境检查和M0开始，按依赖持续推进全部里程碑。

AI会先执行 `bash scripts/bootstrap.sh --yolo`。已有Agent、模型默认值与认证保留；缺工具自动安装。缺网络、sudo或模型认证时只能如实记录阻塞，不能用模拟结果补齐。**登录账号和提供有效额度不是脚本能凭空完成的步骤。**

## 已定型的产品

Rust模块化单体；Codex Commander + Claude CTO/Developer + Pi QA/Research；可配置职位；事件驱动智能监督与确定性Runtime协同；默认`hoh`进入TUI，`hoh -p`非交互；Plan只在交互中切换；Workspace/Team/Focus/Timeline；统一API的Web GUI；内置规格模板、Skill与四层Prompt；需求—任务—候选—证据—验收可追踪；长期恢复与版本化发布。

不重造第四个Coding Agent，不叠Python无限循环，不为所有TUI操作新增Shell子命令。不强制增加隐私/沙箱专项。保留单写者和冻结候选，是工程正确性机制，不是额外权限隔离项目。

**发布分级（CR-001）：** `v0.1` 是中间可用版本（CLI/TUI 外壳、Claude 适配器、持久 Runtime、冻结候选与独立 QA/Review，状态 `INTERIM_RELEASED`）；`v1` 是完整交付（`COMPLETED`）。三个 Pi 插件在 v1 范围内，但上游不可用时以带证据的 DEGRADED 交付（`COMPLETED_DEGRADED`）。

## 文档导航

| 文档 | 用途 |
|---|---|
| [产品定义](docs/00-product-definition.md) | 用户、定位、目标、范围、非目标 |
| [PRD](docs/01-prd.md) | 功能、质量要求、优先级、成功指标 |
| [功能规格](docs/02-functional-spec.md) | 70项行为、边界、使用场景、验收映射 |
| [UX](docs/03-ux.md) | TUI、CLI、Web、输入、时间线、快捷键 |
| [系统架构](docs/04-architecture.md) | 模块、职责、数据流、Rust选型 |
| [接口与数据](docs/05-data-api.md) | Schema、API、SQLite、幂等和约束 |
| [编排系统](docs/06-orchestration.md) | 规格/工程循环、角色、消息、交接 |
| [核心算法](docs/07-algorithms.md) | DAG、调度、监督、上下文、恢复 |
| [开发计划](docs/08-development-plan.md) | v0.1 路径、M0–M7、49个任务、滚动细化 |
| [测试方案](docs/09-testing.md) | 单元、集成、真实Agent、PTY、Web、跨天 |
| [验收规范](docs/10-acceptance.md) | Definition of Done、证据、最终Gate |
| [部署](docs/11-deployment.md) | 构建、安装、后台Host、备份、升级回滚 |
| [目标产品使用手册](docs/12-user-guide.md) | 最终HoH的操作，不冒充已实现命令 |
| [适配器](docs/13-adapters.md) | Codex/Claude/Pi原生接口与差异 |
| [恢复与可靠性](docs/14-resilience.md) | outbox、epoch、故障、接管、资源 |
| [Prompt/Skill/记忆](docs/15-prompts-skills-memory.md) | 分层、覆盖、锁版本、插件分工 |
| [事实核验](docs/16-research-findings.md) | 已验证事实、纠正、设计建议与未知项 |
| [风险与缺口](docs/17-risks-gaps.md) | 未证实项、责任、验证方式 |
| [需求追踪表](docs/18-traceability.md) | REQ/AC/TEST/里程碑对应 |
| [AI开发执行手册](docs/19-ai-development.md) | 首次启动、持续开发、失败恢复 |
| [审核与验证](docs/20-review-report.md) | 实际检查、边界、待关闭缺口 |
| [规格治理](docs/21-spec-governance.md) | 文档权威性、变更、漂移处理 |
| [工程工具使用](docs/22-tooling.md) | 可执行脚本、候选冻结、证据门禁 |
| [ADR](docs/adr/ADR-001.md) | 10项关键架构决策（按编号继续） |
| [开发规范](CONTRIBUTING.md) / [变更记录](CHANGELOG.md) | 提交、测试、审查与版本记录 |

## 机器可读交付

`contracts/`含70项需求（M1/M2 的27项已细化为 detailed，其余为 outline，在各自里程碑开始前细化）、164条验收与同数测试场景（每条指定产出证据的检查）、49个任务、10个JSON Schema（含检查计划、live会话、soak时间线、原生attempt）、OpenAPI和可执行SQL；`resources/`含6个角色Prompt、12个原创工程Skill、16个文档模板；`tools/`与`scripts/`负责环境、自测、单次Agent调用、任务台账、候选冻结、按检查计划执行、证据门禁、基线/CR与生成视图。它们是开发辅助，不是Rust产品的替代实现。

## 检查启动包

```bash
bash scripts/bootstrap.sh --yolo --dry-run
bash scripts/check.sh --bundle
```

第二条需要Python 3.11+与`requirements-dev.txt`中的jsonschema。完成正常bootstrap后会自动准备这些依赖；单独检查可在虚拟环境安装。

`bash scripts/check.sh --product`会在产品未实现时失败。这是正确行为，不应为了通过测试创建只打印成功的假程序。

## 证据与边界

请查看[审核报告](reports/REVIEW.md)。启动包检查不调用真实模型，未编译Rust产品、未执行真实插件联调和72小时产品试运行；这些都是目标项目最终验收的强制任务。文档和工具降低偏离风险，但不能从理论上保证任意AI永不出错或无条件完美交付。
