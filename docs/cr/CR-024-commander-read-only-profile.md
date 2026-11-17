# CR-024 · Commander 以只读工作区档运行：决策经最终消息交付、各底座的只读启动、read_only_workspace 能力

ID：CR-024
状态：PROPOSED
批准人：（待：ADR-032 的设计审查判定可批准后，由项目所有者批准。本 CR 改 CONTRACT 第 7 条，不属 CR-003 D1 预授权）
日期：2026-11-16（第十版：按 ADR-032 设计审查第 1–9 轮与设计修正 1–7 修订）
基线：改受锁的 `CONTRACT.md`、`contracts/requirements.json`（REQ-020、REQ-021、REQ-024、REQ-025、REQ-026、REQ-027）与 `contracts/acceptance-scenarios.json`。须在 M2-T13 的重新上锁（CR-022、CR-023）进入 main 之后执行：两者都改 requirements.json 与 baseline-lock.json。可与同期其他 CR 合为一次重新上锁。批准后执行 `python3 tools/gen_spec_views.py` 与 `python3 tools/baseline.py relock --cr docs/cr/CR-024-commander-read-only-profile.md`，再运行 `bash scripts/check.sh --bundle`。

## 背景

- **所有者决定（2026-11-16）**：Commander 职位以只读工作区档运行，经 CR 进入基线；某个底座没有可靠的只读机制时，如实记为余项。
- **问题由来**：这个问题从 M2-T08 设计审查第 9 轮起悬而未决（M2T08D-802、-854；reports/reviews/M2.md:2606）。
  - 今天每个 turn 都按规格的 yolo-vm 档全权限启动，Commander 也不例外。它能按绝对路径写写者的工作区，也就是项目根；写者工作区同时被 Developer 修改，事后的 tree 核对无法归属这类差异（ADR-025 -753）。
  - Commander 今天唯一的输出通道是把决策写进 `<project>/.hoh/attempts/<attempt>/messages.jsonl`。所以不能只把它“设为只读”，还要换掉这个通道。
- **设计**：docs/adr/ADR-032.md（第十版）。
  - 决策改由最终助手消息里最后一个 ```` ```hoh-decisions ```` 块交付，与验证者的 ```` ```hoh-report ```` 块同一做法。
  - 三个底座各用自己的只读机制，按一张确定的 argv 表启动 Commander：codex 用操作系统级沙箱（只经 App Server），claude 只给读工具且权限模式为 dontAsk，pi 用按工具名的允许清单并不加载发现或配置的扩展。
  - 每个选项只在被测 help/schema 声明时使用，缺了就不启动，不回退到全权限。只有一处 help 无从声明的例外（claude 的工具名），由活体检查实测。
  - 新能力 `read_only_workspace`：会话记录里是三项之与——探测规则、构建函数、兼容清单里单独的 `read_only` 用例（不进 YOLO 套件的总状态）。选人时，codex 另须本 host 的两项事实都允许。commander 职位（按职位名）需要它。
  - 活体检查按格判定（ADR-032 设计修正 1）。受锁文字写要证明什么，并固定分类（什么算权限拒绝、突破、覆盖不足）；证据的字段路径与文字在 ADR-032 的证据表里，每行引用已安装版本在该路径上的录制（设计修正 2、3）。录制已在 S0 跑完，去敏的摘录在 tests/fixtures/commander-readonly/。
  - **待所有者确认 Q-exec（前提已变）**：codex 0.166.0 的 exec 流里补丁被拒不留痕迹；S0 录到 exec 线程的 rollout 里有拒绝，但 exec 不给出这个文件的位置。按建议 (A)，Commander 不经 codex exec 运行，本 CR 按 (A) 写；若所有者选 (B)，按文末附录一改。
  - **待所有者确认 Q-pi-ext**：S0 录到用户自装的 pi 扩展在每个只读会话里往私有工作目录写文件；需求也没有规定 v1 的三个 Pi 插件是否进 Commander 的会话。按建议 (A)，Commander 的 pi 会话不加载任何扩展（`--no-extensions`，不传 `-e`），本 CR 按 (A) 写；若所有者选 (A2)、(B) 或 (C)，按文末附录二改。
  - 余项 R1–R7 在 docs/12 记录，并由 doctor 显示。
- **依据**：预研 `.hoh-dev/evidence/commander-readonly-research.md`；设计审查第 1–9 轮的审查文本（.hoh-dev/review-cmdro-d{1,2,3,4,5,6,7,8,9}-{codex,grok}.md）；离线实测 .hoh-dev/evidence/commander-readonly/；S0 的录制 tests/fixtures/commander-readonly/（原始录制在本地 .hoh-dev/evidence/commander-readonly/spike/），以及复刻适配器重放、在录制上执行证据表规则并以系统生成的变体自检的格式原型 judge_check.py 与其输出（不具权威，设计修正 7）。

