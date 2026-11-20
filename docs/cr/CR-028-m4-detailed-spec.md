# CR-028 · M4 的工作台、终端与容量规格细化

ID：CR-028
状态：DRAFT
批准人：待 CTO 决定与独立设计审查；本稿没有批准或应用基线变更
日期：2026-11-20（第 5 稿；设计审查第 4 轮处置稿，仍为 DRAFT）
输入基线：第 4 稿 `a864584a766ea8c0a4e997b9d59e3e6cd1d7834e`，分支 `prep/M4-T01-spec`；产品来源基线仍为 `067c241ed7dd1aaba0dd5ee1b0f6aa844414e5e2`
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
| CR-024，第4稿从 `/home/ai/dev/hoh` 的 main=`4a1f1c1236f04952f4d657e5db217d46446217b3` 读取 `docs/cr/CR-024-commander-read-only-profile.md`；原 cmdro 工作树已不存在 | Commander 只读档与公开最终消息决策；本工作树输入基线未含该文件，main 已有后续修订（含 supervisor.bridge_ignored）；正式应用时按最终 main 重核，不把 main 的实现状态借给本分支 |

参考 `/home/ai/dev/hoh/.hoh-dev/evidence/hermes-study/report.md` §4–5（来源树 `514e2cb`），只借鉴无光标副作用的启动模式复位、虚拟列表、状态栏字段预算、字素宽度与诊断日志轮换的机制；下文明确哪些是 HoH 提案。没有运行 hermes 代码，也不采用其退出时杀 gateway、丢弃监控队列或按会话条数截断历史的策略。

### 当前实现与需要补齐的部分

1. M1-T08 的 `crates/hoh-app/src/tui.rs`、`tui/app.rs`、`tui/input.rs`、`tui/terminal.rs` 已有 Ratatui/Crossterm 外壳、信号恢复、历史、括号粘贴解码。`tui/editor.rs` 明确是单行编辑器，当前将换行替换为空格；多行输入属于 M4。输入读取是可中断的自有解码器（ADR-014），不能为支持鼠标改回不可取消的阻塞读取。
2. M2-T12 的 `host/http.rs::stream_events` 以 DB 水位和每批 500 行实现 SSE，`Last-Event-ID` 与 `after` 冲突为 409。当前路由有 runs、getRun、team、acceptance、issues、events；OpenAPI 已声明的 tasks、evidence、timeline、artifacts 并非都已有路由。M3-T07 承诺补 tasks 与追踪；其余缺口在本稿分派，不把“有 OpenAPI”当成已实现。
3. M2-T18 的 `persistence/snapshot.rs::AcceptanceView` 与 `getRun.projection.acceptance` 中 accepted/total 是 **AC** 计数，且 accepted 只有当前候选被接受时才非零。状态栏需要需求计数，不能给这个旧字段换标签。Issue 状态来自 ADR-029 的状态函数，不能只读取可能过时的显示列。
4. 当前 `tui.rs::Conn::link` 使用无界消息通道、首次从 seq=0 读；`App.messages` 为增长的向量。这些是 M4 容量改动的实际入口。新工作台须从水位快照和按需页面启动，不能先把百万历史事件全部读进客户端。
5. 当前 `closing.rs` 只在 `iteration.completed` 时建 iterations 行，未写 tasks.iteration_id；adapter 的 Frame/Exit 与诊断输出不等于已持久的公开块。M4-T09 补齐归属、三适配器规范化、公开块与活动事实的生产者；M2-T07 不承担这项未实现工作。
6. 当前配置加载器仅接受 `ui.statusline="standard"`；`ui.view` 为 workspace/team/focus，已有 `ui.mouse`。minimal/custom、键位覆盖和下文容量键须与加载器一起交付，不能提前写进 example 导致 REQ-018 回归（CR-026 的 CR26D-103）。

## 第 2 稿的绑定指令

CTO 在第2稿委派中已决定：性能测量在本 VM 可执行、release/串行/一个指定检查，不放宽 100/500 ms 或 150 MiB；M4 负责迭代归属和三适配器公开流持久化；任务遵守单 check owner 与 verify 无 acceptance；保留 SSE 410/顶层 snapshot_watermark 与既有错误语义；默认复制文件，OSC 52 延期；SGR 1006 用自有 poll 解码、X10 跳过；Ctrl+J 多行且慢 Esc→Enter 不提交；未结束粘贴有字节/RSS 上界；输入适配器帧预算；启动先复位私有模式；低空间只挡新对象/新写 Attempt，状态事务能提交时 Pause/Cancel 仍 202；TUI RSS 也有上界；补足状态栏、分类/集合、八类块、小屏、DB_BUSY、退出码与外层宽度测试。以下数值和未定的数据布局是满足这些指令的提案，仍须设计复审，不是批准记录。

## 第 3 稿的绑定指令与设计修正

第二轮在 `50e9fc7` 上仍为 FAIL；终端恢复与输入保护连续两轮出现同类问题，按 docs/19 先重述完整不变量，再修订相关 AC，而不是继续给旧表述加例外：

1. **屏幕和光标的所有权**：启动只关闭 1000/1002/1003/1006、2004 等无光标恢复副作用的模式；从不在启动复位发送 `CSI ?1049l`。本稿选择不复位遗留 alternate screen，不采用尚无 xterm/VTE/tmux 三方无光标恢复证据的 `?1047l`。teardown 只撤销本实例完整开启的序列，保持 ADR-014 决定 2。AC-015-04 用干净主屏与旧 DECSC 保存光标两个正常往返断言原有行/光标行不变。
2. **宽度的观察来源**：四条路径逐条绑定外层终端及版本，终端实际光标列是 oracle；不规定 ZWJ/VS16 恒宽 2，不用 wcwidth 求和或 HoH 的 CellWidth 自证。第3稿按 East Asian Ambiguous 类别回退、第4稿的手写例外表均已由§4第5稿 CTO 设计修正取代；AC 仍断言不漂列、不留半字、HoH 光标与终端一致。
3. **恢复的观察位置**：SSH 在远程 shell、tmux 在 pane shell、SSH 内 tmux 在远程 pane shell，均在 HoH 退出后且包装器仍存活时读取 stty/modes；HoH 自己 PTY 的流中逐项核对其开启模式的关闭字节。包装器退出后的外层 tty 仅补查泄漏。
4. **提交防护**：任何独立 Esc（无临时层也一样）撤销提交武装；同一 write 的 `\x1b\r` 与间隔小于 30 ms 的 Esc/Enter 均不得 SendInput、改草稿或加换行。Alt+Enter 与 Enter 共用 submit guard；只有 Ctrl+J 是换行键，移除第 2 稿的 Shift+Enter 换行提案，粘贴文本自身的 LF 不受影响。

本节是本次 CTO 指令的照录与设计边界；其余未定实现选择仍以 D1–D12 提问。启动限制不宣称 SIGKILL 后能够恢复上一次进程的屏幕/termios。

## 第 4 稿的绑定指令与设计修正

第3轮在 `ebaf570e` 上仍为 FAIL：Claude 2 high/4 medium/6 low，Grok 1 high/6 medium/2 low。宽度、测量归属和生产事件覆盖连续出现，先按 docs/19 修正不变量。以下来自本次 CTO 委派，不重新提问；具体尚未批准的实现选择仍列 D1–D12：

1. **原字符与列宽（历史指令，宽度模型已被第5稿覆盖）**：East Asian Ambiguous 原样显示的要求保留；此处原有 unicode-width 非 CJK 模型、按类别给定列数及手写例外策略撤销。当前唯一宽度依据和回退规则见§4第5稿 CTO 设计修正；本段不得再作为实现宽度表。
2. **测量服从生产生命周期**：`main.rs::host` 的 setsid、`client.rs::start` 的 hoh-host@.service/委派 scope、每 Attempt 的 cgroup 均保留；禁止要求它们留在 runner 的 pgid。进程以 boot_id+pid+start_time/pidfd 固定，测量其实际 unit/scope 与 Attempt cgroup；资源限制施加到产品实际创建的这些对象，清理靠 unit stop、既有 Attempt 所有权证明及 cgroup populated=0。§5.1 明确测试接入与外部收尾，不改 SSH 脱线存活语义。
3. **required 不可降级**：非参考环境逐 TEST 为 SKIPPED、detail含 `reason=REFERENCE_ENV_MISMATCH`，整次检查退出78。DEGRADED 不得用于这些 required 检查。
4. **分类从生产者闭合**：补全 cancel 五种结果、effect.committed 等；T03 将生产写入类型化，分类从同一注册定义穷举，源码守卫和负例拦下绕过注册的生产者，不能拿两份手写名单互证。
5. **中等级别发现全部进入判定**：低空间 SendInput 唯一状态/码且保草稿；T05/T07/T10 重测并强制阶段时限；Ctrl+C 两种上下文分开，Esc 从不 Pause/Cancel；同 Agent 多职位必须选择；未知费用/上下文不显示0；Ctrl+R 与 /resume 有独立子例；轮次绑定真实终结/暂停/恢复事件与快照。

本轮只修订 DRAFT 文本，不声明上述生产行为已实施或被独立验收。

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

**错误契约（D1，CR28D-05/-52/-65）**：既有错误体/码不替换。SSE `/v1/events` 保留 `409 CONFLICT`（after/Last-Event-ID 矛盾）；保留 `410 {code:"CURSOR_EXPIRED",message,snapshot_watermark:H}`，watermark 在顶层，不能加 details.resync 代替。v1 不删除权威 events，SSE seq 的保留期无限，因此当前产品没有可触发的 SSE 410 分支。该响应保留为 OpenAPI 向后兼容形状：静态核对 code/message/顶层 snapshot_watermark；客户端处理只用明确标注 stub 的注入响应覆盖，不作为实际 SSE 过期的产品证据。查询页的 generation 失效则是可达产品路径，明确返回同形 410；仅对原 Run timeline 补列这个响应，不改变其非法参数的 `404 BAD_REQUEST`。旧 runs/tasks/evidence 等分页非法参数沿用原 404 BAD_REQUEST；缺 Run 仍 RUN_NOT_FOUND。新 `/v1/timeline` 与新 search 路由的非法参数/cursor 才使用 400 QUERY_INVALID；Run timeline 的非法新增过滤也为 404 BAD_REQUEST。

| 新码（拟补 `$defs.ErrorCode`） | 唯一 HTTP 状态 | 用途与正文 |
|---|---|---|
| QUERY_INVALID | 400 | 仅新 timeline/search 查询；沿现有 Error 形状，details 指明参数 |
| ARTIFACT_NOT_FOUND | 404 | 已声明 artifact 路由补齐缺 ID/缺文件/GC tombstone，details.reason 分别为 unknown_id/missing_file/collected |
| ARTIFACT_INVALID | 409 | 登记大小或摘要不符；返回 ID 与错误原因，不返回正文 |
| ARTIFACT_NOT_TEXT | 422 | 新 search 路由：未通过完整 UTF-8 文本分类 |
| ARTIFACT_RANGE_INVALID | 416 | artifact 的新增 Range 功能：无效、多个或越界 Range，带 `Content-Range: bytes */<size>`；不复用 QUERY_INVALID |
| ARTIFACT_UNREADABLE | 503 | 对象 EACCES/I/O 错误；details.reason=permission/io，不冒充认证 403 |
| STORAGE_LOW_SPACE | 503 | 既有 SendInput 的新增对象写入预检拒绝，沿原 Error 体；details.reason=STORAGE_LOW_SPACE，含 available_bytes/required_bytes/min_free_bytes；不返回202（§5，D6） |

所有新码只用于此处点名的新增路由/新增功能（包括既有 SendInput 的对象写入预检），使用原 Error 的字段，不发明第二套通用错误体；403 的 token/Host/Origin 语义不变。DB_BUSY/DB_FULL 仍 503；本地 UI_CLIPBOARD_UNAVAILABLE、UI_INPUT_TOO_LARGE、UI_KEY_CONFLICT、UI_UNKNOWN_EVENT 不加入 Run 状态。同名 `STORAGE_LOW_SPACE` 还在 `$defs.RejectionReason` 追加，用于调度拒绝（§5）；ErrorCode用于HTTP、RejectionReason用于派发，各守自己的字段，不改 BlockedReason。

**查询文本规则（D7）**：登记不可变对象时流式校验全文件 UTF-8、拒 NUL，将 text/binary 分类与内容摘要一同登记；未封口或未分类对象不能搜索。Range 以字节为单位，UI 保留最多 3 个未完成 UTF-8 字节到下一页。搜索每次消费至多 8 MiB 新字节，保留最多 query_bytes−1 个尾字节给下一窗口；按匹配起点去重（允许重叠匹配），cursor 绑定 sha256/query/下一个未报告起点/扫描 offset，limit 命中时从最后已报告起点+1 恢复，不能跳过本窗口余下匹配。上下文截在字素边界且≤256 字节。二进制只显示 metadata 与下载动作，正文控制字节不进入终端。

**拟议受锁持久化变更**：M3 的 `002_spec_drafting.sql` 之后增加下一顺序迁移（预期 `003_ui_observability.sql`，应用时确认编号），同时加入 `scripts/product/persistence-whitelist.json`。M4-T09 交付迁移运行与公开输出存储，M4-T03 交付查询/GC；不改写 001/002，不新增验收真值。表/索引至少有：`ui_iteration_ranges(run_id,number,iteration_key,start_seq,close_seq)`、`ui_event_index(seq,run_id,iteration_key,attempt_id,role,role_epoch,session_id)`，索引 `(run_id,seq)`、`(iteration_key,seq)`、`(attempt_id,seq)`、`(role,seq)`、`(session_id,seq)`；`public_segments(artifact_id,attempt_id,block_id,stream,first_offset,bytes,sha256,sealed,created_seq)`；去重的 `(attempt_id,native_stream,native_item_id,part)`；对象引用索引 `(owner_kind,owner_id,artifact_id)` 与 tombstone。class 不持久化：T09 只写身份索引，T03 查询时用集中 kind 注册表计算分类，不产生 T09→T03 的反向依赖或第二次回填。

迁移事务**只建空结构**，不在事务中全量回填；先完成原 reconcile，再由单 writer 分批（每批≤500 行/1 MiB、批间让出控制队列）回填，保存断点，`index_ready=false/indexed_through` 明示不完整，仍可读原快照与接受 Pause/Cancel。首次回填、schema 索引版本变化、启动完整性核对发现缺索引时才重建；普通重启不递增 generation。新 generation 完整构建到 H 后原子切换，期间查询新索引以 503 DB_BUSY、details.reason=index_rebuilding 明示重试，不给半页假称完整。debug 专有 fixture 钩子只触发同一重建函数；release 性能测试由§5.1记录的pre-003产品提交在同次检查构建release、以其自己的writer产生002百万库，不靠debug钩子。计时从候选首次open开始，包含迁移/reconcile及回填中kill/reopen；kill→reconcile与host ready均≤30s，索引尚未ready不阻塞控制，完整backfill阶段≤600s、不因重启清零，见TEST-064-01。iteration_key引用ui_iteration_ranges而非只含已关闭轮次的旧iterations表；(run_id,number)唯一，close_seq可空且非空时大于start_seq。其他外键/唯一约束、升级失败回滚、旧 binary 拒高 schema 按 REQ-062/M3 迁移运行器测试。

### 2.1 M4 迭代归属生产者（D3，M4-T09）

提案采用**已提交关轮边界**，避免为 UI 改工程关轮/预算语义：Run R 的第 N 轮是该 Run 的事件序列 `(close(N−1), close(N)]`，第一轮下界为 0；最后一次 close 之后仅在该 Run 尚未终结时为开放的 N+1。终结后不展示开放轮；最后一次 close 后尚无关轮的尾部事件仍保留 seq/run，但 iteration=null、只入 P，不制造假轮次。该边界是按查询快照 H 确定的派生成员关系（不是修改 events）：Run 终结/恢复事实按下表的真实生产载荷和seq登记；旧H仍按H以内的终结/恢复事件计算，不读取当前runs.state替代历史，也不覆盖旧边界；历史/回填使用相同规则；INCOMPLETE 恢复时依原 close 边界重开该暂存区间，不增加关轮计数。项目 seq 中别的 Run 的事件不属于 R。稳定 `iteration_key` 使用现有 `child_id(run_id,"iteration/N")` 算法（展示名 I1 等只为夹具别名）；`iteration.completed` 自身属于关闭的那轮。在线写事件时在同一 writer 事务读取上次 close 并登记归属，关闭后 key 不变，重放依同一边界还原。run_id=null 的项目事件 iteration=null；未关闭轮次也可过滤，不伪造 iterations 已关行、不增加预算、不触发一次额外 accepted。

**终结边界的事件与状态（D3）**：由 `writer.rs::append_result` 的 `RunResult.state`、`control.rs::finish_cancel`、`retry.rs` 的 incomplete/resumed 及 `reconcile.run` 已提交载荷派生，不把“收到 run.result”直接等同终结。每次查询只折叠 seq≤H 的完整已提交事实；同事务以末尾 run.result 的 state 为最终状态。

| 已提交事实 | H 下的轮次尾部规则 |
|---|---|
| `run.result` 且 `payload.state` 为 COMPLETED、FAILED、CANCELLED；`run.cancelled` 的 state=CANCELLED | Run 真正终结；不显示开放 N+1，末 close 之后尚无关闭边界的事件 iteration=null/只P。FAILED由真实状态迁移后writer产生run.result，不虚构run.failed。相同状态重复结果幂等 |
| `run.incomplete`（stop/from/remaining）及 `run.result.state=INCOMPLETE` | 可恢复停止；隐藏暂存的 N+1，尾部iteration=null/只P，保留原下界，不新增close |
| `run.paused`、`run.pause_requested`、`run.cancel_requested`；`run.result.state=PAUSED/BLOCKED`；WAITING_EXTERNAL、BLOCKED等非终态 | 尚未终结，原开放轮保留，不能因CLI已有退出结果或暂停而制造关轮/隐藏轮次；cancel_requested要等真实取消完成 |
| 显式ResumeRun的同事务 `reconcile.run{from,command_id,...}` 与 `run.resumed{command_id,reason}` 配对，from=INCOMPLETE；调度器的run.resumed另有from/to/due，按其真实字段识别。reconcile.run本身没有to，不从缺字段猜状态；同事务若再写run.result则以其state为准 | H2重开原close之后的同一N+1，不增加计数；恢复前H1保持隐藏。同一尾事件e在H1为null、H2属于N+1；H1已发出的page/cursor不漂移 |

`milestone_ready/release_request` 自身不终结Run；生产者若依 ADR-031 将停止后的Record写成run_id=null，UI保持该null，不按payload里的Run名称补归属。异常/缺失状态载荷只给诊断和index_ready=false，不猜终态。上述规则只改变读模型，不能绕开既有终态不可重开的Run迁移守卫。

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

**用户正文生产者（D11）**：M4-T09 在 M3 既有 SendInput 接受事务接入公开消息登记，以 input_id 为幂等键；记录的是 CR/LF 规范化后的已接受原文，而不是 adapter 后续的文件引用提示。超过 16 KiB 的正文先经同一对象写入流程封口，再在 input.accepted 同事务写 public.message 的引用；低空间失败按§5唯一503 STORAGE_LOW_SPACE返回，保留草稿且不写input.accepted/public.message，重放不多一块。尚未选定 Attempt 时 attempt/session 可空，按已解析 target role/epoch 显示在 Workspace 的用户对话；投递后用 input_id 关联到真实 Attempt/Focus，不伪造用户为 Agent 或从未来绑定猜身份。被拒输入仍是草稿/拒绝诊断，不造接受消息。adapter 的 user 回显若有 input_id 则关联原块，不能重复；无法核对的回显注明来源，不能冒充新用户提交。AC-011-05 从Persistence核对正文/hash，AC-015-03 再核对Workspace/Focus展开/导出逐字节和hash，四态由 M3 继续决定。

**背压与不活跃（D11）**：当前 host/runner.rs::inactivity 从 executor.output_bytes 及工具进程观测判断静默，并非以 public.message 提交时间计时；但管道停读仍会停止字节计数。提案由 T09 给该观测增加规范化队列阻塞标记：仅暂停这段本地背压时间的 inactivity 计时，解除后接续已有静默时长，不无限重置；记录本地存储阻塞诊断而非 Agent 活动/进展。Cancel/进程终结/deadline/总预算仍照常，不能因本地背压阻断。AC-011-05 用可控时钟覆盖 DB_BUSY 持续超过 quiet、解除后无输出的真正静默和显式取消，回归 REQ-042/ADR-022 决定 19。

