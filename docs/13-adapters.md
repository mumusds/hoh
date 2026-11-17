# Agent适配器设计与兼容性

## 统一接口（HoH自定义，非上游API）

probe()->Capabilities；start(ExecutionRequest)->AttemptHandle；send(Input)->DeliveryReceipt；subscribe()->EventStream；cancel()->CancelReceipt；resume(SessionRef)->AttemptHandle；inspect()->ObservedState；shutdown()->Result。

能力包括structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage、command_execution（CR-021）、read_only_workspace（CR-024）。按版本/测试得到，不按品牌推断；判不了为 null（未知），只有 true 才算具备。Request带cwd、role、session_ref、prompt_bundle、budget、model_optional、runtime_profile。使用argv而非shell；模型字段为空即底座实际默认，执行后记录resolved model。

**command_execution**（ADR-030 决定 12）：适配器在 HoH 的启动选项（YOLO）下能运行命令。只由被测二进制的 help/schema 得出，每种 transport 一条解析规则，以未改动的录制品为正对照：

- codex App Server：产品 YOLO 启动给出的 `thread/start`（`approvalPolicy: "never"`、`sandbox: "danger-full-access"`）对被测 `ClientRequest.json` 合法，且 `params.item.type` 为 `commandExecution` 的 `item/started` 帧对被测 `ServerNotification.json` 合法（都不放开 `params`）；两者都“是”为 true，有“否”为 false，否则 null。
- codex exec：`exec --help` 声明 YOLO 选项 `--dangerously-bypass-approvals-and-sandbox`。
- claude：help 声明 `--dangerously-skip-permissions`，且 `--tools` 的说明列出工具名 `Bash`（每个词剥去括号、引号与句末标点，按逗号拆分，逐段精确比较）；`--tools` 未声明为 null。`--restricted` 的说明不作依据。
- pi：help 的 `Built-in Tool Names:` 段有名字恰为 `bash` 的一行（名字与说明在第一个 ` - ` 处分开，去掉首尾空白），且其说明不含 `off by default`；没有该段为 null。

会话的能力是探测值与适配器在这种启动下实现的能力相与（claude、codex 在 YOLO 时，pi 总是），写进 `sessions.capabilities_json`。会话记录不随职位变化：它按 YOLO 启动测量，所以 Commander 会话行上的 `command_execution` 是该适配器全权限下的能力，而 claude、pi 的 Commander turn 本身没有命令工具（ADR-032 决定 4）。除 Commander 的 turn 外，HoH 不传 `--tools`、`--no-tools` 之类缩小工具集的选项；Commander 的 turn 按只读工作区档启动（claude 为 `--tools Read,Grep,Glob --allowedTools Read,Grep,Glob --permission-mode dontAsk --strict-mcp-config`，pi 为 `--tools read,grep,find,ls --no-mcp --no-extensions`，不加载任何扩展；codex 的线程为 `approvalPolicy: "never"`、`sandbox: "read-only"`；REQ-021 行为 9，ADR-032 决定 3）。command_execution 离线的 true 只表示被测版本声明了运行命令的内置工具、并在 HoH 的启动选项下可用；真实 turn 里命令能否运行由 adapter-live 检查记录。

**read_only_workspace**（CR-024，ADR-032 决定 4）：适配器能以只读工作区档启动该 Agent。会话记录里的值是三项之与，都由被测二进制得出：探测规则（claude、pi 的 help 声明只读档的每个选项与取值；codex App Server 的只读 `thread/start`、`thread/resume` 对被测 schema 合法；codex 只有 exec 时为 false）、只读档的构建函数不拒绝、兼容清单里该版本的 `read_only` 用例为 PASS（它不计入 `suite.status`，FAIL 或 NOT_RUN 只使这一项不为 true）。选人时 codex 另看本 host 的两项事实：本机沙箱检查（`codex sandbox -P :read-only`，以临时 `CODEX_HOME` 运行）不为 unconfined 或 unavailable，App Server 没有在本 host 运行中降级为 exec；它们只从 `granted` 里去掉这项能力，不改会话记录，不使任何会话换绑。离线的 true 是这三项之与，只说明被测版本能以只读档启动；只读档在真实 turn 里是否生效由 adapter-live 检查的 TEST-021-07 证明（M2T20R-50）。