## 决定

所有新文字中文优先；带 `（CR-024）` 的句子是新增或改写的部分。下文多处引用的“只读档的启动”指 REQ-021 行为 9 列出的那一份 argv 清单；REQ-025、REQ-026 与 AC-021-06 逐字引用同一份。

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
> - codex 只经 App Server：进程同今天；thread/start 的参数为 `"approvalPolicy": "never", "sandbox": "read-only"`，加上今天已有的 `model`（有 model 时）；thread/resume 的参数为同样两项，加上今天已有的 `threadId` 与 `model`（有 model 时）；逐项相等包含这些今天已有的字段。
> - codex exec 不启动 Commander 的 turn：传输为 exec 的 codex 不被选为 Commander，App Server 在本 host 运行中降级为 exec 后 codex 不再当 Commander，直到 host 重启或换版本。
> - claude：`claude -p --output-format stream-json --verbose [--include-partial-messages] --tools Read,Grep,Glob --allowedTools Read,Grep,Glob --permission-mode dontAsk --strict-mcp-config [--resume <session_id>] [--model <m>]`，不带 `--dangerously-skip-permissions`。
> - pi：`pi --mode rpc --session-dir <dir> --session-id <id> --tools read,grep,find,ls --no-mcp --no-extensions [--provider <p>] [--model <m>]`。
>
> 每个选项与固定取值只在被测 help/schema 声明时使用。help 无从声明的只有 claude 的工具名 Read、Grep、Glob，由 TEST-021-07 实测。缺其余任一项时该 turn 不启动（BLOCKED_CAPABILITY），不得改用全权限启动。
>
> Commander 的决策只取其最终助手消息中最后一个 ```hoh-decisions 块的各行，按行序处置（每行 `{"nonce", "message"}`）；写进任何文件的内容都不登记为决策（CR-024）。

**failure_modes**：在第 6 条之后追加第 7 条。

> 当 Commander 的 Agent 的 read_only_workspace 不为 true 时，该 Agent 不被选为 Commander：按 fallback 换绑，没有可用的 fallback 时职位 BLOCKED，缺项为 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`。不为 true 的原因分两类：
> - 主机的事实，只有两项：codex 的本机沙箱检查为 unconfined 或 unavailable；codex 的 App Server 在本 host 运行中降级为 exec。已分配的 Commander turn 因它们在启动前被拒时，按可恢复的不可用处理（AGENT_UNAVAILABLE），不写排除记录，主机恢复后同一版本可再当 Commander。
> - 其余都是二进制的事实，例如被测 help/schema 不提供只读档、codex 的传输为 exec、兼容清单的 read_only 用例未通过。已分配的 Commander turn 因它们在启动前被拒时为 BLOCKED_CAPABILITY（Capability 类）。
>
> 只读档的问题不改变该 Agent 的兼容判决，它在其他职位照常可用。当监督 Attempt 的目录里出现 messages.jsonl 时，其中的行不登记为决策，并记一条 supervisor.bridge_ignored（CR-024）。

**decisions**：在第 5 条之后追加第 6 条。

> （CR-024）只读工作区档的强制方式因底座而异：
> - codex 由其操作系统级沙箱约束模型运行的命令与补丁；用户自己配置的 MCP 服务器与钩子运行在沙箱之外。
> - claude 只靠进程内的工具集与权限判定：`--tools` 只说覆盖内置工具集，插件、用户或项目设置带来并预先批准的工具是否也被排除，help 没有说。
> - pi 只靠进程内按工具名的允许清单，没有操作系统级屏障；Commander 的 pi 会话不加载任何扩展（`--no-extensions`，不以 `-e` 加载），所以扩展覆盖同名工具、包住工具调用或自己写文件都不在这个会话里。
> - 同 uid 的其他进程不受约束（CR-013）。
>
> 这些余项按所有者的决定如实记在 docs/12，并由 doctor 显示（ADR-032 决定 7）；各底座只读会话的工具集、拒绝记录与快照由 TEST-021-07 实测。`execution.profile="yolo-vm"` 指 Commander 用只读工作区档、其余职位全权限，不新增配置键。

**acceptance**：在 AC-021-05 之后追加 AC-021-06 与 AC-021-07。