### 3. 状态与进度提案（D2、D11）

1. `m` 是当前已批准 spec 中的全部需求数，不是 Task 数、AC 数，也不是仅已排进当前 Run 的需求。没有规格时显示“未批准规格，0/0”，不显示 100%。`n` 只计当前附着 Run 的最新候选上、身份与该 spec 一致、全部 AC 被有效验收且候选已 ACCEPTED 的需求；不拼接不同候选或不同 Run 的 PASS。尚未纳入任务的需求仍在分母中，分子为 0。历史/跨 Run 的通过可查，但不借给当前候选。
2. required 需求全部 AC 为 PASS 才计入；degradable 全部 AC 为 PASS 时可计入 n；含合规 DEGRADED 时 n 不加 1、m 保留该需求、只给 `degraded=k` 加 1，不得写为全部 PASS。无 AC、证据失效/缺失、orphan、UNKNOWN、BLOCKED、FLAKY、SKIPPED 均不计。客户端调用 host 的验收投影，不重写 Gate 规则。
3. 新候选未 ACCEPTED 或规格变化时 n=0；显示新 spec revision 与新增/删除的需求 ID 差集数量。删除的范围只显示已批准的变更，不由 UI 修改。旧计数保留在历史详情；`spec_review` PASS、TaskResult COMPLETED 和模型“100%完成”不增加分子。
4. `open` 只计状态函数得出的 OPEN，`reopened` 只计 REOPENED，避免重复计数；按当前 Run 的唯一 issue_id 统计。状态栏阻塞数取 Run/Task 的持久阻塞，不把等待服务、排队和离线混为失败。
5. 未附着时显示“未附着 Run”、n/m=未知、阻塞/issue=未知；沿 ADR-031 显示项目 baseline，不把最近 Run 偷当当前。新候选归零仍显示候选 ID，最近 ACCEPTED 的计数仅在历史详情，不用它填当前 n。默认优先序：mode（含 pending）、阶段/任务、n/m accepted、blocked/open/reopened；随后 Git 分支/候选、运行时间、预算。Focus 才显示该职位当次 Attempt 的 Agent、实际解析模型、上下文；配置模型不冒充 resolved model。成本与上下文未知显示“未知（role/agent，原因）”，订阅调用不推算美元费用。已知部分成本显示“已知部分”，不是总额。
6. 活动分类依据显式工具开始/结束、服务等待、原生压缩、调度排队、终结和最后活动事实。只有进程存活时显示“进程存活；活动未知”；没有在途监督的 Commander 显示“事件监测中”。模型摘要注明“分析”，不能覆盖确定性状态。缺上下文上限、缺已用量或计量身份不匹配时百分比为 null；不从字符数估算 token。

状态栏字段表（D8）：minimal只有mode、阶段/任务、n/m accepted、blocked/open/reopened四组；standard按这四组后接git、candidate、elapsed、budget；custom按这四组后接配置数组0–8项、严格保序且不重复。Focus的Agent/实际模型/上下文在Focus头部与状态详情可查；custom配置agent/model/context时只在Focus显示，未Focus省去并可在状态页解释。窄屏先换行保四组，再省可选字段，省去项从状态页可达。同帧状态栏与进度取单一完整getRun响应H；Timeline独立标last_applied_seq，禁止宣称可变表能回读旧H。

### 4. 布局、动作、输入与配置提案（D7–D10）

Workspace 保持默认单主区，团队摘要可折叠；Team 显示全部配置职位的状态卡，不把六职位误做六个常驻进程。Focus 的身份包含 `(run_id,role,role_epoch,attempt_id,session_id)`，其中role/epoch视图可不带attempt，session只在明确选择时过滤；Attempt视图必须带attempt_id。没有attempt_id的handoff只出现在H13可匹配的role/epoch/session视图，Attempt过滤返回空；不从session猜某次Attempt。换绑后不自动把旧消息换成新持有者。布局阈值保持 docs/03：<100 列单区/标签，100–159 列主区加一个详情区，≥160 列允许三列，分屏必须由用户选择。80/120/180 是必测宽度，另测 99/100/159/160 边界；<80 列或高度<12 行显示紧凑提示与可操作的输入/退出入口，允许省略可从状态页读取的信息，不丢草稿。

动作使用稳定 action ID，不新增 Shell 命令。拟定公共操作：workspace/team/focus/timeline、task/issue/evidence 抽屉、选择角色/会话、展开/折叠、复制、搜索/下一处/上一处、全屏/返回、状态详情、设置、命令面板、历史搜索、`/resume`历史Run选择、模式切换、提交、暂停/恢复/取消/脱离。Enter 在编辑器提交、列表展开；Tab在编辑器当前斜杠命令token有唯一前缀时仅补全（/he→/help）、保持编辑焦点且Enter前不提交；歧义前缀或无补全则只移焦、保留原文（D9，细化docs/03“补全或移焦”）；Esc 只退一层；Ctrl+R 保留历史搜索；`?` 在编辑器是文字；Ctrl+P 命令面板。列表默认 `Space` 折叠、`f` 全屏、`y` 复制、`/` 搜索、`n/N` 下一/上一处；这些只在列表 scope 生效，命令面板提供相同动作。鼠标单击选择、双击展开、滚轮滚动，动作按钮可单击；禁用鼠标后所有动作仍可键盘执行。

历史与恢复沿 ADR-014 决定7、ADR-031和现有 `tui/app.rs`：Ctrl+R缩小匹配，再按Ctrl+R取更早条；检索中的Enter只接受匹配到编辑器、结束检索，不调用submit或SendInput；Esc恢复检索前草稿并解除武装。Up/Down的已确认历史替换也只是编辑动作。`/resume`打开当前项目可附着Run的选择器，未选择前当前附着身份/草稿不变，POST、ResumeRun、PauseRun、CancelRun均为0；Esc只关闭选择器。选定Run才读取快照/接续SSE，附着本身不发ResumeRun、不创建Task/Attempt；选中INCOMPLETE也保持该状态，之后显式恢复沿既有控制/SendInput语义。它与Ctrl+R输入历史不是同一动作，不新增Shell子命令。

复制提案（D7，默认文件已由 CTO 指定）：把选定公开块的全部纯文本流式写入客户端项目的 `.hoh/exports/<sha256>.txt`（临时文件→fsync→rename），在 ADR-027 冻结守卫中追加 exports/ 并核对实际忽略有效，忽略被用户移除时拒绝导出并解释，不悄悄造成工作区污染；不自动清理用户显式导出，按 exports 单列占用并可从文件管理删除，返回可复制路径和 sha256；不包含边框、颜色码或隐藏字段。这是明确的“复制到文件”，不是谎称已进入系统剪贴板。OSC 52 **延期，不在 M4**，不增加 osc52 配置值；ADR-014 的 CSI-only 约束保留。未来若引入，必须另有修订规定原子写出/截断恢复，不能依靠 CAN 终止 OSC。本稿不再把它列为 CTO 选择。读取仍经 host 的 Artifact API，UI 不写 host 状态表。导出错误显示路径与原因，不记成功。默认不自动启动外部编辑器、浏览器或 shell。

多行输入把 CRLF/孤立 CR 规范成 LF；键盘换行只认 Ctrl+J，粘贴中的 LF 保留；Shift+Enter 不再定义为换行。取消 Esc+Enter 多行别名。任何独立 Esc（30 ms 超时，包括编辑器已有焦点且无临时层）先按原语义退一层（如有），再设 `submit_armed=false`。解码为 Alt+Enter 的 ESC CR（同批或 <30 ms）无论原 armed 值都先解除武装，经同一 guard 拒绝提交；不插入换行、不改草稿。false 状态的普通 Enter 只回编辑焦点并提示“已返回编辑，未发送；修改草稿（可粘贴或接受历史），或按 Ctrl+J 换行后，再按 Enter 发送”，仍保持 false，连按 Enter/Alt+Enter 均不发送。重新武装只由已确认的本地草稿内容变化触发：普通插入/删除、Ctrl+J、成功括号粘贴、Up/Down替换历史、Ctrl+R接受不同的匹配、Ctrl+C清空、面板插入文字；无变化/被拒的插入、光标移动、历史预览不重武装。取消临时层（包括Esc恢复旧草稿）总以false结束；空草稿Enter不发送。历史确认Enter被检索层消费，即使该内容变化重武装也要下一次显式普通Enter才提交。Alt+Enter 永远不是提交或换行的捷径；能辨认的 Shift+Enter 无动作且不重武装。50/100/500 ms 的独立 Esc、单 write `\x1b\r`、两 write 间隔 10/20 ms 均逐例测试。此规则同时补 docs/03/ADR-014；Esc从不发PauseRun/CancelRun、不改变Run状态或Attempt进程。

编辑器 UTF-8 正文限 1 MiB；超限拒绝整次插入/粘贴，原草稿不变。未完成 paste 最多保留 1 MiB 正文+结束标记的 6 字节状态，超过即丢弃到结束标记/信号；不得把余字节解释成命令。64 MiB 未结束粘贴相对 idle 的 TUI RSS 增量≤16 MiB。完整 paste 不提交，仍需显式 Enter；粘贴内 Ctrl+C 是数据，外部 SIGINT/SIGTERM/SIGHUP 仍立即进入退出路径。

**adapter 帧预算（M4-T07）**：1 MiB 是编辑/SendInput 的正文限，不是原生 inline 限。只有正文≤256 KiB 且按该 adapter 的实际 JSON 编码、包装和最坏回显预算≤768 KiB 时 inline；其余在同一投递链中写不可变正文文件，给该 Attempt 已声明可读根内的路径、字节数、sha256 与“读取该文件作为用户输入”的短引用，最多16 KiB。不把大正文再附回 prompt；原生单帧仍≤1 MiB。文件位置遵守该底座和职位的读取根（含 CR-024 Commander 只读档），fixture 从底座可见路径核对可读性；APPLIED 仍仅指 M3 安全点投递已应用，不声称 Agent 已读取或理解文件。全转义输入即使≤256 KiB 也走文件。文件与 input_id/Attempt 绑定、重试复用摘要、未确认投递不标 APPLIED；受理前失败保留草稿；已受理的文件正文在投递阶段遇低空间保持QUEUED并记录§5的STORAGE_LOW_SPACE，不写DELIVERED/APPLIED，不另开投递循环；其他不可读/摘要不符仍明确REJECTED，不静默截断。D10同时提案在SendInput受理路径支持最多8MiB的JSON请求（解码正文仍≤1MiB，包装后的实际请求也须≤8MiB），使最坏JSON转义仍能到达文件引用分支；不能只测adapter函数而绕过HTTP默认body限制。超上界沿host/http.rs::submit_command的既有400 BAD_REQUEST/Error体（too large or broken）、保留草稿且不返回202；其他命令保留现有2 MiB（2097152字节）JSON请求上限：路由读取上限8 MiB后，在解码命令种类并执行任何写入前再校验原始body长度，非SendInput超过2 MiB为400 BAD_REQUEST；不新造413或错误体。每个 adapter 的 fixture 必测从HTTP受理到adapter的最大1 MiB、256 KiB边界、NUL/引号/反斜杠最坏转义、文件不可读/摘要不符；此扩展沿 M3 SendInput 的四态和幂等，不改 route/安全点。

鼠标：在 ADR-014 自有 poll 解码器中解析 SGR 1006 的 M/m，坐标可>223，完整序列后紧接 Enter/UTF-8 字符不吞下一键；残缺/非法序列有≤500 ms 和≤64字节界限，超限丢该序列且不进入无限吞键；遇不属于SGR参数/终止字节的Enter或UTF-8起始字节，立即终止残缺序列并将该字节交还正常键解码，不消费下一键。X10 仍整段跳过。正常仅开启1000/1002/1006（不开任意移动1003），退出逐一关闭实际开启项；启动仍关闭可能遗留的1003。tmux占用鼠标时四路径键盘测试照常，原始SGR必测子例直接注入应用PTY，不能把发往tmux客户端却被拦截的字节算作已送达。docs/12说明终端支持时可Shift拖选，tmux拦截时可关ui.mouse并用终端选择；文件复制动作仍全路径可用。

启动复位：安装信号/恢复守卫后，进入 raw/alternate/paste/mouse 之前，只发送 CSI 关闭 1000/1002/1003/1006、2004，并显示光标。**启动阶段从不发送 `CSI ?1049l`，也不发送 `?1047l` 或其他屏幕切换/恢复光标序列**；不以 DECRQM 显示 alternate 已启用为发送 1049l 的理由。保持 ADR-014“未进入不发送退出”，teardown 只对本实例完整开启的 1049h 配对一次 1049l。分段写失败仍依 CSI/CAN 和原恢复守卫处理，复位完成前不接收输入。SIGKILL 留下的 alternate screen 与原 termios 不在下次启动的修复保证内：该变体只证明 mouse/paste 复位，不要求恢复上次主屏/光标/termios，也不宣称恢复成功；用户需在 shell 恢复终端后重开。hermes 的无条件 1049 复位不采用。

**第 5 稿 CTO 设计修正（绑定，CR28D-300/-350）——宽度必须测量，不得手写。** 宽度在三轮审查中重复失败，撤销第4稿 unicode-width 模型加手写例外的方案。编辑、删除、原文索引仍按 grapheme；布局、裁剪、换行、选择与逻辑光标只使用下述逐路径实测表。unicode-width、Ratatui CellWidth、libc wcwidth 只能作差异诊断，不能产生或覆盖表值。中文、Ambiguous 标点、框线及已约定 emoji 必须原样显示；ui.icons=ascii只选择UI图标样式，不替换正文字符。

第4轮报告的纠错观察是：✈️在vt100 0.18.1与tmux 3.6均为2列，👨‍👩‍👧‍👦分别为8/2列，👍🏻两者均为4列而unicode-width为2列。这些是**审查证据，不是本稿填入的生产表值**；也不是可以继续扩充的手写例外表。T07必须从下述工具生成全部值，第5稿不提交虚构测量表。

1. **提交语料与测量器（T07交付）**：仓库内新增 `tests/fixtures/terminal-width/corpus.json`、逐路径 `direct.json/ssh.json/tmux.json/ssh-tmux.json` 和版本清单 `manifest.json`；测量工具放 `tools/terminal-width/`，工具/语料/生成表同提交。语料以原始UTF-8字节和码点序列标识grapheme，不做NFC归一化，不含人工width字段。必须包含中文正文与全角标点“，。、《》”、East Asian Ambiguous的“”—…├─│αβ→★✓及‘’·、普通emoji🙂✅、VS15/VS16成对样本（✈︎/✈️、❤︎/❤️、⚠︎/⚠️、▶︎/▶️）、ZWJ（👩‍💻、👨‍👩‍👧‍👦及家庭变体）、肤色修饰（👍🏻、👍🏽及各修饰符）、旗帜（🇺🇸及其他区域指示符对）、组合重音é/多重组合符、孤立组合符，覆盖两份报告点名的全部簇。中文正文至少包含“中文终端工作台、需求审查、任务状态、输入输出、证据与恢复”的全部不同字素，不只用“中”代表中文；ASCII可打印字符亦全部纳入，供标尺/回退使用。T07提交的corpus逐项列出精确UTF-8字节与码点，清单固定后每项必测；扩大正文覆盖只能追加语料并重生成全路径表，不能凭字符所属区段推算未测宽度或手改数字补表。
2. **逐路径测量**：直连用锁定的vt100 crate在进程内解析真实原始字节并读取screen/cursor；tmux在独立socket/pane内写原簇并读实际光标报告（pane cursor/DSR），SSH路径通过SSH在应用所在PTY请求cursor report并由本地锁定终端实际响应，SSH内tmux同时记录远程pane和外层终端。不能在SSH远端调用wcwidth冒充测量。每簇在清空的宽屏上定位到已知起点，写原簇后取光标差及cells，再写ASCII标尺；另测非零起点、拼接与末列换行。固定TERM、UTF-8 locale、tmux配置、终端/包装器完整版本及可执行文件或crate摘要；重复探针结果不一致即失败。表每项含cluster→实测列数、各层cursor/cells及原始记录引用，manifest绑定corpus、测量器、Cargo.lock、路径配置和版本；规范化表不含时钟/临时路径等非确定字段，原始报告单独保存。
3. **渲染只消费实测值**：已确认活动路径时取该路径表；不知道是哪条支持路径时取所有可能路径的交集：仅每个可能路径都有该簇且实测值一致才原样渲染，采用这个共同值。不能从TERM或TMUX变量单独推断完整外层终端/版本；无法验证身份时保留所有可能路径，不能猜一个窄值。任一层无法安全呈现（含零宽独立簇、测得内容依赖相邻单元格、内外层对齐失败）也进入下面同一回退。T07的路径识别须给出可核对的来源；没有可靠识别时默认走交集，仍须通过四路径正式矩阵。多个终端层时保留逐层观察，不能只对齐pane却遮住外层漂列。
4. **唯一回退（D10具体呈现提案）**：对不在适用表中的簇，或可能路径之间宽度不同的簇，统一显示可逆ASCII码点串 `<U+XXXX U+YYYY ...>`（大写十六进制、最少4位），不输出不确定宽度的原簇。所用ASCII自身必须已在每个可能路径实测为单列，标尺也核验这一点；不满足则该路径不兼容、正式检查FAIL，不能宣称光标安全。码点串可逐ASCII换行，编辑光标/Backspace仍只停在原grapheme边界；原文、提交、复制和导出逐字节不变。缺表也不能默认为unicode-width。支持集外的终端仅记录未验证兼容限制，不借M4 PASS。所有已在语料且路径一致的中文、Ambiguous/框线、🙂✅、✈️❤️⚠️▶️、🇺🇸和肤色修饰符簇保持原字素；最后一类使用实测跨度，即便它不同于unicode-width。已确认路径上可安全呈现的ZWJ按该路径表；路径未确定且有分歧时用同一回退，不能按“全部ZWJ”或“全部emoji”分类回退。
5. **绘制跨度必须真实生效**：T07需把测得跨度传到最终输出、裁剪、清除旧行与后续光标定位；不能只改editor宽度而让Ratatui再次按其内置宽度计算覆盖。实测跨度可能大于2，渲染单元记录完整占用区间与continuation，编辑/resize/窄屏不留旧尾格或半个字；回退串在小屏可滚动，用户可读到完整表示。每条语料在每条路径的实际输出均逐层验证无漂列/无半字，HoH逻辑光标等于终端报告，且标尺落在所占区间之后。缺表与路径未确定分歧的负例同样测量最终回退光标。
6. **验收现场再生成**：AC-015-01在检查机器上使用同一已提交测量器逐路径重生成到证据目录，将版本/config/corpus/规范表逐字段与已提交版本比较。任何版本漂移或表差异均FAIL，不能重写源表后继续同候选检查、不能SKIPPED/78规避。需更新终端版本时先独立重测、审查生成diff、提交新表和版本，再冻结新候选重跑；§5.1性能环境中“软件补丁仅记录”不豁免本项宽度版本匹配。M7两平台若版本不同则提交各平台路径表，仍在同一最终检查现场比较；不得沿用另平台的值。

| 路径 | 外层终端/版本与宽度观察 | 恢复观察点 |
|---|---|---|
| 直连 | 测试终端仿真器 vt100 0.18.1（Cargo.lock）；读取其 screen/cursor 状态 | 启动 HoH 的同一 shell、HoH 退出后 |
| SSH | 本地外层 vt100 0.18.1；OpenSSH 10.2p1（本次读到的发行版包后缀一并记录）；不是把 SSH 当宽度模型 | 远程 shell 内 HoH 退出后、SSH 尚未退出 |
| tmux | 外层 vt100 0.18.1 + tmux 3.6 pane 模型；逐层核对 pane cursor 与外层光标位置（扣 pane 起点） | pane shell 内 HoH 退出后、tmux 客户端尚未退出 |
| SSH 内 tmux | 本地外层 vt100 0.18.1 + 远程 tmux 3.6 pane，SSH 同上 | 远程 pane shell 内 HoH 退出后、tmux/SSH 均存活 |

