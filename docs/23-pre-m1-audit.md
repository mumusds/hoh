# M1 开工前审计报告

日期：2026-11-03　范围：启动包 v1.1（基线 revision 3）、M0 成果、本机开发环境　处置：[CR-003](cr/CR-003-unblock-autonomous-delivery.md)

## 1. 结论

- **可以交给 AI 长期自主开发，但不能保证一次跑到 v1 COMPLETED。** 体量估算约 5.7–8.3 万行 Rust、1.3–2 万行 TS，60–90 轮独立审查，至少一个（大概率两个）72 小时 soak 窗口，是跨多个会话、持续数周的工作量。最终状态以 `release_gate --profile v1` 为准。
- **原规格与工具有一批会让无人值守开发卡住或走偏的缺陷**，都已在 CR-003 中修正（见第 3 节）。M1 的规格本身基本可写；M2 不补齐就只能猜。
- **启动包的治理设计质量高**：基线锁、冻结候选、检查计划、独立会话绑定都是可执行的强制规则，包自检 223 项全过，CI 绿色，M0 经过 3 轮独立审查。

## 2. 审计方法

三个只读审查会话并行，各自只拿到文件路径与问题范围，不带主开发会话的结论：

| 维度 | 看什么 | 结果 |
|---|---|---|
| 规格一致性 | requirements/scenarios/schema/openapi/SQL/设计文档/真实协议样例之间的矛盾与不可判定 AC | 20 项（6 高） |
| 门禁工具 | 会让诚实产品被误判失败、让未完成产品通过、或让开发循环卡死的工具缺陷 | 12 项（3 高） |
| 计划可行性 | 需求覆盖、任务粒度与顺序、用户文档与配置、外部硬依赖 | 15 项（5 高） |

主开发会话对每条高优先级发现都做了抽查复核；另外核对了机器环境、CI 状态与三个原生 Agent 的认证。

## 3. 发现与处置

### 3.1 会卡住自主开发的问题（已修正）

| 问题 | 处置 |
|---|---|
| M3–M7 的 5 个 spec 任务都要用户批准 CR，但没有预授权规则，同时又要求“不等用户确认” | CR-003 D1：只细化、不删范围、不降门槛的 CR 经独立 Review 后由 CTO 批准，用户可否决（[docs/21](21-spec-governance.md)） |
| M1-T06、M1-T08、M2-T02 的完成条件依赖排在后面的能力，无法诚实完成 | 任务按 AC 显式拆分归属；端到端 AC 移到 M2-T13，恢复相关 AC 移到 M2-T05/M2-T16 |
| v0.1 依赖被排到 v0.1 之后的 Codex 适配器、团队配置、Commander 监督 | v0.1 改为 M2 全部完成后发布（范围与门槛不变） |
| HTTP/SSE API 在 M6 之前没有任务负责，但 v0.1 的 AC 要调用它 | 新增 M2-T12（Engine host 骨架与本地 API） |
| deployment 检查排在 v0.1 QA 之后，实现它会作废已做的 QA | 新增 M2-T14，放在 v0.1 QA 之前 |
| 任务的祖先关系缺失（如 M7-T04 不依赖 M1-T04/T05） | validate_bundle 新增归属规则：AC 所需检查必须由任务自身或祖先落地；profile 收口任务必须以全部归属任务为祖先。旧任务图在新规则下报出 36 处违规 |
| Pi 当不了独立 QA（会话 ID 抓不到） | agent_task 识别 Pi JSON 模式首行 `{"type":"session","id":…}` |
| 网络抖动导致的自动重试会把完成的会话判为失败 | 只按终态判失败，重试计入 `transient_errors` |
| 独立报告要填的哈希只有会话结束后才存在，Agent 无法生成有效报告 | `agent_task.py --report-out`：会话结束后由桥生成报告并填写身份与哈希 |
| 72h soak 冻结检出目录，另开 worktree 会分叉任务台账 | 台账固定在主 worktree，链接 worktree 共享 |
| `taskboard next` 在已有写任务时仍推荐写任务 | 已修正 |

### 3.2 M2 规格缺口（已修正）

重放状态码（200/202）矛盾、`hoh -p` 参数顺序违规、commands 表缺 kind/payload、候选无状态与报告表、Run 级 BLOCKED 含义与原因优先级、原因码两套写法、ContextHandoff 装不下 Commander 交接包、Claude 取消样例与 CANCELLED 映射矛盾、“未知帧”不可判定、鉴权失败样例缺失、compat manifest 身份、监督触发器表结构、调度与 Commander 输入无 schema、`--resume` 语义、团队配置存储、遗留命名、未定义的配置键。逐项修正见 CR-003 的表格。

### 3.3 门禁漏洞（已加严）

| 漏洞 | 加严后 |
|---|---|
| 一份日志可以给同一检查下的所有 AC 当证据 | 每条 AC 需要场景检查产出的 `test-results.jsonl` 中自己 TEST id 的通过记录 |
| live 检查不核对实际用了哪些 Agent | v1 的 adapter-live 必须含 claude/codex/pi 会话，plugin-live 必须列出已加载插件 |
| 独立报告可在检查结束后改写 | 报告必须是检查声明并哈希的 `independent-report.json` |
| 发布物可以是任意非空文件 | 必须是 build 检查产出的 `hoh-release.tar.gz` |

