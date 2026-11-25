# CR-040 · 跳过适配器不用的已知超限帧

ID：CR-040
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权（两个互补的独立 Review 会话见审查记录）
日期：2026-11-25
关联：M3L-100（high，产品，lead 在 M3-T05 QA 中发现），REQ-026、REQ-027（行为 7、失败模式 4、AC-027-03），docs/13-adapters.md“进程与终结”

## 问题

pi 1.6.0 的 `--mode rpc` 把每个会话事件写成一行 JSON（`rpc-mode.js`：`session.subscribe(event => output(toJsonEvent(event)))`），pi-agent-core 在一次运行结束时发出 `{type:"agent_end", messages: newMessages}`（`agent-loop.js:157/187/212`），带着这次运行的全部消息；`agent-session.js:816` 再附上 `willRetry`。所以 `agent_end` 是累积记录，运行越长越大。M3-T05 的 QA pi 会话（attempt `1854d61f-13c1-46a2-8799-71a208f3dbce`，`--mode json`，同一 `toJsonEvent` 路径）第 71333 行的 `agent_end` 为 1,399,850 字节，其余各行都不超过 37,508 字节。

产品的 `FrameReader` 拒绝超过 `FRAME_LIMIT`（1 MiB）的帧，pi.rs 收到 `Frame::TooLarge` 即硬停进程并以 PROTOCOL_FRAME_TOO_LARGE 使 attempt FAILED。适配器从不读 `agent_end` 的 `messages`（只看类型：结束一次运行，在没有 `agent_settled` 的版本上触发空闲查询）。于是一个已经完成工作的长 pi attempt 被判为失败，而 REQ-026 要求的正是这样的 turn 以 `agent_settled` 结束为 SETTLED。

因果链：长运行 → `agent_end` 累积全部消息 → 一行超过 1 MiB → reader 拒绝 → 适配器硬停、FAILED → 已完成的工作被记为失败并可能被重试。

## 调查：长而成功的会话中可能超过 1 MiB 的记录

只读调查已安装的 CLI（pi 1.6.0 的发行包 dist 文件，claude 2.1.331 的打包源码，codex-cli 0.171.0 的 `app-server generate-json-schema`/`generate-ts` 输出）与仓库里录制的协议样例（pi 1.5.0/1.6.0、claude 2.1.317/2.1.318、codex 0.166.0），未启动任何 live 会话。调查笔记：`.hoh-dev/self-tests/m3pifrm/survey.md`。

| 适配器 | 记录（行首字节） | 可否超过 1 MiB | 适配器是否需要其载荷 | 本 CR |
|---|---|---|---|---|
| pi | `{"type":"agent_end","messages":` | 是：本次运行的全部消息（实测 1,399,850 字节） | 否：只用类型 | 跳过 |
| pi | `{"type":"turn_end","message":` | 是：带本轮全部 `toolResults`；读图片的工具结果是 base64，上限 4.5 MB（`image-resize-core.js:9`） | 否：只用类型（无 `agent_settled` 的版本作“工作结束”） | 跳过 |
| pi | `{"type":"tool_execution_end","toolCallId":` | 是：带工具的完整 `result`（同上，图片） | 否：只用类型（同上） | 跳过 |
| pi | `{"type":"message_start","message":{"role":"toolResult",`、`{"type":"message_end","message":{"role":"toolResult",` | 是：工具结果消息（同上，图片） | 否：只读 assistant 的 `message_end`；toolResult 只用类型 | 跳过 |
| pi | assistant 的 `message_end` | 不太可能：受模型输出 token 上限约束，未见 | 是：回复文本、`stopReason`、`errorMessage` | 不跳过 |
| claude | `{"type":"user",` | 是：工具结果写两次（`message.content` 与 `tool_use_result`）；图片 base64 上限 5,242,880；Read 不限长；Write/Edit 的 `originalFile` 是整个文件（超过 10 MiB 才省略） | 否：claude.rs 不读 `user` 的任何字段 | 跳过 |
| claude | `assistant` | 不太可能：单条消息上限 128,000 输出 token | 是：`error`、`api_error*`、`message.model`、`parent_tool_use_id` | 不跳过 |
| claude | `result` | 不太可能 | 是：`subtype`、`is_error`、`result`、`usage` 等；且行首不是 `type`（`{"duration_api_ms":`） | 不跳过 |
| codex App Server | `{"method":"turn/diff/updated","params":` | 是：本 turn 的累积 diff | 否：只做已知帧登记 | 跳过 |
| codex App Server | `item/completed` 的 commandExecution（`aggregatedOutput`）与 fileChange（`changes`） | 是：schema 未声明上限；录得单个 `cat` 293,459 字节；delete 的 diff 是整个文件 | 是：复制进 `adapter.item` 供只读判定（ADR-032 设计修正 7） | 不跳过（范围外 1） |
| codex App Server | `thread/resume` 的响应（`{"id":N,"result":{"thread":{"id":"`） | 是：`Thread.turns` 在 resume 时带全部历史；HoH 未发 `excludeTurns` | 是：`thread.id`、`model`、`sandbox`，位于 `turns` 之后 | 不跳过（范围外 2） |
| codex exec | `item.completed` 的 command_execution | 是：`aggregated_output` | 是：`adapter.item` 记 `itemId`、`itemType`（`id` 在 `type` 之前） | 不跳过（范围外 1） |
| 三者 | 其余记录（init、限流、增量、响应等） | 否 | — | 不跳过 |

