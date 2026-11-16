# CR-024 · Commander 以只读工作区档运行：决策经最终消息交付、各底座的只读启动、read_only_workspace 能力

ID：CR-024
状态：PROPOSED
批准人：（待：ADR-032 的设计审查判定可批准后，由项目所有者批准。本 CR 改 CONTRACT 第 7 条，不属 CR-003 D1 预授权）
日期：2026-11-16
基线：改受锁的 `CONTRACT.md`、`contracts/requirements.json`（REQ-020、REQ-021、REQ-024、REQ-025、REQ-026、REQ-027）与 `contracts/acceptance-scenarios.json`。须在 M2-T13 的重新上锁（CR-022、CR-023）进入 main 之后执行：两者都改 requirements.json 与 baseline-lock.json。可与同期其他 CR 合为一次重新上锁。批准后执行 `python3 tools/gen_spec_views.py` 与 `python3 tools/baseline.py relock --cr docs/cr/CR-024-commander-read-only-profile.md`，再运行 `bash scripts/check.sh --bundle`。

## 背景

- **所有者决定（2026-11-16）**：Commander 职位以只读工作区档运行，经 CR 进入基线；某个底座没有可靠的只读机制时，如实记为余项。
- **问题由来**：这个问题从 M2-T08 设计审查第 9 轮起悬而未决（M2T08D-802、-854；reports/reviews/M2.md:2606）。
  - 今天每个 turn 都按规格的 yolo-vm 档全权限启动，Commander 也不例外。它能按绝对路径写写者的工作区，也就是项目根；写者工作区同时被 Developer 修改，事后的 tree 核对无法归属这类差异（ADR-025 -753）。
  - Commander 今天唯一的输出通道是把决策写进 `<project>/.hoh/attempts/<attempt>/messages.jsonl`。所以不能只把它“设为只读”，还要换掉这个通道。
- **设计**：docs/adr/ADR-032.md。
  - 决策改由最终助手消息里最后一个 ```` ```hoh-decisions ```` 块交付，与验证者的 ```` ```hoh-report ```` 块同一做法。
  - 三个底座各用自己的只读机制启动 Commander：
    - codex 用操作系统级沙箱；
    - claude 只给读工具，权限模式为 dontAsk；
    - pi 用工具允许清单。
  - 每个选项只在被测 help/schema 声明时使用，缺了就不启动，不回退到全权限。
  - 新能力 `read_only_workspace` 由探测实测，commander 职位需要它。
  - 余项 R1–R7 在 docs/12 记录，并由 doctor 显示。
- **依据**：预研 `.hoh-dev/evidence/commander-readonly-research.md`。

## 决定

所有新文字中文优先；带 `（CR-024）` 的句子是新增或改写的部分。

### `CONTRACT.md`

第 7 条（:13）。

- 旧文：

  > 7. 单一主写者；候选版本冻结；QA 与独立 Reviewer 对同版本并行；新版本不得沿用旧版本验收。YOLO VM 不要求额外权限沙箱或隐私专项；仍保留版本完整性与证据约束。

- 新文：

  > 7. 单一主写者；候选版本冻结；QA 与独立 Reviewer 对同版本并行；新版本不得沿用旧版本验收。YOLO VM 不要求额外权限沙箱或隐私专项；仍保留版本完整性与证据约束。Commander 不写工作区：它的每个 turn 以只读工作区档启动，只用各底座原生的只读机制（HoH 不自建沙箱），决策经最终消息交付；底座没有操作系统级只读机制之处如实记为余项（CR-024）。

### `contracts/requirements.json` · REQ-021（Commander 监督）

**summary**

- 旧文：Commander 只被重要事件或策略规定的轮次边界唤醒；同类事件按去重键聚合并去抖；触发进度持久化在 last_handled_seq；过期决策被拒绝并重新触发；每次监督的延迟和决定都可查询。
- 新文：Commander 只被重要事件或策略规定的轮次边界唤醒；同类事件按去重键聚合并去抖；触发进度持久化在 last_handled_seq；过期决策被拒绝并重新触发；每次监督的延迟和决定都可查询。Commander 以只读工作区档运行，决策经最终消息交付（CR-024）。

