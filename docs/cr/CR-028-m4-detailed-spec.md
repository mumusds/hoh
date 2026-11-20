# CR-028 · M4 的工作台、终端与容量规格细化

ID：CR-028
状态：DRAFT
批准人：待 CTO 决定与独立设计审查；本稿没有批准或应用基线变更
日期：2026-11-20（第 2 稿；设计审查第 1 轮处置稿，仍为 DRAFT）
输入基线：第 1 稿 `8960fdf5a0360bae8e1842301eed8bece93e85e4`，分支 `prep/M4-T01-spec`；产品来源基线仍为 `067c241ed7dd1aaba0dd5ee1b0f6aa844414e5e2`
适用任务：M4-T01 的准备工作；正式应用仍须等 M3-T05 完成并领取 M4-T01

本次只修订此文件。拟把 REQ-010、011、012、013、014、015、016、064 从 outline 细化为 detailed，保留全部旧 AC/TEST 标识并扩展为 37 条 AC。需求、场景、OpenAPI、SQL、配置键表、生成视图及任务图的修改均是下文的**应用提案**，本次不修改它们，不 relock、不领取任务、不推进全局台账。下节已绑定的 CTO 指令不再提问；文中的 D1–D12 仅保留尚未决定的细化选择；引用这些提案的 AC 是拟议验收标准，不能在批准前当作已经采纳的产品决定。

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

参考 `/home/ai/dev/hoh/.hoh-dev/evidence/hermes-study/report.md` §4–5（来源树 `514e2cb`），只借鉴启动模式复位、虚拟列表、状态栏字段预算、字素宽度与诊断日志轮换的机制；下文明确哪些是 HoH 提案。没有运行 hermes 代码，也不采用其退出时杀 gateway、丢弃监控队列或按会话条数截断历史的策略。

### 当前实现与需要补齐的部分

1. M1-T08 的 `crates/hoh-app/src/tui.rs`、`tui/app.rs`、`tui/input.rs`、`tui/terminal.rs` 已有 Ratatui/Crossterm 外壳、信号恢复、历史、括号粘贴解码。`tui/editor.rs` 明确是单行编辑器，当前将换行替换为空格；多行输入属于 M4。输入读取是可中断的自有解码器（ADR-014），不能为支持鼠标改回不可取消的阻塞读取。
2. M2-T12 的 `host/http.rs::stream_events` 以 DB 水位和每批 500 行实现 SSE，`Last-Event-ID` 与 `after` 冲突为 409。当前路由有 runs、getRun、team、acceptance、issues、events；OpenAPI 已声明的 tasks、evidence、timeline、artifacts 并非都已有路由。M3-T07 承诺补 tasks 与追踪；其余缺口在本稿分派，不把“有 OpenAPI”当成已实现。
3. M2-T18 的 `persistence/snapshot.rs::AcceptanceView` 与 `getRun.projection.acceptance` 中 accepted/total 是 **AC** 计数，且 accepted 只有当前候选被接受时才非零。状态栏需要需求计数，不能给这个旧字段换标签。Issue 状态来自 ADR-029 的状态函数，不能只读取可能过时的显示列。
4. 当前 `tui.rs::Conn::link` 使用无界消息通道、首次从 seq=0 读；`App.messages` 为增长的向量。这些是 M4 容量改动的实际入口。新工作台须从水位快照和按需页面启动，不能先把百万历史事件全部读进客户端。
5. 当前 `closing.rs` 只在 `iteration.completed` 时建 iterations 行，未写 tasks.iteration_id；adapter 的 Frame/Exit 与诊断输出不等于已持久的公开块。M4-T09 补齐归属、三适配器规范化、公开块与活动事实的生产者；M2-T07 不承担这项未实现工作。
6. 当前配置加载器仅接受 `ui.statusline="standard"`；`ui.view` 为 workspace/team/focus，已有 `ui.mouse`。minimal/custom、键位覆盖和下文容量键须与加载器一起交付，不能提前写进 example 导致 REQ-018 回归（CR-026 的 CR26D-103）。

## 第 2 稿的绑定指令

CTO 在本轮委派中已决定：性能测量在本 VM 可执行、release/串行/一个指定检查，不放宽 100/500 ms 或 150 MiB；M4 负责迭代归属和三适配器公开流持久化；任务遵守单 check owner 与 verify 无 acceptance；保留 SSE 410/顶层 snapshot_watermark 与既有错误语义；默认复制文件，OSC 52 延期；SGR 1006 用自有 poll 解码、X10 跳过；Ctrl+J 多行且慢 Esc→Enter 不提交；未结束粘贴有字节/RSS 上界；输入适配器帧预算；启动先复位私有模式；低空间只挡新对象/新写 Attempt，状态事务能提交时 Pause/Cancel 仍 202；TUI RSS 也有上界；补足状态栏、分类/集合、八类块、小屏、DB_BUSY、退出码与外层宽度测试。以下数值和未定的数据布局是满足这些指令的提案，仍须设计复审，不是批准记录。

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
| `GET /v1/timeline`（新增）与现有 `/v1/runs/{run_id}/timeline` | 前者覆盖当前 host 的项目，后者固定该 Run；level=project/iteration/agent，新增可选 run_id（仅项目路由）、iteration_id、role、role_epoch、attempt_id、session_id、after_seq、before_seq、watermark；limit/cursor 沿用 Page。过滤条件交集，跨 Run 的 iteration/attempt 拒绝；项目级无 run_id 的事件不会伪装成某一 Run 的事件 | M4-T09 交付归属事实；M4-T03 实施下文分类索引/查询；M4-T06 完成客户端 |
| `TimelinePage`/`BlockDescriptor`（新增命名 schema） | Page 的 items/next_cursor/watermark；每块 block_id、seq、kind、run_id、iteration_id、role、role_epoch、attempt_id、session_id、spec_id、candidate_id、summary、refs。refs 是带类型的 requirement/issue/decision/commit/evidence/artifact 标识；没有的字段为 null，不能由 Agent 名反推角色 | M4-T03 |
| 已声明的 tasks/evidence 路由 | tasks 以 M3-T07 的输出为准；evidence 补分页与候选/TEST/ArtifactRef 链。只读查询，不制造验收结果 | M3-T07；缺失 evidence 路由由 M4-T03 |
| 已声明的 `/v1/artifacts/{artifact_id}` | 仅已注册 ID；增加单一 byte Range 的 206/416、Content-Range、ETag（sha256）、Accept-Ranges；无 Range 时仍为原 200 流式下载，不缓冲全文件。缺失 404，摘要不符 409，繁忙/磁盘问题 503，保留 403 | M4-T03 |
| `GET /v1/artifacts/{artifact_id}/search`（新增，D7） | 字面 UTF-8 搜索，不执行正则；query 长度 1–256 字节，limit 缺省 100、最大 100；返回递增 byte_offset、匹配长度、最多 256 字节上下文与续查 cursor。正文流式扫描，单次最多扫描 8 MiB，更多返回 continuation；无匹配返回空列表；非文本 422 | M4-T03；M4-T05 使用 |

查询不能加载全部原始 transcript。块正文经 artifact 引用按需读取；实时公开增量来自已提交的小事件，尚未封口的 segment 不作为可验收 ArtifactRef，封口后登记不可变对象及内容摘要，原消息 block_id 保持不变。二进制对象只能看 metadata/下载，不把二进制控制序列原样写到终端。M4-T03 交付 typed API 和相关 SQLite 派生索引，不依赖 M6 Web。M6 后续客户端复用这些契约。

**错误契约（D1，CR28D-05/-52/-65）**：既有错误体/码不替换。SSE `/v1/events` 保留 `409 CONFLICT`（after/Last-Event-ID 矛盾）；保留 `410 {code:"CURSOR_EXPIRED",message,snapshot_watermark:H}`，watermark 在顶层，不能加 details.resync 代替。查询页的 generation 失效也明确返回同形 410；仅对原 Run timeline 补列这个响应，不改变其非法参数的 `404 BAD_REQUEST`。旧 runs/tasks/evidence 等分页非法参数沿用原 404 BAD_REQUEST；缺 Run 仍 RUN_NOT_FOUND。新 `/v1/timeline` 与新 search 路由的非法参数/cursor 才使用 400 QUERY_INVALID；Run timeline 的非法新增过滤也为 404 BAD_REQUEST。

| 新码（拟补 `$defs.ErrorCode`） | 唯一 HTTP 状态 | 用途与正文 |
|---|---|---|
| QUERY_INVALID | 400 | 仅新 timeline/search 查询；沿现有 Error 形状，details 指明参数 |
| ARTIFACT_NOT_FOUND | 404 | 已声明 artifact 路由补齐缺 ID/缺文件/GC tombstone，details.reason 分别为 unknown_id/missing_file/collected |
| ARTIFACT_INVALID | 409 | 登记大小或摘要不符；返回 ID 与错误原因，不返回正文 |
| ARTIFACT_NOT_TEXT | 422 | 新 search 路由：未通过完整 UTF-8 文本分类 |
| ARTIFACT_RANGE_INVALID | 416 | artifact 的新增 Range 功能：无效、多个或越界 Range，带 `Content-Range: bytes */<size>`；不复用 QUERY_INVALID |
| ARTIFACT_UNREADABLE | 503 | 对象 EACCES/I/O 错误；details.reason=permission/io，不冒充认证 403 |

所有新码只用于此处点名的新增路由/新增功能，使用原 Error 的字段，不发明第二套通用错误体；403 的 token/Host/Origin 语义不变。DB_BUSY/DB_FULL 仍 503；本地 UI_CLIPBOARD_UNAVAILABLE、UI_INPUT_TOO_LARGE、UI_KEY_CONFLICT、UI_UNKNOWN_EVENT 不加入 Run 状态。低空间的调度拒绝另在 `$defs.RejectionReason` 追加 `STORAGE_LOW_SPACE`（§5），不改 BlockedReason。

**查询文本规则（D7）**：登记不可变对象时流式校验全文件 UTF-8、拒 NUL，将 text/binary 分类与内容摘要一同登记；未封口或未分类对象不能搜索。Range 以字节为单位，UI 保留最多 3 个未完成 UTF-8 字节到下一页。搜索每次消费至多 8 MiB 新字节，保留最多 query_bytes−1 个尾字节给下一窗口；按匹配起点去重（允许重叠匹配），cursor 绑定 sha256/query/下一个未报告起点/扫描 offset，limit 命中时从最后已报告起点+1 恢复，不能跳过本窗口余下匹配。上下文截在字素边界且≤256 字节。二进制只显示 metadata 与下载动作，正文控制字节不进入终端。

**拟议受锁持久化变更**：M3 的 `002_spec_drafting.sql` 之后增加下一顺序迁移（预期 `003_ui_observability.sql`，应用时确认编号），同时加入 `scripts/product/persistence-whitelist.json`。M4-T09 交付迁移运行与公开输出存储，M4-T03 交付查询/GC；不改写 001/002，不新增验收真值。表/索引至少有：`ui_iteration_ranges(run_id,number,iteration_key,start_seq,close_seq)`、`ui_event_index(seq,run_id,iteration_key,attempt_id,role,role_epoch,session_id,class)`，索引 `(run_id,seq)`、`(iteration_key,seq)`、`(attempt_id,seq)`、`(role,seq)`、`(session_id,seq)`；`public_segments(artifact_id,attempt_id,block_id,stream,first_offset,bytes,sha256,sealed,created_seq)`；去重的 `(attempt_id,native_stream,native_item_id,part)`；对象引用索引 `(owner_kind,owner_id,artifact_id)` 与 tombstone。

迁移事务**只建空结构**，不在事务中全量回填；先完成原 reconcile，再由单 writer 分批（每批≤500 行/1 MiB、批间让出控制队列）回填，保存断点，`index_ready=false/indexed_through` 明示不完整，仍可读原快照与接受 Pause/Cancel。首次回填、schema 索引版本变化、启动完整性核对发现缺索引时才重建；普通重启不递增 generation。新 generation 完整构建到 H 后原子切换，期间查询新索引以 503 DB_BUSY、details.reason=index_rebuilding 明示重试，不给半页假称完整。debug 专有 fixture 钩子只触发同一重建函数；release 性能测试用真实旧 schema 升级路径，不靠 debug 钩子。百万库 kill→reconcile 与 host ready 均≤30 s；回填≤600 s，见 TEST-064-01。iteration_key引用ui_iteration_ranges而非只含已关闭轮次的旧iterations表；(run_id,number)唯一，close_seq可空且非空时大于start_seq。其他外键/唯一约束、升级失败回滚、旧 binary 拒高 schema 按 REQ-062/M3 迁移运行器测试。

### 2.1 M4 迭代归属生产者（D3，M4-T09）

提案采用**已提交关轮边界**，避免为 UI 改工程关轮/预算语义：Run R 的第 N 轮是该 Run 的事件序列 `(close(N−1), close(N)]`，第一轮下界为 0；最后一次 close 之后为开放的 N+1。项目 seq 中别的 Run 的事件不属于 R。稳定 `iteration_key` 使用现有 `child_id(run_id,"iteration/N")` 算法（展示名 I1 等只为夹具别名）；`iteration.completed` 自身属于关闭的那轮。在线写事件时在同一 writer 事务读取上次 close 并登记归属，关闭后 key 不变，重放依同一边界还原。run_id=null 的项目事件 iteration=null；未关闭轮次也可过滤，不伪造 iterations 已关行、不增加预算、不触发一次额外 accepted。

这是 UI 事件成员关系；Task 可跨轮重试，不能把其全部事件都归到 tasks.iteration_id 的单值。保留该旧列，不把它充作事实；同 Task 新 Attempt 在下一轮的事件属于下一轮。M3 CR-026 只把该列原样迁移，拥有规格起草/审查/SendInput/关轮规则；未承担此归属生产者。本任务不改变 M3 的 Task/Attempt 生命周期、iteration.completed 触发点或 REQ-040 净进展公式。旧库回填用真实关轮事件，缺关轮/异常重复关轮保留诊断且拒称完整；不从文本或时钟猜。

### 2.2 三原生流到持久公开事实（D11，M4-T09）

沿现有 adapter 的已解码通知增加公共规范化出口，接入生产 runner→Persistence；不止在测试注入 token.delta/tool.output。绑定 project/run/task/attempt/role_epoch/session/config_revision 与原生 item/call id；公开 user/assistant 文本、工具开始/结束和公开结果、handoff/error 进入稳定块；压缩、等待、用量是同身份的 activity 事实。源端没有提供时写 unsupported/unknown 的诊断，绝不因 alive 推断活动，也不要求所有底座都有在线 usage。

