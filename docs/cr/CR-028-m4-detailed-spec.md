# CR-028 · M4 的工作台、终端与容量规格细化

ID：CR-028
状态：DRAFT
批准人：待 CTO 决定与独立设计审查；本稿没有批准或应用基线变更
日期：2026-11-20
输入基线：`067c241ed7dd1aaba0dd5ee1b0f6aa844414e5e2`，分支 `prep/M4-T01-spec`
适用任务：M4-T01 的准备工作；正式应用仍须等 M3-T05 完成并领取 M4-T01

本稿只新增此文件。拟把 REQ-010、011、012、013、014、015、016、064 从 outline 细化为 detailed，保留全部旧 AC/TEST 标识并扩展为 34 条 AC。需求、场景、OpenAPI、SQL、配置键表、生成视图及任务图的修改均是下文的**应用提案**，本次不修改它们，不 relock、不领取任务、不推进全局台账。文中的 D1–D12 是待 CTO 选择的细化提案；引用这些提案的 AC 是拟议验收标准，不能在批准前当作已经采纳的产品决定。

## 背景与依据

八项需求在输入基线上均为 M4、P1、required、`release_targets=["v1"]`；每项只有 summary 和一条占位 AC，没有 `behavior`、`failure_modes`、`decisions`、`doc_refs`。`docs/02-functional-spec.md` 与 `docs/18-traceability.md` 是这些内容的生成视图，不是另一份事实源。M4-T01 的 `done_when` 要求每个实现任务至多两项需求、八条 AC，并为每条 AC 闭合 host、API、终端和验证能力的依赖。

依据按权威与用途列出，行号会移动，以下以契约名、函数名和章节为锚：

| 来源 | 本稿使用的约束 |
|---|---|
| `CONTRACT.md` 第 3、4、7、8、13、15、17 条，CR-001 | 命令面、模式、单写者与冻结候选、默认单主区、客户端退出不取消、Ubuntu 支持范围、细化不减范围 |
| `docs/03-ux.md` 的 Workspace、视图、Timeline、键盘与终端 | 100/160 列阈值、块与锚点、真实状态、键位、NO_COLOR、鼠标增强、ASCII 回退 |
| `docs/01-prd.md` 非功能目标；`docs/07-algorithms.md` §8 | 4 vCPU/8 GiB/本地 SSD 参考 VM；输入 p95≤100 ms、事件可见 p95≤500 ms、idle Rust core RSS≤150 MiB；百万事件分页；seq 排序、幂等投影、约 50 ms 合并绘制 |
| `docs/04-architecture.md`、`docs/05-data-api.md`、`docs/14-resilience.md` | 客户端经 host 查询、事务水位、对象文件、背压、低空间保护、被引用证据不自动删除 |
| `docs/09-testing.md`、`docs/10-acceptance.md`、`tests/check-plan.json` | 每 AC 一个 TEST 和指定 check；product/mock/live 分离；同候选的独立 QA/Review |
| `docs/19-ai-development.md`、`docs/21-spec-governance.md`、CR-003 D1 | 委派边界、审查与预授权条件；草案不代表批准 |
| CR-026，读取自 `/home/ai/dev/hoh-wt/m3-spec/docs/cr/CR-026-m3-detailed-spec.md`，该工作树 HEAD=`8cd7b715cc9b78c0178c7c1ce29540eb6f1dd3a5` | M3 已批准、尚未实施；下节逐项列出 M4 必须等待的契约 |
| CR-025，`docs/cr/CR-025-budget-risk-role-binding-pending-gate-date-time.md` | `budget.risk`、`ROLE_BINDING_PENDING` 的真实含义；沿用“背景→决定→AC/场景→影响→验证”的格式 |
| CR-024，读取自 `/home/ai/dev/hoh-wt/cmdro/docs/cr/CR-024-commander-read-only-profile.md`，批准提交 `fa7dbca2` | Commander 只读档与公开最终消息决策；输入基线/main 中没有该文件，不能声称已经合入或实施 |

没有采用外部 TUI 产品的行为作为 HoH 决定。本稿未运行参考仓库的代码。

### 当前实现与需要补齐的部分

1. M1-T08 的 `crates/hoh-app/src/tui.rs`、`tui/app.rs`、`tui/input.rs`、`tui/terminal.rs` 已有 Ratatui/Crossterm 外壳、信号恢复、历史、括号粘贴解码。`tui/editor.rs` 明确是单行编辑器，当前将换行替换为空格；多行输入属于 M4。输入读取是可中断的自有解码器（ADR-014），不能为支持鼠标改回不可取消的阻塞读取。
2. M2-T12 的 `host/http.rs::stream_events` 以 DB 水位和每批 500 行实现 SSE，`Last-Event-ID` 与 `after` 冲突为 409。当前路由有 runs、getRun、team、acceptance、issues、events；OpenAPI 已声明的 tasks、evidence、timeline、artifacts 并非都已有路由。M3-T07 承诺补 tasks 与追踪；其余缺口在本稿分派，不把“有 OpenAPI”当成已实现。
3. M2-T18 的 `persistence/snapshot.rs::AcceptanceView` 与 `getRun.projection.acceptance` 中 accepted/total 是 **AC** 计数，且 accepted 只有当前候选被接受时才非零。状态栏需要需求计数，不能给这个旧字段换标签。Issue 状态来自 ADR-029 的状态函数，不能只读取可能过时的显示列。
4. 当前 `tui.rs::Conn::link` 使用无界消息通道、首次从 seq=0 读；`App.messages` 为增长的向量。这些是 M4 容量改动的实际入口。新工作台须从水位快照和按需页面启动，不能先把百万历史事件全部读进客户端。
5. 当前配置加载器仅接受 `ui.statusline="standard"`；`ui.view` 为 workspace/team/focus，已有 `ui.mouse`。minimal/custom、键位覆盖和下文容量键须与加载器一起交付，不能提前写进 example 导致 REQ-018 回归（CR-026 的 CR26D-103）。

## 已确定的决定与 M3 契约

以下不再作为 CTO 问题：

| 契约 | M4 必须保持的行为 | 提供任务 |
|---|---|---|
| CR-026 REQ-007：`SetMode`、`run.mode_change_pending`、`run.mode_changed`、`[ui] mode_cycle` | 模式与 Run 状态正交；pending 不能提前显示为已生效；Shift+Tab 与斜杠共用命令；不增加 Shell 子命令 | M3-T09 |
| CR-026 REQ-017：`SendInput`、`input.delivered`、QUEUED/DELIVERED/APPLIED/REJECTED、boundary、role_epoch | 输入默认给 Commander；@role 与含糊目标选择器；显示实际目标、turn/next_attempt；本地按键或 HTTP 202 不等于 APPLIED | M3-T09 |
| CR-026 REQ-034：`docs/spec/index.json`、scenarios、ApproveSpec | Markdown 为用户规格权威；index.json 字节摘要为规格身份；M4 只读这些事实，不建立另一个规格索引权威 | M3-T02 |
| CR-026 REQ-033/O6：`spec.changed`、`candidate.stale`、全部旧验收失效 | 新规格使所有旧验收退出当前计数，历史仍可查；不能只扣除变化的需求 | M3-T02 |
| CR-026 REQ-032：REQ→AC→TASK→candidate→TEST→EVIDENCE→acceptance | 详情链接使用产品追踪；broken/orphan/stale 明示；用户项目不需要 `contracts/` 目录 | M3-T07 |
| CR-026 REQ-008/O1/C22：spec_draft Task、`spec_approval`、ApproveSpec 来源判定 | 起草、批准、独立审查分开显示；只有用户批准；没有规格时 0/0 不代表完成；不令运行中的 Run 退回 SPECIFYING | M3-T04 |
| CR-026 REQ-031：`spec-review-report.schema.json`、`spec-review-attempt.schema.json`、`spec_review.opened/reported` | 规格审查不是候选验收，不能把它的 PASS 计入 accepted；带 critical/high 的报告不满足 readiness | M3-T06 |
| CR-026 REQ-028：`run.baseline`；REQ-040：last_progress 与 no_progress | 基线和最近进展来自事实；进程 alive 不等于思考或进展 | M3-T07、M3-T10 |
| REQ-018/019/020、CR-025 | 职位不等于 Agent/模型/会话；pending checkpoint 与 ROLE_BINDING_PENDING 保留；换绑不能把旧会话归到新 epoch | M2-T07、M2-T17、M2-T21 |
| REQ-043、ADR-014/021/031 | Esc 关临时层；Ctrl+C 非空编辑器先清空；顶层空编辑器显示脱离/暂停/取消选择；SIGTERM、SIGHUP、关闭客户端不取消后台任务 | M1-T08、M2-T06、M2-T13 |

M3-T05 未完成时不领取本稿的实现任务。M4-T01 应用时逐项重查这些提供者，若其最终契约与此表不同，先修订本稿并审查；不能以兼容性猜测代替依赖。

## 决定提案

### 1. 共同约定：读模型、身份与分页（D1、D3、D4）