**职位所需能力**（REQ-020 决定 1，ADR-030 决定 12）：所有职位要 structured_events；commander、reviewer 另要 resume；developer、qa 另要 command_execution；commander 另要 read_only_workspace，按职位名判定，引用 commander 合同的自定义职位不要（CR-024，ADR-032 决定 4）。缺任一项的 Agent 不被选给该职位，判决行为 `<agent>:CAPABILITY_UNSUPPORTED:<缺的能力名>`（见“交接和Fallback”）。

## Codex

生产首选App Server stdio：initialize→initialized→thread/start或resume→turn/start→item事件→turn/completed，失败/取消区别于completed success [S03]。关联threadId/turnId/itemId；turn/steer需expectedTurnId，过期拒绝。未知字段原样留raw旁路，不panic。

早期/不支持App Server时可用exec --json一次调用；该模式不提供等价持续控制，不得静默承诺mid_turn_steer。YOLO flag以当前--help探测，示例 `--dangerously-bypass-approvals-and-sandbox`；无flag则报告能力不足，不猜成不存在参数。

## Claude

-p + --output-format stream-json + --verbose，按需求启用include-partial-messages [S05,S06]。session元数据、消息/tool、result输出分开。error/interrupt/max-turns不等成功。stdio stream-input与resume以版本帮助和真样例确认，v1至少正确实现分轮resume与有界cancel。

YOLO常见为--dangerously-skip-permissions，等价底座bypassPermissions；不将HoH Auto映射Claude permission auto。不是改掉原生system prompt，而是用官方支持的追加/Agent规则或任务上下文。非 Commander 的 turn 保持原生工具能力；Commander 的 claude 只有 Read、Grep、Glob，权限模式为 dontAsk（CR-024，ADR-032 决定 3）。

## Pi

--mode rpc，启用真实持久session-dir/session-id；不使用--no-session。prompt命令有correlation id；stdout按LF字节分帧，不能用把U+2028也分行的阅读器。先订阅再发送，持续drain stdout/stderr，避免管道阻塞 [S07,S08]。

prompt disposition=handled表示没有启动模型run，不能等永不存在的settled。started/queued的最终静止按版本支持的agent_settled；agent_end后可能压缩/重试继续。未知版本无settled时必须通过state/队列/重试状态验证或明确不支持，不能加sleep猜空闲。

RPC ctx.hasUI=true只表示部分协议UI能力。confirm/select/input/editor需要发送response或cancel，否则死锁；footer/custom等不能按完整TUI处理 [S09]。HoH渲染自己的状态栏，rpiv overlay映射数据而非嵌入Pi画面。headless交互必须按策略取消/记录阻塞，不伪造真人选择。

## 进程与终结

独立进程组；命令超时/取消先native cancel，等待短grace后SIGTERM整组，再有界SIGKILL并wait/reap。读stdout/stderr并行；协议frame上限1MiB初值，超限保存诊断并fail，不无限buffer。wall deadline与inactivity watchdog分开，长测试无输出不直接当死机。

对象销毁不意味着子进程树已结束，Tokio句柄生命周期需显式设计 [S23]。启动新attempt前确保同写租约旧进程不再写；否则block。

## 版本锁和协议样例

实际安装版本、launcher 与实际实现的 path/hash（pi 的 launcher 是 sh 脚本，实际实现在 install/releases/<版本>；codex 的 npm 包装下是 vendored 原生二进制）、--help hash、native schema hash、兼容套件的结果与套件集摘要进入compat manifest。fixture含成功/失败/取消/压缩/工具/未知字段。升级先在无写任务的维护窗口跑compat suite，再用于新attempt。脚本安装成功不等协议兼容。

**compat manifest 结构**：`contracts/compat-manifest.schema.json`（CR-003）。**是否已验证**（ADR-030 决定 9；staffing、prepare、闸门与 doctor 共用同一个判定 `compat::verdict`）：清单中该 Agent 的项与当前探测的完整身份（启动器路径与 sha256、实现路径与 sha256）、help 与 schema 哈希、`--version` 原文都相等，且 `suite.fixture_set_sha256` 等于本构建的套件集摘要，才算相符；相符且 PASS 为已验证，相符且 FAIL 为失败，相符、NOT_RUN 而套件跑过（`run_at` 非空）为判不了，无清单、无该项、不相符或只探测未跑套件为待验证。只有已验证的版本可用于新 attempt，其余 COMPAT_UNVERIFIED。比 schema 标题多比较的几项只会使更多版本未验证。doctor 对已验证的版本按 transport 给出 compatible/degraded，其余记 unknown，说明写出原因（FAIL 的用例、判不了的部分、身份或哈希不符、套件集不是本构建的）。

