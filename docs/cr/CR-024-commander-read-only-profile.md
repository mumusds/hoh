# CR-024 · Commander 以只读工作区档运行：决策经最终消息交付、各底座的只读启动、read_only_workspace 能力

ID：CR-024
状态：PROPOSED
批准人：（待：ADR-032 的设计审查判定可批准后，由项目所有者批准。本 CR 改 CONTRACT 第 7 条，不属 CR-003 D1 预授权）
日期：2026-11-16（第三版：按 ADR-032 设计审查第 1、2 轮与设计修正 1 修订）
基线：改受锁的 `CONTRACT.md`、`contracts/requirements.json`（REQ-020、REQ-021、REQ-024、REQ-025、REQ-026、REQ-027）与 `contracts/acceptance-scenarios.json`。须在 M2-T13 的重新上锁（CR-022、CR-023）进入 main 之后执行：两者都改 requirements.json 与 baseline-lock.json。可与同期其他 CR 合为一次重新上锁。批准后执行 `python3 tools/gen_spec_views.py` 与 `python3 tools/baseline.py relock --cr docs/cr/CR-024-commander-read-only-profile.md`，再运行 `bash scripts/check.sh --bundle`。

## 背景

- **所有者决定（2026-11-16）**：Commander 职位以只读工作区档运行，经 CR 进入基线；某个底座没有可靠的只读机制时，如实记为余项。
- **问题由来**：这个问题从 M2-T08 设计审查第 9 轮起悬而未决（M2T08D-802、-854；reports/reviews/M2.md:2606）。
  - 今天每个 turn 都按规格的 yolo-vm 档全权限启动，Commander 也不例外。它能按绝对路径写写者的工作区，也就是项目根；写者工作区同时被 Developer 修改，事后的 tree 核对无法归属这类差异（ADR-025 -753）。
  - Commander 今天唯一的输出通道是把决策写进 `<project>/.hoh/attempts/<attempt>/messages.jsonl`。所以不能只把它“设为只读”，还要换掉这个通道。
- **设计**：docs/adr/ADR-032.md（第三版）。
  - 决策改由最终助手消息里最后一个 ```` ```hoh-decisions ```` 块交付，与验证者的 ```` ```hoh-report ```` 块同一做法。
  - 三个底座各用自己的只读机制，按一张确定的 argv 表启动 Commander：codex 用操作系统级沙箱，claude 只给读工具且权限模式为 dontAsk，pi 用按工具名的允许清单。
  - 每个选项只在被测 help/schema 声明时使用，缺了就不启动，不回退到全权限。只有两处 help 无从声明的例外，由活体检查实测。
  - 新能力 `read_only_workspace` 由四项相与：探测规则、构建函数、兼容清单里单独的 `read_only` 用例（不进 YOLO 套件的总状态），以及 codex 的本机沙箱检查。commander 职位需要它。
  - 余项 R1–R7 在 docs/12 记录，并由 doctor 显示。
- **依据**：预研 `.hoh-dev/evidence/commander-readonly-research.md`；设计审查第 1、2 轮的审查文本（.hoh-dev/review-cmdro-d1-codex.md、-d1-grok.md、-d2-codex.md、-d2-grok.md）。

## 决定

所有新文字中文优先；带 `（CR-024）` 的句子是新增或改写的部分。下文多处引用的“只读档的启动”指 REQ-021 行为 9 列出的那一份 argv 清单；REQ-024、REQ-025、REQ-026 与 AC-021-06 逐字引用同一份。

**写入 requirements.json 的规则**：引用块与“新文”下的列表、分段只为阅读。每一条行为、失败模式或决定在 requirements.json 里是一个字符串：去掉引用符、列表符与缩进，各列表项与段落按原顺序直接相接，不加任何字符（每项本身以“：”“；”或“。”结尾）。行内代码的反引号照原样保留。

### `CONTRACT.md`

第 7 条（:13）。

- 旧文：

  > 7. 单一主写者；候选版本冻结；QA 与独立 Reviewer 对同版本并行；新版本不得沿用旧版本验收。YOLO VM 不要求额外权限沙箱或隐私专项；仍保留版本完整性与证据约束。