## 决定

1. **已知超限帧的跳过（reader）。** `FrameReader` 可带一条跳过规则 `Skip { known, max_length }`（`crate::frame`）：`known` 只凭帧的前 4 KiB（`HEAD_BYTES`）判断它是否为某个适配器不需要其载荷的已知记录，给出该记录的名字或不认识。帧超过上限时：
   - 规则不认识（或没有规则）：照旧拒绝，`Frame::TooLarge`（长度、前缀 sha256、前 4 KiB），之后不再成帧。AC-027-03 的全部保证不变。
   - 规则认识：reader 丢弃已缓冲的字节，此后每次只读一个读块、只在其中找 LF，边读边累计长度与 sha256，不保留帧的字节；读到 LF 后给出 `Frame::Skipped { kind, length, sha256, head }`（长度与 sha256 不含 LF 及其前一个 CR，CR 与 LF 分在两次读中也一样），并从 LF 之后继续正常成帧。整个过程缓冲仍不超过上限加一个读块。
   - 规则认识、但流在 LF 之前结束（帧被截断），或帧长超过 `max_length`（默认 `SKIP_LIMIT` = 256 MiB）：拒绝，`Frame::TooLarge`，长度与前缀 sha256 覆盖已读的全部字节，前 4 KiB 为帧首。被截断的记录不算已知记录。

   规则只按确切的字节前缀判断（各 CLI 用 `JSON.stringify`/serde 输出，无空白，键序固定，前缀经录制样例核对）。前缀不符（另一个类型、只是以已知类型名开头、有空白、键序不同）一律不认识，按上限拒绝。
2. **白名单（适配器）。** 由调查得出，每项都是适配器不读其载荷的记录：
   - pi：`agent_end`、`turn_end`、`tool_execution_end`、role 为 `toolResult` 的 `message_start` 与 `message_end`。
   - claude：`user`。
   - codex App Server：`turn/diff/updated`。codex exec 没有白名单。
3. **适配器对跳过帧的处理。** 每个被跳过的帧记一条 `adapter.diagnostic`：`reason` 为 `known frame over the limit skipped`，含 `type`（规则给的名字）、`length`、`sha256`（整帧）与 `head`（前 4 KiB）。帧照常进入原始 stdout 日志（reader 之前的 Tee），可从原始日志取回。pi 对跳过帧按其类型继续判定，与收到只含类型（message_* 另含 `role: "toolResult"`）的记录相同：`agent_end` 结束一次运行并在没有 `agent_settled` 的版本上查询空闲，其余在这类版本上记为“工作结束”。claude 与 codex 的白名单记录本来就只做已知帧登记，跳过后只记诊断。被跳过的帧不送给 turn 的事件订阅者，不做未知字段检查（白名单只含该版本登记表里的已知类型）。
4. **不变的上限与失败。** 帧上限仍为 1 MiB，读块仍为 64 KiB；未在白名单的超限帧、无 LF 的超限帧照旧使 attempt FAILED（PROTOCOL_FRAME_TOO_LARGE），其他 attempt 与 host 不受影响。回放（兼容套件、`replay_facts`）本来就不按上限拒绝，不变。