**兼容套件**（ADR-030 决定 10，REQ-027 行为 3）。用例写进清单的 `suite.cases`：

- `probe`：对被测启动器重做一次完整探测，得出的条目须与注册表查看时相同（身份与三个哈希），否则丢弃这次运行，下一次查看重来。
- `success`、`failure`、`cancel`、`compaction`、`tool`、`unknown_fields`：每类是三项的与。
  1. 请求符合性：适配器自己的构建函数（驱动与套件调用同一组函数）为该类生成的每个请求，对被测二进制核对。codex App Server 的请求以被测 `ClientRequest.json`、`initialized` 以 `ClientNotification.json`、对审批请求的无头答复以导出中该请求的响应 schema 校验，不放开 `params`。codex exec、claude、pi 没有 schema 导出，对各启动变体（YOLO、有无 model、新会话与续会话）的 argv 核对被测 help：每个选项须被声明，HoH 传的固定取值（`--output-format stream-json`、`--mode rpc`、`--listen stdio://`）须被列出，子命令（codex 的 `exec`、`resume`）须被列出；构建函数自己拒绝（如 help 没有 YOLO 选项）也是不符合。
  2. 帧接纳（只 codex App Server）：嵌入帧中适配器据以判断的 `turn/completed`、`item/started`、`item/completed` 对被测 `ServerNotification.json` 合法。“否”说明被测版本不会再发出适配器所读的形状；“是”不证明它会发出。
  3. 回放：嵌入 fixture 经适配器回放，终态、失败码与 `adapter.unknown_event` 数等于其 `expected.json`。

  任一项“否”→ 该用例 FAIL；没有“否”而有判不了（校验器给不出结论、步数或时限用完、导出缺所需的响应 schema）→ NOT_RUN；全“是”→ PASS。
- `live`：恒为 NOT_RUN。新版本在真实 turn 中的响应帧，以及 claude 经 stdin 的提示词、pi 的 RPC 命令这些没有 schema 的载荷，离线判不了，不在 PASS 的依据之内；它们由 adapter-live 检查单独记录（REQ-027 决定 2）。产品从不运行这个用例（会产生费用）。

套件 status：`probe` 与六类都 PASS 为 PASS，任一 FAIL 为 FAIL，否则 NOT_RUN（判不了）；三种都写 `run_at` 与 `cases`。判不了从不写成 PASS；同一身份的 FAIL 与判不了不重跑，身份或套件集变了才重跑。

**嵌入 fixture 与套件集**：合成 fixture（每个 Agent 六类的 jsonl 与 `expected.json`，连同各类的启动变体表）编进产品，不含录制机器的路径与凭据；录制的真实样例（tests/fixtures/protocol）只留在测试里，不随产品分发。`suite.fixture_set_sha256` 记本构建的套件集摘要（CR-021）：嵌入 fixture 与变体表、请求构建函数以固定占位输入对每个启动变体实际给出的消息或 argv、hoh-agents 的套件定义摘要（该 crate 的 `src/**`、Cargo.toml、帧注册表与工作区 Cargo.lock 的摘要），按 (名字, sha256) 排序后求摘要。所以一个 HoH 构建只接受它自己的套件得出的 PASS：上一构建的 PASS 即使 Agent 完全不变也只是待验证，HoH 每升级一次，每个 Agent 在写租约空闲时重跑一次（离线，秒级），期间新 turn 等待。