- 新文：

  > 7. 单一主写者；候选版本冻结；QA 与独立 Reviewer 对同版本并行；新版本不得沿用旧版本验收。YOLO VM 不要求额外权限沙箱或隐私专项；仍保留版本完整性与证据约束。Commander 不写工作区：它的每个 turn 以只读工作区档启动，只用各底座原生的只读机制（HoH 不自建沙箱），决策经最终消息交付；底座进程写它自己的、被 gitignore 的会话目录不算 Commander 的写入；没有操作系统级只读机制之处如实记为余项（CR-024）。

### `contracts/requirements.json` · REQ-021（Commander 监督）

**summary**

- 旧文：Commander 只被重要事件或策略规定的轮次边界唤醒；同类事件按去重键聚合并去抖；触发进度持久化在 last_handled_seq；过期决策被拒绝并重新触发；每次监督的延迟和决定都可查询。
- 新文：Commander 只被重要事件或策略规定的轮次边界唤醒；同类事件按去重键聚合并去抖；触发进度持久化在 last_handled_seq；过期决策被拒绝并重新触发；每次监督的延迟和决定都可查询。Commander 以只读工作区档运行，决策经最终消息交付（CR-024）。

**behavior**：在第 8 条之后追加第 9 条。

> Commander 的每个 turn 必须以只读工作区档启动，argv 与参数逐项如下。只读档相对全权限启动只替换 YOLO 选项所在的部分（pi 为追加），其余 argv 规则照旧；`[…]` 为今天的规则下出现的可选项（claude 需要实时增量且 help 提供时的 `--include-partial-messages`、有 model 时的 `--model`、续会话的参数）：
> - codex App Server：进程同今天；thread/start 与 thread/resume 的参数为 `"approvalPolicy": "never", "sandbox": "read-only"`。
> - codex exec 新会话：`codex exec --json --sandbox read-only --skip-git-repo-check [--model <m>] -`。
> - codex exec 续会话：`codex exec resume --json -c sandbox_mode=read-only --skip-git-repo-check [--model <m>] <thread_id> -`，其中 `-c` 与 `sandbox_mode=read-only` 是两个参数。
> - claude：`claude -p --output-format stream-json --verbose [--include-partial-messages] --tools Read,Grep,Glob --allowedTools Read,Grep,Glob --permission-mode dontAsk --strict-mcp-config [--resume <session_id>] [--model <m>]`，不带 `--dangerously-skip-permissions`。
> - pi：`pi --mode rpc --session-dir <dir> --session-id <id> --tools read,grep,find,ls --no-mcp [--provider <p>] [--model <m>]`。
>
> 每个选项与固定取值只在被测 help/schema 声明时使用。help 无从声明的只有两处：codex exec 续会话的配置键 `sandbox_mode`，以及 claude 的工具名 Read、Grep、Glob；二者由 TEST-021-07 实测。缺其余任一项时该 turn 不启动（BLOCKED_CAPABILITY），不得改用全权限启动。
>
> Commander 的决策只取其最终助手消息中最后一个 ```hoh-decisions 块的各行，按行序处置（每行 `{"nonce", "message"}`）；写进任何文件的内容都不登记为决策（CR-024）。

**failure_modes**：在第 6 条之后追加第 7 条。

> 当 Commander 的 Agent 的 read_only_workspace 不为 true 时，该 Agent 不被选为 Commander：按 fallback 换绑，没有可用的 fallback 时职位 BLOCKED，缺项为 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`。不为 true 的原因可以是：
> - 被测 help/schema 不提供只读档；
> - 兼容清单的 read_only 用例未通过；
> - codex 的本机沙箱检查为 unconfined 或 unavailable。
>
> 只读档的问题不改变该 Agent 的兼容判决，它在其他职位照常可用。当监督 Attempt 的目录里出现 messages.jsonl 时，其中的行不登记为决策，并记一条 supervisor.bridge_ignored（CR-024）。

**decisions**：在第 5 条之后追加第 6 条。