- UI 读模型绑定 `project_id/run_id/spec_id/spec_sha256/candidate_id/watermark`，无对应事实时字段为 null。数据只由 Persistence 的只读事务和已提交事件产生。客户端自己的选择、展开、搜索、输入草稿属于本地状态，不产生工程执行命令。
- 启动：取快照 H，加载不超过 H 的最新一页摘要，订阅 SSE `after=H`；历史通过页面向前取。事件按项目 seq 排序；过滤之后 seq 可以有空隙，不能误判为丢事件。重复 seq 不重复应用；重连先补 `(last,H]` 再 live `>H`。用户界面标明“重连中/数据可能过期”和最后已应用 seq，补齐之前不清除此标记。
- 快照响应先完整校验再替换，禁止将 H1 的计数、H2 的候选拼成一个“当前”状态。跨页 cursor 包含查询条件、H 和派生索引 generation；新事实通过 SSE/新快照处理，不漂移旧页。服务端拒绝篡改、跨查询或超过已提交水位的 cursor；派生索引重建提交时才递增 generation，旧 generation 为 CURSOR_EXPIRED，客户端重取快照再续接，单纯重启不递增。已有 SSE 对矛盾 after/Last-Event-ID 的 409 保留；SSE 的 seq cursor 与查询页 cursor 不混用，不以丢弃权威事件来制造过期测试。
- 稳定身份提案：事件块 `event:<project_id>:<seq>:<part>`；实体块 `entity:<type>:<id>`，历史实体另带 spec/candidate/attempt 身份；分片 part 从 0 起、以存储时确定的记录边界划分。滚动锚点是 `(block_id, grapheme_offset)`，不使用会随流式追加改变的绝对屏幕行号。同一公开消息追加保持 block_id。
- 未知 kind 保留 seq、kind、公开摘要及诊断，不 panic、不把它解释为成功。未知字段按现有兼容规则处理；隐藏推理、原始凭证和内部 Prompt 不属于 Focus 的“完整会话”。Focus 完整指适配器已公开的用户/助手消息、工具调用与结果、交接和错误，含明确的缺失区间。
- 容量单位是字节（KiB=1024、MiB=1048576），宽度是终端显示列，字符编辑单位是 Unicode grapheme。分页 limit 沿用缺省 100、合法 1–500；新客户端最多缓存 1000 个块描述、4 MiB 正文、2 个在途页面请求；每页正文最多 256 KiB。达到上限先逐出不在当前视窗/锚点的正文，锚点身份可保留，返回时重新取页。不因缓存逐出丢掉服务端事实。

### 2. 查询契约的拟议补齐（D1、D3、D7）

批准后在 `contracts/openapi.json` 的命名 schema 中明确下列字段与错误响应；旧字段和旧路由语义保持。所有读取仍经过本地 token、Host/Origin 检查，不允许 UI 直接读写 SQLite。

| 位置 | 拟议字段/行为 | 实现归属 |
|---|---|---|
| `getRun.projection.ui`（可加字段） | `spec{id,revision,sha256}`、`baseline{commit,source}`、`run_baseline`、`task{task_id,phase}`、`last_activity`、`input_states[]`、`team_revision`；顶部 baseline 沿 ADR-031，取项目 COMPLETED Run 中最近 ACCEPTED 候选的 commit，没有则显示“无”；M3 的 run.baseline 独立放 run_baseline 详情，不能替换顶部含义。值来自同一 H，数组分页引用而非内嵌全部历史 | M4-T02，消费 M3 输入/基线契约 |
| `getRun.projection.progress`（可加字段） | `spec_id/spec_revision/candidate_id/requirements_total/requirements_accepted/requirements_degraded/issues_open/issues_reopened/scope_added/scope_removed`；保留原 `projection.acceptance` 的 AC 计数 | M4-T06 |
| `GET /v1/timeline`（新增）与现有 `/v1/runs/{run_id}/timeline` | 前者覆盖当前 host 的项目，后者固定该 Run；level=project/iteration/agent，新增可选 iteration_id、role、attempt_id、before_seq、watermark；limit/cursor 沿用 Page。过滤条件交集，跨 Run 的 iteration/attempt 拒绝；项目级无 run_id 的事件不会伪装成某一 Run 的事件 | M4-T03 交付索引/查询，M4-T06 完成三级映射与客户端 |
| `TimelinePage`/`BlockDescriptor`（新增命名 schema） | Page 的 items/next_cursor/watermark；每块 block_id、seq、kind、run_id、iteration_id、role、role_epoch、attempt_id、spec_id、candidate_id、summary、refs。refs 是带类型的 requirement/issue/decision/commit/evidence/artifact 标识；没有的字段为 null，不能由 Agent 名反推角色 | M4-T03 |
| 已声明的 tasks/evidence 路由 | tasks 以 M3-T07 的输出为准；evidence 补分页与候选/TEST/ArtifactRef 链。只读查询，不制造验收结果 | M3-T07；缺失 evidence 路由由 M4-T03 |
| 已声明的 `/v1/artifacts/{artifact_id}` | 仅已注册 ID；增加单一 byte Range 的 206/416、Content-Range、ETag（sha256）、Accept-Ranges；无 Range 时仍为原 200 流式下载，不缓冲全文件。缺失 404，摘要不符 409，繁忙/磁盘问题 503，保留 403 | M4-T03 |
| `GET /v1/artifacts/{artifact_id}/search`（新增，D7） | 字面 UTF-8 搜索，不执行正则；query 长度 1–256 字节，limit 缺省 100、最大 100；返回递增 byte_offset、匹配长度、最多 256 字节上下文与续查 cursor。正文流式扫描，单次最多扫描 8 MiB，更多返回 continuation；无匹配返回空列表；非文本 422 | M4-T03；M4-T05 使用 |

查询不能加载全部原始 transcript。块正文经 artifact 引用按需读取；实时公开增量来自已提交的小事件，尚未封口的 segment 不作为可验收 ArtifactRef，封口后登记不可变对象及内容摘要，原消息 block_id 保持不变。二进制对象只能看 metadata/下载，不把二进制控制序列原样写到终端。M4-T03 交付 typed API 和相关 SQLite 派生索引，不依赖 M6 Web。M6 后续客户端复用这些契约。

**拟议原因码**：追加 ErrorCode `QUERY_INVALID`（400，新查询参数/cursor 错误）、`ARTIFACT_NOT_FOUND`（404）、`ARTIFACT_INVALID`（409，大小/摘要不符）、`ARTIFACT_NOT_TEXT`（422）。Range 越界用 416 + `QUERY_INVALID`，并给 `Content-Range: bytes */<size>`。`CURSOR_EXPIRED`（409 + details.resync=true/snapshot_watermark）、DB_BUSY/DB_FULL（503）沿用现有码；新错误响应列入 OpenAPI。客户端诊断 `UI_CLIPBOARD_UNAVAILABLE`、`UI_INPUT_TOO_LARGE`、`UI_KEY_CONFLICT`、`UI_UNKNOWN_EVENT` 是本地显示状态，不擅自加入 HCP 的 Run 状态或 BlockedReason。

**拟议受锁持久化变更**：在 M3 的 `002_spec_drafting.sql` 之后新增下一顺序迁移（预期 `003_ui_observability.sql`，应用时确认编号），只增加派生查询索引、公开日志 segment 的登记及对象引用索引；不改写 001/002，不新增验收真值列。索引键至少含 `(run_id,seq)`、`(iteration_id,seq)`、`(attempt_id,seq)`、`(role,seq)`；事件到这些键的映射从已提交 Task/Attempt 身份确定，不能从文本猜。日志段登记至少有 artifact_id、attempt_id、stream、first_offset、bytes、sha256、sealed、created_seq；引用索引以 `(owner_kind,owner_id,artifact_id)` 去重。它们可由权威事件/工件登记重建；重建时旧视图标 stale，完成前不对外给出“完整”结果。新增迁移须含外键、唯一约束、升级失败回滚与旧版本拒读测试，按 REQ-062 和 CR-026 的迁移运行器处理；不借此实施 M5 的备份产品。

### 3. 状态与进度提案（D2、D11）

1. `m` 是当前已批准 spec 中的全部需求数，不是 Task 数、AC 数，也不是仅已排进当前 Run 的需求。没有规格时显示“未批准规格，0/0”，不显示 100%。`n` 只计当前附着 Run 的最新候选上、身份与该 spec 一致、全部 AC 被有效验收且候选已 ACCEPTED 的需求；不拼接不同候选或不同 Run 的 PASS。尚未纳入任务的需求仍在分母中，分子为 0。历史/跨 Run 的通过可查，但不借给当前候选。
2. required 需求全部 AC 为 PASS 才计入；degradable 只按现有 release-policy 的合规规则满足，另显示 `degraded=k`，不得写为全部 PASS。无 AC、证据失效/缺失、orphan、UNKNOWN、BLOCKED、FLAKY、SKIPPED 均不计。客户端调用 host 的验收投影，不重写 Gate 规则。
3. 新候选未 ACCEPTED 或规格变化时 n=0；显示新 spec revision 与新增/删除的需求 ID 差集数量。删除的范围只显示已批准的变更，不由 UI 修改。旧计数保留在历史详情；`spec_review` PASS、TaskResult COMPLETED 和模型“100%完成”不增加分子。
4. `open` 只计状态函数得出的 OPEN，`reopened` 只计 REOPENED，避免重复计数；按当前 Run 的唯一 issue_id 统计。状态栏阻塞数取 Run/Task 的持久阻塞，不把等待服务、排队和离线混为失败。
5. 默认优先序：mode（含 pending）、阶段/任务、n/m accepted、blocked/open/reopened；随后 Git 分支/候选、运行时间、预算。Focus 才显示该职位当次 Attempt 的 Agent、实际解析模型、上下文；配置模型不冒充 resolved model。成本与上下文未知显示“未知（role/agent，原因）”，订阅调用不推算美元费用。已知部分成本显示“已知部分”，不是总额。
6. 活动分类依据显式工具开始/结束、服务等待、原生压缩、调度排队、终结和最后活动事实。只有进程存活时显示“进程存活；活动未知”；没有在途监督的 Commander 显示“事件监测中”。模型摘要注明“分析”，不能覆盖确定性状态。缺上下文上限、缺已用量或计量身份不匹配时百分比为 null；不从字符数估算 token。