这是可复现的仿真器路径，不能冒充 xterm/VTE GUI 实测；实际运行记录全部终端/包装器版本、TERM、locale 与回退策略。本表只声明待测路径的版本；生成manifest记录实际完整版本。升级后须重生成并提交表，验收不能自动接受版本漂移。四路径各在会话内记录进入前后 stty -g；同一应用 PTY 输出 tap 保存完整开启/关闭序列，逐个模式核对配对/顺序，tmux 捕获发生在转译前。终端模型可查询时也读回 mode；应用字节证据不能被包装器自动恢复替代。包装器退出后再核对本地外层 tty/modes/哨兵，只作为额外无泄漏检查。

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

**SendInput的唯一低空间结果（D6，CR28D-204）**：正文超过16 KiB需先封口对象，预检不足时HTTP唯一为 **503 STORAGE_LOW_SPACE**，沿既有Error体，`details={reason:"STORAGE_LOW_SPACE",available_bytes,required_bytes,min_free_bytes}`，required_bytes为本次object_write_bytes+min_free_bytes；无202、command/input接受行、input.accepted或public.message，客户端逐字保留草稿/目标并显示“未接受”，空间恢复后仅用户显式重试、同幂等键最多接受一次。此处不是DB_FULL：后者只在状态事务实际ENOSPC/EDQUOT时使用。若HTTP已经202而投递阶段（含>256 KiB短文件引用的物化）低空间，既有输入保持QUEUED，在同一M3投递链持久化input_id+STORAGE_LOW_SPACE及上述字节数诊断，绝不先标DELIVERED/APPLIED；空间恢复后由既有投递重试/安全点重用原input_id与正文摘要，不再新增用户块。状态事务本身也无法写入时保留最后已提交QUEUED、报告DB_FULL，不能声称诊断已落库。T03负责对象写入和受理前子例，T07在AC-015-03补文件投递子例，不要求T03提前交付T07的传输包装。

**CLI低空间拒绝（D6，CR28D-304）**：沿REQ-004/ADR-031现有503映射，`hoh -p`（含--resume）在受理前收到STORAGE_LOW_SPACE仍退出 **69**，这是客户端提交失败，不写run.result、不把Run改成BLOCKED。保留HTTP code/details，文本明确“输入未接受：存储空间不足（STORAGE_LOW_SPACE）”及available/required/min_free，不只显示泛化的“host不可用”。遇此码不自动重试（与本节仅用户显式重试一致），其他可重试503沿原规则。AC-064-04核对text/json/stream-json的现有拒绝输出形状、退出69和零接受事实；正式应用只补REQ-004退出码表/ADR-031说明，保留原AC/编号与69，不另造码。

**两种独立路径**：低空间预检触发但状态事务可提交，PauseRun/CancelRun 仍202且各有持久事件，UI显示已接受；只有状态事务自身 ENOSPC/EDQUOT 才503 DB_FULL且无202/无乐观状态。读取旧已提交事实与终端脱离仍可用。SQLite writer busy 则503 DB_BUSY，保留最后快照/stale，客户端不得被 busy_timeout 阻塞绘制和脱离。实际受限文件系统 ENOSPC 由 `scripts/dev/test-infra.sh` 在基础设施阶段用已授权 sudo -n 准备隔离 loop/ext4 与开发用户权限，记录 test-infra.json（路径、容量、挂载、清理）；检查本身非root。EDQUOT 用现有可注入文件系统 seam 覆盖并标注 fault injection，不伪称启用了真实配额。缺 ENOSPC 设施为检查 FAIL/非零、阶段 BLOCKED，不跳过。临时文件/fdatasync/rename/登记各故障点均保留首次失败；完整文件登记前崩溃为48h孤立对象，登记后已有引用始终保护。

### 5.1 唯一性能测量入口与环境（D4，CR28D-01/-69）

选择**既有 integration 检查的串行 release 测量阶段**，不新增perf check、不更改14项检查的归属/mode/门槛/超时。M4-T01不提前声明尚无生产者的工件；M4-T03交付驱动时按本CR批准范围/relock在integration.artifacts追加 `m4-measurements.json`、`m4-environment.json`、`m4-samples.jsonl`；argv仍为 `bash scripts/product/cargo-tests.sh integration`。既有debug测试全部收取后，串行构建release HoH与独立example测量驱动（非自动libtest target），依次运行场景。禁止并行cargo/另一测量。HoH二进制为冻结源码的普通release构建，不启用debug故障钩子，不改变setsid、后台存活或Attempt隔离逻辑。

**测试如何施加资源包络（D4，提案）**：T03使用检查专属的systemd user slice，产品host仍由客户端的真实service/scope两条路径启动；不要求host、TUI、driver或Attempt同pgid。外部测试启动器及配置只在检查临时根中，发行包不包含、不读取这些设置：

1. **私有manager与安装的明确所有者（CR28D-302，绑定）**：沿 `crates/hoh-app/tests/installed/mod.rs::Manager` 的私有user manager机制，由真实用户manager仅托管唯一 `hoh-m4-manager-<execution>.scope`（Delegate=yes）；测量用service、scope与slice全在其内的测试专属 `systemd --user` 管理器中。测试根以 `mktemp -d /home/ai/dev/m4spec-XXXXXX` 建于参考项目盘的同mount source/FSType/磁盘类别，数据库、对象、旧版本构建与安装也在此盘，不用/tmp或其他tmpfs。子进程环境单独设置测试HOME=`<root>/h`、XDG_RUNTIME_DIR=`<root>/r`及各XDG目录；清除继承的DBUS_SESSION_BUS_ADDRESS，systemctl连接私有`r/systemd/private`，不修改调用者HOME或真实manager环境。私有根需保持Unix socket路径长度可用。实际模板名仍为产品硬编码的 `hoh-host@.service`，仅由安装器写入 `<root>/h/.local/share/systemd/user/hoh-host@.service`（实际XDG_DATA_HOME路径），测试配置覆盖目录为 `<root>/h/.config/systemd/user/`、实例runtime drop-in在 `<root>/r/systemd/user/<instance-unit>.d/`，启动空测试target避免系统用户服务随启动；安装命令/版本根分别为 `<root>/h/.local/bin/hoh`、`<root>/h/.local/share/hoh/`（按安装器真实布局核对），绝不覆盖真实HOME的模板/二进制。两manager身份/ControlGroup、模板路径/hash与ExecStart/EnvironmentFile逐项记录并验证，产品 `client::unit_for` 必须真实认出测试安装。禁止伪systemctl与直接__host替代。该机制当前900s与/tmp默认值不能原样用于本测量：外层scope的 **RuntimeMaxSec=3600**，从启动到检查起点+3300s的清理完成均有覆盖；监护者另按下面3000/3300截止主动收尾。作用域清理后才删除根；缺设施或连错manager直接FAIL。
2. 启动前建立唯一 `hoh-m4-<execution>.slice`，将MemoryMax=8589934592、MemorySwapMax=0、CPUQuota=400%施加在这个**共同父slice**，限制合计而非每进程各8GiB。为本次 `hoh-host@<instance>.service` 写实例级runtime drop-in：Slice指向该slice，CPU/Memory限制不宽于父级，CPUAffinity=0 1 2 3；不改ExecStart、EnvironmentFile、Delegate、Restart或KillMode。不写全模板drop-in。TUI/driver在该slice下的独立scope中，由taskset设CPU 0–3；外部监护者在slice外，不计核心。
3. scope分支由同一release从非安装路径启动，真实 `unit_for` 拒绝service而进入 `client::delegated_scope/launch`。测试PATH只给systemd-run放一个**透明转发启动器**：探测/--help原样转真实工具；唯一允许扩展的是本次项目的真实host启动argv，添加唯一unit名、同父slice及MemoryMax/MemorySwapMax/CPUQuota属性，并用taskset exec原hoh argv。保留--user/--scope/Delegate=yes及其余参数、环境和pid连续性，记录转发前后argv与真实工具hash；systemctl不替换。设置仅属外部测量设施，发行release无新flag/env入口。若scope创建失败而产品走direct降级，记录该行为但该测量FAIL，不能把无约束进程纳入参考PASS。
4. 每Attempt仍由产品在host委派树内创建独立 `attempt-<digest>` cgroup（当前源码的“Attempt scope”是此子cgroup，并非每个都有systemd unit名），用其既有Witness/启动门机制登记与继承父限制。若未来实际创建独立unit/scope，必须在其start gate打开前同样放入本slice并核对限制，不能从父子PID推断继承。不能为了测量禁止Attempt；生命周期夹具至少经真实派发路径启动一个有后代的受控fixture Attempt（product+synthetic，不称live），另单列Agent开销。
5. 启动与每次重启后从host.json、manager MainPID/ControlGroup及Attempt登记交叉核对，记录boot_id、PID、/proc stat start_time、pidfd、pgid、unit/InvocationID、cgroup路径及inode链；pgid只作观察。逐个PID读回有效affinity，逐cgroup记录祖先链的memory.max/memory.swap.max/cpu.max及有效最严限制。AllowedCPUs仅在cpuset实际可用且读回生效时作证；当前VM没有委派cpuset时用service CPUAffinity和scope taskset，不能声称未生效的AllowedCPUs已约束。任一子进程扩宽affinity、逃出slice或单位不受限制均为环境不符，相关TEST SKIPPED/退出78，不静默放行。

第2稿记录的VM是Ubuntu26.04.1 x86_64、Intel Core Ultra 9 285K、12vCPU、MemTotal=67396161536B、无swap、项目盘/dev/sda2 ext4。rotational=1不能证明物理SSD；M4参考profile仍提案使用该盘且不放宽预算，M7独立核验docs/01本地SSD。旧资源探针只证明当时service的memory.max=8589934592、memory.swap.max=0、cpu.max=400000 100000及taskset [0,1,2,3]，不是本稿整套slice/service/scope已验证的证据。本包络不声称整台VM只有4CPU/8GiB。

**观测与计数**：通过实际host unit/scope、TUI scope和每Attempt cgroup枚举进程，核对每次样本的内核身份；从各自cgroup的cpu.stat、memory.current/memory.stat读CPU/内存计量，从成员 `/proc/<pid>/status` 的VmRSS得到逐进程RSS。cgroup内存包含页缓存，不能把memory.current冒称RSS；父子cgroup累计也不重复求和。host与TUI的RSS分别记录，idle **host+TUI总和≤150MiB**，因此各自≤150MiB；活动峰值各≤256MiB。Attempt/Agent/driver/监护者另列、不能算进核心或藏掉其资源包络。host重启换PID时结束旧区间、登记新身份，不沿用PID数值。REQ-068的4CPU/8GiB、至少1000样本及原阈值全部保留，M4不签M7。

**检查进程生命周期（D4）**：T03在debug_integration开始前，由**真实用户manager**启动唯一 `hoh-m4-guardian-<execution>.service` 独立监护者（RuntimeMaxSec=3600），位于私有manager外层scope之外、runner进程组之外，使用启动时保存的真实manager连接，不能随测试的XDG设置误连私有manager。截止时间基于本次检查起点（工作最晚3000s停止、清理最晚3300s完成，保留外层3600s内至少300s余量），在任何被测进程启动前持久保存外层manager scope、私有manager内的slice/unit/project/Attempt所有权清单，持有runner/driver的pidfd并监测死亡或截止时间。driver的正常finally/trap可请求清理；driver/run_check被SIGKILL或超时也由独立监护者执行同一收尾，不能依赖死者trap或“同pgid会一起死”。服务Restart=on-failure保持原值，以显式unit stop撤销重启；只停止登记的实例/scope，不按hoh名字杀进程，不停止整个用户slice。

清理先用产品已有Attempt取消/回收机制与身份守卫，核对Witness、进程组/marker、cgroup.kill及populated=0，再stop已登记host unit/scope与TUI/driver scope；host已死则由监护者按已登记cgroup Witness清理，不对复用PID或新cgroup发信号。监护者保留本检查slice到核验结束：仍存在的每个被测cgroup必须populated=0；已由manager移除的子cgroup须有其Witness的移除证明，且这个尚存的共同父slice最终populated=0（覆盖已移除子树），所有已登记pidfd均终结。随后通过真实manager停止本次外层manager scope并核验该外层cgroup populated=0及全部pidfd终结，才移除本次runtime drop-in/slice、隔离数据库和测试安装；真实用户安装/模板始终不动。停止/退出时间、逐unit结果、PID身份、cgroup样本与删除证明写入声明的三份工件；清理失败/未知不PASS，保留未释放对象供诊断。

**run_check进程组被SIGKILL时**：私有systemd可能与runner同组先死，其内service的setsid/Attempt后代却仍可能存活；独立guardian不依赖该manager响应或runner trap。它在pidfd观察runner死亡后，先按清单核对Attempt Witness并清理，随后向**真实manager**发stop `hoh-m4-manager-<execution>.scope`，以外层scope的受控子树作为最后收尾边界。核验外层cgroup的身份/inode和populated=0，或已移除证明及全部登记pidfd终结；私有manager已死时不要求其unit stop能返回成功，也不把该失败单独伪报残留已清。scope的RuntimeMaxSec仅为第二道上限，不能替代故障后≤30s的证明；guardian不可用/清理证据不全一律FAIL。外层scope存在时不得由其他同名对象替换；PID/cgroup身份不符即保留诊断，不能杀错对象。

T03在**service与scope两条真实路径**各做正常结束、driver失败、外层run_check超时/取消、直接SIGKILL driver/runner的实验；至少一组带有真实cgroup的fixture Attempt及其setsid后代。每例在检查结束/故障触发后≤30s由外部监护者证明上述空树/身份终结，不能以客户端退出代替后台结束。正式测量开始前确认本次没有未知旧host，前后核对非本次unit未被停止；清理预算在总时限中预留。此设施仍待T03实测，生命周期修改按docs/19双独立审查。

**测量排他（CR28D-303，D4方案）**：选用全机同一inode的读写锁 `/run/lock/hoh-m4-measure.lock`，由test-infra预置权限/登记设备号与inode，不按repo/用户/候选另建锁，不unlink重建。所有 `run_check.py` 执行及仓库cargo包装器（包括普通CI、bundle里启动cargo的路径、开发/QA/Review调用）在启动负载前必须取共享锁；含M4测量的integration从preflight开始取独占锁，贯穿debug、两个release构建、造库、回填、measure直到guardian证明清理完成，避免runner已持共享锁再升级的死锁。子包装器只复用经父进程/FD与inode校验的继承锁，不因某env字符串声称已加锁而跳过，也不再次等待自己的独占锁。独占锁由外部guardian持有，在runner被SIGKILL后仍覆盖收尾；guardian退出时FD关闭自动释放。锁等待最多30s并计总3600s，未取得则相关TEST=SKIPPED、detail.reason=MEASUREMENT_BUSY，整次退出78；不能开测或PASS。初始化不合规/锁inode不符为设施FAIL。T03补包装器入口与两个检查并发的负例：持锁测量时另一个cargo/run_check负载不得启动，取消/杀runner后只在清理完成才可启动；同机其他worktree也走同一锁。原始记录包含获得/释放时刻、owner及FD/inode，证明测量窗口内独占。

锁只约束遵守协议的执行者，因此准入还要求同机已运行的HoH检查/cargo全部已纳入包装器并排空旧无锁负载；无法确认或仍有无锁竞争负载时SKIPPED/78（reason=MEASUREMENT_NOISE），不能PASS。本稿选定的是CTO允许的排他锁分支，不另用HoH自身CPU压力作为跳过性能失败的理由。测量期间禁止启动绕过包装器的cargo/其他检查；已知有非仓库竞争作业的机器须先安排排他窗口，不能只锁本repo便宣称全机安静。监护者记录测量前后及每1s窗口内CPU 0–3上可运行进程的内核身份与所属cgroup，允许集合为本次测量树、测量/系统必要服务（冻结环境中逐项声明）与空闲内核任务；发现未登记工作负载、观测缺口>2s或不能核实锁覆盖，则相关测量SKIPPED/78、reason=MEASUREMENT_NOISE，保留原始记录和已有FAIL。允许集合不能用宽泛用户slice掩盖其他检查，不在运行中追加例外。该准入与采样实现是D4待审方案，T03须证明可执行；环境有效而HoH自身CPU竞争/预算超限仍FAIL，不能以产品负载或平均值为借口改成SKIPPED，也不重跑覆盖首次结果。

冻结前 `.hoh-dev/environment.json` 增加 `m4_reference`（随 candidate.environment 摘要绑定）：profile_id、硬匹配键、版本观察值、fixture 与预算。**硬匹配键仅为** CPU family/model/可见拓扑、物理内存、项目盘 mount source/FSType 与已声明后端类别、允许CPU/逐进程affinity、共同slice及产品unit/Attempt树的有效MemoryMax/MemorySwapMax/CPUQuota实际值、构建 profile/release 优化选项。OS 架构与 UTF-8 locale 可用是执行前提；内核/Rust/终端仿真器/locale 名称与版本/tmux/ssh/OS 补丁版本、boot_id/可用空间/rotational 是记录项，不逐字匹配，不能因补丁升级把所有 integration 标为环境不符。release binary hash 在检查内生成，记 m4-environment.json 并绑定冻结源码/构建输入，不事后改冻结 environment。补丁升级时在新候选冻结前记录新观察值，做一次同 fixture/同预算的对照测量并保留原始样本/上次引用；软件升级本身无需重开 CR，不沿用旧 PASS；但tui-pty宽度版本/实测表必须按§4精确比较，漂移即FAIL并先提交新表，不能借本段豁免。改变硬件/profile 或降低预算须修订并复审 D4；不通过重写 reference 把另一机器冒充同 profile。

| 测量 | 数字预算（参考环境） | 采样/执行 |
|---|---|---|
| 多行编辑/滚动/resize 输入到外层 PTY 可见 | p95≤100 ms | 每种80×24、180×40各≥1000次；包括50ms文本合并负载 |
| 事务 commit 到外层屏幕对应seq可见；快 SSE commit到接收 | 各 p95≤500 ms | 各≥1000次；单调时钟、seq、原始样本完整保存 |
| timeline 查询/块首屏 | 各 p95≤500 ms | 每种过滤与每种块≥100次；不能只量renderer |
| writer busy期间脱离 | ≤1s | 参考环境release小场景，产品writer正常批任务占用期间请求脱离；不靠release调试钩子，记录锁/队列与PTY时钟 |
| 绘制/文本增量合并 | 最长50 ms或16 KiB先到 | 调度探针记录窗口；工具/决定/验收事件完整计数 |
| idle 核心与活动峰值 | idle总和≤150 MiB；活动host/TUI各≤256 MiB | 活动60s、idle10s，100ms采样；包含后台索引完成后的稳态 |
| 百万事件夹具、迁移回填 | 构建≤600s；回填≤600s；kill→reconcile/host-ready各≤30s | 单writer产品批提交≤500行/1MiB，ACK/fsync不绕过；不改PRAGMA降低持久性 |
| 本检查总预算 | 仍≤3600s；工作阶段合计≤3000s，至少600s余量 | T03先实测既有integration（含debug tui_pty）、release构建、造库、回填、测量各阶段并记录duration/环境；在冻结m4_reference内为每阶段写数值时限及预计合计，runner逐阶段强制截止。保留造库/回填各600s上界，其余预算与两者合计≤3000s，不得分配满3600s；剩余600s中清理≤300s、调度余量≥300s。无法满足则D4升级CTO，不放宽总超时或删场景 |

**阶段时限与重测（CR28D-205）**：保留 `cargo-tests.sh integration` 的 `--test '*'` 选择器，debug阶段包含普通tui_pty测试（busy_timeout、信号/故障、布局和输入），不能漏算它们。冻结前的m4_reference必须给出debug_integration、release_build、fixture_build、backfill、measure五个正整数秒时限；各工作阶段串行、单调时钟独立计时并由监护者强制截止，限额总和≤3000s、fixture_build/backfill各≤600s。值缺失/总和不合规在启动前FAIL；阶段超过自己的冻结时限立即记录 `reason=PHASE_TIMEOUT,phase,limit_seconds,elapsed_seconds`、受影响TEST为FAIL并非零退出、进入清理，剩余阶段不得借用余量掩盖首失败；run_check外层3600s照旧。清理≤300s、另留≥300s调度余量，单个生命周期证明仍≤30s。这里不编造尚未执行的阶段耗时/分配值：T03的done_when以实测填齐可执行预算为交付条件，缺此条件runner未交付。