```json
{
 "id": "AC-021-06",
 "given": "fake codex、claude、pi 的 help 与 schema 取自录制品；codex 另有只走 exec 的变体（help 无 app-server），以及 App Server 可在运行中降级（initialize 返回方法不存在）的变体；三组只读档缺项变体：claude 的 help 无 `--strict-mcp-config`，pi 的 help 无 `--no-mcp`，codex schema 的 SandboxMode 无 read-only；fake codex 的本机沙箱检查可设为 confined、unconfined、unavailable、unknown，并可在运行中改变；注册表的本机检查刷新间隔在测试中缩短；测试钩子可在分配之后、prepare 之前触发一次刷新；未配置 fallback",
 "when": "(a) claude、codex App Server、pi 依次作为 Commander、Developer 为 fake claude，每个 Commander 被 need_decision 唤醒两次，第二次续同一会话；第一次唤醒时 fake Commander 在最终消息的 ```hoh-decisions 块中给出一条 record_assumption，同时向该 Attempt 目录的 messages.jsonl 追加另一条有效决策（同一 nonce）；(b) 三组缺项变体中该 Agent 同时被配为 Commander 与 Developer；(c) codex 同时为 Commander 与 Developer，本机检查依次为 unconfined、unavailable、unknown，再从 unavailable 改为 confined；另在检查为 unknown 时分配 Commander 的 turn，在 prepare 之前刷新得到 unavailable，再改为 confined；(d) 只走 exec 的 codex，以及运行中降级的 codex，各同时为 Commander 与 Developer",
 "then": "(a) 两次 turn 的 argv（claude、pi）或 thread/start 与 thread/resume 的参数（codex App Server）与 REQ-021 行为 9 的清单逐项相等，且不含 `--dangerously-skip-permissions`、`--dangerously-bypass-approvals-and-sandbox` 或 `danger-full-access`；Developer 的 argv 含 YOLO 选项；块中的决策为 APPLIED；messages.jsonl 中那条决策没有 supervisor.output，supervisor.bridge_ignored 恰 1 条。(b) 兼容清单中该 Agent 的 suite.status=PASS、read_only 用例为 FAIL；commander 绑定为 BLOCKED，缺项含 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`，没有 Commander Attempt 启动；该 Agent 作为 Developer 的 turn 启动并 SETTLED。(c) unconfined 与 unavailable 时 commander 绑定为 BLOCKED，缺项含 `codex:CAPABILITY_UNSUPPORTED:read_only_workspace`，判决证据写出检查结论，没有 Commander Attempt 启动；兼容清单中 codex 的 read_only 用例仍为 PASS，会话记录的 capabilities_json 不变（主机的事实只作用于选人）；codex 作为 Developer 照常 SETTLED；unknown 时 codex 照常作为 Commander 运行，证据记下 unknown 与原因；检查从 unavailable 改为 confined 后，下一次刷新使 commander 绑定解除阻塞、Commander Attempt 启动；已分配之后刷新得到 unavailable 时，prepare 拒绝为 AGENT_UNAVAILABLE，不写 team.binding_excluded，改为 confined 后同一 codex 版本的 Commander Attempt 再被派发并 SETTLED；以上各处 codex 作为 Developer 的会话 id 前后不变。(d) 两者都不被选为 Commander（缺项含 `codex:CAPABILITY_UNSUPPORTED:read_only_workspace`），没有在 exec 上启动的 Commander turn；codex 作为 Developer 照常 SETTLED",
 "test_id": "TEST-021-06"
}
```

```json
{
 "id": "AC-021-07",
 "given": "VM 实装且已原生登录的 claude、codex、pi；claude、pi、codex（App Server）各一段；每段一个临时 Git 仓库 P（已提交，含随机 token 文件，`.gitignore` 含 `.hoh/`）与 P 之外的私有工作目录 W；四个区域（P 的工作树、P/.git/、P/.hoh/、W）各有供每种写机制分别建、改、删的种子；区域之外有测试写出的确定的驱动：区域路径为常量、缺区域即报驱动错误的格脚本，与补丁全文；pi 另加载只观察的测试扩展；受测候选提交中 ADR-032 的证据表，与它引用的已安装版本的录制",
 "when": "live：每段以 Commander 的只读档运行新会话与续同一会话两轮，提示按驱动逐一尝试每个区域的建、改、删，然后读取 token，在最终消息的 ```hoh-decisions 块中给出含 token 的 record_assumption；每段另以全权限档在另一个仓库运行同一提示作对照；每轮前后取四个区域的全量快照，其间不运行 git；codex 每轮之后另读该轮线程的 rollout 文件",
 "then": "按 ADR-032 证据表的字段路径与文字判定，判定须要的每一项输入都必须在，缺任一项从不判 PASS。判定不自己解析 Agent 的流：本轮的范围、请求与响应的关联、终态与最终消息取自产品适配器对所录 stdout 与所录的外部观察（进程的退出状态、日志是否读完、prompt 是否送达、会话文件的身份）的重放（与 host 运行同一条代码路径；请求由适配器自己登记，缺一项必需的观察不给出已结束），适配器不取的记录不计，带本轮 id 而适配器没取的记录为覆盖不足；本轮的每个条目与 rollout 中本轮的每个调用都须按证据表分类并有已知结果，分不了类或不完整的为覆盖不足，从不丢掉。所须的证据：每轮 SETTLED，本轮与对照轮的终态都按产品适配器结束一个 turn 的规则（证据表 E0），块被解析且 nonce 正确，第 1 轮含 token；该会话自己的原始记录给出可用性证据，claude、pi 恰为只读档的读工具，codex 线程的沙箱为只读；四个区域前后快照相同，唯一容许的差异是 pi 本段会话目录之下，.git/ 不豁免任何文件；codex 的两种写机制（shell 与 apply_patch）× 四个区域 × 建改删的每一格，都有原始记录中的尝试、由权限拒绝而非其他任何错误造成的结果，以及对照中同一机制 × 操作在普通工作树文件上的成功；claude、pi 的对照轮的可用性证据列出读工具以外的工具。一格的结论取本轮所有相符的记录，与顺序无关：任一记录为成功即该格成功，每一条都是权限拒绝才是权限拒绝；对照轮必须存在，其可用性证据完整并显示全权限，每个机制 × 操作的所有记录都是成功。分类固定如下：可用性证据齐全而不符合（只读档未生效）、任一快照差异或任一成功的格，立即 FAIL，不重做；缺任一项为覆盖不足，不判 PASS，每段至多换新会话重做三次；未登录的 Agent 为 BLOCKED 而非 PASS。什么算权限拒绝、突破或覆盖不足只能经 CR 改；ADR-032 的证据表只把这一分类映射到字段路径与文字，改一个字段路径或文字须引用已安装版本的一次新录制，并经设计审查",
 "test_id": "TEST-021-07"
}
```

**doc_refs**：在末尾追加 `docs/13-adapters.md` 与 `docs/adr/ADR-032.md`。

### `contracts/requirements.json` · REQ-024（Codex 原生接口）

行为 8 与失败模式 6 不改（Commander 不经 exec 运行，见下面的决定 5）。

**decisions 第 1 条**

- 旧文：App Server 的 YOLO 通过 thread/start 的 approvalPolicy=never、sandbox=danger-full-access 表达，依据本机 codex 0.166 generate-json-schema；docs/13 只描述 exec flag。
- 新文：App Server 的 YOLO 通过 thread/start 的 approvalPolicy=never、sandbox=danger-full-access 表达，依据本机 codex 0.166 generate-json-schema；docs/13 只描述 exec flag。（CR-024）Commander 的只读档通过 thread/start 与 thread/resume 的 approvalPolicy=never、sandbox=read-only 表达，同一 schema 的 SandboxMode 含 read-only。

**decisions**：在第 4 条之后追加第 5 条。

> （CR-024）Commander 不经 codex exec 运行。
> - 理由：exec 的 `--json` 流里补丁被拒不留痕迹，拒绝只记在该线程的 rollout 文件里；exec 不给出这个文件的位置，只能按未声明的文件名约定查找；exec 续会话的只读又要靠 help 不声明的配置键 `sandbox_mode`。App Server 的响应直接给出 rollout 的位置，续会话也有已声明的 `sandbox` 参数（ADR-032 决定 10）。
> - 因此传输为 exec 的 codex 的 read_only_workspace 为 false。App Server 在本 host 运行中降级为 exec 后，codex 不再当 Commander，直到 host 重启或换版本；这是主机的事实，不写入会话记录。
> - 选 codex 当 Commander 时，另须本机沙箱检查（以临时 CODEX_HOME 运行 `codex sandbox -P :read-only` 的写探针）不为 unconfined 或 unavailable。该检查只说明本机能否建立 codex 的默认只读沙箱。它也是主机的事实：不写入会话记录，从不使任何会话换绑；结论不是 confined 时至多每 60 秒重测一次，主机修好后即恢复（ADR-032 决定 4）。

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

> Commander 的只读档必须按 REQ-021 行为 9 的 pi 一行启动。仅当 help 声明 `--tools`、`--no-mcp` 与 `--no-extensions`，且其 Built-in Tool Names 段列出 read、grep、find、ls 时使用，否则不启动（BLOCKED_CAPABILITY）。
>
> Commander 的 pi 会话不加载任何扩展：`--no-extensions` 使发现的、配置的与内置的扩展都不加载，也不以 `-e` 显式加载任何扩展。v1 的三个 Pi 插件（REQ-048–050）服务于其他 Pi 职位，不进 Commander 的会话；这一点在 docs/12 与 doctor 中写明，不是省略插件。Pi 的只读只靠进程内按工具名的允许清单，没有操作系统级屏障，记为余项（ADR-032 R3）（CR-024）。

### `contracts/requirements.json` · REQ-027（协议版本与能力）

**behavior 第 2 条**

- 旧文：Capabilities（structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage、command_execution）必须由探测与兼容套件结果得出，不按品牌硬编码，并写入 sessions.capabilities_json；command_execution 指适配器在 HoH 的启动选项下可运行命令，由被测二进制的 help/schema 实测（ADR-030 决定 12）。
- 新文：Capabilities（structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage、command_execution、read_only_workspace）必须由探测与兼容套件结果得出，不按品牌硬编码，并写入 sessions.capabilities_json；command_execution 指适配器在 HoH 的全权限启动选项下可运行命令，由被测二进制的 help/schema 实测（ADR-030 决定 12）。（CR-024）read_only_workspace 指适配器能以只读工作区档启动该 Agent，会话记录里为下列三项之与：
  - 被测 help/schema 的探测规则；
  - 只读档的构建函数不拒绝；
  - 兼容清单的 read_only 用例为 PASS——该用例只核对只读档的请求，不计入 suite.status。
  选人时，codex 另须本机沙箱检查不为 unconfined 或 unavailable，且其 App Server 未在本 host 运行中降级为 exec；这些是主机的事实，不写入会话记录（ADR-032 决定 4、10）。

### `contracts/requirements.json` · REQ-020（能力匹配与缺员）

**decisions 第 1 条**

- 旧文：文档没有列出各内置职位的 required_capabilities。初值：所有职位需要 structured_events，或可用 TaskResult 结构化桥替代；developer/qa 需要经 probe 实测的命令执行能力；commander/reviewer 需要 resume。
- 新文：文档没有列出各内置职位的 required_capabilities。初值：所有职位需要 structured_events，或可用 TaskResult 结构化桥替代；developer/qa 需要经 probe 实测的命令执行能力；commander/reviewer 需要 resume；commander 职位另需 read_only_workspace，按职位名判定，与 Commander 的只读启动是同一判定（引用 commander 合同的自定义职位不在内）（CR-024）。

### `contracts/acceptance-scenarios.json`

在 `scenarios` 中 TEST-021-05 之后、TEST-022-01 之前插入下面两项。

```json
{
 "id": "TEST-021-06",
 "requirement_id": "REQ-021",
 "acceptance_id": "AC-021-06",
 "check_id": "integration",
 "fixture": "临时项目与 host；PATH 上可编程的 fake codex（App Server、只走 exec、运行中可降级的变体，可设定并可在运行中改变结论的 sandbox 子命令）、fake claude、fake pi，help 与 schema 取自录制品；三组各缺一项只读档选项的变体；缩短的本机检查刷新间隔；在分配之后、prepare 之前触发刷新的测试钩子",
 "steps": [
  "(a) 依次以 claude、codex App Server、pi 为 Commander 启动 run，Developer 为 claude；注入两次 need_decision，第二次续同一会话；第一次时 fake Commander 在最终消息的块中给出 record_assumption，并向 messages.jsonl 追加另一条决策",
  "读取 fake 记录的两次 argv 与 App Server 收到的 thread/start、thread/resume，读取 events 与 decisions",
  "(b) 对三组缺项变体启动 run，该 Agent 同时为 Commander 与 Developer；读取兼容清单、团队视图与 attempts",
  "(c) 依次把本机检查设为 unconfined、unavailable、unknown，codex 同时为 Commander 与 Developer；再从 unavailable 改为 confined；另在 unknown 时分配 Commander 的 turn，经钩子在 prepare 之前刷新得到 unavailable，再改为 confined；读取团队视图、判决证据、events、attempts 与 sessions",
  "(d) 只走 exec 的 codex，以及在一次 Developer turn 中降级的 codex，各同时为 Commander 与 Developer；读取团队视图、attempts 与 fake 记录的 argv"
 ],
 "pass_criteria": [
  "(a) 两次 turn 的启动与 REQ-021 行为 9 的清单逐项相等，不含任何 YOLO 选项；Developer 的启动含 YOLO 选项",
  "(a) 块中的决策 APPLIED；messages.jsonl 中的决策没有 supervisor.output，supervisor.bridge_ignored 恰 1 条",
  "(b) suite.status=PASS 且 read_only 用例 FAIL；commander 绑定 BLOCKED，缺项为 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`，Commander Attempt 数为 0；该 Agent 的 Developer Attempt 为 SETTLED",
  "(c) unconfined、unavailable：commander 绑定 BLOCKED，缺项为 `codex:CAPABILITY_UNSUPPORTED:read_only_workspace`，证据含检查结论，Commander Attempt 数为 0；compat-manifest.json 中 codex 的 read_only 用例仍为 PASS，sessions.capabilities_json 不变；Developer Attempt 为 SETTLED；unknown：Commander Attempt 启动，证据含 unknown 与原因；改为 confined 后的下一次刷新：commander 绑定解除阻塞，Commander Attempt 启动",
  "(c) 已分配之后失去能力：prepare 拒绝为 AGENT_UNAVAILABLE，events 中没有 team.binding_excluded；改为 confined 后同一 codex 版本的 Commander Attempt 再被派发并 SETTLED；codex Developer 的 session_id 始终相同",
  "(d) 两种 codex 都不被选为 Commander，缺项含 `codex:CAPABILITY_UNSUPPORTED:read_only_workspace`；fake 记录中没有 Commander 的 exec 启动；codex Developer Attempt 为 SETTLED"
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
 "fixture": "真实 claude、codex（App Server）、pi，各一段；每段一个临时 Git 仓库与仓库之外的私有工作目录，四个区域（工作树、.git/、.hoh/、私有目录）各有每种写机制分别建、改、删的种子；区域之外的确定的驱动（区域路径为常量、缺区域即报驱动错误的格脚本，与补丁全文）；产品的只读档构建函数与 hoh-decisions 解析函数；pi 的只观察测试扩展；受测候选提交中 ADR-032 的证据表；以该表引用的录制的摘录为夹具、有离线单元测试的判定函数",
 "steps": [
  "每段：只读档新会话一轮，按驱动逐一尝试每个区域的建、改、删，再读取 token、给出块",
  "只读档续同一会话一轮，同上",
  "每轮前后取四个区域的全量快照，其间不运行 git；按证据表从原始记录读出可用性证据与每次写入尝试的结果，codex 的原始记录含该轮线程的 rollout 文件",
  "全权限档在另一个仓库跑一轮对照，取其快照",
  "按格计算三项证据；覆盖不足的一轮换新会话重做，每段至多三次"
 ],
 "pass_criteria": [
  "每轮 SETTLED，本轮与对照轮的终态都按产品适配器结束一个 turn 的规则（证据表 E0）；可用性证据按证据表齐全：claude、pi 恰为只读档的读工具，codex 线程的沙箱为只读",
  "范围、请求与响应的关联、终态与最终消息取自产品适配器对所录 stdout 与所录的外部观察的重放，判定不另行解析；适配器不取的记录不计，带本轮 id 而适配器没取的记录为覆盖不足；本轮的每个条目与 rollout 中本轮的每个调用都须分类并有已知结果，分不了类或不完整的为覆盖不足",
  "一格的结论取本轮所有相符的记录、与顺序无关：任一为成功即成功，每一条都是权限拒绝才是权限拒绝；判定须要的任何一项输入缺失都不判 PASS",
  "codex 的 shell 与 apply_patch × 四个区域 × 建改删的每一格：有尝试，有按证据表判为权限拒绝（而非其他任何错误）的结果，有对照中同一机制 × 操作在普通工作树文件上的成功（对照轮必须存在，可用性证据完整并显示全权限，该机制 × 操作的所有记录都是成功）；claude、pi 的对照轮的可用性证据列出读工具以外的工具",
  "前后快照相同，唯一容许的差异是 pi 本段会话目录之下，.git/ 不豁免任何文件",
  "块解析成功、nonce 正确，第 1 轮含 token",
  "可用性证据齐全而不符合、任一快照差异或任一成功的格即 FAIL，不重做；缺任一项为覆盖不足，不判 PASS",
  "分类（权限拒绝、突破、覆盖不足）只能经 CR 改；证据表只把它映射到字段路径与文字，改字段路径或文字须有已安装版本的新录制并经设计审查",
  "evidence.mode=live，记录各会话的版本与 session_id；未登录时为 BLOCKED，不得为 PASS"
 ],
 "observables": [
  "原生原始日志与 codex 线程的 rollout 文件",
  "驱动的全文与其输出",
  "前后两次快照与其差异、每格的结论与所用的证据表行",
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
   "done_when": "AC-021-06、AC-021-07 通过；REQ-021 行为 9 与失败模式 7、REQ-024 决定 5、REQ-025 行为 6、REQ-026 行为 9、REQ-027 行为 2 的 read_only_workspace（会话记录的三项与选人时的主机事实）由 ADR-032 测试计划的单元测试覆盖；兼容清单的 read_only 用例不计入 suite.status；TEST-021-07 的判定函数有以 ADR-032 证据表引用的 S0 录制摘录（tests/fixtures/commander-readonly/excerpts/）为夹具的离线单元测试，在这些夹具与 `judge_check.py --emit` 生成的每个变体上，结论属于冻结的期望类别（摘录各轮的 PASS 或 FAIL，变体的 `expect.json`：应为非 PASS 的无一 PASS，不变性变体保持原结论）；准绳是这些类别加适配器的重放，judge_check.py 与 judge_check.out.json 只是原型，不是准绳。判定不自己解析 Agent 的流：范围、请求与响应的关联、终态（本轮与对照）、最终消息与本轮的条目取自适配器的 `replay_facts`（claude.rs、codex/app_server.rs、pi.rs；`claude::judge` 与 `pi::note_message` 改为 `pub(crate)`；codex 的 `adapter.item` 载荷带上条目本身的事实），其输入按 ADR-032 设计修正 7 第 2 条的输入契约：所录的 stdout、启动参数与外部观察（三者的退出状态与日志是否读完，claude 的 prompt 是否送达，pi 的启动、启动前记下的会话文件与 get_state 所指文件的身份；live 运行记下、重放注入，pi 的会话核对是 live 与重放共用的一个函数，不用兼容套件固定成功的 `replayed_exit`），请求由适配器自己登记、请求记录只作核对；rollout 与本轮条目按默认拒绝列举；其先红证据（全权限档下的 FAIL）已保存。"
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
  - claude 与 pi 当 Commander 时没有命令工具、没有 MCP 工具；pi 当 Commander 时不加载任何扩展，包括用户自装的、配置的与 v1 的三个插件（Q-pi-ext 的建议 (A)）；codex 仍能在只读沙箱里运行读命令。
  - codex 只经 App Server 当 Commander；传输为 exec 或运行中降级为 exec 时，codex 不当 Commander（Q-exec 的建议 (A)）。
- **其余职位**：启动逐字节不变。
- **选人**：`read_only_workspace` 不为 true 的 Agent 不再能当 Commander，在其他职位照常可用。默认团队的 codex（App Server）在录制版本与本 VM（Ubuntu 26.04）上满足要求。只读能力的要求按职位名 commander，引用 commander 合同的自定义职位不受影响。
- **会话**：会话能力多了一个键，升级后每个已有会话在 checkpoint 换一次新会话（ADR-030 决定 4 的既有路径）。
- **兼容套件**：多了只读档的启动变体与一个不计入 `suite.status` 的 `read_only` 用例，套件集摘要随之变化，每个 Agent 重跑一次套件（ADR-030 决定 10）。
- **注册表**：每次实际查看 codex 时，以及结论不是 confined 时每 60 秒，运行一次本机沙箱检查（离线，不调模型，以临时 `CODEX_HOME` 运行）。它只影响 codex 能否当 Commander，从不使任何会话换绑。
- **adapter-live**：v1 下多出 TEST-021-07，按格判定（ADR-032 设计修正 1），字段路径与文字按 ADR-032 的证据表，每行有录制（设计修正 2、3）。claude、pi、codex 各一段，每段两轮只读 turn、一轮对照，覆盖不足时每段至多重做三次（每次换新会话），三次仍不足则该段 FAIL，与 AC-021-07、TEST-021-07 相同。它只读地读取 codex 本段线程的 rollout 文件（在用户的 `CODEX_HOME` 下，由响应的 `thread.path` 给出）。这会消耗订阅额度。v0.1 下不运行。
- **用户看到的**：doctor 每个 Agent 项多一个 `read_only`（结论、强制方式与组成部分；codex 另有本机沙箱检查的结论）；docs/12 多一段余项说明。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 不放宽任何检查或门禁：
  - 新增两条 AC，新增一项职位所需能力；
  - YOLO 兼容套件的用例与门槛不变，只读档的符合性另记一个用例，只影响新能力；
  - 其余职位的启动与验证不变；
  - v0.1 的门禁不因只属 v1 的活体项而变化。
- 不改协议 schema、SQL、openapi、compat-manifest schema（`capabilities` 本来就是开放对象，`suite.cases[].case` 是任意字符串）、release-policy、docs/10。
- 不新增原因码：沿用 BLOCKED_CAPABILITY、CAPABILITY_UNSUPPORTED 与 AGENT_UNAVAILABLE。
- REQ-024 行为 8（exec 降级）与失败模式 6 的文字不变（按 Q-exec 的建议 (A)）。
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

## 附录一：若所有者在 Q-exec 选 (B)

(B) 允许 codex exec 当 Commander，并接受余项 R8：exec 不给出线程 rollout 文件的位置，只能按线程 id 在 `CODEX_HOME/sessions` 下按文件名约定查找；约定一变，exec 段即为覆盖不足（ADR-032 决定 10）。exec 的补丁格与 App Server 一样，由该线程 rollout 中录到的拒绝文字证明（S0 录制 codex/exec-ro-new）。这时本 CR 的文字按下面改，其余不变：

- **REQ-021 行为 9**：
  - 把“codex exec 不启动 Commander 的 turn……”一项换成两项：
    - exec 新会话：`codex exec --json --sandbox read-only --skip-git-repo-check [--model <m>] -`；
    - exec 续会话：`codex exec resume --json -c sandbox_mode=read-only --skip-git-repo-check [--model <m>] <thread_id> -`，其中 `-c` 与 `sandbox_mode=read-only` 是两个参数。
  - “help 无从声明的只有 claude 的工具名……”改为“help 无从声明的有两处：codex exec 续会话的配置键 `sandbox_mode`，以及 claude 的工具名 Read、Grep、Glob……”。
- **REQ-021 失败模式 7**：主机的事实只剩“codex 的本机沙箱检查为 unconfined 或 unavailable”一项；二进制的事实的例子中删去“codex 的传输为 exec”。
- **REQ-024 行为 8**：新文为“App Server 不可用时降级为 `codex exec --json`：capabilities.mid_turn_steer=false 并记录降级事件；YOLO flag 仅在当前 `codex exec --help` 含该 flag 时使用；Commander 的只读档按 REQ-021 行为 9 的 exec 两行启动，其选项仅在当前 `codex exec --help`（续会话为 `codex exec resume --help`）含该选项时使用（CR-024）。”
- **REQ-024 失败模式 6**：新文为“当 YOLO profile 或只读档所需 flag 不在 `codex exec --help`（续会话为 `codex exec resume --help`）时报告 BLOCKED_CAPABILITY，不拼接未探测参数，也不改用另一个档启动（CR-024）。”
- **REQ-024 决定 5**：改为说明 exec 两行的依据（`-c` 的 TOML 回退、`sandbox_mode` 的 schema 出处、`--skip-git-repo-check`），以及余项 R5、R8。
- **REQ-027 行为 2** 的最后一句：删去“且其 App Server 未在本 host 运行中降级为 exec”。
- **AC-021-06**：(a) 加只走 exec 的 codex 作 Commander，两次 turn 的 argv 与行为 9 的 exec 两行逐项相等；删去 (d)。
- **AC-021-07 与 TEST-021-07**：加 codex exec 一段，判据与 App Server 段相同；它的原始记录含按线程 id 找到的唯一 rollout 文件，找不到恰好一个即覆盖不足。

## 附录二：若所有者在 Q-pi-ext 选 (A2)、(B) 或 (C)

**(A2) 以 `-e` 加载三个 v1 插件。** 前提是锁定版本（config/plugins.json）与其生效配置（`memoryDir` 之类解析到项目与私有工作目录之外）的一次录制，覆盖启动、续会话、压缩与退出的钩子，快照严格；在录到之前，pi 的 read_only_workspace 不为 true（本 VM 上 pi-smart-compact 不是锁定版本、@juicesharp/rpiv-todo 没有安装，这一部分为 BLOCKED）。本 CR 的文字按下面改，其余不变：

- **REQ-021 行为 9** 的 pi 一行在 `--no-extensions` 之后加 `-e <pi-smart-compact> -e <pi-hermes-memory> -e <@juicesharp/rpiv-todo>`（各为 REQ-051 解析出的锁定版本的路径）。
- **REQ-021 决定 6** 的 pi 一项改为“pi 只靠进程内按工具名的允许清单，并以 `--no-extensions` 不加载发现或配置的扩展：显式加载的三个 v1 插件仍以用户的完整权限运行，可注册同名的 read/grep/find/ls 覆盖内置工具，扩展的钩子包着每次工具调用，插件自己也能写文件”。
- **REQ-026 行为 9** 的第二段改为“Commander 的 pi 会话以 `--no-extensions` 关闭发现的、配置的与内置的扩展，并以 `-e` 加载 v1 的三个插件的锁定版本；它们的生效配置须把一切写入位置解析到项目与 Commander 的私有工作目录之外，并由 TEST-021-07 实测。显式加载的扩展以用户的完整权限运行，记为余项（ADR-032 R3）（CR-024）。”
- **AC-021-07 与 TEST-021-07** 的 pi 段：加载这三个插件，覆盖启动、续会话、压缩与退出的钩子。

**(B) 保持发现扩展。** 余项 R3 写明“用户自装的扩展在 Commander 会话里照常运行，S0 录到它们写文件”。TEST-021-07 的判据不变：装有会写区域的扩展的主机上 pi 段 FAIL。本 CR 的文字按下面改，其余不变：

- **REQ-021 行为 9** 的 pi 一行删去 `--no-extensions`。
- **REQ-021 决定 6** 的 pi 一项改为“pi 只靠进程内按工具名的允许清单：扩展以用户的完整权限运行，可注册同名的 read/grep/find/ls 覆盖内置工具，扩展的钩子包着每次工具调用，扩展自己也能写文件”。
- **REQ-026 行为 9**：help 的条件删去 `--no-extensions`；第二段改为“Pi 的只读只靠进程内按工具名的允许清单：扩展以用户的完整权限运行，可注册同名工具覆盖这四个内置工具，扩展的钩子包着每次工具调用，扩展自己也能写文件。这些记为余项（ADR-032 R3）（CR-024）。”

**(C) pi 不当 Commander。** 这缩小 v1 的范围，须所有者另行决定，本附录不给逐字文字；要改的是 REQ-021 行为 9（删去 pi 一行，写明 pi 的 read_only_workspace 恒为 false）、REQ-026 行为 9、REQ-027 行为 2 与 AC-021-06 (a)、AC-021-07 中的 pi。