**何时运行与互斥**（ADR-030 决定 10，REQ-027 失败模式 5）：产品没有运行套件的命令或 API；host 的注册表每次查看之后，对待验证的 Agent 一次一个地准入套件。项目的写租约被有效持有（未过期，或持有者 Attempt 未结束）时不运行，写 `compat.suite_deferred{agent, cli_version, implementation_sha256, lease_attempt_id, lease_epoch, reason: "write_lease_held", detail: "维护窗口：写租约释放后自动运行兼容套件"}`（每个身份与持有者一条）。第一次延后或准入时套件即“打开”（`compat.suite_opened`），运行时另写 `compat.suite_started`；打开期间写者槽不授予新的持有者（读者槽与监督槽照常），所以当前写任务结束后的下一次查看即运行，不会被接连的写任务饿死。同一 host epoch 至多一个打开的套件，别的 Agent 待验证时等它结束。每条命令以探测的时限为界，运行超过 5 分钟即停止（`compat.suite_ended{status: "timed_out"}`）；host 停止时为 `abandoned`，该身份不再需要时为 `withdrawn`；崩溃留下的由新的 host epoch 关闭。

**发布**：`.hoh/compat-manifest.json` 只由 host 写，doctor 只读。发布以内存中的当前清单为底，只替换本 Agent 一项，按受锁 schema 校验，写同目录临时文件、fsync、rename、fsync 目录，在同一事务里核对套件仍打开并写 `compat.suite_ended{status}` 与 `compat.suite_completed{agent, status, implementation_sha256, fixture_set_sha256, cases}`。结束时启动器或实现的身份戳与开始时不同则不发布（`discarded`），写不进为 `unpublished`（至多每 30 s 重试）。host 启动时读清单，读不了或不合 schema 即视为空，并记 `compat.manifest_unreadable`。

**启动前的核对**（ADR-030 决定 9，prepare）：每个新 Attempt 在准备其他任何东西之前，取注册表此刻该 Agent 的一份事实，只用它依次核对：(a) 启动器与实现的身份戳（跟随链接的 dev、ino、ctime）与那次查看时相同；(b) 已验证；(c) 已登录；(d) 所派会话对它新鲜：会话的 `agent_version`、`capabilities_json` 与授予时的身份（`team.session_bound.identity`）都相等。任一不成立即在 spawn 之前拒绝（Attempt FAILED，`attempt.start_refused`；(c) 为 BLOCKED_AUTH，其余为 COMPAT_UNVERIFIED），没有子进程，Task BLOCKED；COMPAT_UNVERIFIED 的 Task 等该 Agent 已验证、Rebind 给出新会话后恢复。闸门的期望值也取自这份事实，所以“会话新鲜”“已验证”“闸门执行的”是同一个身份。会话只授予已验证的版本。

**检查的启动闸门**（ADR-030 决定 9，扩展 ADR-022 决定 13）：产品以 `hoh __gate --expect <agent> <launcher_path> <launcher_sha256> <implementation_path> <implementation_sha256> --stamp <启动器的 dev:ino:ctime_sec:ctime_nsec> <实现的同样四项> --plan <fd|path> --status <path> -- <program> <args…>` 启动 Agent。检查用的选项一同给出，缺一即以 64（用法错误）退出、不执行；都不给是无核对的闸门，只用于直接驱动闸门的测试。