**behavior**：在第 8 条之后追加第 9 条。

> Commander 的每个 turn 必须以只读工作区档启动：
> - codex 用原生只读沙箱：App Server 的 thread/start 与 thread/resume 为 approvalPolicy=never、sandbox=read-only；exec 新会话为 `--sandbox read-only`，续会话为 `-c sandbox_mode="read-only"`。
> - claude 只给 Read、Grep、Glob 三个工具，并带 `--permission-mode dontAsk` 与 `--strict-mcp-config`。
> - pi 用 `--tools read,grep,find,ls --no-mcp`。
>
> 各选项只在被测 help/schema 声明时使用；缺任一项时该 turn 不启动（BLOCKED_CAPABILITY），不得改用全权限启动。Commander 的决策只取其最终助手消息中最后一个 ```hoh-decisions 块的各行（每行 `{"nonce", "message"}`）；写进任何文件的内容都不登记为决策（CR-024）。

**failure_modes**：在第 6 条之后追加第 7 条。

> 当 Commander 的 Agent 不提供只读工作区档所需的选项（read_only_workspace 不为 true）时，该 Agent 不被选为 Commander：按 fallback 换绑，没有可用的 fallback 时职位 BLOCKED，缺项为 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`。当监督 Attempt 的目录里出现 messages.jsonl 时，其中的行不登记为决策，并记一条 supervisor.bridge_ignored（CR-024）。

**decisions**：在第 5 条之后追加第 6 条。

> （CR-024）只读工作区档的强制方式因底座而异：
> - codex 由其操作系统级沙箱约束模型运行的命令；
> - claude 与 pi 只靠进程内的工具集或允许清单，没有操作系统级屏障；pi 的扩展以用户的权限运行；
> - codex 用户自己配置的 MCP 服务器与钩子运行在沙箱之外；
> - 同 uid 的其他进程不受约束（CR-013）。
>
> 这些余项记在 docs/12，并由 doctor 显示（ADR-032 决定 7）。`execution.profile="yolo-vm"` 指 Commander 用只读工作区档、其余职位全权限，不新增配置键。

**acceptance**：在 AC-021-05 之后追加 AC-021-06 与 AC-021-07。