| 底座与现有接口 | 拟映射的公开字段/事件 | 缺失和终结规则 |
|---|---|---|
| codex App Server；兼容 exec | 已锁定 schema 的 item/started、item/completed、公开 message delta、工具 call id、thread/turn id 与显式 usage/compaction 通知；exec 对应已录制 item 帧 | reasoning item、内部 instructions 不进公开块；exec 不捏造 steer/在线 context 能力；未知字段仅留 kind/id/诊断 |
| claude stream-json | assistant/user 的公开 text、tool_use/tool_result id、已声明 stream_event 增量、result 的 usage/resolved model；版本实际有的 compact/wait 通知 | thinking/signature/system prompt 不进公开块；无在线 usage 时结束前为未知，result 到达后才记终值 |
| pi RPC/JSON 兼容流 | 录制版本的 message_update text、tool_execution_start/end、显式 auto_compaction/auto_retry 与最终 message usage/session | thinking 不进公开块；agent_end 后 compact/retry 不等于 settled，沿现有 Pi 终态规则；无 context limit 为 null |

M4-T09 必须为每个受支持模式建立版本+fixture sha256+JSON 字段路径的映射表，依据仓库 compat 录制与 native schema，缺通知的版本明确列 unsupported；此表是实现产物，不假造本次未查到的 provider 字段。只允许白名单的公开字段进入块；认证头/令牌/env、内部 Prompt、隐藏推理字段即使在原始帧出现也不得进入公开 API/对象，故障夹具放唯一 secret 标记验证。公开工具结果里的凭证模式也去敏并标记 redacted；此项是公开面边界，不另起隐私工程。原始审计日志按既有受控路径保留，不原样暴露给 Focus。

每 Attempt 规范化队列≤256 项且总正文≤1 MiB，单公开事件正文≤16 KiB，其余分段外置；活动/工具/决策/验收事实不能丢弃。消费落后时对原生管道背压，控制信号独立；公开文本合并最长 50 ms/16 KiB（先到即提交），渲染合并最多 50 ms，不能合并掉工具边界。段上限 1 MiB，未知长度预检按该上限预留；封口 fsync→rename→登记后才发含 ArtifactRef 的事件；未封口增量仍是已提交的小事件，不能当验收工件。重复通知按 native id/offset 幂等，无原生 id 的帧使用一次已持久的接收 offset；不能用文本去重。重启/EOF/取消保留已提交块及缺口标记，不把半段当完整结果；旧 epoch 只能落到旧 Attempt 历史，不能改当前活动。TEST-011-05 从每个适配器兼容 fixture 经**真实 runner 与存储接线**验证，TEST-014-05 在 adapter-live 补三家真实公共消息/工具/可提供用量的落库观察。M3-T09 继续独占 SendInput 路由、四态与安全点；M4 只提供传输字节预算/文件正文与观察投影，不能再写一套投递循环。

### 3. 状态与进度提案（D2、D11）

1. `m` 是当前已批准 spec 中的全部需求数，不是 Task 数、AC 数，也不是仅已排进当前 Run 的需求。没有规格时显示“未批准规格，0/0”，不显示 100%。`n` 只计当前附着 Run 的最新候选上、身份与该 spec 一致、全部 AC 被有效验收且候选已 ACCEPTED 的需求；不拼接不同候选或不同 Run 的 PASS。尚未纳入任务的需求仍在分母中，分子为 0。历史/跨 Run 的通过可查，但不借给当前候选。
2. required 需求全部 AC 为 PASS 才计入；degradable 全部 AC 为 PASS 时可计入 n；含合规 DEGRADED 时 n 不加 1、m 保留该需求、只给 `degraded=k` 加 1，不得写为全部 PASS。无 AC、证据失效/缺失、orphan、UNKNOWN、BLOCKED、FLAKY、SKIPPED 均不计。客户端调用 host 的验收投影，不重写 Gate 规则。
3. 新候选未 ACCEPTED 或规格变化时 n=0；显示新 spec revision 与新增/删除的需求 ID 差集数量。删除的范围只显示已批准的变更，不由 UI 修改。旧计数保留在历史详情；`spec_review` PASS、TaskResult COMPLETED 和模型“100%完成”不增加分子。
4. `open` 只计状态函数得出的 OPEN，`reopened` 只计 REOPENED，避免重复计数；按当前 Run 的唯一 issue_id 统计。状态栏阻塞数取 Run/Task 的持久阻塞，不把等待服务、排队和离线混为失败。
5. 未附着时显示“未附着 Run”、n/m=未知、阻塞/issue=未知；沿 ADR-031 显示项目 baseline，不把最近 Run 偷当当前。新候选归零仍显示候选 ID，最近 ACCEPTED 的计数仅在历史详情，不用它填当前 n。默认优先序：mode（含 pending）、阶段/任务、n/m accepted、blocked/open/reopened；随后 Git 分支/候选、运行时间、预算。Focus 才显示该职位当次 Attempt 的 Agent、实际解析模型、上下文；配置模型不冒充 resolved model。成本与上下文未知显示“未知（role/agent，原因）”，订阅调用不推算美元费用。已知部分成本显示“已知部分”，不是总额。
6. 活动分类依据显式工具开始/结束、服务等待、原生压缩、调度排队、终结和最后活动事实。只有进程存活时显示“进程存活；活动未知”；没有在途监督的 Commander 显示“事件监测中”。模型摘要注明“分析”，不能覆盖确定性状态。缺上下文上限、缺已用量或计量身份不匹配时百分比为 null；不从字符数估算 token。

状态栏字段表（D8）：minimal只有mode、阶段/任务、n/m accepted、blocked/open/reopened四组；standard按这四组后接git、candidate、elapsed、budget；custom按这四组后接配置数组0–8项、严格保序且不重复。Focus的Agent/实际模型/上下文在Focus头部与状态详情可查；custom配置agent/model/context时只在Focus显示，未Focus省去并可在状态页解释。窄屏先换行保四组，再省可选字段，省去项从状态页可达。同帧状态栏与进度取单一完整getRun响应H；Timeline独立标last_applied_seq，禁止宣称可变表能回读旧H。

### 4. 布局、动作、输入与配置提案（D7–D10）

Workspace 保持默认单主区，团队摘要可折叠；Team 显示全部配置职位的状态卡，不把六职位误做六个常驻进程。Focus 以 `(run_id,role,role_epoch,attempt_id,session_id)` 定位历史公开会话，换绑后不自动把旧消息换成新持有者。布局阈值保持 docs/03：<100 列单区/标签，100–159 列主区加一个详情区，≥160 列允许三列，分屏必须由用户选择。80/120/180 是必测宽度，另测 99/100/159/160 边界；<80 列或高度<12 行显示紧凑提示与可操作的输入/退出入口，允许省略可从状态页读取的信息，不丢草稿。

动作使用稳定 action ID，不新增 Shell 命令。拟定公共操作：workspace/team/focus/timeline、task/issue/evidence 抽屉、选择角色/会话、展开/折叠、复制、搜索/下一处/上一处、全屏/返回、状态详情、设置、命令面板、历史搜索、模式切换、提交、暂停/恢复/取消/脱离。Enter 在编辑器提交、列表展开；Esc 只退一层；Ctrl+R 保留历史搜索；`?` 在编辑器是文字；Ctrl+P 命令面板。列表默认 `Space` 折叠、`f` 全屏、`y` 复制、`/` 搜索、`n/N` 下一/上一处；这些只在列表 scope 生效，命令面板提供相同动作。鼠标单击选择、双击展开、滚轮滚动，动作按钮可单击；禁用鼠标后所有动作仍可键盘执行。

复制提案（D7，默认文件已由 CTO 指定）：把选定公开块的全部纯文本流式写入客户端项目的 `.hoh/exports/<sha256>.txt`（临时文件→fsync→rename），在 ADR-027 冻结守卫中追加 exports/ 并核对实际忽略有效，忽略被用户移除时拒绝导出并解释，不悄悄造成工作区污染；不自动清理用户显式导出，按 exports 单列占用并可从文件管理删除，返回可复制路径和 sha256；不包含边框、颜色码或隐藏字段。这是明确的“复制到文件”，不是谎称已进入系统剪贴板。OSC 52 **延期，不在 M4**，不增加 osc52 配置值；ADR-014 的 CSI-only 约束保留。未来若引入，必须另有修订规定原子写出/截断恢复，不能依靠 CAN 终止 OSC。本稿不再把它列为 CTO 选择。读取仍经 host 的 Artifact API，UI 不写 host 状态表。导出错误显示路径与原因，不记成功。默认不自动启动外部编辑器、浏览器或 shell。

多行输入把 CRLF/孤立 CR 规范成 LF；Ctrl+J 是所有路径必备的换行，Shift+Enter 只在能区分的终端增强。取消 Esc+Enter 的多行别名；为避免慢链路误提交，独立 Esc（30 ms 超时）返回/关闭层后设置 `submit_armed=false`，下一次编辑器 Enter 只恢复编辑焦点并显示“已返回编辑，未发送”，不提交；普通编辑键或 Ctrl+J 恢复 armed，再 Enter 才发送。该提案补入 docs/03/ADR-014，50/100/500 ms 间隔与连续双键都测，不擅自改 Esc 为取消 Run。

编辑器 UTF-8 正文限 1 MiB；超限拒绝整次插入/粘贴，原草稿不变。未完成 paste 最多保留 1 MiB 正文+结束标记的 6 字节状态，超过即丢弃到结束标记/信号；不得把余字节解释成命令。64 MiB 未结束粘贴相对 idle 的 TUI RSS 增量≤16 MiB。完整 paste 不提交，仍需显式 Enter；粘贴内 Ctrl+C 是数据，外部 SIGINT/SIGTERM/SIGHUP 仍立即进入退出路径。

**adapter 帧预算（M4-T07）**：1 MiB 是编辑/SendInput 的正文限，不是原生 inline 限。只有正文≤256 KiB 且按该 adapter 的实际 JSON 编码、包装和最坏回显预算≤768 KiB 时 inline；其余在同一投递链中写不可变正文文件，给该 Attempt 可读的路径、字节数、sha256 与“读取该文件作为用户输入”的短引用，最多16 KiB。不把大正文再附回 prompt；原生单帧仍≤1 MiB。全转义输入即使≤256 KiB 也走文件。文件与 input_id/Attempt 绑定、重试复用摘要、未确认投递不标 APPLIED；失败保留草稿/QUEUED 或显式 REJECTED 原因，不静默截断。D10同时提案在SendInput受理路径支持最多8MiB的JSON请求（解码正文仍≤1MiB，包装后的实际请求也须≤8MiB），使最坏JSON转义仍能到达文件引用分支；不能只测adapter函数而绕过HTTP默认body限制。超上界沿host/http.rs::submit_command的既有400 BAD_REQUEST/Error体（too large or broken）、保留草稿且不返回202；其他命令的大小准入不放宽，不新造413或错误体。每个 adapter 的 fixture 必测从HTTP受理到adapter的最大1 MiB、256 KiB边界、NUL/引号/反斜杠最坏转义、文件不可读/摘要不符；此扩展沿 M3 SendInput 的四态和幂等，不改 route/安全点。

鼠标：在 ADR-014 自有 poll 解码器中解析 SGR 1006 的 M/m，坐标可>223，完整序列后紧接 Enter/UTF-8 字符不吞下一键；残缺/非法序列有≤500 ms 和≤64字节界限，超限丢该序列且不进入无限吞键；遇不属于SGR参数/终止字节的Enter或UTF-8起始字节，立即终止残缺序列并将该字节交还正常键解码，不消费下一键。X10 仍整段跳过。开启的1000/1002/1003/1006在退出逐一关闭；tmux自行占用鼠标时该变体用键盘，原始SGR注入变体仍必测。

启动复位：安装信号/恢复守卫后，进入 raw/alternate/paste/mouse 之前，先发送 CSI 关闭 1000/1002/1003/1006、2004、1049并显示光标；这是对 ADR-014“未进入不发送退出”的**启动阶段显式修订**，只在启动复位阶段无条件一次，后续 teardown 仍按成功开启的状态跟踪、不重复退出。分段写失败仍依 CSI/CAN 和原恢复守卫处理，复位完成前不接收输入；SIGKILL 后靠下次启动修复私有模式，不声称修复上次已丢失的 termios。采用 hermes resetTerminalModes 的先清模式原则，不采用其 gateway 退出行为。

配置拟追加/细化 REQ-018 的键表，所有缺省值与 example 在加载器支持的同一提交进入，均重跑 TEST-018-01；不是 M4-T01 提前改变可运行 example：

| 配置 | 合法值、缺省与作用域 | 交付 |
|---|---|---|
| `ui.view` | 保留 workspace/team/focus，增加 timeline；缺省 workspace；只影响客户端初始视图；没有可用 Focus 身份时显示角色选择器 | M4-T02 基础，M4-T06 增加 timeline |
| `ui.statusline`、`ui.statusline_fields` | standard/minimal/custom，缺省 standard；custom 的 fields 为不重复数组，0–8 项，从 git/candidate/elapsed/budget/agent/model/context/activity 选；必显四组字段不允许移除，fields 只排列可选字段 | M4-T06 |
| `ui.mouse`、`ui.icons` | 保留 mouse=true；icons=auto/ascii，缺省 auto；NO_COLOR 非空只禁 SGR 颜色，仍允许焦点反显等非颜色属性 | M4-T08 |
| `ui.clipboard` | 仅 file，缺省 file；osc52 拒为非法配置；仅显式复制生效 | M4-T05 |
| `ui.keybindings` | 数组 `{action,scope,key}`，缺省空数组；scope 为 global/editor/list/dialog；空数组即内置映射。替换同 action 的缺省键，拒绝不可识别 key/action/scope、同 scope 同 key 的两个 action，以及 global 与任一活动 scope 的冲突 | M4-T08 |
| `observability.log_segment_bytes`、`log_segments` | 整数字节 1048576–67108864，缺省 8388608；整数 2–64，缺省 5；仅未被引用的诊断日志轮换 | M4-T03 |
| `observability.orphan_grace_seconds`、`min_free_bytes` | 整数 3600–604800，缺省 172800（48 h）；整数字节 16777216–1073741824，缺省 33554432；保守清理与写入预检 | M4-T03 |