**考虑过的方案。** lead 建议的方向是 reader 先报告带帧首的超限帧，再由适配器请求丢弃其余部分。本 CR 采用同一机制，只是把“适配器的请求”前移为启动时交给 reader 的规则：reader 在自己的任务里运行、经有界队列把帧交给适配器，往返请求需要另一条通道并让 reader 停下等待；而适配器的判断只依赖帧首，是纯函数，事先给出与事后请求结果相同，没有等待与竞态。提高上限或按类型放开上限都不采用：前者使 AC-027-03 的界变大，后者仍会被足够长的累积记录超过。

## 受锁文件与实现影响

- `contracts/requirements.json` REQ-027：
  - 行为 7 改为：“协议帧超过1 MiB（初值）必须有界失败：缓冲不超过上限加一次读块，保存诊断并使该 attempt FAILED（PROTOCOL_FRAME_TOO_LARGE）。（CR-040）例外只有一种：适配器凭帧的前4 KiB 按确切字节前缀认出的已知记录、且适配器不需要其载荷（白名单：pi 的 agent_end、turn_end、tool_execution_end、toolResult 的 message_start/message_end，claude 的 user，codex App Server 的 turn/diff/updated），读到其 LF 时跳过：边读边丢弃，缓冲同样不超过上限加一次读块，记 adapter.diagnostic（类型、长度、整帧 sha256、前4 KiB），之后继续成帧；被认出但在 LF 前流结束或超过 256 MiB 的帧仍按上限失败。”
  - 失败模式追加：“（CR-040）当长会话的 pi 发出超过1 MiB 的 agent_end 等白名单记录并以 agent_settled 结束时，attempt SETTLED，不因帧长失败；帧首不符白名单的超限帧照旧 FAILED。”
  - acceptance 追加 AC-027-05（TEST-027-05，检查 `integration`）：Given pi fixture：正常帧之后 1 条 1.5 MiB 的 `agent_end`（键序同 pi：`{"type":"agent_end","messages":[…],"willRetry":false}`），其后 `agent_settled`；另备同长度但帧首不在白名单的一行、在 LF 之前流就结束的同一 `agent_end`；帧上限为默认 1 MiB。When 分别运行 attempt 并通过测试探针记录 reader 最大缓冲字节。Then 白名单帧的 attempt=SETTLED，恰 1 条跳过诊断（含类型 agent_end、帧长度、整帧 sha256 与前 4 KiB）；最大缓冲 ≤ 1 MiB+64 KiB；帧首不符与被截断的两份 attempt=FAILED、原因 PROTOCOL_FRAME_TOO_LARGE。
- `contracts/acceptance-scenarios.json`：在 TEST-027-04 之后追加 TEST-027-05。
- `docs/02-functional-spec.md`、`docs/18-traceability.md`：由 `tools/gen_spec_views.py` 生成。
- 不受锁：`docs/13-adapters.md`“进程与终结”加一句跳过已知超限帧的规则；`contracts/development-tasks.json` 中 AC-027-05 归 M1-T07（“跨适配器协议一致性（已知/未知帧登记、1 MiB 帧上限）”，其 `acceptance` 显式列表追加 AC-027-05；integration 检查由其祖先交付，任务图规则不变）。该任务已 done，本 CR 的修复提交是它的补充证据；lead 可另行改归。

实现：`crates/hoh-agents/src/frame.rs`（`Skip`、`Frame::Skipped`、跳过状态），`process.rs`（`Config::skip`），`pi.rs`、`claude.rs`、`codex/app_server.rs`（白名单规则与对 `Frame::Skipped` 的处理），`codex/exec.rs`（无规则；对 `Frame::Skipped` 的分支只记诊断）。

测试（先红）：`frame.rs` 单元测试 `test_027_05_*`（跳过到 LF 且有界、CR/LF 跨读、流末无 LF 被拒、帧首不符被拒、超过 `max_length` 被拒、有规则时 8 MiB 无 LF 照旧被拒）；`tests/pi_adapter.rs` 的 `test_027_05_*`（1.5 MiB `agent_end` 后 `agent_settled` → SETTLED 且有界；没有 `agent_settled` 的版本经空闲查询 SETTLED；分多次写且以 CRLF 结尾；帧首不符 → FAILED；被截断 → FAILED）；`tests/claude_adapter.rs` 与 `tests/codex_adapter.rs` 各 1 个 `test_027_05_*`（1.5 MiB 的白名单记录 claude `user`、codex `turn/diff/updated` → SETTLED 且恰 1 条跳过诊断；同样长的非白名单记录 claude `assistant`、codex `item/agentMessage/delta` → FAILED）。测试用的 fake pi 增加伪记录 `hoh_raw`（按原样、分若干次写出一段文本，保留 pi 的键序）。AC-027-03 的现有测试不改。