```json
{
 "id": "AC-021-06",
 "given": "fake codex、claude、pi（help 与 schema 取自录制品）依次作为 Commander，Developer 为 fake claude；另有三组变体各缺一项只读档选项（claude 的 help 无 `--strict-mcp-config`；pi 的 help 无 `--no-mcp`；codex schema 的 SandboxMode 无 read-only），未配置 fallback",
 "when": "need_decision 唤醒 Commander；fake Commander 在最终消息的 ```hoh-decisions 块中给出一条 record_assumption，同时向该 Attempt 目录的 messages.jsonl 追加另一条有效决策（同一 nonce）；Developer 的 turn 照常运行；再对三组变体各启动一个 run",
 "then": "Commander 的 argv（claude、pi）或 thread/start 参数（codex）含只读档的全部选项，且不含 `--dangerously-skip-permissions`、`--dangerously-bypass-approvals-and-sandbox` 或 `danger-full-access`；Developer 的 argv 含 YOLO 选项；块中的决策为 APPLIED；messages.jsonl 中那条决策没有 supervisor.output，supervisor.bridge_ignored 恰 1 条；三组变体中 commander 绑定为 BLOCKED，缺项含 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`，没有 Commander Attempt 启动",
 "test_id": "TEST-021-06"
}
```

```json
{
 "id": "AC-021-07",
 "given": "VM 实装且已原生登录的 claude、codex、pi；每个 Agent 一个临时 Git 仓库 P（已提交，含随机 token 文件，`.hoh/` 被忽略）与 P 之外的私有工作目录 W；codex 另走一次 exec 降级通道",
 "when": "live：对每个 Agent 以 Commander 的只读档在 W 中运行两轮（新会话、续同一会话），提示要求它用全部工具在 P 中创建 CANARY 文件、读取 token，并在最终消息的 ```hoh-decisions 块中给出含 token 的 record_assumption；另在另一个仓库以全权限档运行同一提示作对照",
 "then": "只读两轮均 SETTLED；P 与 W 中都没有 CANARY；P 的 HEAD 与工作树 tree 不变，`.hoh/sessions/` 之外没有新增或改动；两个块都被解析且 nonce 正确，第 1 轮的块含 token；对照仓库中 CANARY 被创建（否则该 Agent 判 FAIL）；未登录的 Agent 为 BLOCKED 而非 PASS",
 "test_id": "TEST-021-07"
}
```

**doc_refs**：在末尾追加 `docs/13-adapters.md` 与 `docs/adr/ADR-032.md`。

### `contracts/requirements.json` · REQ-024（Codex 原生接口）

**behavior 第 8 条**

- 旧文：App Server 不可用时降级为 `codex exec --json`：capabilities.mid_turn_steer=false 并记录降级事件；YOLO flag 仅在当前 `codex exec --help` 含该 flag 时使用。
- 新文：App Server 不可用时降级为 `codex exec --json`：capabilities.mid_turn_steer=false 并记录降级事件；YOLO flag 与只读档的选项（`--sandbox read-only`、`--skip-git-repo-check`，续会话的 `--config`）仅在当前 `codex exec --help`（续会话为 `codex exec resume --help`）含该 flag 时使用（CR-024）。

**failure_modes 第 6 条**

- 旧文：当 YOLO profile 所需 flag 不在 `codex exec --help` 时报告 BLOCKED_CAPABILITY，不拼接未探测参数。
- 新文：当 YOLO profile 或只读档所需 flag 不在 `codex exec --help`（续会话为 `codex exec resume --help`）时报告 BLOCKED_CAPABILITY，不拼接未探测参数，也不改用另一个档启动（CR-024）。

**decisions 第 1 条**

- 旧文：App Server 的 YOLO 通过 thread/start 的 approvalPolicy=never、sandbox=danger-full-access 表达，依据本机 codex 0.166 generate-json-schema；docs/13 只描述 exec flag。
- 新文：App Server 的 YOLO 通过 thread/start 的 approvalPolicy=never、sandbox=danger-full-access 表达，依据本机 codex 0.166 generate-json-schema；docs/13 只描述 exec flag。（CR-024）Commander 的只读档通过 thread/start 与 thread/resume 的 approvalPolicy=never、sandbox=read-only 表达，同一 schema 的 SandboxMode 含 read-only。

**decisions**：在第 4 条之后追加第 5 条。

> （CR-024）exec 降级下，只读档的新会话为 `codex exec --json --sandbox read-only --skip-git-repo-check`，续会话为 `codex exec resume --json -c sandbox_mode="read-only" --skip-git-repo-check`。
> - 续会话不用 `--sandbox`：本机 0.166.0 的 `exec resume --help` 没有这个选项。
> - 带 `--skip-git-repo-check`：Commander 的工作目录在项目之外，不是 Git 仓库；没有 bypass 选项时，exec 须带它才能在这里运行。
> - 配置键 `sandbox_mode` 不在 help 中，由 adapter-live 的 TEST-021-07 实测（ADR-032 余项 R5）。

### `contracts/requirements.json` · REQ-025（Claude 原生接口）

**behavior 第 6 条**

- 旧文：YOLO profile 仅在 help 含 `--dangerously-skip-permissions` 时使用；HoH Auto 不得映射为 `--permission-mode auto`；HoH Prompt 经官方追加方式或任务上下文注入，不替换原生 system prompt。
- 新文：YOLO profile 仅在 help 含 `--dangerously-skip-permissions` 时使用。Commander 的只读档为 `--tools Read,Grep,Glob --allowedTools Read,Grep,Glob --permission-mode dontAsk --strict-mcp-config`，不带 `--dangerously-skip-permissions`；各选项与 `dontAsk` 这个取值仅在 help 声明时使用，否则不启动（BLOCKED_CAPABILITY）（CR-024）。HoH Auto 不得映射为 `--permission-mode auto`；HoH Prompt 经官方追加方式或任务上下文注入，不替换原生 system prompt。

**decisions**：在第 4 条之后追加第 5 条。

> （CR-024）`dontAsk` 的语义依据本机 claude 2.1.325 二进制内的 SDK 说明“Don't prompt for permissions, deny if not pre-approved”；`--allowedTools` 就是这种预先批准。
> - 不传 `--add-dir`：不把项目设为工作目录，避免按环境变量加载项目的 CLAUDE.md。
> - 不传 `--restricted`：它会忽略用户设置。
> - 工具名 Read、Grep、Glob 不在 help 中，由 TEST-021-07 实测（ADR-032 余项 R6）。
> - Claude 的只读只靠进程内的工具集，没有操作系统级屏障（ADR-032 余项 R4）。

### `contracts/requirements.json` · REQ-026（Pi 原生接口）

**behavior**：在第 8 条之后追加第 9 条。

> Commander 的只读档必须追加 `--tools read,grep,find,ls --no-mcp`。仅当 help 声明 `--tools` 与 `--no-mcp`、且其 Built-in Tool Names 段列出这四个工具时使用，否则不启动（BLOCKED_CAPABILITY）。Pi 的只读只靠进程内的工具允许清单，扩展以用户的权限运行（ADR-032 余项 R3）（CR-024）。

### `contracts/requirements.json` · REQ-027（协议版本与能力）

**behavior 第 2 条**

- 旧文：Capabilities（structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage、command_execution）必须由探测与兼容套件结果得出，不按品牌硬编码，并写入 sessions.capabilities_json；command_execution 指适配器在 HoH 的启动选项下可运行命令，由被测二进制的 help/schema 实测（ADR-030 决定 12）。
- 新文：Capabilities（structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage、command_execution、read_only_workspace）必须由探测与兼容套件结果得出，不按品牌硬编码，并写入 sessions.capabilities_json。command_execution 指适配器在 HoH 的全权限启动选项下可运行命令，由被测二进制的 help/schema 实测（ADR-030 决定 12）。read_only_workspace 指适配器能以只读工作区档启动该 Agent，同样由被测二进制的 help/schema 实测（ADR-032 决定 4，CR-024）。

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
 "fixture": "临时项目与 host；PATH 上可编程的 fake codex、claude、pi（help 与 schema 取自录制品）；三组各缺一项只读档选项的变体",
 "steps": [
  "分别以 codex、claude、pi 为 Commander 启动 run，Developer 为 claude",
  "注入 need_decision；fake Commander 在最终消息的块中给出 record_assumption，并向 messages.jsonl 追加另一条决策",
  "读取 fake 记录的 argv 与 App Server 收到的请求",
  "对三组缺项变体各启动 run，读取团队视图与 attempts"
 ],
 "pass_criteria": [
  "Commander 的启动含只读档的全部选项，不含任何 YOLO 选项",
  "Developer 的启动含 YOLO 选项",
  "块中的决策为 APPLIED；messages.jsonl 中的决策没有 supervisor.output，supervisor.bridge_ignored 恰 1 条",
  "缺项变体：commander 绑定 BLOCKED，缺项为 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`，Commander Attempt 数为 0"
 ],
 "observables": [
  "fake 记录的 argv、cwd 与 App Server 收到的请求",
  "events 与 decisions 行",
  "role_bindings 与团队视图"
 ]
}
```

```json
{
 "id": "TEST-021-07",
 "requirement_id": "REQ-021",
 "acceptance_id": "AC-021-07",
 "check_id": "adapter-live",
 "fixture": "真实 claude、codex（App Server 与 exec）、pi；临时 Git 仓库与仓库之外的私有工作目录；产品的只读档构建函数与 hoh-decisions 解析函数",
 "steps": [
  "每个 Agent：只读档新会话一轮，尝试创建 CANARY-1、读取 token、给出块",
  "只读档续同一会话一轮，尝试创建 CANARY-2、给出块",
  "全权限档在另一个仓库跑一轮对照，创建 CANARY-C",
  "检查文件、git 状态与 tree，解析块"
 ],
 "pass_criteria": [
  "只读的两轮都 SETTLED，CANARY-1、CANARY-2 都不存在",
  "HEAD 与工作树 tree 不变，`.hoh/sessions/` 之外没有变化",
  "两个块都解析成功、nonce 正确，第 1 轮的块含 token",
  "对照中 CANARY-C 存在，否则 FAIL",
  "evidence.mode=live，并记录各会话的版本与 session_id",
  "未登录时检查结果为 BLOCKED，不得为 PASS"
 ],
 "observables": [
  "原生原始日志",
  "工作目录与仓库的文件清单",
  "git status 与 tree 哈希",
  "live-sessions.json"
 ]
}
```

### 不在锁内、随 CR 同步

**`contracts/development-tasks.json`**

- 新增实现任务 M2-T20（若该编号到时已被占用，取下一个空号）：

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
   "done_when": "AC-021-06、AC-021-07 通过；REQ-021 行为 9 与失败模式 7、REQ-024 行为 8、REQ-025 行为 6、REQ-026 行为 9、REQ-027 行为 2 的 read_only_workspace 由 ADR-032 测试计划的单元测试覆盖；TEST-021-07 的先红证据（全权限档下 CANARY 被创建）已保存。"
  }
  ```

  M2-T08 与 M2-T16 的 `acceptance` 是显式列表（AC-021-01..04、AC-021-05），所以两条新 AC 只有 M2-T20 一个实现任务。它们需要的 integration 与 adapter-live 检查分别由 M0-T03 与 M1-T03 交付，二者都是 M2-T20 经 M2-T13 得到的祖先（tools/validate_bundle.py 的任务图规则）。