键位保存前整体校验，不部分生效；恢复默认只删 ui.keybindings 覆盖，不动团队/预算/Prompt。原有 Ctrl+C、Esc、Enter 的上下文含义保留，对这三键的覆盖提案直接拒绝。Ctrl+P 打开面板的动作按 docs/03 **可以重绑**：替换后必须仍有一个无冲突的 global 键可打开命令面板，面板内始终有“恢复默认键位”；无入口的映射整体拒绝。跨互斥 scope 的同键可用；global 与局部重叠则冲突。非法配置仍走 REQ-018 的路径/键诊断，不静默回退。D9 提案：ui 覆盖保存于既有项目配置 `.hoh/config.toml`，使用保留注释的 TOML 编辑、读时摘要 CAS、同目录临时文件+fsync+rename；只改 ui 子键，摘要冲突拒绝、不覆盖其他编辑。REQ-018 的配置监听明确将 ui-only diff 交客户端重载，team/config_revision/活跃 Run 快照不变；混合执行配置仍走原 ConfigureTeam。Shift+Tab 可重绑 mode.cycle，但不改 M3 的 ui.mode_cycle 顺序；Ctrl+J 多行不可被覆盖。

### 5. 容量、日志、证据保护与测量（D4–D6）

结构化事件与小摘要进 SQLite，大正文进对象文件；事件 payload 的公开正文超过 16 KiB 时转为 artifact 引用，不截掉 tool/start/end、decision、acceptance 等关键事实。原始日志保留完整字节；终端渲染转义 ESC/OSC 等控制序列，不能让工具输出伪造 UI 或执行终端命令。迁移旧大 payload 时保留原始事实，用派生索引外置，不 UPDATE/DELETE 旧 events/evidence/acceptance。

普通诊断日志按 8 MiB 轮换、保留 5 段（D5 的缺省）；写入按字节拆段，每段不超过上限。证据、候选、发布、追踪节点、公开会话块和仍活动 Attempt 引用的对象全部保护，包括历史 STALE/INVALID/REJECTED 候选的引用；超过日志配额只标“证据占用”，不删除或改写它们。v1 的本提案不删除权威 events/验收历史，只清理没有任何引用、封口且年龄达到 48 h 的对象与未登记临时文件。创建、登记、引用与 GC 的最后一次可删判定使用同一 Persistence 写序列协调；对象读取持有短期 pin，正在读的对象不回收。清理在取可删集合后必须复查，不能用一次过时扫描删除刚被引用的对象。引用无法可靠解析时保留并诊断，不能假定无引用。

引用的权威根明确为：`evidence.artifact_id`、`independent_reports.report_artifact_id/raw_session_artifact_id`、`handoffs.package_artifact_id`、活动 `attempts.raw_log_path` 及已登记输出、candidate manifest 的 FileRef/ArtifactRef、release manifest 的所有 ArtifactRef、追踪节点的 evidence refs、`public_segments.artifact_id` 和已提交事件载荷中 schema 标为 ArtifactRef 的 `artifact`、`artifacts[]`、`evidence[]`、`refs[]`。路径型引用解析到登记的 path+sha256，effects.artifact_sha256 保守保护同摘要对象；注册 artifacts 行自身不构成引用。未知载荷/解析失败保护可能涉及的对象并诊断，不把它当空引用。GC 保留登记行并原子写 tombstone，待删文件在读 pin 释放后删除；API 返回 404 ARTIFACT_NOT_FOUND/details.reason=collected，区分未知 ID 和丢文件。任何引用的历史对象都不能打 tombstone；崩溃后重复 GC 幂等，权威 events/evidence/acceptance 删除数为0。

大对象写入前需要 `available_bytes >= object_write_bytes + min_free_bytes`；流式未知长度按1 MiB segment 上限预留。低于门槛只拒绝新对象写入和新写 Attempt（包括新写重试），不打断已有执行/读取/控制。新增 `$defs.RejectionReason=STORAGE_LOW_SPACE`：Commander dispatch 的 decisions.reason_code 与自动调度 `schedule.round` 的 rejected 条目都记录该码及 available/required/min_free；不创建新的 Run BlockedReason，不伪称 DB 事务已失败。释放空间后原 READY Task 可按原调度重试，不制造新 Task。回归 REQ-035/036/037 的调度/租约、REQ-042预算/重试、REQ-043控制及 CR-025 ROLE_BINDING_PENDING 优先级。

**两种独立路径**：低空间预检触发但状态事务可提交，PauseRun/CancelRun 仍202且各有持久事件，UI显示已接受；只有状态事务自身 ENOSPC/EDQUOT 才503 DB_FULL且无202/无乐观状态。读取旧已提交事实与终端脱离仍可用。SQLite writer busy 则503 DB_BUSY，保留最后快照/stale，客户端不得被 busy_timeout 阻塞绘制和脱离。实际受限文件系统 ENOSPC 由 `scripts/dev/test-infra.sh` 在基础设施阶段用已授权 sudo -n 准备隔离 loop/ext4 与开发用户权限，记录 test-infra.json（路径、容量、挂载、清理）；检查本身非root。EDQUOT 用现有可注入文件系统 seam 覆盖并标注 fault injection，不伪称启用了真实配额。缺 ENOSPC 设施为检查 FAIL/非零、阶段 BLOCKED，不跳过。临时文件/fdatasync/rename/登记各故障点均保留首次失败；完整文件登记前崩溃为48h孤立对象，登记后已有引用始终保护。

### 5.1 唯一性能测量入口与环境（D4，CR28D-01/-69）

选择**既有 integration 检查的串行 release 测量阶段**，不新增 perf check、不更改14项检查的归属/mode/门槛/超时。批准应用时在 tests/check-plan.json 的 integration.artifacts 追加 `m4-measurements.json`、`m4-environment.json`、`m4-samples.jsonl`；argv 仍 `bash scripts/product/cargo-tests.sh integration`。实现时在其既有测试全部收取后，串行构建 release HoH 与独立 example 测量驱动（不放 `tests/` 的自动 libtest target），再单进程依次运行本节列出的场景，所有子进程在同一受限 systemd user unit；禁止并行 cargo/其他测量。采集 `/proc/<pid>/status` 的 VmRSS（KiB→MiB），分别记录 host/TUI，并断言 idle **host+TUI 总和≤150 MiB**，因此两者各自也≤150 MiB；活动峰值 host≤256 MiB、TUI≤256 MiB。Agent/浏览器不计，测量驱动单列不计核心。REQ-068 的4vCPU/8GiB、至少1000样本与原门槛全部保留，M4不签收M7。

本次读到的 VM 是 Ubuntu 26.04.1 x86_64、Intel Core Ultra 9 285K、12 vCPU、MemTotal=67396161536 B、无swap、项目盘 `/dev/sda2` ext4。虚拟盘 rotational=1，不能据此声称物理后端是SSD；本 M4 **参考环境**提案取本VM这一盘且不放宽预算，M7仍需核验 docs/01 的本地SSD条件。测量将全部被测进程约束到 CPU 0–3（由taskset设置，逐进程核对有效CPU affinity）、MemoryMax=8589934592、MemorySwapMax=0、CPUQuota=400%，禁止其他本次检查工作并行，记录是否施加成功。本次最小资源探针证实user unit只有cpu/memory/pids控制器，AllowedCPUs不会施加cpuset限制；taskset继承的affinity为[0,1,2,3]，memory.max=8589934592、memory.swap.max=0、cpu.max=400000 100000已实际读回，不能把未生效的AllowedCPUs当证据。它是本VM上的4CPU/8GiB资源包络，不虚称整台VM只有4CPU。`scripts/dev/test-infra.sh` 准备可用的 user systemd unit，以taskset把测量driver及其host/TUI子进程设为CPU 0–3；检查逐进程读取实际affinity和cgroup的memory.max/cpu.max，不靠 argv 声称约束有效；记录任一被测子进程扩宽affinity为环境不符，不静默放行。

冻结前 `.hoh-dev/environment.json` 增加 `m4_reference`（随 candidate.environment 摘要绑定）：profile_id、上述 OS/架构/CPU family-model/拓扑、物理内存观察值、项目盘的 mount source/FSType/rotational 与后端未知说明、允许CPU、三个cgroup限制及taskset affinity、内核/Rust/终端buffer/locale/tmux/ssh版本、release binary hash、fixture参数及下列budgets。binary hash 如在检查内才生成，写 m4-environment.json 并与冻结源码构建来源绑定，不事后改冻结 environment；冻结描述只含构建输入/目标profile。同检查输出 m4-environment.json 记录实际值、effective限制与参考匹配结果。环境一致性匹配上述机器/盘/限制/构建/locale版本，boot_id与可用空间是观察量、不要求跨重启相等。不能通过在别的机器重写 reference 来获得同一 profile 的 PASS；变更参考需修订并复审该 CR 的环境提案。

| 测量 | 数字预算（参考环境） | 采样/执行 |
|---|---|---|
| 多行编辑/滚动/resize 输入到外层 PTY 可见 | p95≤100 ms | 每种80×24、180×40各≥1000次；包括50ms文本合并负载 |
| 事务 commit 到外层屏幕对应seq可见；快 SSE commit到接收 | 各 p95≤500 ms | 各≥1000次；单调时钟、seq、原始样本完整保存 |
| timeline 查询/块首屏 | 各 p95≤500 ms | 每种过滤与每种块≥100次；不能只量renderer |
| 绘制/文本增量合并 | 最长50 ms或16 KiB先到 | 调度探针记录窗口；工具/决定/验收事件完整计数 |
| idle 核心与活动峰值 | idle总和≤150 MiB；活动host/TUI各≤256 MiB | 活动60s、idle10s，100ms采样；包含后台索引完成后的稳态 |
| 百万事件夹具、迁移回填 | 构建≤600s；回填≤600s；kill→reconcile/host-ready各≤30s | 单writer产品批提交≤500行/1MiB，ACK/fsync不绕过；不改PRAGMA降低持久性 |
| 本检查总预算 | 仍≤3600s | 原集成+release构建≤900s、造库≤600s、升级回填≤600s、全部测量/故障≤1200s，留300s清理；超时FAIL |

百万库只在这次 integration 执行中构建一次；在同一 candidate/spec/environment/fixture哈希下，以产品一致性备份克隆给各场景，克隆恢复后校验水位和对象hash；不跨候选借证据。100 Run×10 iteration，生产批写产生1,000,000事件与10,000×16KiB段；可验证关轮与引用，不能直接写伪造host投影。小规模功能测试用同一驱动的小数据版，保持其真实存储语义。

带绝对时延/RSS/大容量义务的 TEST-012-01、TEST-015-03、TEST-015-05、TEST-064-01、02、05 **唯一 check_id 都是 integration**，由这个阶段运行包含所需PTY动作的完整场景；tui-pty/unit/普通 integration 回归仍可调用不带这些TEST ID的功能助手。`cargo test --workspace` 与 `--test '*'` 不会发现 example 驱动，因此不重复百万造库、不产生第二套同ID的 SKIPPED/PASS；CI 保留现有测试，追加 tmux/openssh-server/UTF-8 locale 的基础设施准备供功能PTY，受限参考测量只经 run_check integration。在非参考机器上试跑正式integration时：每个受影响TEST输出 schema 合法的 **DEGRADED** 行，detail字符串中写 reason=REFERENCE_ENV_MISMATCH，脚本退出3，汇总阶段记 BLOCKED；缺必跑路径/设施为FAIL行/非零。test-result没有BLOCKED/NOT_RUN枚举，不写进去；run_check execution只记真实退出码，不伪加status。DEGRADED仅用于未能测量的诊断行，required AC绝不通过、不能据此交付COMPLETED_DEGRADED；门禁仍按docs/10拒收。这些行由测量runner写 schema并校验，不能把libtest的ok转换成PASS；环境不符仍产出三个声明文件，samples为空且summary注明未测，不补造样本。环境符合但任何预算超限就是FAIL；p95取全部测量样本升序第ceil(0.95×N)项，首屏冷启动样本也保留；不删慢样本、不把首次失败改写成成功。

## 各需求的细化与 AC→测试映射

应用规则：每项保留 id/category/priority/milestone/title/required_release/release_class/release_targets 和原 summary；改 `spec_maturity=detailed`，把本节“行为/失败”分别转为 behavior/failure_modes，把引用的提案与已定事项放入 decisions，补 doc_refs。AC 表的输入→操作→判定分别是 given/when/then，同号 TEST 为 test_id。每条表行同时定义 `acceptance-scenarios.json` 中的 fixture/steps/pass_criteria；observables 至少包含下列该行要求的屏幕/命令/HTTP/DB/原始日志，不能只保存截图。

每个 TEST 恰归一个 check_id；§5.1 列出的性能组合场景仅在 integration 的 release 阶段执行。tui-pty 场景放进该检查实际运行的 `crates/hoh-app/tests/tui_pty.rs`（可调用公用 TestBackend 断言，但正式结果来自此 target）；unit 在 lib/bin 单元测试，integration 在 tests target，recovery 由既有 recovery runner 收集。名称含 `test_NNN_MM`，原始产物写在本检查 `$HOH_EVIDENCE_DIR` 下。表中均为拟执行的 **product** 测试；FakeAgent/固定事件夹具须在记录中标明，不能声称 live。现阶段全部 NOT_RUN。

### REQ-010 · Workspace 工作台（4 条）

行为：顶部显示项目、已生效 mode、Run、基线身份；正文是统一的公开对话与工程活动，团队摘要可折叠；底部输入和 statusline。pending、输入四态与断线标记由 host 事实驱动，202 只说明接受。选择视图、展开和连接本身不调度任务。失败：API 断连保留带 stale 标记的最后快照与草稿；409 不重放用户命令为新动作；未知事件可见且不崩溃。