- **go 行带钥匙**：适配器开闸写 `go <钥匙>` 加换行，钥匙是每个 Attempt 32 字节随机数的小写 hex，只在 HoH 内存里，不进 argv、环境、库、事件或任何文件。闸门逐字节读到换行；有 `--status` 时只认 `go ` 加 64 位小写 hex，否则以 64 退出、不执行。读完这一行钥匙就离开了管道，Agent 继承的 stdin 只剩其后的字节。
- **exec 之前的核对**：按探测的规则重新解析实现，须得到同一路径；以 `O_RDONLY|O_CLOEXEC` 打开启动器与实现，经描述符计算 sha256，须等于期望值；实现哈希前后两次 `fstat` 的 (dev, ino, ctime) 须相同；两个描述符的 `fstat` 须等于 `--stamp`。
- **结果记录**：每个决定一行 JSON，追加到该 Attempt 日志目录下的 `gate.jsonl`（`O_APPEND|O_NOFOLLOW`，0600）。不符 → `{"outcome": "refused", "key", "code": "COMPAT_UNVERIFIED", "item", "stamp_only", "expected", "seen"}`，向 stderr 写一行 `hoh gate: COMPAT_UNVERIFIED: <项>`，以 65 退出，Agent 没有运行；通过 → `{"outcome": "verified", "plan": "E1"|"E2", "implementation": {"path", "dev", "ino", "ctime_sec", "ctime_nsec"}}`（不带钥匙），再 exec；exec 失败 → 追加 `{"outcome": "exec_failed", "errno"}`，以 127 退出；记录写不进 → 不 exec，以 65 退出（没有记录，按 Agent 自己的失败处理）。只有 `refused` 带钥匙，写下它的闸门随即退出。记录随日志目录作为该 Attempt 的原始日志保存。
- **适配器怎样读**：只有钥匙等于本 Attempt 钥匙的 `refused` 行算闸门拒绝（多于一行取最后一行；结局 COMPAT_UNVERIFIED，另有 `adapter.diagnostic{reason: "GATE_REFUSED", item, expected, seen}`）；退出码、帧数、stderr 以及 `verified`、`exec_failed`、别的钥匙、解析不了的行都不是来源。所以已验证的 Agent 自己在首帧之前以 65 退出、写出同样的 stderr，或往 `gate.jsonl` 追加伪造的行，仍按它自己的失败（如 PROTOCOL_INCOMPLETE，瞬时类）结算。HoH 不防护能 ptrace HoH 进程的同用户程序（它同样能改写 `.hoh/` 的库）。
- **执行计划**：HoH 执行的总是已解析、已核对的实现，不再交给会在运行时重新解析的包装；包装仍在身份里，改了包装就是新身份。E1，实现是 ELF（claude 的版本化二进制、直接安装的 codex、codex npm 包装解析出的 vendored 原生二进制）：经刚核对的描述符执行，即执行 `/proc/self/fd/<n>`、argv[0] 为实现路径（工作区禁止 unsafe，所以不用 `execveat`，内核语义相同，ADR-030“实施中的更正”），路径之后被 rename、删除或改指不影响这次执行；codex npm 包装另加 `CODEX_MANAGED_PACKAGE_ROOT=<包根>` 与 `CODEX_MANAGED_BY_NPM=1`。E2，实现是脚本（pi 的 `dist/bundle/cli.js`、npm 安装的 claude 的 `cli.js`、自身即实现的脚本）：按核对过的 shebang 在 PATH 上找解释器，以实现路径为参数执行；pi 另加它的 sh 启动器设置的 PATH 前缀与 `PI_MANAGED_INSTALL_ROOT`，版本目录由闸门的解析钉住。不认的布局（启动器即实现）按实现的种类走 E1 或 E2。

**两次再核对**（ADR-030 决定 9，所有者 2026-11-13、2026-11-14 的决定）：都以 `stat`（跟随链接）比较启动器与实现路径的 (dev, ino, ctime) 与内存中的期望身份戳，不读任何记录文件。

- 首个输入之前：只用于 HoH 先说话、Agent 在工作输入之前先答复一次的 transport，即 pi 的 RPC（`get_state` 的答复之后、`prompt` 之前）与 codex App Server（`initialize` 的答复之后、`thread/start` 之前）。不等或读不出 → 不发输入，走该适配器“不发输入即关闭”的路径，结局 COMPAT_UNVERIFIED，另有 `adapter.diagnostic{reason: "IMPLEMENTATION_CHANGED_BEFORE_INPUT", file, expected, seen}`；首个输入仍欠着。claude 的 stream-json 与 codex exec 在开闸的同一次写入里就带上提示词，没有这一步。
- turn 进行中：每个带期望值的 turn（工作、验证与监督）从开闸到 Agent 结束每 200 ms 比较一次，帧持续到达时也按间隔进行。不等或读不出 → `adapter.diagnostic{reason: "IMPLEMENTATION_CHANGED_IN_TURN", file, expected, seen}`，同一新身份戳只记一次；不结束进程，不改结局、失败码与类别（REQ-027 行为 4，AC-027-04）。诊断随 turn 结束写出，结算时成为该 Attempt 的事件。
- 闸门与这两次再核对之外的余项 R1、R2、R3 由所有者接受，见 docs/12“异常解释”与 docs/22。