### 4. 布局、动作、输入与配置提案（D7–D10）

Workspace 保持默认单主区，团队摘要可折叠；Team 显示全部配置职位的状态卡，不把六职位误做六个常驻进程。Focus 以 `(run_id,role,role_epoch,attempt_id,session_id)` 定位历史公开会话，换绑后不自动把旧消息换成新持有者。布局阈值保持 docs/03：<100 列单区/标签，100–159 列主区加一个详情区，≥160 列允许三列，分屏必须由用户选择。80/120/180 是必测宽度，另测 99/100/159/160 边界；<80 列或高度<12 行显示紧凑提示与可操作的输入/退出入口，允许省略可从状态页读取的信息，不丢草稿。

动作使用稳定 action ID，不新增 Shell 命令。拟定公共操作：workspace/team/focus/timeline、task/issue/evidence 抽屉、选择角色/会话、展开/折叠、复制、搜索/下一处/上一处、全屏/返回、状态详情、设置、命令面板、历史搜索、模式切换、提交、暂停/恢复/取消/脱离。Enter 在编辑器提交、列表展开；Esc 只退一层；Ctrl+R 保留历史搜索；`?` 在编辑器是文字；Ctrl+P 命令面板。列表默认 `Space` 折叠、`f` 全屏、`y` 复制、`/` 搜索、`n/N` 下一/上一处；这些只在列表 scope 生效，命令面板提供相同动作。鼠标单击选择、双击展开、滚轮滚动，动作按钮可单击；禁用鼠标后所有动作仍可键盘执行。

复制提案（D7）：默认把选定公开块的全部纯文本流式写入客户端项目的 `.hoh/exports/<sha256>.txt`（临时文件→fsync→rename），返回可复制路径和 sha256；不包含边框、颜色码或隐藏字段。这是明确的“复制到文件”，不是谎称已进入系统剪贴板。设置为 osc52 后才向终端发送 OSC 52，最多 64 KiB UTF-8，显示“已发送复制请求（终端未确认）”；不支持、超限或写失败时保留块/草稿，并提供复制到文件。读取仍经 host 的 Artifact API，UI 不写 host 状态表。导出错误显示路径与原因，不记成功。默认不自动启动外部编辑器、浏览器或 shell。

多行输入把 CRLF/孤立 CR 规范成 LF；LF 保留在提交正文。完整 bracketed-paste 结束之前发送次数为 0，结束之后仍须用户 Enter 才提交。没有增强键盘协议时 Ctrl+J 或 Esc+Enter 换行；沿用 ADR-014 的 ESC 等待规则，不保证超出该超时的独立 Esc 和 Enter 被识别为组合键。Shift+Enter 只在可识别该组合的终端中作为增强，不能成为唯一多行入口。提案上限是编辑器 UTF-8 正文 1 MiB，超出时拒绝整次插入/粘贴而保留原草稿，消费完 paste 结束标记，不把余下粘贴字节当作命令。paste 内的控制字节仍是数据，不能把其中的 Ctrl+C 字节猜成用户取消；未完成粘贴期间，终端挂断、外部 SIGINT/SIGTERM/SIGHUP 仍进入原有退出处理，不因缺结束标记无限增长内存。

配置拟追加/细化 REQ-018 的键表，所有缺省值与 example 在加载器支持的同一提交进入，均重跑 TEST-018-01；不是 M4-T01 提前改变可运行 example：

| 配置 | 合法值、缺省与作用域 | 交付 |
|---|---|---|
| `ui.view` | 保留 workspace/team/focus，增加 timeline；缺省 workspace；只影响客户端初始视图；没有可用 Focus 身份时显示角色选择器 | M4-T02 基础，M4-T06 增加 timeline |
| `ui.statusline`、`ui.statusline_fields` | standard/minimal/custom，缺省 standard；custom 的 fields 为不重复数组，0–8 项，从 git/candidate/elapsed/budget/agent/model/context/activity 选；必显四组字段不允许移除，fields 只排列可选字段 | M4-T06 |
| `ui.mouse`、`ui.icons` | 保留 mouse=true；icons=auto/ascii，缺省 auto；NO_COLOR 非空只禁 SGR 颜色，仍允许焦点反显等非颜色属性 | M4-T08 |
| `ui.clipboard` | file/osc52，缺省 file；仅用户显式复制时生效 | M4-T05 |
| `ui.keybindings` | 数组 `{action,scope,key}`，缺省空数组；scope 为 global/editor/list/dialog；空数组即内置映射。替换同 action 的缺省键，拒绝不可识别 key/action/scope、同 scope 同 key 的两个 action，以及 global 与任一活动 scope 的冲突 | M4-T08 |
| `observability.log_segment_bytes`、`log_segments` | 整数 1–64 MiB，缺省 8 MiB；整数 2–64，缺省 5；仅未被引用的诊断日志轮换 | M4-T03 |
| `observability.orphan_grace_seconds`、`min_free_bytes` | 整数 3600–604800，缺省 172800（48 h）；整数 16–1024 MiB，缺省 32 MiB；保守清理与写入预检 | M4-T03 |

键位保存前整体校验，不部分生效；恢复默认只删 ui.keybindings 覆盖，不动团队/预算/Prompt。原有 Ctrl+C、Esc、Enter 的上下文含义保留，对这三键的覆盖提案直接拒绝。Ctrl+P 打开面板的动作按 docs/03 **可以重绑**：替换后必须仍有一个无冲突的 global 键可打开命令面板，面板内始终有“恢复默认键位”；无入口的映射整体拒绝。跨互斥 scope 的同键可用；global 与局部重叠则冲突。非法配置仍走 REQ-018 的路径/键诊断，不静默回退。

### 5. 容量、日志、证据保护与测量（D4–D6）

结构化事件与小摘要进 SQLite，大正文进对象文件；事件 payload 的公开正文超过 16 KiB 时转为 artifact 引用，不截掉 tool/start/end、decision、acceptance 等关键事实。原始日志保留完整字节；终端渲染转义 ESC/OSC 等控制序列，不能让工具输出伪造 UI 或执行终端命令。迁移旧大 payload 时保留原始事实，用派生索引外置，不 UPDATE/DELETE 旧 events/evidence/acceptance。

普通诊断日志按 8 MiB 轮换、保留 5 段（D5 的缺省）；写入按字节拆段，每段不超过上限。证据、候选、发布、追踪节点、公开会话块和仍活动 Attempt 引用的对象全部保护，包括历史 STALE/INVALID/REJECTED 候选的引用；超过日志配额只标“证据占用”，不删除或改写它们。v1 的本提案不删除权威 events/验收历史，只清理没有任何引用、封口且年龄达到 48 h 的对象与未登记临时文件。创建、登记、引用与 GC 的最后一次可删判定使用同一 Persistence 写序列协调；对象读取持有短期 pin，正在读的对象不回收。清理在取可删集合后必须复查，不能用一次过时扫描删除刚被引用的对象。引用无法可靠解析时保留并诊断，不能假定无引用。

大对象写入前需要 `available_bytes >= object_write_bytes + min_free_bytes`。不足时不派发新的写 Attempt、不开始该对象写入，暴露 DB_FULL/低空间诊断；已有已提交状态继续可读。确实满盘导致状态事务不能提交时，Pause/Cancel 也允许返回 DB_FULL，UI 必须显示未持久接受，不能宣称已取消；终端仍可脱离。释放空间后显式重试或持久重试机制恢复，未确认对象写入先 reconcile。对象写完但登记前崩溃的文件是孤立对象，48 h 内保留用于恢复；有已登记引用的文件无论年龄都保留。

测量沿用 docs/01 的参考机器与 100/500 ms 门槛；M4 是相关 UI/容量验收，不能代替 M7 性能与 soak。D4 提议本地分页读取/块首屏 p95≤500 ms；每个样本有单调时钟起止及输入/水位标记，至少 100 次，不以平均数代替 p95。输入延迟测按键写入 PTY 到解析后的屏幕出现标记，事件延迟测事务 commit 到屏幕对应 seq。测试记录构建模式、CPU/内存/文件系统、Rust/终端/tmux/ssh 版本；不符合参考环境记 BLOCKED/NOT_RUN，不能删样本或调大门槛。百万事件灌库不计入交互耗时，但必须用真实产品 Persistence 建立可读数据，不用预画屏幕充当产品查询。

## 各需求的细化与 AC→测试映射

应用规则：每项保留 id/category/priority/milestone/title/required_release/release_class/release_targets 和原 summary；改 `spec_maturity=detailed`，把本节“行为/失败”分别转为 behavior/failure_modes，把引用的提案与已定事项放入 decisions，补 doc_refs。AC 表的输入→操作→判定分别是 given/when/then，同号 TEST 为 test_id。每条表行同时定义 `acceptance-scenarios.json` 中的 fixture/steps/pass_criteria；observables 至少包含下列该行要求的屏幕/命令/HTTP/DB/原始日志，不能只保存截图。

