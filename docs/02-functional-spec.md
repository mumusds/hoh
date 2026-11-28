# 功能规格

> 生成视图：由 `tools/gen_spec_views.py` 从 `contracts/requirements.json`、`contracts/acceptance-scenarios.json` 与 `contracts/development-tasks.json` 生成，请勿手工编辑。需求变更走 CR（见 [规格治理](21-spec-governance.md)）。

成熟度：`detailed` 已细化为可直接实施和判定的验收；`outline` 只有概要验收，所在里程碑的 spec 任务必须先把它细化为 detailed（Readiness Gate，`tools/taskboard.py` 会拒绝领取依赖 outline 需求的实现任务）。

发布类：`required` 必须 PASS；`degradable` 可在有诊断证据和上游引用时记为 DEGRADED，发布状态随之标明 DEGRADED（CR-001）。

## REQ-001 · 默认交互入口

M1 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`CONTRACT.md`、`docs/03-ux.md`、`docs/12-user-guide.md`、`docs/04-architecture.md`、`docs/05-data-api.md`、`docs/11-deployment.md`

在 stdin/stdout 均为 TTY 的项目目录执行 `hoh` 进入 Workspace TUI，恢复已有工程状态与输入历史，用户提交意图前不创建任何 Run/Task/命令。

**必须行为**

- 必须仅在 stdin 与 stdout 均为 TTY 时进入 TUI（alternate screen + raw mode），顶栏显示项目名、mode（取配置 `default_mode`，缺省 AUTO）、当前 run 与 baseline。
- 必须连接本项目已有 host，不存在时启动独立 host 并等待 ready；TUI 只是客户端，任务不在 TUI 进程内执行。
- 必须从 DB 投影恢复已有 Run/Task/阻塞状态显示；打开 TUI 本身不改变任何 run 的 state/revision，不自动恢复 PAUSED/BLOCKED run。
- 用户在输入编辑器按 Enter 提交前，不得写入 commands/runs/tasks/input_queue 行，不得发送 StartRun/SendInput；输入草稿不算提交。
- 必须持久化已提交的输入历史（项目级），重启后可用 Ctrl+R 检索；草稿与历史分开。
- 缺配置时必须在 TUI 内展示与 doctor 同源的 Codex/Claude/Pi 检测引导，不要求用户先执行 init 类命令。
- 正常脱离、SIGTERM 或 panic 时必须退出 alternate screen、关闭 raw mode、恢复光标，终端属性与启动前一致；退出只脱离客户端，不取消已接受的 run。

**失败与边界**

- 当 stdin 或 stdout 不是 TTY 时，不进入 TUI、不读 stdin，退出码64，stderr 提示使用 `hoh -p`，stdout 为0字节，DB 无新增行。
- 当 host 无法启动或在就绪等待内未 ready（如项目锁被占用）时，退出码69，stderr 给出锁持有者/诊断，不创建 run。
- 当 TUI 收到 SIGTERM 时，恢复终端后以143退出；host 存活，run 的 state/revision 不变，原生 Agent 子进程不被杀。
- 当配置文件存在但无法解析时，不以默认值静默覆盖，退出码78并在 stderr 指出文件与位置；配置缺失则走引导而非报错。

**验收**

- **AC-001-01**（TEST-001-01 · 检查 `tui-pty`）Given fixture 项目 `.hoh/` DB 含 run A=PAUSED（revision=7）与 run B=COMPLETED；host 已以 FakeAgent 配置运行；PTY 120x40，已记录 `stty -g` 基线；When 在 PTY 中执行 `hoh`，出现 Workspace 后空闲5 s，输入草稿 `draft` 不按 Enter，再向 hoh 进程发送 SIGTERM；Then 退出码143；runs/tasks/commands/input_queue 行数不变，A 仍 PAUSED 且 revision=7；退出后 `stty -g` 与基线一致；host 仍存活
  - 判定：退出码=143；commands、runs、tasks、input_queue 行数与基线相等；run A state=PAUSED 且 revision=7，run B state=COMPLETED；SIGTERM 前屏幕快照含项目名、run A 短ID 与 `PAUSED`；PTY 输出中最后一个 `ESC[?1049h` 之后存在 `ESC[?1049l` 与 `ESC[?25h`；退出后 `stty -g` 与基线逐字节相同；host 进程 PID 仍存活
- **AC-001-02**（TEST-001-02 · 检查 `tui-pty`）Given 空 Git 项目（无 run），配置 default_mode=AUTO，host 以 FakeAgent 运行，PTY 100x30；When 执行 `hoh`，输入 `hello` 按 Enter，用 Ctrl+C→选择「脱离」退出；再次执行 `hoh`，按 Ctrl+R 输入 `hel`；Then 第一次 Enter 后 commands 恰新增1行 StartRun（payload.goal=`hello`、mode=AUTO、interactive=true）；脱离退出码0；第二次会话编辑器显示 `hello`，且提交前 commands 无新增
  - 判定：Enter 前 commands 行数=0，Enter 后=1 且 kind=StartRun；该 StartRun payload.goal 字节等于 `hello`，mode=AUTO，interactive=true；第一次脱离退出码=0，run 未进入 CANCEL_REQUESTED/CANCELLED；第二次会话 Ctrl+R 后编辑器文本=`hello`；第二次会话结束时 commands 行数仍=1
- **AC-001-03**（TEST-001-03 · 检查 `integration`）Given 与上一 AC 相同的 fixture 项目，测试进程无控制终端；When 分别执行 `hoh </dev/null >out 2>err` 与在 PTY 中执行 `hoh >out`（仅 stdout 被重定向）；Then 两者退出码均为64；out 为0字节；err 含 `-p` 提示；DB 行数不变；未启动新 host 进程
  - 判定：两次退出码均=64；out 文件大小=0；err 含字符串 `-p`；out 与 PTY 输出均不含 `ESC[?1049h`；DB 各表行数与基线相等，无新 host 进程

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 无 TTY 时 bare `hoh` 返回64而非静默退出或降级为 -p，属从严选择。
- 输入历史存储位置文档未定，选择项目 `.hoh/` 下持久化；是否跨项目共享未定。
- PTY 测试等待 Workspace 出现的10 s 是测试超时初值，不是启动性能指标；启动时延阈值文档未给出。

## REQ-002 · 非交互入口

M1 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`CONTRACT.md`、`docs/03-ux.md`、`docs/12-user-guide.md`、`docs/01-prd.md`、`docs/06-orchestration.md`、`docs/15-prompts-skills-memory.md`、`docs/05-data-api.md`、`contracts/protocol.schema.json`

`hoh -p <text>` 只用该文本创建恰好一个非交互 Run（或用 `--resume` 投递给指定 run），全程不读 stdin、不弹提问；需要真人决定时持久化 BLOCKED 并以3退出。

**必须行为**

- 必须要求 `-p` 带非空文本，原样作为唯一 StartRun 的 payload.goal，payload.interactive=false，mode 取配置默认（缺省 AUTO）。
- 必须不读取 stdin、不进入 alternate screen、不输出交互提示；有 TTY 时行为相同。
- 未带 `--resume` 时每次调用只新建1个 run，不续用或改动最近 run；带 `--resume <run_id>` 时以 SendInput 投递该 run，不新建 run。
- 可由已授权默认值确定的事项必须写入 assumption ledger（decision 记录标注 assumption 与依据），不假扮用户回答。
- 缺少只能由用户给出的关键产品决定时，run 必须持久转为 BLOCKED，blocked_reason 与待决问题写入事件，客户端按 REQ-004 退出码表以3退出。
- 无人值守仍须经过规格/readiness 流程，不得因非交互跳过规格直接实施产品代码。
- 前台模式等待 run 到达 COMPLETED/BLOCKED/INCOMPLETE/FAILED/CANCELLED/PAUSED 后退出；WAITING_EXTERNAL 期间继续等待，并在 stderr 显示 next_retry。

**失败与边界**

- 当 `-p` 缺参数或文本为空/仅空白时，退出码64，不连接 host、不新建 run。
- 当 `--resume` 指向不存在的 run_id 时，退出码64，stderr 说明原因，不新建 run。
- 当 `--resume` 指向 COMPLETED run 时拒绝重开，退出码64，原 run 的 state/revision 不变，stderr 提示新建 run。
- 当原生 Agent 缺认证时，run 进入 BLOCKED（blocked_reason=BLOCKED_AUTH），退出码3，绝不报告 COMPLETED。
- 当 stdin 是保持打开但从不写入的管道时，进程仍按 run 终态退出，不因读 stdin 阻塞；写入 stdin 的字节不进入任何 payload。

**验收**

- **AC-002-01**（TEST-002-01 · 检查 `integration`）Given mock 项目 + FakeAgent 脚本：Commander 在规格阶段产生1个 requires_user=true 的决策请求；stdin=/dev/null，无 TTY；When 执行 `hoh -p '实现登录功能' --output json`；Then 退出码3；stdout 唯一 JSON 对象 state=BLOCKED、blocked_reason=NEED_DECISION、含 run_id；DB 恰新增1个 run 且 state=BLOCKED；StartRun.payload.goal 与 argv 文本逐字节相同、interactive=false
  - 判定：退出码=3；stdout 整体 json.loads 成功，state=BLOCKED，blocked_reason=NEED_DECISION；runs 新增恰1行，state=BLOCKED；commands 新增恰1行 kind=StartRun，goal 字节=`实现登录功能`，interactive=false；events 含该决策问题原文，且无客户端产生的其它命令；stdout 与 stderr 均不含 `ESC[?1049h`
- **AC-002-02**（TEST-002-02 · 检查 `integration`）Given FakeAgent 脚本：所有决定可由已授权默认值得出；配置 max_iterations=1 使 run 以 INCOMPLETE 结束；When (a) 在 PTY 中执行 `hoh -p 'fix bug'` 并在启动后向 PTY 写入 `y` 回车；(b) stdin 为保持打开、从不写入的管道执行同一命令；Then 两次退出码均为4；PTY 输出不含 `ESC[?1049h`；`y` 未出现在任何 command/input_queue payload；各自只新增1个 run；decisions 中至少1条 assumption 记录
  - 判定：两次退出码均=4，run state=INCOMPLETE；PTY 输出不含 `ESC[?1049h`；所有 commands/input_queue payload 不含由 stdin 写入的 `y`；每次执行 runs 新增恰1行；每个 run 的 decisions 至少1行 state=APPLIED 的 assumption 记录，其 basis 非空（依据引用）；管道方式未关闭 stdin 仍在超时内退出
- **AC-002-03**（TEST-002-03 · 检查 `integration`）Given fixture 项目含 run C=COMPLETED（revision=5）与 run K=BLOCKED（NEED_DECISION）；FakeAgent 在收到答复后以 INCOMPLETE 结束；When 依次执行 `hoh -p`、`hoh -p ''`、`hoh -p '  '`、`hoh -p x --resume NOPE`、`hoh -p x --resume <C>`、`hoh -p '采用方案A' --resume <K>`；Then 前5项退出码64、stdout 0字节、runs 行数不变且 C 仍 COMPLETED/revision=5；第6项不新建 run，commands 新增1行 SendInput(run_id=K)，退出码=4
  - 判定：前5条退出码均=64，stdout 大小=0，stderr 非空；前5条后 runs 行数不变，run C state=COMPLETED 且 revision=5；第6条后 runs 行数不变，commands 新增恰1行 kind=SendInput 且 run_id=K；第6条退出码=4，run K 终态=INCOMPLETE

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- `-p` 是否接受 stdin 作为提示来源文档未定；选择从严：从不读取 stdin。
- 缺关键决策的 blocked_reason 命名 NEED_DECISION 为初值。
- `--resume` 指向不存在或 COMPLETED 的 run 退出码64为初值（COMPLETED 不得重开同 run）。
- 前台 -p 遇 PAUSED 即返回（退出码6，见 REQ-004），遇 WAITING_EXTERNAL 继续等待，为初值选择。
- （CR-003）`hoh -p <文本> --resume <run>` 提交 SendInput{input_kind=REPLY, target_role=null}：Runtime 若发现该 run 恰有一个待答问题，就把文本作为其答案（等价于 AnswerQuestion）并从 blocked_reasons 移除 NEED_DECISION，集合清空时由 Runtime 回到 resume_state；否则按 STEER 处理。前台客户端只以 seq 大于该命令接受事件的 run.result 决定退出码；stream-json 回放的历史事件带 replay=true。

## REQ-003 · 极简命令面

M1 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`CONTRACT.md`、`docs/03-ux.md`、`docs/12-user-guide.md`、`docs/adr/ADR-002.md`、`docs/04-architecture.md`

`hoh` 仅暴露 attach/web/doctor 三个子命令与内置 help、--help、--version；其它子命令、位置参数与越界选项一律以用法错误64退出且无副作用。

**必须行为**

- 顶层帮助必须仅列出子命令 attach、web、doctor（及内置 help），选项仅 -p、--output、--detach、--project、--resume、--help、--version。
- 隐藏的内部 daemon 入口不得出现在任何 --help 输出或 shell 补全中。
- `plan`、`mode`、`run`、`resume`、`init` 等未定义子命令与无 `-p` 的位置参数必须返回用法错误64，不得被当作提示词执行。
- `--resume` 只可与 `-p` 或 `attach` 组合，`--detach` 只可与 `-p` 组合；违规组合返回64。
- 用法错误必须在连接/启动 host 前判定：stdout 为0字节，不创建 `.hoh/`，不写 DB。
- `plan`/`mode` 的错误提示必须指向 TUI 内 `/plan` 或 Shift+Tab，不提供等价 Shell 命令。
- `hoh attach` 未给 run_id 且无 TTY、存在多个可附着 run 时必须拒绝并列出候选 id，不自动选择最近 run。

**失败与边界**

- 当参数解析库默认用法错误码为2时，必须统一改为64；任何用法错误不得以2退出。
- 当执行 `hoh plan` 等被禁止子命令时，退出码64，stderr 含 usage，stdout 0字节，无 host 进程被启动。
- 当 `hoh attach` 无 id、无 TTY 且可附着 run≥2 时，退出码64，stderr 列出全部候选 run_id，所有 run 状态不变。
- 当 `--project` 指向不存在的目录时，退出码64，不在该路径创建任何文件。

**验收**

- **AC-003-01**（TEST-003-01 · 检查 `integration`）Given 发布构建的 `hoh` 二进制，临时 HOME 与空项目目录，COLUMNS=80，NO_COLOR=1；When 执行 `hoh --help`、`hoh help`、`hoh attach --help`、`hoh web --help`、`hoh doctor --help`、`hoh --version`；Then 均退出码0且 stderr 为空；`hoh --help` stdout 与 golden 快照逐字节一致；解析出的子命令集合恰为 {attach,web,doctor,help}；`--version` 首行匹配 `^hoh \d+\.\d+\.\d+`
  - 判定：6条命令退出码均=0，stderr 大小=0；`hoh --help` 与 golden 逐字节相等；子命令集合={attach,web,doctor,help}；顶层选项集合={-p,--output,--detach,--project,--resume,--help,--version}；所有帮助输出不含 daemon、plan、mode、run 子命令条目；项目目录执行前后树 hash 相同
- **AC-003-02**（TEST-003-02 · 检查 `integration`）Given 同上环境，记录项目目录树 hash 与进程表基线；When 逐条执行 argv 表：`plan`、`mode plan`、`run`、`resume`、`init`、`'做个X'`、`--resume r1`、`--detach`、`--project /nonexistent -p x`；Then 每条退出码64（无一为2）、stdout 0字节、stderr 含 `usage`；`plan` 与 `mode plan` 的 stderr 含 `/plan`；目录树 hash 不变且无新 hoh host 进程；`/nonexistent` 未被创建
  - 判定：9条退出码均=64；9条 stdout 大小均=0，stderr 均含 `usage`；`plan`、`mode plan` 的 stderr 含 `/plan`；目录树 hash 与基线相同，不存在 `.hoh/`；无新增 hoh 进程；`/nonexistent` 不存在
- **AC-003-03**（TEST-003-03 · 检查 `integration`）Given fixture 项目含2个可附着 run（RUNNING、PAUSED），测试进程无 TTY；When 执行 `hoh attach </dev/null >out 2>err`；Then 退出码64；out 0字节；err 同时含两个 run_id；两 run 的 state/revision 不变；host 无新增客户端订阅
  - 判定：退出码=64；out 大小=0，err 含两个完整 run_id；两 run state 与 revision 不变；host 订阅计数与执行前相等

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- `init` 与无 `-p` 的位置参数作为用法错误64，文档未逐一列出，按最小命令面从严。
- `hoh attach` 无 id 时：恰1个可附着 run 允许直接附着，0个返回64，为初值选择。
- `--output json|stream-json` 与 bare `hoh`（TUI）同用是否报64未定，选择报64。

## REQ-004 · 标准输出契约

M1 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`CONTRACT.md`、`docs/03-ux.md`、`docs/22-tooling.md`、`docs/12-user-guide.md`、`docs/05-data-api.md`、`docs/14-resilience.md`、`docs/01-prd.md`、`contracts/protocol.schema.json`、`contracts/sql/001_initial.sql`

`--output text|json|stream-json` 下 stdout 只承载结果数据、stderr 只承载诊断，stream-json 为带 seq 的 LF 分帧事件流，且退出码按统一表映射 run 终态与错误类别。

**必须行为**

- json 模式 stdout 必须恰为1个 JSON 对象并以单个 LF 结尾，至少含 run_id、state、exit_code、blocked_reason、spec_revision、accepted、total、last_event_seq。
- stream-json 模式 stdout 每行1个符合 protocol.schema.json#/$defs/Event 的对象并以单个 0x0A 结束，seq 严格递增，最后一行为持久化终态事件 kind=`run.result`（含 state、exit_code）。
- stream-json 必须输出该 run 截至客户端据以退出的那条 run.result（含）的全部已持久化事件，与 DB 中该 run seq ≤ 该 run.result 的 last_event_seq 的 events 的 seq 集合一致；慢消费者不得造成缺口或重排（CR-022）。
- 所有模式下进度、日志、警告与原生 Agent stderr 只写 stderr；text 模式 stdout 只含最终可读结果；stdout 非 TTY 或设 NO_COLOR 时不含 ANSI 转义。
- 输出 JSON 时 U+2028/U+2029 必须转义为 `\u2028`/`\u2029`，记录内不出现原始 LF；读取方只按 0x0A 分帧。
- run 终态退出码：COMPLETED=0，FAILED=1，BLOCKED=3（含 BLOCKED_AUTH、BLOCKED_CAPABILITY、NEED_DECISION），INCOMPLETE=4，CANCELLED=5，PAUSED=6；只有 COMPLETED 为0。
- 非 run 退出码：用法错误64，host 不可用69，配置错误78，客户端因 SIGINT/SIGTERM 脱离130/143（run 不取消）；help/version、doctor 无阻断项、TUI 正常脱离为0。
- `--detach` 必须在 StartRun 事务 commit 后才输出 run_id（json 含 status=ACCEPTED 与 revision）并以0退出，0 不表示 run 完成。

**失败与边界**

- 当原生 Agent 向 stderr 输出≥10 MiB 时，stdout 无污染且客户端不因管道阻塞挂起。
- 当前台客户端收到 SIGINT/SIGTERM 时，退出码130/143，stderr 打印 run_id 与 `hoh attach <run_id>` 提示，run 不进入 CANCEL_REQUESTED，无 CancelRun 命令。
- 当 `--output` 取值不在 text/json/stream-json 时，退出码64，stdout 0字节。
- 当配置无效（78）或 host 不可用（69）时，json 模式 stdout 为0字节，不输出半截对象。
- 当 run 因缺认证进入 BLOCKED_AUTH 时退出码3，绝不为0。
- 当 stdout 消费方提前关闭（EPIPE）时客户端不 panic（stderr 无 `panicked at`），run 状态不变。

**验收**

- **AC-004-01**（TEST-004-01 · 检查 `unit`）Given 退出码映射纯函数；状态全集取自 001_initial.sql runs.state CHECK，另加客户端事件 usage/config/host_unavailable/SIGINT/SIGTERM；When 对每个状态与客户端事件调用映射函数；Then COMPLETED→0、FAILED→1、BLOCKED→3、INCOMPLETE→4、CANCELLED→5、PAUSED→6、usage→64、host_unavailable→69、config→78、SIGINT→130、SIGTERM→143；其余非终态返回「无退出码」；全集无 panic
  - 判定：11个有码输入与期望值逐项相等；CREATED、DISCOVERING、SPECIFYING、READY、RUNNING、VERIFYING、PAUSE_REQUESTED、WAITING_EXTERNAL、CANCEL_REQUESTED 返回无退出码；只有 COMPLETED 映射为0；所有输入均返回且无 panic
- **AC-004-02**（TEST-004-02 · 检查 `integration`）Given FakeAgent 脚本：事件含中文、emoji ZWJ 序列、U+2028、U+2029 与字符串内换行；同时向 stderr 写10 MiB 带标记噪声；run 以 INCOMPLETE 结束；When 执行 `hoh -p 'x' --output stream-json`，测试端每读一行休眠50 ms；Then stdout 按 0x0A 分割的每行均可解析且通过 Event schema；seq 严格递增且集合等于 DB 中该 run 的 events.seq；最后一行 kind=run.result、exit_code=4，进程退出码=4
  - 判定：解析失败行数=0，schema 失败行数=0；seq 严格递增，集合与 DB 完全相等；stdout 字节中 U+2028 以 `\u2028` 出现，原始 E2 80 A8 出现0次；解码后文本等于 FakeAgent 原文；stdout 不含 0x1B 且不含 stderr 噪声标记；最后一行 kind=run.result、exit_code=4，进程退出码=4
- **AC-004-03**（TEST-004-03 · 检查 `integration`）Given 同一 FakeAgent 脚本，stdout 重定向到文件（非 TTY）；When 分别执行 `--output json`、`--output text`、`NO_COLOR=1 hoh -p 'x' --output text` 与 `--detach --output json`；Then json：整个 stdout 一次解析为对象，含必需字段，exit_code=进程退出码=4；text：stdout 不含 0x1B、含 run_id 与 INCOMPLETE；detach：status=ACCEPTED、退出码0，且输出时 DB 已存在该 run 行
  - 判定：json stdout 恰1个 JSON 值且尾部仅1个 LF；json 对象含 run_id、state、exit_code、blocked_reason、spec_revision、accepted、total、last_event_seq；json exit_code 字段=进程退出码=4；两次 text stdout 0x1B 出现0次，含 run_id 与 `INCOMPLETE`；三种前台模式 stderr 含噪声标记而 stdout 不含；detach 退出码=0、status=ACCEPTED，且 runs 中已存在该 run_id
- **AC-004-04**（TEST-004-04 · 检查 `integration`）Given fake 原生 CLI 回放 Codex `codexErrorInfo=unauthorized` 失败样例；另一 FakeAgent 脚本使 run 持续 RUNNING；When 执行 `hoh -p 'x' --output json`；对第二个 run 的前台客户端 `hoh -p 'y' --output stream-json` 发送 SIGINT；Then 第一条退出码3，state=BLOCKED、blocked_reason=BLOCKED_AUTH；第二条退出码130，stderr 含 run_id 与 `hoh attach`，host 中该 run 仍为 RUNNING 且无 CancelRun
  - 判定：第一条退出码=3，blocked_reason=BLOCKED_AUTH，state≠COMPLETED；第二条退出码=130，stderr 含 run_id 与 `hoh attach`；SIGINT 后5 s 内该 run state=RUNNING，commands 中无 kind=CancelRun；stderr 不含 `panicked at`

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 0/3/64/78/130 与 docs/22-tooling.md 工具语义一致；1/4/5/6/69/143 及 BLOCKED 统一为3是本规格提出的初值；124/127 不用于产品（截止→INCOMPLETE=4，缺原生 CLI→BLOCKED=3）。
- stream-json 终态事件 kind=`run.result` 与 json 结果字段集为确认值；M1-T06 实现时把 run.result 与 json 结果对象写入 protocol.schema.json（随该任务的 CR 重锁）。
- U+2028/U+2029 强制转义为从严选择（JSON 标准允许原样输出）。
- 命名已统一（CR-001）：BLOCKED_CAPABILITY 为 Run/Task 阻塞原因，CAPABILITY_UNSUPPORTED 为错误码（API 422），见 docs/12、docs/14。

## REQ-005 · 脱离与附着

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`CONTRACT.md`、`docs/03-ux.md`、`docs/04-architecture.md`、`docs/11-deployment.md`、`docs/12-user-guide.md`、`docs/05-data-api.md`、`docs/14-resilience.md`、`contracts/openapi.json`

hoh -p --detach 在命令持久接受后输出run_id并退出，独立host继续执行；hoh attach <run_id> 重连同一Run并续接事件，不新建host、不重复派发。

**必须行为**

- 必须在StartRun事务COMMIT之后才向stdout输出run_id与提交确认并以退出码0退出；--output json时输出单个对象，含run_id、command_id、revision、status=ACCEPTED。
- 必须由独立session（setsid、无控制终端）的Engine host执行Run；Shell退出、SSH断开、终端SIGHUP均不影响已接受任务。
- hoh attach <run_id> 必须连接现有host并订阅该Run：先按cursor回放已提交事件再接live；attach本身不得创建Run、Task、Attempt或dispatch。
- attach时host不存在则启动host并等待ready；host必须先取得项目锁、host_epoch+1、完成reconcile后才接受写命令；同项目并发启动只允许一个host存活。
- 无TTY且存在≥2个可附着Run时，attach必须要求显式run_id，禁止自动选择最近Run；--resume <run_id> 只作为 -p 或 attach 的限定选项。
- 长期服务由systemd --user托管（Restart=on-failure、有界StartLimit，ExecStart为隐藏daemon入口）；logout后持续运行仅在用户明确启用linger时配置，不以nohup声称可重启恢复。
- 客户端detach只退订事件流：不改变run.state与run.revision，不向Agent发送任何信号或控制命令。

**失败与边界**

- 当StartRun事务未提交（host不可用、COMMIT失败）时，stdout不输出run_id，stderr给诊断code，退出码非0，runs表无该Run行。
- 当attach的run_id不存在时，退出码非0，错误code=RUN_NOT_FOUND，不创建Run、不启动Attempt。
- 当无TTY、未给run_id且有≥2个可附着Run时，退出码非0并在stderr列出候选run_id，不订阅任何Run。
- 当多个客户端同时触发host启动时，取锁失败者改为连接已存在host；不得出现两个host或host_epoch增加2。
- 当host在detach期间崩溃时，下次attach或systemd重启先reconcile；已SETTLED且effect已COMMITTED的Task不新增Attempt。

**验收**

- **AC-005-01**（TEST-005-01 · 检查 `tui-pty`）Given 临时Git项目，无host运行；FakeAgent使每个Attempt运行20 s并记录每次start调用；PTY内运行bash。；When 在PTY执行 hoh -p '完成fixture目标' --detach --output json，读出stdout后向PTY会话发SIGHUP并关闭PTY；60 s后在新PTY执行 hoh attach <run_id>。；Then detach在10 s（初值）内退出码0且stdout仅1个含run_id的JSON对象；HUP后host存活且tty为'?'；attach能看到HUP期间产生的事件；各Task的Attempt数与FakeAgent start次数不因attach增加。
  - 判定：detach退出码=0，耗时≤10 s，stdout恰1个JSON对象，含run_id/command_id/revision且status=ACCEPTED；HUP后host pid仍存在，ps显示tty='?'且sid≠PTY内shell的sid；HUP后60 s内events最大seq>S0；attach收到的事件seq严格递增且覆盖(S0, 当前watermark]；attach前后每个Task的Attempt行数及FakeAgent start次数完全相同
- **AC-005-02**（TEST-005-02 · 检查 `integration`）Given Run R处于PAUSED，无RUNNING Attempt；host已正常停止，projects.host_epoch=N。；When 50 ms内并发启动3个无TTY的 hoh attach R --output stream-json，保持10 s后全部断开。；Then 首个ready后全程恰1个host进程；host_epoch=N+1；3个客户端收到的事件seq序列相同；R仍为PAUSED，attempts与outbox新增0行。
  - 判定：首个ready之后所有采样host进程数=1；projects.host_epoch=N+1；3份stdout的seq序列逐项相同且严格递增；R.state=PAUSED，attempts与outbox行数与开始时相同
- **AC-005-03**（TEST-005-03 · 检查 `integration`）Given host运行，存在2个非终态Run R1、R2。；When stdin/stdout均为管道（无TTY）执行 hoh attach；再执行 hoh attach run-does-not-exist。；Then 两次退出码均非0；第一次stderr同时列出R1与R2的run_id且未订阅任何Run；第二次错误code=RUN_NOT_FOUND；runs表行数不变。
  - 判定：两次退出码均≠0；第一次stderr包含R1与R2两个run_id，stdout不含任何事件；第二次输出的错误code=RUN_NOT_FOUND；host订阅者数为0，runs行数=2
- **AC-005-04**（TEST-005-04 · 检查 `recovery`）Given systemd --user安装host unit（Restart=on-failure、有界StartLimit），测试用户已启用linger；Run R中T1的Attempt已SETTLED且effect=COMMITTED，T2的Attempt RUNNING；所有客户端已detach。；When 对host主进程执行kill -9，随后执行 hoh attach R。；Then systemd重启host（NRestarts+1），host_epoch+1；reconcile完成事件早于任何写命令被接受；T1不新增Attempt；T2仅在reconcile判定未执行后才重投；从kill到reconcile开始≤30 s。
  - 判定：NRestarts增加1，新host pid≠旧pid；projects.host_epoch增加1；reconcile完成事件seq小于重启后首个被接受写命令的seq；T1的Attempt行数不变；T2新增Attempt≤1，且其创建事件晚于对旧Attempt的「未执行」判定事件；reconcile开始时刻−t0≤30 s
- **AC-005-05**（TEST-005-05 · 检查 `integration`）Given 测试构建故障注入使StartRun事务COMMIT返回错误。；When 执行 hoh -p '目标' --detach --output json。；Then 退出码非0；stdout为空；stderr含诊断code；runs表无新行；不存在该命令启动的Agent进程。
  - 判定：退出码≠0；stdout字节数=0；stderr包含错误code；runs与attempts行数不变，FakeAgent进程数=0
- **AC-005-06**（TEST-005-06 · 检查 `deployment`）Given 全新临时 HOME（无 ~/.local/bin/hoh、无 hoh 用户 unit），候选的发布包，systemd --user 实例可用；一个已初始化 Git 的 fixture 项目；When 执行发布包内的 `install.sh`；随后 `hoh --version`、`systemctl --user is-enabled hoh-host@<项目 hash>.service`；在 fixture 项目执行 `hoh -p 'x' --detach --output json` 与 `hoh attach <run_id> --output json`；最后执行 `install.sh --uninstall`；Then 安装后 `command -v hoh`=$HOME/.local/bin/hoh，`hoh --version` 含候选 commit；unit 已启用且 ExecStart 指向已安装二进制；detach 返回 run_id 且 attach 退出码0；卸载后二进制与 unit 都已删除、fixture 项目 .hoh/ 原样保留；安装与卸载均不需要 root
  - 判定：hoh 位于 $HOME/.local/bin 且版本含候选 commit；unit 已启用且 ExecStart 指向已安装二进制；detach 输出 run_id，attach 退出码0；卸载后二进制与 unit 不存在，.hoh/ 文件树 hash 不变；全程 euid≠0 且未调用 sudo

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- detach返回时限文档未定，取初值10 s（本地冷启动host、FakeAgent）。
- detach的JSON对象即/v1/commands 的202响应体（含run_id，CR-003）；具体退出码数值由REQ-004统一，此处只断言0/非0。
- RUN_NOT_FOUND错误码名为初值。
- StartLimit与RestartSec未定，初值StartLimitBurst=5、StartLimitIntervalSec=300、RestartSec=5 s。
- （CR-003）POST /v1/commands 的 202 响应体为 {command_id, run_id, revision, status=ACCEPTED}；StartRun 的 run_id 由 host 在同一接受事务内分配。同 key 同 payload 的重放返回与首次逐字节相同的 202 响应体并带 `Idempotent-Replayed: true` 头。

## REQ-006 · 环境诊断

M1 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/12-user-guide.md`、`docs/11-deployment.md`、`docs/13-adapters.md`、`docs/22-tooling.md`、`docs/17-risks-gaps.md`、`contracts/openapi.json`

`hoh doctor` 逐项报告三个原生 Agent 及运行依赖的 installed、protocol、auth_check（含 unknown），默认不调用模型、不读取也不输出凭证，版本探测不算认证成功。

**必须行为**

- 必须为 codex、claude、pi 各输出一项：installed、path、version、binary_sha256、protocol(compatible/degraded/incompatible/unknown)、auth_check(verified/not_verified/missing/unknown)、capabilities、remedy。
- 还必须报告 git、node（≥22.19，Pi 需要）、三个 Pi 插件安装记录、项目 `.hoh/` 可写性与 host 状态；插件运行兼容性未经实测时为 unknown。
- protocol 只依据实际 --version/--help/原生 schema 与 compat manifest：缺必需 flag/方法为 incompatible，仅 exec 可用为 degraded，hash 未经兼容套件验证为 unknown。
- 默认不调用模型：auth_check 只来自原生非模型状态命令（如 `claude auth status`、`codex login status`），最多为 not_verified 或 missing；verified 只能由显式 `hoh doctor --live` 的最小原生调用成功得到。
- 不得读取凭证文件或凭证环境变量的值，不得输出任何 token/API key；原生状态命令输出只提取结论，不回显原文。
- 每个外部探测必须有超时（初值20 s）；超时时终止其进程组，该项记 unknown 并给出诊断。
- 支持 `--output text|json`；退出码：存在 installed=false、protocol=incompatible 或 auth_check=missing 的必需项为3，否则0；0 不表示已认证，unknown/not_verified 列入 summary.unverified。

**失败与边界**

- 当某 CLI 不在 PATH 时，installed=false，protocol 与 auth_check 为 unknown，remedy 给出安装动作，退出码3。
- 当版本缺少必需 flag/方法（如 Claude 缺 `--output-format`，Codex 既无 `app-server` 也无 `exec --json`）时，protocol=incompatible 并列出缺项，退出码3。
- 当原生状态命令报告未登录时，auth_check=missing，remedy 指向原生登录命令，doctor 不尝试登录或复制账户。
- 当原生状态命令输出中含密钥样字符串时，doctor 的 stdout、stderr 与写出的文件均不出现该字符串。
- 当状态命令挂起时，超时后杀其进程组，auth_check=unknown，doctor 在超时+5 s 内结束。
- 当已安装二进制 hash 与 compat manifest 不符时，protocol=unknown，remedy 为在无写任务时运行兼容套件。

**验收**

- **AC-006-01**（TEST-006-01 · 检查 `integration`）Given PATH 沙箱放置可编程 fake codex/claude/pi；兼容行的 fake 二进制 hash 已登记于 PASS 的 compat manifest；5行表：CLI 缺失、旧版缺必需 flag/方法、兼容但状态命令报未登录、兼容且报已登录、状态命令挂起60 s；When 每行执行 `hoh doctor --output json`（不带 `--live`）；Then 目标项依次为 installed=false/auth=unknown；protocol=incompatible；auth=missing；auth=not_verified；auth=unknown；退出码依次为3、3、3、0、0；所有行所有项 auth_check≠verified
  - 判定：5行目标项字段与期望表逐项相等；退出码序列=[3,3,3,0,0]；任何行 auth_check 均不为 verified；第2行 incompatible 项列出具体缺失 flag/方法名；第4、5行 summary.unverified 含目标 agent；第5行总耗时≤25 s，挂起的 fake 进程组已被 reap
- **AC-006-02**（TEST-006-02 · 检查 `integration`）Given OPENAI_API_KEY、ANTHROPIC_API_KEY 设为随机 canary `sk-CANARY-<32hex>`；隔离 HOME 下 `.codex/auth.json`、`.claude/.credentials.json` 写入同一 canary；fake 状态命令在输出中回显 canary；When 在 `strace -f -e trace=open,openat` 下分别执行 `hoh doctor --output json` 与 `--output text`；Then stdout、stderr 与 doctor 写出的所有文件中 canary 出现0次；strace 记录中无对两个凭证文件的打开；auth_check=not_verified
  - 判定：canary 在 stdout、stderr、新写文件中出现次数=0；strace 日志中凭证文件路径出现次数=0；相关 agent auth_check=not_verified；两次退出码=0
- **AC-006-03**（TEST-006-03 · 检查 `adapter-live`）Given 目标 VM 实装的 codex 与 claude（及 bootstrap 安装的 pi，若有）已原生登录；记录各自 `--version` 原文；When 执行 `hoh doctor --output json`，再执行显式 `hoh doctor --live --output json`；Then 第一次 version 与原生 `--version` 一致、无 verified、所有子进程 argv 属于探测白名单；第二次完成最小原生调用的 agent 为 verified 并附 attempt 记录，未登录者为 missing 且退出码3
  - 判定：默认模式各项 version 与原生输出相等；默认模式 auth_check∈{not_verified,missing,unknown}；默认模式子进程 argv 均属 {--version、--help、子命令 --help、schema 生成、原生状态命令}；live 模式 verified 项各有 mode=live 的 attempt 记录与原生会话 id；live 模式任一 agent 未登录时其项=missing 且退出码=3

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- verified 的唯一来源选为显式 `hoh doctor --live`（产生模型费用，是选项而非子命令），沿用 preflight --live 思路；文档未定义产品 doctor 的 live 方式。
- 退出码0只表示无确定阻断项，unknown/not_verified 不致非0；是否应更严格（如 unknown 也返回3）待确认。
- 探测超时20 s 沿用 tools/preflight.py 初值；protocol 新增 degraded 值表示仅 exec 降级可用。

## REQ-007 · Slash 与统一操作

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/03-ux.md`、`docs/12-user-guide.md`、`CONTRACT.md`、`docs/adr/ADR-031.md`

两种入口得到同一 SetMode 行为；Shell 不需要对称子命令

**必须行为**

- TUI 的 /plan、/build、/auto 与 Shift+Tab 都提交同一 SetMode 命令（CommandService 的规范化意图）；HTTP 的 SetMode 得到形状相同的事件；Shell 没有对称子命令（CONTRACT 第 3 条）。
- Shift+Tab 按配置 [ui] mode_cycle 的顺序循环（docs/03 的“配置可改”）：缺省为 ["PLAN", "BUILD", "AUTO"]，合法值是 2 或 3 个互不相同的模式名（PLAN、BUILD、AUTO）组成的数组，非法值使配置被拒（REQ-018 的同一路径）（C21）；附着 Run 时作用于该 Run，未附着时只改下一个由 TUI 启动的 Run 的模式（C20）。
- 模式变更在安全点生效：有运行中的写 Attempt 时先写 run.mode_change_pending{from, to}、不再派发新的写 Attempt，该 Attempt 到终态后写 run.mode_changed{from, to}；没有时立即写 run.mode_changed。
- / 打开模糊选择器，列出高频命令（/plan /build /auto /team /model /status /resume /help）；未知命令显示“未知命令”且不提交任何命令。
- /plan 等 HoH 命令永不转给底座 Agent；转给底座的斜杠须显式写目标（/agent <role> <text>，C19），没有目标时不转。

**失败与边界**

- 未知斜杠命令不产生命令、事件或 Agent 输入。
- 运行中切换时，待生效期间没有新的写派发。
- SetMode 的 expected_revision 过期时返回 409 STALE_REVISION，模式不变。

**验收**

- **AC-007-01**（TEST-007-01 · 检查 `tui-pty`）Given TUI（PTY）附着一个 AUTO 的 Run，另一个客户端经 HTTP 可提交命令；另一个项目的配置为 [ui] mode_cycle = ["PLAN", "AUTO"]；When 第一个项目的 TUI 中依次输入 /plan、按两次 Shift+Tab，随后经 HTTP 提交 SetMode{mode:PLAN}；第二个项目的 TUI 附着一个 PLAN 的 Run 后按两次 Shift+Tab；Then 第一个项目四次各产生恰好一条 SetMode 命令与一条 run.mode_changed{from, to}，依次为 AUTO→PLAN、PLAN→BUILD、BUILD→AUTO、AUTO→PLAN；/plan 与 HTTP 的两条命令除 id 外 payload 相同；顶栏显示当前模式；第二个项目依次为 PLAN→AUTO、AUTO→PLAN，没有经过 BUILD
  - 判定：四条 SetMode 与四条 run.mode_changed 依次为 AUTO→PLAN、PLAN→BUILD、BUILD→AUTO、AUTO→PLAN；/plan 与 HTTP 的 payload 相同；顶栏模式正确；第二个项目依次为 PLAN→AUTO、AUTO→PLAN
- **AC-007-02**（TEST-007-02 · 检查 `integration`）Given BUILD 的 Run 中一个写 Attempt 正在运行，另有 READY 的写 Task；When TUI 中输入 /plan，随后该 Attempt 结束；Then /plan 返回 202 并写 run.mode_change_pending{from:BUILD, to:PLAN}；Attempt 结束前没有新的写派发；结束后写 run.mode_changed 且 runs.mode=PLAN
  - 判定：202 且 run.mode_change_pending；期间无写派发；结束后 run.mode_changed 且 runs.mode=PLAN
- **AC-007-03**（TEST-007-03 · 检查 `tui-pty`）Given TUI 附着 Run，绑定的 Agent 为假 Agent（记录输入）；When 输入 /，再输入 /foo，再输入 /plan，最后输入 /agent developer /compact；Then / 列出八个高频命令；/foo 显示“未知命令”且没有命令提交；/plan 不出现在任何 Agent 输入中；/agent 的文本以 developer 为目标投递，未写目标的转发不发生
  - 判定：选择器列八个命令；/foo 无命令；/plan 不进 Agent 输入；/agent 以 developer 为目标

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- Shift+Tab 的顺序由 [ui] mode_cycle 配置，缺省 PLAN→BUILD→AUTO（CR-026，C21，CR26D-56）。
- 未附着时 /plan 不改项目默认（project.default_mode），只改下一个 Run（CR-026，C20）。
- 转给底座的语法为 /agent <role> <text>（CR-026，C19）。

## REQ-008 · Plan 互动与文档

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`CONTRACT.md`、`docs/03-ux.md`、`docs/06-orchestration.md`、`docs/15-prompts-skills-memory.md`、`docs/12-user-guide.md`、`docs/adr/ADR-031.md`、`docs/adr/ADR-025.md`、`contracts/sql/002_spec_drafting.sql`

产生版本化规格，允许提问，未批准实施不修改产品源码

**必须行为**

- 规格只经用户批准成为修订（O1）。批准有三个入口：ApproveSpec；以 AnswerQuestion 答复批准问题 spec_approval；SendInput{input_kind: REPLY, target_role: null} 被解析为该问题的答复（hoh -p approve --resume 与 TUI 的普通答复都走它，commands.rs 的 REPLY 路径）。host 在 HTTP 层为每个命令判定来源并随命令传入：Unix socket 的对端凭据，TCP 由本机连接表找到对端进程；对端进程或它的任一祖先带任何 Attempt 的标记（HOH_ATTEMPT_ID），或对端处于任何 Attempt 的 cgroup 中，来源为 attempt（reason 依次为 marker、ancestor_marker、attempt_cgroup）；对端无法确认（找不到对端进程、/proc 不可读、对端属于另一个 uid）为 unknown（reason=origin_unknown）；其余为 user。三个入口归一到同一个批准应用点，来源在那里判定：不是 user 即以 403 SPEC_APPROVAL_NOT_USER 拒绝（details.reason 为上述之一），什么也不写；同用户的服务或端口转发来的请求不被当作用户批准，Agent 工具调用里开的 shell 是 Attempt 的后代，同样被拒。Commander 没有批准规格的决策（DecisionPayload 不含此类），它的任何输出都不产生修订。剩余风险（脱掉标记、离开 cgroup 并脱离 Attempt 祖先链的同用户进程）写入 docs/12。ApproveSpec 的 decision：APPROVE_ONLY 批准、模式不变；APPROVE_AND_BUILD 批准并把该 Run 的模式置为 BUILD（同 SetMode，在安全点生效，REQ-007）；REJECT 不产生修订。
- 没有已批准规格时（所有者 2026-11-17 对 C22 的决定 (a)，见 CR-026 第 2 节）：StartRun 照旧以 runtime:no_spec 阻塞（BLOCKED(NEED_DECISION)，resume_state=CREATED），runs.spec_id 为空，没有 Task 与 Attempt，-p 以 3 退出（所有者 2026-11-16 的 Q1(a)）。问题的文字为“项目还没有已批准的规格。答复 draft：HoH 先只读地了解项目，再由 Plan 起草规格并请你批准，批准之前不改产品代码；项目有了已批准的规格之后，也可以答复 adopt 采用它。”，选项为 [adopt, draft]。项目此时已有已批准修订，则 draft 与 adopt 相同；否则答复释放 no_spec 声明，Run 回到 CREATED，由 Runtime 经只读的 DISCOVERING（REQ-028：run.baseline 先于任何写入）进入 SPECIFYING，spec_id 仍为空，并在进入 SPECIFYING 的同一事务建首个起草 Task。
- 起草 Task：由 Runtime 建，kind=spec_draft，职位 cto，writer，不引用 REQ（没有 task_requirements，不经 create_tasks 与 plan::check），以 spec.drafting{run_id, task_id} 标记。只有 kind=spec_draft 的 Task 可以没有规格；新建没有规格的 Task 只在项目还没有任何已批准修订时被接受（迁移 002 的 CHECK 与触发器）；首次批准之后，那个起草 Task 的 spec_id 保持为空（历史：它起草的是修订 1），之后建的起草 Task 的 spec_id 为当前已批准修订。起草 Task 与规格审查 Task 按 Runtime 规则类派发（不经调度轮；与监督 Task 同形：LEASED 的 Task、PREPARED 的 Attempt 与运行器通知），派发时 Run 须处于工作状态 SPECIFYING、READY、RUNNING 或 VERIFYING，派发不改变 Run 的生命周期状态：已启动的 Run 不回到 SPECIFYING（生命周期没有这条边）；PAUSE_REQUESTED、PAUSED、BLOCKED、WAITING_EXTERNAL 中不派发新的，恢复后照常。起草 Task 持项目的写者租约，所以同一项目的两个起草依次进行，也不与产品写 Attempt 同时运行；取消阶梯、终结证明、工作区记录、效果台账与 kill -9 之后的 reconcile 同其他写 Task（REQ-041、REQ-043、REQ-019 行为 6）。首个起草 Attempt 的准备阶段在非 git 目录中本地 git init（REQ-028），记为该 Attempt 的 effect。首次批准之前 Run 没有规格，Commander 不被唤醒（监督照旧以 NO_SPEC 延后），起草由 Runtime 规则、看门狗（ADR-022 决定 19）与墙钟期限约束；起草 Agent 需要用户回答时以 NEED_DECISION 的问题停下。Run 级问题（起草 Agent 的问题、spec_approval）的 resume_state 是提问时 Run 的状态。
- 起草 Task 的完成不冻结候选、不要求 AC：Runtime 先核对写集（行为 5），再要求起草结果已提交（未提交时与写 Task 的冻结拒绝一样请它更正），并从该提交的 Markdown 重新生成索引、与提交中的 index.json 逐字节相同（REQ-034）。通过后 Task 为 IMPLEMENTED，Runtime 以 Run 级问题 spec_approval{spec_sha256, git_commit} 请用户批准（Run 为 BLOCKED(NEED_DECISION)，resume_state 为提问时的状态）；修订之后的起草在提出 CR（REQ-033）时同样提这个问题。修订只经用户的 ApproveSpec 产生：问题的答复不论经哪个入口到达，都在行为 1 的批准应用点构造并提交该用户的 ApproveSpec——答复 approve 为 decision=APPROVE_ONLY，其他任何答复为 REJECT 且答复文本为 note，spec_sha256 取问题中的值——与直接提交的 ApproveSpec 走同一条路径：同一来源判定、同一摘要核对（工作树重新生成的索引不等于问题中的 spec_sha256 即 409 SPEC_CONFLICT，什么也不写）、同一写入；commands 中记为 ApproveSpec，幂等键由答复命令派生。项目还没有已批准修订时，批准在一个事务里写修订 1（spec.changed{cause:"approved"}）、使该 Run 采用它（runs.spec_id）、起草 Task 为 ACCEPTED、Run 回到 resume_state，此后 Commander 照常被唤醒；项目已有已批准修订时（例如另一个起草 Run 先得到批准），该 Run 先采用当前修订，起草结果以 CR 提出（REQ-033，base 为当前修订）。REJECT 使起草 Task 回到 READY，下一个 Attempt 以 note 为输入继续起草。无人值守的 -p 停在这个问题并以 3 退出，永不自批。
- Plan 的写集是 docs/spec/、docs/ 与 .hoh/：起草 Task 完成时 Runtime 以 Git 核对它开始以来的写集（Attempt 开始时的提交到完成时的提交之间的改动，加上已跟踪的修改与删除、未跟踪的新文件）；任一路径在写集之外时，写 plan.write_violation{task_id, paths}，不送批也不提出 CR，Task 为 BLOCKED（NEED_DECISION）并以问题列出这些路径；HoH 不替用户还原它们。PLAN 中 Commander 计划写产品源文件的 Task（writer 且不是起草 Task）被拒（ILLEGAL_TRANSITION，details 写“PLAN 不实施”）。
- 有已批准修订后，PLAN 与 AUTO 中的起草同样由起草 Task 承担（spec_id 为当前已批准修订），由 Commander 照常监督：在 SPECIFYING 中，或在已启动的 Run 中（例如 RUNNING 的 BUILD Run 经 /plan 在安全点改为 PLAN，Run 保持 RUNNING）；完成时以工作树中的规格提出 CR（change_requests PROPOSED，REQ-033），并以 spec_approval 问题请用户批准。批准之后新修订的独立规格审查同样在 Run 当前的工作状态中派发（REQ-031 行为 3）。
- Plan 中允许提问：Commander 的 request_user_decision 成为 question.asked，TUI 显示选项；待答期间 Run 为 BLOCKED（NEED_DECISION），resume_state 为提问时的状态（SPECIFYING 中即 SPECIFYING），答复后回到它；答复按 question_id 记录，不被记成假设。
- 未批准不实施：ProposeSpecChange 未批准时 Run 的 spec_id 不变；切到 BUILD 也无写派发（readiness 未过，REQ-031、REQ-009），既有阻塞不被清空。

**失败与边界**

- 来源不是 user（Attempt 的进程、它的后代——含 Agent 工具调用里的 shell 与在其中运行的 hoh -p approve——、Attempt cgroup 中的进程，或对端无法确认）的批准，不论经 ApproveSpec、AnswerQuestion 还是 SendInput REPLY 到达，都返回 403 SPEC_APPROVAL_NOT_USER；没有新修订、没有采用、没有产品写派发。
- 起草 Task 写了写集之外的路径（已跟踪的修改、删除或未跟踪的新文件）时，写 plan.write_violation，不送批也不提出 CR，Task 不前进。
- 没有已批准规格时 StartRun 阻塞在 runtime:no_spec，runs.spec_id 为空，没有 Task 与 Attempt；draft 之前不派发任何 Agent turn；用户批准之前 specs 没有行。
- host 在起草 Attempt 运行中崩溃时，重启后照 REQ-041 reconcile：不重复建起草 Task，已记录的 effect（git init、模板实例化）不重复执行，specs 仍没有行。
- 答复前 Run 为 BLOCKED(NEED_DECISION)，resume_state 为提问时的状态，不进入 READY。

**验收**

- **AC-008-01**（TEST-008-01 · 检查 `tui-pty`）Given BUILD 的 Run R 已采用修订 1、处于 RUNNING，一个产品写 Attempt 运行中；cto 的假 Agent 起草修订 2 的内容（只写 docs/spec/ 并提交），假 reviewer 对修订 2 给出 PASS；When 用户在 TUI 中输入 /plan，随即经 API 提交 PauseRun；写 Attempt 结束后提交 ResumeRun；起草完成、CR 提出后在 TUI 中批准；Then R 的状态事件中从不出现 SPECIFYING：/plan 在写 Attempt 结束的安全点生效（mode=PLAN），PauseRun 使 R 为 PAUSE_REQUESTED，写 Attempt 结束后为 PAUSED，PAUSED 期间没有起草派发；ResumeRun 之后 R 回到 RUNNING，起草 Task 在 RUNNING 中派发；批准问题期间 R 为 BLOCKED(NEED_DECISION) 且 resume_state=RUNNING；批准后有修订 2，spec_review.opened 在 R 为 RUNNING 时写入，审查 PASS 之前修订 2 下没有产品写派发；Git 记录的起草写集（含未跟踪与删除）只含 docs/spec/、docs/ 与 .hoh/ 下的路径，产品源文件的摘要不变
  - 判定：状态事件中没有 SPECIFYING；/plan 在安全点生效，PAUSE_REQUESTED→PAUSED 期间无起草派发；ResumeRun 后回到 RUNNING 并在 RUNNING 中派发起草；批准问题期间 BLOCKED 且 resume_state=RUNNING；修订 2 的 spec_review.opened 写于 RUNNING，审查 PASS 前无产品写派发；起草写集只含 docs/spec/、docs/、.hoh/，产品源文件摘要不变
- **AC-008-02**（TEST-008-02 · 检查 `recovery`）Given 一个空目录，没有已批准规格；cto 的假 Agent 起草时先写 docs/spec/ 并提交，再等待 30 s 才报告完成；When StartRun（PLAN）两次，得到 R1、R2；用户以 draft 答复 R1 的 no_spec 问题；起草 Attempt 运行中 kill -9 host 并重启；起草完成后用户以 ApproveSpec{decision:APPROVE_ONLY} 批准；随后以 draft 答复 R2 的 no_spec 问题；Then 阶段一：R1、R2 都为 BLOCKED(NEED_DECISION)，问题 runtime:no_spec 的文字与选项 [adopt, draft] 如 REQ-008 行为 2，spec_id 为空，没有 Task 与 Attempt。阶段二：draft 之后 R1 的状态事件依次为 CREATED、DISCOVERING、SPECIFYING，DISCOVERING 期间 R1 没有 Attempt 与 effect，SPECIFYING 中 spec_id 仍为空；恰有一个起草 Task（kind=spec_draft、spec_id 为空、职位 cto、writer、没有 task_requirements）与一条 spec.drafting；git init 恰一次，为该 Attempt 的 effect，没有远程；kill -9 与重启之后没有第二个起草 Task、git init 没有再执行，Attempt 按 REQ-041 reconcile；完成时没有候选，起草 Task 为 IMPLEMENTED，R1 为 BLOCKED(NEED_DECISION)、resume_state=SPECIFYING、问题 spec_approval 带 spec_sha256；此时 specs 没有行，R2 仍阻塞在 no_spec。阶段三：批准写 revision=1 的已批准修订（spec.changed{cause:"approved"}），R1 的 spec_id 为它，起草 Task 为 ACCEPTED 且 spec_id 仍为空，R1 回到 SPECIFYING 并有它的监督触发器；R2 的 draft 答复等同 adopt：R2 采用修订 1，没有建 spec_id 为空的 Task，tasks 中 spec_id 为空的行仍只有 R1 的起草 Task
  - 判定：阶段一：两个 Run 阻塞在 runtime:no_spec，文字与选项如行为 2，spec_id 为空，无 Task 与 Attempt；阶段二：R1 经 CREATED、DISCOVERING（期间无 Attempt 与 effect）到 SPECIFYING 且 spec_id 为空；恰一个 spec_id 为空的起草 Task 与一条 spec.drafting；git init 恰一次且为 effect；重启后无第二个起草 Task、无重复 effect；完成时无候选，问题 spec_approval，specs 无行，R2 仍阻塞；阶段三：修订 1 与 spec.changed{cause:approved}，R1 采用并回到 SPECIFYING，有监督触发器；起草 Task ACCEPTED 且 spec_id 仍为空；R2 的 draft 等同 adopt，spec_id 为空的行仍只有 R1 的起草 Task
- **AC-008-03**（TEST-008-03 · 检查 `tui-pty`）Given PLAN 的 Run 在 SPECIFYING，Commander 发出 request_user_decision（两个选项）；When TUI 显示后等待 10 s，随后用户选择第二个选项；Then TUI 显示问题与两个选项；等待期间 Run 为 BLOCKED(NEED_DECISION) 且 resume_state=SPECIFYING；答复后 Run 回到 SPECIFYING，AnswerQuestion 事件带 question_id 与 choice，假设台账中没有该条
  - 判定：TUI 显示问题与选项；等待期间 BLOCKED(NEED_DECISION) 且 resume_state=SPECIFYING；答复后回到 SPECIFYING 且记录 question_id 与 choice；台账无该条
- **AC-008-04**（TEST-008-04 · 检查 `integration`）Given PLAN 的 Run 采用了修订 1；Commander 的计划含一个 developer 的写 Task；cto 的起草假 Agent 修改一个已跟踪的产品源文件、新建一个未跟踪的产品源文件、删除一个产品源文件；When Runtime 处理该计划，起草 Task 完成；Then 计划被拒（ILLEGAL_TRANSITION，details 含“PLAN 不实施”），没有导入 Task；起草 Task 的完成写 plan.write_violation，paths 恰为这三个路径，不提出 CR，Task 为 BLOCKED(NEED_DECISION) 且问题列出这三个路径；没有新的已批准修订
  - 判定：计划被拒且 details 含“PLAN 不实施”；plan.write_violation 的 paths 恰为三个路径；不提出 CR，Task BLOCKED(NEED_DECISION) 且问题列出路径；没有新的已批准修订
- **AC-008-05**（TEST-008-05 · 检查 `integration`）Given PLAN 的 Run R 的首次起草已完成、spec_approval 待答（项目还没有已批准修订）；host 在受委托的 cgroup 中运行；起草 Attempt 的假 Agent 进程 P 带 Attempt 标记并读到 host.json 的 token；FakeCommander 在输出中要求批准；When P 经 TCP 与 Unix socket 各提交一次 ApproveSpec，以 AnswerQuestion 答复 approve，并在 P 中运行 hoh -p approve --resume <R>（SendInput REPLY）；P 启动的 shell Q 清除自身环境中的 HOH_ATTEMPT_ID 后提交 ApproveSpec（祖先 P 带标记）；一个自身与祖先都不带标记的进程 Z 被放入该 Attempt 的 cgroup 后提交 ApproveSpec；随后不属于任何 Attempt、不在任何 Attempt cgroup 中的用户 shell 运行 hoh -p approve --resume <R>；之后修订 2 的 CR 待批时，P 再以 ApproveSpec 与 hoh -p approve --resume <R> 各提交一次，用户 shell 随后同样以 hoh -p approve --resume <R> 批准；Then P 的六次请求、Q 与 Z 的请求都返回 403 SPEC_APPROVAL_NOT_USER，details.reason 对 P 为 marker、对 Q 为 ancestor_marker、对 Z 为 attempt_cgroup，P 的两次 hoh -p approve 以错误退出并输出该错误；这些请求之后没有新的已批准修订、没有采用、没有产品写派发；FakeCommander 的输出没有产生修订；用户 shell 的两次 hoh -p approve 分别产生修订 1 与修订 2，commands 中各记一条 ApproveSpec{decision:APPROVE_ONLY}，spec_sha256 等于各自问题中的值
  - 判定：P 的六次请求为 403 且 reason=marker，hoh -p 以错误退出；Q 为 403 且 reason=ancestor_marker；Z 为 403 且 reason=attempt_cgroup；这些请求后无新修订、无采用、无产品写派发；FakeCommander 输出不产生修订；用户两次 hoh -p approve 产生修订 1 与 2，各记一条 ApproveSpec{APPROVE_ONLY} 且摘要等于问题中的值

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 批准权取所有者 2026-11-17 的决定 O1：只有用户批准规格；来源判定（标记、祖先标记、cgroup，无法确认即拒）在 HTTP 层做出、随命令传入，在三个入口归一后的批准应用点执行，是 host 能做到的执行，剩余风险写入用户指南（CR-026，CR26D-06、-105、-152、-203）。
- 首次批准之前的起草取所有者 2026-11-17 对 C22 的决定 (a)：Plan 对话、由 Agent 起草，起草是 spec_id 为空的起草 Task，受锁 SQL 增加迁移 002（只有起草 Task 可以没有规格；项目有已批准修订之后不再新建这样的 Task，已有的保持为空）；草稿只经用户的 ApproveSpec 成为修订 1；Q1(a) 与 M2T13D-59 不变（CR-026，CR26D-01、-50、-100、-150、-151、-250）。
- Runtime 的起草与规格审查 Task 在 Run 的工作状态中派发，不改生命周期：已启动的 Run 不回到 SPECIFYING，Run 的状态迁移表与 AC-036-01（TEST-036-01 的 355 个被测对）不变（CR-026，CR26D-201）。
- Plan 的写集为 docs/spec/、docs/ 与 .hoh/；原型工件也只能放在 docs/ 下（CR-026，C23）。
- ApproveSpec 的 APPROVE_AND_BUILD 等于批准加 SetMode(BUILD)；spec_approval 问题的答复不论经 AnswerQuestion 还是 SendInput REPLY 到达，都构造为用户的 ApproveSpec（approve 为 APPROVE_ONLY，其他为 REJECT），与直接的 ApproveSpec 同一条路径（CR-026，CR26D-252）。
- 新码 SPEC_APPROVAL_NOT_USER（ErrorCode，403）（CR-026）。

## REQ-009 · Build 与 Auto

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/03-ux.md`、`docs/06-orchestration.md`、`CONTRACT.md`、`docs/adr/ADR-031.md`

Build 遵守既定规格；Auto 可补规划；缺必需规格均不越过 readiness

**必须行为**

- BUILD 遵守既定规格：Commander 的 create_tasks 只能引用当前已批准修订中的 REQ；BUILD 中不起草规格，需要改规格时提示用户切到 PLAN 或提 CR。
- AUTO 可补规划：规格缺必需项时 Runtime 派发起草 Task（REQ-008，写集同 PLAN），补充只经用户批准生效（O1）：交互时以问题请用户批准，无人值守时 Run 以 NEED_DECISION 停在批准前（-p 以 3 退出）；批准前无写产品源文件的派发，也没有新的已批准修订。
- BUILD 在缺必需规格时不自行补规格，相关 Task 为 BLOCKED（READINESS_NOT_MET）并在问题中提示可切到 AUTO 或 PLAN（C24）。
- 模式与生命周期正交：PAUSED、BLOCKED 中可改模式，恢复时按新模式；PLAN→BUILD 只在规格已批准且 readiness 已过时才有写派发；BUILD/AUTO→PLAN 在安全点生效（REQ-007）。

**失败与边界**

- BUILD 中引用未批准 REQ 的 create_tasks 被拒（ILLEGAL_TRANSITION，details 写“BUILD 不改规格，用 /plan 或 CR”），task_requirements 不变。
- AUTO 的补充未批准时，没有写产品源文件的派发，也没有新的已批准修订。
- 无人值守的 AUTO 缺规格时停在批准前（NEED_DECISION），不自批。

**验收**

- **AC-009-01**（TEST-009-01 · 检查 `integration`）Given BUILD 的 Run，已批准修订含 REQ-A；Commander 先提出 create_tasks，一个 Task 引用 REQ-A，另一个引用不存在于修订的 REQ-Z；再提出只含引用 REQ-A 的 Task 的 create_tasks；When Runtime 依次处理这两个决策；Then 第一个决策 REJECTED（ILLEGAL_TRANSITION，details 含“BUILD 不改规格，用 /plan 或 CR”与 REQ-Z），没有导入任何 Task；第二个决策 APPLIED，导入引用 REQ-A 的 Task 且 task_requirements 恰为 REQ-A
  - 判定：第一个 REJECTED 且 details 含提示与 REQ-Z，无 Task 导入；第二个 APPLIED 且 task_requirements 恰为 REQ-A
- **AC-009-02**（TEST-009-02 · 检查 `integration`）Given 三个 Run 在同一缺 AC 的已批准修订上：一个交互的 AUTO、一个无人值守（-p）的 AUTO、一个 BUILD；When 三者各运行至停止派发；Then 两个 AUTO 都派发起草 Task 并提出 CR，没有新的已批准修订、没有写产品源文件的派发；交互的以问题请用户批准；无人值守的 Run=BLOCKED(NEED_DECISION) 且 -p 以 3 退出；BUILD 不起草，相关 Task 为 BLOCKED(READINESS_NOT_MET) 且问题提示可切到 AUTO 或 PLAN
  - 判定：两个 AUTO 派发起草 Task 并提出 CR，无新修订、无产品写派发；交互的请用户批准；无人值守的 BLOCKED(NEED_DECISION) 且退出码 3；BUILD 的 Task BLOCKED(READINESS_NOT_MET) 且问题提示切换
- **AC-009-03**（TEST-009-03 · 检查 `unit`）Given 模式状态机的全部 (状态, 模式, 目标模式) 组合表；When 逐项调用模式迁移判定；Then 与表逐项一致：PAUSED 与 BLOCKED 中可改模式且恢复后按新模式；PLAN→BUILD 只在规格已批准且 readiness 已过时允许写派发；BUILD/AUTO→PLAN 有运行中写 Attempt 时为待生效
  - 判定：与表逐项一致

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- BUILD 缺规格时阻塞而不自动转 AUTO（CR-026，C24）。
- AUTO 的补充规格同样只由用户批准（O1）（CR-026）。

## REQ-010 · Workspace 工作台

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/adr/ADR-014.md`、`docs/adr/ADR-031.md`、`docs/cr/CR-028-m4-detailed-spec.md`

统一活动、团队摘要、输入框和 statusline；状态由事件投影产生

**必须行为**

- 顶部显示项目、已生效 mode、Run、基线身份；正文是统一的公开对话与工程活动，团队摘要可折叠；底部输入和 statusline。pending、输入四态与断线标记由 host 事实驱动，202 只说明接受。选择视图、展开和连接本身不调度任务。
- C2（CTO 2026-11-25）：每项决策的依据取已公开 basis/reason/note、影响取已提交 effects，分别保留来源；任一缺失显示 unknown 且不计 PASS。M4-T02 对缺依据或影响生产者的决策类别经 refinement CR 加法增加可选协议字段及真实生产者，fixture 不得捏造；AC-010-01 意义不变。

**失败与边界**

- API 断连保留带 stale 标记的最后快照与草稿；409 不重放用户命令为新动作；未知事件可见且不崩溃。

**验收**

- **AC-010-01**（TEST-010-01 · 检查 `tui-pty`）Given 120×32 PTY；三Agent承接commander/developer/qa；水位H；既有COMPLETED Run最近ACCEPTED提交B，当前R的run.baseline=B0≠B、BUILD；公开U/A、task/candidate活动、带rationale与impact的decision；多个RUNNING Task，含新重试及其已失败旧Attempt、同started_at的两个Task、缺失/不可解析started_at或无唯一RUNNING Attempt的Task；另有全unknown与无RUNNING子例；缺basis/reason/note的decision与缺已提交effects的decision各一（沿真实生产者保留来源）；缺失/不可解析started_at、无唯一RUNNING Attempt及全unknown异常行，以停机host的隔离数据库SQL fixture播种，记录SQL/行值/H，重开后经真实查询与PTY读取；同started_at子例也固定播种相等值，不靠host时钟碰巧相等；夹具只构造排序输入，不伪造M3模式/输入/批准/投递行为；When 打开Workspace，折叠团队，打开基线详情；Ctrl+P列动作，分别打开Task/Issue/Evidence抽屉；同快照TestBackend与PTY对照；同时间子例先断言两个当前RUNNING Attempt的started_at相等，再断言task_id决胜；逐子例读取同H的ui.task并核对展示，再从tasks路由查询全部Task；Then 同屏项目/R/B、BUILD、U/A/活动、输入/statusline；decision显示依据和影响；详情B0不替换顶栏B；默认一个主区、三职位摘要一致；面板列全部已实现动作且三抽屉列host对应ID/状态/证据引用、可返回；打开/切换的POST与Task/Attempt新增为0；ui.task在同一H的RUNNING Task中，排序时间仅取该Task于H唯一RUNNING Attempt的attempts.started_at（runner接收派发、进入STARTING时持久化的启动事实）；重试使用当前新Attempt，忽略已结束历史Attempt，不用Task创建时间/首次启动/客户端时钟替代。按解析后的UTC时刻降序、同时间按task_id升序；无唯一RUNNING Attempt或started_at缺失/不可解析时排序值为unknown，排在已知时间之后，unknown之间仍按task_id升序，不借旧Attempt补值。没有RUNNING Task时ui.task=null；其它Task仍由tasks路由可查；逐项断言新重试改变选择、历史Attempt不参与、同时间task_id决胜、缺失值后置/全unknown稳定排序和无RUNNING为null；缺依据或缺影响的decision对应项显示unknown且不计PASS，已有项仍保留真实来源，不捏造占位依据/影响
  - 判定：同屏项目/R/B、BUILD、U/A/活动、输入/statusline；decision显示依据和影响；详情B0不替换顶栏B；默认一个主区、三职位摘要一致；面板列全部已实现动作且三抽屉列host对应ID/状态/证据引用、可返回；打开/切换的POST与Task/Attempt新增为0；ui.task在同一H的RUNNING Task中，排序时间仅取该Task于H唯一RUNNING Attempt的attempts.started_at（runner接收派发、进入STARTING时持久化的启动事实）；重试使用当前新Attempt，忽略已结束历史Attempt，不用Task创建时间/首次启动/客户端时钟替代。按解析后的UTC时刻降序、同时间按task_id升序；无唯一RUNNING Attempt或started_at缺失/不可解析时排序值为unknown，排在已知时间之后，unknown之间仍按task_id升序，不借旧Attempt补值。没有RUNNING Task时ui.task=null；其它Task仍由tasks路由可查；逐项断言新重试改变选择、历史Attempt不参与、同时间task_id决胜、缺失值后置/全unknown稳定排序和无RUNNING为null；缺依据或缺影响的decision对应项显示unknown且不计PASS，已有项仍保留真实来源，不捏造占位依据/影响
- **AC-010-02**（TEST-010-02 · 检查 `tui-pty`）Given 写 Attempt 运行中，M3 的模式与输入投递可控；When `/plan`；普通输入发给 commander，先接受再 delivered/applied；另一个输入被拒；分别令SetMode与SendInput遇到一次expected_revision 409；Then pending 时仍显示 BUILD 和待 PLAN，安全点后才显示 PLAN；两个输入按 ID 显示 QUEUED→DELIVERED→APPLIED、QUEUED→REJECTED，保留拒绝原因；显示实际角色/epoch/boundary；SetMode的409不改mode、不自动重发新命令；SendInput保留client::fresh既有语义：409后重读revision并以新UUID重提，test_010_02分别断言两条路径，不把SetMode的禁重发规则扩到SendInput
  - 判定：pending 时仍显示 BUILD 和待 PLAN，安全点后才显示 PLAN；两个输入按 ID 显示 QUEUED→DELIVERED→APPLIED、QUEUED→REJECTED，保留拒绝原因；显示实际角色/epoch/boundary；SetMode的409不改mode、不自动重发新命令；SendInput保留client::fresh既有语义：409后重读revision并以新UUID重提，test_010_02分别断言两条路径，不把SetMode的禁重发规则扩到SendInput
- **AC-010-03**（TEST-010-03 · 检查 `tui-pty`）Given H=100、草稿“继续核对”，应用105后断线，host到110；可触发真实索引重建与writer busy锁；32KiB未提交草稿与已选目标，真实host对象预检低空间；DB_FULL子例保持对象预检有足够空间，再用db_faults.rs的io_fault_shim在state.db-wal的sync注入EDQUOT（F7_IO_ERRNO=122，F7_IO_STATE使事后room probe同样报告无空间），或等价ENOSPC seam，只作用于状态事务并标fault injection；不以真实低盘绕过STORAGE_LOW_SPACE预检；When 恢复并重复105；debug钩子触发同一重建路径，以旧page cursor查询；静态核对SSE 410 OpenAPI并以明确stub响应测试客户端解析；writer持锁超过busy_timeout，等待请求已进入该锁后尝试脱离，再解除；从PTY提交该草稿分别收到受理前503 STORAGE_LOW_SPACE和retryable=true的503 DB_FULL；前者立即观察拒绝，后者保持故障贯穿既有≤10s同键/同字节自动重试窗口，窗口结束后观察拒绝，再核对草稿/目标并脱离；另起客户端恢复空间后由用户重新输入草稿、选择目标并显式提交新动作（ADR-021新UUID）；Then stale/105期间保留草稿，补齐110后去stale、块不重复；page真实过期410且顶层snapshot_watermark=110，重取快照保锚点；SSE矛盾真实409 CONFLICT，SSE 410仅静态/stub兼容覆盖，v1产品不可达；busy时控制503 DB_BUSY且未接受、旧快照可绘制；探针证明脱离在writer释放/超时前完成，不等满busy_timeout且无Cancel（绝对1s在§5.1参考测量另核）；重连/重取快照不自动发起新用户命令、不乐观加进度；两种受理前503均逐字保留原草稿与目标，不乐观清空或新增用户块/接受事实；STORAGE_LOW_SPACE不自动重试，立即显示“未接受”及available/required/min_free，仍可脱离；DB_FULL先运行ADR-021既有≤10s同键/同字节重试，持续拒绝至窗口结束才显示“未接受”，此后仍可脱离，不能把零自动重试断言用于DB_FULL；脱离前验证草稿/目标保留，脱离后不承诺持久草稿或旧幂等键；恢复后的新用户动作使用新UUID，最多一次接受/用户块，拒绝的旧动作保持零接受事实
  - 判定：stale/105期间保留草稿，补齐110后去stale、块不重复；page真实过期410且顶层snapshot_watermark=110，重取快照保锚点；SSE矛盾真实409 CONFLICT，SSE 410仅静态/stub兼容覆盖，v1产品不可达；busy时控制503 DB_BUSY且未接受、旧快照可绘制；探针证明脱离在writer释放/超时前完成，不等满busy_timeout且无Cancel（绝对1s在§5.1参考测量另核）；重连/重取快照不自动发起新用户命令、不乐观加进度；两种受理前503均逐字保留原草稿与目标，不乐观清空或新增用户块/接受事实；STORAGE_LOW_SPACE不自动重试，立即显示“未接受”及available/required/min_free，仍可脱离；DB_FULL先运行ADR-021既有≤10s同键/同字节重试，持续拒绝至窗口结束才显示“未接受”，此后仍可脱离，不能把零自动重试断言用于DB_FULL；脱离前验证草稿/目标保留，脱离后不承诺持久草稿或旧幂等键；恢复后的新用户动作使用新UUID，最多一次接受/用户块，拒绝的旧动作保持零接受事实
- **AC-010-04**（TEST-010-04 · 检查 `unit`）Given 已提交1..20，含未知kind、公开控制字符、pending/input；两种分批与重复5/10；When 比较一次重放/分批/快照增量的规范化投影；Then 20个不同seq且逐字段相同；未知kind有UI_UNKNOWN_EVENT/seq；ESC/OSC转义可见、不执行；pending不当生效；事务未提交不发SSE的断言移至TEST-064-05真实host场景
  - 判定：20个不同seq且逐字段相同；未知kind有UI_UNKNOWN_EVENT/seq；ESC/OSC转义可见、不执行；pending不当生效；事务未提交不发SSE的断言移至TEST-064-05真实host场景

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- （CR-028，CTO 2026-11-21 批准 D1–D12）D1/D4/D11；§1/2/2.2 的同水位读模型、输入四态与公开事实；§5.1 的性能与证据边界。AC 中的章节号、F13/H13 与参考环境均指 docs/cr/CR-028-m4-detailed-spec.md；2026-11-25 应用核对仅更新来源/已实现前提及已授权的事件分类和 GC 根，不修改 AC 语义或阈值。
- C3（CTO 2026-11-25）：输入历史加法扩展既有 H/limit/cursor 读模型；InputStatePageRef={run_id,watermark,limit,cursor}，watermark=H、limit=1–500/缺省100、首 cursor=null，续查 GET /v1/runs/{run_id}/inputs 的 Page.next_cursor。ui.task在同一H的RUNNING Task中，排序时间仅取该Task于H唯一RUNNING Attempt的attempts.started_at（runner接收派发、进入STARTING时持久化的启动事实）；重试使用当前新Attempt，忽略已结束历史Attempt，不用Task创建时间/首次启动/客户端时钟替代。按解析后的UTC时刻降序、同时间按task_id升序；无唯一RUNNING Attempt或started_at缺失/不可解析时排序值为unknown，排在已知时间之后，unknown之间仍按task_id升序，不借旧Attempt补值。没有RUNNING Task时ui.task=null；其它Task仍由tasks路由可查。

## REQ-011 · Team/Focus 布局

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/03-ux.md`、`docs/04-architecture.md`、`docs/adr/ADR-022.md`、`docs/adr/ADR-024.md`、`docs/cr/CR-025-budget-risk-role-binding-pending-gate-date-time.md`、`docs/cr/CR-028-m4-detailed-spec.md`

窄屏单列/标签，宽屏可分屏；完整会话按需进入，不强制三滚动窗

**必须行为**

- Team 按配置职位展示绑定、epoch、可用性、当前任务与 pending；Focus 按完整身份查看公开会话，历史按需分页。布局转换保留草稿、展开状态和 grapheme 锚点。

**失败与边界**

- 角色删除或历史内容缺失显示缺失身份与返回入口；同 Agent 多职位不自动选择；窗口过小不 panic，也不修改执行状态。

**验收**

- **AC-011-01**（TEST-011-01 · 检查 `tui-pty`）Given 默认Workspace、无预选角色；六职位，cto/developer同为claude，各有不同会话；后选developer，块K的offset=12，草稿“尚未发送”；When 先未预选打开Team，再从claude进入Focus；在选择器等待，显式选developer；依次resize80/99/100/120/159/160/180列、32行，选择Team分屏再返回Workspace；Then 选择器同时列cto与developer及真实epoch/session，选定前Focus内容为空、无默认落入任一职位；选后只显示developer；整个导航无POST/派发/Task或Attempt新增。80/99为单区标签；100–159最多主区+1详情；160/180用户选择后最多三列；默认Workspace不强制三窗；选择身份、K/12与草稿保持，所有行显示宽度≤窗口
  - 判定：选择器同时列cto与developer及真实epoch/session，选定前Focus内容为空、无默认落入任一职位；选后只显示developer；整个导航无POST/派发/Task或Attempt新增。80/99为单区标签；100–159最多主区+1详情；160/180用户选择后最多三列；默认Workspace不强制三窗；选择身份、K/12与草稿保持，所有行显示宽度≤窗口
- **AC-011-02**（TEST-011-02 · 检查 `tui-pty`）Given 七职位含security_reviewer；developer epoch1 A1在途，READY developer Task T2待派，codex换绑待checkpoint；When Team/Focus A1；对T2派发一次得到拒绝，再结束A1、生效epoch2并派A2；Then 七职位可达；ROLE_BINDING_PENDING确由拒绝派发产生；pending保留claude/A1与待codex；历史Focus仍A1/epoch1，选择当前才A2/epoch2；内容不混；重用session时按Attempt边界显示，session过滤跨Attempt时逐块仍带attempt_id
  - 判定：七职位可达；ROLE_BINDING_PENDING确由拒绝派发产生；pending保留claude/A1与待codex；历史Focus仍A1/epoch1，选择当前才A2/epoch2；内容不混；重用session时按Attempt边界显示，session过滤跨Attempt时逐块仍带attempt_id
- **AC-011-03**（TEST-011-03 · 检查 `integration`）Given 同agent两role、三个session；A1有公开message/tool/error及非公开内部字段；H13真实handoff无attempt_id；一段公开日志缺文件；When 按A1遍历Focus；分别切到H13的role/epoch视图、session视图，再加attempt_id=A1过滤；查询另一role与缺失artifact；Then A1的公开message/tool/error按offset/seq完整且不混role/session；role/epoch/session视图的handoff集合逐项等于H13，任何Attempt过滤的handoff子集为空，不从session推断Attempt；内部字段不在响应；缺段ARTIFACT_NOT_FOUND，descriptor保留身份与缺失提示，不为空成功；无spawn/任务写入
  - 判定：A1的公开message/tool/error按offset/seq完整且不混role/session；role/epoch/session视图的handoff集合逐项等于H13，任何Attempt过滤的handoff子集为空，不从session推断Attempt；内部字段不在响应；缺段ARTIFACT_NOT_FOUND，descriptor保留身份与缺失提示，不为空成功；无spawn/任务写入
- **AC-011-04**（TEST-011-04 · 检查 `tui-pty`）Given 80×24、角色选择与草稿状态；可访问状态页；When resize40×8、1×1再120×32；每个小尺寸输入字素“a”、打开状态页、关闭，再选择历史已移除角色；Then 40×8显示“窗口过小”及“输入 / Ctrl+C退出”；1×1仅显示ASCII !，Ctrl+P面板提供可逐字符滚动的ASCII诊断 small terminal/input/Ctrl+C exit 与状态字段英文键和值（不用一格截断宽字）；各尺寸接受a且不提交，Ctrl+C选择/脱离仍可达；被省略的四组事实在可滚动状态页逐字段可读；恢复后草稿新增两个a、锚点保持，历史角色有标记；无panic/越界/Run修订变化
  - 判定：40×8显示“窗口过小”及“输入 / Ctrl+C退出”；1×1仅显示ASCII !，Ctrl+P面板提供可逐字符滚动的ASCII诊断 small terminal/input/Ctrl+C exit 与状态字段英文键和值（不用一格截断宽字）；各尺寸接受a且不提交，Ctrl+C选择/脱离仍可达；被省略的四组事实在可滚动状态页逐字段可读；恢复后草稿新增两个a、锚点保持，历史角色有标记；无panic/越界/Run修订变化
- **AC-011-05**（TEST-011-05 · 检查 `integration`）Given 每个codex App Server/受支持exec、claude、pi模式的版本绑定兼容fixture；含public message/tool开始结束、compact/wait/usage有与无、secret标记、两role/epoch；真实runner与Persistence，非直接注入UI事件；两个不同input_id/block_id的相同正文，以及同block_id/stream两个不同first_offset的相同段；When 经SendInput接受事务与native frame入口逐帧/重复/超大正文输入，队列背压；在提交前后kill/reopen、取消、EOF；从持久公开块读出；登记上述两类重复内容并在reopen后重放同input_id/native身份/offset；Then 三家按§2.2白名单输出逐字段/顺序/身份相同；另经SendInput受理普通/大正文后从Persistence读用户块，正文/hash等于规范化原文（非文件引用提示），重放input_id与可关联的native回显不重复；DB_BUSY背压超过quiet不产生attempt.inactive，解除后真正静默到余下quiet才产生，取消仍可达；启动和结束工具均可查、未知能力显式null，secret/隐藏字段不入供API消费的公开读模型或公开对象；未结束Attempt即可读到已提交消息/活动；队列≤256/1MiB、段≤1MiB；重复无重块、旧epoch不更新当前活动；已提交块重启不丢，未提交尾段有缺口而非完整成功；fixture标product+synthetic/recorded来源，不称live；public_segments以(block_id,stream,first_offset)标识每次出现，artifact_id为可复用外键：不同消息相同正文各有独立块/段，同块不同offset的相同段均保留且可共享同artifact_id；重放原input_id、(attempt_id,native_stream,native_item_id,part)及段offset不新增块/段；001→002→003迁移后逐项核对外键、两类重复内容与重启重放幂等
  - 判定：三家按§2.2白名单输出逐字段/顺序/身份相同；另经SendInput受理普通/大正文后从Persistence读用户块，正文/hash等于规范化原文（非文件引用提示），重放input_id与可关联的native回显不重复；DB_BUSY背压超过quiet不产生attempt.inactive，解除后真正静默到余下quiet才产生，取消仍可达；启动和结束工具均可查、未知能力显式null，secret/隐藏字段不入供API消费的公开读模型或公开对象；未结束Attempt即可读到已提交消息/活动；队列≤256/1MiB、段≤1MiB；重复无重块、旧epoch不更新当前活动；已提交块重启不丢，未提交尾段有缺口而非完整成功；fixture标product+synthetic/recorded来源，不称live；public_segments以(block_id,stream,first_offset)标识每次出现，artifact_id为可复用外键：不同消息相同正文各有独立块/段，同块不同offset的相同段均保留且可共享同artifact_id；重放原input_id、(attempt_id,native_stream,native_item_id,part)及段offset不新增块/段；001→002→003迁移后逐项核对外键、两类重复内容与重启重放幂等

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- （CR-028，CTO 2026-11-21 批准 D1–D12）D1/D9/D10/D11；§1/2/2.1/2.2/4 的身份、布局与公开流；REQ-013 的 H13 交接成员规则。AC 中的章节号、F13/H13 与参考环境均指 docs/cr/CR-028-m4-detailed-spec.md；2026-11-25 应用核对仅更新来源/已实现前提及已授权的事件分类和 GC 根，不修改 AC 语义或阈值。
- C3（CTO 2026-11-25）：AC-011-04 保持原判据及 M4-T02 属主；T02 交付小屏显示和其余自身 AC，未有 progress 时显示 unknown 不算该项 PASS。M4-T06 写者在共享progress交付后补齐并执行test_011_04的四组事实、小尺寸交互与恢复断言；M4-T12 验证任务依赖 M4-T02 与提供共享 progress 聚合的 M4-T06，届时完整验证四组事实与 AC-011-04；M4-T04 依赖该验证任务。AC 集合与 check owner 不变。 T06交付前，四组事实的test_011_04保留#[ignore]及原因，T02/T05只记SKIPPED；T06补齐后才去ignore并执行，禁止弱断言PASS。

## REQ-012 · 可展开内容块

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/10-acceptance.md`、`docs/cr/CR-028-m4-detailed-spec.md`

展开、复制、搜索、全屏、返回保留滚动锚点；键盘与鼠标可达

**必须行为**

- message/task/tool/diff/test/evidence/decision/handoff 块都有稳定身份；八类均必须支持展开、复制、搜索、全屏与返回。键盘与鼠标调用同一动作。展开只读，不执行工具或测试；结果引用候选、TEST 与原始证据。

**失败与边界**

- 缺失/损坏/非文本/权限错误在原块展示原因，保留返回和重试；旧候选标历史/失效，不自动切到新候选同名文件。

**验收**

- **AC-012-01**（TEST-012-01 · 检查 `integration`）Given §5.1参考环境/release串行阶段；八类块各一，首尾标记X，Tool20MiB；Evidence C1；父锚点K/200；When 每类先SGR单击选择、双击展开、滚轮滚动，再键盘展开→分页→全文搜索X（首尾两处不重叠，记录offset x1<x2）→n→N→全屏→Esc返回→复制文件，追加另一块；真实PTY采≥100次首屏；Then 单击只改选中项且保持折叠，双击恰一次展开，滚轮改变视窗锚点但不改所选块身份/不执行动作；八类键盘动作均可达且不执行工具/测试；搜索先定位x1，n准确到x2、N回x1，所选block_id/草稿不变，仅只读查询且零新增命令；返回K/200、首尾可达、导出字节/hash等于公开纯文本；首屏p95≤500ms且缓存有界（同AC-064-02：描述≤1000、正文≤4MiB、在途页≤2）；键鼠指向相同身份；环境不符SKIPPED诊断/退出78、AC不通过，禁止静默PASS
  - 判定：单击只改选中项且保持折叠，双击恰一次展开，滚轮改变视窗锚点但不改所选块身份/不执行动作；八类键盘动作均可达且不执行工具/测试；搜索先定位x1，n准确到x2、N回x1，所选block_id/草稿不变，仅只读查询且零新增命令；返回K/200、首尾可达、导出字节/hash等于公开纯文本；首屏p95≤500ms且缓存有界（同AC-064-02：描述≤1000、正文≤4MiB、在途页≤2）；键鼠指向相同身份；环境不符SKIPPED诊断/退出78、AC不通过，禁止静默PASS
- **AC-012-02**（TEST-012-02 · 检查 `integration`）Given 20MiB+17字节完整UTF-8对象；“中文X”跨256KiB与8MiB边界各一处、末尾一处，连续重叠匹配aaa；NUL/无效UTF-8二进制B含唯一ESC/OSC标记；When 分Range拼全文；search limit=1续扫到结束、重复cursor；无匹配；请求B搜索并用PTY打开B；Then 拼接hash正确无缺重字；每个匹配的byte_offset精确、跨窗口不漏不重、上下文合法UTF-8≤256B；无匹配完成为空；B为422 ARTIFACT_NOT_TEXT；PTY只含metadata/下载动作，捕获中B原始控制标记字节为0
  - 判定：拼接hash正确无缺重字；每个匹配的byte_offset精确、跨窗口不漏不重、上下文合法UTF-8≤256B；无匹配完成为空；B为422 ARTIFACT_NOT_TEXT；PTY只含metadata/下载动作，捕获中B原始控制标记字节为0
- **AC-012-03**（TEST-012-03 · 检查 `tui-pty`）Given Evidence缺文件、摘要不符；独立认证403变体在隔离host重启后由旧token客户端请求（旧SSE断开、新请求均被拒）；对象chmod造成EACCES、DB_BUSY；旧C1与新C2同文件名；65KiB公开文本；When 打开/重试/返回；文件复制成功与不可写目录失败；尝试设置osc52；移除exports忽略规则再复制；Then 分别显示404 ARTIFACT_NOT_FOUND、409 ARTIFACT_INVALID、原403、503 ARTIFACT_UNREADABLE(permission)、503 DB_BUSY；403使整个连接标stale/认证失效、保草稿/最后快照、停止重试旧token；显式重新连接读取新凭证并取快照后才恢复，不冒充单对象权限；无PASS/空成功；C1不换C2；65KiB文件复制完整，失败无成功消息；osc52配置拒绝、无OSC/DCS输出；忽略失效拒绝导出且不产生未跟踪文件；草稿锚点不变
  - 判定：分别显示404 ARTIFACT_NOT_FOUND、409 ARTIFACT_INVALID、原403、503 ARTIFACT_UNREADABLE(permission)、503 DB_BUSY；403使整个连接标stale/认证失效、保草稿/最后快照、停止重试旧token；显式重新连接读取新凭证并取快照后才恢复，不冒充单对象权限；无PASS/空成功；C1不换C2；65KiB文件复制完整，失败无成功消息；osc52配置拒绝、无OSC/DCS输出；忽略失效拒绝导出且不产生未跟踪文件；草稿锚点不变
- **AC-012-04**（TEST-012-04 · 检查 `unit`）Given 八类块、100 次追加、10 次 resize，焦点为第三块；正文含中文/组合字符；选中块被过滤或其对象回收；When 对展开/折叠/全屏/返回/搜索状态作纯动作序列；重复同一提交事件；Then block_id 与 grapheme_offset 不因前插/重复事件漂移；offset 超过新正文时钳到最后字素边界并提示；被过滤保留隐藏锚点，取消过滤可返回；不可恢复块明确提示并选 seq 最近的前驱（无前驱选后继），不跳到无关 Run
  - 判定：block_id 与 grapheme_offset 不因前插/重复事件漂移；offset 超过新正文时钳到最后字素边界并提示；被过滤保留隐藏锚点，取消过滤可返回；不可恢复块明确提示并选 seq 最近的前驱（无前驱选后继），不跳到无关 Run

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- （CR-028，CTO 2026-11-21 批准 D1–D12）D1/D4/D7/D9；§1/2/4/5.1 的缓存、Range/搜索、文件复制与同一动作；八类块不缩减。AC 中的章节号、F13/H13 与参考环境均指 docs/cr/CR-028-m4-detailed-spec.md；2026-11-25 应用核对仅更新来源/已实现前提及已授权的事件分类和 GC 根，不修改 AC 语义或阈值。

## REQ-013 · 三级时间线

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/03-ux.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`contracts/openapi.json`、`docs/cr/CR-028-m4-detailed-spec.md`

可定位需求、issue、decision、commit 与证据，重放后相同

**必须行为**

- Project 看当前项目跨 Run 的规格、候选、Issue、决定、轮次结果和 Run 生命周期；Iteration 看选中 Run/iteration 的工程事实（含 Task、候选、测试、证据）；Agent 看明确 role/epoch/attempt 的公开消息、工具与交接。项目级事实没有 iteration 时不塞进某个轮次。按 seq 排序，时间戳仅展示。每项可沿类型化引用定位需求、Issue、Decision、commit、Evidence；保留过滤条件和返回锚点。

**失败与边界**

- 非法过滤/cursor 明确拒绝；合法无匹配显示空；未知事件有诊断块；broken/orphan/stale 引用不能伪装成可用证据。

**验收**

- **AC-013-01**（TEST-013-01 · 检查 `tui-pty`）Given F13，另有 REQ/issue/decision/commit/evidence 的完整追踪引用；120×32；When 依次选择 Project、R1/I1、A1；打开五类引用再返回；seq=8 后断线，补到 12；Then Project 可见 [1,5,7,8,9,10,12]；I1 可见 [2,5,6,7,8,9]；A1 可见 [3,4]；五类详情的 ID/spec/candidate/hash 与来源一致；断线前后最终列表、过滤条件和锚点与连续读取相同
  - 判定：Project 可见 [1,5,7,8,9,10,12]；I1 可见 [2,5,6,7,8,9]；A1 可见 [3,4]；五类详情的 ID/spec/candidate/hash 与来源一致；断线前后最终列表、过滤条件和锚点与连续读取相同
- **AC-013-02**（TEST-013-02 · 检查 `integration`）Given F13含重复/逆序timestamp；limit2、H12，之后追加13；所有合法过滤值与明确空集；When 按本节F13与H13完整期望集合翻页、重复cursor、改过滤/H、重建generation、试跨Run iteration/attempt；Then 严格seq序、每个并集精确等于表，无13混入；重复cursor同页；新/v1/timeline非法为400 QUERY_INVALID，旧Run路由非法仍404 BAD_REQUEST；两页路由generation过期410且顶层snapshot_watermark；SSE矛盾409为真实HTTP；410仅静态核对OpenAPI顶层形状、不伪造产品过期；H13各handoff完整成员集亦精确相同；合法空items=[]/next_cursor=null
  - 判定：严格seq序、每个并集精确等于表，无13混入；重复cursor同页；新/v1/timeline非法为400 QUERY_INVALID，旧Run路由非法仍404 BAD_REQUEST；两页路由generation过期410且顶层snapshot_watermark；SSE矛盾409为真实HTTP；410仅静态核对OpenAPI顶层形状、不伪造产品过期；H13各handoff完整成员集亦精确相同；合法空items=[]/next_cursor=null
- **AC-013-03**（TEST-013-03 · 检查 `integration`）Given 四条独立链：TEST缺evidence；当前修订中PASS绑定旧候选而非当前候选；批准新修订后旧修订验收节点；当前完整合法链；When 经refs往返详情，在新修订前后各查一次；Then 标签唯一：broken（缺evidence）、orphan（当前修订的旧候选PASS）、stale（旧修订全部验收节点）、current（合法链）；先做新修订失效再判旧PASS，stale不另写orphan/历史；身份/原seq保留、非current不计accepted；不生成证据/执行检查
  - 判定：标签唯一：broken（缺evidence）、orphan（当前修订的旧候选PASS）、stale（旧修订全部验收节点）、current（合法链）；先做新修订失效再判旧PASS，stale不另写orphan/历史；身份/原seq保留、非current不计accepted；不生成证据/执行检查
- **AC-013-04**（TEST-013-04 · 检查 `unit`）Given F13 的持久事件、重复 4/9、未知 12；两种交付分批及同一 H 快照；When 比较全量折叠、快照+增量、断线补历史三种规范化投影；切过滤再切回；枚举生成的ProducedEventKind::ALL并调用所有动态生产分支；运行生产构造/唯一INSERT的源码守卫与三种旁路负例（未分类variant用test_013_04的compile_fail,E0004 doctest）；另输入results生产的candidate_ready并尝试错误诊断分类；另核H13/notice身份；Then 块序列/refs/身份/诊断一致；candidate_ready在I中保留真实task/candidate/attempt/round引用，P/A不新增该项，UI_UNKNOWN_EVENT/诊断子集为空；将其映射到诊断的变异必须被断言拒绝，不能以“有分类”放行。所有生产kind均有显式分类，含cancel.native_cancel/sigterm/sigkill/reaped/lost/unreaped、effect.committed、完整lease/runner收尾30种、integrity_violation及原config/compat/session集合；动态分支输出与登记一致；未分类variant不能编译、原始字符串和旁路INSERT被守卫拒绝，任一绕过负例未被检出则测试失败；H13 role/epoch/session集合与表完全一致；12 恰一诊断；切过滤不改变事件真值；无以 timestamp 排序、以 Agent 名合并 role 或从模型摘要创建状态的行为
  - 判定：块序列/refs/身份/诊断一致；candidate_ready在I中保留真实task/candidate/attempt/round引用，P/A不新增该项，UI_UNKNOWN_EVENT/诊断子集为空；将其映射到诊断的变异必须被断言拒绝，不能以“有分类”放行。所有生产kind均有显式分类，含cancel.native_cancel/sigterm/sigkill/reaped/lost/unreaped、effect.committed、完整lease/runner收尾30种、integrity_violation及原config/compat/session集合；动态分支输出与登记一致；未分类variant不能编译、原始字符串和旁路INSERT被守卫拒绝，任一绕过负例未被检出则测试失败；H13 role/epoch/session集合与表完全一致；12 恰一诊断；切过滤不改变事件真值；无以 timestamp 排序、以 Agent 名合并 role 或从模型摘要创建状态的行为
- **AC-013-05**（TEST-013-05 · 检查 `integration`）Given 两Run事件交错；R1关轮前开放I1、同Task跨两轮重试，含run_id=null项目事件；分别准备COMPLETED、FAILED、CANCELLED、PAUSED、INCOMPLETE五个独立Run变体及真实既有iteration.completed旧库；When 经产品单writer/CAS与真实控制或预算路径逐事务追加/关轮、kill/reopen并回填；完成变体观察run.result(state=COMPLETED)，失败变体观察run.result(state=FAILED)，取消变体先cancel_requested再cancelled及result，暂停变体观察paused及result；INCOMPLETE前后取H1，显式ResumeRun生效后取H2，重查相同尾事件e与旧H1页；核对milestone_ready/release_request；Then 每事件按§2.1状态表与Run+close区间归属；close属被关轮、开放/关闭key一致，跨轮Task不被tasks.iteration_id吞并。COMPLETED/FAILED/CANCELLED及INCOMPLETE在对应H不展示N+1，未关闭尾段iteration=null/只P；cancel_requested、PAUSED（含run.result.state=PAUSED）仍保留原开放轮，BLOCKED亦不因result而终结；milestone_ready/release_request不改变终结判断。INCOMPLETE恢复H2重开原N+1，e由H1的null变为H2的该轮；H1重查逐字段/成员不变，无新close/预算计数。项目null事件不从payload补Run；重启/回填/在线集合相同；缺损边界index_ready=false且诊断，不伪造完整
  - 判定：每事件按§2.1状态表与Run+close区间归属；close属被关轮、开放/关闭key一致，跨轮Task不被tasks.iteration_id吞并。COMPLETED/FAILED/CANCELLED及INCOMPLETE在对应H不展示N+1，未关闭尾段iteration=null/只P；cancel_requested、PAUSED（含run.result.state=PAUSED）仍保留原开放轮，BLOCKED亦不因result而终结；milestone_ready/release_request不改变终结判断。INCOMPLETE恢复H2重开原N+1，e由H1的null变为H2的该轮；H1重查逐字段/成员不变，无新close/预算计数。项目null事件不从payload补Run；重启/回填/在线集合相同；缺损边界index_ready=false且诊断，不伪造完整

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- （CR-028，CTO 2026-11-21 批准 D1–D12）D1/D3/D4；§1/2/2.1 的水位/游标/迭代归属；本 CR 的完整 kind 分类、F13/H13 集合及应用核对的生产者闭包。AC 中的章节号、F13/H13 与参考环境均指 docs/cr/CR-028-m4-detailed-spec.md；2026-11-25 应用核对仅更新来源/已实现前提及已授权的事件分类和 GC 根，不修改 AC 语义或阈值。
- C3（CTO 2026-11-25）：M4-T06 交付 Timeline/状态栏共用 progress 聚合；依赖它的 M4-T12 才能验证 AC-011-04 的四组事实，不以 T02 的 unknown 占位代替证据，不新增或移动 AC。

## REQ-014 · 真实进度状态栏

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/03-ux.md`、`docs/01-prd.md`、`docs/05-data-api.md`、`docs/adr/ADR-029.md`、`docs/cr/CR-025-budget-risk-role-binding-pending-gate-date-time.md`、`docs/cr/CR-028-m4-detailed-spec.md`

显示 n/m accepted、open/reopened issues；未知成本/上下文显示未知且指明 Agent

**必须行为**

- 状态栏可配置但不能隐藏真实进度/阻塞；标准和精简在窄屏优先保留四组事实，可多行换行，详细字段从状态页可达。
- `m` 是当前已批准 spec 中的全部需求数，不是 Task 数、AC 数，也不是仅已排进当前 Run 的需求。没有规格时显示“未批准规格，0/0”，不显示 100%。`n` 只计当前附着 Run 的最新候选上、身份与该 spec 一致、全部 AC 被有效验收且候选已 ACCEPTED 的需求；不拼接不同候选或不同 Run 的 PASS。尚未纳入任务的需求仍在分母中，分子为 0。历史/跨 Run 的通过可查，但不借给当前候选。
- required 需求全部 AC 为 PASS 才计入；degradable 全部 AC 为 PASS 时可计入 n；含合规 DEGRADED 时 n 不加 1、m 保留该需求、只给 `degraded=k` 加 1，不得写为全部 PASS。无 AC、证据失效/缺失、orphan、UNKNOWN、BLOCKED、FLAKY、SKIPPED 均不计。客户端调用 host 的验收投影，不重写 Gate 规则。
- 新候选未 ACCEPTED 或规格变化时 n=0；显示新 spec revision 与新增/删除的需求 ID 差集数量。删除的范围只显示已批准的变更，不由 UI 修改。旧计数保留在历史详情；`spec_review` PASS、TaskResult COMPLETED 和模型“100%完成”不增加分子。
- `open` 只计状态函数得出的 OPEN，`reopened` 只计 REOPENED，避免重复计数；按当前 Run 的唯一 issue_id 统计。状态栏阻塞数取 Run/Task 的持久阻塞，不把等待服务、排队和离线混为失败。
- 未附着时显示“未附着 Run”、n/m=未知、阻塞/issue=未知；沿 ADR-031 显示项目 baseline，不把最近 Run 偷当当前。新候选归零仍显示候选 ID，最近 ACCEPTED 的计数仅在历史详情，不用它填当前 n。默认优先序：mode（含 pending）、阶段/任务、n/m accepted、blocked/open/reopened；随后 Git 分支/候选、运行时间、预算。Focus 才显示该职位当次 Attempt 的 Agent、实际解析模型、上下文；配置模型不冒充 resolved model。成本与上下文未知显示“未知（role/agent，原因）”，订阅调用不推算美元费用。已知部分成本显示“已知部分”，不是总额。
- 活动分类依据显式工具开始/结束、服务等待、原生压缩、调度排队、终结和最后活动事实。只有进程存活时显示“进程存活；活动未知”；没有在途监督的 Commander 显示“事件监测中”。模型摘要注明“分析”，不能覆盖确定性状态。缺上下文上限、缺已用量或计量身份不匹配时百分比为 null；不从字符数估算 token。

**失败与边界**

- 缺成本/上下文/活动事实或身份不匹配时显示未知，不能填 0；历史、规格审查与 TaskResult COMPLETED 不计当前 accepted；断线快照标 stale。

**验收**

- **AC-014-01**（TEST-014-01 · 检查 `tui-pty`）Given r1有RQ1(2AC)、RQ2(1AC)、RQ3(1AC)；C1范围仅RQ1/2且ACCEPTED，RQ3不在该Run任何Task中；C1接受后新增两个仅关联RQ1/2的blocked Task，现有5个done/共7个Task；OPEN2/REOPENED1/RESOLVED1；qa/pi配置模型requested-X、实际resolved-Y，未知费用/上下文；When 打开status/Focus；新C2与budget.risk；批准r2新增RQ4并删除RQ3（fixture模拟用户批准，不由UI擅删）；Then 先2/3 accepted、blocked=2/open=2/reopened=1；分子不取Task/AC数，blocked按Task唯一ID且Run级reason单列不双计；Focus显示resolved-Y，requested-X仅配置详情；费用与上下文值明确显示“未知（qa/pi，原因）”，字段不得显示0、$0或0%来代替未知；C2为0/3；r2为0/3、revision2/added1/removed1；旧计数仅历史；预算原因/上限可查
  - 判定：先2/3 accepted、blocked=2/open=2/reopened=1；分子不取Task/AC数，blocked按Task唯一ID且Run级reason单列不双计；Focus显示resolved-Y，requested-X仅配置详情；费用与上下文值明确显示“未知（qa/pi，原因）”，字段不得显示0、$0或0%来代替未知；C2为0/3；r2为0/3、revision2/added1/removed1；旧计数仅历史；预算原因/上限可查
- **AC-014-02**（TEST-014-02 · 检查 `unit`）Given 无spec、零AC、某AC FAIL、FROZEN/STALE/INVALID、旧spec PASS、spec_review PASS、TaskResult COMPLETED；另spec共3需求：同一ACCEPTED候选范围内一个全PASS、一个合规DEGRADED，另一个未纳入该Run任务范围的需求未满足；When 与host共用聚合函数，重复验收事实；Then 不满足者分子0，无spec为0/0且未完成；3需求例为accepted=1/3、degraded=1，DEGRADED不进分子且在分母；只有当前spec+候选ACCEPTED且该需求所有AC PASS才加1；重复不增计
  - 判定：不满足者分子0，无spec为0/0且未完成；3需求例为accepted=1/3、degraded=1，DEGRADED不进分子且在分母；只有当前spec+候选ACCEPTED且该需求所有AC PASS才加1；重复不增计
- **AC-014-03**（TEST-014-03 · 检查 `tui-pty`）Given 80/120/180×32；三档，custom=[budget,candidate]；可识别H1/H2响应与独立Timeline seq；When 切档重开，Focus/离开，候选更新和Issue重开并发；Then minimal仅mode/阶段任务/n-m/blocked-issues四组；standard依次四组+git/candidate/elapsed/budget；custom四组+budget/candidate严格按序；窄屏可换行、超出可选项进状态页；保存后重开保持差异；Focus上下文只用该身份；状态栏/进度同帧只来自一个完整getRun响应H（探针可查），Timeline另显last_applied_seq不强称同H；非法数组拒绝保旧档
  - 判定：minimal仅mode/阶段任务/n-m/blocked-issues四组；standard依次四组+git/candidate/elapsed/budget；custom四组+budget/candidate严格按序；窄屏可换行、超出可选项进状态页；保存后重开保持差异；Focus上下文只用该身份；状态栏/进度同帧只来自一个完整getRun响应H（探针可查），Timeline另显last_applied_seq不强称同H；非法数组拒绝保旧档
- **AC-014-04**（TEST-014-04 · 检查 `integration`）Given 明确的工具开始/结束及已知最后活动时间t1/t2、服务等待、压缩、排队、空闲/进程 alive，另含与工具事实冲突的模型摘要“任务已完成”，及 context used=25/limit=100、used 缺失、limit 缺失三例；两个 Agent 只有一个可计美元；When 查询 progress/ui 活动与用量，并在真实PTY观察Focus/状态详情，再换 role_epoch；Then 状态分别为工具运行/等待服务/压缩/排队/空闲；最后活动时间由对应已提交事实从t1更新t2并可见（UTC时间或可核对相对时间），未知明确显示未知；摘要标“分析”，与工具运行并列而不覆盖它/不变更确定状态或accepted；仅 alive 为活动未知，Commander 无 turn 为事件监测中；25/100=25%，缺任一数值为 null；成本标已知部分并列缺项身份，不能当总额；换绑不沿用旧 epoch 的用量
  - 判定：状态分别为工具运行/等待服务/压缩/排队/空闲；最后活动时间由对应已提交事实从t1更新t2并可见（UTC时间或可核对相对时间），未知明确显示未知；摘要标“分析”，与工具运行并列而不覆盖它/不变更确定状态或accepted；仅 alive 为活动未知，Commander 无 turn 为事件监测中；25/100=25%，缺任一数值为 null；成本标已知部分并列缺项身份，不能当总额；换绑不沿用旧 epoch 的用量
- **AC-014-05**（TEST-014-05 · 检查 `adapter-live`）Given 当前冻结候选host与三家已认证的真实native会话，各给定只读小文件与唯一公共回复标记；记录compat版本/session/resolved model；When 各调用原生工具读该文件并给公共消息，结束前后读持久块/activity；detach/reconnect，再对一会话取消；Then codex/claude/pi均有可核对原生session，公开回复/工具开始结束经真实producer落库，持久身份/seq与原始公开帧对应；已有在线usage/compact/wait字段一致、未提供则unknown而非猜测；detach不取消，显式取消保留前缀及终结事实；缺认证沿adapter-live既有约定为阶段BLOCKED、逐测试SKIPPED且detail注明verdict=BLOCKED、退出3，不记PASS；live-sessions.json与原始证据来自adapter-live，不以fixture替代
  - 判定：codex/claude/pi均有可核对原生session，公开回复/工具开始结束经真实producer落库，持久身份/seq与原始公开帧对应；已有在线usage/compact/wait字段一致、未提供则unknown而非猜测；detach不取消，显式取消保留前缀及终结事实；缺认证沿adapter-live既有约定为阶段BLOCKED、逐测试SKIPPED且detail注明verdict=BLOCKED、退出3，不记PASS；live-sessions.json与原始证据来自adapter-live，不以fixture替代

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- （CR-028，CTO 2026-11-21 批准 D1–D12）D2/D8/D11；§3 的真实需求计数、DEGRADED 单列、未知用量与活动；§4 三档 statusline。AC 中的章节号、F13/H13 与参考环境均指 docs/cr/CR-028-m4-detailed-spec.md；2026-11-25 应用核对仅更新来源/已实现前提及已授权的事件分类和 GC 根，不修改 AC 语义或阈值。

## REQ-015 · 输入与终端兼容

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/03-ux.md`、`docs/01-prd.md`、`docs/09-testing.md`、`docs/adr/ADR-014.md`、`docs/adr/ADR-031.md`、`docs/cr/CR-028-m4-detailed-spec.md`

按显示列宽处理；粘贴不误发送；异常退出恢复 raw/alternate screen

**必须行为**

- Unicode grapheme/显示宽度、多行编辑、括号粘贴、不依赖增强键盘协议；resize/网络分片/异常退出都不丢原草稿或误发命令。teardown 覆盖 raw、alternate screen、光标、bracketed paste 和新增 mouse reporting；正常退出、SIGTERM、SIGHUP、panic/unwind/初始化失败均恢复可恢复的终端状态。SIGKILL/断电不能执行 teardown，不能宣称可恢复（恢复 host 另属 REQ-041）。

**失败与边界**

- 失败时沿用 REQ-004 的退出码，不新增用户命令。

**验收**

- **AC-015-01**（TEST-015-01 · 检查 `tui-pty`）Given §4锁定Unicode数据生成的完整corpus、全部已提交平台的四路径实测压缩表与manifest；生产UI常量/边框集合和直接读取的docs/03中文正文；外层PTY及pane、80/120/180列；含✈️、👩‍💻、👨‍👩‍👧‍👦、👍🏻、👍🏽；两行CRLF粘贴；缺表、路径分歧与跨平台分歧负例；When 从锁定数据重生成corpus逐项核对，检查机器重测四路径全表并比较版本/解压后的规范内容；每码点/序列每路径捕获HoH实际绘制流/cells/cursor，测非零起点/末列/后续标尺、移动删除/resize；生产恒用全部平台全部表交集，验证安全项原样、缺表/分歧/不安全项回退；穷举UI非ASCII字形并显示非手选中文段落；paste再Enter；各路径debug panic；在会话内shell取stty/modes/哨兵，最后退出包装器补查外层；Then corpus与锁定数据规定的并集逐项一致，无码点/序列缺失、额外项或重复；每项均实测，压缩解压无损，重生成版本/表与提交表逐项相等，漂移/缺项/差异即FAIL且不改源表。每条语料每条路径（含内外层）无列漂移、无半字符、HoH光标=终端报告，标尺与实测跨度对齐；不能漏测一致但不同于unicode-width的肤色簇。HoH UI常量和所用边框集合的每个非ASCII字形（含↑↓、┌┐└┘）均在全部平台路径安全交集内且原样绘制；docs/03直接读取的中文正文无任何码点串。中文、Ambiguous标点/框线、🙂✅、✈️❤️⚠️▶️、🇺🇸及一致的肤色簇均为原字素，跨度仅取生成表。任一表缺项/分歧/不安全只用§4唯一ASCII回退，跨平台也一样，最终光标相等；无运行时路径确认分支；复制/提交原文不变。不得用unicode-width/wcwidth/CellWidth自证或按字符类别回退；Backspace删完整原字素；paste前命令0、Enter后恰1且LF两行；panic101；各shell/pane在包装器存活时stty与进入前相同，本实例模式配对关闭、哨兵完整；外层无泄漏仅附加，四路径任缺FAIL非零
  - 判定：corpus与锁定数据规定的并集逐项一致，无码点/序列缺失、额外项或重复；每项均实测，压缩解压无损，重生成版本/表与提交表逐项相等，漂移/缺项/差异即FAIL且不改源表。每条语料每条路径（含内外层）无列漂移、无半字符、HoH光标=终端报告，标尺与实测跨度对齐；不能漏测一致但不同于unicode-width的肤色簇。HoH UI常量和所用边框集合的每个非ASCII字形（含↑↓、┌┐└┘）均在全部平台路径安全交集内且原样绘制；docs/03直接读取的中文正文无任何码点串。中文、Ambiguous标点/框线、🙂✅、✈️❤️⚠️▶️、🇺🇸及一致的肤色簇均为原字素，跨度仅取生成表。任一表缺项/分歧/不安全只用§4唯一ASCII回退，跨平台也一样，最终光标相等；无运行时路径确认分支；复制/提交原文不变。不得用unicode-width/wcwidth/CellWidth自证或按字符类别回退；Backspace删完整原字素；paste前命令0、Enter后恰1且LF两行；panic101；各shell/pane在包装器存活时stty与进入前相同，本实例模式配对关闭、哨兵完整；外层无泄漏仅附加，四路径任缺FAIL非零
- **AC-015-02**（TEST-015-02 · 检查 `tui-pty`）Given 无增强协议四路径，草稿甲、带可观测身份的受控后台Attempt；分别无层编辑器/列表/临时层；输入历史有“旧稿乙”；独立原始SGR应用PTY及tmux占用鼠标变体；When 键盘：Ctrl+J输入乙、Ctrl+J输入丙后Enter；各焦点Esc后50/100/500ms再Enter、单write \x1b\r、两write间隔10/20ms、连续Enter/Alt+Enter；Esc→编辑→Enter、Esc→粘贴→Enter、Esc→Ctrl+R接受历史→另一次Enter；列表Enter、编辑器?、非空Ctrl+C；空编辑器Ctrl+C打开后分别取消菜单、选脱离/暂停/取消；可辨认Shift+Enter。鼠标：向应用PTY注入坐标300的SGR紧接Enter/中文键、残缺SGR/X10；tmux占用时仍全跑键盘；Then 首例甲\n乙\n丙恰1 SendInput（正文中的换行按LF）；所有慢/快/同批Esc-Enter无SendInput、原稿不变/零新LF；有层Esc只退一层、无层也解除armed，所有Esc子例PauseRun=CancelRun=0、Run状态及原Attempt身份/存活不变。连续Enter/Alt+Enter不绕guard；编辑/成功粘贴/接受不同历史才重武装，历史确认Enter本身发送0，随后显式Enter才恰1；只有Ctrl+J换行，Shift+Enter无动作。非空编辑器Ctrl+C：草稿变空，SendInput=PauseRun=CancelRun=0。空编辑器Ctrl+C：显示脱离/暂停/取消三项，未选和Esc关闭时控制命令0；选脱离仅客户端退出/控制0，选暂停恰1 PauseRun，选取消恰1 CancelRun且不附带其他命令。列表Enter仅展开，?为文字；SGR后键不吞/X10跳过/畸形有界；tmux拦截鼠标只豁免被拦点击、不豁免键盘/退出配对
  - 判定：首例甲\n乙\n丙恰1 SendInput（正文中的换行按LF）；所有慢/快/同批Esc-Enter无SendInput、原稿不变/零新LF；有层Esc只退一层、无层也解除armed，所有Esc子例PauseRun=CancelRun=0、Run状态及原Attempt身份/存活不变。连续Enter/Alt+Enter不绕guard；编辑/成功粘贴/接受不同历史才重武装，历史确认Enter本身发送0，随后显式Enter才恰1；只有Ctrl+J换行，Shift+Enter无动作。非空编辑器Ctrl+C：草稿变空，SendInput=PauseRun=CancelRun=0。空编辑器Ctrl+C：显示脱离/暂停/取消三项，未选和Esc关闭时控制命令0；选脱离仅客户端退出/控制0，选暂停恰1 PauseRun，选取消恰1 CancelRun且不附带其他命令。列表Enter仅展开，?为文字；SGR后键不吞/X10跳过/畸形有界；tmux拦截鼠标只豁免被拦点击、不豁免键盘/退出配对
- **AC-015-03**（TEST-015-03 · 检查 `integration`）Given PTY按UTF-8/CRLF/起止标记边界分块（不声称内核保留单字节write）；草稿保留；合法最终1MiB与+1B；64MiB无结束paste；三家adapter编码/回显fixture；两个不同input_id/block_id的相同正文，以及同block_id/stream两个不同first_offset的相同段；When 合法/超限paste，结束后普通键；未结束时SIGTERM；从HTTP受理经真实投递链对每adapter测试§4的256KiB/1MiB/最坏转义与不可读文件，另测超8MiB请求，以及合法非SendInput命令用JSON空白补到3MiB；对已202的大正文在物化Attempt可读根文件前制造低空间、再释放重试；登记上述两类重复内容并在reopen后重放同input_id/native身份/offset；Then 完整paste不自动提交，超限原稿不变且UI_INPUT_TOO_LARGE；残缺保留≤1MiB+6B，丢尾到结束或信号，RSS增≤16MiB（RSS子例由本TEST在§5.1串行release阶段运行）；后续键正常、无斜杠执行；每adapter inline≤256KiB且编码≤768KiB、否则短文件引用≤16KiB，文件位于Attempt可读根且底座fixture可读取、字节/hash精确；Workspace/Focus展开的用户块等于已接受原文（不是短引用）、同input_id恰一块；每帧≤1MiB不触发PROTOCOL_FRAME_TOO_LARGE；超8MiB以及3MiB非SendInput均400 BAD_REQUEST、无202/commands或events写入、草稿保留；已202输入投递文件低空间时保持QUEUED并记STORAGE_LOW_SPACE及available/required/min_free，未DELIVERED/APPLIED；释放后原input_id仅投递/用户块各一次，不可读/摘要错明确REJECTED；fixture标product，非live；环境不符RSS子例SKIPPED/退出78、整AC不通过；相同正文的两次独立输入在Workspace/Focus各保留一块，同块不同offset的相同段按offset完整展开/导出，可复用artifact_id而不丢字节；重放同input_id/native身份/offset仍恰一份对应块/段
  - 判定：完整paste不自动提交，超限原稿不变且UI_INPUT_TOO_LARGE；残缺保留≤1MiB+6B，丢尾到结束或信号，RSS增≤16MiB（RSS子例由本TEST在§5.1串行release阶段运行）；后续键正常、无斜杠执行；每adapter inline≤256KiB且编码≤768KiB、否则短文件引用≤16KiB，文件位于Attempt可读根且底座fixture可读取、字节/hash精确；Workspace/Focus展开的用户块等于已接受原文（不是短引用）、同input_id恰一块；每帧≤1MiB不触发PROTOCOL_FRAME_TOO_LARGE；超8MiB以及3MiB非SendInput均400 BAD_REQUEST、无202/commands或events写入、草稿保留；已202输入投递文件低空间时保持QUEUED并记STORAGE_LOW_SPACE及available/required/min_free，未DELIVERED/APPLIED；释放后原input_id仅投递/用户块各一次，不可读/摘要错明确REJECTED；fixture标product，非live；环境不符RSS子例SKIPPED/退出78、整AC不通过；相同正文的两次独立输入在Workspace/Focus各保留一块，同块不同offset的相同段按offset完整展开/导出，可复用artifact_id而不丢字节；重放同input_id/native身份/offset仍恰一份对应块/段
- **AC-015-04**（TEST-015-04 · 检查 `tui-pty`）Given 独立后台Run与外层shell；三类起始态：干净主屏、主屏已用DECSC在另一行保存旧光标、前进程遗留mouse/paste/alternate；前两类预写多行唯一标记并把当前光标置于第r行且r不同于旧保存行，记录基线stty/单元格/光标；mouse=true、debug故障点；When 干净/旧DECSC各做正常启动脱离往返，在teardown完成且任何诊断/shell新提示输出前取外层buffer；残留态单独启动；再各测SIGINT/SIGTERM/SIGHUP、panic、alt后初始化失败、部分输出失败、信号与I/O同时失败；退出后读哨兵；Then 所有启动复位仅关闭1000/1002/1003/1006/2004，不含1049l/1047l；本实例完整1049h后仅teardown配对一次1049l；干净与旧DECSC往返原有行的文本/位置及光标行r（并核列）与基线一致；残留alt只检查mouse/paste复位，不承诺旧屏/旧termios恢复。可写PTY退出stty等于本次进入前、实际开启模式逐项关闭/光标显示；正常0、信号130/143/129、panic101、I/O74、并发信号码优先；恢复一次且不吞哨兵，后台无Pause/Cancel、新客户端可查
  - 判定：所有启动复位仅关闭1000/1002/1003/1006/2004，不含1049l/1047l；本实例完整1049h后仅teardown配对一次1049l；干净与旧DECSC往返原有行的文本/位置及光标行r（并核列）与基线一致；残留alt只检查mouse/paste复位，不承诺旧屏/旧termios恢复。可写PTY退出stty等于本次进入前、实际开启模式逐项关闭/光标显示；正常0、信号130/143/129、panic101、I/O74、并发信号码优先；恢复一次且不吞哨兵，后台无Pause/Cancel、新客户端可查
- **AC-015-05**（TEST-015-05 · 检查 `integration`）Given §5.1参考VM、release串行，百万库；80×24与180×40各≥1000样本，50ms增量合并负载；When 并发提交事件并编辑/滚动/resize，记录输入→外层屏幕、commit→seq可见；插入tool/decision/acceptance边界；Then 各尺寸输入p95≤100ms、事件p95≤500ms；每个合并窗口≤50ms或16KiB先到，关键事件计数完整；无丢键/重复提交，非renderer独立计时；环境不符SKIPPED/退出78不通过，真实超限FAIL保留样本
  - 判定：各尺寸输入p95≤100ms、事件p95≤500ms；每个合并窗口≤50ms或16KiB先到，关键事件计数完整；无丢键/重复提交，非renderer独立计时；环境不符SKIPPED/退出78不通过，真实超限FAIL保留样本

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- （CR-028，CTO 2026-11-21 批准 D1–D12）D9/D10；§4 的唯一 Ctrl+J 换行、Esc/Alt+Enter guard、原屏所有权、全量 Unicode 逐路径实测表/完整交集及唯一回退；§5.1 的帧/paste/性能测量；REQ-015 两平台矩阵。AC 中的章节号、F13/H13 与参考环境均指 docs/cr/CR-028-m4-detailed-spec.md；2026-11-25 应用核对仅更新来源/已实现前提及已授权的事件分类和 GC 根，不修改 AC 语义或阈值。

## REQ-016 · 无障碍与键位

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/03-ux.md`、`docs/01-prd.md`、`docs/09-testing.md`、`config/hoh.example.toml`、`docs/cr/CR-028-m4-detailed-spec.md`

所有重要操作可键盘执行；不只靠颜色；有冲突检测与恢复默认

**必须行为**

- 重要操作全部有键盘路径；状态有文字与形状/ASCII 标记，不只靠颜色；焦点可见；NO_COLOR、无鼠标与 ASCII 图标同时可用。自定义键位按 scope 校验并可恢复默认。

**失败与边界**

- 冲突列出两个 action/scope/key，保存整体拒绝；非法配置不破坏现有 Run；没有终端能力不显示虚假的启用状态。

**验收**

- **AC-016-01**（TEST-016-01 · 检查 `tui-pty`）Given NO_COLOR=1、ui.mouse=false、ui.icons=ascii，80×32；list.copy=c与冲突配置；历史“旧稿乙”、当前草稿甲，当前Run A与可选INCOMPLETE Run B；When 键盘遍历四视图/角色会话/三抽屉/块/模式/设置/提交/控制；Ctrl+R检索并Enter接受旧稿，另一次检索Esc取消；/resume打开选择器、Esc退出、重开并选B；保存冲突后恢复默认；Then 每动作有逐项界面/命令记录；Ctrl+R确认只将旧稿乙放编辑器、SendInput=0，下一显式Enter才可提交；取消检索恢复原草稿且不提交。/resume未选前A与草稿不变、选项列B、无POST/控制；Esc只关选择器；选择B才附着其快照/SSE，B仍INCOMPLETE且ResumeRun/PauseRun/CancelRun/Task/Attempt新增均0。无颜色SGR/鼠标启用，状态有文字和ASCII；c复制、冲突整体拒绝，恢复后y复制/c未绑定；显式后台控制每次仅对应一条命令
  - 判定：每动作有逐项界面/命令记录；Ctrl+R确认只将旧稿乙放编辑器、SendInput=0，下一显式Enter才可提交；取消检索恢复原草稿且不提交。/resume未选前A与草稿不变、选项列B、无POST/控制；Esc只关选择器；选择B才附着其快照/SSE，B仍INCOMPLETE且ResumeRun/PauseRun/CancelRun/Task/Attempt新增均0。无颜色SGR/鼠标启用，状态有文字和ASCII；c复制、冲突整体拒绝，恢复后y复制/c未绑定；显式后台控制每次仅对应一条命令
- **AC-016-02**（TEST-016-02 · 检查 `unit`）Given keybinding 表：同 list 同 key 两动作；global 与 editor 重叠；editor/list 同 key；未知 key/action/scope；覆盖 Ctrl+C/Esc/Enter/Ctrl+J；将面板重绑为未占用 Ctrl+O，以及使面板无有效入口的配置；When 校验各表，分别试保存；读取错误 details；Then 前两种冲突与未知/保留覆盖/无面板入口均拒绝，给出准确 action/scope/key；互斥 editor/list 同 key 及合法 Ctrl+O 重绑允许；失败后有效映射逐字段不变；所有默认动作仍可从命令面板到达
  - 判定：前两种冲突与未知/保留覆盖/无面板入口均拒绝，给出准确 action/scope/key；互斥 editor/list 同 key 及合法 Ctrl+O 重绑允许；失败后有效映射逐字段不变；所有默认动作仍可从命令面板到达
- **AC-016-03**（TEST-016-03 · 检查 `tui-pty`）Given editor/list/dialog 各 scope，草稿含 `? / f y n`；重绑 list.search 为 Ctrl+F，面板改为 Ctrl+O；When editor输入唯一前缀/he后Tab，再试歧义前缀/；普通editor/list/dialog各Tab移焦，Ctrl+O打开面板，Esc返回；使用重绑键，保存并重开客户端，在面板恢复默认；Then 唯一前缀/he经Tab补为/help、焦点仍在editor，Enter前无提交/SendInput；歧义前缀/只移焦、原文字不变、无命令；非补全Tab依scope移焦且焦点可见。编辑器文字不被列表/帮助热键吞掉；焦点可见且 Esc 回到原草稿/锚点；重开后绑定仍有效；恢复后 Ctrl+P 再次打开面板；只清键覆盖，team/budget/Prompt/mode_cycle、配置注释与其他键均不变；ui-only保存不产生ConfigureTeam/新team_revision，CAS冲突拒绝且原文件不变；mode.cycle重绑后顺序仍遵守ui.mode_cycle
  - 判定：唯一前缀/he经Tab补为/help、焦点仍在editor，Enter前无提交/SendInput；歧义前缀/只移焦、原文字不变、无命令；非补全Tab依scope移焦且焦点可见。编辑器文字不被列表/帮助热键吞掉；焦点可见且 Esc 回到原草稿/锚点；重开后绑定仍有效；恢复后 Ctrl+P 再次打开面板；只清键覆盖，team/budget/Prompt/mode_cycle、配置注释与其他键均不变；ui-only保存不产生ConfigureTeam/新team_revision，CAS冲突拒绝且原文件不变；mode.cycle重绑后顺序仍遵守ui.mode_cycle
- **AC-016-04**（TEST-016-04 · 检查 `unit`）Given 同一投影包含 PASS/FAIL/BLOCKED/UNKNOWN/STALE/DEGRADED/REOPENED、选中和未选中项；80/120/180 列；When 以彩色与 NO_COLOR+ASCII 两套样式渲染 TestBackend；Then 每种状态在去样式后的文字中仍可区分，选中项有 ASCII 焦点标记；禁色不含前景/背景色，保留必要非颜色属性；无超过显示宽度、无必需动作只有图标无标签
  - 判定：每种状态在去样式后的文字中仍可区分，选中项有 ASCII 焦点标记；禁色不含前景/背景色，保留必要非颜色属性；无超过显示宽度、无必需动作只有图标无标签

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- （CR-028，CTO 2026-11-21 批准 D1–D12）D8/D9/D10；§4 的 scope/action/key、保留键、面板可达、ui-only CAS 保存、历史与 /resume；NO_COLOR/ASCII/键盘完整路径。AC 中的章节号、F13/H13 与参考环境均指 docs/cr/CR-028-m4-detailed-spec.md；2026-11-25 应用核对仅更新来源/已实现前提及已授权的事件分类和 GC 根，不修改 AC 语义或阈值。

## REQ-017 · 运行中对话

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/03-ux.md`、`docs/06-orchestration.md`、`docs/12-user-guide.md`、`docs/adr/ADR-018.md`、`docs/adr/ADR-024.md`、`docs/adr/ADR-025.md`、`docs/adr/ADR-028.md`

Ask 不改任务；steer 有生效边界；CR 改规格；控制可独立于模型执行

**必须行为**

- 输入按种类路由（docs/03-ux.md）：普通输入默认给 Commander，@role 是稳定目标；ASK 走只读查询；STEER 记录生效边界；CHANGE 写 CR（REQ-033）；CONTROL 转为对应的控制命令（PauseRun、ResumeRun、CancelRun，C26）。输入状态为 QUEUED→DELIVERED→APPLIED|REJECTED。
- ASK 不改任务：回答作为消息返回，tasks 与 runs 的 revision 不变，不新增工作 Attempt；给非 Commander 职位的 ASK 不打断其运行中的 turn，在下一个安全点或经只读查询回答（C25）。
- STEER 的生效边界：绑定 Agent 支持 turn 内引导（mid_turn_steer）时在当前 turn 内投递，否则排到该职位的下一个 Attempt；事件 input.delivered{boundary:"turn"|"next_attempt"} 写明边界；新持有者的首个输入付清之前不投递（M2T17R-153）。
- @role 解析：一个 Agent 绑定多个职位时按当前任务的职位解析并显示实际目标，含糊时弹出选择器；投递只给当前 epoch 的绑定，换绑后迟到的投递被拒（STALE_ROLE_EPOCH，M2T17R-154）。
- 投递循环（ADR-018 决定 4）接入 host：QUEUED 的输入至少投递一次，以 (consumer, message_id) 去重；consume 的 KeyReuse 计为一次拒绝，不重试到死信（M2T17R-254）；领取永不取验证交接（ADR-028 决定 9）；host 重启后未 ACK 的照常重投；接管之后的 query/steer 路由取代 deferred 回应，历史上 deferred 的不补投（ADR-028 决定 13）。
- 控制可独立于模型执行：PauseRun、CancelRun 在 Commander 与全部 Agent 不可用时照常生效（AC-043-04 已判，本需求不重复）。

**失败与边界**

- ASK 之后 tasks 与 runs 的 revision 不变，没有新工作 Attempt。
- 换绑后发往旧 epoch 的投递为 REJECTED（STALE_ROLE_EPOCH）。
- host 在投递后、ACK 前崩溃，重启后重投且只应用一次。
- CHANGE 在批准前不改规格与任务。

**验收**

- **AC-017-01**（TEST-017-01 · 检查 `integration`）Given Run R 中 Task T1 运行，FakeCommander 可回答；When 向默认目标发 ASK“进度如何”；Then input_queue 中该输入依次为 QUEUED、DELIVERED、APPLIED 并带回答消息；全部 tasks 与 runs 的 revision 不变；没有新的工作 Attempt
  - 判定：输入状态依次 QUEUED、DELIVERED、APPLIED 并带回答；revision 不变；无新工作 Attempt
- **AC-017-02**（TEST-017-02 · 检查 `adapter-live`）Given 两个 Run 的 developer Attempt 运行中：一个绑定真实的 Codex CLI（App Server，支持 mid_turn_steer），另一个绑定不支持的假 Agent；When 各经产品 host 的 SendInput{input_kind:STEER, target_role:developer} 发一条引导；Then Codex 的一条经 host 的投递链在当前 turn 内到达（适配器的 steer 请求在该 turn 结束前发出），写 input.delivered{boundary:"turn"}；假 Agent 的一条在下一个 Attempt 的输入中收到，写 input.delivered{boundary:"next_attempt"}；两者都只投递一次
  - 判定：Codex 的经 host 投递链在当前 turn 内到达，boundary=turn；假 Agent 的 boundary=next_attempt；各只投递一次
- **AC-017-03**（TEST-017-03 · 检查 `integration`）Given 已批准修订 1 的 Run，Task 运行中；Commander 与全部 Agent 都不可用（假 CLI 不响应）；When 发 CHANGE“把导出格式改为 CSV”，再发 CONTROL“暂停”；Then CHANGE 新增一条 PROPOSED 的 change_requests（REQ-033），批准前 specs、runs.spec_id 与 tasks 的状态不变；CONTROL 转为一条 PauseRun 命令，Run 经 PAUSE_REQUESTED 进入 PAUSED；两者都没有任何模型 turn 参与
  - 判定：一条 PROPOSED 的 change_requests；specs、spec_id 与任务状态不变；CONTROL 成为 PauseRun 且 Run 进入 PAUSED；无模型 turn
- **AC-017-04**（TEST-017-04 · 检查 `tui-pty`）Given Agent claude 绑定 cto 与 developer；当前运行的是 developer 的 Task；随后 developer 换绑到 codex（epoch+1）；When TUI 输入“@claude 先写测试”，随后向换绑前的 epoch 投递一条迟到的消息；Then 消息栏显示实际目标 developer，input_queue 的 target_role=developer；迟到的投递为 REJECTED(STALE_ROLE_EPOCH)
  - 判定：显示实际目标 developer 且 target_role=developer；迟到投递 REJECTED(STALE_ROLE_EPOCH)
- **AC-017-05**（TEST-017-05 · 检查 `recovery`）Given 投递循环接入 host；三条 QUEUED 输入 M1、M2、M3，其中 M3 的 consume 会以 KeyReuse 失败；outbox 中有一条验证交接；When 投递一轮，在 M2 已投递、未 ACK 时 kill -9 host 并重启；Then 按 message_id 逐条：M1 应用 1 次；M2 的传输投递 2 次（重启前后）而业务应用 1 次（receipts 中 (consumer, M2) 恰一行）；M3 应用 0 次、记一次拒绝且不进死信；验证交接没有被领取
  - 判定：M1 应用 1 次；M2 传输 2 次、应用 1 次；M3 应用 0 次、拒绝 1 次、无死信；验证交接未被领取

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- ASK 不打断运行中的 turn（docs/12：问进度不会打断编码）（CR-026，C25）。
- CONTROL 输入即转为对应的控制命令，不经模型（CR-026，C26）。

## REQ-018 · 默认与自定义团队

M2 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`config/hoh.example.toml`、`CONTRACT.md`、`docs/06-orchestration.md`、`docs/04-architecture.md`、`docs/05-data-api.md`、`docs/13-adapters.md`、`docs/12-user-guide.md`、`contracts/protocol.schema.json`、`contracts/sql/001_initial.sql`、`docs/cr/CR-028-m4-detailed-spec.md`

按 config/hoh.example.toml 提供六个默认职位绑定，用户可经 ConfigureTeam 在 checkpoint 修改各职位的 Agent、模型、职责和回退，也可新增职位；职位与进程、会话解耦。

**必须行为**

- 必须在无项目配置或采用 config/hoh.example.toml 时得到同一有效绑定：commander=codex、cto=claude、developer=claude、qa=pi、researcher=pi、reviewer=codex，且 reviewer.independent_session=true。
- 必须同时载入同一文件的非团队默认值：execution.writer_slots=1、reader_slots=2、reserve_supervisor_slot=true，supervision.policy=balanced，budget.max_local_repairs=3、max_protocol_repairs=2，不内置 USD 预算默认值。
- 职位绑定必须是数据（role→agent/model/职责Prompt覆盖/有序fallback），不是常驻进程；同一 Agent 产品可承担多个职位，每个职位的 attempt 使用各自的 session_id。
- /team、Web 设置和配置文件三个入口都必须归一为 ConfigureTeam 命令，用于修改任一职位的 agent、model、职责覆盖和 fallback，或新增 [team.<name>] 自定义职位。
- ConfigureTeam 必须带 expected_revision 和 apply_at=checkpoint；持久接受后返回 202。运行中的 attempt 沿用旧绑定，新绑定在下一个 checkpoint 生效，受影响职位的 role_epoch 加 1。
- model 为空表示使用底座的实际默认模型，attempt 结束后记录 resolved model；每个 attempt 都要记录当时的 team 配置 revision 和 role_epoch。
- 配置的非法值和未知键都不能被静默忽略，也不能回退成默认团队；配置被拒绝时，上次已提交的 team revision 保持不变。

**失败与边界**

- agent 不是已注册适配器（codex/claude/pi）时，ConfigureTeam 返回 400 并给出键路径；team revision 和各 role_epoch 不变。
- expected_revision 落后于当前 team revision 时返回 409，错误体带 current_revision，任何绑定都不部分生效。
- 配置删除 commander/developer/qa/reviewer 中任一必需职位，或把 reviewer.independent_session 设为 false 时，拒绝该配置并指出键路径。
- apply_at 不是 checkpoint 时，按 protocol.schema.json 的 ConfigureTeam 分支校验失败，返回 400，运行中的会话不被立即替换。
- 配置文件 TOML 解析失败或 config_version≠1.0 时，host 报告文件路径和出错键，并拒绝用该配置启动新 run，不改用内置默认。

**验收**

- **AC-018-01**（TEST-018-01 · 检查 `unit`）Given 两份输入：一个没有任何用户配置的空临时目录；一份逐字复制的 config/hoh.example.toml；When 分别交给配置解析器，生成有效配置 A（无配置）和 B（example）；Then A 与 B 逐字段相等；六职位依次为 codex/claude/claude/pi/pi/codex，reviewer.independent_session=true；writer_slots=1、reader_slots=2、reserve_supervisor_slot=true、policy=balanced、max_local_repairs=3、max_protocol_repairs=2
  - 判定：A==B；team.<role>.agent 六项与期望表完全一致；execution/supervision/budget 的 6 个字段与 example 一致；有效配置中不存在任何 USD 预算默认值字段
- **AC-018-02**（TEST-018-02 · 检查 `integration`）Given FakeAgent 团队运行中：developer attempt A1 为 RUNNING，team revision=r，developer role_epoch=1，agent=claude；When 提交 ConfigureTeam{bindings:{developer:{agent:codex,model:m-test}},apply_at:checkpoint}，expected_revision=r；随后让 A1 settle，并派发下一个 developer 任务；Then 返回 202 且 status=ACCEPTED；A1 保持 agent=claude、role_epoch=1；下一个 attempt A2 为 agent=codex、resolved_model=m-test、role_epoch=2，config revision=r+1
  - 判定：HTTP 202，body.status=ACCEPTED；A1 运行期间 snapshot 显示 pending 变更，且生效绑定仍为 claude；绑定生效事件的 seq 大于 A1 结束事件的 seq；attempts(A2) 记录 role_epoch=2、config revision=r+1、resolved_model=m-test
- **AC-018-03**（TEST-018-03 · 检查 `integration`）Given 默认团队，cto 与 developer 都绑定 claude；FakeAgent 记录每次 spawn 及其 session_id；When 先派发一个 cto 任务和一个 developer 任务；然后结束 developer 会话进程，再派发一个 developer 任务；Then cto 与 developer 的 session_id 不同，sessions.role 分别为 cto 和 developer；进程退出后 team revision 不变，第三次派发成功，cto 的 session_id 未变化
  - 判定：session_id(cto)≠session_id(developer)；developer 进程退出前后 team revision 相等；第三个 attempt 状态为 SETTLED，cto 的 session 行未被修改
- **AC-018-04**（TEST-018-04 · 检查 `integration`）Given 运行中的项目，team revision=r；When 依次提交 5 个 ConfigureTeam：agent=unknown；删除 qa 职位；reviewer.independent_session=false；apply_at=now；expected_revision=r-1；Then 前 4 个返回 400，错误体含键路径；第 5 个返回 409 且 current_revision=r；全部结束后 team revision 仍为 r，各 role_epoch 不变，没有绑定生效事件
  - 判定：状态码依次为 400,400,400,400,409；错误体都含 code、message、retryable、current_revision 字段；team revision 前后相等，role_epoch 集合前后相等；events 中没有新增的绑定生效事件
- **AC-018-05**（TEST-018-05 · 检查 `integration`）Given 项目配置新增 [team.security_reviewer] agent=codex，并引用一个职责 Prompt 覆盖文件；When 加载配置后派发一个 role=security_reviewer 的 FakeAgent 任务；Then GetSnapshot 的团队列表含 security_reviewer 且 role_epoch=1；该 attempt 记录 prompt 来源为该覆盖文件及其 sha256；其 session 与 reviewer 会话不同
  - 判定：团队列表含 security_reviewer，role_epoch=1；attempt 的 prompt 来源路径与 sha256 等于覆盖文件；session_id(security_reviewer)≠session_id(reviewer)

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- example 只有 agent 与 reviewer.independent_session 两个键。选择在 [team.<role>] 下增加可选键 model（字符串，可空）、fallback（Agent 名有序数组）、prompt（覆盖文件路径），需与 example 同步更新。
- 文档没有给出配置校验错误码。选择 HTTP 400 + code=INVALID_TEAM_CONFIG（初值），版本冲突沿用 409。
- [team.*] 下出现未知键时，选择拒绝并指出键路径，不静默忽略。
- reviewer.independent_session=false 选择直接拒绝，因为独立审查是 CONTRACT 第 7 条的硬约束，不作为可选项。
- 自定义职位选择必须引用一个内置角色契约或提供 prompt 覆盖文件，否则拒绝。
- （CR-003）完整配置键表（未知键一律拒绝）：顶层 config_version；[project] default_mode、document_profile；[execution] profile、writer_slots、reader_slots、reserve_supervisor_slot、cancel_native_grace_seconds=10、cancel_term_grace_seconds=5；[delivery] max_delivery_attempts=5；[persistence] writer_queue_capacity=256；（CR-007）[scheduler] max_wait_rounds=20，[scheduler.weights] user_value=4、unblock_value=3、risk_reduction=3、information_gain=2；[team.<role>] agent、model、fallback、prompt、independent_session；[supervision] policy；[budget] max_duration_hours、max_iterations、max_local_repairs、max_protocol_repairs、max_usd、deadline_at；[ui] view、statusline、mouse；[web] host、port。config/hoh.example.toml 列出全部键（可选键以注释给出），内置默认值与之逐项相等。
- （CR-026）[assumptions] 表：键为决定类名（ASCII 小写字母、数字与下划线），值为字符串或字符串数组，是无人值守时允许假设的默认值（REQ-030，所有者 2026-11-17 的决定 O2）；product_direction、scope、non_goals 为保留名，出现即配置被拒（这三类永不假设）；config/hoh.example.toml 中为空表（与加载器的支持在同一提交加入，见 M3-T06），空表不产生任何假设；表外的未知键照旧拒绝。
- （CR-026）[ui] mode_cycle：Shift+Tab 循环的模式顺序（docs/03 的“配置可改”），缺省 ["PLAN", "BUILD", "AUTO"]，合法值为 2 或 3 个互不相同的 PLAN、BUILD、AUTO 组成的数组，其他值使配置被拒（REQ-007）；example 中的缺省值与加载器的支持在同一提交加入（M3-T09）。
- （CR-026）[supervision] max_discussion_rounds：同一争议的意见轮数上限（docs/06 的“默认可调上限”），缺省 2，合法值为 1–5 的整数，其他值使配置被拒（REQ-023）；example 中的缺省值与加载器的支持在同一提交加入（M3-T10）。
- （CR-028 §4）M4 配置键随所属实现任务与 example 同提交支持：ui.view 保留 workspace/team/focus 并增加 timeline（缺省 workspace，T06）；ui.statusline=standard/minimal/custom（缺省 standard），ui.statusline_fields 为 0–8 项不重复数组，依序选 git/candidate/elapsed/budget/agent/model/context/activity，必显四组事实不可移除（T06）。
- （CR-028 §4）ui.mouse 保留 true；ui.icons=auto/ascii，缺省 auto（T08）；ui.clipboard 仅 file，缺省 file，osc52 非法（T05）；ui.keybindings 缺省 []，元素为 {action,scope,key}，scope=global/editor/list/dialog，整体校验冲突、保留键与 global 命令面板可达性（T08）。ui.mode_cycle 的 M3 值与顺序不变。
- （CR-028 §4/5，T03）observability.log_segment_bytes 为 1048576–67108864 整数字节，缺省 8388608；log_segments 为 2–64 整数，缺省 5；orphan_grace_seconds 为 3600–604800 整数，缺省 172800；min_free_bytes 为 16777216–1073741824 整数字节，缺省 33554432。KiB=1024、MiB=1048576；轮换只作用于未引用诊断，引用证据/公开块/资源 pin/重放对象全部保护。
- （CR-028 §4）项目 .hoh/config.toml 的 ui-only 保存使用保注释 TOML、读时摘要 CAS、同目录临时文件+fsync+rename；失败整体拒绝并保留原文件。恢复默认只删除 ui.keybindings 覆盖，不改 team/budget/Prompt/mode_cycle。ui-only diff 交客户端重载，不产生 ConfigureTeam/team revision 或改变活跃 Run 配置快照；混合执行配置仍走原 ConfigureTeam。新键/example 必须与加载器同提交，回归 TEST-018-01；M4-T01 不提前改可运行 example。

## REQ-019 · 角色交接

M2 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/14-resilience.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/03-ux.md`、`contracts/protocol.schema.json`、`contracts/sql/001_initial.sql`、`tools/reference_model.py`

运行中更换职位持有者（如 Commander 从 Codex 换成 Pi）时，在安全点递增 role_epoch 并隔离旧会话指令；新持有者从 checkpoint 交接包接手，原任务不重复执行。

**必须行为**

- 绑定变更必须先持久化为待交接请求，在该职位当前 turn 结束或到达 writer 安全点后才生效；生效前界面显示 pending，不显示为已完成。
- 生效时必须在同一事务内完成：更新绑定、该职位 role_epoch+1、追加交接事件、写入新会话启动的 outbox。
- role_epoch 低于当前值的 decision、steer 和消息只进入历史（decisions.state=REJECTED，原因 STALE_ROLE_EPOCH），不得改变任何 task/run 状态，也不得产生派发。
- 新持有者的首个输入必须是 checkpoint 交接包，包含 goal、当前 spec_sha256/revision（Run 尚无 spec 时两者为 null，CR-014）、已 APPLIED 的决策、OPEN/REOPENED issue、未 ACK 的 outbox 与 QUEUED 输入、当前 candidate 身份；不能是旧会话全文。
- 交接不能重复原任务：已 LEASED/RUNNING 的 Task 及其 STARTING/RUNNING attempt 继续执行，已 ACKED 的消息不重投；新持有者对已租用或已 ACCEPTED 的 task 发出的派发，由租约/状态检查拒绝。
- 紧急中断写者时，必须把该 attempt 标为 UNKNOWN_EFFECT，先 reconcile 工作区 hash 与 commit，然后才允许新写者取得写租约。

**失败与边界**

- 旧 Codex Commander 在 epoch 1 失效后才返回 decision 时，该 decision 记为 REJECTED(STALE_ROLE_EPOCH)，task revision 不变，也不新增 outbox。
- 两个 ConfigureTeam 以相同 expected_revision 并发提交时，恰好一个返回 202，另一个返回 409 并带 current_revision。
- 新绑定的 Agent 在生效前探测失败或缺认证时，交接不生效：旧绑定与 epoch 保留，同时记录失败原因并发出角色失效事件。
- host 在 epoch 递增事务提交之后、新会话启动之前崩溃时，重启后保持新 epoch 与新绑定，只启动一个新会话。
- 新 Commander 对 RUNNING task 重复派发时被拒绝，该 task 的 attempt 数不增加。

**验收**

- **AC-019-01**（TEST-019-01 · 检查 `integration`）Given FakeAgent run：commander=codex(role_epoch=1)，task T 为 RUNNING(revision=5)；脚本让旧 Codex 在交接后返回 decision{role_epoch:1,expected_revision:5}；When 提交 ConfigureTeam{bindings:{commander:{agent:pi}},apply_at:checkpoint}，等交接生效后投递旧 Codex 的 decision；Then commander 变为 agent=pi、role_epoch=2；旧 decision 行 state=REJECTED，原因为 STALE_ROLE_EPOCH；T.revision 仍为 5；该 decision 之后 outbox 新增行数=0
  - 判定：decisions 中该行 state=REJECTED，原因含 STALE_ROLE_EPOCH；tasks.T.revision=5；旧 decision 处理后 outbox 行数不变；交接生效事件 seq < 旧 decision 拒绝事件 seq
- **AC-019-02**（TEST-019-02 · 检查 `integration`）Given run 含 goal G、spec revision 3、2 条 APPLIED decision、1 条 OPEN 与 1 条 REOPENED issue、1 条 QUEUED 输入、当前 candidate C；旧 Commander 原始日志中植入唯一哨兵串；When commander 从 codex 交接到 pi 并生效；Then 新 Commander 首个输入中的 ContextHandoff 通过 protocol.schema.json 校验，goal=G、spec_sha256 对应 revision 3、candidate.id=C；附带的决策/issue/待投递清单与 DB 一致；哨兵串出现 0 次
  - 判定：schema 校验 0 错误；决策 ID 集合 == APPLIED 集合；issue ID 集合 == OPEN∪REOPENED 集合；待投递 ID 集合 == 未 ACK outbox ∪ QUEUED input_queue；哨兵串计数=0
- **AC-019-03**（TEST-019-03 · 检查 `integration`）Given 交接前 task T 有 1 个 RUNNING attempt，消息 M 已 ACKED，task T2 已 ACCEPTED；When 交接生效后，由脚本化的新 Commander 提交对 T 和 T2 的 dispatch decision；Then T 在 settle 前 attempt 数始终为 1；T2 不再执行；M 的 outbox.attempt_count 不变；这两条 dispatch decision 记为 REJECTED 并写明原因
  - 判定：count(attempts where task_id=T)=1；count(attempts where task_id=T2) 与交接前相等；outbox(M).attempt_count 与交接前相等；两条 decision 的 state=REJECTED
- **AC-019-04**（TEST-019-04 · 检查 `integration`）Given commander=codex，team revision=r；When 两个客户端同时提交 expected_revision=r 的 ConfigureTeam：commander→pi 与 commander→claude；Then 恰好一个返回 202，另一个返回 409 且 current_revision=r+1；最终绑定等于被接受的那一个；commander role_epoch 只递增 1 次
  - 判定：每轮状态码组合为 {202,409}；每轮 role_epoch 增量=1；最终 agent 与 202 请求的 bindings 一致
- **AC-019-05**（TEST-019-05 · 检查 `recovery`）Given FakeAgent 团队，在交接事务提交后、Pi 会话启动前注入 kill -9 host；When 重启 host 并完成 reconcile；此后旧 Codex 进程继续输出 3 条 epoch=1 消息；Then 从重启到 reconcile 完成 ≤30 秒；commander 仍为 pi 且 role_epoch=2；role=commander、agent=pi 的新 session 恰好 1 个；3 条旧消息全部被拒绝，没有重复派发
  - 判定：reconcile 完成时间 - 重启时间 ≤30s；commander role_epoch=2，agent=pi；新 commander session 数=1；旧 epoch 拒绝数=3，dispatch outbox 无重复 message_id

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- role_epoch 按 (run_id, role) 维护（SQL role_bindings 表，CR-001），Commander 交接不会连带隔离 Developer 的在途结果，避免原任务重做。
- 新绑定 Agent 在生效时探测失败：选择保留旧绑定与旧 epoch，记录交接失败事件，而不是让该职位进入空缺。
- 文档没有定义 Commander 的安全点。选择：当前 Commander turn 结束，或者没有在途 turn；不主动中断在途 turn。

## REQ-020 · 能力匹配与缺员

M2 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/13-adapters.md`、`docs/06-orchestration.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/14-resilience.md`、`docs/12-user-guide.md`、`CONTRACT.md`、`config/hoh.example.toml`

首选 Agent 不可用时，只启用用户已授权、且探测结果满足该职位能力契约的 fallback；没有符合条件的 fallback 时该职位 BLOCKED。独立 QA/Review 不能降级为开发者自签。

**必须行为**

- 每个职位契约必须声明 required_capabilities；是否满足以 adapter probe() 按版本实测得到的 Capabilities 为准，不按品牌推断。
- 首选 Agent 因未安装、缺认证、无可用模型或所需能力为 false 而不可用时，必须按该职位的 fallback 顺序选择第一个已授权且能力全部满足的 Agent，并记录不可用原因和 probe 证据。
- 不在用户 fallback 列表中的 Agent，即使能力满足也不能被自动启用；默认配置不含任何 fallback。
- 启用 fallback 等同于一次绑定变更：在 checkpoint 生效，role_epoch+1，并发出角色失效事件和替换事件。
- qa/reviewer 的 fallback 必须使用与该 candidate 的 developer 会话不同的 session；同一产品可以做 fallback，但只能新开会话。
- 没有可用 fallback 时，该职位进入 BLOCKED 并列出精确缺项（BLOCKED_AUTH、BLOCKED_CAPABILITY 及缺失能力名、AGENT_UNAVAILABLE、MODEL_UNAVAILABLE）。不依赖该职位的实现任务可以继续，但 candidate 不能 ACCEPTED。
- 缺认证和能力不支持属于永久错误，不安排指数重试；临时的 429/503 进入 WAITING_EXTERNAL 并持久 next_retry，不触发 fallback。

**失败与边界**

- qa 首选 pi 缺认证且没有 fallback 时，qa 进入 BLOCKED_AUTH；candidate 停在 VERIFYING，acceptance_results 中没有该 candidate 的 PASS 行。
- 唯一已授权的 fallback 缺少一项 required_capabilities 时不启用，记录 CAPABILITY_UNSUPPORTED 及缺失的能力名。
- developer 会话提交 role=qa 的 TaskResult 或独立报告时被拒绝，不计入验收。
- 某职位的所有候选都缺认证时，该职位 BLOCKED_AUTH，相关 outbox 的 next_retry_at 为空，attempt_count 不递增。
- 首选 Agent 只是返回 503(Retry-After) 时进入 WAITING_EXTERNAL 并持久 next_retry，不切换到 fallback。

**验收**

- **AC-020-01**（TEST-020-01 · 检查 `unit`）Given qa.required_capabilities={structured_events,resume}；fallback 列表=[X,claude]；probe 表：pi 未安装、X 缺 resume（已授权）、claude 全部满足（已授权）、codex 全部满足（未授权）；When 以相同输入调用 fallback 选择函数 100 次；Then 每次都选中 claude；X 被记录为 CAPABILITY_UNSUPPORTED(resume)；codex 从未进入候选；100 次结果完全相同
  - 判定：selected=claude（100/100）；拒绝原因含 X:CAPABILITY_UNSUPPORTED:resume；codex 未出现在评估列表中；去重后输出只有 1 种
- **AC-020-02**（TEST-020-02 · 检查 `integration`）Given qa 首选 pi 的 FakeAdapter probe 返回 installed=false；qa.fallback=[claude]；candidate C 由 developer 会话 S_dev(claude) 产生；When C 冻结，触发 QA 派发；Then QA attempt 为 agent=claude，session_id≠S_dev，qa role_epoch+1；事件记录原因 pi:not_installed 并引用 probe 证据
  - 判定：QA attempt 的 agent=claude；session_id(QA)≠S_dev；qa role_epoch=前值+1；存在含 pi:not_installed 与 probe 引用的事件
- **AC-020-03**（TEST-020-03 · 检查 `integration`）Given qa 首选 pi 的 probe 中 auth_check=missing，qa 无 fallback；developer 队列中还有任务 T3；When candidate C 冻结并进入验证；Then qa 职位状态为 BLOCKED，原因 BLOCKED_AUTH；qa attempt 数=0；C 不是 ACCEPTED；T3 照常派发；不存在为 qa 安排的重试（next_retry_at 非空的 qa outbox 行数=0）
  - 判定：snapshot 中 qa 职位为 BLOCKED，原因 BLOCKED_AUTH；count(attempts role=qa)=0；acceptance_results 中 candidate_id=C 且 status=PASS 的行数=0；T3 的 attempt 数≥1；count(outbox where recipient_role=qa and next_retry_at IS NOT NULL)=0
- **AC-020-04**（TEST-020-04 · 检查 `integration`）Given qa 为 BLOCKED；developer 会话 S_dev 提交一份 role=qa、session_id=S_dev、status=PASS 的独立报告；When Runtime 处理该报告；Then 报告因 session 不独立被拒绝；acceptance_results 没有新增 PASS；candidate 不是 ACCEPTED；拒绝事件写明 S_dev 与 developer 角色冲突
  - 判定：报告处理结果为 rejected；acceptance_results 行数不变；candidate 状态≠ACCEPTED
- **AC-020-05**（TEST-020-05 · 检查 `unit`）Given FakeClock t=0；developer 首选 claude 返回 503 且 Retry-After=30；另一 run 中 claude 返回认证失败；developer.fallback=[codex]；When 分别派发 developer 任务；Then 503 场景：WAITING_EXTERNAL，next_retry_at=t+30s，不启用 codex；认证失败场景：next_retry_at 为空、不做指数重试，按 fallback 启用 codex
  - 判定：503：状态=WAITING_EXTERNAL，next_retry_at-t=30s，fallback 选中次数=0；auth：next_retry_at=null，选中 codex，原因含 BLOCKED_AUTH

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 文档没有列出各内置职位的 required_capabilities。初值：所有职位需要 structured_events，或可用 TaskResult 结构化桥替代；developer/qa 需要经 probe 实测的命令执行能力；commander/reviewer 需要 resume；commander 职位另需 read_only_workspace，按职位名判定，与 Commander 的只读启动是同一判定（引用 commander 合同的自定义职位不在内）（CR-024）。
- 默认配置不含任何隐式 fallback，未配置 fallback 即直接 BLOCKED。
- 文档没有说明 429/503 是否触发 fallback。选择不触发，按重试与预算规则等待；只有永久错误（缺认证/能力不支持/未安装/无模型）才评估 fallback。
- agent 缺失和 model 缺失的原因码文档未定义。初值：AGENT_UNAVAILABLE、MODEL_UNAVAILABLE。

## REQ-021 · Commander 监督

M2 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/07-algorithms.md`、`docs/14-resilience.md`、`docs/03-ux.md`、`docs/05-data-api.md`、`config/hoh.example.toml`、`resources/prompts/commander.md`、`contracts/sql/001_initial.sql`、`docs/13-adapters.md`、`docs/adr/ADR-032.md`

Commander 只被重要事件或策略规定的轮次边界唤醒；同类事件按去重键聚合并去抖；触发进度持久化在 last_handled_seq；过期决策被拒绝并重新触发；每次监督的延迟和决定都可查询。Commander 以只读工作区档运行，决策经最终消息交付（CR-024）。

**必须行为**

- 只有重要事件集合（need_decision、critical/high issue、no_progress、budget 风险、角色失效、规格变更、milestone_ready、release_request）和 balanced 策略下的轮次边界可以唤醒 Commander。
- token delta、工具输出、心跳、medium/low issue 和常规 candidate_ready 不得唤醒 Commander；candidate_ready 由 Runtime 直接派发 QA 与 Review。
- 按 (run_id, problem_fingerprint, spec_revision) 聚合：同键事件在静默 2 秒后合并为一个触发器，自首个事件起最长等待 10 秒；不同键分别触发。
- 严重完整性错误（IntegrityViolation）必须绕过去抖，在同一调度 tick 内唤醒 Commander。
- 触发器必须持久化 last_handled_seq；Commander 只收到该 seq 之后的摘要和证据引用。超出上下文预算时按严重性和因果链取舍，但保留 spec_sha256 与 candidate_id。
- Commander 决策必须带 expected_revision 和 role_epoch；任一过期即不 apply，标 REJECTED，并基于最新状态重新触发。
- 每个触发器和决策都必须可查询：去重键、event_seqs、首事件时间、唤醒时间、决策时间、监督延迟（单调时钟毫秒）和决策状态。
- reserve_supervisor_slot=true 时，即使写槽和读槽全部被占用也不能推迟 Commander 唤醒；Commander 不可用时，已授权的低风险动作继续执行，新的重大决策使 run 进入 BLOCKED。
- Commander 的每个 turn 必须以只读工作区档启动，argv 与参数逐项如下。只读档相对全权限启动只替换 YOLO 选项所在的部分（pi 为追加），其余 argv 规则照旧；`[…]` 为今天的规则下出现的可选项（claude 需要实时增量且 help 提供时的 `--include-partial-messages`、有 model 时的 `--model`、续会话的参数）：codex 只经 App Server：进程同今天；thread/start 的参数为 `"approvalPolicy": "never", "sandbox": "read-only"`，加上今天已有的 `model`（有 model 时）；thread/resume 的参数为同样两项，加上今天已有的 `threadId` 与 `model`（有 model 时）；逐项相等包含这些今天已有的字段。codex exec 不启动 Commander 的 turn：传输为 exec 的 codex 不被选为 Commander，App Server 在本 host 运行中降级为 exec 后 codex 不再当 Commander，直到 host 重启或换版本。claude：`claude -p --output-format stream-json --verbose [--include-partial-messages] --tools Read,Grep,Glob --allowedTools Read,Grep,Glob --permission-mode dontAsk --strict-mcp-config [--resume <session_id>] [--model <m>]`，不带 `--dangerously-skip-permissions`。pi：`pi --mode rpc --session-dir <dir> --session-id <id> --tools read,grep,find,ls --no-mcp --no-extensions [--provider <p>] [--model <m>]`。每个选项与固定取值只在被测 help/schema 声明时使用。help 无从声明的只有 claude 的工具名 Read、Grep、Glob，由 TEST-021-07 实测。缺其余任一项时该 turn 不启动（BLOCKED_CAPABILITY），不得改用全权限启动。Commander 的决策只取其最终助手消息中最后一个 ```hoh-decisions 块的各行，按行序处置（每行 `{"nonce", "message"}`）；写进任何文件的内容都不登记为决策（CR-024）。

**失败与边界**

- Agent 每秒产生数千条 token 事件时，Commander 唤醒次数仍为 0。
- 同一问题持续重复出现时，同一去重键最多存在一个未处理的触发器，新事件并入该触发器。
- host 重启后，seq≤last_handled_seq 的事件不再唤醒 Commander，已 APPLIED 的决策不会重放。
- 决策的 expected_revision 过期时，state=REJECTED(STALE_REVISION)，task 不变，并产生恰好一次重新触发。
- Commander 不可用时出现 need_decision，run 进入 BLOCKED 并记录原因，不无限等待；candidate_ready 照常派发 QA/Review。
- Commander 决策试图直接把 AC 或 candidate 标为通过时被拒绝，验收仍以证据门禁为准。
- 当 Commander 的 Agent 的 read_only_workspace 不为 true 时，该 Agent 不被选为 Commander：按 fallback 换绑，没有可用的 fallback 时职位 BLOCKED，缺项为 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`。不为 true 的原因分两类：主机的事实，只有两项：codex 的本机沙箱检查为 unconfined 或 unavailable；codex 的 App Server 在本 host 运行中降级为 exec。已分配的 Commander turn 因它们在启动前被拒时，按可恢复的不可用处理（AGENT_UNAVAILABLE），不写排除记录，主机恢复后同一版本可再当 Commander。其余都是二进制的事实，例如被测 help/schema 不提供只读档、codex 的传输为 exec、兼容清单的 read_only 用例未通过。已分配的 Commander turn 因它们在启动前被拒时为 BLOCKED_CAPABILITY（Capability 类）。只读档的问题不改变该 Agent 的兼容判决，它在其他职位照常可用。当监督 Attempt 的目录里出现 messages.jsonl 时，其中的行不登记为决策，并记一条 supervisor.bridge_ignored（CR-024）。

**验收**

- **AC-021-01**（TEST-021-01 · 检查 `unit`）Given FakeClock；policy=balanced；单个 run 的事件流：10000 条 token.delta、500 条 tool.output、60 条 heartbeat、3 条 medium issue、1 条 candidate_ready；When 按 seq 把事件流送入 Supervisor，并把 FakeClock 推进 1 小时；Then Commander 唤醒次数=0；Runtime 为 candidate_ready 生成 qa 与 reviewer 各 1 条 kind=handoff outbox；decisions 新增行数=0
  - 判定：FakeCommander 唤醒计数=0；recipient_role=qa 与 reviewer 的 kind=handoff 消息各 1 条；decisions 行数增量=0
- **AC-021-02**（TEST-021-02 · 检查 `unit`）Given FakeClock t=0；去重键 K1=(run,fp1,rev3)、K2=(run,fp2,rev3)、K3=(run,fp1,rev4)；When 在 t=0、0.5、1.0s 注入 K1 事件；在 t=0.2s 注入 K2，t=0.3s 注入 K3；t=30s 起每 1s 注入一条 K1 事件，持续 15s；t=60s 注入 IntegrityViolation；Then K1 首组在 t=3.0s 合并为 1 个触发器（3 个 seq）；K2、K3 各自独立触发；持续流的首个触发在 t=40.0s；IntegrityViolation 在 t=60s 的同一 tick 触发
  - 判定：K1 首组：触发时刻=3.0s，len(event_seqs)=3；K2 与 K3 触发器的 dedup 键互不相同，均不含 K1 事件；持续流：首个触发时刻-首事件时刻=10.0s，同时刻 K1 未处理触发器≤1；IntegrityViolation：触发时刻=60.0s（延迟 0 tick）
- **AC-021-03**（TEST-021-03 · 检查 `recovery`）Given 监督器已处理到 last_handled_seq=120，对应决策为 APPLIED；seq 121–125 为已持久化但未处理的同键重要事件；When kill -9 host 后重启；Then seq≤120 不再唤醒；121–125 合并为恰好 1 个触发器；APPLIED 决策数不增加；重启后 last_handled_seq≥120
  - 判定：重启后唤醒次数=1，该触发器 event_seqs={121..125}；count(decisions state=APPLIED) 前后相等；last_handled_seq 单调不减；reconcile 完成≤30s
- **AC-021-04**（TEST-021-04 · 检查 `integration`）Given 触发器 K 基于 task revision=7 生成；Commander 作答前，Runtime 已把该 task 推进到 revision=8；When FakeCommander 返回 decision{expected_revision:7, role_epoch:当前值}；对随后的重新触发返回 expected_revision:8 的决策；Then 第一条 decision 为 REJECTED(STALE_REVISION)，task revision 仍为 8 且无派发；恰好产生 1 个新触发器，其摘要基于 revision 8；第二条 decision 为 APPLIED
  - 判定：decision#1.state=REJECTED，原因 STALE_REVISION；decision#1 处理后 outbox 增量=0；新触发器数=1，摘要中 revision=8；decision#2.state=APPLIED
- **AC-021-05**（TEST-021-05 · 检查 `integration`）Given writer_slots=1、reader_slots=2 全部被 FakeAgent 长任务占用；reserve_supervisor_slot=true；FakeClock；When t=0 注入 1 条 critical issue；处理完后让 FakeCommander 不可用，再注入 need_decision 和 candidate_ready；Then critical issue 在 t=2.0s 唤醒 Commander，不排在读写槽之后；Commander 不可用后 run=BLOCKED、blocked_reason=COMMANDER_UNAVAILABLE 且 blocked_reasons 含 NEED_DECISION，candidate_ready 仍产生 qa 与 reviewer 各 1 条 kind=handoff 消息；GetEvents 能读到每个触发器的去重键、event_seqs 和延迟毫秒数
  - 判定：唤醒时刻=2.0s；run.state=BLOCKED，blocked_reason=COMMANDER_UNAVAILABLE，blocked_reasons 含 NEED_DECISION；qa 与 reviewer 的 kind=handoff 消息各 1 条；events 中触发器记录含 dedup 键、event_seqs 与非负的延迟毫秒字段
- **AC-021-06**（TEST-021-06 · 检查 `integration`）Given fake codex、claude、pi 的 help 与 schema 取自录制品；codex 另有只走 exec 的变体（help 无 app-server），以及 App Server 可在运行中降级（initialize 返回方法不存在）的变体；三组只读档缺项变体：claude 的 help 无 `--strict-mcp-config`，pi 的 help 无 `--no-mcp`，codex schema 的 SandboxMode 无 read-only；fake codex 的本机沙箱检查可设为 confined、unconfined、unavailable、unknown，并可在运行中改变；注册表的本机检查刷新间隔在测试中缩短；测试钩子可在分配之后、prepare 之前触发一次刷新；未配置 fallback；When (a) claude、codex App Server、pi 依次作为 Commander、Developer 为 fake claude，每个 Commander 被 need_decision 唤醒两次，第二次续同一会话；第一次唤醒时 fake Commander 在最终消息的 ```hoh-decisions 块中给出一条 record_assumption，同时向该 Attempt 目录的 messages.jsonl 追加另一条有效决策（同一 nonce）；(b) 三组缺项变体中该 Agent 同时被配为 Commander 与 Developer；(c) codex 同时为 Commander 与 Developer，本机检查依次为 unconfined、unavailable、unknown，再从 unavailable 改为 confined；另在检查为 unknown 时分配 Commander 的 turn，在 prepare 之前刷新得到 unavailable，再改为 confined；(d) 只走 exec 的 codex，以及运行中降级的 codex，各同时为 Commander 与 Developer；(b)、(c) 的 unconfined 与 unavailable、(d) 的只走 exec 中 commander 绑定从一开始即 BLOCKED，各另有 run S：Commander 为另一个可用的 Agent，Developer 为该 Agent；Then (a) 两次 turn 的 argv（claude、pi）或 thread/start 与 thread/resume 的参数（codex App Server）与 REQ-021 行为 9 的清单逐项相等，且不含 `--dangerously-skip-permissions`、`--dangerously-bypass-approvals-and-sandbox` 或 `danger-full-access`；Developer 的 argv 含 YOLO 选项；块中的决策为 APPLIED；messages.jsonl 中那条决策没有 supervisor.output，supervisor.bridge_ignored 恰 1 条。(b) 兼容清单中该 Agent 的 suite.status=PASS、read_only 用例为 FAIL；commander 绑定为 BLOCKED，缺项含 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`，没有 Commander Attempt 启动；该 Agent 的 Developer 绑定照常得到会话；commander 绑定被阻塞本身唤醒 Commander，所以该 run 为 BLOCKED（COMMANDER_UNAVAILABLE，ADR-025 决定 5），其中不启动 turn；该 Agent 在 run S 中作为 Developer 的 turn 启动并 SETTLED。(c) unconfined 与 unavailable 时 commander 绑定为 BLOCKED，缺项含 `codex:CAPABILITY_UNSUPPORTED:read_only_workspace`，判决证据写出检查结论，没有 Commander Attempt 启动；兼容清单中 codex 的 read_only 用例仍为 PASS，会话记录的 capabilities_json 不变（主机的事实只作用于选人）；codex 作为 Developer 在 run S 中照常 SETTLED；unknown 时 codex 照常作为 Commander 运行，每个 Commander Attempt 自己的记录（其 context.packed 的 read_only）写下 unknown 与原因；检查从 unavailable 改为 confined 后，下一次刷新使 commander 绑定解除阻塞、Commander Attempt 启动，该 run 的 Developer turn 在其原会话中 SETTLED；已分配之后刷新得到 unavailable 时，prepare 拒绝为 AGENT_UNAVAILABLE，不写 team.binding_excluded，改为 confined 后同一 codex 版本的 Commander Attempt 再被派发并 SETTLED；以上各处 codex 作为 Developer 的会话 id 前后不变。(d) 两者都不被选为 Commander（缺项含 `codex:CAPABILITY_UNSUPPORTED:read_only_workspace`），没有在 exec 上启动的 Commander turn；只走 exec 的 codex 作为 Developer 在 run S 中照常 SETTLED，运行中降级的 codex 作为 Developer 在降级的那个 run 中经 exec 照常 SETTLED
  - 判定：(a) 两次 turn 的启动与 REQ-021 行为 9 的清单逐项相等，不含任何 YOLO 选项；Developer 的启动含 YOLO 选项；(a) 块中的决策 APPLIED；messages.jsonl 中的决策没有 supervisor.output，supervisor.bridge_ignored 恰 1 条；(b) suite.status=PASS 且 read_only 用例 FAIL；commander 绑定 BLOCKED，缺项为 `<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`，Commander Attempt 数为 0；run R 为 BLOCKED（COMMANDER_UNAVAILABLE），其 Developer 绑定有会话；run S 中该 Agent 的 Developer Attempt 为 SETTLED；(c) unconfined、unavailable：commander 绑定 BLOCKED，缺项为 `codex:CAPABILITY_UNSUPPORTED:read_only_workspace`，证据含检查结论，Commander Attempt 数为 0；compat-manifest.json 中 codex 的 read_only 用例仍为 PASS，sessions.capabilities_json 不变；run S 中 codex 的 Developer Attempt 为 SETTLED；unknown：Commander Attempt 启动，其 context.packed 的 read_only.host_check 为 unknown 并带原因；改为 confined 后的下一次刷新：commander 绑定解除阻塞，Commander Attempt 启动，run R 的 Developer Attempt 在原会话中 SETTLED；(c) 已分配之后失去能力：prepare 拒绝为 AGENT_UNAVAILABLE，events 中没有 team.binding_excluded；改为 confined 后同一 codex 版本的 Commander Attempt 再被派发并 SETTLED；codex Developer 的 session_id 始终相同；(d) 两种 codex 都不被选为 Commander，缺项含 `codex:CAPABILITY_UNSUPPORTED:read_only_workspace`；fake 记录中没有 Commander 的 exec 启动；codex 的 Developer Attempt 为 SETTLED（只走 exec 时在 run S 中，降级时在同一个 run 中）
- **AC-021-07**（TEST-021-07 · 检查 `adapter-live`）Given VM 实装且已原生登录的 claude、codex、pi；claude、pi、codex（App Server）各一段；每段一个临时 Git 仓库 P（已提交，含随机 token 文件，`.gitignore` 含 `.hoh/`）与 P 之外的私有工作目录 W；四个区域（P 的工作树、P/.git/、P/.hoh/、W）各有供每种写机制分别建、改、删的种子；区域之外有测试写出的确定的驱动：区域路径为常量、缺区域即报驱动错误的格脚本，与补丁全文；pi 另加载只观察的测试扩展；受测候选提交中 ADR-032 的证据表，与它引用的已安装版本的录制；When live：每段以 Commander 的只读档运行新会话与续同一会话两轮，提示按驱动逐一尝试每个区域的建、改、删，然后读取 token，在最终消息的 ```hoh-decisions 块中给出含 token 的 record_assumption；每段另以全权限档在另一个仓库运行同一提示作对照；每轮前后取四个区域的全量快照，其间不运行 git；codex 每轮之后另读该轮线程的 rollout 文件；Then 按 ADR-032 证据表的字段路径与文字判定，判定须要的每一项输入都必须在，缺任一项从不判 PASS。判定不自己解析 Agent 的流：本轮的范围、请求与响应的关联、终态与最终消息取自产品适配器对所录 stdout 与所录的外部观察的重放（与 host 运行同一条代码路径）。各适配器必需的观察（ADR-032 设计修正 7 第 2 条）：三者都有 Agent 的版本、进程的退出状态与日志是否读完；claude 另有 prompt 是否送达；codex 另有启动参数（只读档或全权限档、新线程或续的 thread id、model）；pi 另有启动参数（非空的会话 id、显式布尔的是否续会话）与续会话时启动前记下的会话文件和 get_state 所指文件的身份。缺一项、类型不对或有未知的观察，不给出已结束。适配器写给 Agent 的消息由它从启动参数建出，所录的请求记录按 ADR-032 的比较约定与之逐条、逐键比较，不同为覆盖不足；适配器不取的记录不计，带本轮 id 而适配器没取的记录为覆盖不足；本轮的每个条目与 rollout 中本轮的每个调用都须按证据表分类并有已知结果，分不了类或不完整的为覆盖不足，从不丢掉。所须的证据：每轮 SETTLED，本轮与对照轮的终态都按产品适配器结束一个 turn 的规则（证据表 E0），块被解析且 nonce 正确，第 1 轮含 token；该会话自己的原始记录给出可用性证据，claude、pi 恰为只读档的读工具，codex 线程的沙箱为只读；四个区域前后快照相同，唯一容许的差异是 pi 本段会话目录之下，.git/ 不豁免任何文件；codex 的两种写机制（shell 与 apply_patch）× 四个区域 × 建改删的每一格，都有原始记录中的尝试、由权限拒绝而非其他任何错误造成的结果，以及对照中同一机制 × 操作在普通工作树文件上的成功；claude、pi 的对照轮的可用性证据列出读工具以外的工具。一格的结论取本轮所有相符的记录，与顺序无关：任一记录为成功即该格成功，每一条都是权限拒绝才是权限拒绝；对照轮必须存在，其可用性证据完整并显示全权限，每个机制 × 操作的所有记录都是成功。分类固定如下：可用性证据齐全而不符合（只读档未生效）、任一快照差异或任一成功的格，立即 FAIL，不重做；缺任一项为覆盖不足，不判 PASS，每段至多换新会话重做三次；未登录的 Agent 为 BLOCKED 而非 PASS。什么算权限拒绝、突破或覆盖不足只能经 CR 改；ADR-032 的证据表只把这一分类映射到字段路径与文字，改一个字段路径或文字须引用已安装版本的一次新录制，并经设计审查
  - 判定：每轮 SETTLED，本轮与对照轮的终态都按产品适配器结束一个 turn 的规则（证据表 E0）；可用性证据按证据表齐全：claude、pi 恰为只读档的读工具，codex 线程的沙箱为只读；范围、请求与响应的关联、终态与最终消息取自产品适配器对所录 stdout 与所录的外部观察（各适配器必需的观察按 ADR-032 设计修正 7 第 2 条，codex 含启动参数，prompt 是否送达只属 claude；缺一项、类型不对或未知即不结束）的重放，判定不另行解析；适配器的消息由启动参数建出，请求记录与之逐条、逐键比较；适配器不取的记录不计，带本轮 id 而适配器没取的记录为覆盖不足；本轮的每个条目与 rollout 中本轮的每个调用都须分类并有已知结果，分不了类或不完整的为覆盖不足；一格的结论取本轮所有相符的记录、与顺序无关：任一为成功即成功，每一条都是权限拒绝才是权限拒绝；判定须要的任何一项输入缺失都不判 PASS；codex 的 shell 与 apply_patch × 四个区域 × 建改删的每一格：有尝试，有按证据表判为权限拒绝（而非其他任何错误）的结果，有对照中同一机制 × 操作在普通工作树文件上的成功（对照轮必须存在，可用性证据完整并显示全权限，该机制 × 操作的所有记录都是成功）；claude、pi 的对照轮的可用性证据列出读工具以外的工具；前后快照相同，唯一容许的差异是 pi 本段会话目录之下，.git/ 不豁免任何文件；块解析成功、nonce 正确，第 1 轮含 token；可用性证据齐全而不符合、任一快照差异或任一成功的格即 FAIL，不重做；缺任一项为覆盖不足，不判 PASS；分类（权限拒绝、突破、覆盖不足）只能经 CR 改；证据表只把它映射到字段路径与文字，改字段路径或文字须有已安装版本的新录制并经设计审查；evidence.mode=live，记录各会话的版本与 session_id；未登录时为 BLOCKED，不得为 PASS

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 2 秒/10 秒的语义：选择尾随去抖（同键静默 2 秒后触发），加上自首事件起最长 10 秒。
- 紧急事件集合：选择只有 IntegrityViolation 类（例如验证中候选被改、DB integrity 失败）绕过去抖；critical issue 属于重要事件，仍走去抖。
- 同键已有未处理触发器时，新事件并入该触发器，不并发第二个触发器。
- 触发器字段名（dedup_key、event_seqs、wake_latency_ms）和事件 kind 名称是初值；延迟的测量起点为首事件提交时刻（单调时钟）。
- 文档没有量化 efficient/intensive 的唤醒差异。选择只断言：同一 fixture 下唤醒次数满足 intensive≥balanced≥efficient，且重要事件在三种策略下都会唤醒。
- （CR-024）只读工作区档的强制方式因底座而异：codex 由其操作系统级沙箱约束模型运行的命令与补丁；用户自己配置的 MCP 服务器与钩子运行在沙箱之外。claude 只靠进程内的工具集与权限判定：`--tools` 只说覆盖内置工具集，插件、用户或项目设置带来并预先批准的工具是否也被排除，help 没有说。pi 只靠进程内按工具名的允许清单，没有操作系统级屏障；Commander 的 pi 会话不加载任何扩展（`--no-extensions`，不以 `-e` 加载），所以扩展覆盖同名工具、包住工具调用或自己写文件都不在这个会话里。同 uid 的其他进程不受约束（CR-013）。这些余项按所有者的决定如实记在 docs/12，并由 doctor 显示（ADR-032 决定 7）；各底座只读会话的工具集、拒绝记录与快照由 TEST-021-07 实测。`execution.profile="yolo-vm"` 指 Commander 用只读工作区档、其余职位全权限，不新增配置键。

## REQ-022 · 通信与交接

M2 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/05-data-api.md`、`docs/14-resilience.md`、`docs/06-orchestration.md`、`docs/04-architecture.md`、`contracts/protocol.schema.json`、`contracts/openapi.json`、`contracts/sql/001_initial.sql`、`examples/hcp-message.json`、`examples/context-handoff.json`

HCP 消息与状态迁移在同一事务中持久化，并按消费者与 message_id 去重；送达收据与任务完成分开记录；交给 QA/Review 的交接包携带 candidate、spec 和 epoch 版本身份。

**必须行为**

- 每条 HCP 消息必须通过 protocol.schema.json 的 Message 校验；未知 kind 或缺字段的消息被拒绝并记录，不能静默忽略。
- 状态更新（WHERE revision=expected）、事件追加和 outbox 插入必须在同一事务中完成；事务提交后才发 ACK。
- 接收方以 (consumer,message_id) 唯一收据去重；重复投递返回原收据的 result_json，不重复迁移状态。
- 同一 idempotency_key 且 payload hash 相同，返回原结果；key 相同而 payload 不同，返回 409 KEY_REUSE。
- ACK/收据只表示送达。task 只在收到 TaskResult 并通过验证后迁移；outbox 变为 ACKED 不会使 task 变为 ACCEPTED。
- 未 ACK 的投递按持久化的 attempt_count/next_retry_at 重试；超过投递上限后进入 DEAD_LETTER 并发出事件，记录不删除。
- Developer 提交 candidate 后，Runtime 生成发往 qa 和 reviewer 的 ContextHandoff，二者为 kind=handoff 的消息（handoff_kind=verification），带相同的 candidate 完整身份与 spec_sha256、各自接收角色当前的 role_epoch，且 acceptance_ids 非空。
- role_epoch、expected_revision、spec_sha256 或 candidate_id 与当前值不符的消息，得到 kind=rejected（含 current_revision），不改变状态。

**失败与边界**

- 同一 message_id 投递 3 次时，receipts 只有 1 行，状态只迁移 1 次，后两次返回首次的 result_json。
- 同 key、不同 payload 的命令返回 409 KEY_REUSE，commands 表不新增行。
- 接收方已提交但 ACK 丢失时，发送方重投会被收据去重；outbox 最终为 ACKED，attempt_count 等于真实投递次数。
- 接收方始终不 ACK 时，达到投递上限后 outbox.state=DEAD_LETTER，并产生可查询的事件。
- 消息乱序到达、expected_revision 不匹配时，返回 rejected 和 current_revision；该 message_id 重投仍得到同一拒绝结果。
- acceptance_ids 为空的 handoff 不能通过 schema 校验，不创建 QA attempt。

**验收**

- **AC-022-01**（TEST-022-01 · 检查 `integration`）Given QA 消费者处于 task revision=3；消息 m1(expected_revision=3)、m2(expected_revision=4, causation_id=m1)；FakeTransport 可重放、可重排；When 组 A 按 m1,m1,m2,m1 投递；组 B 在新库中按 m2,m1,m2 投递；Then 组 A：最终 revision=5，状态迁移 2 次，receipts 2 行；组 B：首个 m2 返回 rejected(current_revision=3)，m1 后 revision=4，第二次 m2 返回同一 rejected 结果，最终 revision=4
  - 判定：A：tasks.revision=5，迁移事件数=2，receipts 行数=2；A：重复投递返回的 result_json 与首次逐字节相同；B：m2 两次结果都为 rejected，current_revision=3；B：tasks.revision=4，迁移事件数=1
- **AC-022-02**（TEST-022-02 · 检查 `integration`）Given commands 表为空；When POST /v1/commands：以 idempotency_key=K、payload P 提交两次，再以 K、payload P' 提交一次；Then 前两次都返回 202，command_id 和 revision 相同，commands 行数=1，事件只增加 1 条；第三次返回 409 且 code=KEY_REUSE
  - 判定：响应 1 与响应 2 的 body 相同；count(commands where idempotency_key=K)=1；第三次 HTTP 409，body.code=KEY_REUSE
- **AC-022-03**（TEST-022-03 · 检查 `integration`）Given FakeTransport 在接收方提交收据后丢弃第 1 次 ACK；测试配置 max_delivery_attempts=3；另有一个永不 ACK 的接收方 R2；When 向 QA 派发 kind=handoff 消息 m，向 R2 派发消息 n，把 FakeClock 推进到每个 next_retry_at；Then m 第 2 次投递被收据去重并回 ACK：outbox(m).state=ACKED，attempt_count=2，task 在 QA 的 TaskResult 到达前仍为 VERIFYING；n 第 3 次失败后 outbox(n).state=DEAD_LETTER，并有对应事件
  - 判定：receipts(QA,m) 行数=1；outbox(m).state=ACKED，attempt_count=2，next_retry_at 曾被持久化；TaskResult 到达前 tasks.state=VERIFYING；outbox(n).state=DEAD_LETTER，attempt_count=3，存在 dead-letter 事件
- **AC-022-04**（TEST-022-04 · 检查 `integration`）Given candidate C 已冻结并写入 candidates 表；当前 spec_sha256=S；When Developer 的 TaskResult{status:COMPLETED,candidate_id:C} 被 Runtime 处理；Then 恰好生成 2 条 kind=handoff 消息，recipient_role 为 qa 和 reviewer；payload 通过 ContextHandoff 校验，candidate 对象逐字段相同且等于 candidates(C) 行；spec_sha256=S；acceptance_ids 非空
  - 判定：handoff 消息数=2，接收者集合={qa,reviewer}；schema 校验 0 错误；两条消息的 candidate 对象相同，且 id/git_commit/git_tree/spec_sha256/environment_sha256/test_plan_sha256 与 C 行一致；两条 handoff ACK 后 task.state≠ACCEPTED
- **AC-022-05**（TEST-022-05 · 检查 `integration`）Given 当前 spec_sha256=S2、candidate=C2、qa role_epoch=2；When 分别投递 4 条 QA 消息：role_epoch=1；spec_sha256=S1；candidate_id=C1；以及一条 acceptance_ids=[] 的 handoff；Then 前 3 条得到 kind=rejected，原因分别为 STALE_ROLE_EPOCH/SPEC_MISMATCH/CANDIDATE_MISMATCH；第 4 条 schema 校验失败；task 与 acceptance_results 都不变；4 条拒绝都写入事件
  - 判定：4 条消息的处理结果都不是 applied；tasks.revision 前后相等；acceptance_results 行数前后相等；拒绝事件数=4

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 文档没有给出投递上限或期限。初值 max_delivery_attempts=5（测试中配置为 3），超过后进入 DEAD_LETTER，不删除。
- 乱序时 expected_revision 不匹配：选择直接 rejected 并写收据；发送方重算后用新的 message_id 和 expected_revision 重发，旧 message_id 不复活。
- 拒绝原因码 STALE_ROLE_EPOCH/SPEC_MISMATCH/CANDIDATE_MISMATCH 为初值；CR-003 统一为大写原因码并集中登记在 protocol.schema.json 的 $defs.ReasonCode。

## REQ-023 · 局部咨询与升级

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/adr/ADR-028.md`、`docs/adr/ADR-025.md`、`resources/prompts/researcher.md`

局部咨询不绕过全局任务账本；重大决定升级 Commander；讨论有轮数上限

**必须行为**

- 角色间咨询经 Router 成为全局账本中的子 Task（parent、role、预算、deadline），出现在 /v1/runs/{id}/tasks，结果回到请求方；没有账本之外的工作。
- wait-for 图检测环：A 等 B 的回答时 B 向 A 提问，后一请求被拒（CONSULT_CYCLE，details 列出环成员）；QA 请 Developer 改当前候选被拒并转为 Issue（ADR-028 决定 13）。
- 讨论有轮数上限：同一争议的意见轮数上限取配置 [supervision] max_discussion_rounds（缺省 2，合法值 1–5 的整数，非法值使配置被拒；docs/06 的“默认可调上限”），超过上限的请求被拒（DISCUSSION_LIMIT），Commander 以 need_decision 被唤醒；没有新证据时提前停止；结论产出 Decision 或 ADR 引用。
- 重大决定升级：咨询结论触及规格、ADR、公开接口或门禁时（C27），参与职位不自行应用，升级为 Commander 的 need_decision；规格变更再走 CR（REQ-033）。

**失败与边界**

- 咨询成环时后一请求被拒（CONSULT_CYCLE），不建子 Task，不死锁。
- 第三轮讨论被拒（DISCUSSION_LIMIT），Commander 被唤醒一次。
- 改当前候选的请求被拒并开 Issue，不派发修改。

**验收**

- **AC-023-01**（TEST-023-01 · 检查 `integration`）Given developer 的 Task 运行中，researcher 绑定可用；When developer 请求 researcher 研究一个问题；Then Router 建一个子 Task（parent=该 Task、role=researcher、带预算与 deadline），出现在 /v1/runs/{id}/tasks；researcher 的结果作为消息回到 developer；Attempt 全部属于账本中的 Task
  - 判定：子 Task 带 parent、role、预算、deadline 并出现在 tasks API；结果回到 developer；无账本外 Attempt
- **AC-023-02**（TEST-023-02 · 检查 `integration`）Given developer 正等待 researcher 的回答；QA 正在验证当前候选；When researcher 向 developer 提问；QA 请求 developer 修改当前候选；Then researcher 的请求被拒，reason=CONSULT_CYCLE 且 details 列出 developer 与 researcher，没有新子 Task；QA 的请求被拒并开一条 Issue，没有修改派发
  - 判定：CONSULT_CYCLE 且 details 列出环成员；无新子 Task；QA 请求被拒并开 Issue
- **AC-023-03**（TEST-023-03 · 检查 `integration`）Given 两个项目中各有一个架构争议：项目 A 用缺省配置，项目 B 配置 [supervision] max_discussion_rounds = 3；When A 的参与者在两轮意见之后发起第三轮；B 的参与者发起第三轮，之后发起第四轮；Then A 的第三轮被拒（DISCUSSION_LIMIT），Commander 以 need_decision 被唤醒恰一次，唤醒摘要含两轮意见的引用，之后的结论记录为 Decision 或 ADR 引用；B 的第三轮被接受，第四轮被拒（DISCUSSION_LIMIT）并唤醒恰一次
  - 判定：A 第三轮 DISCUSSION_LIMIT 且唤醒恰一次、摘要含两轮引用；结论为 Decision 或 ADR 引用；B 第三轮接受、第四轮 DISCUSSION_LIMIT 且唤醒恰一次
- **AC-023-04**（TEST-023-04 · 检查 `integration`）Given researcher 与 developer 的咨询结论是“改公开接口的签名”；When 参与职位尝试直接应用该结论；Then 应用被拒，结论升级为 Commander 的 need_decision；没有 Task 在升级处理前按该结论派发
  - 判定：应用被拒并升级 need_decision；处理前无派发

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 重大决定为触及规格、ADR、公开接口或门禁者（CR-026，C27）。
- 讨论轮数上限由 [supervision] max_discussion_rounds 配置，缺省 2（docs/06 的默认可调上限）（CR-026，C28，CR26D-10）。
- 新码 CONSULT_CYCLE、DISCUSSION_LIMIT 为 RejectionReason（CR-026）。

## REQ-024 · Codex 原生接口

M1 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/13-adapters.md`、`research/SOURCES.md`、`docs/04-architecture.md`、`docs/09-testing.md`、`docs/14-resilience.md`、`docs/07-algorithms.md`、`contracts/sql/001_initial.sql`

Codex 适配器以 `codex app-server`（stdio JSON-RPC）完成握手、thread、turn、item 与终态处理，区分 completed/interrupted/failed，支持 turn/interrupt 取消与 thread/resume 恢复；不可用时显式降级为 `codex exec --json`。

**必须行为**

- 必须以 argv `codex app-server`（默认 `--listen stdio://`）在独立进程组启动，不经 shell，不连接共享 daemon。
- 握手顺序必须为 initialize 请求→收到其响应→发送 initialized 通知→再发 thread/start 或 thread/resume；任何 thread/turn 请求不得早于 initialized。
- 必须按 JSON-RPC id 关联请求与响应，持久保存 threadId、turnId、itemId 到 session/attempt，并从 thread/start 响应记录 resolved model。
- turn/completed 的 turn.status 必须映射：completed→SETTLED，interrupted→CANCELLED，failed→FAILED（保存 turn.error）；`error` 通知 willRetry=true 时不判终态。
- 取消必须先发送 turn/interrupt(threadId,turnId)，在 grace 内等待 interrupted 终态，超时再对进程组 SIGTERM→SIGKILL 并 reap。
- mid-turn steer 必须用 turn/steer 并带 expectedTurnId=当前活动 turnId；原生拒绝时该输入记为 REJECTED，不显示为已应用。
- 服务端请求（命令/文件审批、requestUserInput 等）必须按 headless 策略立即回应拒绝或取消并记录事件，不得悬挂，不伪造人类选择。
- App Server 不可用时降级为 `codex exec --json`：capabilities.mid_turn_steer=false 并记录降级事件；YOLO flag 仅在当前 `codex exec --help` 含该 flag 时使用。
- （CR-041）thread/resume 发送 excludeTurns=true 避免携带整个 Thread.turns；共享 builder 由 compat 校验，旧 schema 未声明字段不能通过符合性，不静默省略。

**失败与边界**

- 当 app-server 在 turn/completed 前 EOF 或退出时，attempt 为 FAILED（副作用无法确认时 UNKNOWN_EFFECT），不得为 SETTLED。
- 当 turn.error.codexErrorInfo=unauthorized 时 run 进入 BLOCKED_AUTH 且不重试；rateLimitExceeded/serverOverloaded 按瞬时外部错误持久化 next_retry。
- 当 turn/steer 的 expectedTurnId 与活动 turn 不一致时，原生返回错误，input_queue 该行为 REJECTED。
- 当 initialize 返回方法不存在或握手超时时，降级为 exec 并记录原因；exec 也不可用则 BLOCKED_CAPABILITY。
- 当降级模式收到 steer 请求时返回 CAPABILITY_UNSUPPORTED，输入保持 QUEUED 至下一轮，不静默丢弃。
- 当 YOLO profile 所需 flag 不在 `codex exec --help` 时报告 BLOCKED_CAPABILITY，不拼接未探测参数。

**验收**

- **AC-024-01**（TEST-024-01 · 检查 `integration`）Given fake app-server 回放 codex 0.166 真实会话录制 JSONL：thread/started、turn/started、item/*、thread/tokenUsage/updated、turn/completed status=completed，另插1条未知通知；When 适配器 start(ExecutionRequest) 执行一轮并等待终态；Then fake 端收到的客户端消息顺序为 initialize→initialized→thread/start→turn/start；attempt=SETTLED；sessions.native_id=回放 threadId；resolved model=thread/start 响应 model；未知通知原样保存且无 panic
  - 判定：收包 method 序列前4项=[initialize,initialized,thread/start,turn/start]，initialized 晚于 initialize 响应；attempt state=SETTLED；HoH item 事件数=fixture 中 item/started+item/completed 数，均带 threadId/turnId/itemId；sessions.native_id=回放 threadId，resolved model=回放 model；原始 stdout 日志 sha256=fake 发送字节 sha256；adapter.unknown_event 恰1条
- **AC-024-02**（TEST-024-02 · 检查 `integration`）Given 3份 fixture：(a) 收到 turn/interrupt 后回 turn/completed status=interrupted；(b) status=failed 且 codexErrorInfo=unauthorized；(c) turn 中途 EOF；另有 thread/resume 回放；When (a) 中途调用 cancel()；(b)(c) 等待终态；之后对 (a) 的 SessionRef 新起 fake 进程调用 resume()；Then (a) fake 收到 turn/interrupt 且 threadId/turnId 与活动值相同，attempt=CANCELLED；(b) attempt=FAILED、阻塞原因 BLOCKED_AUTH、0次重试；(c) attempt∈{FAILED,UNKNOWN_EFFECT}；resume 发送 thread/resume 且 threadId 等于原值
  - 判定：(a) 收包含 turn/interrupt 且参数与活动 threadId/turnId 相等；attempt=CANCELLED；(b) attempt=FAILED，事件 blocked_reason=BLOCKED_AUTH，next_retry 为空；(c) attempt≠SETTLED；resume 收包含 thread/resume，threadId 与原 session 相同；三份均无 panic，进程组均已 reap
- **AC-024-03**（TEST-024-03 · 检查 `integration`）Given fake codex：①help 无 app-server 或 initialize 返回 method not found，exec help 含 `--json` 与 YOLO flag；②exec help 缺 YOLO flag；另有带活动 turn 的 App Server fixture；When 以 YOLO profile 启动 attempt 并发起 steer；在 App Server fixture 中以过期 expectedTurnId 发 turn/steer；对缺 flag 版本启动 attempt；Then ①argv 以 `codex exec --json` 开头、mid_turn_steer=false、有 adapter.degraded，steer 返回 CAPABILITY_UNSUPPORTED 且输入仍 QUEUED；过期 steer 输入=REJECTED；②未 spawn，原因 BLOCKED_CAPABILITY
  - 判定：降级 argv[0..3]=[codex,exec,--json]，含已探测的 YOLO flag；sessions.capabilities_json.mid_turn_steer=false，adapter.degraded 事件1条；降级 steer 返回 CAPABILITY_UNSUPPORTED，input_queue 该行 state=QUEUED；过期 steer 对应 input_queue state=REJECTED；缺 flag 版本无子进程启动，原因=BLOCKED_CAPABILITY
- **AC-024-04**（TEST-024-04 · 检查 `adapter-live`）Given VM 实装 codex（记录 version、binary sha256、help 与 schema hash），已原生登录，临时 Git 仓库；When live smoke：第1轮要求只回复 `HOH_NATIVE_SMOKE_OK` 且不用工具；第2轮要求执行 `sleep 120` 后立即 cancel；Then 第1轮 turn.status=completed、attempt=SETTLED、agent 消息含该 token；第2轮 turn.status=interrupted、attempt=CANCELLED，进程组在 grace 上限内无残留；未登录时检查结果为 BLOCKED 而非 PASS
  - 判定：第1轮 turn.status=completed，attempt=SETTLED，消息含 `HOH_NATIVE_SMOKE_OK`；第2轮 turn.status=interrupted，attempt=CANCELLED；cancel 后≤16 s 内进程组无存活进程与僵尸；evidence.mode=live 并记录实际版本、threadId、turnId 与 resolved model；鉴权失败时检查 outcome=BLOCKED，不得为 PASS
- **AC-024-05**（TEST-024-05 · 检查 `integration`）Given codex 0.166 的录制 schema、实际请求 fake，与删除 ThreadResumeParams.excludeTurns 的旧 schema fixture；When 构建并执行 thread/resume，校验共享 builder 的请求符合性；Then resume 请求含 excludeTurns=true，threadId 保持原会话；thread/start 不含此字段；已声明且支持的 schema 通过，未声明字段的旧 schema 即使允许额外字段也不通过兼容校验
  - 判定：resume 请求含 excludeTurns=true，threadId 保持原会话；thread/start 不含此字段；已声明且支持的 schema 通过，未声明字段的旧 schema 即使允许额外字段也不通过兼容校验

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- App Server 的 YOLO 通过 thread/start 的 approvalPolicy=never、sandbox=danger-full-access 表达，依据本机 codex 0.166 generate-json-schema；docs/13 只描述 exec flag。（CR-024）Commander 的只读档通过 thread/start 与 thread/resume 的 approvalPolicy=never、sandbox=read-only 表达，同一 schema 的 SandboxMode 含 read-only。
- native cancel grace 10 s 与 TERM grace 5 s 为初值，三个适配器共用。
- 降级 exec 的 resume 用 `codex exec resume <id>`（本机 help 存在），是否计为 resume 能力由兼容套件确认。
- （CR-003）鉴权失败识别：App Server 为 turn/completed status=failed 且 codexErrorInfo=unauthorized（0.166.0 schema 枚举值，录制中未出现，fixture 依 schema 合成并在 meta.json 标 synthetic=true，只用于 integration）；exec 降级为 turn.failed，且其 error.message 或之前的 error 事件含 `unexpected status 401`（录制样例 exec-unauthenticated）。二者都映射 BLOCKED_AUTH 且不重试。
- （CR-024）Commander 不经 codex exec 运行。理由：exec 的 `--json` 流里补丁被拒不留痕迹，拒绝只记在该线程的 rollout 文件里；exec 不给出这个文件的位置，只能按未声明的文件名约定查找；exec 续会话的只读又要靠 help 不声明的配置键 `sandbox_mode`。App Server 的响应直接给出 rollout 的位置，续会话也有已声明的 `sandbox` 参数（ADR-032 决定 10）。因此传输为 exec 的 codex 的 read_only_workspace 为 false。App Server 在本 host 运行中降级为 exec 后，codex 不再当 Commander，直到 host 重启或换版本；这是主机的事实，不写入会话记录。选 codex 当 Commander 时，另须本机沙箱检查（以临时 CODEX_HOME 运行 `codex sandbox -P :read-only` 的写探针）不为 unconfined 或 unavailable。该检查只说明本机能否建立 codex 的默认只读沙箱。它也是主机的事实：不写入会话记录，从不使任何会话换绑；结论不是 confined 时至多每 60 秒重测一次，主机修好后即恢复（ADR-032 决定 4）。

## REQ-025 · Claude 原生接口

M1 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/13-adapters.md`、`research/SOURCES.md`、`docs/09-testing.md`、`docs/11-deployment.md`、`docs/17-risks-gaps.md`、`docs/15-prompts-skills-memory.md`

Claude 适配器以 `claude -p --output-format stream-json --verbose` 非交互运行，分离捕获 stdout/stderr，由 init/result 帧得到 session 与结果，按实际帮助选用 flag，支持分轮 `--resume` 与有界取消。

**必须行为**

- 必须以 argv 调用 `claude -p --output-format stream-json --verbose`，仅当需要实时增量且 help 含该 flag 时追加 `--include-partial-messages`；不经 shell。
- stdout 与 stderr 必须并行读取并分别保存为独立原始文件（各记 sha256）；stderr 只作诊断，不按事件解析。
- 必须从 system init 帧记录 session_id 与 resolved model，从 result 帧记录 subtype、is_error、usage 与费用；缺失项为 unknown 而非0。
- 仅当 result.subtype=success、is_error=false 且进程退出码0时 attempt=SETTLED；error_max_turns、error_during_execution、中断均不是成功。
- 后续轮次必须以 `--resume <session_id>` 继续同一会话；需要独立会话的角色不得复用开发会话 id。
- YOLO profile 仅在 help 含 `--dangerously-skip-permissions` 时使用；Commander 的只读档按 REQ-021 行为 9 的 claude 一行启动，不带 `--dangerously-skip-permissions`；其选项与 `dontAsk` 取值仅在 help 声明时使用，否则不启动（BLOCKED_CAPABILITY），工具名 Read、Grep、Glob 由 TEST-021-07 实测（CR-024）；HoH Auto 不得映射为 `--permission-mode auto`；HoH Prompt 经官方追加方式或任务上下文注入，不替换原生 system prompt。
- 取消必须先发 SIGINT（原生中断）→ grace 后对进程组 SIGTERM → 再有界 SIGKILL 并 reap；默认 grace 10 s/5 s（初值）。
- 已请求取消的 attempt 一律以 CANCELLED 结束：取消优先于 result 帧与退出码判定，取消后收到的 result 帧（如 error_during_execution）只作诊断保存（CR-003）。

**失败与边界**

- 当未请求取消且 stdout 在 result 帧前 EOF 时，attempt=FAILED（原因 PROTOCOL_INCOMPLETE），即使退出码为0。
- 当未请求取消且 result.is_error=true 或 subtype≠success 时 attempt=FAILED 并保存原 subtype；按录制样例识别的鉴权失败映射 BLOCKED_AUTH 且不重试。
- 当 stdout 出现非 JSON 行时保存原始字节并发诊断事件，不 panic；终态仍只由合法 result 帧决定。
- 当以 root 运行且需要 bypass 时，不启动进程，报告 BLOCKED_CAPABILITY 并提示改用非 root VM 用户。
- 当子进程忽略 SIGINT/SIGTERM 时，grace 合计后 SIGKILL 整个进程组，无孤儿与僵尸，attempt=CANCELLED。
- 当 help 缺 `--output-format` 或不支持 stream-json 时，protocol=incompatible，attempt 不启动（BLOCKED_CAPABILITY）。

**验收**

- **AC-025-01**（TEST-025-01 · 检查 `integration`）Given fake claude 回放录制 stream-json（claude 2.1.x 真实样例：system init、assistant 含 tool_use、user tool_result、result success），并向 stderr 交错写入2 MiB 文本；When 适配器执行一轮，再用返回的 session_id 执行第二轮；Then stdout.jsonl 与 fixture stdout 字节一致、stderr.log 与 fixture stderr 字节一致；attempt=SETTLED；sessions.native_id=init 帧 session_id；resolved model=init 帧 model；第二轮 argv 含 `--resume <session_id>`
  - 判定：stdout.jsonl sha256=fixture stdout sha256；stderr.log sha256=fixture stderr sha256；attempt state=SETTLED；sessions.native_id 与 init/result 帧 session_id 相等；resolved model=init 帧 model；第二轮 argv 含 `--resume` 且其后一项=session_id，不含 `--permission-mode auto`
- **AC-025-02**（TEST-025-02 · 检查 `integration`）Given 表驱动7行：error_max_turns；is_error=true；result 前 EOF 且退出码0；success 但退出码1；含1行非 JSON；help 缺 `--include-partial-messages`；YOLO 下 help 缺 `--dangerously-skip-permissions`；When 逐行执行 start（需要实时增量）并等待终态；Then 前4行 attempt=FAILED；非 JSON 行无 panic、诊断事件1条、终态由 result 帧决定；缺 partial flag 时 argv 不含该 flag 且对应能力=false；缺 YOLO flag 时未 spawn 且原因 BLOCKED_CAPABILITY
  - 判定：error_max_turns、is_error、EOF、退出码1 四行 attempt=FAILED；EOF 行原因=PROTOCOL_INCOMPLETE；非 JSON 行：adapter 诊断事件恰1条，原始行字节已保存；缺 partial 行 argv 不含 `--include-partial-messages`，capabilities 对应项=false；缺 YOLO 行无子进程启动，原因=BLOCKED_CAPABILITY；全部行 argv 不含 `--permission-mode auto`
- **AC-025-03**（TEST-025-03 · 检查 `integration`）Given fake claude 忽略 SIGINT 与 SIGTERM，并 fork 同进程组孙进程 `sleep 600`；测试配置 native grace=1 s、TERM grace=1 s；When attempt 运行中调用 cancel()；Then 信号顺序为 SIGINT→SIGTERM(进程组)→SIGKILL(进程组)，相邻间隔≥对应 grace；cancel 起3 s 内进程组全部 reap，无僵尸；attempt=CANCELLED
  - 判定：信号日志顺序=[SIGINT,SIGTERM,SIGKILL]；SIGINT→SIGTERM 间隔≥1 s，SIGTERM→SIGKILL 间隔≥1 s；cancel 后≤3 s 时 /proc 中该 pgid 进程数=0，无僵尸；attempt state=CANCELLED，非 SETTLED/FAILED
- **AC-025-04**（TEST-025-04 · 检查 `adapter-live`）Given VM 实装 claude（记录 version、path、sha256、help hash），已原生登录，非 root 用户，临时 Git 仓库；When 第1轮只回复 `HOH_NATIVE_SMOKE_OK`；第2轮用 `--resume` 要求复述上轮 token；第3轮要求运行 `sleep 120` 后 cancel；Then 第1轮 result subtype=success、attempt=SETTLED；第2轮 session_id 与第1轮相同且回复含 token；第3轮 attempt=CANCELLED，进程组≤16 s 内全部 reap；未登录时检查结果为 BLOCKED 而非 PASS
  - 判定：第1轮 result.subtype=success、is_error=false，attempt=SETTLED；第2轮 init session_id=第1轮 session_id，result 文本含 token；第3轮 attempt=CANCELLED，cancel 后≤16 s 进程组进程数=0；evidence.mode=live 并记录版本、session_id、resolved model；鉴权失败时检查 outcome=BLOCKED，不得为 PASS

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 原生取消选 SIGINT；`--input-format stream-json` 下是否有协议级 interrupt 需以版本样例确认后再启用。
- 成功判定同时要求 result success、is_error=false 与退出码0，属从严选择。
- PROTOCOL_INCOMPLETE 命名为初值；`--include-partial-messages` 默认关闭，仅在需要实时增量时开启。
- （CR-003）claude 2.1.317 录制样例 sigint-cancel 显示：SIGINT 后仍输出 result(subtype=error_during_execution, is_error=true) 且退出码0，故取消判定以「已请求取消」为准；AC-025-01 所需含 tool_use 的成功样例由 M1-T03 补录。
- （CR-024）`dontAsk` 的语义依据本机 claude 2.1.325 二进制内的 SDK 说明“Don't prompt for permissions, deny if not pre-approved”；`--allowedTools` 是这种预先批准。不传 `--add-dir`：不把项目设为工作目录，避免按环境变量加载项目的 CLAUDE.md。不传 `--restricted`：它会忽略用户设置。`--tools` 的说明只覆盖内置工具集；插件、用户或项目设置带来并预先批准的工具是否被排除，help 没有说，记为余项（ADR-032 R4）。干净配置下的工具集由 TEST-021-07 从 init 帧实测。

## REQ-026 · Pi 原生接口

M1 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/13-adapters.md`、`research/SOURCES.md`、`docs/09-testing.md`、`docs/11-deployment.md`

Pi 适配器以 `pi --mode rpc` 和持久会话目录运行，严格按 LF 字节分帧，先订阅后发送，按 prompt disposition 与 agent_settled（或状态核验）判定完成，并能在重启后恢复同一会话。

**必须行为**

- 必须以 argv `pi --mode rpc` 加持久 `--session-dir`（位于 `.hoh/` 下按 session 固定）与 session id 启动，禁止 `--no-session`；路径写入 sessions.native_path。
- stdout 必须按 0x0A 字节分帧并剥离 LF 前可选的 CR，再解码 UTF-8；不使用会把 U+2028/U+2029 当换行的读取器；单帧上限1 MiB（初值）。
- 必须先启动 stdout/stderr 持续读取任务再写入任何命令，stdout 与 stderr 并行 drain，避免管道阻塞。
- 每条 prompt 命令必须带唯一 correlation id 并据此关联 response；response 成功只表示接受，不是完成。
- disposition=handled 必须立即结束该次交付（未启动模型 run，不等 settled）；started/queued 必须等 agent_settled 才判定静止；agent_end 后的重试/压缩不算完成。
- 版本不支持 agent_settled 时必须在 agent_end 等事件后用状态查询核验（非 streaming、无待重试/压缩、队列空）或声明能力不支持；禁止用 sleep 推测空闲。
- 对话类扩展 UI 请求（confirm/select/input/editor）在 headless 下必须按策略回 cancel 并记录阻塞事件；非对话类 UI 请求只映射为数据事件，不嵌入 Pi 画面。
- 取消必须先发原生中止命令，超 grace 后对进程组 SIGTERM→SIGKILL 并 reap。
- Commander 的只读档必须按 REQ-021 行为 9 的 pi 一行启动。仅当 help 声明 `--tools`、`--no-mcp` 与 `--no-extensions`，且其 Built-in Tool Names 段列出 read、grep、find、ls 时使用，否则不启动（BLOCKED_CAPABILITY）。Commander 的 pi 会话不加载任何扩展：`--no-extensions` 使发现的、配置的与内置的扩展都不加载，也不以 `-e` 显式加载任何扩展。v1 的三个 Pi 插件（REQ-048–050）服务于其他 Pi 职位，不进 Commander 的会话；这一点在 docs/12 与 doctor 中写明，不是省略插件。Pi 的只读只靠进程内按工具名的允许清单，没有操作系统级屏障，记为余项（ADR-032 R3）（CR-024）。

**失败与边界**

- 当帧内含 U+2028/U+2029 或一帧跨多次 read 到达时，解析出的帧数与内容必须与 fixture 完全一致。
- 当单帧超过1 MiB 仍无 LF 时，停止缓冲，保存诊断（长度、前缀 sha256、前4 KiB）并使 attempt FAILED，不无限分配内存。
- 当 agent_end 之后出现自动重试或压缩事件时，attempt 保持 RUNNING 直至 agent_settled。
- 当 headless 收到 select/confirm 请求时，不得自动选择任何选项；未回应即视为缺陷。
- 当 Pi 进程被 kill -9 后，以同 session-dir/session id 重启必须恢复同一会话；恢复失败则 attempt=FAILED 并保留旧会话目录，不新建会话冒充。
- 当 prompt response 为 success=false 时，该输入为 REJECTED 并保存错误，不等待 settled。

**验收**

- **AC-026-01**（TEST-026-01 · 检查 `integration`）Given fake pi 回放录制 RPC JSONL：帧内含 U+2028、U+2029、中文与 emoji；一帧拆成3次 write，两帧合并为1次 write；收到 prompt 后先于 response 立即发出 agent_start；同时向 stderr 写10 MiB；When 适配器启动 fake pi，发送 prompt 并等待终态；Then 解析帧数=fixture 帧数且逐帧 JSON 值相等；agent_start 被捕获并排在该 prompt 事件序列首位；解析错误0；30 s 测试超时内到达终态
  - 判定：解析帧数与 fixture 相等，逐帧 JSON 值相等；含 U+2028/U+2029 的帧各保持为1帧；agent_start 事件存在且位于该 prompt 事件序列首位；解析错误计数=0；10 MiB stderr 已完整保存，终态在30 s 内到达
- **AC-026-02**（TEST-026-02 · 检查 `integration`）Given FakeClock 冻结；4份 fixture：(a) disposition=handled；(b) started→agent_end→自动重试→agent_end→agent_settled；(c) 同 (b) 但中间为压缩；(d) 无 agent_settled 的版本，状态查询先报忙后报空闲；When 逐份发送 prompt，并在 (b)(c)(d) 首个 agent_end 后立即读取 attempt 状态；Then (a) 不等待 settled 即完成；(b)(c) 首个 agent_end 后仍 RUNNING，收到 agent_settled 后才 SETTLED；(d) 仅在状态查询报空闲后 SETTLED；全程 FakeClock 未推进
  - 判定：(a) 终态在 response 后到达，未发出任何等待 settled 的计时器；(b)(c) 首个 agent_end 后 attempt=RUNNING，agent_settled 后=SETTLED；(d) 状态查询次数≥2，最后一次报空闲后才 SETTLED；FakeClock 推进次数=0
- **AC-026-03**（TEST-026-03 · 检查 `integration`）Given fake pi 记录 argv 与会话目录；fixture 含 confirm、select 两个对话类 UI 请求与1个状态类非对话请求；When 执行一轮至 settled，kill -9 fake pi，用同一 SessionRef 调用 resume()；再回放 UI 请求；Then 两次 argv 均含 `--mode rpc` 与相同 `--session-dir`/session id，均不含 `--no-session`；sessions.native_path 不变；confirm/select 各收到1条 cancel 响应且无选项值；非对话请求收到0条响应
  - 判定：两次 argv 的 `--session-dir` 与 session id 值相同，均不含 `--no-session`；resume 前后 sessions.native_path 相同，未新建 session 行；confirm、select 各1条 cancel 响应，响应中无选项值；非对话请求响应数=0，并生成对应数据事件；每个对话请求均有阻塞/决策事件记录
- **AC-026-04**（TEST-026-04 · 检查 `adapter-live`）Given VM 上按 bootstrap 安装的 Pi（记录 version、path、sha256、help hash 与 Node 版本），已原生配置 provider，临时 Git 仓库；未安装时本 AC 记 NOT_RUN；When RPC 发 prompt 只回复 `HOH_NATIVE_SMOKE_OK`；结束后终止进程并以同会话重启，要求复述上轮 token；再发长任务后 cancel；Then 第1轮终态由 agent_settled（或状态核验）判定且回复含 token；重启后会话 id 相同且回复含 token；cancel 后 attempt=CANCELLED、进程组全部 reap；缺认证时检查结果为 BLOCKED 而非 PASS
  - 判定：第1轮 response 被接受，终态依据为 agent_settled 或状态核验，回复含 token；第2轮会话 id 与第1轮相同，回复含 token；cancel 后≤16 s 进程组进程数=0，attempt=CANCELLED；argv 不含 `--no-session`；evidence.mode=live；鉴权失败时 outcome=BLOCKED；未安装时 outcome=NOT_RUN，均不得为 PASS

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 帧名已对照本机 Pi 1.5.0 自带文档（docs/rpc.md、rpc-commands.md、rpc-extension-ui.md）核实：命令以 `id` 关联 response；prompt 的 disposition 为 started/queued/handled；事件 agent_start/agent_end/agent_settled；中止命令 `abort`；状态查询 `get_state`（isStreaming、isCompacting、pendingMessageCount）；对话类 extension_ui_request（select/confirm/input/editor）须以同 id 的 extension_ui_response 回 `cancelled: true`，notify/setStatus/setWidget/setTitle/set_editor_text 不需回应；会话参数 `--session-dir`/`--session-id`。M0-T04 仍须录制真实样例。
- 已修正（CR-002，依据 Pi 1.5.0 docs/rpc.md）：只按 0x0A 分帧，并剥离 LF 前可选的 CR 以接受 CRLF。
- 会话参数为 `--session-id <id>`（Pi 1.5.0 help）；UI 请求 cancel 回应测试阈值≤1 s 为确认值。
- （CR-003）Pi 1.5.0 没有结构化鉴权错误码：response success=false 时按录制模式分类——error 以 `No API key found` 开头→BLOCKED_AUTH，以 `Model not found` 开头→MODEL_UNAVAILABLE（样例 rpc-no-provider、rpc-invalid-model）；不匹配任何模式→PermanentExternal 并保存原文。

## REQ-027 · 协议版本与能力

M1 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/13-adapters.md`、`docs/04-architecture.md`、`docs/07-algorithms.md`、`docs/17-risks-gaps.md`、`docs/05-data-api.md`、`docs/03-ux.md`、`contracts/sql/001_initial.sql`、`contracts/openapi.json`

适配器从实际版本、--help 与原生 schema 探测能力并写入 compat manifest，按能力而非品牌启用功能；未知事件/字段原样保存不 panic，超大帧有界失败，不支持的能力显式降级。

**必须行为**

- probe() 必须记录 launcher path 与 sha256，以及解析后的实际实现 path 与 sha256（如 pi 由 install/current-version 解析到 releases/<版本>，codex 由 npm 包装解析到 vendored 原生二进制）、--version 原文、相关 --help 的 sha256、原生 schema hash（Codex 用 `codex app-server generate-json-schema` 输出），写入符合 contracts/compat-manifest.schema.json 的 compat manifest（CR-003）。
- Capabilities（structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage、command_execution、read_only_workspace）必须由探测与兼容套件结果得出，不按品牌硬编码，并写入 sessions.capabilities_json；command_execution 指适配器在 HoH 的全权限启动选项下可运行命令，由被测二进制的 help/schema 实测（ADR-030 决定 12）。（CR-024）read_only_workspace 指适配器能以只读工作区档启动该 Agent，会话记录里为下列三项之与：被测 help/schema 的探测规则；只读档的构建函数不拒绝；兼容清单的 read_only 用例为 PASS——该用例只核对只读档的请求，不计入 suite.status。选人时，codex 另须本机沙箱检查不为 unconfined 或 unavailable，且其 App Server 未在本 host 运行中降级为 exec；这些是主机的事实，不写入会话记录（ADR-032 决定 4、10）。
- 兼容套件 fixture 必须对每个 agent 覆盖成功、失败、取消、压缩、工具、未知字段6类，manifest 记录所用 fixture 集 hash（本构建的套件集摘要：覆盖嵌入 fixture、请求构建函数的实际输出与套件定义，ADR-030 决定 10）与结果。
- 实现 sha256、help 或 schema hash 与 manifest 不符时，新 attempt 不得使用该版本（COMPAT_UNVERIFIED），直到在无写租约时重跑兼容套件通过；运行中的 attempt 不受影响。
- 未知事件类型与已知事件中的未知字段必须保存原始帧并产生 `adapter.unknown_event` 诊断，不 panic、不改变终态判定。
- 请求不支持的能力必须返回 CAPABILITY_UNSUPPORTED（API 为422）或走文档化降级（如 steer 排到下一轮），不得静默伪装支持。
- 协议帧超过1 MiB（初值）必须有界失败：缓冲不超过上限加一次读块，保存诊断并使该 attempt FAILED（PROTOCOL_FRAME_TOO_LARGE）。（CR-040）跳过例外：适配器凭帧的前4 KiB 按确切字节前缀认出的已知记录、且适配器不需要其载荷（白名单：pi 的 agent_end、turn_end、tool_execution_end、toolResult 的 message_start/message_end，claude 的 user，codex App Server 的 turn/diff/updated），读到其 LF 时跳过：边读边丢弃，缓冲同样不超过上限加一次读块，记 adapter.diagnostic（类型、长度、整帧 sha256、前4 KiB），之后继续成帧；被认出但在 LF 前流结束或超过 256 MiB 的帧仍按上限失败。（CR-041）另对确切前缀识别的 Codex 完成 item（及携带相同 changes 的 fileChange item/started）做有界字符串归约（App Server commandExecution/fileChange、exec command_execution）：所有字段与元素保留，超过4 KiB 前缀预算的字符串值改为带 truncated/prefix/original_bytes/sha256 的对象；键不截断。归约结构≤512 KiB，原始≤256 MiB，读到 LF 且完整 JSON/UTF-8 验证才交付；路由标识必须完整。无 LF、格式损坏、超预算或未知帧仍失败。判定事实被截断时只读判定器不得 PASS。
- 能力为 false 或数据缺失的指标（如 token_usage）必须显示 unknown/null，不显示0。
- 每个适配器按 CLI 版本维护已知帧登记表 tests/fixtures/protocol/<agent>/<version>/known-frames.json（由录制样例与官方文档汇总，含已知但忽略的辅助帧，如 Claude 的 rate_limit_event、system/task_*，Codex 的 remoteControl/status/changed、deprecationNotice）；只有不在登记表中的帧才产生 adapter.unknown_event（CR-003）。

**失败与边界**

- 当安装脚本成功但兼容套件未运行时，protocol=unknown，不视为兼容。
- 当新版 Codex help 不再含 `app-server` 时，探测得 mid_turn_steer=false 并降级 exec，不沿用旧 manifest 推断。
- 当出现未知 method/type 帧时保存原始帧，attempt 终态与去掉该帧的对照回放一致。
- 当收到8 MiB 无 LF 的单行时，最大缓冲≤1 MiB+64 KiB，attempt FAILED，host 与其它 attempt 继续运行。
- 当有写租约活跃时请求运行兼容套件，被拒绝并提示维护窗口，不中途替换正在使用的版本。
- 当 Claude 不支持中途 steer 时，steer 输入保持 QUEUED 至下一轮 resume 投递，不标 APPLIED。
- （CR-040）当长会话的 pi 发出超过1 MiB 的 agent_end 等白名单记录并以 agent_settled 结束时，attempt SETTLED，不因帧长失败；帧首不符白名单的超限帧照旧 FAILED。

**验收**

- **AC-027-01**（TEST-027-01 · 检查 `integration`）Given fixture 含3组 fake 二进制：codex 0.166 的 help 与 schema 输出、去掉 `app-server` 的旧 codex、缺 `--include-partial-messages` 的旧 claude，各 sha256 已知；When 对每组调用 probe() 两次并生成 compat manifest；Then 每项通过 contracts/compat-manifest.schema.json 校验，含 launcher_path 与 launcher_sha256、implementation_path 与 implementation_sha256（均等于已知值；fake 二进制自身即实现时两组相同）、cli_version（--version 原文）、transport、capabilities、help_sha256、schema_sha256（无 schema 为 null）、suite（只探测未跑套件时 status=NOT_RUN）；旧 codex capabilities.mid_turn_steer=false 且 transport=exec（降级）；旧 claude capabilities.partial_messages=false；同输入两次探测结果相同
  - 判定：3项 manifest 均通过 compat-manifest.schema.json 校验，launcher_sha256 与 implementation_sha256 等于已知值；Claude 与旧 codex 项 schema_sha256=null，codex 0.166 项为其 schema 输出的 sha256（64位十六进制）；旧 codex capabilities.mid_turn_steer=false，transport=exec；两版 codex 能力集不同，且各自两次探测结果完全相同；旧 claude capabilities.partial_messages=false
- **AC-027-02**（TEST-027-02 · 检查 `integration`）Given 三个适配器各1份 fixture：插入1条不在该版本已知帧登记表中的 method/type 帧，并在已知事件中加入未知字段 `x_future`；另备删除未知帧的对照 fixture；When 回放两组 fixture 至终态；Then 无 panic；每份恰1条 adapter.unknown_event 且含原始帧 sha256；原始日志 sha256=输入字节 sha256；`x_future` 可从 raw 旁路取回；终态与对照回放一致
  - 判定：6次回放无 panic、进程均正常结束；每份含未知帧的回放 adapter.unknown_event 数=1，事件含帧 sha256；原始日志 sha256 与输入一致；`x_future` 字段值可从 raw 取回且等于输入；attempt 终态与对照回放逐一相等
- **AC-027-03**（TEST-027-03 · 检查 `integration`）Given 三个适配器各1份 fixture：若干正常帧后接8 MiB 无 LF 的单行；帧上限为默认1 MiB；同 host 另有1个正常 fake attempt 并行；When 回放并通过测试探针记录 reader 最大缓冲字节；Then 最大缓冲≤1 MiB+64 KiB；attempt=FAILED、原因 PROTOCOL_FRAME_TOO_LARGE；诊断含帧长度、前缀 sha256 与前4 KiB；并行 attempt=SETTLED
  - 判定：三个适配器最大缓冲字节均≤1,114,112；超大帧 attempt state=FAILED，原因=PROTOCOL_FRAME_TOO_LARGE；诊断记录含长度、前缀 sha256、前4096字节；并行正常 attempt state=SETTLED，host 进程未退出
- **AC-027-04**（TEST-027-04 · 检查 `integration`）Given compat manifest 已登记 fake codex A 的 hash 且兼容套件 PASS；一个使用 A 的 RUNNING attempt 持有写租约；When 将 PATH 中 codex 替换为 hash 不同的 B 后请求新 attempt；请求运行兼容套件；待写租约释放后运行套件并重试；Then 新 attempt 被拒（COMPAT_UNVERIFIED）而原 attempt 继续至 SETTLED；写租约活跃时套件请求被拒；套件 PASS 后 manifest 的 codex 项更新为 B（implementation_sha256=B 的 hash、suite.status=PASS）且新 attempt 成功启动；PASS 前 doctor 显示 protocol=unknown
  - 判定：替换后首个新 attempt 被拒，错误码=COMPAT_UNVERIFIED，无子进程启动；原 attempt 最终 state=SETTLED，其进程仍为 A；doctor codex 项 protocol=unknown；租约活跃时套件请求被拒绝；套件 PASS 后 manifest 的 codex 项 implementation_sha256 等于 B 的 hash 且 suite.status=PASS，重试 attempt 成功启动
- **AC-027-05**（TEST-027-05 · 检查 `integration`）Given pi fixture：正常帧之后1条 1.5 MiB 的 `agent_end`（键序同 pi：`{"type":"agent_end","messages":[…],"willRetry":false}`），其后 `agent_settled`；另备同长度但帧首不在白名单的一行、在 LF 之前流就结束的同一 `agent_end`；帧上限为默认1 MiB；When 分别运行 attempt 并通过测试探针记录 reader 最大缓冲字节；Then 白名单帧的 attempt=SETTLED，恰1条跳过诊断（含类型 agent_end、帧长度、整帧 sha256 与前4 KiB）；最大缓冲≤1 MiB+64 KiB；帧首不符与被截断的两份 attempt=FAILED、原因 PROTOCOL_FRAME_TOO_LARGE
  - 判定：白名单帧 attempt state=SETTLED；恰1条跳过诊断，含 type=agent_end、帧长度、整帧 sha256、前4096字节；最大缓冲字节≤1,114,112；帧首不符与被截断的 attempt state=FAILED，原因=PROTOCOL_FRAME_TOO_LARGE
- **AC-027-06**（TEST-027-06 · 检查 `integration`）Given Codex App Server commandExecution/fileChange 与 exec command_execution 的 1.5 MiB item 完成帧（fileChange 另含相同 changes 的 started 帧）；未知超限帧、无 LF、无效尾部和超结构预算变体；只读判定 fixture；When 运行 adapter attempt 与 reader 探针，并把带截断标记的判定事实送入只读判定器；Then 已知合法 item 的 attempt=SETTLED；所有字段保留，大字符串值为含 truncated=true、prefix、original_bytes、sha256 的对象；诊断含原始整帧长度/hash/head，raw 日志完整；最大保留字节≤1 MiB+64 KiB；其他变体 FAILED/PROTOCOL_FRAME_TOO_LARGE；只读判定不得依据截断事实给出 PASS
  - 判定：已知合法 item 的 attempt=SETTLED；所有字段保留，大字符串值为含 truncated=true、prefix、original_bytes、sha256 的对象；诊断含原始整帧长度/hash/head，raw 日志完整；最大保留字节≤1 MiB+64 KiB；其他变体 FAILED/PROTOCOL_FRAME_TOO_LARGE；只读判定不得依据截断事实给出 PASS

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- COMPAT_UNVERIFIED、PROTOCOL_FRAME_TOO_LARGE 命名与读块64 KiB 为初值；帧上限1 MiB 为 docs/13 初值。
- 兼容套件是否必须含 live smoke（产生费用）未定；选择离线探测+fixture 为必需，live smoke 由 adapter-live 检查单独记录。
- Claude/Pi 无原生 schema 命令，schema_sha256 记 null，以 help hash 与 fixture 为准。

## REQ-028 · 项目发现

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/01-prd.md`、`docs/05-data-api.md`、`docs/12-user-guide.md`、`docs/adr/ADR-021.md`

建立基线，保留脏文件，不重写架构、不强制初始化远程

**必须行为**

- Run 的 CREATED→DISCOVERING→SPECIFYING 由 Runtime 推进（CONTRACT 第 6 条“常规阶段自动流转”，ADR-031 给 M3 的移交）：CREATED 的 Run 已采用规格，或已以 draft 进入起草（REQ-008）时，Runtime 把它置为 DISCOVERING 并做项目发现，写 run.baseline{head, tree, dirty:[{path, sha256}], untracked:[{path, sha256}], build_commands, test_commands, docs}，之后才进入 SPECIFYING；run.baseline 先于该 Run 的任何写入（git init、Attempt、effect）。Commander 的 transition_run 不能让 Run 在 run.baseline 之前离开 DISCOVERING（ILLEGAL_TRANSITION）。基线随 Run 记录（C11）；非 git 目录的 head 与 tree 为 null。
- 发现只读：不改写、不 stash、不 reset 工作树，不新建提交，不改远程配置；git status --porcelain、脏文件与未跟踪文件的摘要在发现前后逐字相同。
- 构建与测试命令按常见约定检测（Cargo.toml、package.json 的 scripts、Makefile、pyproject.toml），检测不到记 unknown（C10）；发现不自动运行项目的构建与测试（O3）；用户确认后运行一次，结果按命令记入基线，之后的回归以基线为对照。
- 写 Task 的路径范围（REQ-031 的任务范围）含有用户未提交修改的文件时，HoH 不碰这些文件：Task 为 BLOCKED（NEED_DECISION），问题列出这些文件（O3）；这些文件的摘要在 Run 全程不变。
- Greenfield（空目录或非 git 目录）：HoH 只在首个起草 Attempt 的准备阶段（REQ-008：持该 Attempt 的写者租约，记为它的 effect）本地 git init，不加远程、不推送；首个提交只含 docs/spec/、docs/ 与 .hoh/ 下的路径（起草 Task 的写集核对）；已是 git 仓库时不重复初始化。
- 已有文档按常见位置自动发现（README、docs/、ADR 目录），列入基线的 docs。
- 不重写架构：发现不创建任何写 Task；Brownfield 的计划中每个 Task 都必须引用用户目标范围内（当前已批准修订中）至少一条 REQ，不引用任何 REQ 的 Task（如重构既有模块）被拒（REQ-031、REQ-009）。

**失败与边界**

- 发现过程中任何写操作都不发生：git status --porcelain 与文件摘要不变，git remote -v 不变，提交数不变。
- 写 Task 涉及脏文件时，Task=BLOCKED(NEED_DECISION)，问题列出文件路径，文件摘要不变。
- 不引用已批准 REQ 的计划 Task 被拒，没有被导入。
- 基线已失败的测试不被计为 HoH 引入的回归。

**验收**

- **AC-028-01**（TEST-028-01 · 检查 `integration`）Given 夹具 git 仓库：有代码、测试、README 与 docs/，两个已修改文件、一个未跟踪文件，一个远程；When 以新 Run 执行项目发现；Then 写 run.baseline，含 head、tree、两个脏文件与一个未跟踪文件的路径与 sha256、检测到的 build_commands 与 test_commands、README 与 docs/；发现前后 git status --porcelain 逐字相同、脏与未跟踪文件的摘要不变、提交数与 git remote -v 不变；发现期间没有创建写 Task
  - 判定：run.baseline 字段齐全且列出脏与未跟踪文件；前后 git 状态、摘要、提交数与 remote 逐字相同；发现期间无写 Task
- **AC-028-02**（TEST-028-02 · 检查 `integration`）Given 基线检测到测试命令 cargo test，其中一个测试本来失败；用户确认运行测试；When 发现运行测试一次，随后一个候选使同一测试仍失败；Then 基线按命令记录 pass/fail 且含该失败测试；该候选的回归判定不把它算作 HoH 引入的回归；用户确认前没有运行任何项目命令
  - 判定：确认前无项目命令运行；基线记录 pass/fail 含失败测试；回归判定不含该测试
- **AC-028-03**（TEST-028-03 · 检查 `integration`）Given 夹具仓库中文件 F 有用户未提交的修改；已批准修订含 REQ-A；Commander 先提出计划 P1：写 Task T（引用 REQ-A，路径范围含 F）与 Task U（不引用任何 REQ，重构既有模块）；再提出只含 T 的计划 P2；When Runtime 依次处理 P1 与 P2，并运行调度轮；Then P1 整体被拒（details 指出 U 没有引用已批准 REQ），T 与 U 都没有导入，tasks 与 task_requirements 不变；P2 被接受、T 被导入；T 为 BLOCKED(NEED_DECISION)，问题列出 F；没有 T 的写 Attempt；F 的 sha256 在 Run 全程不变，没有 stash 或 reset
  - 判定：P1 整体被拒且 details 指出 U 未引用 REQ，零导入；P2 接受、T 导入；T BLOCKED(NEED_DECISION) 且问题列出 F；无 T 的写 Attempt；F 摘要全程不变
- **AC-028-04**（TEST-028-04 · 检查 `integration`）Given 一个空目录与一个已有提交的 git 仓库，两者都没有已批准规格；cto 的假 Agent 起草时只写 docs/spec/ 并提交；When 各启动一个 Run，用户以 draft 答复 runtime:no_spec 问题，起草 Task 完成；Then 两个 Run 的 run.baseline 都先于该 Run 的任何 Attempt 与 effect 记录（空目录的 head 为 null）；空目录在起草 Attempt 的准备阶段被本地 git init（该 Attempt 的 effect），没有远程，首个提交只含 docs/spec/、docs/ 与 .hoh/ 下的路径；已有仓库没有被重新初始化，提交历史保留，新提交同样只含这三处路径
  - 判定：run.baseline 先于任何 Attempt 与 effect；空目录在起草 Attempt 的准备阶段本地初始化（effect）且无 remote；首个提交只含三处路径；已有仓库未重新初始化且新提交只含三处路径

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 已有仓库的做法取所有者 2026-11-17 的决定 O3：碰到未提交文件先阻塞询问；发现不自动运行构建与测试（CR-026）。
- 基线随 Run 记录（run.baseline），不同 Run 可以有不同的基线（CR-026，C11）。
- 命令检测按常见约定，检测不到为 unknown，不猜测（CR-026，C10）。
- 规格阶段之前的 CREATED→DISCOVERING→SPECIFYING 由 Runtime 推进，基线先于任何写入（CR-026，CR26D-151）。

## REQ-029 · 内置模板

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/21-spec-governance.md`、`docs/15-prompts-skills-memory.md`、`docs/00-product-definition.md`、`config/hoh.example.toml`、`resources/templates/profiles.json`

产品/需求/功能/UX/架构/开发/测试/验收/部署/使用规范齐全；按规模选用

**必须行为**

- 内置模板覆盖十类规范：产品（product-definition）、需求（prd）、功能（feature）、UX（ux）、架构（architecture）、开发（development-plan）、测试（test-plan）、验收（acceptance）、部署（deployment）、使用（user-guide），按 lite、standard、full 三档组织（resources/templates/profiles.json）；full 含全部十类，lite 与 standard 是按规模的子集。
- 模板随产品分发（嵌入二进制，计入 resource_bundle_version，C17）；无网络、无外部工具也能实例化；hoh doctor 列出模板包版本。
- 实例化填实全部必填占位（不残留占位语法），并写 docs/21 的元数据：id、title、revision、status、owners、related_requirements、source_refs。
- 档取 project.document_profile 的有效值：项目配置写了就用它，没写就用配置的缺省值 full（REQ-018，config/hoh.example.toml 不变）；实例化记录所用的档与来源（project 或 default）（C16）。
- 改档时只补缺的文档，不覆盖用户已编辑的文件。
- 规格模板（docs/spec/ 的骨架）含 REQ-034 的固定标题（用户、范围、非目标、决策、开放问题、需求），供 REQ-030 的五类内容与 REQ-008 的起草流程使用（C7）。
- 实例化的产品入口是首次批准之前的起草（REQ-008）：起草 Task 的首个 Attempt 开始之前（git init 之后），Runtime 持该 Attempt 的写者租约，把规格模板以 Run 的目标文本实例化到 docs/spec/，并按有效档把 profiles.json 所列的全部模板实例化到 docs/（每个模板一份、文件名以模板 id 开头；full 档含十类 product-definition、prd、feature、ux、architecture、development-plan、test-plan、acceptance、deployment、user-guide，另有 api-data、adr、contributing、changelog、change-request、milestone-report），整体记为该 Attempt 的一个 effect；已存在的文件不覆盖；这些路径都在 Plan 的写集内，由起草 Agent 填写并随规格提交。

**失败与边界**

- 实例化结果残留占位语法或缺元数据字段时，判为失败并列出文件与位置。
- 改档时已编辑的文件内容不变。
- 离线环境实例化不访问网络。

**验收**

- **AC-029-01**（TEST-029-01 · 检查 `unit`）Given 三个空的夹具项目，project.document_profile 分别为 lite、standard、full；When 各实例化一次；Then full 的生成文件覆盖十类：产品、需求、功能、UX、架构、开发、测试、验收、部署、使用（各至少一份，按 profiles.json 的映射）；lite 与 standard 各生成 profiles.json 所列的全部模板；生成文件中没有残留的占位语法；每份都有 id、title、revision、status、owners、related_requirements、source_refs
  - 判定：full 覆盖十类；lite 与 standard 齐所列模板；无残留占位；元数据字段齐全
- **AC-029-02**（TEST-029-02 · 检查 `integration`）Given 两个项目：一个的项目配置没有 document_profile，一个先以 lite 实例化后用户编辑了其中一份文件，再把档改为 full；When 各实例化一次；Then 未写档的项目用 full，并记录档为 full、来源为 default；改档的只新增 full 比 lite 多出的文档，已编辑文件的 sha256 不变
  - 判定：未写档用 full 且记录来源 default；只补缺的文档；已编辑文件不变
- **AC-029-03**（TEST-029-03 · 检查 `deployment`）Given 安装的发布包，测试环境不提供网络，HOME 与 XDG 目录为测试私有、没有运行中的 host；一个新的空项目，cto 职位绑定到随测试安装的假 Agent（它不改 docs/ 下的模板，只提交 docs/spec/ 后完成）；When 执行 hoh doctor；以 hoh -p 启动 Run（host 由此启动），以 strace -f -p 附着该 host 的全部线程（pid 取自 host 的运行记录），再在 strace -f 下以 hoh -p draft --resume <run_id> 选择起草；Then doctor 输出列出模板包版本；起草 Attempt 开始时 docs/spec/ 有规格骨架，docs/ 下有 full 档 profiles.json 所列的全部模板文件，其中十类 product-definition、prd、feature、ux、architecture、development-plan、test-plan、acceptance、deployment、user-guide 各一份且没有残留占位语法；两个 strace 的全部记录（客户端、host 与 host 的子进程）中 connect 的目标只有本地地址（AF_UNIX 或回环），或 sa_family=AF_NETLINK 且 nl_pid=0 的内核端点；后一种仅允许同一进程此前由 socket(AF_NETLINK, SOCK_RAW|..., NETLINK_SOCK_DIAG) 创建的描述符，两个 strace 同时记录 socket、close 与 connect，并按 pid 和 fd 关联到该 pid 中 connect 之前最近一次返回该 fd 的 socket，close 后旧关联失效；其他 netlink 协议、非零 nl_pid、无法关联的 fd 或无法解码的记录均失败；第二个 hoh -p 停在批准问题并以 3 退出
  - 判定：doctor 列出模板包版本；docs/spec/ 骨架与 full 档全部模板（十类各一份）且无占位；客户端、host 与其子进程的 connect 只到本地地址；第二个 -p 以 3 退出

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 未在项目配置中指定档时用配置缺省 full，不改变 REQ-018 的缺省值，并记录来源 default（CR-026，C16，CR26D-09）。
- 模板嵌入二进制，与职位契约、schema 同一资源包（CR-026，C17）。

## REQ-030 · 规格收敛

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/15-prompts-skills-memory.md`、`docs/01-prd.md`、`docs/21-spec-governance.md`、`config/hoh.example.toml`、`docs/adr/ADR-031.md`

记录用户/范围/非目标/决策/开放问题；必要不确定性不伪造用户答案

**必须行为**

- 规格记录五类内容（REQ-034 的固定标题）：用户、范围、非目标、决策、开放问题；每类非空或明写“无”。
- 每条决策注明来源：用户答复（question_id）、用户的交互输入（input_id），或假设（record_assumption 的事件 id）；没有来源的决策不允许。
- 交互运行中，需要用户选择时以 request_user_decision 提问并等待答复，答复按 question_id 记录，不被记成假设。
- 无人值守（-p）时只能假设项目配置 [assumptions] 中列出的默认值（O2）：每个假设以 record_assumption 记录，basis 写配置键；basis 不是 [assumptions] 中列出的键的 record_assumption 被拒（决策 REJECTED），不写假设；产品方向、范围、非目标与未列出的必需决定一律不假设，Run 以 NEED_DECISION 阻塞并持久化问题。[assumptions] 中以 product_direction、scope、non_goals 为名的键使配置被拒（与未知键同一路径），即使列出也不会被假设；config/hoh.example.toml 的 [assumptions] 为空表，空表不产生任何假设。
- 规格提议与送批（ProposeSpecChange、ApproveSpec）时 Runtime 校验来源：标为用户决定而在 DB 中找不到对应 AnswerQuestion 或交互输入的条目，使其被拒（SPEC_INVALID，details 列出无来源的条目）。
- 假设台账是事件的投影（record_assumption 的应用事件），不另建表（C6）。

**失败与边界**

- 无人值守遇到产品方向这类关键未知时，Run=BLOCKED(NEED_DECISION)，问题持久化，-p 以 3 退出并列出问题，不写任何假设。
- 把假设写成用户决定的规格被拒（SPEC_INVALID），没有新的已批准修订。
- 配置 [assumptions] 未列出的决定类，无人值守时不假设，照样阻塞；列出 product_direction、scope 或 non_goals 的配置被拒。

**验收**

- **AC-030-01**（TEST-030-01 · 检查 `integration`）Given 交互运行，目标“做一个记账工具”，FakeCommander 脚本提出一个范围问题，用户答复其选项；When 起草完成，规格经提议送批；Then 规格五个标题下各有内容或“无”；决策中来自答复的条目引用其 question_id，DB 中有对应的 AnswerQuestion 事件；批准通过
  - 判定：五节齐全；答复来源的决策引用 question_id 且 DB 有 AnswerQuestion；批准通过
- **AC-030-02**（TEST-030-02 · 检查 `integration`）Given 无人值守（-p）的 AUTO Run。项目 A 有已批准修订 1（没有语言与产品方向的决定），配置 [assumptions] 列出 language="Rust"；FakeCommander 补规格时依次提出 record_assumption{basis: assumptions.language}、record_assumption{basis: assumptions.product_direction}，再以 request_user_decision 问产品方向。项目 B、C、D 的配置 [assumptions] 分别列出 product_direction、scope、non_goals；When 四个项目各以 hoh -p 运行至停止；Then A：语言以 record_assumption 记录且 basis 含 assumptions.language；产品方向的 record_assumption 被拒（REJECTED，details 指出 product_direction 不是 [assumptions] 中列出的键），没有该假设；Run=BLOCKED(NEED_DECISION)、产品方向的问题持久化、-p 以 3 退出并列出该问题；没有新的已批准修订。B、C、D：配置都被拒，各自指出键路径 assumptions.product_direction、assumptions.scope、assumptions.non_goals，没有任何假设与派发，-p 以配置错误退出
  - 判定：A：basis 含 assumptions.language；A：product_direction 的 record_assumption 被拒且无该假设；A：BLOCKED(NEED_DECISION)、问题持久化、退出码 3；A：没有新的已批准修订；B、C、D：配置被拒并各自指出保留名的键路径，无假设与派发
- **AC-030-03**（TEST-030-03 · 检查 `integration`）Given 送批的规格中一条决策标为用户决定，引用的 question_id 在 DB 中没有答复（实为假设）；When 以 ProposeSpecChange 提议；Then 返回 422，code=SPEC_INVALID，details 列出该条目及其缺失的来源；没有 change_requests 行与新的已批准修订
  - 判定：HTTP 422 且 code=SPEC_INVALID，details 列出该条目；无 change_requests 行与新修订

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 无人值守可假设的范围取所有者 2026-11-17 的决定 O2：只有项目配置中列出的默认值；产品方向、范围与非目标一律阻塞，配置中也不能为它们给默认值（CR-026，CR26D-52）。
- 批准只由用户（O1），无人值守的 Run 停在批准之前（CR-026）。
- 假设台账由事件投影，不新增表（CR-026，C6）；五节模板属 REQ-029 的模板（C7）。
- 新码 SPEC_INVALID（ErrorCode，422）报告提议或送批规格的结构或来源问题（CR-026）。

## REQ-031 · Readiness Gate

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/07-algorithms.md`、`docs/adr/ADR-017.md`、`docs/adr/ADR-028.md`、`docs/adr/ADR-031.md`

必要 AC 可观察、依赖可达、关键决定确定；仅阻断当前受影响范围

**必须行为**

- Readiness 是已批准规格修订、它的独立规格审查与当前计划的纯函数，由 Runtime 自动判定（C5），结果为 READY 或 NOT_READY{reasons}；Commander 不能覆盖判定结果。
- 不就绪的原因：范围内 required REQ 没有 AC（missing_ac）；AC 缺 then 或 check（unobservable）；计划的依赖不存在或成环（DAG_MISSING_DEPENDENCY、DAG_CYCLE）；开放问题中标为阻塞的未决（undecided，标记方式见 C4）；该修订没有可用的独立规格审查（spec_review）。
- 独立规格审查（docs/06 规格循环：write→independent spec review→readiness→task DAG）：Run 采用一个已批准修订，而该修订既没有 CURRENT 的审查报告也没有进行中的审查时，Runtime 在这个 Run 中为它建一个 reviewer 职位的只读审查 Task（spec_id 为该修订，不引用 REQ），写 spec_review.opened{review_id, run_id, spec_id, spec_revision, spec_sha256, git_commit, task_id}（git_commit 是该修订的规格提交：从它的 Markdown 重新生成的索引的 sha256 等于 specs.sha256）。它按 Runtime 规则类派发：Run 处于 SPECIFYING、READY、RUNNING 或 VERIFYING 时派发，不改变 Run 的生命周期状态（已启动的 Run 不回到 SPECIFYING，例如验证中批准的修订，它的审查在 VERIFYING 或 RUNNING 中派发），PAUSE_REQUESTED、PAUSED、BLOCKED、WAITING_EXTERNAL 中不派发新的；持读者租约，在该提交的只读检出中运行。审查按修订而不按 Run：readiness 的 spec_review 只在该修订最新的 CURRENT 报告为 PASS、且报告中没有 critical 或 high 的发现时满足；FAIL、UNKNOWN、缺失、REJECTED、带 critical 或 high 发现的 PASS（模型自报的状态不覆盖它自己的发现）或绑定别的修订，都使 readiness 为 NOT_READY(spec_review)。审查 Task 是普通的读 Task：取消、租约与崩溃恢复同其他读 Task；它终结而没有被收取的报告时，Runtime 为同一修订再开一次审查（新的 review_id）。用户批准（O1）不替代它。
- 规格审查的报告与原生记录：审查 Agent 在最后的消息中给出 hoh-report 块；只在该 Attempt 的 TaskResult 为 COMPLETED 时，host 构造两份对象并登记为 artifacts——原生记录（新的受锁契约 contracts/spec-review-attempt.schema.json：record_type=runtime_spec_review_export、role=reviewer、status=COMPLETED、session_ids（HoH 会话与原生会话）、review_id、attempt_id、spec_revision、spec_sha256、git_commit、stdout_sha256 为该 turn 原始 stdout 日志的 sha256）与报告（新的受锁契约 contracts/spec-review-report.schema.json：对象是规格修订而不是候选，没有 candidate_id，带 spec_revision、spec_sha256、git_commit，raw_session 与 native_attempt 为 FileRef）。两者都不进 independent_reports 与候选链，不开验证轮次，也不造候选；contracts/native-attempt.schema.json（CAND-* 必填）不用于规格审查，也不改。收取时 Runtime 逐项核对，任一不符即 state=REJECTED 并记原因：报告与原生记录各自符合 schema；raw_session 的 sha256 等于原始日志的字节，native_attempt 的 sha256 等于原生记录的字节；原生记录的 session_ids 含报告的 session_id，role、spec_revision、spec_sha256、git_commit 与报告的相同，stdout_sha256 等于 raw_session 的 sha256，status 为 COMPLETED；review_id、attempt_id、spec_revision、spec_sha256、git_commit 等于 spec_review.opened 与收取它的 Attempt；会话在 sessions 中，且与起草该修订的任何会话（以 spec.drafting 标记、结果成为该修订的 Task 的 Attempt）及 Commander 的会话都不是同一会话（REQ-053 的会话独立，按 CR-029 的身份规则）：HoH 会话 id 相同，或 (agent, 原生会话 id) 相同，都算同一会话，所以不同的 HoH 会话共用同一个原生底座会话也不独立；起草者与 Commander 一方取其当前绑定、绑定历史中的 Attempt 与 session.native 记录的全部身份，审查方取原生记录的 session_ids（排除审查方自己的 HoH id）与它自身已存储的原生身份；相同的原生 id 字符串属于不同 agent 时不构成冲突；发现的 id 不重复。全部相符即 state=CURRENT。写 spec_review.reported{review_id, status, state, reasons, session_id, attempt_id, report, raw_session, native_attempt}（后三者为 path 与 sha256）。
- 只阻断受影响的范围：一个 Task 的 readiness 只看它的 REQ 及其依赖闭包；不就绪的 Task 为 BLOCKED（READINESS_NOT_MET，details 列出原因与对应 id），其他就绪的 Task 照常派发，Run 不因此整体停下。
- 标为非阻塞的开放问题不阻断 readiness，列入 Run 的开放问题清单（投影与 Timeline 可查）。
- Run 只在 readiness 通过后从 SPECIFYING 进 READY，并写 readiness.passed{spec_revision}；规格修订变化时对受影响范围重新判定（REQ-033），并对新修订重新做独立规格审查。

**失败与边界**

- 范围内 required REQ 缺 AC 时，该 REQ 的 Task 为 BLOCKED（READINESS_NOT_MET，details.reasons 含 missing_ac 与 REQ id），没有该 Task 的 Attempt。
- 计划依赖成环时，计划被拒（DAG_CYCLE），不导入任何 Task。
- Commander 在 readiness 未过时提出 transition_run 到 READY，决策 REJECTED（ILLEGAL_TRANSITION，details 写 readiness 原因），Run 不变。
- 独立规格审查为 FAIL、UNKNOWN、缺失、非独立会话、绑定旧修订，或为带 critical 或 high 发现的 PASS 时，readiness 为 NOT_READY(spec_review)（后者 details 为 blocking_findings 并列出发现的 id），Run 不进 READY。
- 另一个 HoH 会话出具的规格审查，其原生记录的 session_ids 含起草该修订的会话所用的同一 (agent, 原生会话 id) 时，同样是非独立会话（not_independent，CR-029）；同一原生 id 字符串属于另一个 agent 时不因此判为非独立。
- 规格审查的原生记录缺失、不符合 spec-review-attempt 契约，或与报告、spec_review.opened、收取它的 Attempt 不一致时，报告为 REJECTED，spec_review 不满足。

**验收**

- **AC-031-01**（TEST-031-01 · 检查 `unit`）Given 五个夹具（规格加计划，独立规格审查均为同修订独立会话的 PASS）：齐全；required REQ 缺 AC；AC 缺 then；依赖成环；开放问题中一条阻塞问题未决；When 对每个夹具调用 readiness 判定；Then 齐全的为 READY；其余依次为 NOT_READY 且 reasons 恰为 missing_ac、unobservable、DAG_CYCLE、undecided，并各自给出对应的 REQ、AC 或问题 id
  - 判定：齐全者 READY；其余四个的 reasons 恰为 missing_ac、unobservable、DAG_CYCLE、undecided 并含对应 id
- **AC-031-02**（TEST-031-02 · 检查 `integration`）Given 已批准修订中需求组 A（齐全）与 B（一条 required REQ 缺 AC），规格审查 PASS，计划含 A 的 Task TA 与 B 的 Task TB，Run 处于 RUNNING；When 运行三轮调度；Then TA 被派发；TB 为 BLOCKED、blocked_reason=READINESS_NOT_MET 且 details.reasons 含 missing_ac 与该 REQ id，TB 没有 Attempt；Run 仍为 RUNNING
  - 判定：TA 被派发；TB BLOCKED 且 READINESS_NOT_MET，details 含 missing_ac 与 REQ id；TB 无 Attempt；Run 仍 RUNNING
- **AC-031-03**（TEST-031-03 · 检查 `unit`）Given 规格的开放问题中有一条标为“阻塞：否”的未决问题，其余齐全，规格审查 PASS；When 调用 readiness 判定并读 Run 的投影；Then 判定为 READY；该问题出现在 Run 的开放问题清单中，带其 id 与“非阻塞”标记
  - 判定：READY；清单含该问题 id 与非阻塞标记
- **AC-031-04**（TEST-031-04 · 检查 `integration`）Given 项目 A 的 Run R 处于 SPECIFYING，已批准修订 1 不就绪（missing_ac）；项目 B 的 Run R2 采用修订 1、处于 VERIFYING（候选 C 在验证中）；When Commander 提出 R 的 transition_run 到 READY；随后用户批准项目 A 补齐 AC 的修订 2，Runtime 派发它的规格审查，假 reviewer 在独立会话中给出 PASS；项目 B 的用户在 C 验证中批准修订 2 的 CR，假 reviewer 同样给出 PASS；随后用户批准项目 A 的修订 3，在它的规格审查上经同一收取入口（测试替换 host 构造的原生记录）依次送入六份 PASS 报告：原生记录的 review_id、attempt_id、stdout_sha256、spec_sha256、git_commit 各被换成别的值一次，另一份没有原生记录；再让一次审查 Attempt 以 FAILED 的 TaskResult 结束；最后假 reviewer 给出合法的 PASS；Then 第一次决策 REJECTED（ILLEGAL_TRANSITION，details 含 missing_ac），R 仍为 SPECIFYING；修订 2 批准后 Runtime 写恰一条 spec_review.opened（spec_revision=2，git_commit 为修订 2 的规格提交）并派发恰一个 reviewer 的规格审查 Attempt，其会话不同于 Commander 与起草修订 2 的会话；报告经 spec-review-report 契约收取为 spec_review.reported{status:PASS, state:CURRENT}，带报告、原始会话与原生记录的 path 与 sha256；原生记录 record_type=runtime_spec_review_export，review_id、attempt_id、spec_revision=2、spec_sha256、git_commit 与 spec_review.opened 相同，session_ids 含报告的会话，stdout_sha256 等于原始会话的 sha256；independent_reports 与 candidates 没有新行；PASS 之前 R 不进 READY，之后写 readiness.passed{spec_revision:2}，R 进入 READY。项目 B：R2 的状态事件中没有 SPECIFYING，它的 spec_review.opened 写于批准之后、R2 处于 VERIFYING 或 RUNNING 时；C 按 REQ-033 置 STALE；审查 PASS 之前项目 B 修订 2 下没有写派发。修订 3 的拒收：六份报告都收取为 spec_review.reported{state:REJECTED}，reasons 依次指出 review_id、attempt_id、stdout_sha256、spec_sha256、git_commit 不符与缺少原生记录；FAILED 的 Attempt 没有导出原生记录与报告，也没有 spec_review.reported；合法的 PASS 之前修订 3 的 readiness 每次都为 NOT_READY(spec_review)，合法的 PASS 之后为 READY（正对照）
  - 判定：首个决策 REJECTED 且 details 含 missing_ac；恰一条 spec_review.opened 与一个规格审查 Attempt，会话独立；spec_review.reported 为 PASS、CURRENT 且带报告、原始会话与原生记录引用；原生记录绑定 review、attempt、修订、摘要与提交，会话与 stdout 摘要相符；independent_reports 与 candidates 无新行；项目 B：没有 SPECIFYING，审查在 VERIFYING 或 RUNNING 中派发，C 为 STALE，PASS 前无写派发；修订 3：五个绑定各篡改一次与缺少原生记录都为 REJECTED 且 reasons 指出该项，FAILED 的结果不导出，这些之后 readiness 不满足，合法 PASS 之后满足；PASS 之前不进 READY；readiness.passed{spec_revision:2} 后 Run 为 READY
- **AC-031-05**（TEST-031-05 · 检查 `unit`）Given 已批准修订 2 齐全；十一份规格审查记录：同修订独立会话 PASS；同修订 FAIL；同修订 UNKNOWN；没有记录；起草修订 2 的会话出具的 PASS；绑定修订 1 的 PASS；带一条 high 发现的 PASS；带一条 critical 发现的 PASS；只带 medium 与 low 发现的 PASS；另一个 HoH 会话出具、其原生记录的 session_ids 含起草修订 2 的会话所用的同一 (agent, 原生会话 id) 的 PASS；原生记录含与起草会话相同的原生 id 字符串、但属于另一个 agent 的 PASS（CR-029）；When 对每份记录各调用一次 readiness 判定；Then 第一份、第九份与第十一份得到 READY；第二至第八份与第十份为 NOT_READY 且 reasons 含 spec_review，details 依次为 fail、unknown、missing、not_independent、stale_revision、blocking_findings（列出那条 high 的 id）、blocking_findings（列出那条 critical 的 id）、not_independent
  - 判定：同修订独立会话 PASS、只带 medium、low 发现的 PASS，与原生 id 字符串相同但属于另一个 agent 的 PASS 为 READY；其余 reasons 含 spec_review 且 details 依次为 fail、unknown、missing、not_independent、stale_revision、blocking_findings、blocking_findings、not_independent，两条 blocking_findings 列出发现 id；另一个 HoH 会话而原生记录含起草会话的同一 (agent, 原生会话 id) 的 PASS 为 not_independent

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- readiness 由 Runtime 自动判定，Commander 只能补规格或计划（CR-026，C5）。
- 阻塞性的标记写在规格的开放问题条目里（每条带“阻塞：是/否”），缺省为阻塞（CR-026，C4）。
- 独立规格审查由 reviewer 职位在独立会话中做，属 readiness 的输入；对象是规格修订：报告与原生记录用两个新的受锁契约 spec-review-report.schema.json 与 spec-review-attempt.schema.json，保留会话、原始输出摘要与真实完成状态的核对；审查的开启与收取是事件（spec_review.opened、spec_review.reported），不用 independent_reports 与验证轮次（二者的候选外键与 round_of_task 只适用于候选），也不为规格造候选（CR-026，CR26D-05、-101、-200）。
- spec_review 只由没有 critical 或 high 发现的 PASS 满足：规格审查没有 Issue 的生命周期，发现由下一次修订回应，修订之后重新审查（CR-026，CR26D-202）。
- 规格审查与起草一样在 Run 的工作状态中派发，不改生命周期（CR-026，CR26D-201）。
- 新码 READINESS_NOT_MET 为 BlockedReason（Task 的 blocked_reason）（CR-026）。

## REQ-032 · 需求追踪

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/10-acceptance.md`、`docs/05-data-api.md`、`docs/adr/ADR-027.md`、`docs/adr/ADR-029.md`、`docs/cr/CR-017-same-content-refreeze-in-one-run.md`

REQ→TASK→candidate→TEST→EVIDENCE→acceptance 可双向查找

**必须行为**

- 产品为用户项目维护追踪链 REQ→AC→TASK→candidate→TEST→EVIDENCE→acceptance 结果，任一节点可双向查到完整链（C8：产品功能；本仓库的 traceability 工具以仿本校验产品的导出）。
- 断链报为 broken 并指出缺哪一环：TEST 没有证据、证据没有候选、AC 没有 TEST、Task 没有 REQ。
- 孤立验收报为 orphan：PASS 绑定的不是当前候选或当前规格修订。
- API 与 DB 一致：/v1/runs/{id}/acceptance 为每条 AC 给出候选、TEST 与证据引用，/v1/runs/{id}/tasks 为每个 Task 给出 REQ；产品不另造第二份追踪事实。
- 规格修订变化后（O6），旧修订下的全部验收节点标为 stale（不只受影响的链），与 REQ-033 的验收失效一致；历史可查，不静默沿用。
- 追踪的输入全部取自候选的 spec 文件：需求取它的 requirements，TEST→check 取它的 scenarios（用户项目的 docs/spec/index.json，REQ-034 行为 2），用户项目以发布目标 project 为档；scripts/product/traceability.py 的 World 与产品的追踪导出用同一读取。只有 spec 文件没有 scenarios 键时（HoH 自身仓库的 contracts/requirements.json 布局）才读 contracts/acceptance-scenarios.json；没有 contracts/ 目录的项目照常批准、冻结与追踪。

**失败与边界**

- 证据被删或缺候选时，追踪报告列为 broken 并指明缺失环节，完成门禁不计该 AC 为 accepted。
- AC 的 PASS 绑定旧候选或旧修订时，报为 orphan，不计入当前进度。
- spec 文件的 scenarios 缺某个 TEST 或它的 check 时，该 TEST 报为 broken（缺 check），不回退去读项目的 contracts/。

**验收**

- **AC-032-01**（TEST-032-01 · 检查 `traceability`）Given 夹具项目经产品运行得到的追踪导出：2 条 REQ、4 条 AC、3 个 Task、1 个候选；测试再删去一条 TEST 的证据，并把一条 AC 的 PASS 改绑旧候选；When 以 traceability 检查运行 tests/bundle 中名称含 test_032_01 的测试，它调用产品的导出并读取追踪报告；Then 任一节点（REQ、AC、Task、候选、TEST、证据、结果）都能正反两个方向查到完整链；没有证据的 TEST 列在 broken 中并指出缺 evidence；绑定旧候选的 PASS 列在 orphan 中；其余链完整
  - 判定：每个节点正反可达；无证据的 TEST 在 broken 中且指出缺 evidence；绑定旧候选的 PASS 在 orphan 中
- **AC-032-02**（TEST-032-02 · 检查 `integration`）Given 产品 host 的 Run 有 3 个 Task 与 4 条 AC 的验证结果；When 读取 /v1/runs/{id}/acceptance 与 /v1/runs/{id}/tasks 的全部分页；Then 每条 AC 的候选、TEST 与证据引用，以及每个 Task 的 REQ，与直接查询 DB 的结果逐项相等
  - 判定：两份清单逐项相等
- **AC-032-03**（TEST-032-03 · 检查 `integration`）Given 修订 1 的追踪链完整，REQ-A 与 REQ-B 都已验收；批准只改写 REQ-A 的修订 2；When 再次导出追踪报告；Then REQ-A 与未改动的 REQ-B 两条链上的验收节点都标为 stale，历史结果仍可查；修订 2 下没有节点沿用修订 1 的 PASS
  - 判定：REQ-A 与 REQ-B 的验收节点都 stale；历史可查；无沿用修订 1 的 PASS
- **AC-032-04**（TEST-032-04 · 检查 `integration`）Given 一个没有 contracts/ 目录的夹具 git 项目：docs/spec/ 下两份 Markdown 共 2 条 REQ、3 条 AC（check 行依次为 unit、unit、integration）与生成的 index.json，tests/check-plan.json 列出 unit 与 integration；项目还没有已批准修订；When 用户以 ApproveSpec 批准，经产品冻结一个候选，随后对该候选运行追踪（scripts/product/traceability.py 的 World，档 project），并读取产品对同一候选的追踪导出；Then 候选的 spec 为指向 docs/spec/index.json 的单个 FileRef；World.tests() 恰得到 3 个 TEST，check 依次为 unit、unit、integration，等于 index.json 的 scenarios，也等于 acceptance_criteria.check_id；没有成员证据时 3 个 TEST 都是 MISSING（ADR-027 原则一：场景检查没有任何成员时的状态；与 NOT_RUN 一样是无结论、未满足）而不是错误；追踪与导出期间没有打开项目根下 contracts/ 中的任何路径；产品导出的 TEST→check 与 World 的逐项相同
  - 判定：candidate.spec 为指向 index.json 的单个 FileRef；3 个 TEST 的 check 依次为 unit、unit、integration 且等于 scenarios 与 acceptance_criteria.check_id；无成员证据时为 MISSING（不是错误）；未打开项目根下 contracts/ 的路径；产品导出与 World 逐项相同

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- REQ-032 是产品功能；TEST-032-01 的测试在 tests/bundle、名称含 test_032_01，以 traceability 检查校验产品当前导出的追踪报告（运行产品导出，再注入缺证据与旧候选），不以手写的导出代替（CR-017，CR-026，C8）。
- M3 的追踪入口是 API（Page 的 items 为任意对象，ADR-029）与 TUI 的状态摘要；Web 视图属 M6（CR-026，C9）。
- 追踪输入随冻结形式：用户项目的 TEST→check 只来自候选 spec 文件 index.json 的 scenarios，contracts/acceptance-scenarios.json 只用于没有 scenarios 键的 spec 文件（CR-026，CR26D-102）。

## REQ-033 · 规格变更

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/21-spec-governance.md`、`docs/07-algorithms.md`、`docs/06-orchestration.md`、`docs/03-ux.md`、`docs/adr/ADR-028.md`、`docs/adr/ADR-021.md`、`docs/10-acceptance.md`、`contracts/sql/001_initial.sql`

生成 CR、revision、影响闭包；受影响验收过期，不静默沿用旧PASS

**必须行为**

- 规格变更都走 CR：ProposeSpecChange（或 CHANGE 输入、或起草 Task 的完成，见 REQ-008）以当前已批准修订为 base，写一条 change_requests 行（PROPOSED），reason 为提议理由，impact_json 列出影响闭包；批准之前规格、Run 的 spec_id 与任务都不变。
- 影响闭包按显式映射计算：被改动的 REQ（增删改）→ 其 AC → 其 TEST/check → 以 task_requirements 关联的 Task；不按文件路径猜测（C2）。
- 批准（ApproveSpec，只由用户，O1）写新修订（revision+1）、CR 为 APPLIED、result_spec_id 指向新行，写 spec.changed{cause:"changed", from, to, cr_id}；REJECT 把 CR 置为 REJECTED，什么也不改。
- 任何修订都使该 Run 全部验收结果失效（O6）：新修订下没有任何 accepted，直到在新冻结的候选上重新验收；旧结果保留为历史，显示为 STALE，不计入当前进度与完成门禁。规格摘要是 index.json 的 sha256（REQ-034），任一 Markdown 文件的任一字节变化都产生新修订与新摘要。
- 验证进行中规格变化时分两步：批准事务内，验证中的候选写 candidate.stale{candidate_id, reason:"spec_changed"}、它的独立报告翻转为 STALE，并对其验证 Attempt 请求取消（cancel_requested）；轮次只在这些验证进程（含其 run-check 子进程）经取消阶梯证明已结束之后，以 superseded 关闭恰好一次（ADR-028 决定 11，closing.rs 的关轮规则）。取消之后到达的验证输出只记录为迟到，不计入任何结果；host 在其间崩溃时，重启的 reconcile 先证明进程已结束，再恰好一次关轮。
- 影响闭包内、以 SPEC_MISMATCH 阻塞的写 Task，批准后迁移：仍有对应 REQ 的，task_requirements 改映射到新修订的 REQ 并写 task.migrated；对应 REQ 被删除的，Task 取消、由 Commander 计划的替代 Task 承接并写 task.replaced{from, to}。任何 Task 都不再在旧修订下派发。

**失败与边界**

- 批准前，规格、runs.spec_id、tasks 与 acceptance_results 不变。
- 以修订 n 的证据判修订 n+1 的 AC 时，投影与完成门禁（EVIDENCE_GATE_UNMET）都不计为 accepted，显示 STALE；已验收计数的分母随修订变化并显示 spec revision。
- CR 的 base 不是当前已批准修订时（期间已有别的批准），ApproveSpec 返回 409 SPEC_CONFLICT，CR 保持 PROPOSED。
- 验证进程尚未证明结束时轮次不关闭；它们的迟到输出不改变任何验收结果。
- 被删 REQ 的写 Task 没有替代时，它保持 CANCELLED，不在旧修订下重新派发。

**验收**

- **AC-033-01**（TEST-033-01 · 检查 `integration`）Given 已批准修订 1 含 REQ-A（AC-A-01、AC-A-02，各有 TEST 并由 Task TA 关联）与 REQ-B；用户把 REQ-A 的 AC-A-02 改写；When 以 ProposeSpecChange{reason, spec_sha256=新摘要} 提议，查看 CR，然后以 ApproveSpec{decision:APPROVE_ONLY} 批准；Then 批准前 change_requests 一行 PROPOSED、base_spec_id=修订 1、impact_json 恰含 REQ-A、AC-A-01..02、它们的 TEST 与 TA，不含 REQ-B 的任何项，specs 与 runs.spec_id 不变；批准后 specs 新增修订 2、CR 为 APPLIED 且 result_spec_id=修订 2、写 spec.changed{cause:"changed", from:1, to:2}
  - 判定：批准前 CR 为 PROPOSED，impact_json 恰为 REQ-A 的闭包；批准前 specs 与 runs.spec_id 不变；批准后修订 2、CR APPLIED、spec.changed{cause:changed}
- **AC-033-02**（TEST-033-02 · 检查 `recovery`）Given Run R 的候选 C 在修订 1 下 VERIFYING：QA 的验证 Attempt 正在跑一个需时 60 s 的 run-check，Review 的验证 Attempt 正在运行；When 批准修订 2；QA 的假 Agent 在取消之后仍写出一份 PASS 报告；再在另一次运行中，于批准提交之后、验证进程结束之前 kill -9 host 并重启；Then 批准事务内写 candidate.stale{candidate_id:C, reason:"spec_changed"}、C 的独立报告为 STALE、两个验证 Attempt 为 cancel_requested，轮次此时仍未关闭；两个验证进程与 run-check 子进程经取消阶梯证明结束之后，轮次以 superseded 关闭恰好一次；迟到的报告不产生任何 accepted；重启的一次也在 reconcile 证明进程结束后恰好一次关轮；修订 2 下 accepted AC 数为 0
  - 判定：批准事务内 candidate.stale{reason:spec_changed}、报告 STALE、两个 Attempt cancel_requested，轮次未关；进程与 run-check 子进程结束证明之后轮次 superseded 恰一次；迟到报告无 accepted；重启后 reconcile 证明结束再恰一次关轮；修订 2 下 accepted 为 0
- **AC-033-03**（TEST-033-03 · 检查 `integration`）Given 修订 1 下 REQ-A、REQ-B 的全部 AC 已 PASS 并 accepted；修订 2 与修订 1 只差 REQ-A 所在 Markdown 文件中的一个字符；When 批准修订 2，读 /v1/runs/{id}/acceptance 与 Run 的投影，随后 Commander 以 transition_run 申请 COMPLETED；Then 修订 2 的摘要（index.json 的 sha256）与修订 1 不同；两者都显示修订 2 的全部 AC（含未改动的 REQ-B）为未验收，旧结果标 STALE 并可查；已验收计数为 0/修订 2 的 AC 总数并显示 spec revision 2；完成申请被拒（EVIDENCE_GATE_UNMET），Run 不是 COMPLETED
  - 判定：两个修订的摘要不同；修订 2 的全部 AC 未验收、旧结果 STALE；计数为 0/总数并显示 revision 2；完成申请被拒（EVIDENCE_GATE_UNMET），Run 不是 COMPLETED
- **AC-033-04**（TEST-033-04 · 检查 `integration`）Given 修订 1 下写 Task T1（REQ-A）与 T2（REQ-C）以 SPEC_MISMATCH 阻塞；修订 2 改写 REQ-A、删除 REQ-C；When 批准修订 2，Commander 为 REQ-C 的去留计划替代 Task T3，调度轮运行；Then T1 的 task_requirements 改指修订 2 的 REQ-A 并写 task.migrated，T1 可派发；T2 为 CANCELLED 并写 task.replaced{from:T2, to:T3}；之后没有任何 Task 以修订 1 的 spec_id 派发
  - 判定：T1 映射到修订 2 并 task.migrated，可派发；T2 CANCELLED 并 task.replaced{from:T2,to:T3}；无修订 1 的派发

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 证据失效的粒度取所有者 2026-11-17 的决定 O6：任何修订使全部验收失效；影响闭包只决定要做的工作，不决定保留哪些 PASS；验收在新冻结的候选上重跑全部 required 检查（与 docs/07 §6 的发布全量一致）（CR-026）。
- 影响闭包按 REQ→AC→TEST→TASK 的显式映射，不按路径（CR-026，C2）。
- CR 的记录是 change_requests 行；提议的规格内容就是工作树中的规格文件与索引，以摘要锁定，不另写 CR 文件（CR-026，C3）。
- 验证中的规格变化先失效与请求取消、后关轮，关轮等进程结束证明（ADR-028 决定 11）（CR-026，CR26D-03）。

## REQ-034 · 文档事实源

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/05-data-api.md`、`docs/21-spec-governance.md`、`docs/15-prompts-skills-memory.md`、`docs/adr/ADR-028.md`、`docs/adr/ADR-031.md`、`contracts/candidate.schema.json`、`contracts/sql/001_initial.sql`

文件是语义事实源，DB是索引/状态；冲突可检测；生成页不产生第二权威源

**必须行为**

- 用户项目的规格是 Git 工作树中规格根 docs/spec/ 下的 Markdown 文件（O4）：固定一级标题（用户、范围、非目标、决策、开放问题、需求），需求为“## REQ-<id> <标题>”，验收为“### AC-<req>-<nn>”并带 given、when、then、check 四个 id 行；精确语法由 M3-T02 的 ADR 规定，HoH 内置模板给出（REQ-029）。
- 规格的冻结形式是生成的规格索引 docs/spec/index.json（随规格一起提交、被 Git 跟踪）：由 HoH 从规格根的 Markdown 文件确定性生成的规范 JSON（UTF-8、LF、键按字典序、两空格缩进、末尾一个换行），含 generated 标记、files（每份 Markdown 文件的路径与 sha256，按路径排序）、requirements（与 contracts/requirements.json 中需求对象同形的需求与验收；每条 AC 带 test_id=TEST-<req>-<nn>；追踪读取的 release_class 取需求的 class 行，缺省 required，release_targets 恒为 ["project"]：用户项目只有一个发布目标）与 scenarios（每条 AC 一项 {id: 它的 test_id, requirement_id, acceptance_id, check_id}，check_id 取该 AC 的 check 行，按 id 排序）。规格摘要就是这个文件字节的 sha256；任一 Markdown 文件的任一字节变化都改变 files 中的摘要，从而改变规格摘要。
- 已批准修订是经 ApproveSpec 批准、sha256 为该修订规格摘要的 specs 行；specs 只有已批准修订（M2T13D-59，ADR-031 决定 1），只有已批准修订被采用、派发、冻结与验收引用。DB 只是索引与状态：已批准修订的 requirements、acceptance_criteria 行由 HoH 从该修订的索引写入，acceptance_criteria.check_id 取 scenarios 中该 AC 的 check_id。
- ApproveSpec 的 spec_sha256 必须等于 HoH 在提交时从工作树重新生成的索引的 sha256，且工作树中已跟踪的 index.json 与之逐字节相同；不等即冲突（SPEC_CONFLICT），什么也不写。
- 冻结与验证以 index.json 为候选的 spec 文件（candidate.spec 为单个 FileRef，candidate.spec_sha256 为它的 sha256）：冻结时 HoH 从该提交的 Markdown 重新生成索引并要求与提交中的 index.json 逐字节相同，否则拒绝冻结（SPEC_INVALID）；traceability 从它的 requirements 读需求、从它的 scenarios 读 TEST→check（REQ-032）。它替换 M2 的约定路径 contracts/requirements.json（ADR-028 决定 4，CR-018），用户项目不需要 contracts/ 目录。
- 索引在提议与批准时构建；Markdown 无法解析时返回 422 SPEC_INVALID 并写 spec.index_failed{path, line, error}，不写 change_requests 也不写新修订，派发依据仍是上一已批准修订，禁止基于半更新的索引派发。
- index.json 等生成页带“生成，勿改”标记与来源摘要，不是事实源（Markdown 才是）；生成时发现它被人工修改，写 spec.generated_drift{path} 并按 Markdown 重新生成。

**失败与边界**

- ApproveSpec 的 spec_sha256 与重新生成的索引不同，或已跟踪的 index.json 与之不同，返回 409 SPEC_CONFLICT，details 含 expected 与 actual 两个摘要，specs、requirements、acceptance_criteria 与 runs 都不变。
- Markdown 无法解析时，索引不变（仍为上一已批准修订），写 spec.index_failed，不派发依据未批准内容的任务。
- 提交中的 index.json 与其 Markdown 不一致时拒绝冻结（SPEC_INVALID），没有候选。
- 规格根不存在或没有已批准修订时，项目视为没有规格（StartRun 以 runtime:no_spec 阻塞，runs.spec_id 为空，ADR-031 决定 1），不读任何约定路径。

**验收**

- **AC-034-01**（TEST-034-01 · 检查 `integration`）Given 夹具项目 docs/spec/ 下两份 Markdown 规格共 3 条 REQ、7 条 AC，与 HoH 生成的 docs/spec/index.json 一起提交于 C1；项目尚无已批准修订；When 用户以 ApproveSpec{spec_sha256=index.json 的 sha256, decision:APPROVE_ONLY} 批准，随后冻结一个候选；Then specs 中有 revision=1 的已批准修订，sha256 等于 index.json 字节的 sha256，index.json 的 files 列出两份 Markdown 的路径与 sha256；requirements 3 行、acceptance_criteria 7 行且 id 与文件一致；index.json 的 scenarios 有 7 项，每项的 check_id 等于对应 AC 的 check 行，acceptance_criteria.check_id 与之逐条相同；candidate.spec 为 {path:docs/spec/index.json, sha256} 且 candidate.spec_sha256 与之相同；冻结进程未打开 contracts/requirements.json
  - 判定：specs 1 行且 sha256 等于 index.json 的 sha256；index.json 的 files 列出两份 Markdown；requirements 3 行、acceptance_criteria 7 行；scenarios 7 项且 check_id 与 AC 的 check 行及 acceptance_criteria.check_id 相同；candidate.spec 与 spec_sha256 指向 index.json；未打开 contracts/requirements.json
- **AC-034-02**（TEST-034-02 · 检查 `integration`）Given 已批准修订 1（摘要 S1）；用户把一份 Markdown 改动后重新生成索引（摘要 S2），未提出变更；When 以 ApproveSpec{spec_sha256=S1} 提交批准；Then 返回 409，code=SPEC_CONFLICT，details 含 expected=S1 与 actual=S2；specs、requirements、acceptance_criteria 与 runs 行逐值不变；工作树文件不变
  - 判定：HTTP 409 且 code=SPEC_CONFLICT，details 含两个摘要；四张表逐值不变；工作树文件不变
- **AC-034-03**（TEST-034-03 · 检查 `integration`）Given 已批准修订 1 的 Run R 处于 RUNNING、有 READY 的 Task；用户把一份规格文件改坏（AC 缺 then 行）；When 以 ProposeSpecChange 提议，调度轮再运行三轮；随后修好文件，经 ProposeSpecChange 与 ApproveSpec 批准；Then 坏文件的提议返回 422 SPEC_INVALID、写 spec.index_failed{path, line, error}，没有 change_requests 行与新的已批准修订，三轮内派发依据仍是修订 1；修好后经 CR 得到修订 2 的已批准修订与重建的索引
  - 判定：提议返回 422 SPEC_INVALID；spec.index_failed 一条且载荷含 path、line、error；无 change_requests 行与新修订；三轮派发依据为修订 1；修好后修订 2 与新索引
- **AC-034-04**（TEST-034-04 · 检查 `unit`）Given 已批准修订 1，已跟踪的 docs/spec/index.json 带“生成，勿改”标记与来源摘要；When 用户手工改 index.json 中一条 AC 的文字并提交，随后先冻结一个候选、再触发生成；Then 冻结被拒（SPEC_INVALID，details 指出 index.json 与 Markdown 不一致），没有候选；生成时写 spec.generated_drift{path}，index.json 恢复为按 Markdown 生成的内容；specs、requirements、acceptance_criteria 不变
  - 判定：冻结被拒为 SPEC_INVALID 且无候选；spec.generated_drift 一条；index.json 恢复并带来源摘要与标记；三张表不变

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 规格格式与位置取所有者 2026-11-17 的决定 O4：Markdown 放 docs/spec/，固定标题与 id 行，HoH 生成 JSON 索引（CR-026）。
- 冻结形式取生成的 index.json：多份 Markdown 在候选中由一个 FileRef 表示，候选、验证与追踪的受锁契约都不需要改（candidate.schema.json 的 spec 仍是单个 FileRef；索引的 requirements 与 contracts/requirements.json 同形）（CR-026，CR26D-02）。
- 索引带 scenarios（TEST→check）：acceptance_criteria.check_id 与追踪都从它读，候选仍只有一个 spec FileRef，spec_sha256 覆盖它（CR-026，CR26D-102）。
- 冲突以新码 SPEC_CONFLICT（409）报告，不复用 CONFLICT：后者指命令 id 冲突（CR-026，C1）。
- 生成页漂移时以 Markdown 为准覆盖并记录，不尝试合并人工修改（CR-026）。

## REQ-035 · 双层编排

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/07-algorithms.md`、`docs/05-data-api.md`、`docs/14-resilience.md`、`contracts/sql/001_initial.sql`、`contracts/protocol.schema.json`、`tools/reference_model.py`、`docs/adr/ADR-003.md`

Commander只提交带role_epoch与expected_revision的决策提议，Runtime确定性校验依赖、epoch、预算、角色能力与前置版本后原子执行或拒绝；常规阶段自动流转无需Commander批准。

**必须行为**

- 必须把Commander输出持久为decisions行（state=PROPOSED，含role_epoch、expected_revision、payload），再由Runtime校验后置APPLIED或REJECTED；Agent与UI不得直接修改tasks/runs。
- 派发提议必须同时满足：依赖Task全为ACCEPTED、role_epoch等于当前、expected_revision等于当前、spec revision为当前、目标角色绑定满足能力、预算可预留；任一不满足即REJECTED并附原因码。
- APPLIED必须在同一事务内完成：decision置APPLIED、目标Task/Run按迁移表CAS更新、event追加、dispatch outbox插入。
- expected_revision过期的决策不得apply：记为REJECTED历史，并以新snapshot重新唤醒Supervisor重算。
- candidate_ready等常规迁移必须由Runtime自动派发QA与Review；只有need_decision、critical/high issue、no_progress、预算风险、角色失效、规格变更、milestone_ready、release_request唤醒Commander。
- Commander的自然语言结论不得改变验收状态或把Run置COMPLETED；验收只由证据门禁判定。
- Commander不可用时，已授权低风险动作（常规QA/Review派发、已调度重试、pause/cancel、状态保存）继续；需要新重大决策时Run进入BLOCKED，不无限等待。

**失败与边界**

- 当提议派发依赖未ACCEPTED的Task时，decision=REJECTED、reason=DEPENDENCY_NOT_ACCEPTED，Task状态不变，outbox无新行。
- 当提议来自旧role_epoch（交接前的Commander）时，reason=STALE_ROLE_EPOCH，只存历史，不改变任何Task。
- 当提议的迁移不在迁移表内（如Task RUNNING→ACCEPTED）时，reason=ILLEGAL_TRANSITION，对应命令返回409。
- 当预算不可预留或目标角色缺能力时，分别reason=BUDGET_NOT_RESERVABLE/ROLE_CAPABILITY_MISSING，不转派给缺能力Agent。
- 当Commander不可用且出现需决策事件时，Run=BLOCKED且原因=COMMANDER_UNAVAILABLE，不存在伪造的Commander decision。

**验收**

- **AC-035-01**（TEST-035-01 · 检查 `unit`）Given Run R(revision=7, role_epoch=3, spec revision=2)；T1 READY且依赖T0(ACCEPTED)；T2依赖T3(RUNNING)；T4 cost=3而剩余预算=1；qa绑定缺T5所需能力。；When FakeCommander依次提交6个提议：派发T2；role_epoch=2派发T1；expected_revision=6派发T1；派发T4；派发T5给qa；将T1由READY直接置ACCEPTED。；Then 6个decision均REJECTED，reason依次匹配6个提议（依赖、role_epoch、revision、预算、能力、非法迁移）；tasks/runs的state与revision不变；outbox与attempts新增0行。
  - 判定：6个decision均为REJECTED，reason_code依次为DEPENDENCY_NOT_ACCEPTED、STALE_ROLE_EPOCH、STALE_REVISION、BUDGET_NOT_RESERVABLE、ROLE_CAPABILITY_MISSING、ILLEGAL_TRANSITION；每个拒绝恰对应1条拒绝事件；tasks/runs的state与revision前后逐字段相同；outbox与attempts新增行数=0
- **AC-035-02**（TEST-035-02 · 检查 `integration`）Given 文件SQLite：Run R(revision=7, role_epoch=3)；T1 READY，依赖全ACCEPTED，预算充足，developer绑定具备所需能力，writer租约空闲。；When FakeCommander提交expected_revision=7、role_epoch=3的「派发T1」提议，随后原样再提交一次。；Then 首个提议在单一事务内：decision=APPLIED、T1=LEASED且revision+1、leases新增writer行、events+1、outbox+1且其event_seq指向该事件；第二次提交reason=STALE_REVISION。
  - 判定：decision1.state=APPLIED，T1.state=LEASED，T1.revision=原值+1；leases(project_id,'writer')恰1行且attempt_id属于T1；新增event 1条、outbox 1条，outbox.event_seq=该event.seq；decision2.state=REJECTED且reason=STALE_REVISION
- **AC-035-03**（TEST-035-03 · 检查 `integration`）Given Run R中Developer Attempt（FakeAgent）产出冻结candidate C1；QA与Review角色绑定可用。；When Runtime处理candidate_ready事件。；Then QA与Review两条kind=handoff outbox行在同一事务内生成且candidate_id均为C1；FakeCommander调用次数=0。
  - 判定：outbox新增2行，recipient_role分别为qa与reviewer，payload.candidate_id=C1；两行event_seq相同（同一事务）；FakeCommander调用次数=0
- **AC-035-04**（TEST-035-04 · 检查 `unit`）Given Run R处于VERIFYING，C1无evidence与acceptance_results。；When FakeCommander以当前role_epoch与revision提交决策：「所有AC已通过，标记REQ已验收并将Run置COMPLETED」。；Then decision=REJECTED、reason=EVIDENCE_GATE_UNMET；Run仍VERIFYING；acceptance_results行数=0。
  - 判定：decision.state=REJECTED，reason=EVIDENCE_GATE_UNMET；runs.state=VERIFYING，revision不变；acceptance_results行数=0
- **AC-035-05**（TEST-035-05 · 检查 `integration`）Given FakeCommander设为不可用（启动即失败）；Run R RUNNING，C1已冻结；同时存在一个critical issue需决策。；When Runtime处理candidate_ready与该critical issue事件。；Then QA与Review仍被派发（qa与reviewer各1条kind=handoff消息）；Run进入BLOCKED且原因=COMMANDER_UNAVAILABLE；sender为Commander的APPLIED decision新增0行。
  - 判定：outbox含qa与reviewer的C1 kind=handoff消息各1行；runs.state=BLOCKED且阻塞原因=COMMANDER_UNAVAILABLE；新增APPLIED decision中Commander来源行数=0

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 拒绝原因码（DEPENDENCY_NOT_ACCEPTED、STALE_*、BUDGET_NOT_RESERVABLE、ROLE_CAPABILITY_MISSING、ILLEGAL_TRANSITION、EVIDENCE_GATE_UNMET、COMMANDER_UNAVAILABLE）为确认值；M1-T01 实现领域类型时集中写入 protocol.schema.json 枚举（随该任务 CR 重锁）。
- decisions 表已含 reason_code 列（CR-001 SQL），原因码不只写在 event 中。
- 「已授权低风险动作」文档未枚举；本选择仅含常规QA/Review派发、已调度重试、pause/cancel与状态持久化。
- （CR-003）Run=BLOCKED 表示存在只有外部才能解除的全局阻塞：停止 Commander 决策类派发（新的实现任务），已运行的 Attempt 继续到安全点，Runtime 自动规则（candidate_ready→QA/Review、已调度重试、pause/cancel、持久化）照常执行。runs.blocked_reason 为当前阻塞集合中优先级最高的一项（BLOCKED_AUTH > COMMANDER_UNAVAILABLE > BUDGET_UNENFORCEABLE > BLOCKED_CAPABILITY > NEED_DECISION），完整集合写入 runs.blocked_reasons_json；集合清空且无 pause/cancel 请求时回到 resume_state。
- （CR-003）Commander 提议的结构为 protocol.schema.json 的 $defs.DecisionPayload（dispatch、transition_task、transition_run、request_user_decision、create_tasks、record_assumption），计划校验输入为 $defs.PlanProposal；拒绝原因取 $defs.ReasonCode。

## REQ-036 · 持久生命周期

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/05-data-api.md`、`docs/14-resilience.md`、`docs/09-testing.md`、`docs/04-architecture.md`、`contracts/sql/001_initial.sql`、`contracts/protocol.schema.json`

Run/Iteration/Task/Attempt/Session以独立全局ID持久化，按单源迁移表推进；每次迁移在一个SQLite事务内完成revision CAS、事件追加与outbox插入，崩溃后无半提交。

**必须行为**

- Run/Iteration/Task/Attempt/Session必须使用互不复用的UUID类全局ID；排序与回放只依据events.seq，不依据ID或时间戳。
- Run、Task、Attempt迁移必须对照hoh-core单一迁移表校验，状态名集合与001_initial.sql CHECK枚举一致；非法迁移返回DomainConflict（ILLEGAL_TRANSITION）且不写事件。
- Run终态为COMPLETED/CANCELLED/FAILED且无出边；COMPLETED只能来自VERIFYING；PAUSED只来自PAUSE_REQUESTED；CANCELLED只来自CANCEL_REQUESTED；恢复（ResumeRun）先reconcile，该停止状态自己的停止条件不再成立时回到resume_state，阻塞集合非空时回到BLOCKED（CR-023）。
- 每次迁移必须在一个BEGIN IMMEDIATE事务内：UPDATE…WHERE id=? AND revision=expected（rowcount=1）、revision+1、events追加、所需outbox插入；rowcount=0则回滚并返回409与current_revision。
- 命令ACK、SSE广播与投影更新只能在COMMIT成功之后发生；日志文本可异步写，关键事件不得因内存队列满被丢弃。
- 重试只能新增Attempt行；旧Attempt保留原终态与raw_log_path，失败历史只追加。
- 每个Attempt必须绑定role_epoch、host_epoch、配置revision与session_id；QA与开发、Commander与Reviewer的Attempt必须使用不同session_id。

**失败与边界**

- 当expected_revision与当前不符时，UPDATE影响0行，事务回滚，返回409且current_revision为当前值，events/outbox无新增。
- 当进程在事务内任一点被kill -9时，重启后该迁移的state/event/outbox全有或全无。
- 当请求COMPLETED→RUNNING或FAILED Attempt→RUNNING等终态出边时，返回ILLEGAL_TRANSITION，状态不变。
- 当COMMIT失败（如SQLITE_FULL）时，客户端收到错误而非202，订阅者未收到该事件，投影不变。
- 当QA Attempt试图复用产出该candidate的开发session_id时，不创建Attempt并返回SESSION_NOT_INDEPENDENT。
- 当ResumeRun时该停止状态自己的停止条件仍成立（INCOMPLETE的预算或期限仍耗尽、WAITING_EXTERNAL没有到期的Task且仍有Task在等待next_retry_at、BLOCKED的阻塞集合非空）时，返回409 ILLEGAL_TRANSITION并在details写明是哪一项，不写任何行，state/revision不变（CR-023）。

**验收**

- **AC-036-01**（TEST-036-01 · 检查 `unit`）Given hoh-core迁移表；状态枚举取自001_initial.sql：Run 15个、Task 9个、Attempt 7个。；When 穷举全部有序对(from,to)（225+81+49=355）调用迁移校验。；Then 355对判定与迁移表逐项一致；状态集合与SQL CHECK枚举相同；pass_criteria所列必拒对全部返回ILLEGAL_TRANSITION。
  - 判定：迁移表状态集合与SQL枚举完全相同；被测对数=355，判定与迁移表逐项一致；必拒对全部返回ILLEGAL_TRANSITION：Run终态→任意、非VERIFYING→COMPLETED、非PAUSE_REQUESTED→PAUSED、非CANCEL_REQUESTED→CANCELLED、CREATED→RUNNING；Task非VERIFYING→ACCEPTED、READY→RUNNING；Attempt SETTLED/FAILED/CANCELLED→任意
- **AC-036-02**（TEST-036-02 · 检查 `recovery`）Given 文件DB+测试构建故障注入点：BEGIN后、state UPDATE后、event INSERT后、outbox INSERT后、COMMIT后ACK前；Run R的READY→RUNNING迁移会派发T1。；When 对每个注入点各执行20次：触发迁移→在注入点SIGKILL host→重启host并读取DB。；Then COMMIT前4个点的80次：R仍READY、revision不变、该迁移的events与outbox行数=0；COMMIT后ACK前的20次：R=RUNNING且该迁移events与outbox各恰1行；100次PRAGMA integrity_check均为ok。
  - 判定：COMMIT前注入的80次全部为：R.state=READY、revision不变、该迁移events=0、outbox=0；COMMIT后注入的20次全部为：R.state=RUNNING、events=1、outbox=1且outbox.event_seq指向该event；100次integrity_check结果均为ok；不存在outbox.event_seq指向不存在event的行
- **AC-036-03**（TEST-036-03 · 检查 `integration`）Given Run R中T1首个Attempt A1由FakeAgent返回TransientExternal失败，第二个A2同样失败，第三个成功并产出C1。；When Runtime按重试策略处理T1，随后为C1派发QA。；Then T1共3个Attempt且ID互不相同，A1、A2保持FAILED与原raw_log_path；每个Attempt的role_epoch、host_epoch、配置revision、session_id非空；QA Attempt的session_id≠开发Attempt的session_id。
  - 判定：T1的attempts行数=3，attempt_id两两不同；A1、A2的state=FAILED且raw_log_path与快照相同；全部Attempt的role_epoch/host_epoch/config_revision/session_id非空；QA Attempt.session_id≠T1任一开发Attempt.session_id
- **AC-036-04**（TEST-036-04 · 检查 `integration`）Given host运行，1个SSE客户端与1个TUI投影已订阅；故障注入使下一次COMMIT返回SQLITE_FULL。；When 提交PauseRun命令；随后关闭注入，以新idempotency_key再提交一次。；Then 首次返回非202错误且retryable=true；5 s内订阅者收到该迁移事件0条，Run state/revision不变；第二次返回202，订阅者恰收到1条。
  - 判定：首次HTTP状态≠202且错误体retryable=true；首次后5 s内两个订阅者收到PAUSE_REQUESTED相关事件数=0；runs.state与revision与提交前相同；第二次HTTP 202，每个订阅者恰收到1条对应事件

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- VERIFYING→RUNNING（候选失败后修复/下一增量）文档未显式列出，按工程循环选择为合法。
- INCOMPLETE是否终态未定；本选择终态仅COMPLETED/CANCELLED/FAILED，INCOMPLETE/BLOCKED/WAITING_EXTERNAL/PAUSED可经ResumeRun回到resume_state：先reconcile；该停止状态自己的停止条件仍成立时拒绝（ILLEGAL_TRANSITION，说明是哪一项），不写任何行——INCOMPLETE为使它停下的预算或期限仍耗尽，WAITING_EXTERNAL为没有到期的Task且仍有Task在等待next_retry_at，BLOCKED为阻塞集合非空，PAUSED没有停止条件；否则回到resume_state，阻塞集合非空时回到BLOCKED（CR-023）。
- attempts 已含 config_revision 列（CR-001 SQL）；SESSION_NOT_INDEPENDENT 为确认的错误码。
- 故障注入点是产品代码中的空操作钩子（CR-006，项目所有者批准）：产品对每个钩子的实现什么也不做，制造故障的实现只在测试目标中，由测试在同一进程内传入；产品入口（main 及其调用的一切）只使用空实现，没有环境变量、命令行参数或配置文件能换掉它。产品二进制中没有能产生故障的代码；全部代码都在 AC-062-05 的受检构建中（成员不声明 Cargo 特性）。

## REQ-037 · 单写者与并行验证

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/07-algorithms.md`、`docs/14-resilience.md`、`docs/04-architecture.md`、`docs/08-development-plan.md`、`contracts/sql/001_initial.sql`、`contracts/protocol.schema.json`、`config/hoh.example.toml`、`docs/adr/ADR-004.md`

同一repo任一时刻至多一个代码写租约；candidate冻结后QA与Review在独立读槽并行验证同一candidate身份，修复只形成新candidate，不在被验版本上边修边验。

**必须行为**

- 必须在同一事务以CAS获取leases(project_id,kind='writer')，记录attempt_id、epoch、expires_at；writer_slots=1，只改文档的任务同样需要写租约。
- 未获写租约的Task保持READY排队，不置FAILED；无写租约的Attempt不得启动写任务。
- candidate冻结后QA与Review Attempt必须绑定同一candidate_id及相同git_commit/git_tree/spec/environment/test_plan摘要，在独立读槽（初值reader_slots=2）并行执行，运行目录与写目录分离。
- barrier必须等QA与Review都到终态才判定；一方FAIL仍收集另一方证据；候选accepted需QA完成、Review完成、确定性checks有效且覆盖当前scope的required AC。
- 验证前后必须核对运行目录tree与candidate一致；不一致时该证据outcome=INVALID。
- QA/Review要求修改当前candidate时必须拒绝同步等待，转为绑定该candidate的Issue；修复在新candidate上进行，新candidate不得沿用旧candidate验收。
- 授予新写租约前必须确认上一持有者进程已结束或已被fence，否则不派发。

**失败与边界**

- 当第二个写Task申请写租约时，CAS失败，该Task保持READY，leases中writer行仍为1行。
- 当QA执行中运行目录文件被修改时，该证据outcome=INVALID，candidate不被accepted。
- 当Review绑定的git_commit与当前candidate不同（旧commit）时，其结果不计入当前barrier。
- 当barrier期间Developer提交修复时，生成新candidate C2，C2的acceptance_results在其QA/Review完成前为0行。
- 当QA请求Developer修改当前candidate时，请求被拒并生成Issue，wait-for图中不出现QA→Developer边。

**验收**

- **AC-037-01**（TEST-037-01 · 检查 `integration`）Given 固定种子生成1000组随机序列：每组≤50个写/读Task（含只改docs的写Task）及交错的acquire/release/expire操作。；When 在真实SQLite租约表上按序列执行租约操作，每步后查询有效租约。；Then 任一时刻有效writer租约数≤1；有效读租约数≤reader_slots；每次授予的writer epoch严格递增；docs-only写Task同样经writer租约。
  - 判定：所有步骤writer有效租约数≤1；所有步骤reader有效租约数≤2；writer epoch序列严格递增；docs-only写Task的Attempt均有对应writer租约记录
- **AC-037-02**（TEST-037-02 · 检查 `integration`）Given candidate C1已冻结；FakeAgent QA与Review各运行3 s，分别返回FAIL与PASS；reader_slots=2。；When Runtime处理candidate_ready。；Then QA与Review Attempt的candidate_id与5项身份摘要全相同；两者运行区间重叠；barrier在两者均终态后才关闭；C1未accepted；两份evidence均入库。
  - 判定：两Attempt绑定candidate_id=C1，git_commit/git_tree/spec/env/test_plan摘要相同；每个Attempt的started_at早于另一个的ended_at；barrier关闭事件seq大于两个Attempt终态事件seq；C1无accepted结论，evidence含2行（FAIL与PASS）
- **AC-037-03**（TEST-037-03 · 检查 `integration`）Given C1冻结且QA运行目录已检出；FakeAgent QA在第1 s改写一个已跟踪源文件，然后报告PASS。；When QA Attempt结束，Runtime执行运行后tree核对。；Then 该evidence outcome=INVALID；acceptance_results中C1无PASS行；事件记录前后tree hash且二者不同。
  - 判定：evidence.outcome=INVALID；acceptance_results中candidate_id=C1且status=PASS行数=0；事件payload含pre_tree与post_tree且不相等
- **AC-037-04**（TEST-037-04 · 检查 `integration`）Given C1的barrier未关闭（QA运行中）；FakeAgent QA发出「请Developer修改当前candidate后我继续」请求。；When Runtime处理该请求；barrier关闭后派发修复，FakeAgent Developer产出C2。；Then 请求被拒并新增1个candidate_id=C1的Issue；barrier关闭前该Issue的修复Task未派发；C2产生后其acceptance_results为0行，直到C2的QA与Review完成。
  - 判定：请求结果为rejected，issues新增1行且candidate_id=C1；wait-for图无QA→Developer边；修复Task的dispatch事件seq大于C1 barrier关闭事件seq；C2冻结时acceptance_results中C2行数=0

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- barrier开放期间Developer是否可在写目录做不相关增量文档未定；本选择允许不相关写Task，但源自当前candidate Issue的修复须待barrier关闭后派发。
- reader_slots=2取自config示例，作为初值。
- 多读槽使用编号 kind（reader-1、reader-2…）；SQL leases.kind 约束已相应放宽（CR-002）。
- （CR-007）派发提议所需的租约被占用时，提议 REJECTED、reason=LEASE_UNAVAILABLE，Task 保持 READY，tasks 与 leases 不变，只写一条拒绝事件。写租约被占用指未过期，或其持有 Attempt 尚未结束；读槽全部占用时同理。

## REQ-038 · 任务DAG调度

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/07-algorithms.md`、`tools/reference_model.py`、`contracts/sql/001_initial.sql`、`docs/01-prd.md`、`docs/09-testing.md`

计划以Kahn拓扑校验拒绝环与缺失依赖；就绪集按严重层→docs/07加权分→created_seq,task_id确定性排序，老化有上限，同输入同参数总得到相同派发序列。

**必须行为**

- 计划入库前必须做Kahn拓扑检查（O(V+E)）；存在环、缺失依赖或重复task_id时整个计划拒绝，不部分入库。
- eligible必须同时满足：依赖全为ACCEPTED、绑定当前spec revision、未被租约占用、角色能力满足、预算可预留；高分不得越过未满足依赖。
- 排序必须先分层：关键回归/数据完整性(0)→解阻(1)→普通(2)；层内按score=(4*user_value+3*unblock_value+3*risk_reduction+2*information_gain+age_bonus)/max(cost_estimate,1)降序。
- 平局必须依次按created_seq升序、task_id字典序打破；结果与输入顺序及哈希迭代顺序无关。
- 各输入分值必须在0..5；cost缺估计用中位桶；age_bonus截断在0..10。
- eligible但未被选的调度轮数达到max_wait_rounds（初值20）的Task进入层0与层1之间的老化子层，子层内按首次eligible序号FIFO；层0为空时等待轮数有上界（防饥饿）。
- 权重与参数必须写入Run配置快照；活跃Run中不得修改，变更只对新Run生效。
- 1000个活跃Task及其依赖必须可确定性调度，不在每次选择时全表反序列化历史。

**失败与边界**

- 当计划含环时，返回DAG_CYCLE并列出环上task_id，tasks与task_dependencies无该计划新行。
- 当依赖指向不存在的task_id时，返回DAG_MISSING_DEPENDENCY，计划拒绝。
- 当分值超出0..5或cost<0时，返回SCORE_OUT_OF_RANGE，不静默截断。
- 当最高分Task依赖未ACCEPTED时不派发它，改选下一个eligible；无eligible时返回无可派发而非强行运行。
- 当对活跃Run修改调度权重时，返回409，Run配置快照不变。

**验收**

- **AC-038-01**（TEST-038-01 · 检查 `unit`）Given 依赖均ACCEPTED的就绪集（cost同单位）：A层0(user_value=0,cost=5)；B层1(unblock=2,cost=1)；C、F层2(user_value=5,cost=1,created_seq=10,id t-c/t-f)；D、E层2(user_value=3,risk=2,cost=2,created_seq 12/11)。；When 调度器对该集合排序，并对同一输入的10个随机排列重复。；Then 11次结果均为A,B,C,F,E,D；score：A=0、B=6、C=F=20、D=E=9（误差≤1e-9）。
  - 判定：11次顺序均为[A,B,C,F,E,D]；A、B、C、D、E、F的score分别为0、6、20、9、9、20（|误差|≤1e-9）
- **AC-038-02**（TEST-038-02 · 检查 `unit`）Given 固定种子随机DAG生成器：1000例，每例10–1000个Task，分值0..5，部分Task已ACCEPTED、被租约占用、绑定旧spec revision、缺角色能力或超出预算。；When 对每例以原顺序及3个随机排列各自循环选择直到无eligible。；Then 每例4次派发序列完全相同；所有被选Task满足eligible五项条件；依赖未ACCEPTED的Task被选次数=0。
  - 判定：1000例中4种顺序的派发序列全部相同；所有被选Task满足：依赖ACCEPTED、当前spec、未租约、能力满足、预算可预留；违反任一条件的选择次数=0；含1000个Task的例子全部在测试超时内完成
- **AC-038-03**（TEST-038-03 · 检查 `integration`）Given 三个计划：P1含环a→b→a；P2含依赖missing；P3含user_value=7的Task。；When 分别提交计划校验并尝试入库。；Then P1返回DAG_CYCLE且列出a与b；P2返回DAG_MISSING_DEPENDENCY；P3返回SCORE_OUT_OF_RANGE；tasks与task_dependencies新增0行。
  - 判定：三个错误code依次为DAG_CYCLE、DAG_MISSING_DEPENDENCY、SCORE_OUT_OF_RANGE；P1错误体列出的环包含a与b；tasks与task_dependencies行数不变
- **AC-038-04**（TEST-038-04 · 检查 `unit`）Given 层2就绪集：H(user_value=2,cost=2,初始score=4)较早创建；每轮补入新的L(user_value=3,cost=2,score=6)；age_bonus按每轮eligible未被选+1（初值），上限10。；When 连续运行调度轮并每轮选1个Task。；Then H在第5轮被选（age_bonus=4，score=6，与L同分按created_seq胜出）；所有Task的age_bonus从不>10。
  - 判定：前4轮选中L，第5轮选中H；第5轮H的age_bonus=4且score=6；全部轮次age_bonus最大值≤10
- **AC-038-05**（TEST-038-05 · 检查 `integration`）Given Run R处于RUNNING，配置快照记录调度权重(4,3,3,2)。；When 提交修改R权重为(1,1,1,1)的配置变更。；Then 返回409；R配置快照仍为(4,3,3,2)；后续调度score仍按原权重计算。
  - 判定：HTTP状态=409；配置快照权重=(4,3,3,2)；该次调度的score与原权重手算值一致
- **AC-038-06**（TEST-038-06 · 检查 `unit`）Given 层0为空；层2中H(user_value=0,cost=5,score=0)最早eligible；每轮补入一个新的L(user_value=3,cost=2,score=6)；max_wait_rounds=20，age_bonus上限10。；When 连续运行25个调度轮，每轮选1个Task。；Then 前20轮选中L；第21轮选中H（已进入老化子层）；全部轮次中任一eligible Task连续未被选的轮数≤20。
  - 判定：第1–20轮选择均为L；第21轮选择为H；最大连续等待轮数≤20；H在第21轮的子层标记=aged

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- age_bonus增长单位未定；本选择为每个调度轮eligible但未被选+1（不依赖墙钟），上限10。
- 已修正（CR-002）：age_bonus 上限 10 只能追平有限分差，无法防饥饿。新增 max_wait_rounds（初值 20）：eligible 但未被选的轮数达到上限的 Task 进入位于层0与层1之间的老化子层，子层内按首次 eligible 的序号 FIFO；层0为空时任何 eligible Task 的等待轮数有上界。
- cost_estimate桶未定；初值为整数1..5，缺估计取中位桶3。
- 分层赋值规则未定；本选择：critical/high回归或数据完整性Issue→0，被阻塞Task依赖→1，其余→2。
- 超出0..5的分值选择拒绝而非截断；SCORE_OUT_OF_RANGE、DAG_CYCLE、DAG_MISSING_DEPENDENCY、DAG_DUPLICATE_TASK（CR-007）为初值错误码。
- （CR-007）调度参数为配置 [scheduler] 的 max_wait_rounds 与 [scheduler.weights] 的四个权重（$defs.SchedulerSettings），StartRun 时写入 runs.config_snapshot_json 的 scheduler 键；快照缺键按默认值读；每轮调度事件记下所用参数。修改调度参数的配置变更是带 scheduler 的 ConfigureTeam：run_id 指向已存在的 Run 时返回 409 RUN_CONFIG_FROZEN，不写任何东西，快照与之后的 score 不变；run_id 为 null 时形成新的项目配置 revision，只对之后启动的 Run 生效（随 team revision 由 M2-T07 实现）。

## REQ-039 · 上下文渐进披露

M2 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/07-algorithms.md`、`docs/15-prompts-skills-memory.md`、`docs/06-orchestration.md`、`CONTRACT.md`、`contracts/protocol.schema.json`、`examples/prompt-context.json`、`resources/prompts/qa.md`

每次 attempt 的 Prompt 由四层上下文按预算装箱：强制项永不裁剪，其余预算按 45/35/20 分给代码、证据和知识；被省略的内容保留引用；交接只传引用与摘要，不复制会话全文。

**必须行为**

- 每个 attempt 的 Prompt 必须由 Role Contract、Project Context、Task Context、Evidence Context 四层合成，并记录 prompt 文本 sha256、资源包版本、输入引用列表和截断策略。
- 强制项（角色契约、goal、任务范围、spec 与 in-scope AC、candidate 身份、输出 Schema）必须原样放入；放不下时报 CONTEXT_BUDGET_TOO_SMALL，不启动 attempt。
- 扣除强制项后的预算默认按代码 45%、证据 35%、知识 20% 分配；每类内部按 relevance、recency、causal relation 排序装入。
- 未装入或以摘要代替的条目，必须保留 path+sha256 引用和检索索引项，需要时可取回原文。
- 跨会话或跨角色交接只传引用和摘要，不复制其他会话的完整 transcript，也不传隐藏推理。
- token 数不是由模型 tokenizer 计算时，必须标注 estimated=true。
- Skill 正文按需加载，系统 Prompt 只附短索引；project memory 召回的内容标为经验，不作为验收事实。
- 输入引用与资源包版本相同时，装箱结果必须逐字节一致。

**失败与边界**

- budget_tokens 小于强制项总量时，返回 CONTEXT_BUDGET_TOO_SMALL 并给出 required 与 available 数值，attempt 不进入 RUNNING。
- 某类候选内容超出其份额时，只装入排序靠前的条目，其余用引用代替，该类实际 token 数不超过份额。
- 其他会话的原始日志内容出现在 Prompt 中，视为上下文污染缺陷。
- 同一输入两次装箱得到不同的 prompt sha256，视为非确定性缺陷。

**验收**

- **AC-039-01**（TEST-039-01 · 检查 `unit`）Given budget_tokens=10000；强制项估算 4000；候选代码 12000、证据 9000、知识 6000（多条，各带 relevance/recency/causal 分值）；使用估算 tokenizer；When 调用 ContextPacker 装箱；Then 强制项逐字节出现；代码≤2700、证据≤2100、知识≤1200；每个未装入或被摘要的条目都有 path+sha256 引用和索引项；token 计数标 estimated=true
  - 判定：强制项原文都是输出的子串；三类 token 分别 ≤2700/2100/1200，总量 ≤10000；被省略条目数 == 引用索引条目数；estimated=true
- **AC-039-02**（TEST-039-02 · 检查 `unit`）Given 固定强制项集合，大小为 M token；200 个随机预算，取值范围 [M-1, M+20000]，其中包含 M-1 与 M；When 对每个预算调用装箱；Then 预算<M 时全部返回 CONTEXT_BUDGET_TOO_SMALL 且不生成 Prompt；预算≥M 时 goal、spec_sha256、全部 in-scope AC 正文、candidate 身份和输出 Schema 都完整出现，总 token≤预算
  - 判定：预算=M-1 时报 CONTEXT_BUDGET_TOO_SMALL；所有预算≥M 的样例中强制项完整，失败数=0；所有样例总 token≤预算
- **AC-039-03**（TEST-039-03 · 检查 `integration`）Given Developer 会话原始日志 ≥200KB，内含唯一哨兵串；准备一次 Developer→QA 交接和一次 Commander 交接；When Runtime 为 QA 和新 Commander 生成 ContextHandoff 与 Prompt；Then 两份 Prompt 中哨兵串都出现 0 次；references 全部为 ArtifactRef(path+sha256)；Prompt 的 token 数≤ContextHandoff.budget_tokens
  - 判定：哨兵串计数=0（两份）；references 每项都有 64 位十六进制 sha256 且文件存在；token(Prompt)≤budget_tokens
- **AC-039-04**（TEST-039-04 · 检查 `integration`）Given attempt A 已完成，记录了 prompt sha256、资源包版本、输入引用列表和截断策略；When 用 A 记录的输入引用和同一资源包版本重新装箱；Then 新 Prompt 的 sha256 等于 A 记录的值；上述四个字段都非空
  - 判定：sha256(重装) == attempts(A).prompt_sha256；4 个字段都不为空

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 文档只写了“默认分配”，没说某类用不完的份额是否可以挪给其他类。选择以份额为硬上限，不挪用，便于复现和判定。
- 估算 tokenizer 的误差容限未定。选择所有预算判定都以估算值为准，并标 estimated。

## REQ-040 · 停滞与诊断

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/07-algorithms.md`、`docs/06-orchestration.md`、`docs/adr/ADR-031.md`、`docs/adr/ADR-025.md`、`docs/adr/ADR-029.md`

按问题指纹/状态变化而非commit计数诊断，升级/研究/改计划

**必须行为**

- 净进展是向量（docs/07 §2）：新增 accepted AC、关闭的 issue、重开的 issue、新增 critical/high、未消除的决策风险；判定不读提交数。
- 连续 3 轮验证 accepted 不增、critical/high 不降、且失败 fingerprint 重复时，写恰好一条 no_progress{window:3, fingerprints, vector, last_progress_seq}，以 High 唤醒 Commander。
- 窗口重设：一轮出现关闭的 issue 或 accepted 增加即重设；规格新增 REQ 时重设并在事件中写原因；发布稳定阶段问题总数下降即为进展。
- no_progress 的唤醒带诊断（重复的 fingerprint、最后一次进展的 seq、建议：升级、研究或改计划）；Commander 的回答可建研究 Task 或改计划；Runtime 不自动循环“继续”。
- no_progress 管“有推进而无净进展”，ADR-031 更正 C 的 STALLED 管“没有任何推进”；同一窗口内两者不对同一指纹重复唤醒（C29）。

**失败与边界**

- 只改 README 或 docs 的三轮候选、提交数增加而 accepted 不增，照样判为无进展。
- 同一窗口不写第二条 no_progress，重启后不重复。
- STALLED 已为同一状况唤醒时不再写 no_progress。

**验收**

- **AC-040-01**（TEST-040-01 · 检查 `integration`）Given 脚本化的验证结果：连续 3 轮同一失败 fingerprint F 复现，accepted AC 不增，critical/high 不降；When Runtime 处理第 3 轮的结果，随后再处理一轮同样的结果并重启 host；Then 第 3 轮后恰写一条 no_progress{window:3, fingerprints:[F], vector, last_progress_seq} 并以 High 唤醒 Commander；第 4 轮与重启后不再写第二条
  - 判定：恰一条 no_progress 且载荷字段齐全；High 唤醒；第 4 轮与重启后无第二条
- **AC-040-02**（TEST-040-02 · 检查 `unit`）Given 三轮候选只改 README 与 docs/，提交数各增加，accepted 不增；When 对三轮结果调用进展向量函数与判定；Then 判定为无进展；进展向量中没有提交数分量，提交数变化不改变判定
  - 判定：判定无进展；向量无提交数分量
- **AC-040-03**（TEST-040-03 · 检查 `unit`）Given 四个结果序列：第 2 轮关闭一个 issue；第 2 轮 accepted 增加；第 2 轮规格新增一条 REQ；发布稳定阶段问题总数从 5 降到 3；When 对每个序列逐轮判定；Then 前两个序列在第 2 轮重设窗口、第 3 轮不判无进展；第三个序列重设并在事件中写原因“规格新增 REQ”；第四个序列判为进展
  - 判定：前两个第 2 轮重设；第三个重设并写原因；第四个判为进展
- **AC-040-04**（TEST-040-04 · 检查 `integration`）Given 两个 Run 各有一次 no_progress 唤醒 Commander：Run X 的 FakeCommander 以改计划回答（取消重复失败的 Task、以 create_tasks 建替代 Task），Run Y 的以建研究 Task 回答；When 两个 Commander 各处理其唤醒；Then 两份唤醒摘要都含重复的 fingerprint、最后一次进展的 seq，以及升级、研究、改计划三项建议；X 中原 Task 为 CANCELLED、替代 Task 被导入；Y 中出现 role=researcher 的新 Task；两个 Run 中 Runtime 都没有自行派发“继续”的 Commander turn
  - 判定：摘要含 fingerprint、最后进展 seq 与三项建议；X 的原 Task CANCELLED 且替代 Task 导入；Y 出现 researcher 的新 Task；无自动“继续” turn

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 窗口 3 轮为初值（docs/07 §2），固定写在 Runtime（CR-026）。
- no_progress 与 STALLED 分工如上，同一状况只唤醒一次（CR-026，C29）。

## REQ-041 · 恢复与幂等

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/14-resilience.md`、`docs/04-architecture.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/09-testing.md`、`docs/13-adapters.md`、`docs/01-prd.md`、`contracts/sql/001_initial.sql`、`docs/adr/ADR-005.md`

host在kill -9、断网或VM重启后按固定顺序reconcile：取锁与完整性检查、递增host_epoch、核对未完成Attempt的进程/会话/effect，再决定入库、重投或UNKNOWN_EFFECT；旧epoch与过期租约结果被fence。

**必须行为**

- host启动必须按序：取项目锁→schema版本与完整性检查→host_epoch+1→扫描LEASED/RUNNING Task及其PREPARED/STARTING/RUNNING Attempt→查询进程/会话/effect→处置；完成前不接受写命令。
- 处置必须是：能确认完成则观察结果入库；确认未执行才重投；无法确认则Attempt=UNKNOWN_EFFECT并产生诊断，禁止盲目重放。
- 进程存活判定必须同时匹配boot_id、PID与进程启动标识（如/proc/<pid>/stat starttime）；不匹配即视为非本Attempt进程，不得对其发信号。
- 租约过期不得直接判旧进程死亡：先inspect并cancel/kill旧进程组、确认reap后才以epoch+1重授；携带旧lease epoch、旧role_epoch或旧host_epoch的写入一律拒绝并只存历史。
- 命令按idempotency_key去重：同key同payload hash返回原结果；同key异payload返回409 KEY_REUSE；消费以(consumer,message_id)收据去重。
- 外部效果必须经effect ledger PREPARED→STARTED→OBSERVED→COMMITTED；STARTED后崩溃先按run marker/external_ref查询；PR先查已有再创建。
- 常见进程故障（Agent子进程退出、host崩溃后重启）必须在≤30 s内被检测并进入reconcile，不含上游模型不可用等待。

**失败与边界**

- 当STARTED后的effect无法经marker/会话/commit确认时，Attempt=UNKNOWN_EFFECT，Task不自动重投，诊断事件含effect_key。
- 当记录的PID存在但starttime或boot_id不同（PID复用/VM重启）时，判定原进程已不存在，不向该PID发信号。
- 当被fence的旧Attempt晚到TaskResult时，返回STALE_LEASE_EPOCH，结果存为历史事件，Task revision不变。
- 当旧host_epoch的写入到达时，带host_epoch条件的UPDATE影响0行，返回IntegrityViolation，状态不变。
- 当旧进程组在kill升级后仍无法reap时，不授予新写租约，Task保持阻塞并给诊断。
- 当DB完整性检查失败时，host拒绝写命令并输出诊断，不进入派发。

**验收**

- **AC-041-01**（TEST-041-01 · 检查 `recovery`）Given FakeAgent每次执行dispatch时向外部计数文件追加effect_key，结束时写完成marker；Run含5个Task。；When 在5个点kill -9 host：发送dispatch前、发送后收ACK前、收ACK后写收据前、effect STARTED后OBSERVED前、effect COMMITTED后Task迁移前；每点10次，harness在kill后1 s内重启host。；Then 每个effect_key在计数文件中出现≤1次；未完成Attempt均处置为SETTLED、确认未发送后重投或UNKNOWN_EFFECT之一；reconcile完成事件seq早于重启后任一新dispatch；kill到reconcile开始≤30 s。
  - 判定：50次运行中每个effect_key计数≤1；旧Attempt最终state∈{SETTLED,CANCELLED,UNKNOWN_EFFECT}且重投仅发生在「未发送」判定之后；UNKNOWN_EFFECT Attempt所属Task无自动新增Attempt；每次reconcile完成seq<重启后首个dispatch outbox的event_seq；每次reconcile开始时刻−t0≤30 s
- **AC-041-02**（TEST-041-02 · 检查 `recovery`）Given 写租约epoch=4由Attempt A1持有，A1的FakeAgent进程组运行且忽略SIGTERM；FakeClock推进超过expires_at。；When Runtime处理租约过期；随后注入A1携带lease epoch=4的TaskResult，并以旧host_epoch调用一次持久化写接口。；Then 事件顺序为inspect→native cancel→SIGTERM→SIGKILL→reap确认→授予epoch=5；授予前A1进程组存活数=0；epoch=4结果返回STALE_LEASE_EPOCH且Task revision不变；旧host_epoch写入影响0行并返回IntegrityViolation。
  - 判定：相关事件seq顺序与期望完全一致；epoch=5授予事件之前A1进程组无存活进程（/proc扫描）；TaskResult响应code=STALE_LEASE_EPOCH，Task.revision不变，历史事件1条；旧host_epoch写入rowcount=0且错误类别=IntegrityViolation
- **AC-041-03**（TEST-041-03 · 检查 `recovery`）Given attempts中A2记录pid=P（实为测试启动的无关sleep进程），boot_id相同但starttime不同；A3记录不同boot_id（模拟VM重启前）。；When 重启host执行reconcile。；Then A2、A3均判定原进程不存在，按effect/session核对处置；无关sleep进程仍存活且收到信号数=0；A2、A3最终state≠RUNNING。
  - 判定：辅助进程存活且信号计数=0；A2、A3的state∈{SETTLED,FAILED,UNKNOWN_EFFECT}；reconcile事件payload记录判定依据为starttime/boot_id不匹配
- **AC-041-04**（TEST-041-04 · 检查 `integration`）Given host运行；故障注入使/v1/commands在COMMIT后、发送202前断开连接。；When 客户端以idempotency_key=K、payload X提交PauseRun，断开后以K+X重试；再以K+不同payload Y提交。；Then 重试返回202且command_id与revision等于首次提交结果；该命令事件数=1；K+Y返回409 code=KEY_REUSE；commands表中K仅1行。
  - 判定：重试响应202，command_id与revision等于DB中首次结果；该command对应PAUSE_REQUESTED事件数=1；K+Y响应409且code=KEY_REUSE；commands中idempotency_key=K行数=1
- **AC-041-05**（TEST-041-05 · 检查 `recovery`）Given FakeRemote（本地fake forge）支持按run marker查询PR；effect E(创建PR)为STARTED且远端已有带该marker的PR；effect E2为STARTED而FakeRemote查询返回不可用。；When 重启host执行reconcile。；Then E置OBSERVED或COMMITTED且external_ref=已有PR；E2置UNKNOWN、所属Attempt=UNKNOWN_EFFECT并产生诊断事件；FakeRemote创建调用数=0。
  - 判定：E.state∈{OBSERVED,COMMITTED}且external_ref=预置PR标识；E2.state=UNKNOWN，其Attempt.state=UNKNOWN_EFFECT；存在含E2 effect_key的诊断事件；FakeRemote创建调用次数=0

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 租约TTL与续约周期未定：初值TTL=120 s、每30 s续约。
- attempts 已含 pid、boot_id、process_start_id 列（CR-001 SQL）；进程组 id 等于 pid（独立 session 启动）；STALE_LEASE_EPOCH 为确认错误码。
- host崩溃的≤30 s检测依赖systemd RestartSec或下一次客户端启动；以RestartSec=5 s（初值）测量。

## REQ-042 · 预算与等待

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/07-algorithms.md`、`docs/14-resilience.md`、`docs/13-adapters.md`、`docs/12-user-guide.md`、`docs/01-prd.md`、`docs/10-acceptance.md`、`config/hoh.example.toml`

外部429/5xx与超时按Retry-After或full-jitter指数退避并持久next_retry_at，重启不清计数与预算；鉴权/能力缺失直接BLOCKED；预算、迭代或deadline耗尽时停止派发并报告INCOMPLETE，未知价格显示unknown。

**必须行为**

- 429/503必须优先用Retry-After；没有时full-jitter：delay=uniform(0,min(cap,base·2^n))（初值base=2 s、cap=300 s），并在同事务持久attempt_count、next_retry_at、deadline。
- 等待期间该Task不可派发；仅当Run内没有其他可派发Task时Run为WAITING_EXTERNAL，否则Run保持RUNNING（CR-003）；next_retry_at之前不得派发该项；计时用单调时钟，重启后按持久UTC期限重算。
- 同一外部故障首次失败后最多重试8次（初值）；用尽后Task=BLOCKED并产生need_decision，不无限隐藏重试。
- 鉴权缺失必须直接BLOCKED（BLOCKED_AUTH），CLI能力不兼容直接BLOCKED（BLOCKED_CAPABILITY）；不进入指数退避。
- 同一问题本地修复≤3次、协议格式纠正≤2次（max_local_repairs/max_protocol_repairs）；超限即升级决策，不再派发同类修复。
- 预算（max_iterations=100、max_duration_hours=120、可选token/费用上限）必须派发前预留并持久累计；耗尽或deadline_at到达即停止新派发，Run=INCOMPLETE并列出剩余项。
- 价格未知时cost必须为null并标unknown；不以估算冒充账单。配置了费用上限而所用模型价格未知时，该上限无法执行，派发前即BLOCKED（BUDGET_UNENFORCEABLE）。
- wall deadline与inactivity watchdog必须分别计时；无输出但工具进程仍运行不判为死机。
- 预算风险（REQ-021行为1的“budget 风险”）：Runtime每次推进Run（pace）判定预算停止时，在同一事务内对每种已设上限的预算也判定风险；读取预算、查重与写事件同在这一事务；其他只判定能否派发的预算检查（调度准入、派发决策、Commander turn的分配）不判定风险，它们先看到的越线在下一次pace时写。判定：max_iterations、max_duration_hours与价格已知的max_usd按停止的同一口径（已用加预留）达到上限的80%；deadline_at按墙钟，自Run创建起经过的时长达到创建至deadline_at时长的80%（该时长不为正时不判定）。成立、该预算的停止尚不成立、且该Run该种在该上限下还没有风险事件时，写一条budget.risk{kind, used, limit, unit, threshold}：threshold为0.8；used为判定所用的总量（已用加预留，deadline_at为已过去的墙钟秒数）；limit为同单位的上限（deadline_at为创建至deadline_at的秒数）。按水平判定：暂停期间越过的，在恢复后第一次判定时写。上限改变后按新上限重新判定。同一Run、同一种、同一上限只写一次，重启后不重复。它只唤醒Commander，不改变任何预算的执行：停止派发仍只在用尽或deadline到达时发生，也不计入重试次数（CR-025）。

**失败与边界**

- 当host在WAITING_EXTERNAL期间重启时，attempt_count、next_retry_at、已用预算保持不变，重启后不立即重试。
- 当返回鉴权失败时，Run=BLOCKED且原因BLOCKED_AUTH，next_retry_at为空，attempt_count=1。
- 当迭代数达到max_iterations而required AC未全部accepted时，Run=INCOMPLETE，不得COMPLETED，最终JSON列出剩余项。
- 当Retry-After晚于deadline_at时，不等待越过deadline，到期即INCOMPLETE。
- 当模型价格未知时，最终报告cost_usd=null、cost_status=unknown，不输出0。

**验收**

- **AC-042-01**（TEST-042-01 · 检查 `unit`）Given FakeClock起点T0、固定种子RNG；FakeAgent对Task连续返回5次503（无Retry-After），第6次返回429且Retry-After: 120。；When Runtime依次处理每次失败，并在每个next_retry_at前1 ms与到达时检查派发。；Then 第n次(n=0..4)delay∈[0,min(300,2·2^n)] s且next_retry_at已持久；429后next_retry_at=失败时刻+120 s；next_retry_at−1 ms时派发数0、到达时1；等待期间Run=WAITING_EXTERNAL。
  - 判定：5个503 delay均落在[0,min(300,2·2^n)] s；429后next_retry_at−失败时刻=120 s；每个等待区间末前1 ms派发数=0，到达时=1；等待区间Run.state=WAITING_EXTERNAL
- **AC-042-02**（TEST-042-02 · 检查 `recovery`）Given Run处于WAITING_EXTERNAL：Task attempt_count=3、next_retry_at=T+200 s；已用iterations=7及已用时长已持久。；When 在T+5 s kill -9 host，T+10 s重启。；Then 重启后attempt_count=3、next_retry_at=T+200 s、iterations=7逐值不变；T+200 s前派发数=0；不新增Attempt。
  - 判定：重启前后attempt_count、next_retry_at、iterations、已用时长逐字段相等；T+199 s前派发数=0；T+200 s后派发数=1且attempts仅新增该1行
- **AC-042-03**（TEST-042-03 · 检查 `integration`）Given 三个Run：R1的FakeAgent返回鉴权失败；R2的probe缺所需能力；R3的FakeAgent连续返回503共9次。；When 各自派发并由Runtime处理。；Then R1=BLOCKED、原因BLOCKED_AUTH、attempt_count=1、next_retry_at为空；R2=BLOCKED、原因BLOCKED_CAPABILITY且未启动Attempt；R3第9次失败后不再调度、Task=BLOCKED并产生need_decision；三者均≠COMPLETED。
  - 判定：R1：state=BLOCKED、原因=BLOCKED_AUTH、attempt_count=1、next_retry_at=NULL；R2：state=BLOCKED、原因=BLOCKED_CAPABILITY、attempts=0；R3：attempts=9，此后无新dispatch，Task=BLOCKED，存在need_decision事件；三个Run的state均≠COMPLETED
- **AC-042-04**（TEST-042-04 · 检查 `integration`）Given Run Ra配置max_iterations=2，价格表不含所用模型，FakeAgent QA每轮FAIL；Run Rb设deadline_at=T0+60 s。；When Ra运行至停止派发；Rb推进FakeClock到T0+60 s。；Then Ra第2轮后无新dispatch且state=INCOMPLETE，--output json最终结果列出未accepted的REQ、cost_usd=null、cost_status=unknown；Rb到期后state=INCOMPLETE；两者≠COMPLETED。
  - 判定：Ra迭代数=2，之后outbox无新dispatch；Ra最终JSON含state=INCOMPLETE、非空剩余REQ列表、cost_usd=null、cost_status=unknown；Rb.state=INCOMPLETE；Ra与Rb的state≠COMPLETED
- **AC-042-05**（TEST-042-05 · 检查 `unit`）Given 同一issue fingerprint F的本地修复已失败3次；某Attempt的协议格式纠正已发生2次；之后host重启一次。；When Runtime评估第4次本地修复与第3次协议纠正。；Then 两者均不派发；各产生1条need_decision升级事件；重启后计数仍为3与2；相关Task≠ACCEPTED。
  - 判定：重载后计数=3与2；两次请求dispatch数=0；need_decision事件数=2；相关Task.state≠ACCEPTED
- **AC-042-06**（TEST-042-06 · 检查 `integration`）Given Run Rc配置费用上限max_usd=5，所用模型不在价格表中；Run Rd未配置费用上限，其余相同。；When 两个Run各尝试派发第一个Attempt。；Then Rc在派发前进入BLOCKED、原因BUDGET_UNENFORCEABLE、attempts=0；Rd正常派发，cost_usd=null且cost_status=unknown。
  - 判定：Rc：state=BLOCKED、blocked_reason=BUDGET_UNENFORCEABLE、attempts行数=0；Rd：attempts行数≥1、cost_usd=null、cost_status=unknown；两者state≠COMPLETED
- **AC-042-07**（TEST-042-07 · 检查 `unit`）Given Run Re设max_iterations=100，已用迭代79；Run Rf设deadline_at=创建时刻+100 s（墙钟）；FakeClock。；When Re的预算判定一次；已用迭代到80后判定一次、再判定一次、重启（重新打开库）后再判定一次；Rf的墙钟到创建后79 s与80 s各判定一次。；Then Re在79时budget.risk为0条；到80时恰好1条budget.risk{kind:max_iterations, used:80, limit:100, unit:iterations, threshold:0.8}，之后的判定与重启后的判定都不再写；Rf在79 s时0条，80 s时恰好1条{kind:deadline_at, used:80, limit:100, unit:seconds, threshold:0.8}；这些判定前后，两个Run的state、派发数与attempt_count都不变。
  - 判定：Re：79时budget.risk为0条，80时恰好1条且载荷为{kind:max_iterations, used:80, limit:100, unit:iterations, threshold:0.8}，再判定与重启后仍为1条；Rf：+79 s为0条，+80 s恰好1条且载荷为{kind:deadline_at, used:80, limit:100, unit:seconds, threshold:0.8}；每次判定前后两个Run的state、outbox派发数与attempt_count不变

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 退避base=2 s、cap=300 s、瞬时重试上限8次均为初值。
- 已修正（CR-002）：价格未知时 cost 为 null 并显示 unknown；若用户配置了费用上限而所用模型价格未知，该上限无法执行，派发前即 BLOCKED（BUDGET_UNENFORCEABLE），不在无法核算时继续花费；未配置费用上限时照常受其他预算约束。
- 命名已统一（CR-001）：BLOCKED_AUTH/BLOCKED_CAPABILITY 为阻塞原因，CAPABILITY_UNSUPPORTED 为错误码；runs 已含 blocked_reason 列。
- tasks 已含 attempt_count、next_retry_at；预算账本为 budgets 表（CR-001 SQL）。
- （CR-003）费用与期限配置键：[budget] max_usd（可选，缺省表示未设费用上限）、deadline_at（可选，RFC 3339 UTC）；与 max_duration_hours、max_iterations 同属 [budget]。
- 预算风险的阈值80%为初值，是预警而不是保证：通常在停止之前还留有上限的约五分之一，供Commander重排剩余工作、向用户提出范围调整或请用户追加预算；一次迭代或一个Attempt的预留可能一步越过80%直到100%，那时只有停止，没有风险事件。更早的阈值在长Run中过早唤醒，更晚的留下的余量不够一次派发。价格未知的费用上限无法执行（BUDGET_UNENFORCEABLE），没有风险可报。kind取停止的名字（max_iterations、max_duration_hours、max_usd、deadline_at，与run.incomplete的stop相同）；unit为iterations、seconds或usd（max_duration_hours与deadline_at为seconds）。deadline_at与它的停止一样用墙钟；max_duration_hours只计Attempt的已用加预留，等待不计入（CR-025）。

## REQ-043 · 暂停取消分离

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`CONTRACT.md`、`docs/14-resilience.md`、`docs/13-adapters.md`、`docs/03-ux.md`、`docs/12-user-guide.md`、`docs/05-data-api.md`、`docs/04-architecture.md`

pause持久后立即阻止新派发并在安全点进入PAUSED；cancel经优先通道native interrupt→SIGTERM进程组→SIGKILL并reap，保留工作区与日志后CANCELLED；detach只断开客户端。

**必须行为**

- PauseRun必须在一个事务内置PAUSE_REQUESTED并保存resume_state，COMMIT后不再产生新dispatch；当前Attempt到达终态（安全点）后置PAUSED，不中断该Attempt。
- ResumeRun必须先reconcile再恢复resume_state；不得重复派发运行中或已完成的Attempt。
- CancelRun必须走优先队列，置CANCEL_REQUESTED（UI显示CANCELLING）；先native interrupt，grace后SIGTERM进程组，再grace后SIGKILL并wait/reap（初值10 s/5 s）。
- 进入CANCELLED前必须记录工作区tree/diff hash与日志路径；不得reset、clean或删除工作区改动；未完成candidate不得标verified。
- 超时不等于已取消：进程组仍有存活进程时保持CANCEL_REQUESTED并继续升级，不写CANCELLED。
- 客户端关闭、SSH断开、TUI收到SIGHUP/SIGTERM只退订并恢复终端，不向Agent发送任何控制命令或信号；空闲顶层首次Ctrl+C显示脱离/暂停/取消选择。
- pause/cancel不依赖Commander或任何模型同意，Commander不可用时照常生效。
- PAUSE_REQUESTED期间收到Cancel时Cancel优先；CANCEL_REQUESTED期间的Pause被拒绝。

**失败与边界**

- 当Agent忽略native interrupt与SIGTERM时，两段grace后SIGKILL，结束后进程组存活进程数=0且无僵尸。
- 当升级kill后进程仍报告存活时，Run保持CANCEL_REQUESTED并显示CANCELLING，不报告CANCELLED。
- 当Pause发生在Attempt中途时，该Attempt不被中断，Pause后新增dispatch=0。
- 当对终态Run发Pause或Cancel时，返回409 ILLEGAL_TRANSITION，状态不变。
- 当TUI收到SIGTERM时，终端恢复，host与Agent进程均存活，Run state/revision不变。

**验收**

- **AC-043-01**（TEST-043-01 · 检查 `integration`）Given Run R RUNNING；T1 Attempt运行中（FakeAgent共8 s）；T2、T3 READY且eligible。；When 第2 s提交PauseRun，第12 s提交ResumeRun。；Then Pause返回202后Run=PAUSE_REQUESTED、resume_state=RUNNING；T1未被中断并SETTLED后Run=PAUSED；2–12 s新增dispatch=0；Resume后先有reconcile事件再派发T2，T1无新Attempt。
  - 判定：Pause响应202，随后runs.state=PAUSE_REQUESTED且resume_state=RUNNING；T1 FakeAgent未收到interrupt，A1.state=SETTLED，之后runs.state=PAUSED；2–12 s之间outbox新增dispatch=0；Resume后reconcile事件seq<T2 dispatch事件seq，T1 Attempt行数=1
- **AC-043-02**（TEST-043-02 · 检查 `integration`）Given Run R RUNNING；FakeAgent启动子进程与孙进程（同进程组），其中一个忽略SIGTERM；工作区有未提交修改与未跟踪文件。；When 提交CancelRun。；Then 事件按CANCEL_REQUESTED→interrupt→SIGTERM→SIGKILL→工作区hash→CANCELLED出现；CANCELLED前进程组存活与僵尸数=0；git status不变；candidate未标verified；耗时≤17 s（初值）。
  - 判定：相关事件按期望顺序出现；CANCELLED事件前pgid内存活进程数=0，状态Z进程数=0；cancel前后git status --porcelain输出逐字节相同；无verified/accepted标记写入；CancelRun提交到CANCELLED耗时≤17 s
- **AC-043-03**（TEST-043-03 · 检查 `tui-pty`）Given PTY中hoh TUI附着运行中的Run R（FakeAgent运行60 s）；已记录host与Agent pid。；When 依次：对TUI发SIGHUP；重新attach后对TUI发SIGTERM；再attach后在空闲顶层空输入框按Ctrl+C并选择「脱离」。；Then 三次TUI均退出；SIGTERM与Ctrl+C两次终端恢复（离开alternate screen且icanon/echo开启）；首次Ctrl+C只显示含脱离/暂停/取消的选择；host与Agent pid全程存活；Run state/revision不因客户端退出改变。
  - 判定：3次TUI进程均退出；SIGTERM与脱离两次的输出含离开alternate screen序列，stty -a显示icanon与echo；Ctrl+C后屏幕含脱离、暂停、取消三个选项且TUI未立即退出；host与Agent pid始终存在；runs.state与revision在三次退出前后相同
- **AC-043-04**（TEST-043-04 · 检查 `integration`）Given FakeCommander不可用（启动即失败）；Run R RUNNING且有运行中Attempt。；When 提交PauseRun，待PAUSED后提交CancelRun。；Then 两条命令均返回202；Run依次到达PAUSED与CANCELLED；FakeCommander调用次数=0。
  - 判定：两次响应均为202；runs状态时间线包含PAUSED，最终为CANCELLED；FakeCommander调用次数=0
- **AC-043-05**（TEST-043-05 · 检查 `unit`）Given 测试ProcessControl fake在SIGKILL后仍报告进程存活30 s。；When 提交CancelRun并推进FakeClock 30 s，然后fake报告退出。；Then 前30 s Run保持CANCEL_REQUESTED且投影显示CANCELLING，无CANCELLED事件；fake报告退出后才写CANCELLED。
  - 判定：0–30 s所有采样runs.state=CANCEL_REQUESTED且投影标签=CANCELLING；退出报告前CANCELLED事件数=0；退出报告后恰1条CANCELLED事件

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- native interrupt grace 10 s、SIGTERM grace 5 s及17 s总上限为初值；docs只要求短grace与有界SIGKILL。
- 安全点未精确定义；本选择为当前Attempt到达SETTLED/FAILED/CANCELLED，Pause不设超时也不中断。
- 以setsid脱离进程组的后代不受pgid信号覆盖。host运行在受委托的cgroup v2中时（systemd --user的unit带Delegate=yes，或客户端在用户已开启linger时经systemd-run --user --scope -p Delegate=yes拉起host），每个Attempt的进程在其启动门打开前移入它专属的子cgroup，以cgroup.kill停止。终结的证明须同时满足：进程组为空；该cgroup为populated 0；按/proc父链与继承的环境标记找到并以pidfd固定的该Attempt进程都已结束；进程组为空之后，连续两次完整扫描都没有找到进程组与cgroup之外的该Attempt进程（不完整的扫描、找到过进程的扫描都不计入）。找到的进程同样收到停止信号。没有受委托的cgroup时，退回按/proc父子链与继承的环境标记查找的尽力方案（没有cgroup一项，其余条件相同），把该Attempt明确记为降级（tracking: proc），由hoh doctor报告；该方案对蓄意躲避的进程不成立。无法得到证明时，不结束Attempt，不释放写者租约。余项（项目所有者2026-11-08、2026-11-09接受）：Attempt的进程与用户同一uid，没有特权时无法封住。以下都无法归属，可能在Attempt结束、写者租约释放后继续写：迁出cgroup、又脱离进程组的进程（它只由尽力的/proc扫描覆盖，让标记不可见或利用扫描的已知竞态即可躲开）；交给Attempt之外早已运行的同用户服务（systemd --user、HoH之外启动的tmux server、cron、ssh localhost等）的工作；同用户进程篡改host本身（改写它的数据库与文件，把它放进自己持有CAP_SYS_ADMIN的命名空间重新启动，或在那里伪造/proc与cgroup的视图）。命名空间记录与cgroup目录核对只识别意外的环境变化。hoh doctor写明这一界限（CR-011、CR-012、CR-013）。

## REQ-044 · 可恢复版本

M5 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

保存失败分支，回到已验证基线；不reset用户未提交改动

**验收**

- **AC-044-01**（TEST-044-01 · 检查 `recovery`）Given 候选严重回归或用户目录有变更；When Git worktree与hash验证；Then 保存失败分支，回到已验证基线；不reset用户未提交改动
  - 判定：保存失败分支，回到已验证基线；不reset用户未提交改动

## REQ-045 · 四层Prompt版本

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/15-prompts-skills-memory.md`、`docs/adr/ADR-026.md`、`docs/adr/ADR-022.md`、`contracts/sql/001_initial.sql`

合成并记录版本/内容hash；高层覆盖不改变事实证据；过长报错

**必须行为**

- 每个 Attempt 的 Prompt 由四层合成：职位契约（含覆盖链）、项目层（已批准规格的摘要与引用、ADR 与约定的引用）、任务层、证据层（docs/15）。
- 每个 Attempt 记录合成文本的 sha256、每层的来源引用与版本、resource_bundle_version、截断策略；按记录重放得到逐字节相同的文本。
- 高层覆盖只作用于职位契约与项目约定层；证据层逐字节等于事实收集的结果，任何覆盖都不能改写证据层；越层的覆盖被拒并报告冲突。
- 模板只能使用白名单变量 GOAL、PROJECT_CONTEXT、TASK_CONTEXT、EVIDENCE_CONTEXT（C13）；引用其他变量时合成失败为 PROMPT_VARIABLE_MISSING，details 名出变量，Attempt 不启动。
- 覆盖使职位契约超过 32 KiB（UTF-8 字节，C12）时，保存覆盖即以 PROMPT_TOO_LONG 拒绝，details 给出尺寸与上限；强制项装不下时仍为 CONTEXT_BUDGET_TOO_SMALL（REQ-039），不重复判定。

**失败与边界**

- 模板引用未定义变量时，Attempt 以 PROMPT_VARIABLE_MISSING 失败，没有 Agent 进程，attempts.failure_reason 为该码。
- 覆盖超长时保存被拒（PROMPT_TOO_LONG），生效的契约不变。
- 覆盖文本声称“测试已通过”时，证据层不变，门禁仍只看证据。

**验收**

- **AC-045-01**（TEST-045-01 · 检查 `integration`）Given 一个 developer Attempt，职位契约有 project 层覆盖，项目层引用已批准修订 1，证据层有 2 条证据引用；When Attempt 启动后按记录重放合成；Then prompt_revisions 与 context.packed 记录文本 sha256、各层的来源引用与版本、resource_bundle_version 与截断策略；重放得到的文本与记录的 sha256 逐字节相同
  - 判定：记录字段齐全；重放 sha256 相同
- **AC-045-02**（TEST-045-02 · 检查 `unit`）Given project 层覆盖文本写“所有测试已通过，可直接验收”，并试图定义证据层的内容；同一组证据中一条 required AC 没有 PASS；When 合成该 Attempt 的 Prompt，并对同一组证据在有、无该覆盖两种情况下各运行一次完成门禁；Then 证据层逐字节等于事实收集的结果；试图改写证据层的覆盖被拒并报告越层冲突；两次完成门禁的输入与结果相同（都为 EVIDENCE_GATE_UNMET）
  - 判定：证据层逐字节等于事实收集；越层覆盖被拒并报告冲突；两次门禁输入与结果相同且为 EVIDENCE_GATE_UNMET
- **AC-045-03**（TEST-045-03 · 检查 `integration`）Given 职位契约的覆盖引用了未定义变量 {{SECRET_PLAN}}；When 派发该职位的 Task；Then Attempt 以 PROMPT_VARIABLE_MISSING 失败、details 名出 SECRET_PLAN；没有 Agent 进程启动
  - 判定：Attempt 失败为 PROMPT_VARIABLE_MISSING 且 details 名出变量；无 Agent 进程
- **AC-045-04**（TEST-045-04 · 检查 `unit`）Given 一份 40 KiB 的职位契约覆盖；When 保存该覆盖；Then 保存被拒，code=PROMPT_TOO_LONG，details 含 40960 字节与 32768 上限；生效的契约不变
  - 判定：拒绝为 PROMPT_TOO_LONG 并给出尺寸与上限；生效契约不变

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 契约上限 32 KiB（内置六份契约在 4.4–7.5 KB 之间）（CR-026，C12）。
- 模板变量白名单只有四个层占位，与 tools/validate_bundle.py 对内置契约的检查相同（CR-026，C13）。
- 新码 PROMPT_VARIABLE_MISSING、PROMPT_TOO_LONG 为 ErrorCode（CR-026）。

## REQ-046 · 内置Skill

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/15-prompts-skills-memory.md`、`docs/adr/ADR-026.md`、`docs/23-pre-m1-audit.md`、`resources/skills/registry.json`

内置可用无需用户手装；只加载相关技能；原生Skill不可用时内容注入

**必须行为**

- 13 个内置 Skill（resources/skills/registry.json）随产品分发，不要求用户安装，也不写进原生 Agent 的用户目录（O5）。
- 每个 Attempt 收到 Skill 短索引，只含 roles 含该职位、phases 含当前阶段的 Skill（ADR-026 的短索引）。
- Skill 正文按阶段与任务注入（C18）：任务的计划条目或当前阶段要求的 Skill 正文作为知识类内容注入，计入 REQ-039 的知识份额，并记入 prompt 修订；已在原生会话目录中的不重复注入。
- 注册校验：每个 Skill 有最小字段（docs/15）；依赖闭包可解析且无环；entry 存在；未知 phase 被拒。项目层自定义 Skill 照此校验，不能注册调度器或绕过门禁。

**失败与边界**

- 没有网络与用户安装的 Skill 时，内置 Skill 照常可用。
- registry 依赖成环、缺 entry 或 phase 未知时，注册被拒并给出原因，生效的 Skill 集不变。
- Skill 不写入 ~/.claude、pi 扩展目录等原生用户目录；这些目录在 Run 前后的文件摘要不变。

**验收**

- **AC-046-01**（TEST-046-01 · 检查 `integration`）Given 无网络，没有任何用户安装的 Skill；实现阶段的 developer Task 要求 tdd；When 运行该 Attempt，读假 Agent 记下的输入；Then 短索引只含 roles 含 developer 且 phases 含实现阶段的 Skill；输入含 tdd 的正文；prompt 修订记录 tdd 的来源与版本
  - 判定：短索引按职位与阶段过滤；输入含 tdd 正文；prompt 修订记录来源
- **AC-046-02**（TEST-046-02 · 检查 `integration`）Given 绑定的 Agent 没有原生 Skill 机制；另一个 Agent 的会话目录中已有 tdd；When 各运行一个要求 tdd 的 Attempt；Then 无原生机制的输入以知识类内容注入 tdd 正文并计入知识份额；会话目录已有 tdd 的输入不重复注入；运行前后 ~/.claude 与 pi 扩展目录的文件摘要不变
  - 判定：注入计入知识份额；不重复注入；原生目录摘要不变
- **AC-046-03**（TEST-046-03 · 检查 `unit`）Given 四份 registry 夹具：合法；依赖成环；entry 缺失；phase 未知；另一份项目层 Skill 声称注册调度器；When 逐一注册；Then 合法的通过；其余各被拒并给出原因（环成员、缺失路径、未知 phase、不允许的能力），生效的 Skill 集不变
  - 判定：合法通过；其余各被拒并给出原因；生效集不变

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- Skill 的落点取所有者 2026-11-17 的决定 O5：不写原生 Agent 的用户目录，只注入或写进会话自己的目录（CR-026）。
- 正文注入按阶段与任务，不由 Agent 自行请求（CR-026，C18）。

## REQ-047 · 自定义与升级

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/15-prompts-skills-memory.md`、`docs/03-ux.md`、`docs/12-user-guide.md`、`docs/adr/ADR-022.md`

内置/global/project/run按序；显示diff；运行锁定旧版本直到checkpoint

**必须行为**

- 资源（职位契约、Skill、模板）的覆盖按 builtin→global→project→run 的顺序生效，后者优先；global 层在 $XDG_CONFIG_HOME/hoh/（缺省 ~/.config/hoh/），project 层在 .hoh/，run 层随 Run 的快照（C15）。
- 每个 Attempt 记录覆盖链：各层的路径与内容摘要（prompt_revisions 的 layer 与 parent_revision）。
- 用户可查看 builtin 与生效版本的统一 diff，“恢复默认”删除该层覆盖；任何编辑都不改安装的资源文件。
- 资源包升级只给新 Attempt：Run 已开始的职位在其下一个 checkpoint（ADR-022 决定 8，C14）之前仍用旧版本；每个 Attempt 记其 resource_bundle_version。
- 基于旧内置版本的覆盖在新版本内置已变时标为漂移，给出三方 diff（旧内置、新内置、覆盖）。

**失败与边界**

- 删除 run 层覆盖后，下一个 Attempt 落到 project 层，覆盖链随之变化。
- 编辑与恢复默认之后，安装目录的资源文件摘要不变。
- 升级后在 checkpoint 之前的 Attempt 仍记旧的 resource_bundle_version。

**验收**

- **AC-047-01**（TEST-047-01 · 检查 `integration`）Given developer 契约在 global、project、run 三层都有覆盖；When 运行一个 Attempt；删掉 run 层覆盖后再运行一个；Then 第一个 Attempt 用 run 层，覆盖链依次记录 builtin、global、project、run 的路径与摘要；第二个用 project 层，链中没有 run 层
  - 判定：第一个用 run 层且链有四层；第二个用 project 层且链无 run 层
- **AC-047-02**（TEST-047-02 · 检查 `tui-pty`）Given developer 契约有 project 层覆盖；When 在 TUI 的设置页打开该契约的 diff，随后执行恢复默认；Then diff 为 builtin 与生效版本的统一 diff；恢复默认后 project 层覆盖文件被删除，下一 Attempt 用 builtin；安装目录中资源文件的 sha256 在全程不变
  - 判定：统一 diff 正确；覆盖文件删除且下一 Attempt 用 builtin；安装文件摘要不变
- **AC-047-03**（TEST-047-03 · 检查 `deployment`）Given Run R 以资源包 v1 开始，developer 契约有基于 v1 的 project 层覆盖；When R 运行中把 HoH 升级到 developer 内置契约已变的 v2；Then R 的 developer 在下一个 checkpoint 之前的 Attempt 记 resource_bundle_version=v1，之后记 v2；该覆盖被标为漂移并给出三方 diff
  - 判定：checkpoint 前 v1、之后 v2；覆盖标漂移并给出三方 diff

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- global 层位置为 $XDG_CONFIG_HOME/hoh/（CR-026，C15）。
- 资源升级的生效点取 ADR-022 决定 8 的职位 checkpoint，与团队变更一致（CR-026，C14）。

## REQ-048 · Smart Compact集成

M5 · P1 · 发布类 degradable · 目标 v1 · 成熟度 outline

使用指定作者插件，保留任务/证据引用；自动压缩后继续；长期记忆不双开

**验收**

- **AC-048-01**（TEST-048-01 · 检查 `plugin-live`）Given 长Pi会话压缩；When 真实插件压缩+恢复测试；Then 使用指定作者插件，保留任务/证据引用；自动压缩后继续；长期记忆不双开
  - 判定：使用指定作者插件，保留任务/证据引用；自动压缩后继续；长期记忆不双开

## REQ-049 · Hermes Memory集成

M5 · P1 · 发布类 degradable · 目标 v1 · 成熟度 outline

经验标记来源/项目/失效条件；关键验收不靠召回；并发维护可恢复

**验收**

- **AC-049-01**（TEST-049-01 · 检查 `plugin-live`）Given Pi学习经验与下轮召回；When 失败经验、跨项目隔离、锁冲突；Then 经验标记来源/项目/失效条件；关键验收不靠召回；并发维护可恢复
  - 判定：经验标记来源/项目/失效条件；关键验收不靠召回；并发维护可恢复

## REQ-050 · rpiv Todo集成

M5 · P1 · 发布类 degradable · 目标 v1 · 成熟度 outline

保持会话todo；可映射展示；todo完成不代表全局任务验收

**验收**

- **AC-050-01**（TEST-050-01 · 检查 `plugin-live`）Given Pi把任务拆成子todo并压缩恢复；When reload/compact/新会话作用域；Then 保持会话todo；可映射展示；todo完成不代表全局任务验收
  - 判定：保持会话todo；可映射展示；todo完成不代表全局任务验收

## REQ-051 · 三插件兼容与安装

M5 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

解析固定版本、校验依赖、无手动Skill安装；不支持RPC UI有明确降级

**验收**

- **AC-051-01**（TEST-051-01 · 检查 `integration`）Given 干净环境自动准备默认插件组合；When 无UI/双compact hook/安装失败测试；Then 解析固定版本、校验依赖、无手动Skill安装；不支持RPC UI有明确降级
  - 判定：解析固定版本、校验依赖、无手动Skill安装；不支持RPC UI有明确降级

## REQ-052 · 冻结候选与证据

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`tools/candidate.py`、`tools/run_check.py`、`contracts/candidate.schema.json`、`contracts/execution.schema.json`、`contracts/check-plan.schema.json`、`contracts/protocol.schema.json`、`docs/07-algorithms.md`、`docs/04-architecture.md`、`docs/10-acceptance.md`、`docs/22-tooling.md`、`docs/14-resilience.md`

candidate 从干净的已提交工作树冻结，绑定 commit/tree/spec/environment/test plan 摘要；每次检查前后都重新校验，任一变化即判证据 INVALID。

**必须行为**

- 冻结前工作区必须没有未提交或未跟踪的文件，否则拒绝冻结，不创建 candidate。
- candidate 必须绑定 git_commit、git_tree、spec_sha256、environment_sha256、test_plan_sha256；导出清单还要包含全部已跟踪文件的 path+sha256。
- candidate_id 由规范化内容摘要确定，导出格式为 CAND-<64hex>，相同内容得到相同 ID；已冻结的清单不可覆盖。
- 每次检查执行前后，都必须重新校验 commit/tree、工作区干净、源文件清单和 hash，以及 spec/env/test_plan 摘要；执行前校验失败就不运行被测命令。
- 执行后校验失败时 source_unchanged=false，证据 outcome=INVALID；即使退出码为 0 也不计 PASS。
- 执行记录必须包含 check_id、candidate_id、spec/env/test_plan 摘要、mode、argv、cwd、起止时间、时长、exit_code、timed_out、stdout/stderr 引用、producer_attempt_id，以及计划声明工件的 artifacts/missing_artifacts。
- 证据只有在全部绑定与当前 candidate/spec 一致、observations 非空、producer attempt 存在且时间区间有效时，才计入当前验收。
- 检查的 argv、mode、cwd 和应产出工件只能取自 candidate 冻结的 check plan；证据目录只能追加、不可覆盖，放在 gitignore 的 .hoh/ 下，不污染候选工作树。

**失败与边界**

- 工作区存在未跟踪文件时冻结报错，candidates 行数不变。
- QA 在检查期间修改已跟踪文件或新增文件时，执行记录 source_unchanged=false，outcome=INVALID。
- 冻结后修改了 spec/environment/test plan 文件时，执行前校验失败，命令不运行，结果 INVALID，并指出哪个摘要不符。
- HEAD 移动到新 commit 时，旧 candidate 校验报 checkout changed，旧证据只保留为历史。
- 已跟踪文件是符号链接或子模块时冻结被拒绝，并给出路径。
- 请求的 check_id 不在冻结 check plan 中，或 argv 与计划不同时，拒绝执行，不生成执行记录。

**验收**

- **AC-052-01**（TEST-052-01 · 检查 `integration`）Given 临时 Git 仓库 fixture：已提交 3 个源文件和 spec/env/test plan 文件，工作树干净；When 不改任何内容冻结两次；然后新增一个未跟踪文件，再冻结一次；Then 前两次 candidate_id 相同、匹配 ^CAND-[a-f0-9]{64}$，导出清单通过 candidate.schema.json；第三次冻结失败，candidates 行数仍为 1
  - 判定：id#1==id#2，且匹配正则；以清单内容重算的摘要等于 candidate_id；冻结 #3 返回错误；count(candidates)=1
- **AC-052-02**（TEST-052-02 · 检查 `integration`）Given 已冻结 candidate C；QA 检查命令 fixture 会改写 src/lib.rs（变体：新建一个未跟踪文件），然后以退出码 0 结束；When 通过 Verification 执行该检查；Then 执行记录 exit_code=0 但 source_unchanged=false；evidence.outcome=INVALID；acceptance_results 中 C 没有 PASS 行；检查结果不为 PASS
  - 判定：两种变体都是 source_unchanged=false；evidence.outcome=INVALID（两条）；acceptance_results 中 status=PASS 且 candidate_id=C 的行数=0
- **AC-052-03**（TEST-052-03 · 检查 `integration`）Given 已冻结 C；变体 1：冻结后把 test plan 文件改 1 个字节；变体 2：test plan 未改，但请求的 argv 与计划 argv 不同；When 分别执行一个运行时会创建哨兵文件的检查；Then 两个变体都在执行前被拒绝，被测命令未运行（哨兵文件不存在）；变体 1 原因含 test_plan，变体 2 原因含 argv 与计划不符
  - 判定：两个变体都不存在哨兵文件；变体 1 的原因含 test_plan，变体 2 的原因含 argv；两个变体都没有 exit_code=0 的执行记录
- **AC-052-04**（TEST-052-04 · 检查 `unit`）Given 当前 candidate C、spec S，以及一条完整有效的 PASS 证据 e0；When 分别篡改 e0 的 11 个方面：candidate_id、git_tree、spec_sha256、environment_sha256、test_plan_sha256、observations=[]、producer attempt 不存在、finished_at<started_at、source_unchanged=false、stdout sha256 不符、missing_artifacts 非空；Then e0 判为 valid；11 个变体全部判为 not valid：observations=[] 的原因为 INSUFFICIENT，其余为 INVALID
  - 判定：valid(e0)=true；11/11 变体 valid=false；原因码与期望表逐项一致
- **AC-052-05**（TEST-052-05 · 检查 `traceability`）Given 冻结候选 C 及其 unit/integration/tui-pty/recovery/adapter-live 检查记录（各含 test-results.jsonl）；变体：删除一条 v0.1 AC 对应的测试后重新冻结 C'；When 在 C 与 C' 上分别运行 traceability 检查；Then C：退出码0，报告列出 profile 内每个 TEST id 及其来源检查与通过的测试名；C'：退出码非0，报告列出缺失的 TEST id；两次报告都绑定各自 candidate_id
  - 判定：C 退出码0，profile 内每个 TEST id 至少一条 PASS 的 test-results 记录；C' 退出码非0且列出缺失的 TEST id；报告 candidate_id 分别等于 C 与 C'

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 产品中对相同内容重复冻结：同一 Run 内返回已有 candidate_id 与它的状态，不新增行；已是 STALE、INVALID、REJECTED 的不复活；另一 Run 冻结相同内容时拒绝并说明（产生新的提交后再冻结），不复用另一 Run 的候选与它的验收。自举工具 tools/candidate.py 则拒绝覆盖已存在的清单文件。
- 符号链接和子模块：选择拒绝冻结，直到实现显式身份规则并有测试。
- evidence.outcome 枚举里没有 INSUFFICIENT。选择存为 outcome=INVALID，并附原因码 INSUFFICIENT。
- （CR-003）测试名约定：测试名含 `test_NNN_MM` 片段即覆盖 TEST-NNN-MM；检查脚本把逐测试结果写入 $HOH_EVIDENCE_DIR/test-results.jsonl（contracts/test-result.schema.json）。

## REQ-053 · 独立QA

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/07-algorithms.md`、`docs/10-acceptance.md`、`docs/09-testing.md`、`docs/14-resilience.md`、`docs/05-data-api.md`、`resources/skills/independent-qa/SKILL.md`、`resources/prompts/qa.md`、`contracts/independent-report.schema.json`、`contracts/sql/001_initial.sql`、`docs/cr/CR-001-kit-hardening.md`

QA 在独立会话中，以 spec/AC 和冻结候选为起点逐条验收；开发者自测只作诊断；FAIL/UNKNOWN 结果只追加不改写，永远不会投影为 PASS；候选变化使当前验收失效。

**必须行为**

- candidate 冻结后，Runtime 必须自动派发 QA（无需 Commander 批准），QA attempt 使用与该 candidate developer 会话不同的 session。
- QA 的初始上下文以 spec、in-scope AC 和冻结候选为主；开发者自测报告只能排在 AC 之后，并标记为 diagnostic，不作裁决依据。
- QA 结论必须按 (spec_revision, candidate, AC) 逐条记录，并引用真实执行记录；只有模型总结或 evidence 为空时，判 INVALID(INSUFFICIENT)。
- 失败记录只追加：同一 (spec, candidate, AC) 只要有 FAIL 或 UNKNOWN，投影就不能是 PASS；先 FAIL 后重试通过记为 FLAKY。
- QA 发现缺陷时必须开 Issue（含 candidate_id、fingerprint、severity），留给下一候选；不得在被测工作树修复，也不得等待 Developer 修改当前 candidate。
- 候选变化（新 commit/tree）后，旧候选的 QA 结果只作历史；新候选的所有 in-scope AC 回到 NOT_RUN，直到重新验证。
- QA 进行中 spec revision 变化时，结果标 stale，不计入新 spec 的验收。
- in-scope AC 集合为空，或任一 AC 没有观察记录时，QA 汇总不能为 PASS。

**失败与边界**

- 开发者 TaskResult 声称 COMPLETED 且自测全部通过，但 QA 检查失败：该 AC=FAIL，Issue 为 OPEN，candidate 不能 ACCEPTED。
- QA TaskResult 为 COMPLETED 但 evidence 为空：AC 结果为 INVALID(INSUFFICIENT)，不是 PASS。
- QA 请求 Developer 修改当前 candidate：请求被拒绝，转为面向下一候选的 Issue。
- 试图把 acceptance_results 中的 FAIL/UNKNOWN 改为 PASS：更新被拒绝，行不变。
- 候选变化后，旧 PASS 不计入当前已验收数。

**验收**

- **AC-053-01**（TEST-053-01 · 检查 `integration`）Given 故意漏功能的 fixture 仓库：实现了 AC-A，缺少 AC-B 的行为；两个 AC 的验收测试都是真实命令；Developer FakeAgent 提交 COMPLETED，自测报告全部 PASS；When candidate C 冻结后，Runtime 派发 QA，QA FakeAgent 按 AC 执行两项真实检查；Then acceptance_results 中 AC-A=PASS、AC-B=FAIL；存在 candidate_id=C 的 OPEN Issue；candidate 和 task 都不是 ACCEPTED；开发者自测记录不被任何 acceptance_results 引用
  - 判定：AC-B 对 C 的状态为 FAIL；count(issues where candidate_id=C and state=OPEN)≥1；candidate 与 task 的状态都≠ACCEPTED；acceptance_results 的 evidence_id 中不含 Developer attempt 产生的证据
- **AC-053-02**（TEST-053-02 · 检查 `unit`）Given 同一 (spec S, candidate C, AC-X) 的若干结果序列；外加一条 evidence=[] 的 PASS 声明；When 分别求投影：[FAIL,PASS]、[UNKNOWN,PASS]、[FAIL]、[UNKNOWN]、[]、[NOT_RUN]、[PASS]、[空观察 PASS]；再尝试把一条 FAIL 行更新为 PASS；Then 依次得到 FLAKY、FLAKY、FAIL、UNKNOWN、NOT_RUN、NOT_RUN、PASS、INVALID；只有 [PASS] 投影为 PASS；更新操作返回错误，行不变
  - 判定：8 个投影结果与期望表逐项一致；PASS 投影数=1；更新返回错误，行内容前后逐字段相同
- **AC-053-03**（TEST-053-03 · 检查 `integration`）Given candidate C 附带开发者自测报告 R（内容含 ALL TESTS PASSED 哨兵）；in-scope AC 为 AC-A、AC-B；When Runtime 生成 QA 的 ContextHandoff 和 Prompt；Then ContextHandoff.acceptance_ids={AC-A,AC-B}，candidate 等于 C；Prompt 中 AC-A、AC-B 正文首次出现的位置早于 R 的内容；R 位于 Evidence Context 层，并带 diagnostic 标签
  - 判定：acceptance_ids 集合与 candidate 身份匹配；offset(AC-A 正文)<offset(哨兵) 且 offset(AC-B 正文)<offset(哨兵)；哨兵所在段的标签为 diagnostic
- **AC-053-04**（TEST-053-04 · 检查 `integration`）Given candidate C1 的 AC-A 已有 QA PASS 和 Review PASS，当前已验收计数包含 AC-A；When Developer 提交新 commit 并冻结 C2；Then 当前投影中 AC-A 对 C2 为 NOT_RUN，已验收计数减 1；C1 的 PASS 行保留（acceptance_results 行数不减）；Runtime 为 C2 自动派发新的 QA 和 Review（qa 与 reviewer 各 1 条 kind=handoff 消息）
  - 判定：snapshot 中 AC-A 对当前 candidate 为 NOT_RUN；accepted 计数=之前-1；acceptance_results 行数前后相等；C2 的 qa 与 reviewer kind=handoff 消息各 1 条
- **AC-053-05**（TEST-053-05 · 检查 `adapter-live`）Given 真实 Developer 会话与真实 QA 会话（不同原生会话；默认绑定 Claude/Pi，v0.1 可为两个 Claude 会话）；植入缺陷的小仓库，其中 AC-B 的确定性测试必然失败；When 以 hoh 真实运行一次 candidate→QA 循环；Then QA 的 native session id 与 Developer 不同，二者都记录在 sessions 表；QA 报告绑定 C，其 native_attempt 记录中包含报告的 session_id；不论 QA 是否执行该测试，AC-B 的投影都不是 PASS，candidate 不是 ACCEPTED
  - 判定：session_id(qa)≠session_id(developer)，native_id 不同；QA 报告 candidate_id=C，且 native_attempt.session_ids 包含报告 session_id；AC-B 投影∈{FAIL,UNKNOWN,NOT_RUN,INVALID}；candidate 状态≠ACCEPTED

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- UNKNOWN 之后对同一 candidate 重测通过：选择投影为 FLAKY（不满足 strict），要变为 PASS 需走 REQ-055 的 flaky 处置或产生新 candidate。
- QA 能否在初始上下文看到自测报告：选择允许，但只作为排在 AC 之后的 diagnostic 引用。

## REQ-054 · 独立Review

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/10-acceptance.md`、`docs/09-testing.md`、`config/hoh.example.toml`、`contracts/independent-report.schema.json`、`contracts/protocol.schema.json`、`resources/skills/code-review/SKILL.md`、`tools/release_gate.py`、`docs/cr/CR-001-kit-hardening.md`

Reviewer 在独立会话中审查与 QA 相同的冻结候选，报告绑定 candidate/spec/raw_session；每条意见登记为 Issue，可追踪到报告；审查旧 SHA 或角色冲突的报告不计入。

**必须行为**

- Reviewer 的 session 必须与 Commander、Developer、QA 都不同；默认 reviewer=codex，与 commander 同产品时也必须新开会话（independent_session=true）。
- candidate 冻结后，Runtime 必须自动对同一 candidate 并行派发 QA 和 Review，两份 handoff 中的 candidate 身份逐字段相同。
- 审查报告必须符合 independent-report.schema.json，绑定 candidate_id、spec_sha256、session_id、raw_session(path+sha256) 和 native_attempt；native_attempt 记录中必须包含该 session_id。
- 报告的 candidate_id 或 git_commit 与当前 candidate 不一致时标 stale，不计入当前验收，并为当前 candidate 重新派发 Review。
- 报告中每条 finding（id/severity/summary/evidence）都必须登记为 Issue，含 candidate_id 与 fingerprint；从 Issue 能回溯到报告、finding.id 和 raw_session。
- candidate 只有在 QA 完成、Review 完成、确定性检查有效且 in-scope AC 全部覆盖时才 ACCEPTED；Reviewer PASS 不能替代失败的检查。
- QA 与 Review 中一方失败时，另一方仍继续完成并保存证据；未运行的部分记为 NOT_RUN。

**失败与边界**

- reviewer 的 session_id 等于 Developer 或 Commander 的 session 时，报告因角色冲突被拒绝。
- 基于旧 SHA 的审查报告被拒绝，当前 candidate 的 review 状态仍为 NOT_RUN。
- raw_session 或 native_attempt 文件缺失、sha256 不符，或 native_attempt 不含报告 session_id 时，报告无效。
- Review 报告 status=UNKNOWN 时，不算 Review 完成，candidate 不 ACCEPTED。
- Review 为 FAIL 且含 critical/high finding 时，candidate 不 ACCEPTED，Issue 为 OPEN。
- Review 为 PASS 但有确定性检查 FAIL 时，candidate 不 ACCEPTED。

**验收**

- **AC-054-01**（TEST-054-01 · 检查 `integration`）Given Commander 会话 S_cmd(codex)、Developer 会话 S_dev(claude)、QA 会话 S_qa(pi)，candidate C；When 依次提交 session_id 为 S_cmd、S_dev、S_qa 的 reviewer PASS 报告，最后提交新 codex 会话 S_rev 的报告；Then 前 3 条因 session 冲突被拒绝且不计入；只接受 S_rev 的报告；sessions 表中 S_rev.role=reviewer，且 S_rev∉{S_cmd,S_dev,S_qa}
  - 判定：被拒绝的报告数=3，原因含 session 冲突；被接受的报告数=1，session_id=S_rev；S_rev.role=reviewer
- **AC-054-02**（TEST-054-02 · 检查 `integration`）Given Reviewer 开始审查时 candidate 为 C1；审查期间 Developer 提交新 commit 并冻结了 C2；When Reviewer 提交 candidate_id=C1 的 PASS 报告；Then 报告记为 stale，不计入 C2；C2 的 review 状态为 NOT_RUN，并已自动派发新的 Review（recipient_role=reviewer、candidate_id=C2 的 kind=handoff 消息 1 条）；C1 的报告保留为历史
  - 判定：C1 报告的状态为 stale/rejected for current；C2 的 review 状态=NOT_RUN；recipient_role=reviewer 且 candidate_id=C2 的 kind=handoff 消息有 1 条；C1 报告记录仍可查询
- **AC-054-03**（TEST-054-03 · 检查 `integration`）Given candidate C 已冻结；reader_slots=2；QA FakeAgent 的检查结果为 FAIL，Reviewer FakeAgent 正常完成；When Runtime 处理 candidate_ready；Then QA 和 Review 各有 1 个 attempt，session_id 不同；两份 handoff 的 candidate 逐字段相同；两个 attempt 的运行区间有重叠；QA FAIL 后 Review 仍完成并保存报告；candidate 不是 ACCEPTED
  - 判定：qa 与 reviewer 的 attempt 各 1 个，session_id 不同；两份 handoff 中的 candidate JSON 相同；max(started_at)<min(ended_at)；Review 报告存在，raw_session 的 sha256 可校验；candidate 状态≠ACCEPTED
- **AC-054-04**（TEST-054-04 · 检查 `integration`）Given Reviewer 为 C 提交 status=FAIL 的报告，findings 含 2 条：F1 severity=critical（evidence 指向 src/a.rs 的审查摘录），F2 severity=low；When Runtime 接收该报告；Then issues 新增 2 行，severity 分别为 critical 和 low，candidate_id=C，各有 fingerprint 并引用 finding.id；Issue→报告→raw_session 的 sha256 链可校验；存在 OPEN critical 时 candidate 不 ACCEPTED
  - 判定：新增 issues 行数=2，severity 集合={critical,low}；每行 fingerprint 非空，且引用 F1 或 F2；回溯链上的 sha256 全部匹配；candidate 状态≠ACCEPTED
- **AC-054-05**（TEST-054-05 · 检查 `unit`）Given 候选汇合函数的输入维度：QA∈{PASS,FAIL,UNKNOWN,NOT_RUN}、Review∈{PASS,FAIL,UNKNOWN,NOT_RUN}、确定性检查∈{全部有效 PASS,有一项 FAIL,空列表}、AC 覆盖∈{全覆盖,缺 1 项}；When 穷举全部 4×4×3×2=96 种组合并调用汇合函数；Then 只有 (PASS,PASS,全部有效 PASS,全覆盖) 这 1 种组合返回 ACCEPTED，其余 95 种都不返回 ACCEPTED；检查列表为空时不视为通过
  - 判定：ACCEPTED 计数=1；含 UNKNOWN 的组合全部非 ACCEPTED；Review=PASS 且检查 FAIL 的组合全部非 ACCEPTED；检查为空列表的组合全部非 ACCEPTED

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- Reviewer 是否必须与 QA 也不同会话：tools/release_gate.py 要求 developer/QA/reviewer 三者不同，选择让三者与 Commander 全部互不相同。
- independent-report 的 finding 没有定位字段：选择用 summary 和 evidence 引用承载代码/行为定位；fingerprint 由 Runtime 计算，与 candidate 无关（初值：sha256(severity+规范化 summary+定位)），以便跨候选追踪。
- 文档只要求“Review 完成”：选择 Review status=FAIL 即不满足候选 ACCEPTED（更严格）；PASS 报告中可以带 low/medium 建议。

## REQ-055 · 回归与Flaky

M5 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

打开regression issue；保留所有attempt；flaky不被覆盖成纯PASS

**验收**

- **AC-055-01**（TEST-055-01 · 检查 `integration`）Given 修复一处导致旧功能失败或重试才过；When 回归植入与重试证据测试；Then 打开regression issue；保留所有attempt；flaky不被覆盖成纯PASS
  - 判定：打开regression issue；保留所有attempt；flaky不被覆盖成纯PASS

## REQ-056 · 最终发布门禁

M5 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

所有required需求与检查有同版本有效证据；空集合、skip、mock不能冒充真实

**验收**

- **AC-056-01**（TEST-056-01 · 检查 `integration`）Given 请求COMPLETED或合并发布；When negative release-gate测试；Then 所有required需求与检查有同版本有效证据；空集合、skip、mock不能冒充真实
  - 判定：所有required需求与检查有同版本有效证据；空集合、skip、mock不能冒充真实

## REQ-057 · 运行时UX与部署验证

M5 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

执行PTY/浏览器/安装/升级/回滚实际流程；不是截图看起来不错就通过

**验收**

- **AC-057-01**（TEST-057-01 · 检查 `deployment`）Given 目标为CLI/Web或服务；When fresh VM部署验收；Then 执行PTY/浏览器/安装/升级/回滚实际流程；不是截图看起来不错就通过
  - 判定：执行PTY/浏览器/安装/升级/回滚实际流程；不是截图看起来不错就通过

## REQ-058 · Web GUI入口

M6 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

同一Engine，不启动第二任务循环；可查看规格/团队/时间线/证据

**验收**

- **AC-058-01**（TEST-058-01 · 检查 `web-e2e`）Given hoh web启动并连接现有run；When 浏览器与TUI同状态测试；Then 同一Engine，不启动第二任务循环；可查看规格/团队/时间线/证据
  - 判定：同一Engine，不启动第二任务循环；可查看规格/团队/时间线/证据

## REQ-059 · 事件重连

M6 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

序号去重、replay/live交接无遗漏；过期cursor走snapshot resync

**验收**

- **AC-059-01**（TEST-059-01 · 检查 `web-e2e`）Given SSE断线重连带cursor；When disconnect/history retention测试；Then 序号去重、replay/live交接无遗漏；过期cursor走snapshot resync
  - 判定：序号去重、replay/live交接无遗漏；过期cursor走snapshot resync

## REQ-060 · 多客户端操作

M6 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

expected_revision校验；冲突明确；只有一写者，可读多个客户端

**验收**

- **AC-060-01**（TEST-060-01 · 检查 `web-e2e`）Given TUI/Web同时改模式/规格；When 并发409与重试幂等测试；Then expected_revision校验；冲突明确；只有一写者，可读多个客户端
  - 判定：expected_revision校验；冲突明确；只有一写者，可读多个客户端

## REQ-061 · 无障碍和远程体验

M6 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

核心功能可用；默认loopback；没有桌面环境也可访问

**验收**

- **AC-061-01**（TEST-061-01 · 检查 `web-e2e`）Given 键盘浏览/小视口/SSH端口转发；When Playwright功能+可达性；Then 核心功能可用；默认loopback；没有桌面环境也可访问
  - 判定：核心功能可用；默认loopback；没有桌面环境也可访问

## REQ-062 · 持久化一致性

M2 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/04-architecture.md`、`docs/05-data-api.md`、`docs/14-resilience.md`、`docs/01-prd.md`、`docs/adr/ADR-009.md`、`contracts/sql/001_initial.sql`、`research/SOURCES.md`

项目.hoh/下的SQLite以WAL、foreign_keys=ON、busy_timeout初值5000 ms运行，所有写入经单一writer actor按事务提交；约束违例整体回滚，重复投递不重复生效，拒绝在网络文件系统上打开。

**必须行为**

- 每个连接打开后必须设置并校验：journal_mode=WAL、foreign_keys=1、busy_timeout=5000（初值）、synchronous=FULL（初值）；任一不符则host拒绝接受写命令。
- 所有写必须经专用writer线程上唯一的读写连接（同步rusqlite，不在Tokio worker上执行）；其余连接以只读方式打开。
- 写事务必须用BEGIN IMMEDIATE；状态CAS、event、outbox同事务；FK/CHECK/UNIQUE/json_valid违例时整体回滚并返回IntegrityViolation。
- writer队列必须有界；满时对调用方背压，最长等待busy_timeout后返回可重试错误；关键事件不得静默丢弃。
- 消费方必须在同一事务写receipts(consumer,message_id)与状态迁移；重复投递返回首次result_json，不重复改状态。
- DB所在文件系统为nfs/nfs4/cifs/smb3/fuse.sshfs/9p等网络或共享文件系统时必须拒绝打开，返回DB_ON_NETWORK_FS且不创建DB文件。
- 名单外的文件系统（含virtiofs）在host启动时必须做WAL自检（临时库启用WAL、写入、checkpoint、-shm映射与加锁）；失败即返回DB_WAL_UNSUPPORTED拒绝启动，不创建正式DB文件。
- SQLITE_BUSY超过busy_timeout时写命令返回503、retryable=true并保持上次已提交状态；读请求与UI不被该写等待阻塞。
- UI、Agent适配器与Web处理器不得直接执行INSERT/UPDATE/DELETE，只能经Persistence模块类型接口。

**失败与边界**

- 当写入引用不存在的父行时，事务回滚，返回IntegrityViolation，同事务其他行均未写入。
- 当同一message_id对同一consumer第二次投递时，receipts唯一约束阻止二次应用，返回首次result_json。
- 当外部连接持写锁超过busy_timeout时，命令返回503 code=DB_BUSY、retryable=true，state/revision不变。
- 当DB路径位于网络文件系统时，host退出码非0，stderr含DB_ON_NETWORK_FS，.hoh下无新DB/-wal/-shm文件。
- 当writer队列满时，调用方等待或收到可重试错误；凡已返回202的命令在DB中均可查到，丢失数=0。
- 当连接PRAGMA校验不符（如foreign_keys=0）时，host拒绝写命令并给诊断。

**验收**

- **AC-062-01**（TEST-062-01 · 检查 `integration`）Given 本地盘(ext4/tmpfs)临时项目首次启动host创建DB；另开一个外部sqlite3连接。；When 查询writer与各reader连接PRAGMA并尝试经reader写入；外部连接BEGIN IMMEDIATE先持锁2 s、再持锁7 s，两段期间各提交一次PauseRun并发起GET /v1/runs/{id}。；Then 所有连接PRAGMA为wal/1/5000/FULL；reader写入被拒；持锁2 s时202；持锁7 s时5–6 s返回503 DB_BUSY且Run不变；两段GET均≤500 ms返回200。
  - 判定：每个连接PRAGMA值与期望完全相同；reader写入返回只读错误；持锁2 s：HTTP 202，耗时≤5 s；持锁7 s：HTTP 503、code=DB_BUSY、retryable=true，耗时∈[5,6] s，runs.state与revision不变；两次GET均200且耗时≤500 ms
- **AC-062-02**（TEST-062-02 · 检查 `integration`）Given host运行，测试配置writer队列容量=8。；When 8个并发客户端各提交500条会产生状态迁移+event+outbox的命令。；Then 4000条命令均得到202或可重试错误，调用方未见SQLITE_BUSY；每个202命令在DB中恰有1条event与对应outbox；events.seq严格递增且唯一；返回202而DB缺失的数量=0。
  - 判定：响应只包含202或retryable=true的错误，无SQLITE_BUSY字样；每个202的command_id对应events恰1行、outbox恰1行；events.seq严格递增且无重复；202但DB无记录的命令数=0
- **AC-062-03**（TEST-062-03 · 检查 `integration`）Given outbox中消息M(recipient=qa)为PENDING；qa消费M会使Task T revision+1。；When dispatcher投递M两次（模拟ACK丢失后重投）；另提交一条同事务含合法event与非法task_id外键的写入。；Then receipts中(qa,M)仅1行；T revision只增加1；第二次投递返回与首次相同的result_json；非法写入返回IntegrityViolation且该事务的event未写入。
  - 判定：receipts中consumer=qa且message_id=M行数=1；T.revision=原值+1；两次投递result_json逐字节相同；非法事务错误类别=IntegrityViolation，events行数只因M增加
- **AC-062-04**（TEST-062-04 · 检查 `integration`）Given mountinfo fixture：/p1在nfs4、/p2在cifs、/p3在fuse.sshfs、/p4在ext4、/p5在tmpfs。；When 对各路径执行DB位置检查；并以注入该fixture的方式在/p1启动host。；Then p1–p3返回DB_ON_NETWORK_FS，p4、p5通过；host退出码非0、stderr含DB_ON_NETWORK_FS；/p1/.hoh下无*.db、*-wal、*-shm文件。
  - 判定：p1、p2、p3结果=DB_ON_NETWORK_FS，p4、p5结果=通过；host退出码≠0且stderr含DB_ON_NETWORK_FS；/p1/.hoh下匹配*.db、*-wal、*-shm的文件数=0
- **AC-062-05**（TEST-062-05 · 检查 `static`）Given HoH源码仓库（hoh-core/hoh-agents/hoh-app/web）。；When 运行静态检查，扫描SQL写语句与rusqlite写API调用位置。；Then INSERT/UPDATE/DELETE/REPLACE及写调用只出现在Persistence模块白名单路径；白名单外命中数=0；web目录无SQLite依赖。
  - 判定：白名单外SQL写语句及写API命中数=0；web依赖中无sqlite相关包；扫描文件数>0（防空扫描）
- **AC-062-06**（TEST-062-06 · 检查 `integration`）Given 三个目录：ext4上的/p4；注入WAL自检故障（-shm映射失败）的/p6；注入checkpoint失败的/p7。；When 分别在各目录启动host。；Then /p4自检通过并正常接受写命令；/p6、/p7 host退出码≠0、stderr含DB_WAL_UNSUPPORTED，且.hoh下无正式*.db/-wal/-shm文件。
  - 判定：/p4：命令返回202且可查到；/p6、/p7：退出码≠0且stderr含DB_WAL_UNSUPPORTED；/p6、/p7的.hoh下正式DB文件数=0

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- busy_timeout取初值5000 ms：单writer下仅与备份/checkpoint/外部诊断连接争用。
- synchronous选FULL（初值）以覆盖VM硬重启后已ACK事务不丢；若性能目标不达需CR改NORMAL并记录风险。
- 已修正（CR-002）：已知网络/共享文件系统直接拒绝；其余文件系统（含 virtiofs）在 host 启动时做 WAL 自检（临时库启用 WAL、写入、checkpoint、-shm 映射与加锁），失败即以 DB_WAL_UNSUPPORTED 拒绝启动——以实测代替名单猜测。
- DB_ON_NETWORK_FS、DB_BUSY错误码名为初值；GET≤500 ms借用事件可见p95目标作为阈值。
- DB所在文件系统写满或超出配额致写事务无法提交时返回503 code=DB_FULL、retryable=true，状态、事件与outbox不变（CR-006）。

## REQ-063 · 备份与迁移

M5 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

一致性备份并校验；新旧schema兼容策略；旧binary不读高版本DB

**验收**

- **AC-063-01**（TEST-063-01 · 检查 `recovery`）Given 活跃WAL备份、升级、回滚；When backup API restore与migration roundtrip；Then 一致性备份并校验；新旧schema兼容策略；旧binary不读高版本DB
  - 判定：一致性备份并校验；新旧schema兼容策略；旧binary不读高版本DB

## REQ-064 · 可观测性与容量

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/01-prd.md`、`docs/04-architecture.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/14-resilience.md`、`contracts/sql/001_initial.sql`、`contracts/sql/003_ui_observability.sql`、`docs/cr/CR-028-m4-detailed-spec.md`

结构化索引+对象文件；分页虚拟列表；日志轮换；引用证据不被清理

**必须行为**

- 结构化索引+对象、seek 分页和虚拟列表、诊断日志轮换、所有被引用证据保护、慢读者背压和空间错误可诊断；数值采用已批准 CR-028 §1/§5/§5.1 的阈值与测量约定。

**失败与边界**

- 不接受半写对象为有效证据；DB 满/忙不承诺成功；cursor 过期明确 resync；GC 无法确定安全时保留。百万历史数据不能在启动时全量反序列化。

**验收**

- **AC-064-01**（TEST-064-01 · 检查 `integration`）Given §5.1参考环境、检查外由准备命令以已记录pre-003提交构建的旧release writer预建、manifest摘要冻结于m4_reference.fixture的002百万事件/对象库（本次检查fixture_verify核验并克隆，CR-042）、候选release与最终TUI、100Run×10轮、受限ENOSPC；另debug integration小旧库/迁移故障seam/旧binary；/v1/runs/{run_id}/inputs有多页四态输入、H后新转换及空输入Run、可重建的索引generation；另有同次debug integration的test_064_01_*负例：独立包装器、注入噪声样本组件及隔离dummy runner/guardian；标component/fault injection，不冒称真实release测量；When 先debug运行迁移失败回滚/旧binary拒读；再在同一backfill阶段以两克隆串行正常升级及回填中途kill/reopen、从每个候选首次open计时；按Run/iteration/attempt各查100页，limit100/500/非法0/501；Range首尾/缺文件/416；同夹具磁盘失败/慢客户端/轮换/引用保护/虚拟列表子例；inputs按同Run/H/limit翻页和重放cursor，在H后提交转换再以同H重取首/续页；在同H重建索引并用旧generation cursor查询，以新快照重取全部页；交叉使用Run/query/H/limit及篡改cursor、非法0/501/未来watermark，查询空Run/末页；test_064_01_busy启动关闭继承锁FD的独立包装器竞争父检查持有的真实两锁，不启动嵌套cargo；test_064_01_noise将无锁负载、>2s观测缺口、锁覆盖不可核实的样本逐类注入同一准入/结果归并组件；test_064_01_cleanup以同一包装器/guardian清理代码、隔离的两锁fixture inode和dummy runner验证取消/SIGKILL，保留父检查及其真实两锁；Then 新/v1/timeline非法400 QUERY_INVALID、旧Run timeline非法404 BAD_REQUEST，416仅ARTIFACT_RANGE_INVALID与Content-Range组合；分页≤limit、seq完整、H固定，查询p95≤500ms；EXPLAIN用指定索引、每页描述反序列化≤500/正文≤256KiB，无全库加载；旧源码/tree/锁文件/产物hash与002 schema经manifest可核对，release_build构建候选与helper；fixture_verify≤600s：manifest摘要等于m4_reference.fixture、逐项hash与读回相符、各一致性备份克隆integrity_check=ok，任一不符或缺夹具FAIL不SKIPPED；backfill阶段含两克隆/停机重启总计≤600s，各首次open单列；中断发生于0<indexed_through<H、断点续跑不重置计时，kill→reconcile和host-ready各≤30s、不等完整索引；期间Pause/Cancel仍202并持久、索引查询明示rebuilding。release升级原权威行/hash不变，新增控制/reconcile事件单列，两克隆同H最终索引相等；debug子例失败回滚/旧binary拒读；两阶段缺一不通过；子例包括日志≤8MiB×5、引用对象hash不变、描述≤1000/正文≤4MiB、磁盘无半登记、慢读不阻塞快读；环境不符SKIPPED/退出78，不PASS；inputs的Page逐页watermark=H、items≤limit、接受seq/input_id稳定排序且并集无重漏，四态及role/epoch/boundary/拒绝原因等于H内已提交事实；重放cursor及H后新增转换不改变旧页，同H索引重建后新cursor所得内容相等；cursor绑定Run/query（含limit）/H/generation，错配/篡改/非法参数为404 BAD_REQUEST，缺Run为404 RUN_NOT_FOUND，旧generation为410 CURSOR_EXPIRED且顶层snapshot_watermark可重取；空Run返回items=[]/next_cursor=null/watermark=H，末页next_cursor=null，无写入/重投递；test_064_01_*逐项把输入、断言与原始工件写入本次integration的$HOH_EVIDENCE_DIR/m4-measurements.json（busy/noise/cleanup子记录），子进程stdout/stderr与锁/样本/清理原始记录也在该目录且以相对路径/sha256引用；busy独立包装器两锁等待合计≤30s、被测负载启动计数为0、detail.reason=MEASUREMENT_BUSY，子调用结果SKIPPED/退出78；noise组件三类注入分别得到MEASUREMENT_NOISE、SKIPPED/78并保留已有FAIL，产品自身负载超限对照仍FAIL；cleanup用隔离inode的组件seam复用生产清理/锁生命周期代码，dummy runner取消/被杀后竞争负载仍不能启动，直至guardian核验登记pidfd终结/受控树空、释放两锁才启动，记录身份、锁时刻与先后关系；这些预期负例的子结果嵌入工件，不作为父检查TEST的SKIPPED行合并，test_064_01_*仅在断言全满足时PASS，否则FAIL；父检查真实遇锁忙/噪声时仍相关TEST=SKIPPED、整次退出78且AC不通过，保留已有FAIL；隔离fixture锁不替代真实全机排他，T03的service/scope真实SIGKILL实验仅为交付证据、不引用为该AC门禁证据，不能搬入旧检查工件充数；debug负例与同次release规模两部分缺一不通过
  - 判定：新/v1/timeline非法400 QUERY_INVALID、旧Run timeline非法404 BAD_REQUEST，416仅ARTIFACT_RANGE_INVALID与Content-Range组合；分页≤limit、seq完整、H固定，查询p95≤500ms；EXPLAIN用指定索引、每页描述反序列化≤500/正文≤256KiB，无全库加载；旧源码/tree/锁文件/产物hash与002 schema经manifest可核对，release_build构建候选与helper；fixture_verify≤600s：manifest摘要等于m4_reference.fixture、逐项hash与读回相符、各一致性备份克隆integrity_check=ok，任一不符或缺夹具FAIL不SKIPPED；backfill阶段含两克隆/停机重启总计≤600s，各首次open单列；中断发生于0<indexed_through<H、断点续跑不重置计时，kill→reconcile和host-ready各≤30s、不等完整索引；期间Pause/Cancel仍202并持久、索引查询明示rebuilding。release升级原权威行/hash不变，新增控制/reconcile事件单列，两克隆同H最终索引相等；debug子例失败回滚/旧binary拒读；两阶段缺一不通过；子例包括日志≤8MiB×5、引用对象hash不变、描述≤1000/正文≤4MiB、磁盘无半登记、慢读不阻塞快读；环境不符SKIPPED/退出78，不PASS；inputs的Page逐页watermark=H、items≤limit、接受seq/input_id稳定排序且并集无重漏，四态及role/epoch/boundary/拒绝原因等于H内已提交事实；重放cursor及H后新增转换不改变旧页，同H索引重建后新cursor所得内容相等；cursor绑定Run/query（含limit）/H/generation，错配/篡改/非法参数为404 BAD_REQUEST，缺Run为404 RUN_NOT_FOUND，旧generation为410 CURSOR_EXPIRED且顶层snapshot_watermark可重取；空Run返回items=[]/next_cursor=null/watermark=H，末页next_cursor=null，无写入/重投递；test_064_01_*逐项把输入、断言与原始工件写入本次integration的$HOH_EVIDENCE_DIR/m4-measurements.json（busy/noise/cleanup子记录），子进程stdout/stderr与锁/样本/清理原始记录也在该目录且以相对路径/sha256引用；busy独立包装器两锁等待合计≤30s、被测负载启动计数为0、detail.reason=MEASUREMENT_BUSY，子调用结果SKIPPED/退出78；noise组件三类注入分别得到MEASUREMENT_NOISE、SKIPPED/78并保留已有FAIL，产品自身负载超限对照仍FAIL；cleanup用隔离inode的组件seam复用生产清理/锁生命周期代码，dummy runner取消/被杀后竞争负载仍不能启动，直至guardian核验登记pidfd终结/受控树空、释放两锁才启动，记录身份、锁时刻与先后关系；这些预期负例的子结果嵌入工件，不作为父检查TEST的SKIPPED行合并，test_064_01_*仅在断言全满足时PASS，否则FAIL；父检查真实遇锁忙/噪声时仍相关TEST=SKIPPED、整次退出78且AC不通过，保留已有FAIL；隔离fixture锁不替代真实全机排他，T03的service/scope真实SIGKILL实验仅为交付证据、不引用为该AC门禁证据，不能搬入旧检查工件充数；debug负例与同次release规模两部分缺一不通过
- **AC-064-02**（TEST-064-02 · 检查 `integration`）Given §5.1同次百万库克隆，单块100MiB，首/中/尾唯一标记；release真实PTY；When 开Workspace/Focus，末页/搜索/往返滚动60s，每种尺寸输入≥1000次，idle10s采RSS；Then 描述≤1000/正文≤4MiB/在途页≤2，初始不回放百万正文；所有标记可达/锚点稳定；输入p95≤100ms；idle host+TUI总RSS≤150MiB且分别记录/各≤150MiB，活动各≤256MiB；不含Agent/浏览器，不排除TUI；环境不符SKIPPED/退出78，不通过
  - 判定：描述≤1000/正文≤4MiB/在途页≤2，初始不回放百万正文；所有标记可达/锚点稳定；输入p95≤100ms；idle host+TUI总RSS≤150MiB且分别记录/各≤150MiB，活动各≤256MiB；不含Agent/浏览器，不排除TUI；环境不符SKIPPED/退出78，不通过
- **AC-064-03**（TEST-064-03 · 检查 `integration`）Given 6×8MiB+1诊断字节；§5每一引用根各一对象、仅登记无引用对象、未登记临时文件；时钟47/48/49h；When 轮换/GC；扫描后加引用、打开pin并发GC；引用解析故障；GC登记tombstone前后kill/reopen；Then 诊断最多5段/每段≤8MiB；全部引用/活动/pin对象hash不变；47h孤立保留、≥48h且复查无引用才删；collected对象404 ARTIFACT_NOT_FOUND reason=collected，未知ID/丢文件原因不同；解析失败保留诊断；重复GC幂等；events/evidence/acceptance删除0
  - 判定：诊断最多5段/每段≤8MiB；全部引用/活动/pin对象hash不变；47h孤立保留、≥48h且复查无引用才删；collected对象404 ARTIFACT_NOT_FOUND reason=collected，未知ID/丢文件原因不同；解析失败保留诊断；重复GC幂等；events/evidence/acceptance删除0
- **AC-064-04**（TEST-064-04 · 检查 `recovery`）Given 已有E/待写N、min_free32MiB、1MiB流段；32KiB SendInput需封口原文对象；真实ENOSPC与注入EDQUOT；临时写/fsync/rename/登记故障；When 逐点失败或kill/reconcile；低空间但状态事务可提交时提交32KiB输入、分别Pause/Cancel；经hoh -p --resume以text/json/stream-json分别提交32KiB正文并观察低空间拒绝；状态事务本身满盘再试；释放后用原HTTP请求同幂等键重放验证服务端幂等；CLI的再次显式提交是ADR-021的新UUID动作，不要求复用上次进程的key；Then E可读hash不变、N不留引用缺正文；新对象/新写Attempt为STORAGE_LOW_SPACE，含available/required/min_free；32KiB SendInput唯一HTTP503 code=STORAGE_LOW_SPACE、details.reason相同且字节数可核对，无202/接受行/input.accepted/public.message；CLI三种输出均退出69，保留STORAGE_LOW_SPACE/details并明确输入未接受/存储空间不足，不自动重试、不新增run.result或改变Run状态；恢复后显式重试最多一次接受/用户块。低空间但状态事务可提交时Pause/Cancel仍202且持久；事务真失败503 DB_FULL无202；原Task可重试、孤立48h策略不变；缺真实ENOSPC设施FAIL非零，EDQUOT明确注入；投递文件阶段子例由015-03覆盖；受理前503的UI草稿/目标/未接受提示及脱离由AC-010-03的tui-pty覆盖
  - 判定：E可读hash不变、N不留引用缺正文；新对象/新写Attempt为STORAGE_LOW_SPACE，含available/required/min_free；32KiB SendInput唯一HTTP503 code=STORAGE_LOW_SPACE、details.reason相同且字节数可核对，无202/接受行/input.accepted/public.message；CLI三种输出均退出69，保留STORAGE_LOW_SPACE/details并明确输入未接受/存储空间不足，不自动重试、不新增run.result或改变Run状态；恢复后显式重试最多一次接受/用户块。低空间但状态事务可提交时Pause/Cancel仍202且持久；事务真失败503 DB_FULL无202；原Task可重试、孤立48h策略不变；缺真实ENOSPC设施FAIL非零，EDQUOT明确注入；投递文件阶段子例由015-03覆盖；受理前503的UI草稿/目标/未接受提示及脱离由AC-010-03的tui-pty覆盖
- **AC-064-05**（TEST-064-05 · 检查 `integration`）Given §5.1参考环境release；快/慢SSE各1，100events/s共10000条8KiB公开正文，含tool/decision/acceptance；单流应用pending≤500项且序列化字节≤1MiB；另debug integration通过writer seam暂挂未提交事务X（独立小库）；When 慢客户端停读30s直至TCP背压后恢复；独立断线后按last seq重连；查询矛盾after/Last-Event-ID；debug libtest在X提交前后分别观察，release测量不用该钩子；Then 唯一背压行为为停止拉取/填pending（不因满队列主动丢弃或断开），批读取先按字节限界，探针含pending与当前序列化项、不含另报的内核/TCP缓冲；快流每条提交完整且p95≤500ms；慢流恢复/去重并集完整；冲突409 CONFLICT；X提交前对任一SSE不可见、提交后可见；202对应已提交事件缺失0；环境不符SKIPPED/退出78、AC不通过
  - 判定：唯一背压行为为停止拉取/填pending（不因满队列主动丢弃或断开），批读取先按字节限界，探针含pending与当前序列化项、不含另报的内核/TCP缓冲；快流每条提交完整且p95≤500ms；慢流恢复/去重并集完整；冲突409 CONFLICT；X提交前对任一SSE不可见、提交后可见；202对应已提交事件缺失0；环境不符SKIPPED/退出78、AC不通过

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- （CR-028，CTO 2026-11-21 批准 D1–D12）D1/D4/D5/D6；§1/2/5/5.1 的分页、异步回填、字节背压、对象引用/GC/pin、日志与空间保护、唯一串行 release 测量；应用核对的 GC 根闭包。AC 中的章节号、F13/H13 与参考环境均指 docs/cr/CR-028-m4-detailed-spec.md；2026-11-25 应用核对仅更新来源/已实现前提及已授权的事件分类和 GC 根，不修改 AC 语义或阈值。
- C1/C5（CTO 2026-11-25）：M4-T03 的 measurement runner/guardian 先独占 /home/ai/dev/.hoh-test-slot.lock，再取 D4 测量锁；每个被测 run_check/cargo/构建都经持有两锁的外部包装器，run_check 自身保持 BT-03 不取槽。两锁、原阈值、覆盖与清理要求共存。M3-T05 后绑定真实 pre-003 产品 commit 并先实测五阶段；3600s 内不能留600s余量则 CTO 以正式 CR 重规划 required check，任何降阈值/缩范围交所有者。本准备稿不预填耗时或分配数值。

## REQ-065 · Git集成与交付

M6 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

分支候选、PR/CI可选；merge后重新绑定最终SHA；无remote本地release可验收

**验收**

- **AC-065-01**（TEST-065-01 · 检查 `integration`）Given 本地或有GitHub remote；When rebase/change-base/无remote用例；Then 分支候选、PR/CI可选；merge后重新绑定最终SHA；无remote本地release可验收
  - 判定：分支候选、PR/CI可选；merge后重新绑定最终SHA；无remote本地release可验收

## REQ-066 · 安装使用文档

M6 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

二进制、内置资源、Web资产版本一致；安装配置命令经实跑

**验收**

- **AC-066-01**（TEST-066-01 · 检查 `deployment`）Given 干净Ubuntu安装产物；When 全新VM安装和卸载保留数据；Then 二进制、内置资源、Web资产版本一致；安装配置命令经实跑
  - 判定：二进制、内置资源、Web资产版本一致；安装配置命令经实跑

## REQ-067 · 跨天稳定性

M7 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

无未处理崩溃/重复写；服务暂不可用可恢复；至少20个有证据的真实工程轮次

**验收**

- **AC-067-01**（TEST-067-01 · 检查 `soak`）Given 72小时受控运行含真实Agent轮次；When 72h soak + crash/recovery 留原始时间证据；Then 无未处理崩溃/重复写；服务暂不可用可恢复；至少20个有证据的真实工程轮次
  - 判定：无未处理崩溃/重复写；服务暂不可用可恢复；至少20个有证据的真实工程轮次

## REQ-068 · 性能预算

M7 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

idle core RSS≤150MiB(不含Agents/浏览器)，TUI输入p95≤100ms，事件可见p95≤500ms

**验收**

- **AC-068-01**（TEST-068-01 · 检查 `integration`）Given 参考4vCPU/8GiB VM离线回放；When 至少1000次样本与机器配置；Then idle core RSS≤150MiB(不含Agents/浏览器)，TUI输入p95≤100ms，事件可见p95≤500ms
  - 判定：idle core RSS≤150MiB(不含Agents/浏览器)，TUI输入p95≤100ms，事件可见p95≤500ms

## REQ-069 · 交付真实性

M7 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

明确哪些测试真实、模拟、跳过；未通过绝不称生产完成

**验收**

- **AC-069-01**（TEST-069-01 · 检查 `traceability`）Given 执行完整验收和最终汇总；When 交付报告与原始记录一致性审计；Then 明确哪些测试真实、模拟、跳过；未通过绝不称生产完成
  - 判定：明确哪些测试真实、模拟、跳过；未通过绝不称生产完成

## REQ-070 · 长期维护与升级

M7 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

锁版本+固定回归任务；每次变更跑对照；不在活跃run自动升级

**验收**

- **AC-070-01**（TEST-070-01 · 检查 `deployment`）Given 变更Adapter/Prompt/调度策略；When 升级迁移及基线比较；Then 锁版本+固定回归任务；每次变更跑对照；不在活跃run自动升级
  - 判定：锁版本+固定回归任务；每次变更跑对照；不在活跃run自动升级