**失败类别**（ADR-030 决定 6、9）：`Compat` 类只来自 HoH 自己的判断——prepare 的拒绝、钥匙相符的闸门拒绝、首个输入之前的再核对；Task BLOCKED(COMPAT_UNVERIFIED)，不退避，等新身份的套件与 Rebind 后恢复。闸门或首个输入之前的再核对只发现身份戳不同、而重新解析与哈希得出的身份不变（`stamp_only`：chmod、xattr、同内容替换、换走再换回）时按瞬时类：Task READY、退避与重试封顶（ADR-020），failure_reason 仍是 COMPAT_UNVERIFIED。另有 `Model`（Task BLOCKED(MODEL_UNAVAILABLE)）与 `Unavailable`（prepare 时该 Agent 不可用，AGENT_UNAVAILABLE）。

**已知帧登记**：每个 CLI 版本在 `tests/fixtures/protocol/<agent>/<version>/known-frames.json` 列出已知帧（含已知但忽略的辅助帧：Claude 的 `rate_limit_event`、`system/task_*`；Codex 的 `remoteControl/status/changed`、`deprecationNotice`、`thread/goal/cleared` 等），由录制样例与官方文档汇总（M1-T07）。只有不在登记表中的帧才产生 `adapter.unknown_event`，这样“未知帧”可判定。

**鉴权失败分类**（CR-003）：Codex App Server 为 `turn/completed` status=failed 且 `codexErrorInfo=unauthorized`（0.166.0 schema 的枚举值，录制中未出现，fixture 依 schema 合成并标 `synthetic=true`）；Codex exec 为 `turn.failed`，其消息或之前的 `error` 事件含 `unexpected status 401`。Pi 1.5.0 没有结构化错误码，`success:false` 的 error 文本以 `No API key found` 开头为 BLOCKED_AUTH，以 `Model not found` 开头为 MODEL_UNAVAILABLE，其余保存原文并按 PermanentExternal 处理。 Claude 2.1.318 为 assistant 帧带 `error=authentication_failed`（录制样例 `unauthenticated`：空 HOME 下运行，无模型调用；随后 result 为 is_error=true、退出码1），失败的轮次据此记为 BLOCKED_AUTH（M1-T03）。

**原生错误文字**（ADR-030 决定 6）：Auth、Model、Capability 类只来自适配器自己的识别规则（上一条，以及 pi 的两个固定开头）。claude 非 `success` 的 `result.subtype` 与 codex 未识别的 `codexErrorInfo` 原样作为失败码时，若恰与 HoH 的保留码（`$defs.ReasonCode`，如 `BLOCKED_AUTH`、`MODEL_UNAVAILABLE`、`COMPAT_UNVERIFIED`）同名，记为 `native:<码>`，按瞬时类结算，不阻塞绑定、不写排除记录；原帧照旧在原始日志里。新增把原生文字变成失败码的位置也须经这条规则。

**取消优先**（CR-003）：已请求取消的 attempt 一律以 CANCELLED 结束。claude 2.1.317 的样例显示 SIGINT 后仍会输出 `result(subtype=error_during_execution, is_error=true)` 且退出码 0，这个帧只作诊断保存。

## 交接和Fallback

fallback只能满足相同角色契约；没有独立QA时可以继续实现但不能通过最终验收。native进程CLI的最终回答只作为claim；产物/测试证据由HoH独立验证。没有自定义工具时，TaskResult结构化输出作为协作桥，不假设所有底座都支持同一种MCP/Skill注入。

**staffing**（ADR-030 决定 1–5）：host 每个 tick 在调度之前取一份注册表快照，staffing、pace、监督与分配共用它。每个绑定按其 revision 中该职位的 `[agent] ++ fallback`（没有记录的 revision 只有绑定的 Agent）逐个求判决：未查看、探测暂时失败、兼容待验证是等待，不评估 fallback；未安装、不受支持、缺认证、兼容 FAIL 或判不了、模型不可用、能力不支持是永久，评估下一个；第一个可用的被选中。不在列表里的 Agent 从不求值。判决行为 `<agent>:<code>`（如 `pi:not_installed`、`claude:BLOCKED_AUTH`、`X:CAPABILITY_UNSUPPORTED:resume`），原因码只用 `$defs.BlockedReason` 已有的（未查看、未安装、不受支持、探测失败都是 AGENT_UNAVAILABLE）；每个被求值候选的探测证据存为对象，由事件以 `probe_artifact_id` 引用，证据写不进时本 tick 不换绑也不阻塞。