T05、T07、T10每次加入场景后都必须在各自冻结候选上重测**全integration五阶段**，在m4-measurements保存每阶段原始起止/elapsed/limit、选择器及实际测试清单、总耗时和余量，不能复用T03时长。T05涵盖新增块/PTY，T07涵盖busy/信号/输入/adapter与RSS，T10涵盖最终百万库组合；debug与release每段都受限。任一超过阶段/3000s工作总额，或清理后不足300s调度余量，均FAIL并交D4重规划；不删tui_pty、放宽3600s或修改失败记录。调整阶段分配也须在下一候选冻结前记录，不在检查中动态挪预算。

**旧schema生产者与升级计时（CR28D-301，绑定）**：冻结m4_reference之前，T03选定并记录已完成M3、尚未登记/运行迁移003的真实前序产品commit（完整Git ID/tree、002 SQL及Cargo.lock摘要）；字段 `old_schema_producer` 缺失或该提交不是002生产实现即设施FAIL。当前M3尚未完成，因此本草案不拿CR-026文档提交或当前001版本伪造这个commit；T03交付时必须填实。每次正式integration在自己的临时根展开该已记录提交并以其原Cargo.lock/toolchain构建普通release HoH及调用**该版本Persistence/单writer**的独立造库驱动，并串行构建候选release，二者全部计入release_build阶段；记录实际源码树、构建argv、库/二进制hash，不复用别次检查的成品冒充本次构建。驱动只能调用旧版本生产写入接口（单批≤500行/1MiB、真实ACK/fsync、持久性PRAGMA不变），无debug钩子、无直接SQL插入、无候选writer降schema、无删除003结构的捷径；旧实现缺可调用接口或不能满足预算则D4交CTO重规划，不能加一个仿造旧writer。

fixture_build从旧writer首次创建库到一致性备份完成计时≤600s：100 Run×10 iteration、1,000,000条由旧生产事件构造器产生的事件与10,000×16KiB已登记对象，关轮和引用真实可核对（M4 public_segments只在升级后生成，不能预写到002）。旧进程完成并关闭后读回user_version=2、无003索引，保存权威表行/水位及对象hash，再用产品一致性备份克隆，不能只拷DB主文件漏WAL。百万002库在这次检查只造一次；场景克隆只在同次candidate/spec/environment/fixture摘要下复用。旧生产者是版本绑定的fixture来源，不能把其行为记为候选PASS。debug小库功能测试仍调用同一产品接口。

backfill阶段计时t0为候选第一次尝试打开该002库，**早于迁移、reconcile和host-ready**，直到完整索引到H并原子切换generation，≤600s且受冻结阶段时限约束。记录首次ready≤30s与index_ready=false期间的可读快照/Pause/Cancel；不得等回填完成才起计或接受控制。独立未中断克隆与回填中断克隆都在同一backfill阶段内顺序执行、共享其≤600s总额，候选各自首次open时间亦单列，克隆/等待/停机/重启均不扣除。中断例在已观察 `0<indexed_through<H` 的批间对host SIGKILL（保留manager/guardian），重启后kill→reconcile及host-ready各≤30s，从持久断点续跑；不能把完整索引ready作为30s的条件，也不能重新给600s。若没在未完成回填时杀到则子例FAIL，不用debug暂停伪装release路径。前后权威行/seq及对象hash逐项不变，正常新增reconcile/控制事件按原因记录并分开核对；最终索引与未中断克隆对同一H逐项相同，无遗漏/重复、控制ACK持久。两例的总阶段预算装不下仍交D4重规划，不放宽超时。

带绝对时延/RSS/大容量义务的 TEST-012-01、TEST-015-03、TEST-015-05、TEST-064-01、02、05 **唯一 check_id 都是 integration**，但不再要求故障注入也运行在 release。先运行普通 debug integration libtest，TEST-015-03 的 paste/HTTP/三 adapter/文件错误功能、TEST-064-01 的迁移失败回滚与旧 binary 拒读、TEST-064-05 的未提交事务 X 隔离均在这里：通过同一 writer/迁移函数的 debug 专有 seam 暂挂/故障，不从外部 SQLite 写 DB。名称分别带 test_015_03/test_064_01/test_064_05，CI 的 cargo test --workspace 真实执行这些功能；它们不宣称完成性能义务。随后 release example 只执行真实规模/时延/RSS与正常升级路径，不带 debug 钩子，不伪造 release 的故障注入结果。两阶段日志保留各自断言，同一 TEST 的所有行都必须 PASS；测量 runner 另有必需子场景清单，缺任一或只有 debug PASS 时该 TEST 必失败，不合并覆盖首失败。tui-pty/unit 的普通回归用无这些 TEST ID 的助手，避免跨 check 假证据。

`cargo test --workspace` 与 `--test '*'` 不发现 example，CI 不跑百万测量；正式 integration 的三个声明文件从 T03 驱动交付时才同步生效，连提前失败也由 EXIT 收取器保存 summary/environment 与空 samples，明确未执行原因，不补造样本。后续 T05/T07/T10 增加各自必需场景；尚未交付的义务写 schema 合法的 SKIPPED 行，detail.reason=NOT_DELIVERED（不写NOT_RUN状态），不能把子集声称全 AC 通过。

**非参考环境与检查出口（D4）**：M4–M7/v1 的正式 integration PASS 只收自本节同一参考 profile；Ubuntu24.04 矩阵收取 tui-pty，普通功能回归用 cargo test，不在其上运行/申报正式 integration PASS。显式试跑非参考 integration 时，受影响 TEST 逐项写 schema 合法 SKIPPED，detail 为 reason=REFERENCE_ENV_MISMATCH，stderr 同标记，脚本退出 **78**；其余已运行功能原始 PASS 行保留，但整次 execution 非零，不作任何正式 integration 验收。退出3继续专指原脚本 NO_TESTS，不再承担环境不符。test-result 无 BLOCKED/NOT_RUN 枚举，不写进去；SKIPPED仅是未测诊断，required AC仍不通过；这些检查绝不写DEGRADED，不能据此发布COMPLETED_DEGRADED。缺必跑设施/路径、环境符合但预算超限为 FAIL/非零。该限制及每次 integration 的成本必须在 D4 批准时确认；T03 实测若不能在3600s内留600s余量，停止启用测量并交 CTO 重新规划（可提专用 required check，但需另审修订，本稿不自行加检查/放宽超时）。p95 取全部样本升序第 ceil(0.95×N) 项，冷启动和慢样本都保留。

## 各需求的细化与 AC→测试映射

应用规则：每项保留 id/category/priority/milestone/title/required_release/release_class/release_targets 和原 summary；改 `spec_maturity=detailed`，把本节“行为/失败”分别转为 behavior/failure_modes，把引用的提案与已定事项放入 decisions，补 doc_refs。AC 表的输入→操作→判定分别是 given/when/then，同号 TEST 为 test_id。每条表行同时定义 `acceptance-scenarios.json` 中的 fixture/steps/pass_criteria；observables 至少包含下列该行要求的屏幕/命令/HTTP/DB/原始日志，不能只保存截图。

每个 TEST 恰归一个 check_id；§5.1 的性能/RSS断言只在 integration 的 release 阶段，功能/故障子例按该节在同一检查的 debug libtest 执行，两者均为必要证据。tui-pty 场景放进该检查实际运行的 `crates/hoh-app/tests/tui_pty.rs`（可调用公用 TestBackend 断言，但正式结果来自此 target）；unit 在 lib/bin 单元测试，integration 在 tests target，recovery 由既有 recovery runner 收集。名称含 `test_NNN_MM`，原始产物写在本检查 `$HOH_EVIDENCE_DIR` 下。表中为拟执行的 **product** 检查；其中明确的stub响应、故障注入和FakeAgent/固定事件夹具分别标注，stub不作为对应真实路径证据，均不能声称live。现阶段全部 NOT_RUN。

### REQ-010 · Workspace 工作台（4 条）

行为：顶部显示项目、已生效 mode、Run、基线身份；正文是统一的公开对话与工程活动，团队摘要可折叠；底部输入和 statusline。pending、输入四态与断线标记由 host 事实驱动，202 只说明接受。选择视图、展开和连接本身不调度任务。失败：API 断连保留带 stale 标记的最后快照与草稿；409 不重放用户命令为新动作；未知事件可见且不崩溃。

doc_refs：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/adr/ADR-014.md`、`docs/adr/ADR-031.md`；M3 依赖 REQ-007/017/028。提案 D1、D4、D11。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-010-01 / TEST-010-01 | tui-pty | 120×32 PTY；三Agent承接commander/developer/qa；水位H；既有COMPLETED Run最近ACCEPTED提交B，当前R的run.baseline=B0≠B、BUILD；公开U/A、task/candidate活动、带rationale与impact的decision | 打开Workspace，折叠团队，打开基线详情；Ctrl+P列动作，分别打开Task/Issue/Evidence抽屉；同快照TestBackend与PTY对照 | 同屏项目/R/B、BUILD、U/A/活动、输入/statusline；decision显示依据和影响；详情B0不替换顶栏B；默认一个主区、三职位摘要一致；面板列全部已实现动作且三抽屉列host对应ID/状态/证据引用、可返回；打开/切换的POST与Task/Attempt新增为0 |
| AC-010-02 / TEST-010-02 | tui-pty | 写 Attempt 运行中，M3 的模式与输入投递可控 | `/plan`；普通输入发给 commander，先接受再 delivered/applied；另一个输入被拒；模拟一次 expected_revision 409 | pending 时仍显示 BUILD 和待 PLAN，安全点后才显示 PLAN；两个输入按 ID 显示 QUEUED→DELIVERED→APPLIED、QUEUED→REJECTED，保留拒绝原因；显示实际角色/epoch/boundary；409 不改 mode，不自动重发新命令 |
| AC-010-03 / TEST-010-03 | tui-pty | H=100、草稿“继续核对”，应用105后断线，host到110；可触发真实索引重建与writer busy锁 | 恢复并重复105；debug钩子触发同一重建路径，以旧page cursor查询；静态核对SSE 410 OpenAPI并以明确stub响应测试客户端解析；writer持锁超过busy_timeout，等待请求已进入该锁后尝试脱离，再解除 | stale/105期间保留草稿，补齐110后去stale、块不重复；page真实过期410且顶层snapshot_watermark=110，重取快照保锚点；SSE矛盾真实409 CONFLICT，SSE 410仅静态/stub兼容覆盖，v1产品不可达；busy时控制503 DB_BUSY且未接受、旧快照可绘制；探针证明脱离在writer释放/超时前完成，不等满busy_timeout且无Cancel（绝对1s在§5.1参考测量另核）；恢复不自动重发、不乐观加进度 |
| AC-010-04 / TEST-010-04 | unit | 已提交1..20，含未知kind、公开控制字符、pending/input；两种分批与重复5/10 | 比较一次重放/分批/快照增量的规范化投影 | 20个不同seq且逐字段相同；未知kind有UI_UNKNOWN_EVENT/seq；ESC/OSC转义可见、不执行；pending不当生效；事务未提交不发SSE的断言移至TEST-064-05真实host场景 |

### REQ-011 · Team/Focus 布局（5 条）

行为：Team 按配置职位展示绑定、epoch、可用性、当前任务与 pending；Focus 按完整身份查看公开会话，历史按需分页。布局转换保留草稿、展开状态和 grapheme 锚点。失败：角色删除或历史内容缺失显示缺失身份与返回入口；同 Agent 多职位不自动选择；窗口过小不 panic，也不修改执行状态。

doc_refs：`docs/03-ux.md`、`docs/04-architecture.md`、`docs/adr/ADR-022.md`、`docs/adr/ADR-024.md`、CR-025；M3 依赖 REQ-017。提案 D1、D9–D11。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-011-01 / TEST-011-01 | tui-pty | 默认Workspace、无预选角色；六职位，cto/developer同为claude，各有不同会话；后选developer，块K的offset=12，草稿“尚未发送” | 先未预选打开Team，再从claude进入Focus；在选择器等待，显式选developer；依次resize80/99/100/120/159/160/180列、32行，选择Team分屏再返回Workspace | 选择器同时列cto与developer及真实epoch/session，选定前Focus内容为空、无默认落入任一职位；选后只显示developer；整个导航无POST/派发/Task或Attempt新增。80/99为单区标签；100–159最多主区+1详情；160/180用户选择后最多三列；默认Workspace不强制三窗；选择身份、K/12与草稿保持，所有行显示宽度≤窗口 |
| AC-011-02 / TEST-011-02 | tui-pty | 七职位含security_reviewer；developer epoch1 A1在途，READY developer Task T2待派，codex换绑待checkpoint | Team/Focus A1；对T2派发一次得到拒绝，再结束A1、生效epoch2并派A2 | 七职位可达；ROLE_BINDING_PENDING确由拒绝派发产生；pending保留claude/A1与待codex；历史Focus仍A1/epoch1，选择当前才A2/epoch2；内容不混；重用session时按Attempt边界显示，session过滤跨Attempt时逐块仍带attempt_id |
| AC-011-03 / TEST-011-03 | integration | 同agent两role、三个session；A1有公开message/tool/error及非公开内部字段；H13真实handoff无attempt_id；一段公开日志缺文件 | 按A1遍历Focus；分别切到H13的role/epoch视图、session视图，再加attempt_id=A1过滤；查询另一role与缺失artifact | A1的公开message/tool/error按offset/seq完整且不混role/session；role/epoch/session视图的handoff集合逐项等于H13，任何Attempt过滤的handoff子集为空，不从session推断Attempt；内部字段不在响应；缺段ARTIFACT_NOT_FOUND，descriptor保留身份与缺失提示，不为空成功；无spawn/任务写入 |
| AC-011-04 / TEST-011-04 | tui-pty | 80×24、角色选择与草稿状态；可访问状态页 | resize40×8、1×1再120×32；每个小尺寸输入字素“a”、打开状态页、关闭，再选择历史已移除角色 | 40×8显示“窗口过小”及“输入 / Ctrl+C退出”；1×1仅显示ASCII !，Ctrl+P面板提供可逐字符滚动的ASCII诊断 small terminal/input/Ctrl+C exit 与状态字段英文键和值（不用一格截断宽字）；各尺寸接受a且不提交，Ctrl+C选择/脱离仍可达；被省略的四组事实在可滚动状态页逐字段可读；恢复后草稿新增两个a、锚点保持，历史角色有标记；无panic/越界/Run修订变化 |
| AC-011-05 / TEST-011-05 | integration | 每个codex App Server/受支持exec、claude、pi模式的版本绑定兼容fixture；含public message/tool开始结束、compact/wait/usage有与无、secret标记、两role/epoch；真实runner与Persistence，非直接注入UI事件 | 经SendInput接受事务与native frame入口逐帧/重复/超大正文输入，队列背压；在提交前后kill/reopen、取消、EOF；从持久公开块读出 | 三家按§2.2白名单输出逐字段/顺序/身份相同；另经SendInput受理普通/大正文后从Persistence读用户块，正文/hash等于规范化原文（非文件引用提示），重放input_id与可关联的native回显不重复；DB_BUSY背压超过quiet不产生attempt.inactive，解除后真正静默到余下quiet才产生，取消仍可达；启动和结束工具均可查、未知能力显式null，secret/隐藏字段不入供API消费的公开读模型或公开对象；未结束Attempt即可读到已提交消息/活动；队列≤256/1MiB、段≤1MiB；重复无重块、旧epoch不更新当前活动；已提交块重启不丢，未提交尾段有缺口而非完整成功；fixture标product+synthetic/recorded来源，不称live |

### REQ-012 · 可展开内容块（4 条）

行为：message/task/tool/diff/test/evidence/decision/handoff 块都有稳定身份；八类均必须支持展开、复制、搜索、全屏与返回。键盘与鼠标调用同一动作。展开只读，不执行工具或测试；结果引用候选、TEST 与原始证据。失败：缺失/损坏/非文本/权限错误在原块展示原因，保留返回和重试；旧候选标历史/失效，不自动切到新候选同名文件。

doc_refs：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/10-acceptance.md`；M3 依赖 REQ-032/034。提案 D1、D4、D7、D9。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-012-01 / TEST-012-01 | integration | §5.1参考环境/release串行阶段；八类块各一，首尾标记X，Tool20MiB；Evidence C1；父锚点K/200 | 每类先SGR单击选择、双击展开、滚轮滚动，再键盘展开→分页→全文搜索X（首尾两处不重叠，记录offset x1<x2）→n→N→全屏→Esc返回→复制文件，追加另一块；真实PTY采≥100次首屏 | 单击只改选中项且保持折叠，双击恰一次展开，滚轮改变视窗锚点但不改所选块身份/不执行动作；八类键盘动作均可达且不执行工具/测试；搜索先定位x1，n准确到x2、N回x1，所选block_id/草稿不变，仅只读查询且零新增命令；返回K/200、首尾可达、导出字节/hash等于公开纯文本；首屏p95≤500ms且缓存有界；键鼠指向相同身份；环境不符SKIPPED诊断/退出78、AC不通过，禁止静默PASS |
| AC-012-02 / TEST-012-02 | integration | 20MiB+17字节完整UTF-8对象；“中文X”跨256KiB与8MiB边界各一处、末尾一处，连续重叠匹配aaa；NUL/无效UTF-8二进制B含唯一ESC/OSC标记 | 分Range拼全文；search limit=1续扫到结束、重复cursor；无匹配；请求B搜索并用PTY打开B | 拼接hash正确无缺重字；每个匹配的byte_offset精确、跨窗口不漏不重、上下文合法UTF-8≤256B；无匹配完成为空；B为422 ARTIFACT_NOT_TEXT；PTY只含metadata/下载动作，捕获中B原始控制标记字节为0 |
| AC-012-03 / TEST-012-03 | tui-pty | Evidence缺文件、摘要不符；独立认证403变体在隔离host重启后由旧token客户端请求（旧SSE断开、新请求均被拒）；对象chmod造成EACCES、DB_BUSY；旧C1与新C2同文件名；65KiB公开文本 | 打开/重试/返回；文件复制成功与不可写目录失败；尝试设置osc52；移除exports忽略规则再复制 | 分别显示404 ARTIFACT_NOT_FOUND、409 ARTIFACT_INVALID、原403、503 ARTIFACT_UNREADABLE(permission)、503 DB_BUSY；403使整个连接标stale/认证失效、保草稿/最后快照、停止重试旧token；显式重新连接读取新凭证并取快照后才恢复，不冒充单对象权限；无PASS/空成功；C1不换C2；65KiB文件复制完整，失败无成功消息；osc52配置拒绝、无OSC/DCS输出；忽略失效拒绝导出且不产生未跟踪文件；草稿锚点不变 |
| AC-012-04 / TEST-012-04 | unit | 八类块、100 次追加、10 次 resize，焦点为第三块；正文含中文/组合字符；选中块被过滤或其对象回收 | 对展开/折叠/全屏/返回/搜索状态作纯动作序列；重复同一提交事件 | block_id 与 grapheme_offset 不因前插/重复事件漂移；offset 超过新正文时钳到最后字素边界并提示；被过滤保留隐藏锚点，取消过滤可返回；不可恢复块明确提示并选 seq 最近的前驱（无前驱选后继），不跳到无关 Run |

### REQ-013 · 三级时间线（5 条）

行为（D3）：Project 看当前项目跨 Run 的规格、候选、Issue、决定、轮次结果和 Run 生命周期；Iteration 看选中 Run/iteration 的工程事实（含 Task、候选、测试、证据）；Agent 看明确 role/epoch/attempt 的公开消息、工具与交接。项目级事实没有 iteration 时不塞进某个轮次。按 seq 排序，时间戳仅展示。每项可沿类型化引用定位需求、Issue、Decision、commit、Evidence；保留过滤条件和返回锚点。

失败：非法过滤/cursor 明确拒绝；合法无匹配显示空；未知事件有诊断块；broken/orphan/stale 引用不能伪装成可用证据。doc_refs：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`contracts/openapi.json`；M3 依赖 REQ-032/033/034。提案 D1、D3、D4。

#### 分类表与完整集合（D3）

