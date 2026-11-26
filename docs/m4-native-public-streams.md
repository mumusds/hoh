# M4 原生公开流字段来源

本表是 M4-T09 的实现依据，须与产品测试一起阅读。录制与合成夹具均不代表本次 live 验收。
逐文件版本、路径、来源及 SHA-256 见 [来源清单](m4-native-public-stream-sources.json)。
字段白名单不递归复制整个原生对象。未知版本或缺少对应通知时报告 unsupported；
没有用量、成本或上下文容量时保持 null，不从进程存活推断。

| 模式与来源版本 | 原生通知 | 允许读取的 JSON 路径 | 公共含义 |
| --- | --- | --- | --- |
| Claude stream-json 2.1.317、2.1.318 | assistant | `/uuid`、`/message/id`、`/session_id`；`/message/content/*` 仅 `type=text` 的 `text`，或 `type=tool_use` 的 `id/name/input` | 助手正文、工具开始；input 逐字段筛选，非递归公开 |
| 同上 | user | `/uuid`、`/session_id`；`/message/content/*` 中 `type=tool_result` 的 `tool_use_id/content/is_error`，仅 text 内容 | 工具结束／公开结果；不公开内部提示词的 user 回显 |
| 同上 | result | `/uuid`、`/usage/input_tokens`、`/usage/output_tokens`、`/total_cost_usd`；`/modelUsage/<model>/contextWindow`、`canonicalModel` | 结束时真实用量；结束之前未知 |
| Claude stream-json 2.1.317、2.1.318，max-turns-error 录制 | result 且 is_error=true | `/errors/*` 仅字符串，经去敏 | 公开错误；用量仍单独保留 |
| 同上；compat 合成夹具 | system / compact_boundary | `/uuid`、`/subtype` | 显式压缩活动；其它 system 字段不公开 |
| Codex App Server 0.166.0 | item/started、item/completed | `/params/threadId`、`turnId`、`item/id`、`item/type`；`agentMessage` 的 `text`；`commandExecution` 的 `command/status/aggregatedOutput/exitCode` | 稳定消息、工具边界及结果 |
| 同上，原生 ServerNotification schema | item/agentMessage/delta | `/params/threadId`、`turnId`、`itemId`、`delta` | 同块追加；通知没有 offset 时用持久接收序号，不能伪称 provider offset |
| 同上，compat 合成夹具及 schema | thread/tokenUsage/updated | `/params/threadId`、`turnId`、`tokenUsage/total/inputTokens`、`outputTokens`、`totalTokens`、`modelContextWindow` | 显式用量；不读取 reasoning 内容 |
| 同上 | item/started、item/completed，item.type=contextCompaction | `/params/item/id`、`/params/threadId`、`turnId` | 显式压缩活动 |
| Codex App Server 0.166.0，原生 ItemStarted/ItemCompleted schema | collabAgentToolCall、mcpToolCall、dynamicToolCall | `/params/item/id`、`type`、`tool`；`arguments` 仅 command/file_path/path；MCP `result/content/*/text` 仅 type=text；dynamic `contentItems/*/text` 仅 type=inputText | 工具开始/结束；collab 只记录 handoff 边界，不公开 prompt、agentsStates 或内部输出 |
| 同上，ErrorNotification schema | error | `/params/error/message` 经去敏 | 明确错误诊断；不公开 additionalDetails |
| Codex exec 0.166.0 | item.started、item.completed | `/item/id`、`type`；agent_message 的 `text`；command_execution 的 `command/aggregated_output/exit_code/status` | 消息、工具开始和结束；无 steer 或在线 context 能力 |
| 同上 | turn.completed | `/usage/input_tokens`、`output_tokens` | 结束时用量；context limit 未提供时 null |
| Codex exec 0.166.0，exec-turn-failed 录制 | error、turn.failed | `/message` 或 `/error/message`，经去敏 | 公开错误诊断 |
| Pi RPC 1.5.0、1.6.0 | message_update | `/assistantMessageEvent/type`、`contentIndex`、`delta`，仅 text_delta | 文本增量；thinking delta 不公开；消息编号来自接收顺序，不伪造原生 ID |
| 同上 | message_start、message_end | `/message/role`、`responseId`；`/message/content/*/text` 仅 type=text；`/message/usage/input`、`output`、`totalTokens`、`cost/total`、`/message/model` | 助手消息及提供的最终用量；system/user 内部提示词不公开 |
| 同上，compat 合成夹具 | tool_execution_start、tool_execution_end | `/toolCallId`、`toolName`、`args` 白名单；`/result/content/*/text` 仅 type=text、`/isError` | 工具开始与结束，公开结果 |
| Pi RPC 1.5.0、1.6.0，rpc-invalid-model / rpc-abort 录制 | response(success=false)、message_end | `/error`、`/message/errorMessage`，仅字符串，经去敏 | 公开错误或中断原因 |
| 同上，compat 合成夹具 | compaction_start、compaction_end | `/type` | 活动；沿既有 agent_settled 结束判定 |
| Claude stream-json 2.1.317、2.1.318 | wait 活动 | unsupported（已录制协议与 schema 无原生 wait 字段） | 明确 unsupported，不从进程存活或静默推断等待活动 |
| Codex App Server 0.166.0 | wait 活动 | unsupported（已锁定 ServerNotification schema 无原生 wait 通知） | 明确 unsupported，不从进程存活或静默推断等待活动 |
| Codex exec 0.166.0 | wait 活动 | unsupported（exec JSONL 录制无原生 wait 帧） | 明确 unsupported，不从进程存活或静默推断等待活动 |
| Pi RPC 1.5.0、1.6.0 | wait 活动 | unsupported（known-frames 与录制无独立 wait 活动帧；`auto_retry_*` 仅记重试活动） | 明确 unsupported，不从进程存活或静默推断等待活动 |