每个 TEST 恰归一个 check_id。tui-pty 场景放进该检查实际运行的 `crates/hoh-app/tests/tui_pty.rs`（可调用公用 TestBackend 断言，但正式结果来自此 target）；unit 在 lib/bin 单元测试，integration 在 tests target，recovery 由既有 recovery runner 收集。名称含 `test_NNN_MM`，原始产物写在本检查 `$HOH_EVIDENCE_DIR` 下。表中均为拟执行的 **product** 测试；FakeAgent/固定事件夹具须在记录中标明，不能声称 live。现阶段全部 NOT_RUN。

### REQ-010 · Workspace 工作台（4 条）

行为：顶部显示项目、已生效 mode、Run、基线身份；正文是统一的公开对话与工程活动，团队摘要可折叠；底部输入和 statusline。pending、输入四态与断线标记由 host 事实驱动，202 只说明接受。选择视图、展开和连接本身不调度任务。失败：API 断连保留带 stale 标记的最后快照与草稿；409 不重放用户命令为新动作；未知事件可见且不崩溃。

doc_refs：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/adr/ADR-014.md`、`docs/adr/ADR-031.md`；M3 依赖 REQ-007/017/028。提案 D1、D4、D11。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-010-01 / TEST-010-01 | tui-pty | 120×32 PTY；R 的三种 Agent 分别承接 commander/developer/qa，已提交水位 H，项目既有 COMPLETED Run 最近 ACCEPTED 候选 commit=B，当前 R 的 run.baseline=B0≠B，mode=BUILD；公开用户消息 U、助手消息 A、task/candidate 活动各一条 | 打开 Workspace，折叠/展开团队摘要，打开当前 Run 基线详情；对同快照作 TestBackend 与 PTY 文本断言 | 同屏可见项目/R/B、BUILD、U/A/工程活动、输入框与状态栏，当前 Run 详情显示 B0，不替换顶栏 B；只一个主活动区；三职位摘要来自 team；折叠仅隐藏摘要；两种后端字段值一致；打开/折叠产生的 POST、Task/Attempt 新增均为 0 |
| AC-010-02 / TEST-010-02 | tui-pty | 写 Attempt 运行中，M3 的模式与输入投递可控 | `/plan`；普通输入发给 commander，先接受再 delivered/applied；另一个输入被拒；模拟一次 expected_revision 409 | pending 时仍显示 BUILD 和待 PLAN，安全点后才显示 PLAN；两个输入按 ID 显示 QUEUED→DELIVERED→APPLIED、QUEUED→REJECTED，保留拒绝原因；显示实际角色/epoch/boundary；409 不改 mode，不自动重发新命令 |
| AC-010-03 / TEST-010-03 | tui-pty | 快照 H=100，草稿“继续核对”；连接在应用 seq=105 后断开，期间 host 提交到 110 | 断线、恢复，重复交付 105；再重建派生索引，以旧页面 cursor 得到 CURSOR_EXPIRED 并重取 H=110 | 断线显示 stale/105，草稿不变；补齐后同块仅出现一次，已应用水位=110，才消除 stale；过期重建不丢选择锚点；无新增 SendInput/StartRun，无进度乐观递增 |
| AC-010-04 / TEST-010-04 | unit | 投影输入为已提交 1..20，含未知 kind、含控制字符的公开输出、已知 pending/input 事件；另有未提交事件 X | 一次重放与分批重放（含重复 5、10）比较；不将 X 交给提交消费者 | 规范化投影逐字段相同、20 个不同 seq；未知 kind 有 UI_UNKNOWN_EVENT/seq，X 不在投影；公开正文中的 ESC/OSC 转义为可见文本，不能变成控制指令；pending 不被解析成已生效 |

### REQ-011 · Team/Focus 布局（4 条）

行为：Team 按配置职位展示绑定、epoch、可用性、当前任务与 pending；Focus 按完整身份查看公开会话，历史按需分页。布局转换保留草稿、展开状态和 grapheme 锚点。失败：角色删除或历史内容缺失显示缺失身份与返回入口；同 Agent 多职位不自动选择；窗口过小不 panic，也不修改执行状态。

doc_refs：`docs/03-ux.md`、`docs/04-architecture.md`、`docs/adr/ADR-022.md`、`docs/adr/ADR-024.md`、CR-025；M3 依赖 REQ-017。提案 D1、D9–D11。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-011-01 / TEST-011-01 | tui-pty | 默认 Workspace；六职位，cto/developer 同为 claude；选 developer，块 K 的 offset=12，草稿“尚未发送” | 依次 resize 为 80/99/100/120/159/160/180 列、32 行，选择 Team 分屏，再返回 Workspace | 80/99 为单区标签；100–159 最多主区+1 详情；160/180 用户选择后最多三列；默认 Workspace 不强制三窗；选择身份、K/12 与草稿保持；无 POST，所有行显示宽度≤窗口 |
| AC-011-02 / TEST-011-02 | tui-pty | 加自定义 security_reviewer，共七职位；developer epoch=1 的 A1 在途，换绑 codex 待 checkpoint | 打开 Team、Focus A1，结束 A1 并生效 epoch=2/A2 | 七职位均可访问；pending 显示旧 claude/A1 与待 codex，不假称生效；历史 Focus 保持 A1/epoch1，选择“当前”才显示 A2/epoch2；两个会话内容不混合；ROLE_BINDING_PENDING 原因可见 |
| AC-011-03 / TEST-011-03 | integration | 同一 agent 的两个 role、三个 session，A1 有公开 message/tool/handoff/error 及一项非公开内部字段；一段公开日志缺文件 | 按 A1 查询 Focus 各页，再查询另一 role；请求缺失 artifact | A1 的所有公开记录按 offset/seq 可遍历且不混入其他 role/session；内部字段不在响应；缺失段返回 ARTIFACT_NOT_FOUND，descriptor 保留身份和缺失提示，不显示为空成功；无 spawn/任务写入 |
| AC-011-04 / TEST-011-04 | tui-pty | 80×24，角色选择器与输入草稿各有状态 | resize 到 40×8，再 1×1、回 120×32；选择一个已移除的历史角色 | 无 panic、无越界；恢复后草稿、所选历史身份和锚点不变；小窗口可退出，历史角色显示“已移除/历史”与返回入口；后台 Run/Attempt revision 未因 resize 改变 |

### REQ-012 · 可展开内容块（4 条）

行为：message/task/tool/diff/test/evidence/decision/handoff 块都有稳定身份；Task/Tool/Diff/Test/Evidence 必须支持展开、复制、搜索、全屏与返回。键盘与鼠标调用同一动作。展开只读，不执行工具或测试；结果引用候选、TEST 与原始证据。失败：缺失/损坏/非文本/权限错误在原块展示原因，保留返回和重试；旧候选标历史/失效，不自动切到新候选同名文件。

doc_refs：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/10-acceptance.md`；M3 依赖 REQ-032/034。提案 D1、D4、D7、D9。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-012-01 / TEST-012-01 | tui-pty | 五种必测块各一，每块首尾含标记 X，其中 Tool 含 20 MiB 文本；Evidence 绑定 C1；父列表选块 K/offset=200 | 各以键盘和鼠标执行展开→全文搜索 X→全屏→返回至父列表→复制到文件，期间追加另一块 | 返回仍为 K/200；首尾匹配均可达；导出字节等于公开纯文本、sha256 相同；大块首屏 100 次 p95≤500 ms，未全量加载；两种输入方式选到相同身份；工具/测试执行新增=0 |
| AC-012-02 / TEST-012-02 | integration | 1 MiB+17 字节 UTF-8 对象，匹配“中文X”跨 256 KiB 页面边界；末尾另一处；二进制对象 B | 分 Range 取全文件；字面搜索并翻页，搜索无匹配词；请求 B 的搜索 | 拼接字节与注册 hash 相同、无缺字重字；两处匹配 byte_offset 精确，返回合法 UTF-8 上下文；无匹配返回空且扫描完毕；B 返回 422 ARTIFACT_NOT_TEXT；不把分片边界误作字符边界 |
| AC-012-03 / TEST-012-03 | tui-pty | 依次准备缺文件、摘要不符、403、503 的 Evidence；旧 C1 的文件名与新 C2 相同；65 KiB 文本 | 打开/重试/返回；复制到不可写路径；选择 osc52 复制超限文本 | 分别显示缺失、损坏、拒绝、可重试原因；不显示 PASS/空成功；C1 永不替换为 C2 内容；导出失败无成功消息；OSC52 超限不发送正文并可切文件复制；草稿与父锚点不变 |
| AC-012-04 / TEST-012-04 | unit | 八类块、100 次追加、10 次 resize，焦点为第三块；正文含中文/组合字符；选中块被过滤或其对象回收 | 对展开/折叠/全屏/返回/搜索状态作纯动作序列；重复同一提交事件 | block_id 与 grapheme_offset 不因前插/重复事件漂移；offset 超过新正文时钳到最后字素边界并提示；被过滤保留隐藏锚点，取消过滤可返回；不可恢复块明确提示并选 seq 最近的前驱（无前驱选后继），不跳到无关 Run |

### REQ-013 · 三级时间线（4 条）

行为（D3）：Project 看当前项目跨 Run 的规格、候选、Issue、决定、轮次结果和 Run 生命周期；Iteration 看选中 Run/iteration 的工程事实（含 Task、候选、测试、证据）；Agent 看明确 role/epoch/attempt 的公开消息、工具与交接。项目级事实没有 iteration 时不塞进某个轮次。按 seq 排序，时间戳仅展示。每项可沿类型化引用定位需求、Issue、Decision、commit、Evidence；保留过滤条件和返回锚点。

