# CR-041 · Codex 恢复元数据与有界 item 内容

ID：CR-041
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权（两轮各两个互补的独立 Review 会话见审查记录）
日期：2026-11-25
关联：M3L-101、M3L-102（high），REQ-024、REQ-027，CR-040

## 根因与依据

M3L-102：`thread/resume` 未发送 `excludeTurns` → 响应携带整个 `Thread.turns` → 超过 1 MiB → attempt 失败；适配器只需 thread.id、model、sandbox。录制的 codex 0.166.0 `ClientRequest.json` 与本机 codex-cli 0.171.0 的 `generate-json-schema` / `generate-ts` 均明确声明布尔字段 `excludeTurns`。本机调查与命令退出码见 `.hoh-dev/self-tests/m3cxfrm/schema.log`。

M3L-101：App Server 的 commandExecution.aggregatedOutput / fileChange.changes[].diff，以及 exec command_execution.aggregated_output 没有长度上限 → 正常命令的大输出触发 `Frame::TooLarge` → adapter 尚未取出 item 内容和标识即硬停。CR-040 不能直接跳过这些帧：App Server 会将 command/status/exitCode/aggregatedOutput 或 status/changes 写入 adapter.item；exec 至少需要 itemId/itemType。

只读判定器是 `crates/hoh-agents/tests/readonly/mod.rs`（ADR-032，TEST-021-07）。`stream_items` 从 adapter replay 的 item 中枚举每个命令：逐条读取 aggregatedOutput 中的 HOH-CELL 结果，任意一次成功写入必须阻止 PASS；fileChange 用 status 与完整 changes 的 path 枚举写入，当前不读 diff 内容。丢弃输出尾部可能藏掉成功写入；只检查前缀是不安全的。

## 决定