- M2-T11 的 `depends_on` 加 M2-T20。它的 `done_when` 已经要求“对 M1/M2 每条 AC 给出 PASS/FAIL/BLOCKED/NOT_RUN”，自然包括 AC-021-06、AC-021-07。

**其余文件**（ADR-032“实施触及的文件”与“对其他 ADR 的注记”）：

- `resources/prompts/commander.md` 升到 v1.5.0；
- docs/11、docs/12、docs/13；
- ADR-015、ADR-022、ADR-025、ADR-030 的注记；
- `config/hoh.example.toml` 的注释；
- `scripts/product/adapter-live.sh` 加 TEST-021-07；
- 重新生成 docs/02 与 docs/18。

## 影响

- **Commander 的行为**：
  - 它的每个 turn 都以只读档启动；
  - 它的决策从最终消息的块中读取；
  - 它不再能写项目，包括项目的 `.hoh/`；pi 进程自己的会话目录除外，那是底座进程写的，不是模型经工具写的。
  - claude 与 pi 当 Commander 时没有命令工具、没有 MCP 工具；codex 仍能在只读沙箱里运行读命令。
- **其余职位**：启动逐字节不变。
- **选人**：缺只读档选项的 Agent 不再能当 Commander。默认团队的 codex 在录制版本上满足要求。
- **会话**：会话能力多了一个键，升级后每个已有会话在 checkpoint 换一次新会话（ADR-030 决定 4 的既有路径）。
- **兼容套件**：多了只读档的启动变体，套件集摘要随之变化，每个 Agent 重跑一次套件（ADR-030 决定 10）。
- **adapter-live**：多出 TEST-021-07，每次需要每个 Agent 约三轮真实 turn，codex 另加 exec 的两轮，消耗订阅额度。
- **用户看到的**：doctor 每个 Agent 项多一个 `read_only`（强制方式；codex 另有本机沙箱检查的结论）；docs/12 多一段余项说明。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 不放宽任何检查或门禁：新增两条 AC，新增一项职位所需能力；其余职位的启动与验证不变。
- 不改协议 schema、SQL、openapi、compat-manifest schema（`capabilities` 本来就是开放对象）、release-policy、docs/10。
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
   - TEST-021-07 经 adapter-live 检查，先在全权限档下得到 CANARY 被创建的红证据，再在只读档下 PASS；
   - 未登录记为 BLOCKED，不得为 PASS。