## 范围外发现（不由本 CR 处理）

1. codex 的命令输出与文件改动：App Server 的 `item/completed`（commandExecution 的 `aggregatedOutput`、fileChange 的 `changes`）与 exec 的 `item.completed`（`aggregated_output`）可以超过 1 MiB，而适配器需要其内容或 id，不能跳过。一个输出超过 1 MiB 的命令（如 `cat` 大文件）仍会使 codex attempt FAILED。需要另一个设计（例如适配器只保留有界前缀的流式解析），建议单列 issue。
2. codex `thread/resume` 的响应带全部历史（`Thread.turns`），长会话续接时可超过 1 MiB，而适配器需要其中的 `thread.id`、`model`、`sandbox`。codex 自己提示整段历史的回填已弃用、应发 `excludeTurns: true`；0.166 与 0.171 的 `ThreadResumeParams` 都有该字段。建议单列 issue：在 `thread/resume` 请求中发 `excludeTurns: true`（请求符合性由兼容套件按被测 schema 核对）。
3. claude 的 assistant 与 pi 的 assistant `message_end` 理论上可超过 1 MiB（输出 token 上限 128k），适配器需要它们，未见实例，不处理。

## 不变的部分

帧上限 1 MiB、读块 64 KiB、AC-027-01..04 及其场景、PROTOCOL_FRAME_TOO_LARGE 的原因码与诊断字段、host 与其他 attempt 的隔离、回放的成帧方式、检查计划、发布策略、门禁与阈值、CONTRACT 的架构与交互决定都不变。

## 是否属于 CR-003 D1

是，理由：(1) 修正规格之间的不一致——REQ-026 要求以 `agent_settled` 结束的 pi turn 为 SETTLED，而 REQ-027 行为 7 使同一 turn 因一条适配器不读的累积记录而 FAILED；(2) 不删除需求或 AC，新增 AC-027-05；(3) 不放宽门禁、检查或证据规则：AC-027-03 及其检查不变，未知超限帧的失败与缓冲界不变，被跳过的帧仍有诊断且原始字节保留；(4) 不改变 CONTRACT 的架构与交互决定。若审查认为行为 7 的例外属于放宽产品行为，则按 docs/21 交项目所有者批准。

## 验证方式

先红：`cargo test -p hoh-agents --lib frame::tests` 与 `cargo test -p hoh-agents --test pi_adapter test_027_05`（以及 claude/codex 的 `test_027_05`）在实现前失败，实现后通过；AC-027-03 的现有测试保持通过。之后 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`bash scripts/dev/pre-pr.sh`。规格：`python3 tools/gen_spec_views.py`；委派开发者保留 DRAFT、不在分支上重锁，只在 scratch 副本中把本 CR 设为 APPROVED 并 `python3 tools/baseline.py relock --cr docs/cr/CR-040-skip-known-oversized-frames.md`，再 `bash scripts/check.sh --bundle`，验证批准后 bundle 可以通过，这不代表 CR 已批准。

## 审查记录

| 轮 | 冻结提交 | 审查者 | 结论 |
|---|---|---|---|
| 1 | `78542055` | Codex codex-cli 0.171.0，会话 `01a2137e-b534-7370-8c7f-5e65c57dc4cd`（attempt `e37f686d-41d9-4469-8928-d0d7dd2c22a5`），编号 M3PF-001..049：读取器状态机、缓冲界、前缀匹配 | PASS，无发现（独立探针 2,716 个边界组合；5 个新适配器测试在基线上失败、在候选上通过） |
| 1 | `78542055` | Grok grok 1.0.92，会话 `01a2137e-c013-7173-a55f-ecb55d019696`（attempt `c3817e09-869b-4f0f-8714-5316aefed9ba`），编号 M3PF-500..549：跳过后的适配器语义、是否掩盖失败、D1 适用性 | PASS，无发现 |

全部记录见 `reports/qa/M3.md` 的 M3L-100 一节。