原生 session_id/threadId/turnId 由既有 adapter 校验；公开记录的 HoH session/role/epoch/config/spec 从持久 Attempt 取值，不直接把原生 session 字符串当 HoH 身份。

这些白名单已接到 adapter → runner → Persistence；四种模式均由 integration 中的产品 runner fixture 驱动。
Claude stream_event、Pi auto_compaction_*、各版本缺失的 wait 活动和表外字段保持 unsupported/unmapped，按 `(attempt, frame_kind)` 聚合计数并记录 `frame_kind` 与 `count`。
聚合诊断的 `count` 是累计最新值，不是增量：首条为 1，流结束时的汇总条给出该 `frame_kind` 的累计总数，两条不能相加。
Pi auto_retry_start/end 采用版本化 known-frames.json 中的显式类型，只保留活动标记。
其他所有版本（包括空版本）均为 unsupported_native_version（同样按 `(attempt, frame_kind)` 聚合计数），不推断兼容，不改变已有 compat 判定。
Claude 同时识别实际版本输出 `2.1.317 (Claude Code)` / `2.1.318 (Claude Code)`，
Codex 同时识别 `codex-cli 0.166.0`；除此之外不做模糊版本匹配。

公开工具参数和输出仍需要凭证模式去敏，并在事件标出 redacted；auth/env、系统提示词、
thinking/signature、reasoning item、Pi sections/toolsAdded 等不进入公开块或对象。
原生审计日志沿原有受控路径保存，本表不授权把原始日志直接提供给 Focus。

产品 runner 的合成工具夹具来源如下。每行的完整 hash 与上述字段路径表一起构成版本映射；其它录制与 schema 的完整 hash 在来源清单中。

| 模式 | 支持版本 | 工具夹具 SHA-256 |
| --- | --- | --- |
| claude | 2.1.317, 2.1.318 | `a7f06110e8bec9c9c12ce1c7b24cb8321082acc78155b896aad1f828fdbcf8c5` |
| codex | 0.166.0 | `b68ac4bddcd639c29836d0340f97d8755c76f9d5c54723acd3155e74b9eb3ccf` |
| codex-exec | 0.166.0 | `c84f3f02ff65fd307a65a42f47f083276358f01f71a530347a0a5838e7aaebbe` |
| pi | 1.5.0, 1.6.0 | `692491af0d6a4560f0c0fd67509f0fa0739ed5b23b8dd35256e0a13853d5de3e` |

公开正文按 UTF-8 字节计数：≤16 KiB 可放入事件；较大正文以 ≤1 MiB 段封口，经过
fsync → rename → 目录 fsync → artifact 登记，才写引用事件。流式文本增量按 CR-028 §2.2
在 ≤50 ms 或 16 KiB（先到即提交）内合并，工具与块边界立即冲刷；任何后续帧（含未映射帧）先冲刷已到期的合并文本。