doc_refs：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/adr/ADR-014.md`、`docs/adr/ADR-031.md`；M3 依赖 REQ-007/017/028。提案 D1、D4、D11。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-010-01 / TEST-010-01 | tui-pty | 120×32 PTY；三Agent承接commander/developer/qa；水位H；既有COMPLETED Run最近ACCEPTED提交B，当前R的run.baseline=B0≠B、BUILD；公开U/A、task/candidate活动、带rationale与impact的decision | 打开Workspace，折叠团队，打开基线详情；Ctrl+P列动作，分别打开Task/Issue/Evidence抽屉；同快照TestBackend与PTY对照 | 同屏项目/R/B、BUILD、U/A/活动、输入/statusline；decision显示依据和影响；详情B0不替换顶栏B；默认一个主区、三职位摘要一致；面板列全部已实现动作且三抽屉列host对应ID/状态/证据引用、可返回；打开/切换的POST与Task/Attempt新增为0 |
| AC-010-02 / TEST-010-02 | tui-pty | 写 Attempt 运行中，M3 的模式与输入投递可控 | `/plan`；普通输入发给 commander，先接受再 delivered/applied；另一个输入被拒；模拟一次 expected_revision 409 | pending 时仍显示 BUILD 和待 PLAN，安全点后才显示 PLAN；两个输入按 ID 显示 QUEUED→DELIVERED→APPLIED、QUEUED→REJECTED，保留拒绝原因；显示实际角色/epoch/boundary；409 不改 mode，不自动重发新命令 |
| AC-010-03 / TEST-010-03 | tui-pty | H=100、草稿“继续核对”，应用105后断线，host到110；可触发真实索引重建与writer busy锁 | 恢复并重复105；debug钩子触发同一重建路径，以旧page cursor查询；独立验证SSE的过期响应解析；writer持锁令控制请求超busy_timeout，再解除 | stale/105期间保留草稿，补齐110后去stale、块不重复；page过期为410且顶层snapshot_watermark=110，重取快照保锚点；SSE 410沿同形契约，after/Last-Event-ID矛盾仍409 CONFLICT；busy时控制503 DB_BUSY且未接受、旧快照可绘制、脱离≤1s且无Cancel；恢复不自动重发新命令、不乐观加进度 |
| AC-010-04 / TEST-010-04 | unit | 已提交1..20，含未知kind、公开控制字符、pending/input；两种分批与重复5/10 | 比较一次重放/分批/快照增量的规范化投影 | 20个不同seq且逐字段相同；未知kind有UI_UNKNOWN_EVENT/seq；ESC/OSC转义可见、不执行；pending不当生效；事务未提交不发SSE的断言移至TEST-064-05真实host场景 |

### REQ-011 · Team/Focus 布局（5 条）

行为：Team 按配置职位展示绑定、epoch、可用性、当前任务与 pending；Focus 按完整身份查看公开会话，历史按需分页。布局转换保留草稿、展开状态和 grapheme 锚点。失败：角色删除或历史内容缺失显示缺失身份与返回入口；同 Agent 多职位不自动选择；窗口过小不 panic，也不修改执行状态。

doc_refs：`docs/03-ux.md`、`docs/04-architecture.md`、`docs/adr/ADR-022.md`、`docs/adr/ADR-024.md`、CR-025；M3 依赖 REQ-017。提案 D1、D9–D11。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-011-01 / TEST-011-01 | tui-pty | 默认 Workspace；六职位，cto/developer 同为 claude；选 developer，块 K 的 offset=12，草稿“尚未发送” | 依次 resize 为 80/99/100/120/159/160/180 列、32 行，选择 Team 分屏，再返回 Workspace | 80/99 为单区标签；100–159 最多主区+1 详情；160/180 用户选择后最多三列；默认 Workspace 不强制三窗；选择身份、K/12 与草稿保持；无 POST，所有行显示宽度≤窗口 |
| AC-011-02 / TEST-011-02 | tui-pty | 七职位含security_reviewer；developer epoch1 A1在途，READY developer Task T2待派，codex换绑待checkpoint | Team/Focus A1；对T2派发一次得到拒绝，再结束A1、生效epoch2并派A2 | 七职位可达；ROLE_BINDING_PENDING确由拒绝派发产生；pending保留claude/A1与待codex；历史Focus仍A1/epoch1，选择当前才A2/epoch2；内容不混；重用session时按Attempt边界显示，session过滤跨Attempt时逐块仍带attempt_id |
| AC-011-03 / TEST-011-03 | integration | 同一 agent 的两个 role、三个 session，A1 有公开 message/tool/handoff/error 及一项非公开内部字段；一段公开日志缺文件 | 按 A1 查询 Focus 各页，再查询另一 role；请求缺失 artifact | A1 的所有公开记录按 offset/seq 可遍历且不混入其他 role/session；内部字段不在响应；缺失段返回 ARTIFACT_NOT_FOUND，descriptor 保留身份和缺失提示，不显示为空成功；无 spawn/任务写入 |
| AC-011-04 / TEST-011-04 | tui-pty | 80×24、角色选择与草稿状态；可访问状态页 | resize40×8、1×1再120×32；每个小尺寸输入字素“a”、打开状态页、关闭，再选择历史已移除角色 | 40×8显示“窗口过小”及“输入 / Ctrl+C退出”；1×1仅显示ASCII !，Ctrl+P面板提供可逐字符滚动的ASCII诊断 small terminal/input/Ctrl+C exit 与状态字段英文键和值（不用一格截断宽字）；各尺寸接受a且不提交，Ctrl+C选择/脱离仍可达；被省略的四组事实在可滚动状态页逐字段可读；恢复后草稿新增两个a、锚点保持，历史角色有标记；无panic/越界/Run修订变化 |
| AC-011-05 / TEST-011-05 | integration | 每个codex App Server/受支持exec、claude、pi模式的版本绑定兼容fixture；含public message/tool开始结束、compact/wait/usage有与无、secret标记、两role/epoch；真实runner与Persistence，非直接注入UI事件 | 从native frame入口逐帧/重复/超大正文输入，队列背压；在提交前后kill/reopen、取消、EOF；从持久公开块读出 | 三家按§2.2白名单输出逐字段/顺序/身份相同，启动和结束工具均可查、未知能力显式null，secret/隐藏字段不入API或公开对象；未结束Attempt即可读到已提交消息/活动；队列≤256/1MiB、段≤1MiB；重复无重块、旧epoch不更新当前活动；已提交块重启不丢，未提交尾段有缺口而非完整成功；fixture标product+synthetic/recorded来源，不称live |

### REQ-012 · 可展开内容块（4 条）

行为：message/task/tool/diff/test/evidence/decision/handoff 块都有稳定身份；八类均必须支持展开、复制、搜索、全屏与返回。键盘与鼠标调用同一动作。展开只读，不执行工具或测试；结果引用候选、TEST 与原始证据。失败：缺失/损坏/非文本/权限错误在原块展示原因，保留返回和重试；旧候选标历史/失效，不自动切到新候选同名文件。

doc_refs：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/10-acceptance.md`；M3 依赖 REQ-032/034。提案 D1、D4、D7、D9。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-012-01 / TEST-012-01 | integration | §5.1参考环境/release串行阶段；八类块各一，首尾标记X，Tool20MiB；Evidence C1；父锚点K/200 | 各以键盘与SGR鼠标展开→分页→全文搜索X→全屏→Esc返回→复制文件，追加另一块；真实PTY采≥100次首屏 | 八类动作均可达且不执行工具/测试；返回K/200、首尾可达、导出字节/hash等于公开纯文本；首屏p95≤500ms且缓存有界；键鼠指向相同身份；环境不符DEGRADED诊断/退出3、AC不通过，禁止静默PASS |
| AC-012-02 / TEST-012-02 | integration | 20MiB+17字节完整UTF-8对象；“中文X”跨256KiB与8MiB边界各一处、末尾一处，连续重叠匹配aaa；NUL/无效UTF-8二进制B含唯一ESC/OSC标记 | 分Range拼全文；search limit=1续扫到结束、重复cursor；无匹配；请求B搜索并用PTY打开B | 拼接hash正确无缺重字；每个匹配的byte_offset精确、跨窗口不漏不重、上下文合法UTF-8≤256B；无匹配完成为空；B为422 ARTIFACT_NOT_TEXT；PTY只含metadata/下载动作，捕获中B原始控制标记字节为0 |
| AC-012-03 / TEST-012-03 | tui-pty | Evidence缺文件、摘要不符、认证403、对象chmod造成EACCES、DB_BUSY；旧C1与新C2同文件名；65KiB公开文本 | 打开/重试/返回；文件复制成功与不可写目录失败；尝试设置osc52；移除exports忽略规则再复制 | 分别显示404 ARTIFACT_NOT_FOUND、409 ARTIFACT_INVALID、原403、503 ARTIFACT_UNREADABLE(permission)、503 DB_BUSY；无PASS/空成功；C1不换C2；65KiB文件复制完整，失败无成功消息；osc52配置拒绝、无OSC/DCS输出；忽略失效拒绝导出且不产生未跟踪文件；草稿锚点不变 |
| AC-012-04 / TEST-012-04 | unit | 八类块、100 次追加、10 次 resize，焦点为第三块；正文含中文/组合字符；选中块被过滤或其对象回收 | 对展开/折叠/全屏/返回/搜索状态作纯动作序列；重复同一提交事件 | block_id 与 grapheme_offset 不因前插/重复事件漂移；offset 超过新正文时钳到最后字素边界并提示；被过滤保留隐藏锚点，取消过滤可返回；不可恢复块明确提示并选 seq 最近的前驱（无前驱选后继），不跳到无关 Run |

### REQ-013 · 三级时间线（5 条）

行为（D3）：Project 看当前项目跨 Run 的规格、候选、Issue、决定、轮次结果和 Run 生命周期；Iteration 看选中 Run/iteration 的工程事实（含 Task、候选、测试、证据）；Agent 看明确 role/epoch/attempt 的公开消息、工具与交接。项目级事实没有 iteration 时不塞进某个轮次。按 seq 排序，时间戳仅展示。每项可沿类型化引用定位需求、Issue、Decision、commit、Evidence；保留过滤条件和返回锚点。

失败：非法过滤/cursor 明确拒绝；合法无匹配显示空；未知事件有诊断块；broken/orphan/stale 引用不能伪装成可用证据。doc_refs：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`contracts/openapi.json`；M3 依赖 REQ-032/033/034。提案 D1、D3、D4。

#### 分类表与完整集合（D3）

分类由kind白名单决定，**带attempt_id不意味着Agent层**。下表是完整分类规则，P=Project、I=Iteration、A=Agent；I还要求run/iteration归属，A还要求明确role/epoch/attempt，项目级null身份不猜补。多层事件只在所列层出现。具体名字从当前生产者和CR-026提取；新kind若未列入则走最后一行诊断，不靠前缀自动解释为成功。M4-T03实现此表，M4-T06消费，M4-T09只负责归属和public事实。

| kind（同格逗号分隔为独立名字） | 层 | 规则 |
|---|---|---|
| spec.changed, spec.index_failed, spec.generated_drift, spec.drafting, spec_review.opened, spec_review.reported, run.created, run.started, run.result, run.incomplete, run.cancel_requested, run.cancelled, run.pause_requested, run.paused, run.resumed, run.waiting, run.blocked, run.blocker_claimed, run.blocker_released, run.control, run.baseline, run.mode_change_pending, run.mode_changed | P | 规格/Run里程碑；不因在轮内发生而塞进I |
| candidate.frozen, candidate.accepted, candidate.rejected, candidate.stale, candidate.reconcile_failed, candidate.reconcile_recovered, issue.opened, issue.reopened, issue.resolved, issue.consumed_by_rework, issue.observation, decision.applied, decision.rejected, iteration.completed, budget.risk, no_progress, milestone_ready, release_request | P,I | 有Run时进入该轮；无Run只P |
| task.accepted, task.blocked, task.cancelled, task.implemented, task.protocol_correction, task.retry_scheduled, task.reverify, task.rework, task.superseded, task.unblocked, task.migrated, task.replaced, attempt.created, attempt.start_refused, attempt.artifacts, attempt.outputs, attempt.workspace, attempt.workspace_start, attempt.tracking, attempt.moved, attempt.inactive, attempt.inactive_cleared, attempt.provider_reset, schedule.round, plan.imported, plan.write_violation, readiness.passed, context.packed, evidence.recorded, effect.started, effect.observed, repair.local, repair.protocol, repair.refusal_cleared, repair.reverify | I | 派发/执行/证据工程事实，attempt.created不进A |
| verification.attempt_started, verification.candidate_ready_for_join, verification.check_refused, verification.confirmations_dropped, verification.failures, verification.failures_abandoned, verification.failures_attached, verification.failures_consumed, verification.handoff_reissued, verification.joined, verification.output_invalid, verification.report, verification.result, verification.round_closed, verification.round_opened, verification.tree_checked, report.rejected, report.stale, task_result.rejected | I | 验证生命周期，不把report自述当accepted |
| team.configured, team.binding_applied, team.binding_blocked, team.binding_change_pending, team.binding_change_withdrawn, team.binding_excluded, team.binding_fallback, team.binding_pending, team.binding_refused, team.binding_unblocked, team.exclusion_skipped, team.session_bound | P | 显式配置/换绑事实 |
| input.accepted, input.queued, input.moved, input.delivered, message.applied, message.invalid, message.rejected, question.asked, question.answer_accepted, question.answered, need_decision.appended, request.deferred, request.rejected, blocked.withdrawn, pause.withdrawn, outbox.dead_letter, outbox.delivered_late | I | 命令/投递事实，用户正文由public.message单列A，不重复出两条消息 |
| handoff.prepared, handoff.delivered, handoff.superseded, handoff.undelivered, session.start | I,A | 工程交接可看公开包摘要；A只在显式接收/发送身份匹配时出现 |
| public.message, public.tool_started, public.tool_completed, public.segment_sealed, public.activity, public.usage, public.gap（M4新增） | A | §2.2公开规范化输出；segment更新同block不重复显示正文 |
| adapter.agent_start, adapter.blocked, adapter.degraded, adapter.diagnostic, adapter.error, adapter.item, adapter.prompt_refused, adapter.result, adapter.server_request, adapter.turn, adapter.turn_failed, adapter.ui_data, adapter.ui_request, adapter.unknown_event（若生产落库） | A | 只显示去敏后的诊断/公开白名单；已映射的native item不二次渲染成消息；原始帧不是UI事实 |
| supervisor.allocated, supervisor.available, supervisor.blocked, supervisor.closed, supervisor.deferred, supervisor.horizon, supervisor.output, supervisor.output_discarded, supervisor.output_disposed, supervisor.output_failed, supervisor.preempt, supervisor.probe, supervisor.rearmed, supervisor.retrigger, supervisor.step_failed, supervisor.superseded, supervisor.turn_failed, supervisor.turn_limit, supervisor.unavailable, supervisor.withdrawn, supervisor.woken | I | 调度监督事实，不假称正在推理 |
| reconcile.attempt, reconcile.completed, reconcile.diagnostic, reconcile.run, reconcile.started, runner.diagnostic, runner.outside, runner.received, runner.refused, runner.unreaped, cancel.diagnostic, cancel.workspace, bridge.rejected, registry.turn_auth_ignored | I | 恢复诊断；无Run的host诊断改入P |
| 未列kind（包括未来扩展） | P；有Run另I；有明确Attempt另A | 恰一UI_UNKNOWN_EVENT块/seq，保留身份、经去敏的摘要和refs；不推导任务成功 |