失败：非法过滤/cursor 明确拒绝；合法无匹配显示空；未知事件有诊断块；broken/orphan/stale 引用不能伪装成可用证据。doc_refs：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`contracts/openapi.json`；M3 依赖 REQ-032/033/034。提案 D1、D3、D4。

固定三级场景 F13（产品事件夹具，不伪造 live）：同一项目两 Run R1/R2，两 iteration I1/I2；seq 1 为 R1 的 spec.changed，2 为 I1 的 Task 派发，3/4 为 I1/A1 的公开消息/工具结果，5 为 I1 的 candidate.frozen（commit=B），6 为 I1 的证据登记，7 为 I1 的 issue.opened，8 为 I1 的 decision.applied，9 为 I1 的轮次结束，10 为 R2 的 Run 创建，11 为 I2/A2 的公开消息，12 为 R2 的未知事件。除 3/4/11 外这些事件不带 attempt 过滤身份。事件的实际 kind/payload 沿用所属生产者的 ADR；上述描述是 fixture 的语义，不另发明第二套生产事件名。为新公开输出定义的 segment 事件须在 M4-T03 的 ADR 列出到该表的映射。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-013-01 / TEST-013-01 | tui-pty | F13，另有 REQ/issue/decision/commit/evidence 的完整追踪引用；120×32 | 依次选择 Project、R1/I1、A1；打开五类引用再返回；seq=8 后断线，补到 12 | Project 可见 [1,5,7,8,9,10,12]；I1 可见 [2,5,6,7,8,9]；A1 可见 [3,4]；五类详情的 ID/spec/candidate/hash 与来源一致；断线前后最终列表、过滤条件和锚点与连续读取相同 |
| AC-013-02 / TEST-013-02 | integration | F13 中时间戳有重复且 seq=8 的时间早于 7；limit=2，H=12；随后追加 seq=13 | 每个 level/过滤器翻完页；重复 cursor；改 cursor 的过滤条件和水位；空匹配过滤 | 页内/跨页按 seq 严格递增且并集等于 F13 预期、无 13 混入 H=12；重复 cursor 返回同页；不匹配 cursor 返回 400 QUERY_INVALID；空结果为 items=[]、next_cursor=null，不是错误 |
| AC-013-03 / TEST-013-03 | integration | 一个缺 Evidence 的 TEST 链、一个旧 spec 的 PASS、一个已失效候选，另有合法链 | 通过 Timeline refs 逐项查详情；批准新修订后再查旧事件 | 分别显示 broken（缺 evidence）、orphan/历史、stale，保留原身份和原 seq；合法链可正反定位；修订后旧事件仍在且不计当前 accepted；不重新生成证据或执行检查 |
| AC-013-04 / TEST-013-04 | unit | F13 的持久事件、重复 4/9、未知 12；两种交付分批及同一 H 快照 | 比较全量折叠、快照+增量、断线补历史三种规范化投影；切过滤再切回 | 块序列/refs/身份/诊断一致；12 恰一诊断；切过滤不改变事件真值；无以 timestamp 排序、以 Agent 名合并 role 或从模型摘要创建状态的行为 |

### REQ-014 · 真实进度状态栏（4 条）

行为与失败按决定提案 §3；状态栏可配置但不能隐藏真实进度/阻塞。标准和精简在窄屏优先保留四组事实，可在多行状态区换行；详细字段从状态页可达。doc_refs：`docs/03-ux.md`、`docs/01-prd.md`、`docs/05-data-api.md`、`docs/adr/ADR-029.md`、CR-025；M3 依赖 REQ-032/033/034/040。提案 D2、D8、D11。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-014-01 / TEST-014-01 | tui-pty | spec r1 有 RQ1(2 AC)、RQ2(1 AC)、RQ3(1 AC)；C1 ACCEPTED，前两需求全部有效，第三无证据；Task 显示 5/5 done；Issue 为 OPEN 2、REOPENED 1、RESOLVED 1；未知上下文/费用的 qa/pi | 打开 status，Focus qa；产生新候选 C2 与 budget.risk，再批准增加 RQ4 的 r2 | 先为 2/3 accepted、open=2/reopened=1，不是 3/4 AC 或 5/5 Task；qa/pi 的成本/上下文均“未知”带身份；budget.risk 原因/上限可查；C2 未接受即 0/3；r2 为 0/4、revision=2、added=1，旧计数仅历史可见 |
| AC-014-02 / TEST-014-02 | unit | 表驱动：无 spec；零 AC；同需求一个 AC FAIL；候选 FROZEN/STALE/INVALID；旧 spec PASS；合法 degradable 的 DEGRADED；spec_review PASS；只有 TaskResult COMPLETED | 通过与 host 共用的需求聚合函数求值；重复验收事实 | 无 spec=0/0 且 completion 未成立；其余不满足者分子=0；只有候选 ACCEPTED 且全部 AC 按政策满足才计 1，合规降级另计 degraded=1；重复不增计，绝不从 spec_review/模型/TaskResult 得 accepted |
| AC-014-03 / TEST-014-03 | tui-pty | 80/120/180×32；standard/minimal/custom 三档；custom 选择 [budget,candidate]；新鲜 host 快照 | 切档并重开客户端；Focus/离开 Focus；并发发生一个候选更新与一个 Issue 重开 | 各档四组必显事实在终端内可读，custom 可选字段顺序与配置一致，重开保留；只有 Focus 显示对应 Agent 上下文；每一帧事实取同一 watermark，不拼出旧候选的新计数；非法字段/重复字段配置被拒且旧档保留 |
| AC-014-04 / TEST-014-04 | integration | 明确的工具开始/结束、服务等待、压缩、排队、空闲/进程 alive，及 context used=25/limit=100、used 缺失、limit 缺失三例；两个 Agent 只有一个可计美元 | 查询 progress/ui 活动与用量，再换 role_epoch | 状态分别为工具运行/等待服务/压缩/排队/空闲；仅 alive 为活动未知，Commander 无 turn 为事件监测中；25/100=25%，缺任一数值为 null；成本标已知部分并列缺项身份，不能当总额；换绑不沿用旧 epoch 的用量 |

### REQ-015 · 输入与终端兼容（5 条）

行为：Unicode grapheme/显示宽度、多行编辑、括号粘贴、不依赖增强键盘协议；resize/网络分片/异常退出都不丢原草稿或误发命令。teardown 覆盖 raw、alternate screen、光标、bracketed paste 和新增 mouse reporting；正常退出、SIGTERM、SIGHUP、panic/unwind/初始化失败均恢复可恢复的终端状态。SIGKILL/断电不能执行 teardown，不能宣称可恢复（恢复 host 另属 REQ-041）。失败时沿用 REQ-004 的退出码，不新增用户命令。

doc_refs：`docs/03-ux.md`、`docs/01-prd.md`、`docs/09-testing.md`、`docs/adr/ADR-014.md`、`docs/adr/ADR-031.md`；M3 依赖 REQ-007/017。提案 D9、D10。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-015-01 / TEST-015-01 | tui-pty | 实际 PTY，UTF-8；输入 `A中🙂é`（e+U+0301），另以空编辑器 bracketed paste `第一行\r\n第二行🙂`；80/120/180 列 | 逐字素移动/删除、resize；paste 后观察再 Enter；直连、SSH、tmux、SSH 内 tmux 四种路径各执行，最后各触发一次 debug panic | `A中🙂é` 的正文显示宽度=6，光标在字素边界，Backspace 不留下孤立组合符/半 emoji；paste 提交前命令数=0，Enter 后恰一次、正文为 LF 两行；四种路径都保持文字与命令数，无中途换行发送；panic 后该路径内 shell 的 stty 与进入前一致、alternate screen关闭、光标显示 |
| AC-015-02 / TEST-015-02 | tui-pty | 无增强键盘协议；编辑器有“甲”；打开列表与临时层 | Ctrl+J 输入“乙”，Esc+Enter 输入“丙”，最后 Enter；列表 Enter、编辑器 `?`、非空 Ctrl+C、空顶层 Ctrl+C、Esc | 提交正文为“甲\n乙\n丙”，一次 SendInput；列表 Enter 只打开块；`?` 是文字；非空 Ctrl+C 清空且不 cancel；空顶层出现选择，Esc 只关闭；未选择控制前 PauseRun/CancelRun=0 |
| AC-015-03 / TEST-015-03 | tui-pty | bracketed paste 按 1 字节分片，含 UTF-8/CRLF/看似斜杠与控制键文本；原稿“保留”；长度分别刚好使最终正文 1 MiB 与超过 1 字节 | 完成粘贴，再测试缺结束标记时由外部发送 SIGTERM；紧接正常 paste 结束后发普通键 | 合法粘贴保持 LF 与文本、未 Enter 不提交；超限整次插入拒绝，原稿“保留”不变并显示 UI_INPUT_TOO_LARGE，后续键可正常处理；没有斜杠执行或误触控制；不完整粘贴内存有界，信号仍进入 teardown |
| AC-015-04 / TEST-015-04 | tui-pty | shell 的 `stty -g` 基线；mouse=true；独立后台 Run；debug 构建故障点可控 | 分别正常脱离、SIGTERM、SIGHUP、panic、进入 alternate screen 后初始化失败、部分输出失败；每次恢复后 shell 读取哨兵输入 | 可写 PTY 上 stty 与进入前逐字一致，alternate screen/paste/mouse 全关闭、光标显示；SIGTERM=143、SIGHUP=129，I/O 错误=74，panic 非零；哨兵由 shell 完整收到，不被遗留读取线程吞掉；后台无 Pause/Cancel，仍能从新客户端查询 |
| AC-015-05 / TEST-015-05 | tui-pty | 参考 VM；百万事件历史；无 Agent 网络等待混入样本；80×24 与180×40各至少100次；单调时钟和终端 buffer 解码器 | 并行追加已提交事件和多行编辑/滚动/resize；记录输入到屏幕、commit 到对应 seq 可见的延迟 | 两种尺寸分别输入 p95≤100 ms、事件 p95≤500 ms；原始样本与环境信息可查；无丢键/重复提交；测试不是只调用 renderer 的计时；任何超限保留失败记录 |