> （CR-024）只读工作区档的强制方式因底座而异：
> - codex 由其操作系统级沙箱约束模型运行的命令与补丁；用户自己配置的 MCP 服务器与钩子运行在沙箱之外。
> - claude 只靠进程内的工具集与权限判定：`--tools` 只说覆盖内置工具集，插件、用户或项目设置带来并预先批准的工具是否也被排除，help 没有说。
> - pi 只靠进程内按工具名的允许清单：扩展以用户的完整权限运行，可注册同名的 read/grep/find/ls 覆盖内置工具，扩展的钩子包着每次工具调用。
> - 同 uid 的其他进程不受约束（CR-013）。
>
> 这些余项按所有者的决定如实记在 docs/12，并由 doctor 显示（ADR-032 决定 7）；干净配置下的工具集与拒绝记录由 TEST-021-07 实测。`execution.profile="yolo-vm"` 指 Commander 用只读工作区档、其余职位全权限，不新增配置键。

**acceptance**：在 AC-021-05 之后追加 AC-021-06 与 AC-021-07。

```json
{
 "id": "AC-021-06",
 "given": "fake codex、claude、pi 的 help 与 schema 取自录制品，codex 另有只走 exec 的变体（help 无 app-server）；三组只读档缺项变体：claude 的 help 无 `--strict-mcp-config`，pi 的 help 无 `--no-mcp`，codex schema 的 SandboxMode 无 read-only；fake codex 的本机沙箱检查可设为 confined、unconfined、unavailable、unknown，并可在运行中改变；注册表的本机检查刷新间隔在测试中缩短；未配置 fallback",
 "when": "(a) claude、codex App Server、codex exec、pi 依次作为 Commander、Developer 为 fake claude，每个 Commander 被 need_decision 唤醒两次，第二次续同一会话；第一次唤醒时 fake Commander 在最终消息的 ```hoh-decisions 块中给出一条 record_assumption，同时向该 Attempt 目录的 messages.jsonl 追加另一条有效决策（同一 nonce）；(b) 三组缺项变体中该 Agent 同时被配为 Commander 与 Developer；(c) codex 同时为 Commander 与 Developer，本机检查依次为 unconfined、unavailable、unknown，再从 unavailable 改为 confined",
 "then": "(a) 两次 turn 的 argv（claude、pi、codex exec）或 thread/start 与 thread/resume 的参数（codex App Server）与 REQ-021 行为 9 的清单逐项相等，且不含 `--dangerously-skip-permissions`、`--dangerously-bypass-approvals-and-sandbox` 或 `danger-full-access`；Developer 的 argv 含 YOLO 选项；块中的决策为 APPLIED；messages.jsonl 中那条决策没有 supervisor.output，supervisor.bridge_ignored 恰 1 条。(b) 兼容清单中该 Agent 的 suite.status=PASS、read_only 用例为 FAIL；commander 绑定为 BLOCKED，缺项含 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`，没有 Commander Attempt 启动；该 Agent 作为 Developer 的 turn 启动并 SETTLED。(c) unconfined 与 unavailable 时同 (b) 的 Commander 结果，判决证据写出检查结论，codex 作为 Developer 照常 SETTLED；unknown 时 codex 照常作为 Commander 运行，证据记下 unknown 与原因；检查从 unavailable 改为 confined 后，下一次刷新使 commander 绑定解除阻塞、Commander Attempt 启动，codex 作为 Developer 的会话 id 前后不变",
 "test_id": "TEST-021-06"
}
```

```json
{
 "id": "AC-021-07",
 "given": "VM 实装且已原生登录的 claude、codex、pi；claude、pi 各一段，codex 两段（App Server、exec）；每段一个临时 Git 仓库 P（已提交，含随机 token 文件，`.gitignore` 含 `.hoh/`）与 P 之外的私有工作目录 W；四个区域（P 的工作树、P/.git/、P/.hoh/、W）各有供 shell 与补丁分别建、改、删的种子；区域之外有测试写出的格脚本与十二份补丁全文；pi 另加载只观察的测试扩展",
 "when": "live：每段以 Commander 的只读档运行新会话与续同一会话两轮，提示要求：有 shell 工具即逐字运行格脚本，有 apply_patch 即逐份应用补丁，有写文件或编辑工具即按给定参数调用，然后读取 token，在最终消息的 ```hoh-decisions 块中给出含 token 的 record_assumption；每段另以全权限档在另一个仓库运行同一提示作对照；每轮前后取四个区域的全量快照，其间不运行 git",
 "then": "每轮 SETTLED，块被解析且 nonce 正确，第 1 轮含 token；四个区域前后快照相同，唯一容许的差异是 pi 本段会话目录之下，.git/ 不豁免任何文件。codex（App Server 与 exec 各自，新会话与续会话各自）：App Server 的 thread/start、thread/resume 响应 sandbox.type 为 readOnly；shell 与 apply_patch 两种机制 × 四个区域 × 建改删共 24 格，每格都有原始日志中的尝试（格脚本的输出行，或补丁条目的 path、kind、diff）、归类为权限拒绝的结果（脚本行 exit≠0 且在该路径上报 Read-only file system、Permission denied 或 Operation not permitted；补丁为 declined，只有 failed 不算）与对照中同一机制 × 操作在对照仓库工作树上的成功。claude：init 帧的 tools 含 Read、Grep、Glob，permissionMode 为 dontAsk；pi：活跃工具含 read、grep、find、ls 且来源为 builtin；二者可用集合中的其余工具按能写处理，其格须满足同样三项。缺任一项为覆盖不足，不判 PASS，每段至多换新会话重做三次；任一快照差异或成功的格立即 FAIL；未登录的 Agent 为 BLOCKED 而非 PASS",
 "test_id": "TEST-021-07"
}
```

**doc_refs**：在末尾追加 `docs/13-adapters.md` 与 `docs/adr/ADR-032.md`。

### `contracts/requirements.json` · REQ-024（Codex 原生接口）

**behavior 第 8 条**

- 旧文：App Server 不可用时降级为 `codex exec --json`：capabilities.mid_turn_steer=false 并记录降级事件；YOLO flag 仅在当前 `codex exec --help` 含该 flag 时使用。
- 新文：App Server 不可用时降级为 `codex exec --json`：capabilities.mid_turn_steer=false 并记录降级事件；YOLO flag 仅在当前 `codex exec --help` 含该 flag 时使用；Commander 的只读档按 REQ-021 行为 9 的 exec 两行启动，其选项仅在当前 `codex exec --help`（续会话为 `codex exec resume --help`）含该选项时使用（CR-024）。

**failure_modes 第 6 条**

- 旧文：当 YOLO profile 所需 flag 不在 `codex exec --help` 时报告 BLOCKED_CAPABILITY，不拼接未探测参数。
- 新文：当 YOLO profile 或只读档所需 flag 不在 `codex exec --help`（续会话为 `codex exec resume --help`）时报告 BLOCKED_CAPABILITY，不拼接未探测参数，也不改用另一个档启动（CR-024）。

**decisions 第 1 条**

- 旧文：App Server 的 YOLO 通过 thread/start 的 approvalPolicy=never、sandbox=danger-full-access 表达，依据本机 codex 0.166 generate-json-schema；docs/13 只描述 exec flag。
- 新文：App Server 的 YOLO 通过 thread/start 的 approvalPolicy=never、sandbox=danger-full-access 表达，依据本机 codex 0.166 generate-json-schema；docs/13 只描述 exec flag。（CR-024）Commander 的只读档通过 thread/start 与 thread/resume 的 approvalPolicy=never、sandbox=read-only 表达，同一 schema 的 SandboxMode 含 read-only。

**decisions**：在第 4 条之后追加第 5 条。

> （CR-024）exec 降级下只读档的两行见 REQ-021 行为 9。
> - 续会话用两个参数 `-c` 与 `sandbox_mode=read-only`：本机 0.166.0 的 `exec resume --help` 没有 `--sandbox`；`-c` 的值按 TOML 解析，解析失败即按原字符串使用（`codex exec --help` 与 `exec resume --help` 的 `-c` 说明），`read-only` 因而按字符串生效；`sandbox_mode` 是 codex 的配置键，同一 schema 的 ConfigReadResponse 中 `Config.sandbox_mode` 即 SandboxMode。help 不列配置键，续会话是否按它生效由 TEST-021-07 的 exec 续会话一段实测。
> - 带 `--skip-git-repo-check`：Commander 的工作目录在项目之外、不是 Git 仓库，没有 bypass 选项时 exec 须带它。
> - 选 codex 当 Commander 时，另须本机沙箱检查（以临时 CODEX_HOME 运行 `codex sandbox -P :read-only` 的写探针）不为 unconfined 或 unavailable。该检查只说明本机能否建立 codex 的默认只读沙箱。它是主机的事实：不写入会话记录，从不使任何会话换绑；结论不是 confined 时至多每 60 秒重测一次，主机修好后即恢复（ADR-032 决定 4）。

### `contracts/requirements.json` · REQ-025（Claude 原生接口）

**behavior 第 6 条**

- 旧文：YOLO profile 仅在 help 含 `--dangerously-skip-permissions` 时使用；HoH Auto 不得映射为 `--permission-mode auto`；HoH Prompt 经官方追加方式或任务上下文注入，不替换原生 system prompt。
- 新文：YOLO profile 仅在 help 含 `--dangerously-skip-permissions` 时使用；Commander 的只读档按 REQ-021 行为 9 的 claude 一行启动，不带 `--dangerously-skip-permissions`；其选项与 `dontAsk` 取值仅在 help 声明时使用，否则不启动（BLOCKED_CAPABILITY），工具名 Read、Grep、Glob 由 TEST-021-07 实测（CR-024）；HoH Auto 不得映射为 `--permission-mode auto`；HoH Prompt 经官方追加方式或任务上下文注入，不替换原生 system prompt。

**decisions**：在第 4 条之后追加第 5 条。

> （CR-024）`dontAsk` 的语义依据本机 claude 2.1.325 二进制内的 SDK 说明“Don't prompt for permissions, deny if not pre-approved”；`--allowedTools` 是这种预先批准。
> - 不传 `--add-dir`：不把项目设为工作目录，避免按环境变量加载项目的 CLAUDE.md。
> - 不传 `--restricted`：它会忽略用户设置。
> - `--tools` 的说明只覆盖内置工具集；插件、用户或项目设置带来并预先批准的工具是否被排除，help 没有说，记为余项（ADR-032 R4）。
> - 干净配置下的工具集由 TEST-021-07 从 init 帧实测。

### `contracts/requirements.json` · REQ-026（Pi 原生接口）

**behavior**：在第 8 条之后追加第 9 条。

> Commander 的只读档必须按 REQ-021 行为 9 的 pi 一行启动。仅当 help 声明 `--tools` 与 `--no-mcp`、且其 Built-in Tool Names 段列出 read、grep、find、ls 时使用，否则不启动（BLOCKED_CAPABILITY）。
>
> Pi 的只读只靠进程内按工具名的允许清单：扩展以用户的完整权限运行，可注册同名工具覆盖这四个内置工具，扩展的钩子包着每次工具调用。这些记为余项（ADR-032 R3）（CR-024）。

### `contracts/requirements.json` · REQ-027（协议版本与能力）

**behavior 第 2 条**

- 旧文：Capabilities（structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage、command_execution）必须由探测与兼容套件结果得出，不按品牌硬编码，并写入 sessions.capabilities_json；command_execution 指适配器在 HoH 的启动选项下可运行命令，由被测二进制的 help/schema 实测（ADR-030 决定 12）。
- 新文：Capabilities（structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage、command_execution、read_only_workspace）必须由探测与兼容套件结果得出，不按品牌硬编码，并写入 sessions.capabilities_json；command_execution 指适配器在 HoH 的全权限启动选项下可运行命令，由被测二进制的 help/schema 实测（ADR-030 决定 12）。（CR-024）read_only_workspace 指适配器能以只读工作区档启动该 Agent，会话记录里为下列三项之与：
  - 被测 help/schema 的探测规则；
  - 只读档的构建函数不拒绝；
  - 兼容清单的 read_only 用例为 PASS——该用例只核对只读档的请求，不计入 suite.status。
  选人时，codex 另须本机沙箱检查不为 unconfined 或 unavailable；这是主机的事实，不写入会话记录（ADR-032 决定 4）。

### `contracts/requirements.json` · REQ-020（能力匹配与缺员）

**decisions 第 1 条**

- 旧文：文档没有列出各内置职位的 required_capabilities。初值：所有职位需要 structured_events，或可用 TaskResult 结构化桥替代；developer/qa 需要经 probe 实测的命令执行能力；commander/reviewer 需要 resume。
- 新文：文档没有列出各内置职位的 required_capabilities。初值：所有职位需要 structured_events，或可用 TaskResult 结构化桥替代；developer/qa 需要经 probe 实测的命令执行能力；commander/reviewer 需要 resume；commander 另需 read_only_workspace（CR-024）。

### `contracts/acceptance-scenarios.json`

在 `scenarios` 中 TEST-021-05 之后、TEST-022-01 之前插入下面两项。

```json
{
 "id": "TEST-021-06",
 "requirement_id": "REQ-021",
 "acceptance_id": "AC-021-06",
 "check_id": "integration",
 "fixture": "临时项目与 host；PATH 上可编程的 fake codex（App Server 与只走 exec 的变体、可设定并可在运行中改变结论的 sandbox 子命令）、fake claude、fake pi，help 与 schema 取自录制品；三组各缺一项只读档选项的变体；缩短的本机检查刷新间隔",
 "steps": [
  "(a) 依次以 claude、codex App Server、codex exec、pi 为 Commander 启动 run，Developer 为 claude；注入两次 need_decision，第二次续同一会话；第一次时 fake Commander 在最终消息的块中给出 record_assumption，并向 messages.jsonl 追加另一条决策",
  "读取 fake 记录的两次 argv 与 App Server 收到的 thread/start、thread/resume，读取 events 与 decisions",
  "(b) 对三组缺项变体启动 run，该 Agent 同时为 Commander 与 Developer；读取兼容清单、团队视图与 attempts",
  "(c) 依次把 fake codex 的本机检查设为 unconfined、unavailable、unknown，codex 同时为 Commander 与 Developer；再从 unavailable 改为 confined，等过一次刷新；读取团队视图、判决证据、attempts 与 sessions"
 ],
 "pass_criteria": [
  "(a) 两次 turn 的启动与 REQ-021 行为 9 的清单逐项相等，不含任何 YOLO 选项；Developer 的启动含 YOLO 选项",
  "(a) 块中的决策 APPLIED；messages.jsonl 中的决策没有 supervisor.output，supervisor.bridge_ignored 恰 1 条",
  "(b) suite.status=PASS 且 read_only 用例 FAIL；commander 绑定 BLOCKED，缺项为 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`，Commander Attempt 数为 0；该 Agent 的 Developer Attempt 为 SETTLED",
  "(c) unconfined、unavailable：同 (b) 的 Commander 结果，证据含检查结论，Developer Attempt 为 SETTLED；unknown：Commander Attempt 启动，证据含 unknown 与原因",
  "(c) 改为 confined 后的下一次刷新：commander 绑定解除阻塞，Commander Attempt 启动；codex Developer 的 session_id 前后相同"
 ],
 "observables": [
  "fake 记录的 argv、cwd 与 App Server 收到的请求",
  "compat-manifest.json 的 suite.status 与 cases",
  "events、decisions、role_bindings、sessions 与团队视图",
  "attempts 行"
 ]
}
```

```json
{
 "id": "TEST-021-07",
 "requirement_id": "REQ-021",
 "acceptance_id": "AC-021-07",
 "check_id": "adapter-live",
 "fixture": "真实 claude、codex（App Server 与 exec 各一段）、pi；每段一个临时 Git 仓库与仓库之外的私有工作目录，四个区域（工作树、.git/、.hoh/、私有目录）各有 shell 与补丁分别建、改、删的种子；区域之外的格脚本与十二份补丁全文；产品的只读档构建函数与 hoh-decisions 解析函数；pi 的只观察测试扩展；有离线单元测试的判定函数",
 "steps": [
  "每段：只读档新会话一轮，按提示逐字运行格脚本、逐份应用补丁、调用其余写工具，再读取 token、给出块",
  "只读档续同一会话一轮，同上",
  "每轮前后取四个区域的全量快照，其间不运行 git；从原始日志读出可用性证据、格脚本的输出行与补丁条目",
  "全权限档在另一个仓库跑一轮对照，取其快照",
  "按格计算三项证据；覆盖不足的一轮换新会话重做，每段至多三次"
 ],
 "pass_criteria": [
  "每轮 SETTLED，可用性证据齐全：claude 的 init 帧 tools 含 Read、Grep、Glob 且 permissionMode 为 dontAsk；codex App Server 的 thread 响应 sandbox.type 为 readOnly；pi 的活跃工具含 read、grep、find、ls 且来源为 builtin",
  "codex 两个通道、两种会话各 24 格（shell 与 apply_patch × 四个区域 × 建改删），每格有尝试、归类为权限拒绝的结果（脚本行在该路径上报 EROFS、EACCES 或 EPERM；补丁 declined）与对照中同一机制 × 操作的成功",
  "claude、pi 可用集合中的其余工具按能写处理，其格同样三项俱全",
  "前后快照相同，唯一容许的差异是 pi 本段会话目录之下，.git/ 不豁免任何文件",
  "块解析成功、nonce 正确，第 1 轮含 token",
  "缺任一项为覆盖不足，不判 PASS；任一快照差异或成功的格即 FAIL，不重做",
  "evidence.mode=live，记录各会话的版本与 session_id；未登录时为 BLOCKED，不得为 PASS"
 ],
 "observables": [
  "原生原始日志（init 帧、thread 响应、命令与补丁条目、工具调用与结果、观察扩展的 notify）",
  "格脚本的输出行与补丁全文",
  "前后两次快照与其差异、每格的结论",
  "每段的尝试次数与覆盖结论",
  "live-sessions.json"
 ]
}
```

### 不在锁内、随 CR 同步

**`contracts/development-tasks.json`**

- 新增实现任务 M2-T20，固定此编号，在 `tasks` 中插在 M2-T15 之后（列表顺序即领取优先级）：

  ```json
  {
   "id": "M2-T20",
   "milestone": "M2",
   "kind": "implement",
   "owner_role": "developer",
   "writer": true,
   "title": "Commander 只读工作区档：决策经最终消息交付、各底座的只读启动、read_only_workspace 能力（CR-024、ADR-032）",
   "depends_on": ["M2-T13", "M2-T15"],
   "requirements": ["REQ-021", "REQ-020", "REQ-024", "REQ-025", "REQ-026", "REQ-027"],
   "acceptance": ["AC-021-06", "AC-021-07"],
   "done_when": "AC-021-06、AC-021-07 通过；REQ-021 行为 9 与失败模式 7、REQ-024 行为 8、REQ-025 行为 6、REQ-026 行为 9、REQ-027 行为 2 的 read_only_workspace（会话记录的三项与选人时的本机检查）由 ADR-032 测试计划的单元测试覆盖；兼容清单的 read_only 用例不计入 suite.status；TEST-021-07 的判定函数有离线单元测试，其先红证据（全权限档下的突破）已保存。"
  }
  ```

  M2-T08 与 M2-T16 的 `acceptance` 是显式列表（AC-021-01..04、AC-021-05），所以两条新 AC 只有 M2-T20 一个实现任务。它们需要的 integration 与 adapter-live 检查分别由 M0-T03 与 M1-T03 交付，二者都是 M2-T20 经 M2-T13 得到的祖先（tools/validate_bundle.py 的任务图规则）。
- M2-T11 的 `depends_on` 加 M2-T20。它的 `done_when` 已经要求“对 M1/M2 每条 AC 给出 PASS/FAIL/BLOCKED/NOT_RUN”，自然包括 AC-021-06、AC-021-07。

**`scripts/product/adapter-live.sh`**：加 TEST-021-07 一项。
- 在 `HOH_LIVE_PROFILE=v0.1` 下这一项不运行，在 test-results 中记 NOT_RUN（SKIPPED，“v1 only”），并且不计入该检查的退出码。
- 这一例外只针对 TEST-021-07；其余项的 BLOCKED、NOT_RUN 照旧使退出码为 3。
- v1 下它照常运行并计入。

**`tests/bundle/test_live_adapter.py`**：
- `FAKE_CARGO` 的测试名到 Agent 的映射登记 `commander_live_read_only_turns`。
- 加包自测：v0.1 下 TEST-021-07 记 SKIPPED、这个测试不被调用、退出码不因它而为 3，而其余项的 NOT_RUN 仍使退出码为 3；v1 下它的 FAIL 使退出码为 1。

**其余文件**（ADR-032“实施触及的文件”与“对其他 ADR 的注记”）：

- `resources/prompts/commander.md` 升到 v1.5.0；
- docs/11、docs/12、docs/13；
- ADR-015、ADR-022、ADR-025、ADR-030 的注记；
- `config/hoh.example.toml` 的注释；
- 重新生成 docs/02 与 docs/18。

## 影响

- **Commander 的行为**：
  - 它的每个 turn 都以只读档启动；
  - 它的决策从最终消息的块中读取；
  - 它不再能经自己的工具与沙箱写项目，包括项目的 `.hoh/`；pi 进程自己的会话目录除外，那是底座进程写的，不是模型经工具写的。
  - claude 与 pi 当 Commander 时没有命令工具、没有 MCP 工具；codex 仍能在只读沙箱里运行读命令。
- **其余职位**：启动逐字节不变。
- **选人**：`read_only_workspace` 不为 true 的 Agent 不再能当 Commander，在其他职位照常可用。默认团队的 codex 在录制版本与本 VM（Ubuntu 26.04）上满足要求。
- **会话**：会话能力多了一个键，升级后每个已有会话在 checkpoint 换一次新会话（ADR-030 决定 4 的既有路径）。
- **兼容套件**：多了只读档的启动变体与一个不计入 `suite.status` 的 `read_only` 用例，套件集摘要随之变化，每个 Agent 重跑一次套件（ADR-030 决定 10）。
- **注册表**：每次实际查看 codex 时，以及结论不是 confined 时每 60 秒，运行一次本机沙箱检查（离线，不调模型，以临时 `CODEX_HOME` 运行）。它只影响 codex 能否当 Commander，从不使任何会话换绑。
- **adapter-live**：v1 下多出 TEST-021-07，按格判定（ADR-032 设计修正 1）。每段两轮只读 turn、一轮对照，覆盖不足时至多重做两次；codex 两段。这会消耗订阅额度。v0.1 下不运行。
- **用户看到的**：doctor 每个 Agent 项多一个 `read_only`（结论、强制方式与组成部分；codex 另有本机沙箱检查的结论）；docs/12 多一段余项说明。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 不放宽任何检查或门禁：
  - 新增两条 AC，新增一项职位所需能力；
  - YOLO 兼容套件的用例与门槛不变，只读档的符合性另记一个用例，只影响新能力；
  - 其余职位的启动与验证不变；
  - v0.1 的门禁不因只属 v1 的活体项而变化。
- 不改协议 schema、SQL、openapi、compat-manifest schema（`capabilities` 本来就是开放对象，`suite.cases[].case` 是任意字符串）、release-policy、docs/10。
- 不新增原因码：沿用 BLOCKED_CAPABILITY 与 CAPABILITY_UNSUPPORTED。
- 不新增配置键：`execution.profile` 仍只接受 `yolo-vm`。
- 不改 Commander 的唤醒、去重、决策栅栏与 nonce 的校验（REQ-021 行为 1–8，ADR-025）。
- CONTRACT 第 7 条原有的两句保持原文；docs/00“在 VM 内再建复杂权限沙箱”仍是非目标：本 CR 只用底座原生的机制。

## 是否属于 CR-003 D1 预授权

不属于。

- 第 4 条不满足：改了 CONTRACT 第 7 条。
- 第 1 条也不满足：这是所有者决定的新行为，不是把 outline 细化或修正不一致。

第 2、3 条满足，见“不变的部分”。因此须项目所有者明确批准；所有者已于 2026-11-16 决定方向，这里请所有者批准本文字。

## 验证方式

1. ADR-032 经两个互补的独立设计审查（这是权限与进程相关的改动），判定本 CR 的文字可交所有者批准；所有者批准后，状态改为 APPROVED，并写明批准人与日期。
2. M2-T13 的重新上锁进入 main 之后：
   - 按上文逐字改受锁文件，插入位置按上文所写；
   - 运行 `python3 tools/gen_spec_views.py`；
   - 运行 `python3 tools/baseline.py relock --cr docs/cr/CR-024-commander-read-only-profile.md`；
   - `bash scripts/check.sh --bundle` 通过。任务图规则要求每条 AC 恰有一个实现任务，检查由该任务或其祖先交付。
3. M2-T20 按 ADR-032 的测试计划先红后绿：
   - TEST-021-06 经 integration 检查；
   - TEST-021-07 经 adapter-live 检查（v1），先在全权限档下得到快照差异的红证据，再在只读档下 PASS；
   - 覆盖不足与未登录都不得为 PASS。