F13 仍用12条事件，但把每条成员关系写全。I1=`R1/iteration/1`、I2=`R2/iteration/1`，S1/S2为session，epoch均1；第12条kind=`fixture.unknown`，仅夹具名称（不是新产品kind）。事件均经产品writer提交，spec/candidate/证据真实引用由其生产者建立；本表序号是该fixture规范化的局部seq标签，实际产品建立前置可能占用seq 1..L；fixture记录一一映射k→L+k，所有F13查询/画面显式设after_seq=L与watermark=L+12（序号范围过滤），因此前置不混入期望集合。后文H=12、追加13是该映射的简写，原始证据必须保留真实seq，不能删除前置事件。

| seq | kind | run / iteration | role / attempt / session | 层 |
|---|---|---|---|---|
| 1 | spec.changed | null / null | null | P |
| 2 | attempt.created（派发T1） | R1 / I1 | developer / A1 / S1 | I |
| 3 | public.message | R1 / I1 | developer / A1 / S1 | A |
| 4 | public.tool_completed | R1 / I1 | developer / A1 / S1 | A |
| 5 | candidate.frozen（commit=B） | R1 / I1 | null | P,I |
| 6 | evidence.recorded | R1 / I1 | null | I |
| 7 | issue.opened | R1 / I1 | null | P,I |
| 8 | decision.applied | R1 / I1 | null | P,I |
| 9 | iteration.completed | R1 / I1 | null | P,I |
| 10 | run.created | R2 / I2（开放） | null | P |
| 11 | public.message | R2 / I2（开放） | qa / A2 / S2 | A |
| 12 | fixture.unknown | R2 / I2（开放） | null | P,I |

下表包含每个非空成员组；任意组合过滤=同一行层集合与所有过滤器集合的交集，因此每个组合都有唯一完整seq集。跨Run的iteration/attempt组合是非法查询（§2），合法但没有成员才空；epoch2、第三session、未出现role等合法过滤结果明确为 `items=[]`。查询无run_id参数是全项目，不是“仅null”；null项目事件集合用下表核对，Run路由绝不包含它。

| 过滤器 | Project完整seq | Iteration完整seq | Agent完整seq |
|---|---|---|---|
| 无过滤 / 项目 | [1,5,7,8,9,10,12] | [2,5,6,7,8,9,12] | [3,4,11] |
| R1 | [5,7,8,9] | [2,5,6,7,8,9] | [3,4] |
| R2 | [10,12] | [12] | [11] |
| I1（R1） | [5,7,8,9] | [2,5,6,7,8,9] | [3,4] |
| I2（R2，开放） | [10,12] | [12] | [11] |
| A1 / S1 / developer / developer epoch1（各单独或其交集） | [] | [2] | [3,4] |
| A2 / S2 / qa / qa epoch1（各单独或其交集） | [] | [] | [11] |
| 项目事件run=null的成员核对 | [1] | [] | [] |
| R1+qa；R2+developer；role_epoch=2；合法未知session | [] | [] | [] |

`after_seq=L` 再取seq>L、`before_seq=B` 再取seq<B、`watermark=H` 再取seq≤H；排序升序，cursor绑定全部范围参数。范围过滤是T03 API/T06 Timeline的显式过滤，不是测试私自删掉已返回的事件。entity的refs关联到某Attempt不赋予事件该Attempt身份，因而不会让candidate.frozen突然进入A1过滤集。SSE补发只更新对应稳定块；已公开工具开始在完整fixture另有前置，不用省略的开始推断活动状态。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-013-01 / TEST-013-01 | tui-pty | F13，另有 REQ/issue/decision/commit/evidence 的完整追踪引用；120×32 | 依次选择 Project、R1/I1、A1；打开五类引用再返回；seq=8 后断线，补到 12 | Project 可见 [1,5,7,8,9,10,12]；I1 可见 [2,5,6,7,8,9]；A1 可见 [3,4]；五类详情的 ID/spec/candidate/hash 与来源一致；断线前后最终列表、过滤条件和锚点与连续读取相同 |
| AC-013-02 / TEST-013-02 | integration | F13含重复/逆序timestamp；limit2、H12，之后追加13；所有合法过滤值与明确空集 | 按下节完整期望集合翻页、重复cursor、改过滤/H、重建generation、试跨Run iteration/attempt | 严格seq序、每个并集精确等于表，无13混入；重复cursor同页；新/v1/timeline非法为400 QUERY_INVALID，旧Run路由非法仍404 BAD_REQUEST；两页路由generation过期410且顶层snapshot_watermark；SSE矛盾409、SSE过期410形状不变；合法空items=[]/next_cursor=null |
| AC-013-03 / TEST-013-03 | integration | 四条独立链：TEST缺evidence；当前修订中PASS绑定旧候选而非当前候选；批准新修订后旧修订验收节点；当前完整合法链 | 经refs往返详情，在新修订前后各查一次 | 标签唯一：broken（缺evidence）、orphan（当前修订的旧候选PASS）、stale（旧修订全部验收节点）、current（合法链）；先做新修订失效再判旧PASS，stale不另写orphan/历史；身份/原seq保留、非current不计accepted；不生成证据/执行检查 |
| AC-013-04 / TEST-013-04 | unit | F13 的持久事件、重复 4/9、未知 12；两种交付分批及同一 H 快照 | 比较全量折叠、快照+增量、断线补历史三种规范化投影；切过滤再切回 | 块序列/refs/身份/诊断一致；12 恰一诊断；切过滤不改变事件真值；无以 timestamp 排序、以 Agent 名合并 role 或从模型摘要创建状态的行为 |
| AC-013-05 / TEST-013-05 | integration | 两Run的事件交错；R1关轮前开放I1，R2开放I2；同Task重试跨R1两轮，含run_id=null事件；带既有iteration.completed的旧库 | 经单writer逐事务追加、关闭I1、在事务边界kill/reopen、重复通知并回填旧库 | 开放轮与关闭后key相同；每条事件严格按§2.1的Run+close区间归属，close归被关闭轮、下一条归下一轮；项目事件iteration=null；跨轮Task不被旧tasks.iteration_id吞并；重启/回填/在线集合相同、预算/关轮数不增；缺损边界诊断index_ready=false，不伪造完整结果 |

### REQ-014 · 真实进度状态栏（5 条）

行为与失败按决定提案 §3；状态栏可配置但不能隐藏真实进度/阻塞。标准和精简在窄屏优先保留四组事实，可在多行状态区换行；详细字段从状态页可达。doc_refs：`docs/03-ux.md`、`docs/01-prd.md`、`docs/05-data-api.md`、`docs/adr/ADR-029.md`、CR-025；M3 依赖 REQ-032/033/034/040。提案 D2、D8、D11。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-014-01 / TEST-014-01 | tui-pty | r1有RQ1(2AC)、RQ2(1AC)、RQ3(1AC)；C1范围仅RQ1/2且ACCEPTED，RQ3不在该Run任何Task中；C1接受后新增两个仅关联RQ1/2的blocked Task，现有5个done/共7个Task；OPEN2/REOPENED1/RESOLVED1；qa/pi配置模型requested-X、实际resolved-Y，未知费用/上下文 | 打开status/Focus；新C2与budget.risk；批准r2新增RQ4并删除RQ3（fixture模拟用户批准，不由UI擅删） | 先2/3 accepted、blocked=2/open=2/reopened=1；分子不取Task/AC数，blocked按Task唯一ID且Run级reason单列不双计；Focus显示resolved-Y，requested-X仅配置详情；费用/上下文未知带qa/pi；C2为0/3；r2为0/3、revision2/added1/removed1；旧计数仅历史；预算原因/上限可查 |
| AC-014-02 / TEST-014-02 | unit | 无spec、零AC、某AC FAIL、FROZEN/STALE/INVALID、旧spec PASS、spec_review PASS、TaskResult COMPLETED；另spec共3需求：同一ACCEPTED候选范围内一个全PASS、一个合规DEGRADED，另一个未纳入该Run任务范围的需求未满足 | 与host共用聚合函数，重复验收事实 | 不满足者分子0，无spec为0/0且未完成；3需求例为accepted=1/3、degraded=1，DEGRADED不进分子且在分母；只有当前spec+候选ACCEPTED且该需求所有AC PASS才加1；重复不增计 |
| AC-014-03 / TEST-014-03 | tui-pty | 80/120/180×32；三档，custom=[budget,candidate]；可识别H1/H2响应与独立Timeline seq | 切档重开，Focus/离开，候选更新和Issue重开并发 | minimal仅mode/阶段任务/n-m/blocked-issues四组；standard依次四组+git/candidate/elapsed/budget；custom四组+budget/candidate严格按序；窄屏可换行、超出可选项进状态页；保存后重开保持差异；Focus上下文只用该身份；状态栏/进度同帧只来自一个完整getRun响应H（探针可查），Timeline另显last_applied_seq不强称同H；非法数组拒绝保旧档 |
| AC-014-04 / TEST-014-04 | integration | 明确的工具开始/结束、服务等待、压缩、排队、空闲/进程 alive，及 context used=25/limit=100、used 缺失、limit 缺失三例；两个 Agent 只有一个可计美元 | 查询 progress/ui 活动与用量，再换 role_epoch | 状态分别为工具运行/等待服务/压缩/排队/空闲；仅 alive 为活动未知，Commander 无 turn 为事件监测中；25/100=25%，缺任一数值为 null；成本标已知部分并列缺项身份，不能当总额；换绑不沿用旧 epoch 的用量 |
| AC-014-05 / TEST-014-05 | adapter-live | 当前冻结候选host与三家已认证的真实native会话，各给定只读小文件与唯一公共回复标记；记录compat版本/session/resolved model | 各调用原生工具读该文件并给公共消息，结束前后读持久块/activity；detach/reconnect，再对一会话取消 | codex/claude/pi均有可核对原生session，公开回复/工具开始结束经真实producer落库，持久身份/seq与原始公开帧对应；已有在线usage/compact/wait字段一致、未提供则unknown而非猜测；detach不取消，显式取消保留前缀及终结事实；缺认证为阶段BLOCKED/检查非零、逐测试FAIL诊断而非PASS；live-sessions.json与原始证据来自adapter-live，不以fixture替代 |

### REQ-015 · 输入与终端兼容（5 条）

行为：Unicode grapheme/显示宽度、多行编辑、括号粘贴、不依赖增强键盘协议；resize/网络分片/异常退出都不丢原草稿或误发命令。teardown 覆盖 raw、alternate screen、光标、bracketed paste 和新增 mouse reporting；正常退出、SIGTERM、SIGHUP、panic/unwind/初始化失败均恢复可恢复的终端状态。SIGKILL/断电不能执行 teardown，不能宣称可恢复（恢复 host 另属 REQ-041）。失败时沿用 REQ-004 的退出码，不新增用户命令。

doc_refs：`docs/03-ux.md`、`docs/01-prd.md`、`docs/09-testing.md`、`docs/adr/ADR-014.md`、`docs/adr/ADR-031.md`；M3 依赖 REQ-007/017。提案 D9、D10。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-015-01 / TEST-015-01 | tui-pty | 真实外层PTY与固定版本终端buffer；四路径直连/SSH/tmux/SSH内tmux；80/120/180；A中🙂é、末列中、👩‍💻(ZWJ)、✈️(VS16)、两行CRLF粘贴 | 在外层捕获实际绘制流和光标列/换行；移动删除、resize、paste再Enter；各路径debug panic；外层shell读哨兵 | A中🙂é宽6，ZWJ/VS16各宽2；不是内部CellWidth自证：从外层buffer光标列判，末列宽字符移到下一行且不留半字；Backspace按完整字素；paste前命令0、Enter后恰1且LF两行；panic101；SSH退出后本地tty、tmux退出后外层shell stty与进入前相等，屏幕模式关且哨兵完整；四路径任缺一为FAIL非零 |
| AC-015-02 / TEST-015-02 | tui-pty | 无增强键盘协议的四路径；草稿甲，列表/临时层；可区分Shift+Enter的增强变体 | Ctrl+J输入乙、Ctrl+J输入丙后Enter；独立用Esc后50/100/500ms再Enter；列表Enter、编辑器?、非空/空Ctrl+C；SGR报告后立即接Enter及中文键，坐标300；畸形SGR/X10 | 首例“甲\n乙\n丙”恰1 SendInput；每个慢Esc→Enter均0提交且草稿保持、提示未发送，再编辑后显式Enter才提交；增强Shift+Enter换行；列表Enter只展开、?是文字、Ctrl+C清空或选择，未选控制无Pause/Cancel；完整SGR不吞后键、X10跳过、畸形序列不无界等待读取 |
| AC-015-03 / TEST-015-03 | integration | PTY按UTF-8/CRLF/起止标记边界分块（不声称内核保留单字节write）；草稿保留；合法最终1MiB与+1B；64MiB无结束paste；三家adapter编码/回显fixture | 合法/超限paste，结束后普通键；未结束时SIGTERM；从HTTP受理经真实投递链对每adapter测试§4的256KiB/1MiB/最坏转义与不可读文件，另测超8MiB请求被400 BAD_REQUEST拒绝 | 完整paste不自动提交，超限原稿不变且UI_INPUT_TOO_LARGE；残缺保留≤1MiB+6B，丢尾到结束或信号，RSS增≤16MiB（RSS子例由本TEST在§5.1串行release阶段运行）；后续键正常、无斜杠执行；每adapter inline≤256KiB且编码≤768KiB、否则短文件引用≤16KiB，文件字节/hash精确、每帧≤1MiB不触发PROTOCOL_FRAME_TOO_LARGE；超8MiB请求400 BAD_REQUEST、无202且草稿保留，文件错误不APPLIED；fixture标product，非live；环境不符RSS子例DEGRADED/退出3、整AC不通过 |
| AC-015-04 / TEST-015-04 | tui-pty | 外层shell stty基线、独立后台Run；初始mouse/paste/alt已被前进程留下；mouse=true；debug故障点 | 先启动；分别脱离、外部SIGINT/SIGTERM/SIGHUP、无信号panic、alt后初始化失败、部分输出失败、信号与I/O同时失败；外层shell读取哨兵 | 启动关闭1000/1002/1003/1006/2004/1049后才进入；退出可写PTY上stty逐字相同、所有开启模式关闭/光标显示；正常0、SIGINT130/SIGTERM143/SIGHUP129、无信号panic101、无信号I/O74；并发I/O错误时信号码优先；恢复只执行一次、不吞哨兵；后台无Pause/Cancel且新客户端可查询 |
| AC-015-05 / TEST-015-05 | integration | §5.1参考VM、release串行，百万库；80×24与180×40各≥1000样本，50ms增量合并负载 | 并发提交事件并编辑/滚动/resize，记录输入→外层屏幕、commit→seq可见；插入tool/decision/acceptance边界 | 各尺寸输入p95≤100ms、事件p95≤500ms；每个合并窗口≤50ms或16KiB先到，关键事件计数完整；无丢键/重复提交，非renderer独立计时；环境不符DEGRADED/退出3不通过，真实超限FAIL保留样本 |