兼容性矩阵提案 D10：Ubuntu 24.04、26.04 x86_64 各跑四种路径；TERM 至少 xterm-256color 与 tmux-256color（后者在 tmux 内），LC_ALL 为可用 UTF-8 locale；记录实际安装版本，不凭包名声称支持。当前 VM 只能跑一版 Ubuntu 时另一版 NOT_RUN，由 M4-T04 在另一环境收齐；不能从快照或 mock 推断 SSH/tmux 通过。用于测试的本机 sshd/tmux 会话须独立端口/会话、非交互、测试后清理。

### REQ-016 · 无障碍与键位（4 条）

行为：重要操作全部有键盘路径；状态有文字与形状/ASCII 标记，不只靠颜色；焦点可见；NO_COLOR、无鼠标与 ASCII 图标同时可用。自定义键位按 scope 校验并可恢复默认。失败：冲突列出两个 action/scope/key，保存整体拒绝；非法配置不破坏现有 Run；没有终端能力不显示虚假的启用状态。

doc_refs：`docs/03-ux.md`、`docs/01-prd.md`、`docs/09-testing.md`、`config/hoh.example.toml`；M3 依赖 REQ-007/017/047。提案 D8、D9、D10。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-016-01 / TEST-016-01 | tui-pty | NO_COLOR=1、ui.mouse=false、ui.icons=ascii，80×32；有效新键 `list.copy=c`，另有冲突配置 | 不用鼠标遍历共同动作清单：四视图、角色/会话、三抽屉、块操作、模式、历史、设置、提交及暂停/恢复/取消/脱离；保存冲突后恢复默认 | 清单每项均产生指定界面或对应唯一命令，证据逐项记录；无颜色 SGR/鼠标启用序列，状态有中文文字和 ASCII 标记；c 可复制，冲突整体拒绝；恢复后 y 可复制、c 只是未绑定键；后台控制只发生于显式选择 |
| AC-016-02 / TEST-016-02 | unit | keybinding 表：同 list 同 key 两动作；global 与 editor 重叠；editor/list 同 key；未知 key/action/scope；覆盖 Ctrl+C/Esc/Enter；将面板重绑为未占用 Ctrl+O，以及使面板无有效入口的配置 | 校验各表，分别试保存；读取错误 details | 前两种冲突与未知/保留覆盖/无面板入口均拒绝，给出准确 action/scope/key；互斥 editor/list 同 key 及合法 Ctrl+O 重绑允许；失败后有效映射逐字段不变；所有默认动作仍可从命令面板到达 |
| AC-016-03 / TEST-016-03 | tui-pty | editor/list/dialog 各 scope，草稿含 `? / f y n`；重绑 list.search 为 Ctrl+F，面板改为 Ctrl+O | Tab 移焦，Ctrl+O 打开面板，Esc 返回；使用重绑键，保存并重开客户端，在面板恢复默认 | 编辑器文字不被列表/帮助热键吞掉；焦点可见且 Esc 回到原草稿/锚点；重开后绑定仍有效；恢复后 Ctrl+P 再次打开面板；只清键覆盖，team/budget/Prompt/mode_cycle 均不变 |
| AC-016-04 / TEST-016-04 | unit | 同一投影包含 PASS/FAIL/BLOCKED/UNKNOWN/STALE/DEGRADED/REOPENED、选中和未选中项；80/120/180 列 | 以彩色与 NO_COLOR+ASCII 两套样式渲染 TestBackend | 每种状态在去样式后的文字中仍可区分，选中项有 ASCII 焦点标记；禁色不含前景/背景色，保留必要非颜色属性；无超过显示宽度、无必需动作只有图标无标签 |

### REQ-064 · 可观测性与容量（5 条）

行为：结构化索引+对象、seek 分页和虚拟列表、诊断日志轮换、所有被引用证据保护、慢读者背压和空间错误可诊断；数值采用 §1/§5 待答提案。失败：不接受半写对象为有效证据；DB 满/忙不承诺成功；cursor 过期明确 resync；GC 无法确定安全时保留。百万历史数据不能在启动时全量反序列化。

doc_refs：`docs/01-prd.md`、`docs/04-architecture.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/14-resilience.md`、`contracts/sql/001_initial.sql`；M3 依赖 REQ-032/033/034。提案 D1、D4–D6。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-064-01 / TEST-064-01 | integration | 1,000,000 个真实产品存储事件，100 个 Run、每 Run 10 个 iteration；10,000 个公开日志 segment，每段 16 KiB；reference VM；旧 schema 有数据 | 升级并建索引，GET timeline 按 Run/iteration/attempt 各查100页，limit=100/500及非法0/501；首尾 Range/404/416；并在同夹具执行磁盘失败与慢客户端子例（详见 -04/-05） | 每页不超过 limit，seq 去重完整、H 固定；新查询 p95≤500 ms，扫描/反序列化量由索引命中和页上限决定而非全库；正文存对象不内嵌页面；非法 limit 为400，Range字节精确；磁盘失败不留半登记，慢读不阻塞快读；升级前后权威行/hash不变，失败迁移回滚，旧binary拒高schema |
| AC-064-02 / TEST-064-02 | tui-pty | -01 的百万事件库，单块 100 MiB 日志，开头/中间/结尾有唯一标记；参考 VM | 打开 Workspace/Focus、跳到末页、搜索远端标记、往返滚动，持续60 s；输入100次；回到 idle 10 s | 初始不回放百万正文；描述缓存≤1000、正文≤4 MiB、在途页≤2（公开诊断计数/测试探针记录）；所有标记可达且锚点稳定；输入 p95≤100 ms；idle Rust host RSS≤150 MiB，另记录 TUI RSS 与缓存峰值；无全量日志加载或无界通道增长 |
| AC-064-03 / TEST-064-03 | integration | 诊断流连续写 6×8 MiB+1 字节；对象集合含活动 Attempt、AC证据、发布、旧STALE/INVALID候选、公开会话、仅注册且无引用、未登记临时文件；时间可控 | 轮换；47 h/48 h/49 h 清理；在扫描后给待删对象加引用；打开对象并与 GC 并发 | 未引用诊断段最多5个且每段≤8 MiB，完整近期字节可读；全部引用对象字节/hash不变；47 h 孤立对象保留，年龄≥48 h且再次判定无引用者删除；新引用/读取pin保护对象；引用解析失败时保留并诊断；events/evidence/acceptance 行删除=0 |
| AC-064-04 / TEST-064-04 | recovery | 已登记对象 E；待写 N；32 MiB 空间阈值；故障注入覆盖临时写、fsync、rename、登记事务，实际受限文件系统覆盖 ENOSPC/EDQUOT | 每点失败或 kill host，重启 reconcile；空间不足时读/控制/脱离，释放空间再重试 | E 始终可读且 hash相同；N 只出现“完整文件+登记”或未登记孤立文件，不能有引用缺正文；未提交的命令无202；读可用时返回旧水位，写失败503 DB_FULL并标未接受；低空间不派发新写任务；释放后重试只登记一次、无重复副作用；孤立文件按宽限保留/清理 |
| AC-064-05 / TEST-064-05 | integration | 一个快 SSE 客户端、一个停止读取30 s的慢客户端；producer 以100 events/s提交10,000条（每条公开正文8 KiB），含 tool/decision/acceptance；队列上限500事件或1 MiB先到者 | 暂停慢读并确认写端遇到背压后恢复，若服务主动断开则从最后seq重连；query/after与Last-Event-ID冲突一次 | 快客户端按seq收到所有提交，逐事件p95≤500 ms；慢客户端服务缓冲≤上限，满后断开/要求replay，不静默丢关键事件；恢复后的去重并集完整；冲突为409；writer 202对应事件缺失=0；这里只验收host/读取库，TUI队列接入由AC-010-03/064-02覆盖 |

AC-064-01 保留原“百万事件/磁盘满/慢客户端”组合覆盖：其中子例可调用 -04/-05 的共用驱动，仍必须在 integration 下为 TEST-064-01 产出自己的结果；-04 的完整崩溃矩阵独立归 recovery，不能借 integration 的日志当 recovery 通过。旧 AC-012-01、AC-015-01 的组合范围同样保留，新增 AC 负责补足边界，不删除旧 AC 的含义。

## 实现任务拆分与应用清单

### M4-T02/T03 的细化

保留 M4-T01/T02/T03/T04 标识，新增 M4-T05–T08。任务按表中 acceptance 明确分配，不按需求出现次数重复认领 AC。各项 kind=implement、owner_role=developer、writer=true；M4-T04 仍为 verify/qa/writer=false。M4-T01 仍依赖 M3-T05，不提前声明 M3 已实现。