- **粘滞**：当前 Agent 可用时不重选，换到 fallback 之后首选恢复也不换回；当前 Agent 永久不可用时从列表头重选。修改了该职位的新 revision 由 checkpoint 生效（ADR-022 决定 8）。
- **Fallback**：只在该职位没有在途 Attempt 时生效，一个事务里更新绑定（新 Agent；它是配置的首选时取配置的 model，否则为 NULL 即原生默认；role_epoch+1；没有会话；ACTIVE；`config_revision` 不变，所以不被读成配置修改）、写 `team.binding_applied{cause: "fallback"}` 与 `team.binding_fallback{from, to, evaluated, chosen, probe_artifact_id}`、交接包与 session.start（REQ-019 行为 2）；之前绑定不是同一原因的 BLOCKED 时先写失效事件 `team.binding_blocked`。有在途时绑定先 BLOCKED，并只写一次 `team.binding_change_pending{cause: "fallback", to}`，在途结束后的 tick 重新判定。新会话（qa、reviewer 也一样，与 developer 的会话不同）随后授予，首个输入是交接包。
- **Rebind**：Agent 可用而会话陈旧——会话的版本、能力或授予时的实现身份（`team.session_bound.identity`，完整的启动器与实现路径及 sha256）与当前事实不同——时，同一 Agent 与模型 epoch+1 换新会话（`cause: "rebind"`，没有失效与替换事件）；有在途时写 `change_pending{cause: "rebind"}`，该职位暂不派发。没有身份的旧会话算陈旧。
- **缺员**：没有可用候选时绑定 BLOCKED（当前 Agent 的原因，`missing` 为被求值的判决行），`team.binding_blocked` 唤醒 Commander；恢复时 `team.binding_unblocked`，撤销待换绑时 `team.binding_change_withdrawn`。该职位到期的 Task 以同一原因 BLOCKED，不建 Attempt；绑定恢复可用、会话新鲜（有注册表时另要求该 Agent 已验证）后由 pace 放回 READY（`task.unblocked`），WAITING_EXTERNAL 与 BLOCKED 的 Run 也一样（ADR-030 决定 7）。团队视图带 `missing` 与 `pending.cause`（config、fallback、rebind）。

**turn 发现的永久失败**（ADR-030 决定 2、6）：Auth、Capability、Model 类（监督 turn 也一样）在 Task 结算的同一事务里把该职位的绑定 BLOCKED（`team.binding_blocked{cause: "turn", attempt_id}`），下一个 tick 由 staffing 按 fallback 处理。Capability、Model 另写排除记录 `team.binding_excluded{run_id, role, config_revision, agent, agent_version, identity, reason, attempt_id}`：版本与身份取自该 Attempt 的会话（授予时写下、不可变），所以 runner 的结算与崩溃恢复写出同一条；它在同一 revision 内对同一版本与身份的该 Agent 适用，使它按永久不可用处理，新 revision 或 Agent 换了身份即失效，所以一个 revision 与身份之内至多切换 |order| 次，不会来回。没有身份的旧会话不写排除记录（`team.exclusion_skipped`），绑定先 Rebind。Auth 另进入注册表，带该 turn 运行时的身份与查看代次：身份已不是当前身份、或 turn 早于最近一次恢复事实（看得见的登录转变、新身份）的失败只记诊断；锁存的缺认证只在之后的恢复事实或 host 重启时清除。

**Commander**（ADR-030 决定 11）：staffing 对 commander 绑定相同，`[team.commander]` 的 fallback 生效，交接包即新 Commander 的首个输入；监督 turn 的 Auth、Capability、Model 失败与其他职位一样阻塞 commander 绑定（Capability、Model 另写排除记录），所以“探测正常而配置的模型实际不可用”的 Commander 在第一次失败后即换绑，不在原 Agent 上退避到封顶。Commander 的 Agent 待验证时分配延后，写一次 `supervisor.deferred{reason: "COMPAT_UNVERIFIED"}`，与 CHANGE_PENDING 一样不计入不活跃时限。COMMANDER_UNAVAILABLE 只在连 fallback 都不可用时持续，另外至多持续到在途监督 turn 结束。