1. resume 的共享请求构建函数加入 `excludeTurns: true`，driver 与 compat 套件检查同一个输出。兼容校验还要求被测 schema **声明** ThreadResumeParams.excludeTurns 并接受该请求；旧 schema 未声明（即使 additionalProperties 开放）不能因忽略字段得到 PASS，明确保持 COMPAT_UNVERIFIED，升级或使用已有的受测 exec 路径。不得静默去掉参数、猜版本号或把未知当支持。离线历史 replay 保留旧录制请求语义，新的请求行为须通过独立的请求测试覆盖。
2. **有界归约，不跳过 item。** reader 只对确切紧凑 JSON 前缀识别出的 Codex App Server `item/completed` commandExecution / fileChange （fileChange 的 `item/started` 携带相同 changes，也归约）与 exec `item.completed` command_execution 启用流式字符串归约。保留对象所有字段、数组所有元素、普通值及 item/thread/turn 标识；每个大字符串值替换为对象 `{truncated:true,prefix:<完整 UTF-8 前缀>,original_bytes:<解码后 UTF-8 字节数>,sha256:<解码后全部字符串的 sha256>}`。键不能被截断。前缀至多 4 KiB 的原始 JSON 字符串内容；转义和 UTF-8 字符不切半。这个独立类型使消费者不能误将前缀作为完整字符串。
3. **仅超限帧归约（M3CX-001/002）。** 原始帧不超过 1 MiB 时完全保留原有成帧语义：有 LF 为原字节 `Line`，EOF 为原字节 `Partial`，JSON/路由校验仍由原消费者处理；不会因为归约的结构预算或校验而变成 `TooLarge`，也不提前截断字符串。仅确认超过上限后，已知 item 才进入流式归约，归约 JSON 的硬预算仍为 512 KiB。普通输入最多保留上限加 2 字节（兼容 CRLF）；切换时逐段取 4 KiB，在扩展归约结果**之前**释放已消费输入的容量，不另存整帧副本。归约后续读块最多 64 KiB，诊断首部 4 KiB；字符串前缀/解码与输出增长有界。切换时至多约 129 个截断标记各增加不足 160 字节，加上字符串、首部、4 KiB 搬移块和分配余量小于 64 KiB；输入缩至读块后，512 KiB 结构预算留出更大余量。最大保留字节仍不超过 1 MiB+64 KiB。超限帧的结构、短字符串或键合计超过归约预算时拒绝，不丢字段/数组元素；不承诺任意数量文件变化都能缩进固定预算。边界回归覆盖 605,206 字节的短字符串结构帧、恰好 1 MiB（LF/CRLF）、1 MiB+1（结构拒绝与可归约字符串两种）。
4. 对已进入归约的超限帧，整个输入（包括被丢弃的字符串尾部）必须是有效 UTF-8/JSON；必须有 LF，CRLF 去掉最后 CR；原始帧上限仍为 256 MiB。错误转义、无效 Unicode、尾部错误、未知类型/前缀、无 LF、超过原始或归约预算均拒绝。归约成功后校验原始帧种类与完整字符串形式的路由字段，不能把被截断的标识用于关联。原始 stdout Tee 不变；归约事件另记 adapter.diagnostic（类型、原始长度、整帧 sha256、首4 KiB），adapter.item 保留带标记的内容，exec 也记录 aggregated_output。
5. **只读判定 fail closed。** 完成的非文本 item 的判定事实只要含截断标记，就记录证据不全、不得 PASS，不尝试用前缀推断后缀。即使当前判定器不读 diff 或 token-read 输出，也保守拒绝。现有 live 只读检查的 judge 从 `Case.stdout` 的完整原始 stdout 调用 `adapter::replay_steps` 重放；该重放没有 frame limit 或字符串归约，所以在线 `adapter.item` 的截断标记不会到达该 judge。当前 TEST-027-06 的判定器用例是直接注入标记，验证其 fail-closed 防线；live adapter 的有界 item 与 raw replay 的完整事实是不同路径。本次不把 raw 重放引入在线适配器作为自动放行路径。
6. AC-027-03 与 AC-027-05 不变，CR-040 的跳过名单、256 MiB、LF 与诊断保证不变。未知超限帧仍使 attempt FAILED/PROTOCOL_FRAME_TOO_LARGE。不提高 1 MiB、没有 mock 替代 live、没有更改检查计划/门禁。

**M3R-503：块间让出。** 普通成帧、CR-040 跳过和本 CR 的归约在保存非终结读块的状态后显式 `yield_now()`，不依赖 reader 的协作预算。一次 `next()` 的 poll 最多新增一次读取后返回 Pending 或结果；skip/reduce 每次读≤64 KiB，普通路径保留容量复用，单次读≤1 MiB+64 KiB。转入 skip 还可处理此前缓冲的≤1 MiB+64 KiB，归约终结校验仍限于512 KiB。丢弃 future 后保留字节、哈希、待定 CR 及 UTF-8/转义状态，续读不改变帧结果、哈希或预算。当前生产管道与 ChildStdout 都经 PollEvented 消耗 Tokio 的128次预算（原有纯 skip/reduce 单次 poll 最多读8 MiB），但耗尽预算也会延后协作式 abandon/watch，不能代替显式让出。测试覆盖无协作 reader 的前置/块中取消、仅剩1次预算时至多2次读即观察到 abandon，以及三条路径的丢弃后续读。这是工作量界，不是毫秒承诺；同步 raw-log 写入和调度仍影响时间。调查与红绿记录见 `.hoh-dev/self-tests/m3cxfrm/m3r503-*`。

## AC 与测试

- AC-024-05 / TEST-024-05（integration）：resume 实际发送 excludeTurns=true；0.166 schema 请求符合；旧 schema 删除字段后不能通过；新建请求不加此字段；恢复仍正确关联原 thread。
- AC-027-06 / TEST-027-06（integration）：App Server commandExecution、fileChange 与 exec command_execution 的 1.5 MiB 内容在 LF 后正常完成 attempt，所需字段与截断对象保留，长度/hash 正确、raw 不变，reader 峰值 ≤1 MiB+64 KiB；未知帧、无 LF、无效尾部、超预算仍拒绝；只读判定器面对有截断的判定事实不得 PASS。