分类由kind白名单决定，**带attempt_id不意味着Agent层**。下表是完整分类规则，P=Project、I=Iteration、A=Agent；I还要求run/iteration归属，A通常要求明确role/epoch/attempt；以下明确的handoff/session与已接受用户消息允许尚无Attempt，但只匹配其真实role/epoch，不能因此进入任意Attempt过滤。项目级null身份不猜补。多层事件只在所列层出现。具体名字从当前生产者、CR-026及显式标注的CR-024后续main提取；读取历史/未来版本的新kind若未列入则走最后一行诊断，不靠前缀自动解释为成功。M4-T03实现集中kind注册表及查询时分类，M4-T06消费，M4-T09只负责归属和public事实、不存class。注册机制必须约束写入本身：T03把所有生产 `NewEvent.kind` 及通向唯一 `writer::insert` 的构造参数改为封闭的 `ProducedEventKind` 类型；公开协议/持久读取的Event.kind仍为开放字符串，旧未知数据继续可读。类型不提供任意String的生产构造器；每个有限动态分支（cancel信号/PassEnd、effect目标状态、runner的prefix/name、adapter模式）必须先匹配为枚举variant，未知原生种类写已注册的adapter.unknown_event并把原名作为去敏payload。历史/损坏数据的Unknown只存在读取类型；F13等未知写入fixture通过debug专有的测试入口调用同一writer（与§5.1故障seam同编译边界，release不编译），生产调用图不得引用该helper，源码守卫同时核对这一点；不能为夹具给正常生产类型增加任意字符串旁路。

分类和 `ProducedEventKind::ALL/as_str` 由同一枚举定义生成，分类match无通配兜底；新增variant未分类必须编译失败。负例采用命名含 `test_013_04` 的 `compile_fail,E0004` doctest，调用生产注册/分类宏并额外注入一个没有分类臂的variant，与正常编译例共用生产定义，不另写镜像枚举或只因拼写错误变红；unit既有cargo --doc收取其原始结果，不在单元测试里嵌套cargo构建。`test_013_04_kind_registry`枚举该生产类型的全部variant，逐项核对唯一名字/显式层，并调用每种动态生产分支验证其返回variant；另用源码/AST守卫枚举整个生产crate的NewEvent构造与唯一SQL插入入口，禁止新的原始字符串写入、format!绕行或另开INSERT路径，无法分析的宏/构造需显式失败并列source位置，不能跳过。负例必须证明新增未分类variant、把cancel/effect分支改回字符串、增加旁路INSERT三种变异分别被编译或守卫拒绝；不能仅比较两份手工数组。应用时从该生产注册定义生成完整kind清单供审查对照下表，不以grep带点字面量代表全覆盖。当前补全项依据control.rs的Signal/PassEnd、runner.rs的EffectState与host/runner.rs实际调用；除报告点名五项cancel结果，还包含Signal::Native产生的cancel.native_cancel。host/runner.rs::settle的prefix仅lease或runner，后缀来自inspected/native_cancel、StopStep四值、cgroup五值、outside四值，完整30种列在下表；hold.rs中的integrity_violation虽不含点也登记。这次沿生产调用链补全，T03仍须用类型/守卫机械保证后续不再遗漏。新增生产kind须同时有类型与分类，未来/损坏数据才走Unknown。

| kind（同格逗号分隔为独立名字） | 层 | 规则 |
|---|---|---|
| spec.changed, spec.index_failed, spec.generated_drift, spec.drafting, spec_review.opened, spec_review.reported, run.created, run.started, run.result, run.incomplete, run.cancel_requested, run.cancelled, run.pause_requested, run.paused, run.resumed, run.waiting, run.blocked, run.blocker_claimed, run.blocker_released, run.control, run.baseline, run.mode_change_pending, run.mode_changed | P | 规格/Run里程碑；不因在轮内发生而塞进I |
| candidate.frozen, candidate.accepted, candidate.rejected, candidate.stale, candidate.reconcile_failed, candidate.reconcile_recovered, issue.opened, issue.reopened, issue.resolved, issue.consumed_by_rework, issue.observation, decision.applied, decision.rejected, iteration.completed, budget.risk, no_progress, milestone_ready, release_request | P,I | 有Run时进入该轮；无Run只P |
| candidate_ready, task.accepted, task.blocked, task.cancelled, task.implemented, task.protocol_correction, task.retry_scheduled, task.reverify, task.rework, task.superseded, task.unblocked, task.migrated, task.replaced, attempt.created, attempt.start_refused, attempt.artifacts, attempt.outputs, attempt.workspace, attempt.workspace_start, attempt.tracking, attempt.moved, attempt.inactive, attempt.inactive_cleared, attempt.provider_reset, schedule.round, plan.imported, plan.write_violation, readiness.passed, context.packed, evidence.recorded, effect.started, effect.observed, effect.committed, repair.local, repair.protocol, repair.refusal_cleared, repair.reverify | I | 派发/执行/证据工程事实，attempt.created不进A；candidate_ready来自results.rs::complete进入验证轮次，必须是I工程事实，不是UI_UNKNOWN_EVENT/诊断，不与verification.candidate_ready_for_join合并 |
| verification.attempt_started, verification.candidate_ready_for_join, verification.check_refused, verification.confirmations_dropped, verification.failures, verification.failures_abandoned, verification.failures_attached, verification.failures_consumed, verification.handoff_reissued, verification.joined, verification.output_invalid, verification.report, verification.result, verification.round_closed, verification.round_opened, verification.tree_checked, report.rejected, report.stale, task_result.rejected | I | 验证生命周期，不把report自述当accepted |
| config.invalid | P | 配置拒绝/诊断，不伪称已生效 |
| compat.suite_deferred, compat.suite_opened, compat.suite_started, compat.suite_ended, compat.suite_completed, compat.manifest_unreadable | P；有Run另I | 兼容检查生命周期/清单读取诊断，suite完成不等于产品验收 |
| session.native | A（有明确role/epoch）；否则P诊断 | 原生会话绑定，按真实session/attempt查询，无身份不猜 |
| team.configured, team.binding_applied, team.binding_blocked, team.binding_change_pending, team.binding_change_withdrawn, team.binding_excluded, team.binding_fallback, team.binding_pending, team.binding_refused, team.binding_unblocked, team.exclusion_skipped, team.session_bound | P | 显式配置/换绑事实 |
| input.accepted, input.queued, input.moved, input.delivered, message.applied, message.invalid, message.rejected, question.asked, question.answer_accepted, question.answered, need_decision.appended, request.deferred, request.rejected, blocked.withdrawn, pause.withdrawn, outbox.dead_letter, outbox.delivered_late | I | 命令/投递事实，用户正文由public.message单列A，不重复出两条消息 |
| handoff.prepared, handoff.delivered, handoff.superseded, handoff.undelivered, session.start（若持久事件） | I,A | A精确成员规则见下文H13；session.start目前是outbox notice，不凭名字伪造一条events事实 |
| public.message, public.tool_started, public.tool_completed, public.segment_sealed, public.activity, public.usage, public.gap（M4新增） | A | §2.2公开规范化输出及input_id用户原文；未投递用户块按真实target role/epoch匹配，attempt/session过滤为空；segment更新同block不重复正文 |
| adapter.agent_start, adapter.blocked, adapter.degraded, adapter.diagnostic, adapter.error, adapter.item, adapter.prompt_refused, adapter.result, adapter.server_request, adapter.turn, adapter.turn_failed, adapter.ui_data, adapter.ui_request, adapter.unknown_event（若生产落库） | A | 只显示去敏后的诊断/公开白名单；已映射的native item不二次渲染成消息；原始帧不是UI事实 |
| supervisor.allocated, supervisor.available, supervisor.blocked, supervisor.bridge_ignored（CR-024后续main）, supervisor.closed, supervisor.deferred, supervisor.horizon, supervisor.output, supervisor.output_discarded, supervisor.output_disposed, supervisor.output_failed, supervisor.preempt, supervisor.probe, supervisor.rearmed, supervisor.retrigger, supervisor.step_failed, supervisor.superseded, supervisor.turn_failed, supervisor.turn_limit, supervisor.unavailable, supervisor.withdrawn, supervisor.woken | I | 调度监督事实，不假称正在推理 |
| reconcile.attempt, reconcile.completed, reconcile.diagnostic, reconcile.run, reconcile.started, runner.diagnostic, runner.outside, runner.received, runner.refused, runner.unreaped, cancel.diagnostic, cancel.workspace, cancel.native_cancel, cancel.sigterm, cancel.sigkill, cancel.reaped, cancel.lost, cancel.unreaped, integrity_violation, bridge.rejected, registry.turn_auth_ignored | I | 恢复诊断；无Run的host诊断改入P |
| lease.inspected, lease.native_cancel, lease.sigterm, lease.sigkill, lease.reaped, lease.lost_track, lease.cgroup_sigterm, lease.cgroup_sigterm_incomplete, lease.cgroup_kill, lease.cgroup_reaped, lease.cgroup_unreadable, lease.outside_sigterm, lease.outside_sigkill, lease.outside_reaped, lease.outside_unreadable | I | host/runner.rs::settle的lease_expired分支；同样为工程回收事实，不因Attempt身份进入A |
| runner.inspected, runner.native_cancel, runner.sigterm, runner.sigkill, runner.reaped, runner.lost_track, runner.cgroup_sigterm, runner.cgroup_sigterm_incomplete, runner.cgroup_kill, runner.cgroup_reaped, runner.cgroup_unreadable, runner.outside_sigterm, runner.outside_sigkill, runner.outside_reaped, runner.outside_unreadable | I | 同一settle其余cause分支；后缀由reconcile.rs::step_names、cgroup.rs::stop、processes.rs::stop的有限返回集产生，不是接受任意runner前缀 |
| 未列kind（包括未来扩展） | P；有Run另I；有明确Attempt另A | 恰一UI_UNKNOWN_EVENT块/seq，保留身份、经去敏的摘要和refs；不推导任务成功 |

**交接成员 H13（D3）**：`handoff.prepared` 的职位取 `payload.role`；发送身份为 `(role, payload.from.role_epoch, payload.from.session_id)`，接收身份为 `(role, payload.to.role_epoch, payload.to.session_id)`，from/to=null 不构成成员，缺 session 只能匹配 role/epoch，不能匹配 session。顶层 role_epoch 是目标 epoch，不覆盖 from 的旧 epoch。`handoff.delivered/undelivered/superseded` 与 `session.start` 按各自 `payload.role/role_epoch` 匹配；session_id 仅在明确提供时匹配，缺 attempt_id 不按 session 推断某一 Attempt。session.start 当前只为 outbox notice：分类单元测试覆盖其字段，将来若新增持久事件亦遵守此规则，不在产品测试注入一个不存在的生产事件。

H13 是独立真实换绑夹具，下面的期望子集只列两条 handoff 事件（实际 seq 用 h1/h2 映射）：h1=prepared，role=developer，from={epoch1,session S1}，to={epoch2,无session}；h2=delivered，role=developer/epoch2/session S2。全量 I=[h1,h2]、A=[h1,h2]；A 的 developer/epoch1=[h1]、developer/epoch2=[h1,h2]、S1=[h1]、S2=[h2]、qa/epoch3/任意attempt=[]；P=[]。role/epoch/session组合必须在同一个发送或接收成员元组上同时匹配；不能把from.session与to.epoch拼接，例如developer/epoch2/S1为空。中间真实team/session事件保留原seq，另行核对注册表的分类，H13断言只用独立kind过滤助手对完整返回结果取handoff子集，不删原始证据、不假称两条事件连续。AC-013-02/04 同时核对 F13 与 H13。

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
| AC-013-02 / TEST-013-02 | integration | F13含重复/逆序timestamp；limit2、H12，之后追加13；所有合法过滤值与明确空集 | 按本节F13与H13完整期望集合翻页、重复cursor、改过滤/H、重建generation、试跨Run iteration/attempt | 严格seq序、每个并集精确等于表，无13混入；重复cursor同页；新/v1/timeline非法为400 QUERY_INVALID，旧Run路由非法仍404 BAD_REQUEST；两页路由generation过期410且顶层snapshot_watermark；SSE矛盾409为真实HTTP；410仅静态核对OpenAPI顶层形状、不伪造产品过期；H13各handoff完整成员集亦精确相同；合法空items=[]/next_cursor=null |
| AC-013-03 / TEST-013-03 | integration | 四条独立链：TEST缺evidence；当前修订中PASS绑定旧候选而非当前候选；批准新修订后旧修订验收节点；当前完整合法链 | 经refs往返详情，在新修订前后各查一次 | 标签唯一：broken（缺evidence）、orphan（当前修订的旧候选PASS）、stale（旧修订全部验收节点）、current（合法链）；先做新修订失效再判旧PASS，stale不另写orphan/历史；身份/原seq保留、非current不计accepted；不生成证据/执行检查 |
| AC-013-04 / TEST-013-04 | unit | F13 的持久事件、重复 4/9、未知 12；两种交付分批及同一 H 快照 | 比较全量折叠、快照+增量、断线补历史三种规范化投影；切过滤再切回；枚举生成的ProducedEventKind::ALL并调用所有动态生产分支；运行生产构造/唯一INSERT的源码守卫与三种旁路负例（未分类variant用test_013_04的compile_fail,E0004 doctest）；另输入results生产的candidate_ready并尝试错误诊断分类；另核H13/notice身份 | 块序列/refs/身份/诊断一致；candidate_ready在I中保留真实task/candidate/attempt/round引用，P/A不新增该项，UI_UNKNOWN_EVENT/诊断子集为空；将其映射到诊断的变异必须被断言拒绝，不能以“有分类”放行。所有生产kind均有显式分类，含cancel.native_cancel/sigterm/sigkill/reaped/lost/unreaped、effect.committed、完整lease/runner收尾30种、integrity_violation及原config/compat/session集合；动态分支输出与登记一致；未分类variant不能编译、原始字符串和旁路INSERT被守卫拒绝，任一绕过负例未被检出则测试失败；H13 role/epoch/session集合与表完全一致；12 恰一诊断；切过滤不改变事件真值；无以 timestamp 排序、以 Agent 名合并 role 或从模型摘要创建状态的行为 |
| AC-013-05 / TEST-013-05 | integration | 两Run事件交错；R1关轮前开放I1、同Task跨两轮重试，含run_id=null项目事件；分别准备COMPLETED、FAILED、CANCELLED、PAUSED、INCOMPLETE五个独立Run变体及真实既有iteration.completed旧库 | 经产品单writer/CAS与真实控制或预算路径逐事务追加/关轮、kill/reopen并回填；完成变体观察run.result(state=COMPLETED)，失败变体观察run.result(state=FAILED)，取消变体先cancel_requested再cancelled及result，暂停变体观察paused及result；INCOMPLETE前后取H1，显式ResumeRun生效后取H2，重查相同尾事件e与旧H1页；核对milestone_ready/release_request | 每事件按§2.1状态表与Run+close区间归属；close属被关轮、开放/关闭key一致，跨轮Task不被tasks.iteration_id吞并。COMPLETED/FAILED/CANCELLED及INCOMPLETE在对应H不展示N+1，未关闭尾段iteration=null/只P；cancel_requested、PAUSED（含run.result.state=PAUSED）仍保留原开放轮，BLOCKED亦不因result而终结；milestone_ready/release_request不改变终结判断。INCOMPLETE恢复H2重开原N+1，e由H1的null变为H2的该轮；H1重查逐字段/成员不变，无新close/预算计数。项目null事件不从payload补Run；重启/回填/在线集合相同；缺损边界index_ready=false且诊断，不伪造完整 |

### REQ-014 · 真实进度状态栏（5 条）

行为与失败按决定提案 §3；状态栏可配置但不能隐藏真实进度/阻塞。标准和精简在窄屏优先保留四组事实，可在多行状态区换行；详细字段从状态页可达。doc_refs：`docs/03-ux.md`、`docs/01-prd.md`、`docs/05-data-api.md`、`docs/adr/ADR-029.md`、CR-025；M3 依赖 REQ-032/033/034/040。提案 D2、D8、D11。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-014-01 / TEST-014-01 | tui-pty | r1有RQ1(2AC)、RQ2(1AC)、RQ3(1AC)；C1范围仅RQ1/2且ACCEPTED，RQ3不在该Run任何Task中；C1接受后新增两个仅关联RQ1/2的blocked Task，现有5个done/共7个Task；OPEN2/REOPENED1/RESOLVED1；qa/pi配置模型requested-X、实际resolved-Y，未知费用/上下文 | 打开status/Focus；新C2与budget.risk；批准r2新增RQ4并删除RQ3（fixture模拟用户批准，不由UI擅删） | 先2/3 accepted、blocked=2/open=2/reopened=1；分子不取Task/AC数，blocked按Task唯一ID且Run级reason单列不双计；Focus显示resolved-Y，requested-X仅配置详情；费用与上下文值明确显示“未知（qa/pi，原因）”，字段不得显示0、$0或0%来代替未知；C2为0/3；r2为0/3、revision2/added1/removed1；旧计数仅历史；预算原因/上限可查 |
| AC-014-02 / TEST-014-02 | unit | 无spec、零AC、某AC FAIL、FROZEN/STALE/INVALID、旧spec PASS、spec_review PASS、TaskResult COMPLETED；另spec共3需求：同一ACCEPTED候选范围内一个全PASS、一个合规DEGRADED，另一个未纳入该Run任务范围的需求未满足 | 与host共用聚合函数，重复验收事实 | 不满足者分子0，无spec为0/0且未完成；3需求例为accepted=1/3、degraded=1，DEGRADED不进分子且在分母；只有当前spec+候选ACCEPTED且该需求所有AC PASS才加1；重复不增计 |
| AC-014-03 / TEST-014-03 | tui-pty | 80/120/180×32；三档，custom=[budget,candidate]；可识别H1/H2响应与独立Timeline seq | 切档重开，Focus/离开，候选更新和Issue重开并发 | minimal仅mode/阶段任务/n-m/blocked-issues四组；standard依次四组+git/candidate/elapsed/budget；custom四组+budget/candidate严格按序；窄屏可换行、超出可选项进状态页；保存后重开保持差异；Focus上下文只用该身份；状态栏/进度同帧只来自一个完整getRun响应H（探针可查），Timeline另显last_applied_seq不强称同H；非法数组拒绝保旧档 |
| AC-014-04 / TEST-014-04 | integration | 明确的工具开始/结束及已知最后活动时间t1/t2、服务等待、压缩、排队、空闲/进程 alive，另含与工具事实冲突的模型摘要“任务已完成”，及 context used=25/limit=100、used 缺失、limit 缺失三例；两个 Agent 只有一个可计美元 | 查询 progress/ui 活动与用量，并在真实PTY观察Focus/状态详情，再换 role_epoch | 状态分别为工具运行/等待服务/压缩/排队/空闲；最后活动时间由对应已提交事实从t1更新t2并可见（UTC时间或可核对相对时间），未知明确显示未知；摘要标“分析”，与工具运行并列而不覆盖它/不变更确定状态或accepted；仅 alive 为活动未知，Commander 无 turn 为事件监测中；25/100=25%，缺任一数值为 null；成本标已知部分并列缺项身份，不能当总额；换绑不沿用旧 epoch 的用量 |
| AC-014-05 / TEST-014-05 | adapter-live | 当前冻结候选host与三家已认证的真实native会话，各给定只读小文件与唯一公共回复标记；记录compat版本/session/resolved model | 各调用原生工具读该文件并给公共消息，结束前后读持久块/activity；detach/reconnect，再对一会话取消 | codex/claude/pi均有可核对原生session，公开回复/工具开始结束经真实producer落库，持久身份/seq与原始公开帧对应；已有在线usage/compact/wait字段一致、未提供则unknown而非猜测；detach不取消，显式取消保留前缀及终结事实；缺认证沿adapter-live既有约定为阶段BLOCKED、逐测试SKIPPED且detail注明verdict=BLOCKED、退出3，不记PASS；live-sessions.json与原始证据来自adapter-live，不以fixture替代 |

### REQ-015 · 输入与终端兼容（5 条）

行为：Unicode grapheme/显示宽度、多行编辑、括号粘贴、不依赖增强键盘协议；resize/网络分片/异常退出都不丢原草稿或误发命令。teardown 覆盖 raw、alternate screen、光标、bracketed paste 和新增 mouse reporting；正常退出、SIGTERM、SIGHUP、panic/unwind/初始化失败均恢复可恢复的终端状态。SIGKILL/断电不能执行 teardown，不能宣称可恢复（恢复 host 另属 REQ-041）。失败时沿用 REQ-004 的退出码，不新增用户命令。