### 3.4 留给后续任务的事项

| 事项 | 归属 |
|---|---|
| 含 tool_use 的 Claude 成功样例（AC-025-01） | M1-T03 |
| 三个适配器的已知帧登记表 known-frames.json | M1-T07 |
| 录制器加固（M0R3-01..05） | M0-T06 |
| Pi 原生启动时自动安装插件绕过 integrity 校验 | M0-T06 / M5-T03 |
| Web 构建缺失时发布构建失败 | M6-T02 |
| 24.04 的支持声明需要在 24.04 VM 中实测 | M6-T03 / M7-T04（本机为 26.04） |

## 4. 外部工具建议

### 4.1 ECC（affaan-m/ECC）：不安装

ECC 2.2.3 是一套通用的 Agent 增强包（约 71 个 agent、314 个 skill、98 个命令，外加 PreToolUse/PostToolUse/Stop/SessionStart 钩子和持续学习记忆）。不安装的理由：

1. **会污染独立审查。** `agent_task.py` 派发的 Reviewer/QA Claude 会话继承用户级 `~/.claude` 配置，ECC 的钩子、记忆与指令会进入这些会话和协议样例录制。M0 第一轮审查（M0R-02）已经把“操作者钩子和插件混进样例”列为问题。
2. **第二套流程。** ECC 自带计划、记忆、会话摘要，与 HoH 的任务台账、冻结候选、证据门禁重叠，压缩后容易出现两套指令冲突。
3. **上下文开销。** 整个目录会广播给模型，长任务每轮都付这部分 token。
4. **质量来源不同。** 本项目的质量来自 TDD、冻结候选、独立会话审查与证据门禁；CONTRACT 第 10 条也要求不依赖用户额外安装。

### 4.2 dev/setup 全局规范（install-agents.sh）：不安装，可取部分并入项目 CLAUDE.md

该规范的原则（证据先于结论、根因修复、不削弱检查、精准修改、隔离测试并清理）与本项目一致，已并入 [CLAUDE.md](../CLAUDE.md)。不安装是因为四处实际冲突：

1. danger-guard 钩子拦截全部 `sudo`，而 `scripts/bootstrap.sh` 与测试基础设施需要 `sudo -n`，钩子没有放行开关。
2. spec-workflow 以 `~/.agent-specs` 为唯一权威，与 HoH 的 contracts、任务台账、PROGRESS 形成两个恢复点。
3. 第 4 档（并发正确性）要求用户先评审设计，M2 的租约、CAS、epoch fencing 全部属于这一档，会停下来等待。
4. 全局 AGENTS.md 会进入 Codex/Claude 审查会话；它识别子代理靠 `You are a subagent`，而 HoH 的信封是 `[HOH_DELEGATED_TASK]`，审查会话可能被当成顶层任务。

替代做法：项目级 SessionStart 钩子在会话恢复或压缩后注入 PROGRESS 的下一步、运行中的任务与 git 状态，委派会话（设置了 `HOH_BOOTSTRAP_PARENT`）自动跳过（`.claude/settings.json`、`scripts/dev/session-context.sh`）。

## 5. 长期运行的准备

| 项 | 状态 |
|---|---|
| 三个原生 Agent 认证 | Codex（ChatGPT）、Claude、Pi（zai-coding-cn/glm-5.3）均可用 |
| 机器资源 | 12 vCPU、62 GiB 内存、575 GiB 可用磁盘，`sudo -n` 与 docker 可用 |
| 测试基础设施 | 由 `scripts/dev/test-infra.sh` 安装：strace、sqlite3、qemu/KVM（加入 kvm 组）、Chromium/Playwright 依赖、cargo-deny、user linger；测试用 sshd 从发行版包解压到 `.hoh-dev/tools`，不安装系统 SSH 包，不重启、启用系统 SSH 服务；系统 SSH 配置只删除旧版脚本写过的带标记 drop-in |
| 质量门控 | CI：包自检（Python 3.11–3.14）、cargo fmt/clippy/test、cargo-deny；本地：`scripts/check.sh --bundle`、产品检查计划 |
| 会话恢复 | PROGRESS.md + 任务台账 + SessionStart 钩子；`RESUME_PROMPT.md` 按事实重建现场 |
| 额度 | 三家订阅额度会被审查、QA、live 测试与 soak 消耗；撞到 5 小时限额时会话暂停，恢复后按台账续做 |

## 6. 最大的交付风险

1. **规格还有 61% 是 outline。** M3–M7 细化后 AC 预计翻倍，单写者链条严格串行；M6 的 Web GUI 体量接近第二个产品。
2. **外部条件。** 72 小时 soak 的墙钟时间（任何修复都要重跑）、多日 live 额度、新 VM 部署验证。
3. **长会话质量。** 上下文压缩后的连续性依赖 PROGRESS 与台账的及时更新；每个任务都要经过冻结候选与独立审查，不能以“看起来完成”替代证据。