兼容矩阵 D10：M4在本VM Ubuntu26.04.1 x86_64的四路径每次同检查完整执行，TERM为xterm-256color/tmux内tmux-256color，UTF-8 locale，记录所有版本与每路径原始PTY。缺ssh/tmux/locale直接FAIL非零；不写非法BLOCKED/NOT_RUN test-result，不靠返回ok跳过。CI在现有cargo测试前准备tmux、openssh-server、locale与隔离sshd会话；无需把非参考CI称为性能验收。Ubuntu24.04的四路径由现有docs/23指派的M6-T03/M7-T04在实际24.04 VM对最终候选执行并收齐平台证据，本次M4结果不宣称已验证24.04、不从不同环境拼接一条TEST的PASS。批准时在这两个既有任务done_when追加明确的矩阵义务、不转移它们已有AC/检查owner；最终v1无两版证据不得通过。所有sshd端口/tmux会话独立且结束清理。

### REQ-016 · 无障碍与键位（4 条）

行为：重要操作全部有键盘路径；状态有文字与形状/ASCII 标记，不只靠颜色；焦点可见；NO_COLOR、无鼠标与 ASCII 图标同时可用。自定义键位按 scope 校验并可恢复默认。失败：冲突列出两个 action/scope/key，保存整体拒绝；非法配置不破坏现有 Run；没有终端能力不显示虚假的启用状态。

doc_refs：`docs/03-ux.md`、`docs/01-prd.md`、`docs/09-testing.md`、`config/hoh.example.toml`；M3 依赖 REQ-007/017/047。提案 D8、D9、D10。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-016-01 / TEST-016-01 | tui-pty | NO_COLOR=1、ui.mouse=false、ui.icons=ascii，80×32；有效新键 `list.copy=c`，另有冲突配置 | 不用鼠标遍历共同动作清单：四视图、角色/会话、三抽屉、块操作、模式、历史、设置、提交及暂停/恢复/取消/脱离；保存冲突后恢复默认 | 清单每项均产生指定界面或对应唯一命令，证据逐项记录；无颜色 SGR/鼠标启用序列，状态有中文文字和 ASCII 标记；c 可复制，冲突整体拒绝；恢复后 y 可复制、c 只是未绑定键；后台控制只发生于显式选择 |
| AC-016-02 / TEST-016-02 | unit | keybinding 表：同 list 同 key 两动作；global 与 editor 重叠；editor/list 同 key；未知 key/action/scope；覆盖 Ctrl+C/Esc/Enter/Ctrl+J；将面板重绑为未占用 Ctrl+O，以及使面板无有效入口的配置 | 校验各表，分别试保存；读取错误 details | 前两种冲突与未知/保留覆盖/无面板入口均拒绝，给出准确 action/scope/key；互斥 editor/list 同 key 及合法 Ctrl+O 重绑允许；失败后有效映射逐字段不变；所有默认动作仍可从命令面板到达 |
| AC-016-03 / TEST-016-03 | tui-pty | editor/list/dialog 各 scope，草稿含 `? / f y n`；重绑 list.search 为 Ctrl+F，面板改为 Ctrl+O | Tab 移焦，Ctrl+O 打开面板，Esc 返回；使用重绑键，保存并重开客户端，在面板恢复默认 | 编辑器文字不被列表/帮助热键吞掉；焦点可见且 Esc 回到原草稿/锚点；重开后绑定仍有效；恢复后 Ctrl+P 再次打开面板；只清键覆盖，team/budget/Prompt/mode_cycle、配置注释与其他键均不变；ui-only保存不产生ConfigureTeam/新team_revision，CAS冲突拒绝且原文件不变；mode.cycle重绑后顺序仍遵守ui.mode_cycle |
| AC-016-04 / TEST-016-04 | unit | 同一投影包含 PASS/FAIL/BLOCKED/UNKNOWN/STALE/DEGRADED/REOPENED、选中和未选中项；80/120/180 列 | 以彩色与 NO_COLOR+ASCII 两套样式渲染 TestBackend | 每种状态在去样式后的文字中仍可区分，选中项有 ASCII 焦点标记；禁色不含前景/背景色，保留必要非颜色属性；无超过显示宽度、无必需动作只有图标无标签 |

### REQ-064 · 可观测性与容量（5 条）

行为：结构化索引+对象、seek 分页和虚拟列表、诊断日志轮换、所有被引用证据保护、慢读者背压和空间错误可诊断；数值采用 §1/§5 待答提案。失败：不接受半写对象为有效证据；DB 满/忙不承诺成功；cursor 过期明确 resync；GC 无法确定安全时保留。百万历史数据不能在启动时全量反序列化。

doc_refs：`docs/01-prd.md`、`docs/04-architecture.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/14-resilience.md`、`contracts/sql/001_initial.sql`；M3 依赖 REQ-032/033/034。提案 D1、D4–D6。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-064-01 / TEST-064-01 | integration | §5.1参考环境/百万事件与段夹具；旧schema数据、最终TUI；100Run×10轮；受限ENOSPC设施 | release串行升级/回填、按Run/iteration/attempt各查100页，limit100/500/非法0/501；Range首尾/缺文件/416；同夹具磁盘失败/慢客户端/轮换/引用保护/虚拟列表子例 | 新/v1/timeline非法400 QUERY_INVALID、旧Run timeline非法404 BAD_REQUEST，416仅ARTIFACT_RANGE_INVALID与Content-Range组合；分页≤limit、seq完整、H固定，查询p95≤500ms；EXPLAIN用指定索引、每页描述反序列化≤500/正文≤256KiB，无全库加载；kill→reconcile和ready≤30s、构建/回填各≤600s；升级权威行/hash不变、失败回滚/旧binary拒读；子例包括日志≤8MiB×5、引用对象hash不变、描述≤1000/正文≤4MiB、磁盘无半登记、慢读不阻塞快读；环境不符DEGRADED/退出3，不PASS |
| AC-064-02 / TEST-064-02 | integration | §5.1同次百万库克隆，单块100MiB，首/中/尾唯一标记；release真实PTY | 开Workspace/Focus，末页/搜索/往返滚动60s，每种尺寸输入≥1000次，idle10s采RSS | 描述≤1000/正文≤4MiB/在途页≤2，初始不回放百万正文；所有标记可达/锚点稳定；输入p95≤100ms；idle host+TUI总RSS≤150MiB且分别记录/各≤150MiB，活动各≤256MiB；不含Agent/浏览器，不排除TUI；环境不符DEGRADED/退出3，不通过 |
| AC-064-03 / TEST-064-03 | integration | 6×8MiB+1诊断字节；§5每一引用根各一对象、仅登记无引用对象、未登记临时文件；时钟47/48/49h | 轮换/GC；扫描后加引用、打开pin并发GC；引用解析故障；GC登记tombstone前后kill/reopen | 诊断最多5段/每段≤8MiB；全部引用/活动/pin对象hash不变；47h孤立保留、≥48h且复查无引用才删；collected对象404 ARTIFACT_NOT_FOUND reason=collected，未知ID/丢文件原因不同；解析失败保留诊断；重复GC幂等；events/evidence/acceptance删除0 |
| AC-064-04 / TEST-064-04 | recovery | 已有E/待写N、min_free32MiB、1MiB流段；真实ENOSPC与注入EDQUOT；临时写/fsync/rename/登记故障 | 逐点失败或kill/reconcile；低空间但状态事务有余量时分别Pause/Cancel；状态事务自身满盘再试；释放空间重试 | E可读hash不变、N不留引用缺正文；低空间新对象/新写Attempt为STORAGE_LOW_SPACE、带available/required，状态事务能提交时Pause/Cancel均202且持久；事务真失败503 DB_FULL无202，UI未接受仍可脱离；释放后原Task可重试、只登记一次；48h孤立策略；缺真实ENOSPC设施FAIL非零，EDQUOT明确注入 |
| AC-064-05 / TEST-064-05 | integration | §5.1参考环境release；快/慢SSE各1，100events/s共10000条8KiB公开正文，含tool/decision/acceptance；单流应用pending≤500项且序列化字节≤1MiB；另暂挂未提交事务X | 慢客户端停读30s直至TCP背压后恢复；独立断线后按last seq重连；查询矛盾after/Last-Event-ID；事务X提交前后分别观察 | 唯一背压行为为停止拉取/填pending（不因满队列主动丢弃或断开），批读取先按字节限界，探针含pending与当前序列化项、不含另报的内核/TCP缓冲；快流每条提交完整且p95≤500ms；慢流恢复/去重并集完整；冲突409 CONFLICT；X提交前对任一SSE不可见、提交后可见；202对应已提交事件缺失0；环境不符DEGRADED/退出3、AC不通过 |

AC-064-01 完整保留原“百万事件/磁盘满/慢客户端/日志轮换/引用保护/分页虚拟列表”组合，故移到最终UI之后的M4-T10；子例复用 -02/-03/-04/-05 驱动，仍在同次integration中为TEST-064-01记录全部独立断言；-04 的完整崩溃矩阵独立归 recovery，不能借 integration 的日志当 recovery 通过。旧 AC-012-01、AC-015-01 的组合范围同样保留，新增 AC 负责补足边界，不删除旧 AC 的含义。

## 实现任务拆分与应用清单

### 可落盘的任务提案（D12）

保留M4-T01/T02/T03/T04标识，新增T05–T11。下表的检查是AC要使用的既有检查，**不是 tasks[].checks**：`validate_bundle.py::ownership_problems`要求每个check仅一个owner，现有unit/integration=M0-T03、tui-pty=M1-T08、recovery=M2-T05、adapter-live=M1-T03、independent-qa/review=M2-T10保持。verify不得含acceptance，不能把复验全阶段误写成AC拥有权。九个implement各≤2需求/≤8AC，每AC恰一owner。M4-T01仍依赖M3-T05，不提前领取。

| 任务 | requirements | acceptance完整集合 | AC使用的既有检查（不写入tasks[].checks） | depends_on |
|---|---|---|---|---|
| M4-T09 · 迭代归属与原生公开流 | REQ-011、013 | AC-011-05、AC-013-05（2） | integration | M4-T01 |
| M4-T03 · 查询、对象与容量底座 | REQ-064 | AC-064-03、04、05（3） | integration、recovery | M4-T09 |
| M4-T02 · Workspace与Team/Focus | REQ-010、011 | AC-010-01..04、AC-011-01..04（8） | tui-pty、unit、integration | M4-T03 |
| M4-T05 · 八类内容块 | REQ-012 | AC-012-01..04（4） | integration、tui-pty、unit | M4-T02 |
| M4-T06 · Timeline与真实状态栏 | REQ-013、014 | AC-013-01..04、AC-014-01..04（8） | tui-pty、integration、unit | M4-T05 |
| M4-T07 · 输入传输与终端兼容 | REQ-015 | AC-015-01..04（4） | tui-pty、integration | M4-T06 |
| M4-T08 · 无障碍与键位 | REQ-016 | AC-016-01..04（4） | tui-pty、unit | M4-T07 |
| M4-T10 · 最终界面的性能/容量组合 | REQ-015、064 | AC-015-05、AC-064-01、AC-064-02（3） | integration | M4-T08 |
| M4-T11 · 公开活动真实三Agent验证 | REQ-014 | AC-014-05（1） | adapter-live | M4-T06 |

所有实现done_when都要求冻结候选的指定检查有逐测试证据、独立审查无未解决critical/high；自测不是验收。以下JSON是替换M4-T02/T03、追加T05–T11及替换T04的完整提案；M4-T01不在片段中。未写checks意味着不认领任何既有check；T04仅列requirements/depends_on与验证出口，不带acceptance/checks。