| 任务 | requirements | acceptance（完整集合） | checks | depends_on |
|---|---|---|---|---|
| M4-T03 · 查询、对象与容量底座 | REQ-064 | AC-064-01、AC-064-03、AC-064-04、AC-064-05（4） | integration、recovery | M4-T01 |
| M4-T02 · Workspace 与 Team/Focus | REQ-010、REQ-011 | AC-010-01..04、AC-011-01..04（8） | tui-pty、unit、integration | M4-T03 |
| M4-T05 · 内容块与虚拟列表 | REQ-012、REQ-064 | AC-012-01..04、AC-064-02（5） | tui-pty、integration、unit | M4-T02 |
| M4-T06 · Timeline 与真实状态栏 | REQ-013、REQ-014 | AC-013-01..04、AC-014-01..04（8） | tui-pty、integration、unit | M4-T05 |
| M4-T07 · 多行输入与终端兼容 | REQ-015 | AC-015-01..05（5） | tui-pty | M4-T06 |
| M4-T08 · 无障碍与自定义键位 | REQ-016 | AC-016-01..04（4） | tui-pty、unit | M4-T07 |

表中 `..` 在任务 JSON 中展开为全部字符串 ID。M4-T04 的 depends_on 改为以上六项；requirements 保留八项，acceptance 列全 34 项，checks 包含 unit/integration/recovery/tui-pty/independent-qa/independent-review，连同适用 build/static 回归在同一冻结候选上运行。不得以“critical/high 已开修复任务”替代本阶段通过：存在此类问题时报告 FAIL/BLOCKED，修复后新候选重验。

各实现任务的 done_when 共同要求“指定 AC 在冻结候选的指定检查中有逐测试证据，独立审查无未解决 critical/high；自测不等于验收”，并追加：

- **M4-T03**：交付缺失 evidence/timeline/artifact/search 查询与类型、索引迁移/回滚、流式对象与公开输出 segment、引用保护、空间检测、SSE字节/数量背压。read API 与 query-index tests 先完成，供后续 TUI 使用；不得依赖未来 Web/备份/soak。新增配置键/example 与解析同提交，回归 TEST-018-01、REQ-062 持久化、REQ-005/058 的 SSE。对象生命周期/并发按 docs/19 由两名互补独立审查者审查。
- **M4-T02**：快照+增量接线、团队身份、Workspace/Team/Focus 与本地动作/鼠标解码；给新 UI 查询提供生产数据。保留 M3 SetMode、输入投递、history/resume、用户批准入口；`ui.view` 的既有值保持。回归 REQ-001/005/007/017/018，不为测试直接写 host 状态。
- **M4-T05**：五类必测块与八类身份、分页搜索/复制/全屏、虚拟列表、anchor 管理；ui.clipboard 与 example 同提交。文件/OSC52 失败有 UI 诊断，无外部命令自动执行。
- **M4-T06**：F13 的生产事件映射、项目跨 Run 历史与类型化导航、progress 聚合；ui.statusline/fields/view=timeline 与 example 同提交。M3 的 spec/index/scenarios/追踪与全量失效照用，保留旧 getRun acceptance 的 AC 语义。回归 TEST-018-01 和 REQ-032/033/053。
- **M4-T07**：多行编辑/粘贴边界/终端恢复与延迟测量；完成该任务可用平台矩阵，未具备环境明确 BLOCKED/NOT_RUN，M4-T04 收齐。信号/进程生命周期变更依 docs/19 双独立审查。不得把运行 Agent 的等待时间伪装为终端延迟。
- **M4-T08**：重要动作键盘遍历、scope 校验/恢复默认、NO_COLOR/ASCII/mouse 配置；新增键和 example 同提交，回归 TEST-018-01 与 M4-T07 全套输入/teardown。不能让 list 的单字符快捷键吞掉编辑器文字。

### 每条 AC 的依赖闭包

共同祖先链为：实现任务→M4-T01→M3-T05→CR-026 的全部 M3 实现任务→M3-T01→V01-T02→M2/M1。这里是**目标任务图**的能力闭包，不能把尚未合入 CR-026 的工作树当成当前 taskboard 的事实。M4-T01 应用时用最终 task graph 验证每个提供者是本任务或祖先；不通过即修图/重审，不绕过 claim。检查提供者：unit/integration 为 M0-T03，tui-pty 为 M1-T08，recovery 为 M2-T05；build/static 为既有产品检查，QA/Review 为已有独立会话通路。

| AC | 所属任务 / check_id | 本任务或祖先提供的必要能力 |
|---|---|---|
| AC-010-01 | M4-T02 / tui-pty | 本任务的工作台；M4-T03 的读模型；M1-T08 终端；M2-T12 host/API；M2-T13 项目baseline；M2-T07 team；M3-T07 run.baseline |
| AC-010-02 | M4-T02 / tui-pty | 本任务输入展示；M3-T09 SetMode/SendInput/四态与 boundary；M2-T06 安全点；M2-T17 epoch |
| AC-010-03 | M4-T02 / tui-pty | 本任务重连/草稿；M4-T03 水位快照/游标；M2-T12 SSE；M2-T05 host恢复 |
| AC-010-04 | M4-T02 / unit | 本任务纯投影；M4-T03 block描述/公开输出；M3-T09 pending/input事件 |
| AC-011-01 | M4-T02 / tui-pty | 本任务布局/锚点状态；M1-T08 PTY/resize；M2-T07 团队 |
| AC-011-02 | M4-T02 / tui-pty | 本任务 Team/Focus；M2-T07/17 checkpoint换绑；M2-T21 ROLE_BINDING_PENDING；M4-T03 会话分页 |
| AC-011-03 | M4-T02 / integration | 本任务身份过滤；M4-T03 公开segment/Artifact API；M2-T07/17 session/epoch绑定 |
| AC-011-04 | M4-T02 / tui-pty | 本任务紧凑布局/历史角色；M1-T08输入与终端；M2-T07配置角色 |
| AC-012-01 | M4-T05 / tui-pty | 本任务块动作/搜索/复制；M4-T02键鼠与Focus；M4-T03分页对象；M3-T07证据追踪 |
| AC-012-02 | M4-T05 / integration | 本任务UTF-8装配；M4-T03 Range/search；M2-T10/16候选身份 |
| AC-012-03 | M4-T05 / tui-pty | 本任务错误与复制；M4-T03 token/对象错误；M4-T02草稿锚点；M3-T02候选失效 |
| AC-012-04 | M4-T05 / unit | 本任务锚点/块动作；M4-T02局部状态；M4-T03稳定block_id |
| AC-013-01 | M4-T06 / tui-pty | 本任务三级映射；M4-T05详情/返回；M4-T03查询；M3-T07追踪；M2-T18 Issue/验收事实 |
| AC-013-02 | M4-T06 / integration | 本任务过滤语义；M4-T03索引/H绑定cursor；M2-T12 HTTP/SSE |
| AC-013-03 | M4-T06 / integration | 本任务类型导航；M3-T02规格/全量失效；M3-T07追踪；M4-T03工件错误 |
| AC-013-04 | M4-T06 / unit | 本任务三级纯投影；M4-T02增量状态；M4-T03block/schema |
| AC-014-01 | M4-T06 / tui-pty | 本任务需求聚合/UI；M2-T18验收和Issue函数；M2-T21 budget.risk；M3-T02批准/失效；M3-T07需求索引 |
| AC-014-02 | M4-T06 / unit | 本任务聚合；M2-T18有效验收分类；M3-T02修订身份；M3-T06规格审查独立身份 |
| AC-014-03 | M4-T06 / tui-pty | 本任务配置/布局；M4-T03快照水位；M4-T02 Focus；M2-T07配置加载 |
| AC-014-04 | M4-T06 / integration | 本任务活动/用量投影；M2-T07 runner/能力；M2-T17 epoch；M2-T21预算；M3-T10 no_progress |
| AC-015-01 | M4-T07 / tui-pty | 本任务多行/Unicode；M4-T02/05布局与块；M1-T08 PTY解码；M3-T09提交语义；SSH/tmux环境 |
| AC-015-02 | M4-T07 / tui-pty | 本任务编辑器scope；M3-T09命令面板；M2-T06控制；M1-T08历史/退出 |
| AC-015-03 | M4-T07 / tui-pty | 本任务分片/粘贴上限；M1-T08可取消解码；M3-T09斜杠路由 |
| AC-015-04 | M4-T07 / tui-pty | 本任务完整teardown；M1-T08终端Guard；M2-T06后台控制/进程；M4-T02鼠标 |
| AC-015-05 | M4-T07 / tui-pty | 本任务端到端测量；M4-T03大库/SSE；M4-T02/05/06最终UI；参考VM |
| AC-016-01 | M4-T08 / tui-pty | 本任务所有动作/键位；M4-T02/05/06全部视图；M4-T07编辑器；M3-T09命令；M2-T06控制 |
| AC-016-02 | M4-T08 / unit | 本任务scope/冲突；M2-T07配置加载；M3-T09mode_cycle约定 |
| AC-016-03 | M4-T08 / tui-pty | 本任务设置/恢复；M4-T07焦点输入；M3-T03设置与Prompt覆盖；M2-T07团队/预算配置 |
| AC-016-04 | M4-T08 / unit | 本任务样式；M4-T06真实状态；M4-T02布局 |
| AC-064-01 | M4-T03 / integration | 本任务查询/对象/迁移；M2-T01持久化；M2-T12 API；M3-T04迁移运行器；M3-T07tasks追踪 |
| AC-064-02 | M4-T05 / tui-pty | 本任务虚拟列表/正文缓存；M4-T03百万库/搜索；M4-T02工作台/Focus；M1-T08终端 |
| AC-064-03 | M4-T03 / integration | 本任务轮换/引用/GC；M2-T10/16冻结；M2-T18证据/历史；M3-T02失效；现有发布导出引用（无须M6发布UI） |
| AC-064-04 | M4-T03 / recovery | 本任务对象事务/空间保护；M2-T01 DB_FULL；M2-T05/06恢复/取消；M2-T16对象登记 |
| AC-064-05 | M4-T03 / integration | 本任务数量+字节背压；M2-T12 SSE水位；M2-T01单writer；M2-T04可靠事件/投递 |