doc_refs：`docs/03-ux.md`、`docs/01-prd.md`、`docs/09-testing.md`、`docs/adr/ADR-014.md`、`docs/adr/ADR-031.md`；M3 依赖 REQ-007/017。提案 D9、D10。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-015-01 / TEST-015-01 | tui-pty | §4提交的完整corpus、四路径实测表与版本manifest，外层PTY及pane；80/120/180列；中文/标点、Ambiguous/框线、普通emoji、成对VS15/VS16、ZWJ家庭、肤色/旗帜/组合符全部语料；含✈️、👩‍💻、👨‍👩‍👧‍👦、👍🏻、👍🏽；两行CRLF粘贴；缺表簇与路径未知分歧负例 | 在本检查机器用已提交工具重生成四路径全表并比较版本/规范内容；再每簇每路径捕获HoH实际绘制流/cells/cursor，测非零起点/末列/后续标尺、移动删除/resize；路径已确认和未知交集、缺表回退分别测试；paste再Enter；各路径debug panic；在会话内shell取stty/modes/哨兵，最后退出包装器补查外层 | 重生成版本/表与已提交表逐项相等，版本漂移/缺项/差异即FAIL且不改源表。每条语料每条路径（含内外层）无列漂移、无半字符、HoH光标=终端报告，标尺与实测跨度对齐；不能漏测一致但不同于unicode-width的肤色簇。中文、Ambiguous标点/框线、🙂✅、✈️❤️⚠️▶️、🇺🇸及已测一致的肤色簇均为原字素，跨度仅取生成表，不在AC手写列数。已知安全路径用该表；未知路径分歧或缺表簇只用§4唯一ASCII回退，其最终光标同样相等；复制/提交原文不变。不得用unicode-width/wcwidth/CellWidth自证或按字符类别回退；Backspace删完整原字素；paste前命令0、Enter后恰1且LF两行；panic101；各shell/pane在包装器存活时stty与进入前相同，本实例模式配对关闭、哨兵完整；外层无泄漏仅附加，四路径任缺FAIL非零 |
| AC-015-02 / TEST-015-02 | tui-pty | 无增强协议四路径，草稿甲、带可观测身份的受控后台Attempt；分别无层编辑器/列表/临时层；输入历史有“旧稿乙”；独立原始SGR应用PTY及tmux占用鼠标变体 | 键盘：Ctrl+J输入乙、Ctrl+J输入丙后Enter；各焦点Esc后50/100/500ms再Enter、单write \x1b\r、两write间隔10/20ms、连续Enter/Alt+Enter；Esc→编辑→Enter、Esc→粘贴→Enter、Esc→Ctrl+R接受历史→另一次Enter；列表Enter、编辑器?、非空Ctrl+C；空编辑器Ctrl+C打开后分别取消菜单、选脱离/暂停/取消；可辨认Shift+Enter。鼠标：向应用PTY注入坐标300的SGR紧接Enter/中文键、残缺SGR/X10；tmux占用时仍全跑键盘 | 首例甲\n乙\n丙恰1 SendInput（正文中的换行按LF）；所有慢/快/同批Esc-Enter无SendInput、原稿不变/零新LF；有层Esc只退一层、无层也解除armed，所有Esc子例PauseRun=CancelRun=0、Run状态及原Attempt身份/存活不变。连续Enter/Alt+Enter不绕guard；编辑/成功粘贴/接受不同历史才重武装，历史确认Enter本身发送0，随后显式Enter才恰1；只有Ctrl+J换行，Shift+Enter无动作。非空编辑器Ctrl+C：草稿变空，SendInput=PauseRun=CancelRun=0。空编辑器Ctrl+C：显示脱离/暂停/取消三项，未选和Esc关闭时控制命令0；选脱离仅客户端退出/控制0，选暂停恰1 PauseRun，选取消恰1 CancelRun且不附带其他命令。列表Enter仅展开，?为文字；SGR后键不吞/X10跳过/畸形有界；tmux拦截鼠标只豁免被拦点击、不豁免键盘/退出配对 |
| AC-015-03 / TEST-015-03 | integration | PTY按UTF-8/CRLF/起止标记边界分块（不声称内核保留单字节write）；草稿保留；合法最终1MiB与+1B；64MiB无结束paste；三家adapter编码/回显fixture | 合法/超限paste，结束后普通键；未结束时SIGTERM；从HTTP受理经真实投递链对每adapter测试§4的256KiB/1MiB/最坏转义与不可读文件，另测超8MiB请求，以及合法非SendInput命令用JSON空白补到3MiB；对已202的大正文在物化Attempt可读根文件前制造低空间、再释放重试 | 完整paste不自动提交，超限原稿不变且UI_INPUT_TOO_LARGE；残缺保留≤1MiB+6B，丢尾到结束或信号，RSS增≤16MiB（RSS子例由本TEST在§5.1串行release阶段运行）；后续键正常、无斜杠执行；每adapter inline≤256KiB且编码≤768KiB、否则短文件引用≤16KiB，文件位于Attempt可读根且底座fixture可读取、字节/hash精确；Workspace/Focus展开的用户块等于已接受原文（不是短引用）、同input_id恰一块；每帧≤1MiB不触发PROTOCOL_FRAME_TOO_LARGE；超8MiB以及3MiB非SendInput均400 BAD_REQUEST、无202/commands或events写入、草稿保留；已202输入投递文件低空间时保持QUEUED并记STORAGE_LOW_SPACE及available/required/min_free，未DELIVERED/APPLIED；释放后原input_id仅投递/用户块各一次，不可读/摘要错明确REJECTED；fixture标product，非live；环境不符RSS子例SKIPPED/退出78、整AC不通过 |
| AC-015-04 / TEST-015-04 | tui-pty | 独立后台Run与外层shell；三类起始态：干净主屏、主屏已用DECSC在另一行保存旧光标、前进程遗留mouse/paste/alternate；前两类预写多行唯一标记并把当前光标置于第r行且r不同于旧保存行，记录基线stty/单元格/光标；mouse=true、debug故障点 | 干净/旧DECSC各做正常启动脱离往返，在teardown完成且任何诊断/shell新提示输出前取外层buffer；残留态单独启动；再各测SIGINT/SIGTERM/SIGHUP、panic、alt后初始化失败、部分输出失败、信号与I/O同时失败；退出后读哨兵 | 所有启动复位仅关闭1000/1002/1003/1006/2004，不含1049l/1047l；本实例完整1049h后仅teardown配对一次1049l；干净与旧DECSC往返原有行的文本/位置及光标行r（并核列）与基线一致；残留alt只检查mouse/paste复位，不承诺旧屏/旧termios恢复。可写PTY退出stty等于本次进入前、实际开启模式逐项关闭/光标显示；正常0、信号130/143/129、panic101、I/O74、并发信号码优先；恢复一次且不吞哨兵，后台无Pause/Cancel、新客户端可查 |
| AC-015-05 / TEST-015-05 | integration | §5.1参考VM、release串行，百万库；80×24与180×40各≥1000样本，50ms增量合并负载 | 并发提交事件并编辑/滚动/resize，记录输入→外层屏幕、commit→seq可见；插入tool/decision/acceptance边界 | 各尺寸输入p95≤100ms、事件p95≤500ms；每个合并窗口≤50ms或16KiB先到，关键事件计数完整；无丢键/重复提交，非renderer独立计时；环境不符SKIPPED/退出78不通过，真实超限FAIL保留样本 |

兼容矩阵 D10：正式 M4 tui-pty 在本 VM Ubuntu26.04.1 x86_64 四路径每次完整执行，TERM=xterm-256color/tmux内tmux-256color，UTF-8 locale，逐路径终端与版本按§4，宽度表现场重生成及版本差异同样直接FAIL，不能作为环境SKIPPED。缺ssh/tmux/locale直接FAIL非零。T07提案增加仅用于正式矩阵测试注册的非默认 `tui-acceptance` feature（不改变产品行为）；`tui-pty.sh` 在 `HOH_CHECK_ID=tui-pty` 下启用该feature运行 `tui_pty` target，test_015_01/test_015_02矩阵入口先校验检查身份与平台。普通CI的cargo test --workspace不启用该feature，调用无TEST id的功能/恢复冒烟助手；不把条件不满足的formal test return ok，也不生成同ID SKIPPED占位。矩阵包装层仅注册/调用共用助手，默认CI仍真实覆盖解码、绘制与恢复逻辑；正式脚本核对所需TEST实际执行数与四路径manifest，缺项即失败，不能因过滤掉矩阵绿灯。debug功能与正式证据边界明确，CI不冒充26.04性能或正式矩阵验收。

非默认注册也必须进入CI静态编译：T07在现有流程追加启用tui-acceptance的hoh-app全target Clippy（保留--locked与-D warnings），只编译/lint、不执行带TEST id的矩阵、不产出正式test-results；具体命令随T07实际运行后记入CI/文档。本次未执行该未来feature的命令，不声称它已存在。普通无feature构建与正式矩阵执行检查仍都保留（CR28D-207）。

Ubuntu24.04四路径仍由docs/23的M6-T03/M7-T04承担；不把本次M4结果声称为24.04证据。两任务done_when追加：M6-T03准备隔离24.04实例与版本记录；M7-T04在最终候选的**一次正式tui-pty检查**收集26.04/24.04两套四路径，冻结environment预先列两平台描述，向24.04工作目录分发同commit/tree/spec/test-plan并在运行前后校验摘要。原始PTY、会话内stty、逐模式配对与光标断言、每路径退出码按平台子目录收取到该检查evidence目录；同一TEST的两平台子例均PASS才形成总PASS，不引用M4旧候选或把另一次检查的PASS拼入。T07交付平台manifest格式，M7-T04的最终runner对两平台×四路径逐项缺失/不符非零，独立QA核对，Reviewer核对版本与来源；无需让release_gate猜平台维度，缺任一平台使其输入检查失败。此收取方案属D10待审提案，应用清单记入M7-T04，不在本轮修改门禁。所有sshd端口/tmux会话独立且结束清理。

### REQ-016 · 无障碍与键位（4 条）

行为：重要操作全部有键盘路径；状态有文字与形状/ASCII 标记，不只靠颜色；焦点可见；NO_COLOR、无鼠标与 ASCII 图标同时可用。自定义键位按 scope 校验并可恢复默认。失败：冲突列出两个 action/scope/key，保存整体拒绝；非法配置不破坏现有 Run；没有终端能力不显示虚假的启用状态。

doc_refs：`docs/03-ux.md`、`docs/01-prd.md`、`docs/09-testing.md`、`config/hoh.example.toml`；M3 依赖 REQ-007/017/047。提案 D8、D9、D10。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-016-01 / TEST-016-01 | tui-pty | NO_COLOR=1、ui.mouse=false、ui.icons=ascii，80×32；list.copy=c与冲突配置；历史“旧稿乙”、当前草稿甲，当前Run A与可选INCOMPLETE Run B | 键盘遍历四视图/角色会话/三抽屉/块/模式/设置/提交/控制；Ctrl+R检索并Enter接受旧稿，另一次检索Esc取消；/resume打开选择器、Esc退出、重开并选B；保存冲突后恢复默认 | 每动作有逐项界面/命令记录；Ctrl+R确认只将旧稿乙放编辑器、SendInput=0，下一显式Enter才可提交；取消检索恢复原草稿且不提交。/resume未选前A与草稿不变、选项列B、无POST/控制；Esc只关选择器；选择B才附着其快照/SSE，B仍INCOMPLETE且ResumeRun/PauseRun/CancelRun/Task/Attempt新增均0。无颜色SGR/鼠标启用，状态有文字和ASCII；c复制、冲突整体拒绝，恢复后y复制/c未绑定；显式后台控制每次仅对应一条命令 |
| AC-016-02 / TEST-016-02 | unit | keybinding 表：同 list 同 key 两动作；global 与 editor 重叠；editor/list 同 key；未知 key/action/scope；覆盖 Ctrl+C/Esc/Enter/Ctrl+J；将面板重绑为未占用 Ctrl+O，以及使面板无有效入口的配置 | 校验各表，分别试保存；读取错误 details | 前两种冲突与未知/保留覆盖/无面板入口均拒绝，给出准确 action/scope/key；互斥 editor/list 同 key 及合法 Ctrl+O 重绑允许；失败后有效映射逐字段不变；所有默认动作仍可从命令面板到达 |
| AC-016-03 / TEST-016-03 | tui-pty | editor/list/dialog 各 scope，草稿含 `? / f y n`；重绑 list.search 为 Ctrl+F，面板改为 Ctrl+O | editor输入唯一前缀/he后Tab，再试歧义前缀/；普通editor/list/dialog各Tab移焦，Ctrl+O打开面板，Esc返回；使用重绑键，保存并重开客户端，在面板恢复默认 | 唯一前缀/he经Tab补为/help、焦点仍在editor，Enter前无提交/SendInput；歧义前缀/只移焦、原文字不变、无命令；非补全Tab依scope移焦且焦点可见。编辑器文字不被列表/帮助热键吞掉；焦点可见且 Esc 回到原草稿/锚点；重开后绑定仍有效；恢复后 Ctrl+P 再次打开面板；只清键覆盖，team/budget/Prompt/mode_cycle、配置注释与其他键均不变；ui-only保存不产生ConfigureTeam/新team_revision，CAS冲突拒绝且原文件不变；mode.cycle重绑后顺序仍遵守ui.mode_cycle |
| AC-016-04 / TEST-016-04 | unit | 同一投影包含 PASS/FAIL/BLOCKED/UNKNOWN/STALE/DEGRADED/REOPENED、选中和未选中项；80/120/180 列 | 以彩色与 NO_COLOR+ASCII 两套样式渲染 TestBackend | 每种状态在去样式后的文字中仍可区分，选中项有 ASCII 焦点标记；禁色不含前景/背景色，保留必要非颜色属性；无超过显示宽度、无必需动作只有图标无标签 |

### REQ-064 · 可观测性与容量（5 条）

行为：结构化索引+对象、seek 分页和虚拟列表、诊断日志轮换、所有被引用证据保护、慢读者背压和空间错误可诊断；数值采用 §1/§5 待答提案。失败：不接受半写对象为有效证据；DB 满/忙不承诺成功；cursor 过期明确 resync；GC 无法确定安全时保留。百万历史数据不能在启动时全量反序列化。

doc_refs：`docs/01-prd.md`、`docs/04-architecture.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/14-resilience.md`、`contracts/sql/001_initial.sql`；M3 依赖 REQ-032/033/034。提案 D1、D4–D6。