```json
[
  {
    "id": "M4-T09",
    "milestone": "M4",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Iteration attribution and persisted native public streams",
    "depends_on": [
      "M4-T01"
    ],
    "requirements": [
      "REQ-011",
      "REQ-013"
    ],
    "acceptance": [
      "AC-011-05",
      "AC-013-05"
    ],
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。交付§2.1/2.2生产接线、迁移003运行器/回填断点与public段登记；每个native模式的版本字段映射、真实runner fixture、重复/取消/恢复/旧epoch/去敏与有界队列；不改M3投递或关轮算法。回归REQ-024/025/026/027/036/041/062和CR-026迁移002。生命周期与并发按docs/19双独立审查。"
  },
  {
    "id": "M4-T03",
    "milestone": "M4",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Typed UI queries, object lifecycle and bounded SSE",
    "depends_on": [
      "M4-T09"
    ],
    "requirements": [
      "REQ-064"
    ],
    "acceptance": [
      "AC-064-03",
      "AC-064-04",
      "AC-064-05"
    ],
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。交付§2错误与Range/search/evidence/timeline路由、分类表索引、引用/GC/tombstone/低空间原因；交付§5.1唯一release测量基础设施/runner与ENOSPC设施；SSE按字节限界。新配置与example同提交，回归TEST-018-01、REQ-005/035/036/037/042/043/058/062。对象并发按docs/19双独立审查。"
  },
  {
    "id": "M4-T02",
    "milestone": "M4",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Workspace, Team and Focus clients",
    "depends_on": [
      "M4-T03"
    ],
    "requirements": [
      "REQ-010",
      "REQ-011"
    ],
    "acceptance": [
      "AC-010-01",
      "AC-010-02",
      "AC-010-03",
      "AC-010-04",
      "AC-011-01",
      "AC-011-02",
      "AC-011-03",
      "AC-011-04"
    ],
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。交付快照增量、断线/DB_BUSY、布局/身份/小屏、Ctrl+P动作注册表与Task/Issue/Evidence抽屉；自有poll SGR鼠标解码与ADR-014修订。保留M3模式/输入/批准/历史入口，不直接写host状态造测试。回归REQ-001/005/007/017/018。"
  },
  {
    "id": "M4-T05",
    "milestone": "M4",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Eight public block actions and bounded rendering",
    "depends_on": [
      "M4-T02"
    ],
    "requirements": [
      "REQ-012"
    ],
    "acceptance": [
      "AC-012-01",
      "AC-012-02",
      "AC-012-03",
      "AC-012-04"
    ],
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。八类同一动作、Range装配/搜索边界、纯文本文件复制、引用错误和锚点；正文缓存与虚拟列表实现以小夹具回归，最终百万容量归T10；ui.clipboard/file加载与example同提交、exports守卫与占用诊断。不发OSC/DCS。TEST-012-01在既有integration的release阶段完整含PTY测量。"
  },
  {
    "id": "M4-T06",
    "milestone": "M4",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Timeline navigation and factual statusline",
    "depends_on": [
      "M4-T05"
    ],
    "requirements": [
      "REQ-013",
      "REQ-014"
    ],
    "acceptance": [
      "AC-013-01",
      "AC-013-02",
      "AC-013-03",
      "AC-013-04",
      "AC-014-01",
      "AC-014-02",
      "AC-014-03",
      "AC-014-04"
    ],
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。消费T09归属/公开活动与T03分类API，F13全过滤/引用，progress聚合、三档字段/水位探针；配置与example同提交；保留M3追踪标签、规格全量失效与旧acceptance的AC计数。回归TEST-018-01、REQ-032/033/053，不把adapter活动来源写成M2已提供。"
  },
  {
    "id": "M4-T07",
    "milestone": "M4",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Multiline input transport and terminal compatibility",
    "depends_on": [
      "M4-T06"
    ],
    "requirements": [
      "REQ-015"
    ],
    "acceptance": [
      "AC-015-01",
      "AC-015-02",
      "AC-015-03",
      "AC-015-04"
    ],
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。交付编辑器/有界paste、每adapter inline/frame预算与文件引用、启动复位/teardown/外层宽度四路径；只扩展M3投递的传输，不改变四态。§5.1运行TEST-015-03的完整release场景；功能PTY的panic入口仍debug专用。准备CI的tmux/sshd/locale；回归REQ-004/017/024/025/026/043；生命周期双独立审查。"
  },
  {
    "id": "M4-T08",
    "milestone": "M4",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Accessible actions and atomic keybinding settings",
    "depends_on": [
      "M4-T07"
    ],
    "requirements": [
      "REQ-016"
    ],
    "acceptance": [
      "AC-016-01",
      "AC-016-02",
      "AC-016-03",
      "AC-016-04"
    ],
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。键盘遍历所有动作、NO_COLOR/ASCII、scope冲突、项目ui配置CAS原子写保注释、ui-only重载、恢复默认；mode.cycle可重绑但不改ui.mode_cycle。配置/example同提交、回归TEST-018-01与输入teardown。"
  },
  {
    "id": "M4-T10",
    "milestone": "M4",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Final UI performance and million-event acceptance scenarios",
    "depends_on": [
      "M4-T08"
    ],
    "requirements": [
      "REQ-015",
      "REQ-064"
    ],
    "acceptance": [
      "AC-015-05",
      "AC-064-01",
      "AC-064-02"
    ],
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。完成最终UI+百万库组合、迁移/回填30s与600s上界、首中尾日志/虚拟列表/轮换/保护/慢读/磁盘失败、核心总RSS和1000样本交互测量；全部经integration release串行阶段，参考环境不匹配不PASS；REQ-068/M7仍独立验收原门槛。"
  },
  {
    "id": "M4-T11",
    "milestone": "M4",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Live native stream persistence across three agents",
    "depends_on": [
      "M4-T06"
    ],
    "requirements": [
      "REQ-014"
    ],
    "acceptance": [
      "AC-014-05"
    ],
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。扩展既有adapter-live到host→原生→公开持久事实的真实链，三家session/模型/公开工具/用量来源证据；复用T09生产者而非第二份解析器；未知能力如实记录，缺认证不PASS，不改live check owner或范围。"
  },
  {
    "id": "M4-T04",
    "milestone": "M4",
    "kind": "verify",
    "owner_role": "qa",
    "writer": false,
    "title": "M4 independent QA and review on one frozen candidate",
    "depends_on": [
      "M4-T09",
      "M4-T03",
      "M4-T02",
      "M4-T05",
      "M4-T06",
      "M4-T07",
      "M4-T08",
      "M4-T10",
      "M4-T11"
    ],
    "requirements": [
      "REQ-010",
      "REQ-011",
      "REQ-012",
      "REQ-013",
      "REQ-014",
      "REQ-015",
      "REQ-016",
      "REQ-064"
    ],
    "done_when": "同一冻结候选由不同于开发者且彼此独立的QA/Reviewer验证全部37条AC；收取unit/integration/recovery/tui-pty/adapter-live与适用build/static、independent-qa/review的真实证据。critical/high必须为零；开修复任务不算通过。缺前提为BLOCKED/INCOMPLETE，修复后新候选重新验证；M4不签v1完成。"
  }
]
```

### 每条 AC 的依赖闭包

共同祖先是M4-T01→M3-T05→CR-026最终M3实现图→V01-T02→M2/M1，正式应用须用最终图机械核对。这里不宣称尚未合入的CR-026已是当前台账。check提供者见上表；release测量基础设施归T03，不改变integration归属。T09不依赖后续查询API：其fixture从真实Persistence读取规范化事实；T03才提供API给T02以后。大容量完整UI验收放T10，避免在T03验收未来界面。

| AC | owner / check_id | 本任务或祖先的能力 |
|---|---|---|
| AC-010-01 | M4-T02 / tui-pty | T02工作台/面板/三抽屉；T03读API；T09公开生产者；M2-T12 host、M2-T13项目baseline、M3-T07 run.baseline |
| AC-010-02 | M4-T02 / tui-pty | T02展示；M3-T09 SetMode/SendInput四态；M2-T06安全点、M2-T17 epoch |
| AC-010-03 | M4-T02 / tui-pty | T02重连/草稿/非阻塞控制；T03水位/游标/DB_BUSY；M2-T05恢复、M2-T12 SSE |
| AC-010-04 | M4-T02 / unit | T02纯投影；T09公开块；M3-T09 pending/input事实；事务可见性在064-05 |
| AC-011-01 | M4-T02 / tui-pty | T02布局/锚点/Team；M1-T08 PTY、M2-T07团队 |
| AC-011-02 | M4-T02 / tui-pty | T02身份过滤；T03会话查询；T09公开块；M2-T17 checkpoint、M2-T21拒绝码 |
| AC-011-03 | M4-T02 / integration | T03 Focus分页/Artifact API；T09持久公开块；M2-T07/17身份 |
| AC-011-04 | M4-T02 / tui-pty | T02小屏/可滚动状态页/输入；M1-T08终端；M2-T07角色 |
| AC-011-05 | M4-T09 / integration | T09 adapter→runner→Persistence规范化、段事务与恢复；M1-T03/04/05适配器、M2-T07runner、M2-T01/05存储恢复、M3-T04迁移运行器 |
| AC-012-01 | M4-T05 / integration | T05八类块/虚拟列表/纯文本；T02键鼠/PTY；T03查询/测量runner；M3-T07追踪 |
| AC-012-02 | M4-T05 / integration | T05 UTF-8/二进制PTY呈现；T03 Range/search/分类；M2-T16对象 |
| AC-012-03 | M4-T05 / tui-pty | T05复制/exports守卫；T03各错误；T02锚点；M3-T02失效 |
| AC-012-04 | M4-T05 / unit | T05纯动作锚点；T09稳定block_id；T02本地状态 |
| AC-013-01 | M4-T06 / tui-pty | T06三级导航；T03分类查询；T09轮次归属；T05详情；M3-T07追踪 |
| AC-013-02 | M4-T06 / integration | T06完整过滤；T03 H/generation/错误API；T09归属；M2-T12 SSE |
| AC-013-03 | M4-T06 / integration | T06 refs；M3-T02规格/全失效、M3-T07 broken/orphan/stale；T03工件错误 |
| AC-013-04 | M4-T06 / unit | T06投影；T03分类表；T09身份；T02增量状态 |
| AC-013-05 | M4-T09 / integration | T09关轮区间与在线/回填索引；M2-T18关轮事件；M2-T01/05事务恢复；M3保留原关轮语义 |
| AC-014-01 | M4-T06 / tui-pty | T06需求/阻塞计数；T09活动身份；M2-T18验收、M2-T21预算、M3-T02规格、M3-T07追踪 |
| AC-014-02 | M4-T06 / unit | T06聚合；M2-T18有效验收；M3-T02修订、M3-T06规格审查独立身份 |
| AC-014-03 | M4-T06 / tui-pty | T06三档/状态水位；T03单快照；T02 Focus；M2-T07配置 |
| AC-014-04 | M4-T06 / integration | T09规范化与持续activity/usage生产者；T06活动投影；M2-T17 epoch、M2-T21预算、M3-T10 no_progress；M2-T07只提供runner |
| AC-014-05 | M4-T11 / adapter-live | T09真实规范化接线；T03 API；T06活动投影；M1-T03 adapter-live及三适配器；M2-T06取消；需要真实认证 |
| AC-015-01 | M4-T07 / tui-pty | T07多行/外层Unicode/四路径；T02/05布局；M1-T08解码/PTY；M3-T09提交；test-infra ssh/tmux |
| AC-015-02 | M4-T07 / tui-pty | T07编辑器scope/Esc保护；T02面板/SGR；M3-T09模式命令（不声称M3有Ctrl+P）；M2-T06控制 |
| AC-015-03 | M4-T07 / integration | T07边界/帧编码/文件输入；T09段对象；T03测量runner；M1适配器fixtures；M3-T09投递/四态 |
| AC-015-04 | M4-T07 / tui-pty | T07启动复位/退出码/teardown；M1-T08恢复Guard；T02鼠标；M2-T06后台控制 |
| AC-015-05 | M4-T10 / integration | T10百万库最终测量；T07输入；T03参考runner/背压；T05虚拟化、T06状态栏、T08最终键位 |
| AC-016-01 | M4-T08 / tui-pty | T08无障碍；T02面板/抽屉/视图、T05块、T06Timeline/status、T07编辑器；M3-T09命令 |
| AC-016-02 | M4-T08 / unit | T08 scope/冲突；M2-T07配置；M3-T09 mode_cycle |
| AC-016-03 | M4-T08 / tui-pty | T08 ui-only保存/恢复/CAS；T02面板；T07输入；M3-T03覆盖设置/M2-T07配置监听 |
| AC-016-04 | M4-T08 / unit | T08样式；T06真实状态；T02布局 |
| AC-064-01 | M4-T10 / integration | T10完整组合；T09迁移/回填；T03索引/GC/ENOSPC/慢流与测量；T05虚拟列表；T07终端；M3-T04迁移运行器 |
| AC-064-02 | M4-T10 / integration | T10大库/RSS；T03 Range/search/测量；T05缓存；T02 Workspace/Focus；T07最终输入 |
| AC-064-03 | M4-T03 / integration | T03引用/GC/pin/tombstone；T09段登记；M2-T16冻结/M2-T18证据与历史；M3-T02失效；已有release manifest根 |
| AC-064-04 | M4-T03 / recovery | T03对象/空间与STORAGE_LOW_SPACE；T09段上限；M2-T01 DB_FULL、M2-T05/06恢复控制；test-infra ENOSPC设施 |
| AC-064-05 | M4-T03 / integration | T03字节背压/release测量；M2-T12 SSE与M2-T01单writer；T09公开事实 |

### 批准后拟应用的文件

| 文件/区域 | 拟改动与承担者 |
|---|---|
| `contracts/requirements.json` | 八项detailed、37 AC、behavior/failure_modes/decisions/doc_refs；REQ-018补ui键、ui-only监听区分与字节单位；不删除旧AC |
| `contracts/acceptance-scenarios.json` | 保留八个旧TEST、增加29个；各行given/when/then转fixture/steps/pass_criteria，check_id按表；新增的观测路径全部在指定check内 |
| `contracts/openapi.json` | §2 typed字段/路由/Range/search/明确错误，§4 SendInput编码/解码大小限制；既有404 BAD_REQUEST与SSE409/410保持；page expiry扩展410顶层水位 |
| `contracts/protocol.schema.json` | §2六个ErrorCode、RejectionReason追加STORAGE_LOW_SPACE及description；不改Run/TaskResult/Gate状态 |
| 下一顺序`contracts/sql/*_ui_observability.sql`与`scripts/product/persistence-whitelist.json` | §2迁移/索引/段/去重/引用/tombstone；T09运行器+异步回填，T03查询GC；加入扫描白名单与baseline --add |
| `contracts/development-tasks.json` | 上述九implement/T04片段；M4-T01前置保留；M6-T03/M7-T04 done_when追加24.04四路径收齐义务，不转AC或check归属 |
| `tests/check-plan.json`、`scripts/product/cargo-tests.sh`、测量example与结果写入器 | 既有integration追加三个声明产物与串行release阶段；唯一TEST收集规则、schema合法DEGRADED诊断/非零；不改14项check/mode/超时、release-policy或gate |
| `scripts/dev/test-infra.sh`、`.github/workflows/bundle-ci.yml` | T03隔离ENOSPC和有效taskset+systemd 4CPU/8GiB unit，T07四路径/CI tmux/sshd/locale；缺设施失败；不用生产用户目录/共享端口 |
| `.hoh-dev/environment.json`（冻结前） | 记录本VM参考profile、限制/预算/构建输入；检查实际值另写m4-environment，不修改已冻结环境 |
| `docs/02-functional-spec.md`、`docs/18-traceability.md` | 只由生成器更新 |
| `docs/03-ux.md`、`docs/05-data-api.md`、`docs/12-user-guide.md`、`docs/adr/ADR-014.md`、`docs/adr/ADR-027.md`与实现ADR | 实现时记已验证行为；ADR-014明确SGR/Esc保护/启动无条件复位的例外与CSI-only，ADR-027 exports守卫；不把本稿当已实施说明 |
| `config/hoh.example.toml`、Rust与fixtures | 随所属实现任务同提交支持配置/字段，全部回归TEST-018-01；现阶段不提前改example |

正式M4-T01应用先重核最终main/M3、独立复审并决定下文剩余提案，再按docs/21应用/生成/relock/bundle。当前只改DRAFT文件；不relock、不改台账、不推送，不以bundle代替新AC的product验收。

## 不变的内容与授权边界

八项仍required/v1、旧AC/TEST不删除；REQ-064-01原组合范围保留，改归最终UI后的任务；REQ-068不缩窄到host。默认Workspace、可选Team、公开Focus、三级Timeline、真实statusline、完整输入/无障碍、被引用对象保护均保留。三Agent/三Pi插件/Web/M5–M7、72h与20次真实迭代、独立QA/Review及身份绑定不变。仅OSC52这一未被原规格要求的增强延期，文件复制按CTO指令保留。

这是CR-003 D1细化提案；若后续CTO认为某剩余选择改变已确认交互/降低门槛，必须按docs/21升级所有者。本草案作者不批准自己。

## CTO 问题：剩余选择与提案（不是重新询问绑定指令）