API query、容量索引先于 Workspace；虚拟列表 AC-064-02 留给 M4-T05，避免底座任务先验收尚未实现的 UI。最后无障碍覆盖全部动作，因此依赖终端与四个视图完成；不存在为凑每任务八条而留下的隐含后续依赖。

### M4-T01 之后拟应用的文件

| 文件/区域 | 拟改动 |
|---|---|
| `contracts/requirements.json` | 八项 detailed、34 AC、行为/失败/决定/doc_refs；REQ-018 仅补配置键与合法值，不改旧 AC |
| `contracts/acceptance-scenarios.json` | 八个旧 TEST 保留并细化，追加26个；逐项 fixture/steps/pass_criteria/observables/check_id 对应上表 |
| `contracts/openapi.json` | §2 的新 typed 查询字段/响应，保留原读写语义和旧 acceptance 计数 |
| `contracts/protocol.schema.json` | §2 四个 ErrorCode；不改 Run 状态、TaskResult、身份或 Gate 语义 |
| 下一号 `contracts/sql/*_ui_observability.sql` | §2 的派生索引/日志段/引用登记；迁移在 M4-T03 接入与测试 |
| `contracts/development-tasks.json` | 六实现任务的显式 requirements/acceptance/checks/depends_on、M4-T04 收敛；M4-T01 的前置保留 |
| `docs/02-functional-spec.md`、`docs/18-traceability.md` | 仅由生成器更新，不手写 |
| `docs/03-ux.md`、`docs/05-data-api.md`、`docs/12-user-guide.md`、实现 ADR | 随实现记录已验证的行为、兼容矩阵、复制限制和设置；不把本稿提案提前写成使用说明 |
| `config/hoh.example.toml`、Rust 与测试 runner | 随各实现任务交付；检查脚本如需收集新测试只能追加覆盖，不删旧检查/改mode/放宽timeout冒充通过 |

正式应用应先重核 main/M3，独立审查并决定 D1–D12，再按 docs/21 生成视图、对新契约执行 relock（含新迁移的 --add）、bundle 自检。此处是应用步骤说明，本次没有执行这些修改或 relock。所有实现和接受仍须 tools/run_check.py 在冻结候选上按现有 check plan 运行；不把这份文档提交当产品候选通过。

## 不变的内容与授权边界

八项均保持 required/v1，旧 AC/TEST 不删除。默认单 Workspace、可选 Team 分屏、公开 Focus、三级 Timeline、响应式 statusline、完整输入兼容、无障碍、对象保护均保留。三 Agent、三 Pi 插件、Web、M5–M7、72h soak 与至少20个真实轮次、独立 QA/Review 和版本绑定规则不变。未知用量不造数；模型陈述、mock、自测与验收各自标明。

拟议 CR 的意图是 CR-003 D1 的细化：新增数字和错误契约，不降低门槛，不改变架构/命令面。CTO 应特别审查 D2 的进度分母、D5 的保留策略、D7 的复制用户体验是否满足原要求；若认定改变已确认交互或删减范围，必须升级给所有者，不能以本稿的“提案”视作授权。草案撰写者不批准自己、不代用户决定。

## 验证记录与尚未执行的工作

本次是规格草案准备，无生产代码变更，产品红/绿测试、平台矩阵、性能和独立设计审查全部 **NOT_RUN**；本稿的数值是待验收目标，不是测量结果。提交前必须实际运行 `bash scripts/check.sh --bundle` 且退出0；执行日志位于被忽略的 `.hoh-dev/evidence/m4-spec-prep/`。这只证明 BUNDLE 自检，不证明本稿的新 AC 已 PASS。M4-T01 应用草案后还须验证完整机器契约/任务图和生成视图，M4 实现后才有 product 验收。

本次未收到 Runtime 身份头中的 run_id/attempt_id/role_epoch、TaskResult 输出文件或自测目录，因此不编造 Runtime TaskResult/候选 ID；按委派的准备任务交付文件与本地 commit。输入明确授予此 worktree 唯一写权限，工作区起始干净。M3/CR-024 只读外部工作树作为文档来源，未声称它们在当前 main 已实现。

## CTO 问题：提案与理由（全部待决定）

1. **D1 — 读模型与 API 补齐是否采用 §1/§2 的快照 H、类型化块和固定水位 cursor？** 提案：新增 typed projection/schema，补已声明路由，项目级 Timeline 用 `/v1/timeline`，读取正文走 Range/search；保留旧 API 字段。理由：当前 host 缺路由，不能让 UI 读 SQLite；单一水位可避免候选、进度和分页相互矛盾。需确认新迁移的最终编号与错误响应。
2. **D2 — n/m accepted 的分母/跨 Run/降级如何表达？** 提案：m 为当前批准规格全部需求，n 只来自当前 Run 最新 ACCEPTED 候选，缺范围仍在分母；degraded 单列，0/0 不完成。理由：AC 计数与 Task done 不能表达产品范围，跨候选拼证据违背现有不变量。若需要另加“本轮覆盖数”，另列字段，不替换此分母。
3. **D3 — Project/Iteration/Agent 的范围与分层采用 F13 吗？** 提案：Project 跨 Run 只列里程碑事实，Iteration 列工程事实，Agent 列指定身份的公开会话；未知事件保留诊断，类型化引用逐项可追踪。理由：`level` 已有枚举但未规定事件分类，必须给测试确定集合，不能靠摘要猜测。
4. **D4 — 分页、缓存、正文阈值与测量预算是否接受？** 提案：100/500 行、1000 描述/4 MiB 正文/2在途页、256 KiB正文页、16 KiB事件正文外置、单次搜索扫描8 MiB、队列500事件或1 MiB；p95沿用100/500 ms，分页也取500 ms。理由：把“不能全量加载”和“快”变成可判定边界；采样和环境固定，数值可在批准前调整，不能测试失败后私改。
5. **D5 — 日志轮换与自动清理采用保守引用保护吗？** 提案：8 MiB×5段只约束可轮换诊断，孤立对象48 h；保护全部历史候选/证据/发布/公开会话引用及读取pin，不删除权威events。理由：失效候选的失败证据仍须可审计；总磁盘大小不能靠删证据硬封顶。需确认长期占用以诊断/人工治理承接，而非自动丢历史。
6. **D6 — 低空间阈值与控制失败怎样对用户承诺？** 提案：默认32 MiB保留空间，加待写对象大小预检；不足停止新写执行；完全满盘时控制命令可明确DB_FULL，不能承诺已暂停/取消；释放后reconcile重试。理由：持久取消也需要写事务，不能写出无法兑现的“满盘仍必定接受”。
7. **D7 — 复制、搜索、工件打开采用文件默认与显式 OSC52 吗？** 提案：默认完整纯文本复制到 `.hoh/exports`，可选OSC52≤64 KiB且只报告请求已发送；搜索是1–256字节字面串、可续扫描；二进制只看metadata/下载，不自动启动外部程序。理由：SSH/tmux 没有统一可确认的系统剪贴板；仍保留键盘可达的完整内容复制和有界读取。此项须重点确认满足“复制”的用户预期。
8. **D8 — statusline 的 minimal/custom 如何保存？** 提案：沿用项目 ui 配置；mandatory四组总在，可选字段数组0–8项；窄屏换行并提供状态页；客户端设置不改执行配置。理由：既兑现自定义，又不能把阻塞和真实进度隐藏。加载器与example必须同提交。
9. **D9 — keybindings 采用 scope/action/key 数组及保留键规则吗？** 提案：global/editor/list/dialog，冲突整份拒绝；Ctrl+C/Esc/Enter保留，Ctrl+P的面板动作可重绑但必须保留可达入口，面板总能恢复默认；列表字符键不侵入编辑器。理由：兑现 docs/03 的可重绑承诺，明确焦点冲突与恢复通路，避免一份错误配置使客户端不可操作。
10. **D10 — 支持矩阵和输入边界采用 §4/REQ-015 的范围吗？** 提案：两版Ubuntu×直连/SSH/tmux/SSH内tmux；80/120/180及阈值边界；小于80×12紧凑退化；输入上限1 MiB整次拒绝；多行必须有Ctrl+J/Esc+Enter。理由：CONTRACT 已定平台和终端能力，但未定组合矩阵/超大粘贴上限；不依赖Shift+Enter，不把SIGKILL列为可teardown。
11. **D11 — 活动与用量采用身份绑定的显式事实分类吗？** 提案：仅已观察的工具/服务等待/压缩/排队/空闲可显示，alive只显示活动未知；Commander空闲为事件监测中；上下文缺分子或分母即未知，费用部分已知明确标注。理由：状态可置信要求禁止“正在思考”或假成本，需确定字段来源和缺失时文案。
12. **D12 — 六个实现任务与分配是否接受？** 提案：T03先提供读模型与容量，T02工作台/团队，T05块与虚拟列表，T06时间线/状态栏，T07终端，T08无障碍，T04统一独立验证；AC-064-02归T05。理由：每任务≤2需求/≤8 AC，避免先验收后续UI；M3-T05作为共同前置，任何未兑现M3契约先重规划。