流式去敏（DC-M4T09b）：每个块中最后一个换行之后的字节不发布，直到换行到达或块结束（完成帧、进程结束、取消或放弃）。
因此工具边界、不可合并帧和 50 ms 定时器只冲刷到最后一个换行为止。
整行与结束时的尾部都经过批量路径同一个 `redact` 函数，所以只要每行（含换行）短于 32 KiB，无论增量如何切分、何时冲刷，
同一块发布正文的拼接都等于对全文的批量去敏结果，跨模式输出一致。
长行（M4T09R-400）：未结束的一行积累到 16 KiB 加保留窗口 W = 16 KiB（共 32 KiB）时，只发布比最后 W 字节更早的部分，
最后 W 字节继续保留；每次关键词检查都包含整个保留窗口。确切上界：行结束之前，一个字节只有在它之后至少已有 W 字节
无关键词的同行文本时才会提前发布，所以凭证之后 W（16384）字节内结束的关键词总能让它被去敏；只有比关键词结尾早超过
W 字节的部分可能已经原样发布，无法收回，该行其余部分（保留窗口和后续字节）替换为 `[redacted]`。
若关键词在提前发布之前就已出现，整行只发一个 `[redacted]`，其余字节直到换行都不发布。
`redacted` 表示该段被去敏改写过，即该段带有去敏引入的 `[redacted]` 标记；标记所代替的字节即使在之后的段才到达也不再发布，
这些段不因此标记；被去敏行的换行按原样发布，不标记。块只保留未结束的尾部（少于 32 KiB 加一次推送的长度）、
已接收长度与摘要，不保留全文。
时间线次序（M4T09R-505，留给 T05/T06 渲染）：没有换行的增量之后紧跟工具边界时，被保留的文本在工具事件之后才发布；
块正文与偏移正确，但文本与工具在事件序列中的先后可能与原生顺序、批量模式不同。渲染时同一块的文本按
`block_id`/`first_offset` 归并，不按事件 seq 推断文本与工具的相对位置。
段的出现身份是 `(block_id, stream, first_offset)`，相同内容可复用一个 artifact。接收队列为 64 项，
包括持久化中的正文总配额为 1 MiB；仅 `DB_BUSY` 触发背压重试与本地阻塞计时，
`StorageLowSpace`、`DB_FULL`（状态库已满）及其它写入错误都丢弃该正文并继续运行，不让存储拖住 agent。
被丢弃的正文由一条内联 `public.gap` 占据它的位置，并带 `stream`、`first_offset`、`received_offset`、
`dropped_bytes` 与原因。所有 `public.gap`（内联丢弃、原生流未完成 `native_stream_unfinished`、Attempt 中断、
进程结束的丢失与排水丢失）都由持久化层的同一个构造函数生成（DC-M4T09c）：它与 `public.message` 绑定相同的
attempt、run、task、role、role_epoch、session、config_revision、spec 与 current，并带 `reason` 与 `complete=false`；
进程结束的 gap 在 Attempt 结束事务里生成，current 为 false。因此每种 gap 在 `ui_event_index` 中都与该 Attempt 的公开块
同一 session，会话筛选的 Focus 能看到它。源码守卫测试禁止在别处构造 `public.gap`。gap 本身最多尝试 3 次（间隔 50 ms、100 ms，合计远小于一个 quiet 周期），
所以 gap 可能比后续正文晚一些出现；仍写不进时，这次丢失被计数，并写进进程结束时的 `public.gap`
（`reason=public_output_lost`，带 `first_dropped_offset`、`unrecorded_parts`、`dropped_bytes`）。
诊断失败不终止排水，排水消失时原生 stdout 读取与取消继续可达，此后未发布的帧数同样计入结束 gap。
只有 inactivity 暂停 `DB_BUSY` 本地阻塞时间，已累计的 quiet 时间保留；进程结束、取消、期限和预算继续推进。
进程结束后最多等待公开队列一秒，未完成则在原结束事务中写 gap，并附上排队未写的部分；恢复也保留已提交正文并写 gap。
读取被放弃时不再等待：已合并和被保留的文本（按上述规则去敏）与 `native_stream_unfinished` gap 以不阻塞方式送入队列，
放不下的部分计入结束 gap；放弃时尚未送入队列就被取消的一批（M4T09R-501）同样计入。丢失账本依赖单一发布者：
每个队列只有 stdout 读取任务发布、只有一个排水任务按队列顺序结算（M4T09R-504）。
适配器原有帧大小限制继续生效，超限、截断或缩减的帧明确写 gap，不能标为完整文本。

SendInput 在原接受事务中按 input_id 保存 CR/LF 规范化的原文，文件投递提示不能替代它。
角色取显式目标或已解析问题的目标，默认路径为 commander；无 Attempt 时不伪造 Attempt 身份。
对象预检低空间返回唯一的 503 STORAGE_LOW_SPACE，接受事务不留下 commands/input/public 行。
携带已接受 `input_id` 的原生 user 回显关联原块而不重复；无法核对的回显记明 `source=native_echo` 诊断，不冒充新用户提交。
注意：已锁定的原生来源（录制、schema 与 compat 夹具）都不携带 `input_id`，所以目前没有可关联的原生回显，
原生 user 回显一律保持私有；上述关联路径只由注入 `input_id` 的测试覆盖。
原生同 ID/接收 offset 重放不重复；停止或旧 epoch 的历史输出保持原身份且 current=false。

验证使用 `test_011_05_*` / `test_013_05_*`。故障夹具把唯一 marker 注入 auth/env/内部提示词/推理字段，
并把带凭证行的大工具输出落到对象后核对去敏。它是 product fixture 自测，非 live 或独立验收。

工具结束不隐去失败：Claude `is_error`、Pi `isError`、Codex `status=failed` / 非零 `exitCode`（exec 为 `exit_code`）映射为 `activity=tool_error`。`complete` 只表示该公开段已结束，不代表工具成功或验收通过。