测试名 `test_024_05_*` / `test_027_06_*`，先实际红灯，再实现并运行相关回归。fixture 为 mock 协议输入，执行的是产品 reader/adapter；均为自测，非验收。

## 受锁文件与验证

修改 requirements.json 与 acceptance-scenarios.json，生成 docs/02、docs/18；development-tasks.json 的现有显式 AC 列表补入对应 AC（不推进状态）。docs/13 更新规则。本分支不 relock，CR 保持 DRAFT；仅在本任务创建并登记的 `/home/ai/dev/m3cxfrm-*` scratch 副本中临时 APPROVED、relock 并运行 bundle，记录退出码后删除副本。主分支/其他 worktree 不改。

`bash scripts/dev/pre-pr.sh` 全量前台执行；DRAFT 的本分支 baseline mismatch 如实保留，不通过修改基线消除。完整 scratch 检查和分支检查明确区分。独立 QA/Review 与 CR 批准由指挥安排。

## 是否属于 CR-003 D1

是，理由：(1) 修正规格之间的不一致——REQ-024 要求 Codex 恢复原会话，REQ-027 的 1 MiB 帧上限却使正常的 resume 响应（整个 `Thread.turns`）和正常命令的大输出让 attempt FAILED；(2) 不删除需求或 AC，新增 AC-024-05、AC-027-06；(3) 不放宽门禁、检查或证据规则：1 MiB 上限、AC-027-03/05 及其检查、未知超限帧的失败、缓冲界与原始日志都不变，归约只适用于确切前缀认出的已知 item，截断事实不能让只读判定 PASS，旧 schema 仍为 COMPAT_UNVERIFIED；(4) 不改变 CONTRACT 的架构与交互决定。

## 审查记录

| 轮 | 冻结提交 | 审查者 | 结论 |
|---|---|---|---|
| 1 | `62e8d689` | Claude Code 2.1.331（claude-opus-5-5），会话 `6a63d007-c483-4be3-bc7f-34a305d69f80`（attempt `39bdac40-bdab-4f1f-8123-9367edc06772`），编号 M3CX-001..499：协议与帧语义 | PASS；M3CX-001/002 medium，003/005 low，004 文档 |
| 1 | `62e8d689` | Grok 1.0.92（grok-4.7），会话 `01a214c5-34e5-74f1-9a01-720456c522a8`（attempt `9ce90b73-aaaf-4140-96fd-da8383999765`），编号 M3CX-500..999：并发、取消与进程生命周期 | PASS；M3CX-500 medium（与 M3R-503 同类，先写 DC-M3CX-500） |
| 2（修复 diff `62e8d689..0e742bef`） | `0e742bef` | Claude Code 2.1.331（claude-opus-5-5），会话 `963b5d08-f566-4fc3-9d61-3664151abb8a`（attempt `93b6fc46-0b6d-45fa-83ce-79328d3de033`），编号 M3CX-600..699 | PASS；M3CX-001..005 关闭；M3CX-600、601 low（已修） |
| 2（修复 diff `62e8d689..0e742bef`） | `0e742bef` | Grok 1.0.92（grok-4.7），会话 `01a2156e-b53f-7f62-8c08-9e3a5dfbdab5`（attempt `502cfd1c-64c6-48a8-b35d-f582b266e2dd`），编号 M3CX-700..799 | PASS；M3CX-500 关闭；M3CX-700 medium 在修复 diff 之外（既有的 `git_run_limited` 无期限 join），按 docs/19 记入待办 |

全部记录见 `reports/reviews/M3-cleanup.md` 的 CR-041 一节。