| AC / TEST | check_id | 输入（Given） | 操作（When） | 可判定输出（Then） |
|---|---|---|---|---|
| AC-064-01 / TEST-064-01 | integration | §5.1参考环境、同次由已记录pre-003提交构建的旧release writer生成002百万事件/对象库、候选release与最终TUI、100Run×10轮、受限ENOSPC；另debug integration小旧库/迁移故障seam/旧binary | 先debug运行迁移失败回滚/旧binary拒读；再在同一backfill阶段以两克隆串行正常升级及回填中途kill/reopen、从每个候选首次open计时；按Run/iteration/attempt各查100页，limit100/500/非法0/501；Range首尾/缺文件/416；同夹具磁盘失败/慢客户端/轮换/引用保护/虚拟列表子例 | 新/v1/timeline非法400 QUERY_INVALID、旧Run timeline非法404 BAD_REQUEST，416仅ARTIFACT_RANGE_INVALID与Content-Range组合；分页≤limit、seq完整、H固定，查询p95≤500ms；EXPLAIN用指定索引、每页描述反序列化≤500/正文≤256KiB，无全库加载；旧源码/tree/锁文件/产物hash与002 schema可核对，两release构建均计入release_build；造库≤600s、backfill阶段含两克隆/停机重启总计≤600s，各首次open单列；中断发生于0<indexed_through<H、断点续跑不重置计时，kill→reconcile和host-ready各≤30s、不等完整索引；期间Pause/Cancel仍202并持久、索引查询明示rebuilding。release升级原权威行/hash不变，新增控制/reconcile事件单列，两克隆同H最终索引相等；debug子例失败回滚/旧binary拒读；两阶段缺一不通过；子例包括日志≤8MiB×5、引用对象hash不变、描述≤1000/正文≤4MiB、磁盘无半登记、慢读不阻塞快读；环境不符SKIPPED/退出78，不PASS |
| AC-064-02 / TEST-064-02 | integration | §5.1同次百万库克隆，单块100MiB，首/中/尾唯一标记；release真实PTY | 开Workspace/Focus，末页/搜索/往返滚动60s，每种尺寸输入≥1000次，idle10s采RSS | 描述≤1000/正文≤4MiB/在途页≤2，初始不回放百万正文；所有标记可达/锚点稳定；输入p95≤100ms；idle host+TUI总RSS≤150MiB且分别记录/各≤150MiB，活动各≤256MiB；不含Agent/浏览器，不排除TUI；环境不符SKIPPED/退出78，不通过 |
| AC-064-03 / TEST-064-03 | integration | 6×8MiB+1诊断字节；§5每一引用根各一对象、仅登记无引用对象、未登记临时文件；时钟47/48/49h | 轮换/GC；扫描后加引用、打开pin并发GC；引用解析故障；GC登记tombstone前后kill/reopen | 诊断最多5段/每段≤8MiB；全部引用/活动/pin对象hash不变；47h孤立保留、≥48h且复查无引用才删；collected对象404 ARTIFACT_NOT_FOUND reason=collected，未知ID/丢文件原因不同；解析失败保留诊断；重复GC幂等；events/evidence/acceptance删除0 |
| AC-064-04 / TEST-064-04 | recovery | 已有E/待写N、min_free32MiB、1MiB流段；32KiB SendInput需封口原文对象与未提交草稿；真实ENOSPC与注入EDQUOT；临时写/fsync/rename/登记故障 | 逐点失败或kill/reconcile；低空间但状态事务可提交时提交32KiB输入、分别Pause/Cancel；经hoh -p --resume以text/json/stream-json分别提交32KiB正文并观察低空间拒绝；状态事务本身满盘再试；释放后显式同幂等键重试 | E可读hash不变、N不留引用缺正文；新对象/新写Attempt为STORAGE_LOW_SPACE，含available/required/min_free；32KiB SendInput唯一HTTP503 code=STORAGE_LOW_SPACE、details.reason相同且字节数可核对，无202/接受行/input.accepted/public.message，UI保原草稿与目标、显示未接受；CLI三种输出均退出69，保留STORAGE_LOW_SPACE/details并明确输入未接受/存储空间不足，不自动重试、不新增run.result或改变Run状态；恢复后显式重试最多一次接受/用户块。低空间但状态事务可提交时Pause/Cancel仍202且持久；事务真失败503 DB_FULL无202，仍可脱离；原Task可重试、孤立48h策略不变；缺真实ENOSPC设施FAIL非零，EDQUOT明确注入；投递文件阶段子例由015-03覆盖 |
| AC-064-05 / TEST-064-05 | integration | §5.1参考环境release；快/慢SSE各1，100events/s共10000条8KiB公开正文，含tool/decision/acceptance；单流应用pending≤500项且序列化字节≤1MiB；另debug integration通过writer seam暂挂未提交事务X（独立小库） | 慢客户端停读30s直至TCP背压后恢复；独立断线后按last seq重连；查询矛盾after/Last-Event-ID；debug libtest在X提交前后分别观察，release测量不用该钩子 | 唯一背压行为为停止拉取/填pending（不因满队列主动丢弃或断开），批读取先按字节限界，探针含pending与当前序列化项、不含另报的内核/TCP缓冲；快流每条提交完整且p95≤500ms；慢流恢复/去重并集完整；冲突409 CONFLICT；X提交前对任一SSE不可见、提交后可见；202对应已提交事件缺失0；环境不符SKIPPED/退出78、AC不通过 |

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
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。交付§2.1/2.2生产接线、迁移003运行器/回填断点与public段登记；每个native模式的版本字段映射、真实runner fixture、重复/取消/恢复/旧epoch/去敏与有界队列；接入M3接受事务的用户原文块与input_id去重，背压不误报inactivity；索引不存class；不改M3投递或关轮算法。回归REQ-017/024/025/026/027/036/041/042/062和CR-026迁移002。生命周期与并发按docs/19双独立审查。 明确§2.1的COMPLETED/FAILED/CANCELLED、PAUSED与INCOMPLETE/ResumeRun快照边界；T09仅写身份，生产kind类型由后续T03统一接入。"
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
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。交付§2错误与Range/search/evidence/timeline路由、查询时集中kind分类及完整性测试、引用/GC/tombstone/低空间原因；交付§5.1唯一release测量基础设施/runner与ENOSPC设施，三个工件声明只随可产出驱动同提交/relock生效；先实测阶段耗时并留600s总余量，否则提交D4重规划；PID/start_time/pidfd与实际unit/Attempt cgroup归属、正常/失败/超时/取消/SIGKILL无残留证据齐全；debug故障子例与release计量分开，SSE按字节限界。新配置与example同提交，回归TEST-018-01、REQ-005/035/036/037/042/043/058/062。对象与测量进程生命周期并发按docs/19双独立审查。 NewEvent.kind类型化与唯一INSERT守卫覆盖全部生产者/动态分支，不能用手写清单自证；503 STORAGE_LOW_SPACE受理错误/草稿与QUEUED投递诊断按§5，T07再接文件包装。冻结环境填齐五阶段正整数时限，runner/监护者强制执行；service/scope两真实启动路径、每Attempt身份/资源核对及SIGKILL外部收尾通过后才交付。 第5稿：同次构建绑定pre-003提交的release旧writer，真实造002百万库；首次open起计及回填中kill/reconcile两克隆共享backfill时限。私有manager位于项目盘测试根，测试HOME/XDG/hoh-host@模板与安装完全隔离；外层scope和真实manager guardian的RuntimeMaxSec=3600，SIGKILL整个run_check组后30s外层树空的证据。全机锁贯穿测量与清理，run_check/cargo各入口遵守，锁竞争/噪声SKIPPED/78且不能PASS。补503 STORAGE_LOW_SPACE的CLI69及明确提示/不自动重试；REQ-004旧AC不改。"
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
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。交付快照增量、断线/DB_BUSY、布局/身份/小屏、Ctrl+P动作注册表与Task/Issue/Evidence抽屉；自有poll SGR鼠标解码与ADR-014修订。保留M3模式/输入/批准/历史入口，不直接写host状态造测试。回归REQ-001/005/007/017/018。 未预选同Agent多职位必须选择，Focus区分role/epoch/session与Attempt级handoff可见范围；保留/resume选择后仅附着。"
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
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。八类同一动作、Range装配/搜索边界、纯文本文件复制、引用错误和锚点；正文缓存与虚拟列表实现以小夹具回归，最终百万容量归T10；ui.clipboard/file加载与example同提交、exports守卫与占用诊断。不发OSC/DCS。TEST-012-01在既有integration的release阶段完整含PTY测量。 逐类断言鼠标单击选择/双击展开/滚轮滚动；新增场景后按§5.1重测全integration五阶段（含debug tui_pty），保存各段elapsed/limit和总余量，任何阶段超时FAIL。 第5稿：八类块逐项覆盖两处匹配的n到下一处/N回上一处、零命令。"
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
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。消费T09归属/公开活动与T03分类API，F13全过滤/引用，progress聚合、三档字段/水位探针；配置与example同提交；保留M3追踪标签、规格全量失效与旧acceptance的AC计数。回归TEST-018-01、REQ-032/033/053，不把adapter活动来源写成M2已提供。 013-04从受约束的生产类型穷举并证明旁路负例可检出；未知成本/上下文明确未知而非0。 第5稿：candidate_ready显式为I工程事实，分类为诊断的变异必失败；test_013_04 compile_fail doctest与生产定义共源。"
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
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。交付编辑器/有界paste、每adapter inline/frame预算与文件引用、启动复位/teardown/外层宽度四路径；只扩展M3投递的传输，不改变四态。§5.1收取TEST-015-03的debug功能与release RSS两个阶段；功能PTY的panic入口仍debug专用。准备CI的tmux/sshd/locale；回归REQ-004/017/024/025/026/043；生命周期双独立审查。同时交付§4终端版本/oracle与逐路径生成宽度表/唯一回退（Ambiguous原样）、会话内恢复和原屏往返；§5.1中test_015_03功能子例为debug libtest、RSS为release；REQ-015兼容矩阵的非默认测试注册/HOH_CHECK_ID门槛、无TEST id CI冒烟和平台manifest格式。 新增场景后按§5.1重测全integration五阶段（含debug tui_pty的busy/信号/布局/输入），记录elapsed/limit/总余量并强制阶段截止；CI编译/lint非默认tui-acceptance注册；015-02含Ctrl+C双Then、Esc零控制、粘贴/历史重新武装，015-03含3MiB非SendInput拒绝与已202输入低空间QUEUED。 第5稿CTO绑定：提交全语料、仓库测量器、四路径生成宽度表和版本manifest；全部显示跨度取实测值、禁手填及CellWidth覆盖；缺表/未知路径分歧只用一个ASCII回退，完整语料每路径验证光标/cells/末列；现场重生成与提交表不符（含版本漂移）FAIL，不改源表续跑。"
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
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。键盘遍历所有动作、NO_COLOR/ASCII、scope冲突、项目ui配置CAS原子写保注释、ui-only重载、恢复默认；mode.cycle可重绑但不改ui.mode_cycle。配置/example同提交、回归TEST-018-01与输入teardown。 016-01明确Ctrl+R只召回、/resume未选零控制及选后仅附着，不用动作清单的一般断言代替。 第5稿：Tab唯一/he补/help但不提交，歧义/只移焦并保原文。"
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
    "done_when": "指定AC在冻结候选的指定检查中有逐测试证据；独立审查无未解决critical/high；自测不等于验收。完成最终UI+百万库组合、迁移/回填30s与600s上界、首中尾日志/虚拟列表/轮换/保护/慢读/磁盘失败、核心总RSS和1000样本交互测量；性能/规模经integration release串行阶段、故障子例经同检查debug阶段，参考环境不匹配不PASS；REQ-068/M7仍独立验收原门槛。AC-064-01必须同时收debug迁移故障和release规模证据；参考测量含writer忙时脱离≤1s，普通debug不以1s抖动判失败。 最终新增场景后重测全integration五阶段，含全部debug tui_pty成本；逐段elapsed/limit/总余量与强制截止证据齐全，超阶段/工作3000s或余量不足FAIL，不能复用T03/T05/T07时长。 第5稿：实填pre-003生产commit后同次旧/新release构建与002百万库，正常/回填中kill两克隆全部计入原五阶段；必须包含私有manager隔离/全机锁/噪声准入证据，不借旧候选测量。"
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
| AC-013-05 | M4-T09 / integration | T09关轮区间与在线/回填索引；M2-T18关轮事件；M2-T01/05事务恢复；M2-T06取消/暂停、M2-T13 ResumeRun与停止守卫；M3保留原关轮语义 |
| AC-014-01 | M4-T06 / tui-pty | T06需求/阻塞计数；T09活动身份；M2-T18验收、M2-T21预算、M3-T02规格、M3-T07追踪 |
| AC-014-02 | M4-T06 / unit | T06聚合；M2-T18有效验收；M3-T02修订、M3-T06规格审查独立身份 |
| AC-014-03 | M4-T06 / tui-pty | T06三档/状态水位；T03单快照；T02 Focus；M2-T07配置 |
| AC-014-04 | M4-T06 / integration | T09规范化与持续activity/usage生产者；T06活动时间/分析标记与活动投影；M2-T17 epoch、M2-T21预算、M3-T10 no_progress；M2-T07只提供runner |
| AC-014-05 | M4-T11 / adapter-live | T09真实规范化接线；T03 API；T06活动投影；M1-T03 adapter-live及三适配器；M2-T06取消；需要真实认证 |
| AC-015-01 | M4-T07 / tui-pty | T07多行/完整语料与实测表/版本漂移比较/唯一回退/会话内恢复/四路径；T02/05布局；M1-T08解码/PTY；M3-T09提交；test-infra ssh/tmux |
| AC-015-02 | M4-T07 / tui-pty | T07编辑器scope/Esc保护；M1-T08输入历史；T02面板/SGR；M3-T09模式命令（不声称M3有Ctrl+P）；M2-T06控制 |
| AC-015-03 | M4-T07 / integration | T07边界/帧编码/可读文件/原文投影（debug功能+release RSS）；T09段对象；T03低空间/测量runner；M1适配器fixtures；M3-T09投递/四态 |
| AC-015-04 | M4-T07 / tui-pty | T07无光标恢复的启动复位/退出码/teardown/原屏往返；M1-T08恢复Guard；T02鼠标；M2-T06后台控制 |
| AC-015-05 | M4-T10 / integration | T10百万库最终测量；T07输入；T03参考runner/背压；T05虚拟化、T06状态栏、T08最终键位 |
| AC-016-01 | M4-T08 / tui-pty | T08无障碍及历史/Run选择AC；T02面板/抽屉/视图、T05块、T06Timeline/status、T07历史guard；M1-T08 Ctrl+R、M2-T13 /resume、M3-T09命令 |
| AC-016-02 | M4-T08 / unit | T08 scope/冲突；M2-T07配置；M3-T09 mode_cycle |
| AC-016-03 | M4-T08 / tui-pty | T08 ui-only保存/恢复/CAS；T02面板；T07输入；M3-T03覆盖设置/M2-T07配置监听 |
| AC-016-04 | M4-T08 / unit | T08样式；T06真实状态；T02布局 |
| AC-064-01 | M4-T10 / integration | T10完整组合；T09迁移/回填；T03索引/GC/ENOSPC/慢流与测量；T05虚拟列表；T07终端；M3-T04迁移运行器 |
| AC-064-02 | M4-T10 / integration | T10大库/RSS；T03 Range/search/测量；T05缓存；T02 Workspace/Focus；T07最终输入 |
| AC-064-03 | M4-T03 / integration | T03引用/GC/pin/tombstone；T09段登记；M2-T16冻结/M2-T18证据与历史；M3-T02失效；已有release manifest根 |
| AC-064-04 | M4-T03 / recovery | T03对象/空间/SendInput受理503 STORAGE_LOW_SPACE；T09原文对象；M3-T09接受事务；M2-T01 DB_FULL、M2-T05/06恢复控制；test-infra ENOSPC设施 |
| AC-064-05 | M4-T03 / integration | T03字节背压/release测量及debug事务隔离；M2-T12 SSE与M2-T01单writer；T09公开事实 |

### 批准后拟应用的文件

| 文件/区域 | 拟改动与承担者 |
|---|---|
| `contracts/requirements.json` | 八项detailed、37 AC、behavior/failure_modes/decisions/doc_refs；REQ-018补ui键、ui-only监听区分与字节单位；不删除旧AC |
| `contracts/acceptance-scenarios.json` | 保留八个旧TEST、增加29个；各行given/when/then转fixture/steps/pass_criteria，check_id按表；新增的观测路径全部在指定check内 |
| `contracts/openapi.json` | §2 typed字段/路由/Range/search/明确错误，§5 SendInput唯一503 STORAGE_LOW_SPACE及details、§4 SendInput编码/解码大小限制与非SendInput 2MiB复核；既有404 BAD_REQUEST与SSE409/410保持；page expiry扩展410顶层水位 |
| `contracts/protocol.schema.json` | §2七个ErrorCode（含HTTP STORAGE_LOW_SPACE）、RejectionReason追加同名STORAGE_LOW_SPACE及description；不改Run/TaskResult/Gate状态 |
| 下一顺序`contracts/sql/*_ui_observability.sql`与`scripts/product/persistence-whitelist.json` | §2迁移/索引/段/去重/引用/tombstone；T09运行器+异步回填，T03查询GC；加入扫描白名单与baseline --add |
| `contracts/development-tasks.json` | 上述九implement/T04片段；M4-T01前置保留；M6-T03/M7-T04 done_when追加两平台同候选/同次tui-pty原始证据、缺项失败及QA核对，不转AC或check归属 |
| `tests/check-plan.json`、`scripts/product/cargo-tests.sh`、测量example与结果写入器 | T03交付驱动时既有integration才同步追加三个声明产物与串行release阶段；debug故障/libtest及release必要子场景清单、五阶段强制时限与T05/T07/T10重测、schema合法SKIPPED诊断（REFERENCE_ENV_MISMATCH/MEASUREMENT_BUSY/MEASUREMENT_NOISE/NOT_DELIVERED），环境/忙/噪声退出78；同次旧/新release构建和真实002造库/计时；不改14项check/mode/超时、release-policy或gate |
| `scripts/dev/test-infra.sh`、`.github/workflows/bundle-ci.yml` | T03隔离ENOSPC/私有manager与项目盘测试安装、外层scope/真实manager guardian寿命和SIGKILL清理、共同slice和产品service/scope/Attempt的实际4CPU/8GiB、内核身份及阶段时限；预置全机锁/噪声观测设施；T07非默认矩阵feature的全target编译/lint、HOH_CHECK_ID检查与普通CI无TEST id冒烟；缺设施失败；不用生产用户目录/共享端口 |
| `tools/run_check.py`、仓库cargo包装器及其所有调用入口 | T03随测量设施交付全机共享/独占锁、继承校验与清理完成后释放，补并发负例；受锁工具修改按本CR批准后relock，不改check选择/mode/超时或降低gate；未纳管同机负载不得性能PASS |
| `tools/terminal-width/`、`tests/fixtures/terminal-width/`与TUI渲染器 | T07提交语料/测量器/逐路径生成表/版本manifest；运行时跨度消费与唯一回退；AC-015-01现场重生成/版本漂移FAIL；不将手写数字放入新表 |
| REQ-004退出码说明、`docs/adr/ADR-031.md` | T03补503 STORAGE_LOW_SPACE仍映射客户端69且明确存储不足、不自动重试；保留原AC/TEST与其他503行为 |
| `.hoh-dev/environment.json`（冻结前） | 记录本VM参考profile、限制/预算/构建输入及实填old_schema_producer提交/tree/002/锁文件摘要；检查实际值另写m4-environment，不修改已冻结环境 |
| `docs/02-functional-spec.md`、`docs/18-traceability.md` | 只由生成器更新 |
| `docs/03-ux.md`、`docs/05-data-api.md`、`docs/12-user-guide.md`、`docs/adr/ADR-014.md`、`docs/adr/ADR-027.md`与实现ADR | 正式应用点名修改docs/03“使用Ctrl+J或Esc+Enter多行”为“仅Ctrl+J换行；Esc后Enter不提交”，授权来源为第2/3稿已记CTO绑定指令、本次委派继续要求Esc保护（不是作者自行删交互）；若CTO判定超出CR-003 D1则按docs/21转所有者，不在本DRAFT自行批准。ADR-014同步guard/历史重新武装/恢复所有权，docs/12列实测表版本、唯一回退与未验证终端限制，ADR-027 exports守卫；实现时另补实际执行证据，不把本稿当已实施说明 |
| `config/hoh.example.toml`、Rust与fixtures | 随所属实现任务同提交支持配置/字段，全部回归TEST-018-01；现阶段不提前改example |

正式M4-T01应用先重核最终main/M3、独立复审并决定下文剩余提案，再按docs/21应用/生成/relock/bundle。当前只改DRAFT文件；不relock、不改台账、不推送，不以bundle代替新AC的product验收。

## 不变的内容与授权边界

八项仍required/v1、旧AC/TEST不删除；REQ-064-01原组合范围保留，改归最终UI后的任务；REQ-068不缩窄到host。默认Workspace、可选Team、公开Focus、三级Timeline、真实statusline、完整输入/无障碍、被引用对象保护均保留。三Agent/三Pi插件/Web/M5–M7、72h与20次真实迭代、独立QA/Review及身份绑定不变。仅OSC52这一未被原规格要求的增强延期，文件复制按CTO指令保留。

这是CR-003 D1细化提案；若后续CTO认为某剩余选择改变已确认交互/降低门槛，必须按docs/21升级所有者。本草案作者不批准自己。

## CTO 问题：剩余选择与提案（不是重新询问绑定指令）

| ID | 待决定的问题 | 提案与理由 |
|---|---|---|
| D1 | additive读模型/API与迁移形状 | 采用§1/2的H+generation/typed refs及003；旧路由错误保留；新码只用于§2点名的新路由/新功能，包括既有SendInput对象预检的503 STORAGE_LOW_SPACE；最终迁移编号随M3确认 |
| D2 | 当前进度分母与历史呈现 | 当前批准spec全部需求为m，当前Run最新ACCEPTED候选全PASS需求为n，DEGRADED单列；旧计数只在标候选的历史详情，未附着显示未知，避免借旧验收 |
| D3 | 轮次成员、终结尾部与事件分类 | Run内close区间，按真实state区分终结/暂停/INCOMPLETE恢复；H1/H2尾部不漂移；ProducedEventKind写入约束+穷举分类/旁路负例、H13与Focus层级；不改关轮/预算，不依赖tasks.iteration_id |
| D4 | 本VM参考profile、全integration成本与预算 | 保持既有integration唯一串行release、4CPU/8GiB/原门槛；硬件/限制为匹配键，软件补丁仅记录并对照测量；正式integration PASS只收此profile，非参考退出78；T03先量成本、总3600s内留600s，不能满足则CTO重规划是否另加required check（本稿不添加）。§5.1私有manager/项目盘测试根和独立安装、真实manager的外层scope/guardian兜底；同次pre-003 release writer造002库、首次open起计及中断重启共享600s；全机排他锁和逐窗口竞争负载核对、共享锁入口/继承/清理方案，T05/T07/T10全阶段重测/强制时限；debug故障与release计量分层；M7仍核验原SSD/REQ-068 |
| D5 | 日志/孤立对象/导出保留 | 8MiB×5未引用诊断、48h孤立宽限、所有明确引用与pin保护、tombstone；显式exports不自动删且单列占用 |
| D6 | 低空间阈值与输入错误/重试形状 | min_free=32MiB、流段预留1MiB；受理前唯一503 STORAGE_LOW_SPACE+details/保草稿，CLI沿现有69但明确提示存储不足且不自动重试；已202投递遇低空间保持QUEUED，既有安全点重试；控制202/DB_FULL分界已绑定 |
| D7 | 搜索/复制细节 | 完整UTF-8分类、8MiB续扫含尾重叠、file复制到受守卫exports；OSC52已决定延期，不是待选项 |
| D8 | 状态档与ui-only保存 | 采用三档明确字段表与项目配置原子CAS保注释、ui-only不触发ConfigureTeam，保持执行配置冻结语义 |
| D9 | 键位覆盖与防护恢复细节 | Esc/Alt+Enter不发送且仅Ctrl+J换行已绑定，不重问；提案普通Enter在false时保持false，确认改变草稿的编辑/粘贴/历史接受才重新武装，历史确认键不发送；scope/action/key、保留键、面板可达、mode_cycle不变，Tab唯一命令前缀补全/歧义只移焦 |
| D10 | 宽度回退、输入上限与平台收取 | 宽度只由仓库工具逐路径实测/提交/现场重生成已绑定，不再询问是否采用unicode-width；具体提案为§4语料/manifest布局、保守路径交集和唯一可逆ASCII码点串回退，未列/路径分歧不猜宽度，已约定字符原样；编辑1MiB/HTTP8MiB/inline256KiB且编码768KiB，否则可读根文件；M4的26.04正式矩阵与无TEST id CI分离，M7一次检查收齐两平台原始记录/缺项失败，支持范围不减 |
| D11 | 公开事实/用户正文与背压计时 | 白名单public块+activity/usage、native id/offset去重、未知null、有界队列；SendInput接受事务写input_id原文块，引用提示不替原文；仅本地队列背压暂停inactivity（不暂停deadline/取消），三家fixture/live分别补证 |
| D12 | 九实现任务分工 | 先T09生产者、T03查询/容量，再T02/05/06/07/08；T10最终组合、T11三家live，T04独立收口；均≤2REQ/8AC，检查owner不变 |

## 设计审查第 1 轮的处置

输入为第1稿`8960fdf`；Claude报告 `/home/ai/dev/hoh/.hoh-dev/review-cr028-d1-claude.md` 与Grok报告 `/home/ai/dev/hoh/.hoh-dev/review-cr028-d1-grok.md` 的**最后hoh-report JSON**，分别36与22项、合计58项（critical=0、high=13、medium=28、low=17）。保持原severity，不用草案修改声称独立审查已PASS。以下是第2稿的历史处置；其中启动1049复位、固定簇宽、包装器外stty与Shift+Enter等不足已由第3稿正文及下节第2轮处置取代。“修订/采纳”均只指本稿文本回应，等待第2轮复审和后续实现；所有medium已在本文修订，没有延期medium。低项CR28D-30部分采纳、-31按绑定指令不采用OSC，其余一并修订。

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
| CR28D-34 | low | 第2稿采纳有误；第3稿改为只复位无光标副作用的私有模式，维持ADR-014恢复所有权 | §4；AC-015-04；CR28D-100/-150 |
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
| CR28D-81 | low | 第2稿含alt复位有误；第3稿仅mouse/paste，遗留alt明确残余限制 | §4；AC-015-04；CR28D-100/-150 |