| ID | 待决定的问题 | 提案与理由 |
|---|---|---|
| D1 | additive读模型/API与迁移形状 | 采用§1/2的H+generation/typed refs及003；旧路由错误保留，新路由才定义新码；最终迁移编号随M3确认 |
| D2 | 当前进度分母与历史呈现 | 当前批准spec全部需求为m，当前Run最新ACCEPTED候选全PASS需求为n，DEGRADED单列；旧计数只在标候选的历史详情，未附着显示未知，避免借旧验收 |
| D3 | 轮次成员与事件分类 | 采用Run内关轮区间、开放轮稳定key与完整kind表；不改关轮/预算，不依赖从未写入的tasks.iteration_id |
| D4 | 本VM参考profile与新增预算 | 既有integration串行release，4CPU/8GiB包络；本机ext4盘不假称SSD；100/500ms、idle总150MiB、活动各256MiB、构建/回填600s；M7另验证原SSD环境与REQ-068 |
| D5 | 日志/孤立对象/导出保留 | 8MiB×5未引用诊断、48h孤立宽限、所有明确引用与pin保护、tombstone；显式exports不自动删且单列占用 |
| D6 | 低空间数字阈值 | min_free=32MiB、未知流段预留1MiB；STORAGE_LOW_SPACE仅拒新对象/新写Attempt，控制202/DB_FULL分界已由CTO绑定 |
| D7 | 搜索/复制细节 | 完整UTF-8分类、8MiB续扫含尾重叠、file复制到受守卫exports；OSC52已决定延期，不是待选项 |
| D8 | 状态档与ui-only保存 | 采用三档明确字段表与项目配置原子CAS保注释、ui-only不触发ConfigureTeam，保持执行配置冻结语义 |
| D9 | 输入防误发与键位规则 | 独立Esc后抑制一次Enter，普通编辑/Ctrl+J重新武装；scope/action/key覆盖、保留Ctrl+C/Esc/Enter/Ctrl+J、面板始终可达、mode_cycle不变 |
| D10 | 编辑/传输上限和平台分期 | 编辑1MiB、SendInput JSON请求≤8MiB（容纳最坏转义）、inline≤256KiB且编码≤768KiB，超出为文件；M4四路径26.04，M6/M7完成24.04；支持范围不减少 |
| D11 | 公开规范化具体记录形状 | 白名单public块+activity/usage、native id/offset去重、未知能力显式null、≤256项/1MiB队列；三家真实接线与live补证，不猜未观测活动 |
| D12 | 九实现任务分工 | 先T09生产者、T03查询/容量，再T02/05/06/07/08；T10最终组合、T11三家live，T04独立收口；均≤2REQ/8AC，检查owner不变 |

## 设计审查第 1 轮的处置

输入为第1稿`8960fdf`；Claude报告 `/home/ai/dev/hoh/.hoh-dev/review-cr028-d1-claude.md` 与Grok报告 `/home/ai/dev/hoh/.hoh-dev/review-cr028-d1-grok.md` 的**最后hoh-report JSON**，分别36与22项、合计58项（critical=0、high=13、medium=28、low=17）。保持原severity，不用草案修改声称独立审查已PASS。“修订/采纳”均只指本稿文本回应，等待第2轮复审和后续实现；所有medium已在本文修订，没有延期medium。低项CR28D-30部分采纳、-31按绑定指令不采用OSC，其余一并修订。

| ID | severity | disposition | where |
|---|---|---|---|
| CR28D-01 | high | 修订：既有integration唯一release/串行测量，本VM资源/预算/状态与CI规则明确 | §5.1；AC-012-01、015-03/05、064-01/02/05；T03/T10 |
| CR28D-02 | high | 修订：关轮区间定义开放/关闭轮稳定身份，新增生产归属任务与AC | §2.1；AC-013-05；T09；依赖闭包 |
| CR28D-03 | high | 修订：三native规范化→runner→持久公开块/activity/usage，fixture与live分开 | §2.2；AC-011-05、014-04/05；T09/T11 |
| CR28D-04 | medium | 修订：任务JSON无重复checks，verify无acceptance | 任务JSON；check owner说明 |
| CR28D-05 | medium | 修订：旧路由错误保留，SSE/page过期410顶层水位；新路由400 | §2错误契约；AC-010-03、013-02、064-01 |
| CR28D-06 | medium | 修订：Ctrl+P注册表与三个抽屉归T02、有可观察AC | §4；AC-010-01；T02与AC-015-02闭包 |
| CR28D-07 | medium | 修订：所有已知kind与未知缺省分类，派发不因attempt身份进入Agent层 | REQ-013分类表；F13；T03/T06 |
| CR28D-08 | medium | 修订：broken/orphan/stale/current各独立夹具与唯一标签 | AC-013-03；CR-026 REQ-032对照 |
| CR28D-09 | medium | 修订：RQ3不在Run任何Task范围，只入分母 | AC-014-01 |
| CR28D-10 | medium | 修订：inline≤256KiB/编码≤768KiB，其他文件引用；三adapter边界fixture | §4；AC-015-03；T07 |
| CR28D-11 | medium | 修订：pending定义为应用序列化字节/项，按字节批读，满后停止拉取 | AC-064-05；§5.1；T03 |
| CR28D-12 | medium | 修订：DDL-only迁移、reconcile先行、断点回填，30s/600s预算 | §2迁移；§5.1；AC-064-01；T09/T10 |
| CR28D-13 | medium | 修订：STORAGE_LOW_SPACE与schema清单、自动/决策派发观测和回归 | §5；AC-064-04；protocol应用清单 |
| CR28D-14 | medium | 修订：test-infra准备实际ENOSPC，EDQUOT明确注入，缺设施FAIL | §5；AC-064-04；T03 |
| CR28D-15 | medium | 修订：status/progress单getRun H，Timeline单列已应用seq | §3字段表；AC-014-03 |
| CR28D-16 | medium | 修订：逐项引用根、未知保留、GC tombstone与404 reason | §5；AC-064-03 |
| CR28D-17 | medium | 修订：M4本机26.04四路径，24.04仍归M6-T03/M7-T04；CI补设施 | REQ-015兼容矩阵；应用清单 |
| CR28D-18 | medium | 修订：idle host+TUI总和150MiB，分别上界与活动峰值 | §5.1；AC-064-02；REQ-068不变声明 |
| CR28D-19 | medium | 修订：完整UTF-8分类、8MiB边界重叠与cursor去重，跨窗fixture | §2文本规则；AC-012-02 |
| CR28D-20 | medium | 修订：迁移/完整性修复触发重建，debug钩子同生产函数 | §2迁移；AC-010-03、013-02 |
| CR28D-21 | medium | 修订：产品批提交/构建600s、同执行哈希绑定克隆、总3600s | §5.1；AC-064-01 |
| CR28D-22 | low | 修订：未提交不可见改到真实host/SSE事务测试 | AC-010-04、064-05 |
| CR28D-23 | low | 修订：READY T2与显式派发拒绝产生ROLE_BINDING_PENDING | AC-011-02 |
| CR28D-24 | low | 修订：EACCES用ARTIFACT_UNREADABLE/503，认证403独立 | §2；AC-012-03 |
| CR28D-25 | low | 修订：配置用字节整数并列精确界限 | §4配置表 |
| CR28D-26 | low | 修订：项目ui-only CAS原子保存保注释、与ConfigureTeam区分、mode.cycle重绑 | §4；AC-016-02/03；T08 |
| CR28D-27 | low | 修订：exports为用户显式导出、不自动删、占用单列 | §4文件复制；D5 |
| CR28D-28 | low | 修订：指定UTF-8/标记边界分块，64MiB未结束paste与16MiB RSS增量 | §4；AC-015-03；§5.1 |
| CR28D-29 | low | 修订：session_id过滤、Focus按Attempt定位，跨Attempt仍保留身份 | §2查询；AC-011-02/03 |
| CR28D-30 | low | 部分采纳：历史详情显示旧ACCEPTED计数/ID，当前不借旧计数；未附着明示未知 | §3；D2。当前n归零保持现有验收身份约束 |
| CR28D-31 | low | 不采用OSC增强：CTO明确默认文件、OSC52延期，文件复制标明真实结果 | §4；AC-012-03；D7。REQ-012复制全文保留 |
| CR28D-32 | low | 修订：旧064-01组合完整保留，晚到最终UI任务；轮换/保护/虚拟列表逐项断言 | AC-064-01；T10 |
| CR28D-33 | low | 修订：Workspace decision依据/影响可见 | AC-010-01 |
| CR28D-34 | low | 采纳：启动先复位私有模式，ADR-014启动例外明确 | §4；AC-015-04 |
| CR28D-35 | low | 修订：未知长度段按1MiB上限预检/预留 | §2.2/5；AC-064-04 |
| CR28D-36 | low | 修订：下一SQL迁移加入persistence白名单与baseline --add | §2迁移；应用清单 |
| CR28D-50 | high | 修订：可解析任务JSON，一AC一owner/check唯一owner/verify无acceptance | 任务JSON与依赖闭包 |
| CR28D-51 | high | 修订：四路径缺任一FAIL非零，不发明test-result状态；跨平台不混一次PASS | REQ-015矩阵；§5.1；T04 |
| CR28D-52 | high | 修订：SSE保留410、顶层snapshot_watermark；409只矛盾 | §2；AC-010-03、013-02 |
| CR28D-53 | high | 按CTO指令延期OSC52，不新增配置支持，保留CSI-only | §4；AC-012-03；ADR应用清单 |
| CR28D-54 | high | 修订：自有poll解析SGR1006、坐标>223/后键、X10跳过、模式退出 | §4；AC-015-02/04；T02/T07 |
| CR28D-55 | high | 修订：Ctrl+J四路径必备，Esc超30ms保护不提交，增强Shift+Enter | §4；AC-015-02；D9 |
| CR28D-56 | high | 修订：未完成paste≤1MiB+6B、超出丢尾/保草稿、信号退出 | §4；AC-015-03 |
| CR28D-57 | high | 修订：低空间预检与状态事务失败分开，Pause/Cancel202与DB_FULL各有断言 | §5；AC-064-04 |
| CR28D-58 | high | 修订：三档固定差异/顺序/持久化，窄屏状态页 | §3；AC-014-03 |
| CR28D-59 | high | 修订：外层终端光标/换行判宽、末列宽字/ZWJ/VS16、外层tty与shell哨兵 | AC-015-01/04 |
| CR28D-60 | medium | 修订：F13逐条身份、全Run/iteration/agent/session/role/null集合和交集规则 | REQ-013 F13表；AC-013-01/02 |
| CR28D-61 | medium | 修订：DEGRADED不进n、仍进m、单列degraded=1 | §3；AC-014-02 |
| CR28D-62 | medium | 修订：文本与绘制最长50ms/16KiB、输入100ms、关键事件不合并丢失 | §2.2/5.1；AC-015-05、064-05 |
| CR28D-63 | medium | 修订：writer busy→503 DB_BUSY，旧快照绘制/脱离，不乐观成功 | §5；AC-010-03 |
| CR28D-64 | medium | 修订：panic101、信号码优先，SIGINT同完整teardown矩阵 | AC-015-04 |
| CR28D-65 | medium | 修订：416专用ARTIFACT_RANGE_INVALID，与400 QUERY_INVALID分离 | §2错误表；AC-064-01 |
| CR28D-66 | medium | 修订：message/decision/handoff与其余五类同动作与分页复制返回义务 | REQ-012行为；AC-012-01/04；T05 |
| CR28D-67 | medium | 修订：40×8提示/输入/退出，1×1 ASCII诊断/逐字符状态字段，恢复保草稿 | AC-011-04；D10小屏策略 |
| CR28D-68 | medium | 修订：持久blocked分计、removed=1、新分母和requested/resolved模型差异 | AC-014-01 |
| CR28D-69 | medium | 修订：二进制原始字节不入PTY；所有p95受唯一参考环境规则 | AC-012-02；§5.1；性能AC各行 |
| CR28D-80 | low | 采纳：exports加入冻结守卫，忽略被改时拒导出且不产生脏文件 | §4；AC-012-03；ADR-027应用清单 |
| CR28D-81 | low | 采纳：启动前复位mouse/paste/alt，残留模式PTY夹具 | §4；AC-015-04；hermes §4建议 |

## 验证记录与交付边界

本任务是DRAFT规格修订；未改生产代码/受锁契约/检查计划、未领取任务/推进全局台账、未relock。无Runtime run_id/task_id/attempt_id/role_epoch身份头、TaskResult输出文件与指定自测目录，因此不编造TaskResult/candidate ID；依委派要求交付本地文档commit。起始分支prep/M4-T01-spec、HEAD=8960fdf5a0360bae8e1842301eed8bece93e85e4、工作树干净，唯一写权限由本次输入授予。

已执行的命令/记录：修订前 `bash scripts/check.sh --bundle` 退出0（504 tests，1项既有skip）；日志 `.hoh-dev/evidence/m4-spec-d2/bundle-before.log`。文档探针 `.hoh-dev/evidence/m4-spec-d2/probe.py` 读取本稿JSON、CR-026的16项需求/8实现任务，与当前任务图合并后调用真实 `validate_bundle.ownership_problems`：37 AC、9实现任务、58处置、归属错误0、依赖错误0（记录 `probe.json`）；这不是已应用整套机器契约的bundle。资源探针首例缺cpuset失败、第二例证实AllowedCPUs未生效，第三例用taskset+user unit实际读回四CPU/8GiB/400%限制且退出0，记录 `resource-envelope{,-02,-03}.json` 与stderr；首失败保留，不称产品红/绿。修订后 `bash scripts/check.sh --bundle` 退出0（504 tests，397.658s，1项既有skip），记录 `bundle-after.log`/`bundle-after.exit`；最终文档由 `python3 tools/validate_bundle.py` 再校验，任务/AC探针重跑，记录 `validate-final.json` 与 `probe-final.json`。F13的27个层级/过滤完整集合另作集合交集核对，无不符。bundle验证的是当前未应用契约树，不把它说成37条新AC的产品通过。所有临时工作使用 `/home/ai/dev/m4spec-XXXXXX`，每个shell加载指定env.sh。原审查报告与M3/Hermes来源均只读。

生产红/绿、三native fixture/live、平台矩阵、性能、低空间与第2轮独立设计审查均 **NOT_RUN**；本稿预算不是本次测量结果。以上是**自测，非验收**，不报告v1 COMPLETED，也不以本次commit代表CR已批准。下一步由CTO决定D1–D12剩余选择并送第2轮设计复审；M4-T01正式应用仍等待M3-T05。