## 设计审查第 2 轮的处置

输入为第2稿 `50e9fc7b12e9a23e30c6b64a6f809b2d8cd579df`。逐项读取 `/home/ai/dev/hoh/.hoh-dev/review-cr028-d2-claude.md` 与 `review-cr028-d2-grok.md` 的最后一个 hoh-report JSON：Claude 18项（2 high/7 medium/9 low），Grok 11项（3 high/7 medium/1 low），合计29项（5 high/14 medium/10 low）。严重级别保持原报告值；本表“修订”只表示文本已有可审查回应，不代表独立审查已认可或产品实现已通过。本表记录第3稿当时的文本回应，未延期medium；10项low当时一并处理。第3轮已判FAIL；其中宽度、pgid、分类和DEGRADED等修法被下节第3轮处置取代，不能把此历史表当作现行规范或已闭合证据。

| ID | severity | disposition | where |
|---|---|---|---|
| CR28D-100 | high | 按CTO指令修订：启动绝不1049l/1047l，保留ADR-014撤销所有权；干净主屏/旧DECSC两种往返核原行与光标行，遗留alt列残余 | 第3稿绑定指令；§4启动复位；AC-015-04 |
| CR28D-101 | high | 删除固定簇宽；逐路径绑定外层终端版本，以实际光标为oracle；歧义簇ASCII回退为D10显式提案，原文保留 | §4宽度策略/四路径表；AC-015-01；D10 |
| CR28D-102 | medium | 无临时层的Esc同样解除armed；false时连按不发送，Alt+Enter先解除武装且无换行，补50/100/500ms与快键 | §4输入guard；AC-015-02；D9 |
| CR28D-103 | medium | 补config.invalid、全部compat.suite种类/manifest_unreadable、session.native；集中生产kind注册表包含动态分支，漏分类单测失败 | REQ-013分类表；AC-013-04；T03 |
| CR28D-104 | medium | 测量用scope原地exec保持pgid，禁止service/脱离；正常/失败/超时/取消外部核对无残留，生命周期要求双独立审查 | §5.1进程生命周期；T03 done_when |
| CR28D-105 | medium | 硬匹配键与软件观察量分开；补丁升级记录版本并做对照测量，不因版本字符串变化重开CR或误判机器 | §5.1环境；D4 |
| CR28D-106 | medium | T03先量阶段成本并在3600s内留600s；超预算交CTO重规划；环境不符退出78保留NO_TESTS=3；工件声明随驱动同提交才生效 | §5.1预算/出口；T03；应用清单；D4 |
| CR28D-107 | medium | v1无限保留events，产品SSE410不可达；保留OpenAPI静态形状、客户端stub显式标注，不伪造产品过期；page410仍真实测 | §2错误契约；AC-010-03、013-02 |
| CR28D-108 | medium | 暂挂事务/升级失败移至同integration的debug libtest，release只计量及正常升级；无外部写DB/无release调试钩子 | §5.1分层；AC-064-01/05 |
| CR28D-109 | low | 终结Run不显示假开放轮；末close后尾事件只P/iteration=null，按H派生，重放一致并覆盖终结夹具 | §2.1；AC-013-05；D3 |
| CR28D-110 | low | T09索引不存class，T03查询时集中分类，消除反向依赖/再次回填 | §2迁移；分类表；T09/T03 |
| CR28D-111 | low | 沿adapter-live现约：缺认证逐测试SKIPPED/detail verdict=BLOCKED、退出3，非PASS | AC-014-05 |
| CR28D-112 | low | 查明当前inactivity读取executor输出字节/工具活动；提案仅本地背压暂停静默计时，取消/deadline照常并有解除后静默回归 | §2.2背压；AC-011-05；D11 |
| CR28D-113 | low | 文件在Attempt/底座可读根（含Commander只读档），fixture核可读；APPLIED仍是安全点语义，不宣称已读 | §4帧预算；AC-015-03 |
| CR28D-114 | low | 独立host换token制造旧会话403，整个连接stale/保草稿并显式重连，不解释为单对象权限 | AC-012-03 |
| CR28D-115 | low | paste/三adapter/HTTP与文件错误成为带test_015_03的debug libtest，CI可跑；RSS仍release必需子例 | §5.1分层；AC-015-03；T07 |
| CR28D-116 | low | 正常只开1000/1002/1006，启动仍清遗留1003；文档说明Shift拖选条件、tmux拦截与ui.mouse=false | §4鼠标；docs/12应用义务 |
| CR28D-117 | low | 明确M6准备24.04，M7一次最终tui-pty收两平台原始证据；runner缺项失败，独立QA/Reviewer核对，不让gate猜平台 | REQ-015矩阵；M6-T03/M7-T04应用义务；D10 |
| CR28D-150 | high | 按CTO指令不采用按DECRQM有条件1049l建议；启动一律不退出备用屏，补干净主屏和旧DECSC往返 | 第3稿绑定指令；§4；AC-015-04 |
| CR28D-151 | high | SSH远程shell/tmux pane/远程pane在包装器存活时读stty/modes；HoH应用PTY逐项关闭字节，外层只补查 | §4四路径与恢复观察；AC-015-01 |
| CR28D-152 | high | 加单write \x1b\r和10/20ms两键：零SendInput/草稿不变/零新LF；Enter/Alt+Enter同guard，仅Ctrl+J换行 | §4输入guard；AC-015-02 |
| CR28D-153 | medium | 宽度oracle明确为终端实际列/行，记录vt100/tmux版本，禁wcwidth/CellWidth自证；不把本机某簇宽推广到其他路径 | §4宽度策略与版本表；AC-015-01 |
| CR28D-154 | medium | SendInput接受事务写input_id幂等用户原文块，大正文对象展开/导出等于原文；adapter短引用不替换对话正文 | §2.2用户生产者；AC-011-05、015-03；T09 |
| CR28D-155 | medium | AC增加最后活动时间t1→t2可见与冲突模型摘要，标分析且不覆盖工具事实或进度 | AC-014-04；§3；T06 |
| CR28D-156 | medium | 明确payload.role/from/to各epoch/session与notice角色匹配，缺字段不猜；H13列prepared/delivered完整集合 | REQ-013 H13；AC-013-02/04 |
| CR28D-157 | medium | 非默认feature注册正式矩阵且核HOH_CHECK_ID=tui-pty；CI同助手冒烟无该TEST id、不return ok伪通过；M7平台收取另明确 | REQ-015矩阵；T07应用义务；D10 |
| CR28D-158 | medium | AC拆四路径键盘组与原始SGR应用PTY组；tmux占用鼠标不要求点击成功，但不豁免键盘/退出配对 | §4鼠标；AC-015-02 |
| CR28D-159 | medium | 明确正式integration PASS只收参考profile，24.04矩阵仅tui-pty/普通cargo功能；环境不符退出78且整检查非PASS，保留已跑诊断，不借DEGRADED通过required | §5.1出口；REQ-015矩阵；D4 |
| CR28D-160 | low | debug用锁生命周期探针断言脱离先于writer释放/超时、不等busy_timeout；1s绝对界限移到参考测量 | AC-010-03；§5.1预算表 |

## 设计审查第 3 轮的处置

输入为第3稿 `ebaf570efc98675ad1a6d8597f7aeff05e2b294c`。逐项读取两份报告的全部发现及最后hoh-report：Claude `/home/ai/dev/hoh/.hoh-dev/review-cr028-d3-claude.md`，会话 `03677589-bb23-4dbd-9252-8e95c45ee69f`，12项（2 high/4 medium/6 low）；Grok `/home/ai/dev/hoh/.hoh-dev/review-cr028-d3-grok.md`，会话 `13d6a4c6-8f0b-4f8a-a3ab-c2b973c6a24a`，9项（1 high/6 medium/2 low）。合计21项（3 high/10 medium/8 low），均在第4稿回应，没有延期high/medium，原级别不改。下表是历史处置；其宽度例外策略被§4第5稿CTO实测表设计修正覆盖，生命周期/造库仍须遵守第4轮处置。下表“修订”是当时的文本回应；后续第4轮独立复审结果见下一节，不能把本表当成发现已关闭或产品PASS。

| ID | severity | disposition | where |
|---|---|---|---|
| CR28D-200 | high | 按本次CTO指令保留真实setsid/service/scope/Attempt cgroup；撤销同pgid禁脱离。资源限制落到共同slice与实际unit，逐PID/start_time/pidfd/own cgroup测量；独立监护者在SIGKILL后unit stop+populated=0证明清理。未采用审查建议中的“测量不派Attempt”限制 | 第4稿指令；§5.1测试接入/生命周期；T03；D4 |
| CR28D-201 | high | Ambiguous正常显示，不再码点回退；只对有路径分歧证据的具体簇作有限例外；保留原文与终端oracle | §4宽度/例外表；AC-015-01；D10 |
| CR28D-202 | medium | required环境不符统一SKIPPED/detail reason=REFERENCE_ENV_MISMATCH，退出78，禁止DEGRADED；六条计量AC与应用清单同步 | §5.1出口；012-01、015-03/05、064-01/02/05 |
| CR28D-203 | medium | 补cancel六项/effect.committed、沿生产链补lease/runner收尾30种与不带点的integrity_violation；生产写入封闭类型、生成ALL、分类无通配、动态分支穷举及唯一INSERT/AST守卫；三类旁路负例证明漏项可检出 | REQ-013分类表；AC-013-04；T03/T06 |
| CR28D-204 | medium | 唯一503 STORAGE_LOW_SPACE+字节details；受理失败保草稿/无接受消息；已202投递写文件低空间保持QUEUED，沿原链重试 | §2码表、§2.2、§4帧预算、§5；AC-064-04/015-03；D6 |
| CR28D-205 | medium | 保留integration的--test '*'，明确计入debug tui_pty；冻结五阶段时限且强制截止，T05/T07/T10逐段重测全套/记录余量，超限FAIL不挪预算 | §5.1阶段限额；T03/T05/T07/T10 done_when；D4 |
| CR28D-206 | low | 明确粘贴、历史接受、清空、面板文字等已确认内容变化重武装；无变化/预览不重武装，提示可操作方法；历史确认Enter消费而不发送 | §4输入；AC-015-02；D9 |
| CR28D-207 | low | T07在CI编译/lint非默认tui-acceptance全target，仍不执行正式矩阵或伪造TEST行 | REQ-015矩阵；T07；CI应用清单 |
| CR28D-208 | low | INCOMPLETE→ResumeRun对同尾事件在H1/H2核对归属，旧H1页不变；恢复重开同区间不新增关轮 | §2.1状态表；AC-013-05 |
| CR28D-209 | low | Focus可选role/epoch/session级；无attempt_id的交接只在可匹配级出现，Attempt过滤交接为空 | §4 Focus；H13；AC-011-03 |
| CR28D-210 | low | 应用清单点名替换docs/03的Esc+Enter原句并注明已有CTO绑定来源；本DRAFT不自行批准交互变更 | §4；docs/03/ADR-014应用清单与授权边界 |
| CR28D-211 | low | 路由8MiB后按命令种类保留非SendInput 2MiB限制；合法命令加JSON空白至3MiB为400 BAD_REQUEST、零写入 | §4帧预算；AC-015-03 |
| CR28D-250 | high | 与201同根因：正常标点/框线/希腊/箭头单元格必须仍为自身，逐簇实测例外不能扩大为字符类别 | 第4稿指令；§4；AC-015-01 |
| CR28D-251 | medium | Ctrl+C非空先清空且零输入/控制；空稿显示三选项，未选零控制，选择暂停/取消仅对应一条，脱离零控制 | AC-015-02 |
| CR28D-252 | medium | 有层/无层Esc均零Pause/Cancel，受控Run状态与原Attempt身份/存活不变 | §4；AC-015-02 |
| CR28D-253 | medium | 未预选打开Team/Focus，同claude的cto/developer都列在选择器，选前Focus空/无派发，不自动选职位 | AC-011-01；T02 |
| CR28D-254 | medium | 费用与上下文必须显示“未知（qa/pi，原因）”，不能以0/$0/0%代未知 | AC-014-01；T06 |
| CR28D-255 | medium | Ctrl+R接受只召回草稿、确认键不提交；/resume选择前不换附着/不发控制，选后仅附着（INCOMPLETE不自动ResumeRun） | §4历史；AC-015-02/016-01；T02/T07/T08及闭包 |
| CR28D-256 | medium | 真终结按RunResult.state=COMPLETED/FAILED/CANCELLED及run.cancelled；PAUSED/BLOCKED的run.result不终结，INCOMPLETE单列恢复规则；五状态子例 | §2.1；AC-013-05 |
| CR28D-257 | low | 八类块分别单击仅选、双击展开、滚轮改视窗；键盘照测，tmux拦截只豁免被拦点击 | AC-012-01；T05 |
| CR28D-258 | low | 与210一并点名docs/03旧句替换、仅Ctrl+J换行、Esc后Enter不提交与授权来源 | 应用清单；§4 |

## 设计审查第 4 轮的处置

输入为第4稿 `a864584a766ea8c0a4e997b9d59e3e6cd1d7834e`。已逐项读取全部发现与最后hoh-report：Claude `/home/ai/dev/hoh/.hoh-dev/review-cr028-d4-claude.md`，会话 `0a7a7a46-e80d-41d6-9602-6190aae5dec7`，结论PASS、7项（4 medium/3 low）；Grok `/home/ai/dev/hoh/.hoh-dev/review-cr028-d4-grok.md`，会话 `479a9391-1abf-4c09-874f-bee1e60b1021`，结论FAIL、5项（1 high/1 medium/3 low）。共12项（1 high/5 medium/6 low），全部在本稿回应，原severity不变。PASS报告交付不抵消另一报告的可复现high；本稿的处置只代表文本修订，尚未经第5轮独立复审，不声称产品修复/验收PASS。

| ID | severity | disposition | where |
|---|---|---|---|
| CR28D-300 | medium | 纳入CTO绑定设计修正：撤销unicode-width模型和手写例外，工具逐路径实测全语料/提交表，渲染消费实测跨度，覆盖一致但不同于库模型的肤色簇；不再手填✈️值 | §4；AC-015-01；T07；D10 |
| CR28D-301 | medium | 点名实填pre-003产品commit，在同次检查构建旧release并用其writer造002百万库；旧/新构建计release_build，候选首次open起计，回填中kill/reconcile和正常克隆共用阶段600s，不重置；未有真实M3提交不造身份 | §2迁移；§5.1旧schema生产者；AC-064-01；T03/T10；D4 |
| CR28D-302 | medium | 私有测试user manager，保留hoh-host@模板名但根/安装/配置/socket仅测试目录，位于项目盘而非tmpfs；外层scope RuntimeMaxSec=3600；真实manager的独立guardian在run_check组SIGKILL后stop外层scope并证明空树 | §5.1接入/生命周期；T03；D4 |
| CR28D-303 | medium | 采用全机同inode共享/独占锁，run_check/cargo入口遵守，guardian持锁到清理完成；未纳管竞争负载/锁忙不测，逐窗口未登记竞争负载或缺样则SKIPPED/78，不能噪声PASS；准入方案作为D4提案明示 | §5.1排他/出口；T03/T10；工具应用清单；D4 |
| CR28D-304 | low | 明确保留既有503→CLI69映射，但输出STORAGE_LOW_SPACE/存储不足/未接受及details，无自动重试、无run.result或Run状态变更；补CLI三输出格式子例 | §5；AC-064-04；REQ-004说明应用清单；D6 |
| CR28D-305 | low | 未分类variant用test_013_04的compile_fail,E0004 doctest，生产注册定义共源与正常编译例配对；既有unit --doc收取，不嵌套cargo；其他旁路守卫继续保留 | REQ-013分类机制；AC-013-04；T03/T06 |
| CR28D-306 | low | 未交付义务明确SKIPPED/detail.reason=NOT_DELIVERED；不把NOT_RUN写入test-result.status | §5.1阶段结果；应用清单 |
| CR28D-350 | high | 与300同根因且按CTO设计修正处理：删除手写宽度/例外表；语料明确家庭ZWJ/肤色/VS15/VS16/旗帜/组合符与审查命名簇；每簇每路径重生成/版本比较与实际光标/cells验证，缺表/未知路径分歧仅一种确定回退 | §4绑定修正；AC-015-01；T07；D10 |
| CR28D-351 | medium | results.rs生产candidate_ready进入I工程事实层，保留task/candidate/attempt/round引用；断言不是诊断，错误映射的变异必须失败 | REQ-013分类表；AC-013-04；T06 |
| CR28D-352 | low | D1新码范围改为点名的新路由/新功能，显式包括既有SendInput对象预检503 STORAGE_LOW_SPACE | CTO问题D1；§2码表 |
| CR28D-353 | low | 八类块各用两处不重叠匹配，n从x1到x2、N回x1，block/草稿不变且零命令 | AC-012-01；T05 |
| CR28D-354 | low | Tab唯一/he补/help、Enter前不提交；歧义/只移焦且原文不变，与普通scope移焦并列覆盖 | §4动作；AC-016-03；T08；D9 |

## 验证记录与交付边界

本任务是DRAFT规格修订；只改本文件，未改生产代码/受锁契约/检查计划、未领取任务/推进全局台账、未relock。没有Runtime run_id/task_id/attempt_id/role_epoch身份头、TaskResult输出文件与指定自测目录，因此不编造TaskResult/candidate ID；依委派要求交付本地文档commit。起始分支prep/M4-T01-spec、HEAD=a864584a766ea8c0a4e997b9d59e3e6cd1d7834e、工作树干净，唯一写权限由本次输入授予。

前稿诊断留在 `.hoh-dev/evidence/m4-spec-d2/`、`m4-spec-d3/`、`m4-spec-d4/`，不重写首次失败或借它们证明本稿行为。第5稿本地自检记录在 `.hoh-dev/evidence/m4-spec-d5/`；临时根由 `mktemp -d /home/ai/dev/m4spec-XXXXXX`创建，TMPDIR指向它，各shell加载指定env.sh；外部报告/main/M3只读。来源身份与摘要记source-bindings.json。恢复点为该目录checkpoint.md，不改全局PROGRESS/taskboard。

修订前已执行前稿ownership探针与 `python3 tools/validate_bundle.py`，PASS，记录ownership-before.json、validate-before.json。修订后按本稿37条AC与CR-026的16项需求/8实现任务合并拟议任务图，调用真实 `validate_bundle.ownership_problems`，核对单AC owner、单check owner、verify无acceptance、≤2REQ/8AC及逐AC依赖闭包；四轮处置逐ID/原severity覆盖检查单列。纯文档改动按 `resources/skills/tdd/SKILL.md` 不运行生产红绿循环。

提交前已执行 `bash scripts/check.sh --bundle`：退出0，504 tests，测试耗时391.902s、命令总耗时393.850s，1项既有skip（test_repository_packages_verified：Pi packages未安装），原始记录为bundle.log、bundle.exit、bundle-result.json。skip保留，未计为产品PASS。修订后ownership自检PASS：37 AC/9实现任务、归属与依赖闭包错误0，四轮120条处置的ID/原severity一致，本轮12项为1 high/5 medium/6 low；记录ownership-final.json、review-coverage-final.json。`python3 tools/validate_bundle.py` 与 `git diff --check` 通过，当前契约product_accepted=false；这不是本提案应用后的新AC验收。范围核对scope-check.json仅本文件变化、无未跟踪源码；临时根在bundle结束后清理，证据保留在忽略目录。

本稿37条AC的产品红/绿、宽度测量器/生成表、三native fixture/live、平台矩阵、性能/私有manager及噪声隔离和低空间场景均 **NOT_RUN**；第5轮独立设计复审亦NOT_RUN。新约定是待实现/审查的规格，不是本次产品测量结果。这是**自测，非验收**；不报告v1 COMPLETED，也不以本地commit代表CR已批准。旧schema生产commit与可执行阶段预算须等M3完成由T03实填；不能在此编造。下一步由CTO审定D1–D12具体提案并送第5轮设计复审；M4-T01正式应用仍等待M3-T05。
