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
  - 判定：两次退出码均=4，run state=INCOMPLETE；PTY 输出不含 `ESC[?1049h`；所有 commands/input_queue payload 不含由 stdin 写入的 `y`；每次执行 runs 新增恰1行；每个 run 的 decisions 至少1行 payload 含 assumption=true 与依据引用；管道方式未关闭 stdin 仍在超时内退出
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
- stream-json 必须输出该 run 全部已持久化事件，与 DB events 的 seq 集合一致；慢消费者不得造成缺口或重排。
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

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

两种入口得到同一 SetMode 行为；Shell 不需要对称子命令

**验收**

- **AC-007-01**（TEST-007-01 · 检查 `tui-pty`）Given 在 TUI 用 /plan 或 Shift+Tab；When 同 CommandHandler 的集成测试；Then 两种入口得到同一 SetMode 行为；Shell 不需要对称子命令
  - 判定：两种入口得到同一 SetMode 行为；Shell 不需要对称子命令

## REQ-008 · Plan 互动与文档

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

产生版本化规格，允许提问，未批准实施不修改产品源码

**验收**

- **AC-008-01**（TEST-008-01 · 检查 `tui-pty`）Given 进入 Plan 讨论范围并修改决定；When 交互脚本检查写集只含 docs/spec；Then 产生版本化规格，允许提问，未批准实施不修改产品源码
  - 判定：产生版本化规格，允许提问，未批准实施不修改产品源码

## REQ-009 · Build 与 Auto

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

Build 遵守既定规格；Auto 可补规划；缺必需规格均不越过 readiness

**验收**

- **AC-009-01**（TEST-009-01 · 检查 `integration`）Given 选择 Build 或 Auto 后提交目标；When 模式状态迁移测试；Then Build 遵守既定规格；Auto 可补规划；缺必需规格均不越过 readiness
  - 判定：Build 遵守既定规格；Auto 可补规划；缺必需规格均不越过 readiness

## REQ-010 · Workspace 工作台

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

统一活动、团队摘要、输入框和 statusline；状态由事件投影产生

**验收**

- **AC-010-01**（TEST-010-01 · 检查 `tui-pty`）Given 多个 Agent 执行任务；When Ratatui TestBackend 与真实 PTY 快照；Then 统一活动、团队摘要、输入框和 statusline；状态由事件投影产生
  - 判定：统一活动、团队摘要、输入框和 statusline；状态由事件投影产生

## REQ-011 · Team/Focus 布局

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

窄屏单列/标签，宽屏可分屏；完整会话按需进入，不强制三滚动窗

**验收**

- **AC-011-01**（TEST-011-01 · 检查 `tui-pty`）Given 窄屏与宽屏切换并选择一个角色；When 80/120/180列 resize 测试；Then 窄屏单列/标签，宽屏可分屏；完整会话按需进入，不强制三滚动窗
  - 判定：窄屏单列/标签，宽屏可分屏；完整会话按需进入，不强制三滚动窗

## REQ-012 · 可展开内容块

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

展开、复制、搜索、全屏、返回保留滚动锚点；键盘与鼠标可达

**验收**

- **AC-012-01**（TEST-012-01 · 检查 `tui-pty`）Given 打开 Task/Tool/Diff/Test/Evidence 块；When 交互序列与大日志延迟测试；Then 展开、复制、搜索、全屏、返回保留滚动锚点；键盘与鼠标可达
  - 判定：展开、复制、搜索、全屏、返回保留滚动锚点；键盘与鼠标可达

## REQ-013 · 三级时间线

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

可定位需求、issue、decision、commit 与证据，重放后相同

**验收**

- **AC-013-01**（TEST-013-01 · 检查 `tui-pty`）Given 按 Project/Iteration/Agent 筛选；When 断线重放与过滤测试；Then 可定位需求、issue、decision、commit 与证据，重放后相同
  - 判定：可定位需求、issue、decision、commit 与证据，重放后相同

## REQ-014 · 真实进度状态栏

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

显示 n/m accepted、open/reopened issues；未知成本/上下文显示未知且指明 Agent

**验收**

- **AC-014-01**（TEST-014-01 · 检查 `tui-pty`）Given 收到验收、回归、预算事件；When 快照数据与计算断言；Then 显示 n/m accepted、open/reopened issues；未知成本/上下文显示未知且指明 Agent
  - 判定：显示 n/m accepted、open/reopened issues；未知成本/上下文显示未知且指明 Agent

## REQ-015 · 输入与终端兼容

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

按显示列宽处理；粘贴不误发送；异常退出恢复 raw/alternate screen

**验收**

- **AC-015-01**（TEST-015-01 · 检查 `tui-pty`）Given 输入中文/emoji/多行粘贴并走 SSH/tmux；When PTY Unicode、resize、panic teardown；Then 按显示列宽处理；粘贴不误发送；异常退出恢复 raw/alternate screen
  - 判定：按显示列宽处理；粘贴不误发送；异常退出恢复 raw/alternate screen

## REQ-016 · 无障碍与键位

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

所有重要操作可键盘执行；不只靠颜色；有冲突检测与恢复默认

**验收**

- **AC-016-01**（TEST-016-01 · 检查 `tui-pty`）Given 关闭鼠标/颜色并自定义键位；When no-color与按键冲突用例；Then 所有重要操作可键盘执行；不只靠颜色；有冲突检测与恢复默认
  - 判定：所有重要操作可键盘执行；不只靠颜色；有冲突检测与恢复默认

## REQ-017 · 运行中对话

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

Ask 不改任务；steer 有生效边界；CR 改规格；控制可独立于模型执行

**验收**

- **AC-017-01**（TEST-017-01 · 检查 `integration`）Given 发送 Ask/Steer/Change/Control 和 @role；When 持久化输入队列测试；Then Ask 不改任务；steer 有生效边界；CR 改规格；控制可独立于模型执行
  - 判定：Ask 不改任务；steer 有生效边界；CR 改规格；控制可独立于模型执行

## REQ-018 · 默认与自定义团队

M2 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`config/hoh.example.toml`、`CONTRACT.md`、`docs/06-orchestration.md`、`docs/04-architecture.md`、`docs/05-data-api.md`、`docs/13-adapters.md`、`docs/12-user-guide.md`、`contracts/protocol.schema.json`、`contracts/sql/001_initial.sql`

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

## REQ-019 · 角色交接

M2 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/14-resilience.md`、`docs/05-data-api.md`、`docs/07-algorithms.md`、`docs/03-ux.md`、`contracts/protocol.schema.json`、`contracts/sql/001_initial.sql`、`tools/reference_model.py`

运行中更换职位持有者（如 Commander 从 Codex 换成 Pi）时，在安全点递增 role_epoch 并隔离旧会话指令；新持有者从 checkpoint 交接包接手，原任务不重复执行。

**必须行为**

- 绑定变更必须先持久化为待交接请求，在该职位当前 turn 结束或到达 writer 安全点后才生效；生效前界面显示 pending，不显示为已完成。
- 生效时必须在同一事务内完成：更新绑定、该职位 role_epoch+1、追加交接事件、写入新会话启动的 outbox。
- role_epoch 低于当前值的 decision、steer 和消息只进入历史（decisions.state=REJECTED，原因 STALE_ROLE_EPOCH），不得改变任何 task/run 状态，也不得产生派发。
- 新持有者的首个输入必须是 checkpoint 交接包，包含 goal、当前 spec_sha256/revision、已 APPLIED 的决策、OPEN/REOPENED issue、未 ACK 的 outbox 与 QUEUED 输入、当前 candidate 身份；不能是旧会话全文。
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

- 文档没有列出各内置职位的 required_capabilities。初值：所有职位需要 structured_events，或可用 TaskResult 结构化桥替代；developer/qa 需要经 probe 实测的命令执行能力；commander/reviewer 需要 resume。
- 默认配置不含任何隐式 fallback，未配置 fallback 即直接 BLOCKED。
- 文档没有说明 429/503 是否触发 fallback。选择不触发，按重试与预算规则等待；只有永久错误（缺认证/能力不支持/未安装/无模型）才评估 fallback。
- agent 缺失和 model 缺失的原因码文档未定义。初值：AGENT_UNAVAILABLE、MODEL_UNAVAILABLE。

## REQ-021 · Commander 监督

M2 · P0 · 发布类 required · 目标 v1 · 成熟度 detailed，参考：`docs/06-orchestration.md`、`docs/07-algorithms.md`、`docs/14-resilience.md`、`docs/03-ux.md`、`docs/05-data-api.md`、`config/hoh.example.toml`、`resources/prompts/commander.md`、`contracts/sql/001_initial.sql`

Commander 只被重要事件或策略规定的轮次边界唤醒；同类事件按去重键聚合并去抖；触发进度持久化在 last_handled_seq；过期决策被拒绝并重新触发；每次监督的延迟和决定都可查询。

**必须行为**

- 只有重要事件集合（need_decision、critical/high issue、no_progress、budget 风险、角色失效、规格变更、milestone_ready、release_request）和 balanced 策略下的轮次边界可以唤醒 Commander。
- token delta、工具输出、心跳、medium/low issue 和常规 candidate_ready 不得唤醒 Commander；candidate_ready 由 Runtime 直接派发 QA 与 Review。
- 按 (run_id, problem_fingerprint, spec_revision) 聚合：同键事件在静默 2 秒后合并为一个触发器，自首个事件起最长等待 10 秒；不同键分别触发。
- 严重完整性错误（IntegrityViolation）必须绕过去抖，在同一调度 tick 内唤醒 Commander。
- 触发器必须持久化 last_handled_seq；Commander 只收到该 seq 之后的摘要和证据引用。超出上下文预算时按严重性和因果链取舍，但保留 spec_sha256 与 candidate_id。
- Commander 决策必须带 expected_revision 和 role_epoch；任一过期即不 apply，标 REJECTED，并基于最新状态重新触发。
- 每个触发器和决策都必须可查询：去重键、event_seqs、首事件时间、唤醒时间、决策时间、监督延迟（单调时钟毫秒）和决策状态。
- reserve_supervisor_slot=true 时，即使写槽和读槽全部被占用也不能推迟 Commander 唤醒；Commander 不可用时，已授权的低风险动作继续执行，新的重大决策使 run 进入 BLOCKED。

**失败与边界**

- Agent 每秒产生数千条 token 事件时，Commander 唤醒次数仍为 0。
- 同一问题持续重复出现时，同一去重键最多存在一个未处理的触发器，新事件并入该触发器。
- host 重启后，seq≤last_handled_seq 的事件不再唤醒 Commander，已 APPLIED 的决策不会重放。
- 决策的 expected_revision 过期时，state=REJECTED(STALE_REVISION)，task 不变，并产生恰好一次重新触发。
- Commander 不可用时出现 need_decision，run 进入 BLOCKED 并记录原因，不无限等待；candidate_ready 照常派发 QA/Review。
- Commander 决策试图直接把 AC 或 candidate 标为通过时被拒绝，验收仍以证据门禁为准。

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

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 2 秒/10 秒的语义：选择尾随去抖（同键静默 2 秒后触发），加上自首事件起最长 10 秒。
- 紧急事件集合：选择只有 IntegrityViolation 类（例如验证中候选被改、DB integrity 失败）绕过去抖；critical issue 属于重要事件，仍走去抖。
- 同键已有未处理触发器时，新事件并入该触发器，不并发第二个触发器。
- 触发器字段名（dedup_key、event_seqs、wake_latency_ms）和事件 kind 名称是初值；延迟的测量起点为首事件提交时刻（单调时钟）。
- 文档没有量化 efficient/intensive 的唤醒差异。选择只断言：同一 fixture 下唤醒次数满足 intensive≥balanced≥efficient，且重要事件在三种策略下都会唤醒。

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

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

局部咨询不绕过全局任务账本；重大决定升级 Commander；讨论有轮数上限

**验收**

- **AC-023-01**（TEST-023-01 · 检查 `integration`）Given 开发者请求研究或出现架构争议；When wait-for环与讨论预算测试；Then 局部咨询不绕过全局任务账本；重大决定升级 Commander；讨论有轮数上限
  - 判定：局部咨询不绕过全局任务账本；重大决定升级 Commander；讨论有轮数上限

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

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- App Server 的 YOLO 通过 thread/start 的 approvalPolicy=never、sandbox=danger-full-access 表达，依据本机 codex 0.166 generate-json-schema；docs/13 只描述 exec flag。
- native cancel grace 10 s 与 TERM grace 5 s 为初值，三个适配器共用。
- 降级 exec 的 resume 用 `codex exec resume <id>`（本机 help 存在），是否计为 resume 能力由兼容套件确认。
- （CR-003）鉴权失败识别：App Server 为 turn/completed status=failed 且 codexErrorInfo=unauthorized（0.166.0 schema 枚举值，录制中未出现，fixture 依 schema 合成并在 meta.json 标 synthetic=true，只用于 integration）；exec 降级为 turn.failed，且其 error.message 或之前的 error 事件含 `unexpected status 401`（录制样例 exec-unauthenticated）。二者都映射 BLOCKED_AUTH 且不重试。

## REQ-025 · Claude 原生接口

M1 · P0 · 发布类 required · 目标 v1, v0.1 · 成熟度 detailed，参考：`docs/13-adapters.md`、`research/SOURCES.md`、`docs/09-testing.md`、`docs/11-deployment.md`、`docs/17-risks-gaps.md`、`docs/15-prompts-skills-memory.md`

Claude 适配器以 `claude -p --output-format stream-json --verbose` 非交互运行，分离捕获 stdout/stderr，由 init/result 帧得到 session 与结果，按实际帮助选用 flag，支持分轮 `--resume` 与有界取消。

**必须行为**

- 必须以 argv 调用 `claude -p --output-format stream-json --verbose`，仅当需要实时增量且 help 含该 flag 时追加 `--include-partial-messages`；不经 shell。
- stdout 与 stderr 必须并行读取并分别保存为独立原始文件（各记 sha256）；stderr 只作诊断，不按事件解析。
- 必须从 system init 帧记录 session_id 与 resolved model，从 result 帧记录 subtype、is_error、usage 与费用；缺失项为 unknown 而非0。
- 仅当 result.subtype=success、is_error=false 且进程退出码0时 attempt=SETTLED；error_max_turns、error_during_execution、中断均不是成功。
- 后续轮次必须以 `--resume <session_id>` 继续同一会话；需要独立会话的角色不得复用开发会话 id。
- YOLO profile 仅在 help 含 `--dangerously-skip-permissions` 时使用；HoH Auto 不得映射为 `--permission-mode auto`；HoH Prompt 经官方追加方式或任务上下文注入，不替换原生 system prompt。
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
- Capabilities（structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage）必须由探测与兼容套件结果得出，不按品牌硬编码，并写入 sessions.capabilities_json。
- 兼容套件 fixture 必须对每个 agent 覆盖成功、失败、取消、压缩、工具、未知字段6类，manifest 记录所用 fixture 集 hash 与结果。
- 实现 sha256、help 或 schema hash 与 manifest 不符时，新 attempt 不得使用该版本（COMPAT_UNVERIFIED），直到在无写租约时重跑兼容套件通过；运行中的 attempt 不受影响。
- 未知事件类型与已知事件中的未知字段必须保存原始帧并产生 `adapter.unknown_event` 诊断，不 panic、不改变终态判定。
- 请求不支持的能力必须返回 CAPABILITY_UNSUPPORTED（API 为422）或走文档化降级（如 steer 排到下一轮），不得静默伪装支持。
- 协议帧超过1 MiB（初值）必须有界失败：缓冲不超过上限加一次读块，保存诊断并使该 attempt FAILED（PROTOCOL_FRAME_TOO_LARGE）。
- 能力为 false 或数据缺失的指标（如 token_usage）必须显示 unknown/null，不显示0。
- 每个适配器按 CLI 版本维护已知帧登记表 tests/fixtures/protocol/<agent>/<version>/known-frames.json（由录制样例与官方文档汇总，含已知但忽略的辅助帧，如 Claude 的 rate_limit_event、system/task_*，Codex 的 remoteControl/status/changed、deprecationNotice）；只有不在登记表中的帧才产生 adapter.unknown_event（CR-003）。

**失败与边界**

- 当安装脚本成功但兼容套件未运行时，protocol=unknown，不视为兼容。
- 当新版 Codex help 不再含 `app-server` 时，探测得 mid_turn_steer=false 并降级 exec，不沿用旧 manifest 推断。
- 当出现未知 method/type 帧时保存原始帧，attempt 终态与去掉该帧的对照回放一致。
- 当收到8 MiB 无 LF 的单行时，最大缓冲≤1 MiB+64 KiB，attempt FAILED，host 与其它 attempt 继续运行。
- 当有写租约活跃时请求运行兼容套件，被拒绝并提示维护窗口，不中途替换正在使用的版本。
- 当 Claude 不支持中途 steer 时，steer 输入保持 QUEUED 至下一轮 resume 投递，不标 APPLIED。

**验收**

- **AC-027-01**（TEST-027-01 · 检查 `integration`）Given fixture 含3组 fake 二进制：codex 0.166 的 help 与 schema 输出、去掉 `app-server` 的旧 codex、缺 `--include-partial-messages` 的旧 claude，各 sha256 已知；When 对每组调用 probe() 两次并生成 compat manifest；Then 每项通过 contracts/compat-manifest.schema.json 校验，含 launcher_path 与 launcher_sha256、implementation_path 与 implementation_sha256（均等于已知值；fake 二进制自身即实现时两组相同）、cli_version（--version 原文）、transport、capabilities、help_sha256、schema_sha256（无 schema 为 null）、suite（只探测未跑套件时 status=NOT_RUN）；旧 codex capabilities.mid_turn_steer=false 且 transport=exec（降级）；旧 claude capabilities.partial_messages=false；同输入两次探测结果相同
  - 判定：3项 manifest 均通过 compat-manifest.schema.json 校验，launcher_sha256 与 implementation_sha256 等于已知值；Claude 与旧 codex 项 schema_sha256=null，codex 0.166 项为其 schema 输出的 sha256（64位十六进制）；旧 codex capabilities.mid_turn_steer=false，transport=exec；两版 codex 能力集不同，且各自两次探测结果完全相同；旧 claude capabilities.partial_messages=false
- **AC-027-02**（TEST-027-02 · 检查 `integration`）Given 三个适配器各1份 fixture：插入1条不在该版本已知帧登记表中的 method/type 帧，并在已知事件中加入未知字段 `x_future`；另备删除未知帧的对照 fixture；When 回放两组 fixture 至终态；Then 无 panic；每份恰1条 adapter.unknown_event 且含原始帧 sha256；原始日志 sha256=输入字节 sha256；`x_future` 可从 raw 旁路取回；终态与对照回放一致
  - 判定：6次回放无 panic、进程均正常结束；每份含未知帧的回放 adapter.unknown_event 数=1，事件含帧 sha256；原始日志 sha256 与输入一致；`x_future` 字段值可从 raw 取回且等于输入；attempt 终态与对照回放逐一相等
- **AC-027-03**（TEST-027-03 · 检查 `integration`）Given 三个适配器各1份 fixture：若干正常帧后接8 MiB 无 LF 的单行；帧上限为默认1 MiB；同 host 另有1个正常 fake attempt 并行；When 回放并通过测试探针记录 reader 最大缓冲字节；Then 最大缓冲≤1 MiB+64 KiB；attempt=FAILED、原因 PROTOCOL_FRAME_TOO_LARGE；诊断含帧长度、前缀 sha256 与前4 KiB；并行 attempt=SETTLED
  - 判定：三个适配器最大缓冲字节均≤1,114,112；超大帧 attempt state=FAILED，原因=PROTOCOL_FRAME_TOO_LARGE；诊断记录含长度、前缀 sha256、前4096字节；并行正常 attempt state=SETTLED，host 进程未退出
- **AC-027-04**（TEST-027-04 · 检查 `integration`）Given compat manifest 已登记 fake codex A 的 hash 且兼容套件 PASS；一个使用 A 的 RUNNING attempt 持有写租约；When 将 PATH 中 codex 替换为 hash 不同的 B 后请求新 attempt；请求运行兼容套件；待写租约释放后运行套件并重试；Then 新 attempt 被拒（COMPAT_UNVERIFIED）而原 attempt 继续至 SETTLED；写租约活跃时套件请求被拒；套件 PASS 后 manifest 的 codex 项更新为 B（implementation_sha256=B 的 hash、suite.status=PASS）且新 attempt 成功启动；PASS 前 doctor 显示 protocol=unknown
  - 判定：替换后首个新 attempt 被拒，错误码=COMPAT_UNVERIFIED，无子进程启动；原 attempt 最终 state=SETTLED，其进程仍为 A；doctor codex 项 protocol=unknown；租约活跃时套件请求被拒绝；套件 PASS 后 manifest 的 codex 项 implementation_sha256 等于 B 的 hash 且 suite.status=PASS，重试 attempt 成功启动

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- COMPAT_UNVERIFIED、PROTOCOL_FRAME_TOO_LARGE 命名与读块64 KiB 为初值；帧上限1 MiB 为 docs/13 初值。
- 兼容套件是否必须含 live smoke（产生费用）未定；选择离线探测+fixture 为必需，live smoke 由 adapter-live 检查单独记录。
- Claude/Pi 无原生 schema 命令，schema_sha256 记 null，以 help hash 与 fixture 为准。

## REQ-028 · 项目发现

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

建立基线，保留脏文件，不重写架构、不强制初始化远程

**验收**

- **AC-028-01**（TEST-028-01 · 检查 `integration`）Given 打开有代码测试文档的旧仓库；When Brownfield含未提交修改测试；Then 建立基线，保留脏文件，不重写架构、不强制初始化远程
  - 判定：建立基线，保留脏文件，不重写架构、不强制初始化远程

## REQ-029 · 内置模板

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

产品/需求/功能/UX/架构/开发/测试/验收/部署/使用规范齐全；按规模选用

**验收**

- **AC-029-01**（TEST-029-01 · 检查 `unit`）Given 创建 lite/standard/full项目规格；When 所有模板可实例化且无必填残缺；Then 产品/需求/功能/UX/架构/开发/测试/验收/部署/使用规范齐全；按规模选用
  - 判定：产品/需求/功能/UX/架构/开发/测试/验收/部署/使用规范齐全；按规模选用

## REQ-030 · 规格收敛

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

记录用户/范围/非目标/决策/开放问题；必要不确定性不伪造用户答案

**验收**

- **AC-030-01**（TEST-030-01 · 检查 `integration`）Given 模糊目标进入规划；When 互动与无人值守分支测试；Then 记录用户/范围/非目标/决策/开放问题；必要不确定性不伪造用户答案
  - 判定：记录用户/范围/非目标/决策/开放问题；必要不确定性不伪造用户答案

## REQ-031 · Readiness Gate

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

必要 AC 可观察、依赖可达、关键决定确定；仅阻断当前受影响范围

**验收**

- **AC-031-01**（TEST-031-01 · 检查 `unit`）Given 任务准备开始实施；When 缺AC/循环依赖/非阻塞未知测试；Then 必要 AC 可观察、依赖可达、关键决定确定；仅阻断当前受影响范围
  - 判定：必要 AC 可观察、依赖可达、关键决定确定；仅阻断当前受影响范围

## REQ-032 · 需求追踪

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

REQ→TASK→candidate→TEST→EVIDENCE→acceptance 可双向查找

**验收**

- **AC-032-01**（TEST-032-01 · 检查 `traceability`）Given 需求关联实现和测试；When 断链与孤立验收检测；Then REQ→TASK→candidate→TEST→EVIDENCE→acceptance 可双向查找
  - 判定：REQ→TASK→candidate→TEST→EVIDENCE→acceptance 可双向查找

## REQ-033 · 规格变更

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

生成 CR、revision、影响闭包；受影响验收过期，不静默沿用旧PASS

**验收**

- **AC-033-01**（TEST-033-01 · 检查 `integration`）Given 用户新增/修改已验收功能；When 修改需求后门禁拒绝旧证据；Then 生成 CR、revision、影响闭包；受影响验收过期，不静默沿用旧PASS
  - 判定：生成 CR、revision、影响闭包；受影响验收过期，不静默沿用旧PASS

## REQ-034 · 文档事实源

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

文件是语义事实源，DB是索引/状态；冲突可检测；生成页不产生第二权威源

**验收**

- **AC-034-01**（TEST-034-01 · 检查 `integration`）Given 用户编辑 Git 中规格；When 并发编辑与哈希冲突测试；Then 文件是语义事实源，DB是索引/状态；冲突可检测；生成页不产生第二权威源
  - 判定：文件是语义事实源，DB是索引/状态；冲突可检测；生成页不产生第二权威源

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
- Run终态为COMPLETED/CANCELLED/FAILED且无出边；COMPLETED只能来自VERIFYING；PAUSED只来自PAUSE_REQUESTED；CANCELLED只来自CANCEL_REQUESTED；恢复回到resume_state并先reconcile。
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
- INCOMPLETE是否终态未定；本选择终态仅COMPLETED/CANCELLED/FAILED，INCOMPLETE/BLOCKED/WAITING_EXTERNAL/PAUSED可经ResumeRun回到resume_state。
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

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

按问题指纹/状态变化而非commit计数诊断，升级/研究/改计划

**验收**

- **AC-040-01**（TEST-040-01 · 检查 `unit`）Given 连续无净进展或修复循环；When 重复修复和只改文档案例；Then 按问题指纹/状态变化而非commit计数诊断，升级/研究/改计划
  - 判定：按问题指纹/状态变化而非commit计数诊断，升级/研究/改计划

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

**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）

- 退避base=2 s、cap=300 s、瞬时重试上限8次均为初值。
- 已修正（CR-002）：价格未知时 cost 为 null 并显示 unknown；若用户配置了费用上限而所用模型价格未知，该上限无法执行，派发前即 BLOCKED（BUDGET_UNENFORCEABLE），不在无法核算时继续花费；未配置费用上限时照常受其他预算约束。
- 命名已统一（CR-001）：BLOCKED_AUTH/BLOCKED_CAPABILITY 为阻塞原因，CAPABILITY_UNSUPPORTED 为错误码；runs 已含 blocked_reason 列。
- tasks 已含 attempt_count、next_retry_at；预算账本为 budgets 表（CR-001 SQL）。
- （CR-003）费用与期限配置键：[budget] max_usd（可选，缺省表示未设费用上限）、deadline_at（可选，RFC 3339 UTC）；与 max_duration_hours、max_iterations 同属 [budget]。

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
- 以setsid脱离进程组的后代不受pgid信号覆盖；本选择额外按/proc父子链终止该Attempt后代，需实现时确认可行。

## REQ-044 · 可恢复版本

M5 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

保存失败分支，回到已验证基线；不reset用户未提交改动

**验收**

- **AC-044-01**（TEST-044-01 · 检查 `recovery`）Given 候选严重回归或用户目录有变更；When Git worktree与hash验证；Then 保存失败分支，回到已验证基线；不reset用户未提交改动
  - 判定：保存失败分支，回到已验证基线；不reset用户未提交改动

## REQ-045 · 四层Prompt版本

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

合成并记录版本/内容hash；高层覆盖不改变事实证据；过长报错

**验收**

- **AC-045-01**（TEST-045-01 · 检查 `unit`）Given 修改role/project/task/evidence来源；When 冲突、预算和missing variable测试；Then 合成并记录版本/内容hash；高层覆盖不改变事实证据；过长报错
  - 判定：合成并记录版本/内容hash；高层覆盖不改变事实证据；过长报错

## REQ-046 · 内置Skill

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

内置可用无需用户手装；只加载相关技能；原生Skill不可用时内容注入

**验收**

- **AC-046-01**（TEST-046-01 · 检查 `integration`）Given 进入需求/实现/TDD/QA/review/release；When 无外部skill安装的端到端测试；Then 内置可用无需用户手装；只加载相关技能；原生Skill不可用时内容注入
  - 判定：内置可用无需用户手装；只加载相关技能；原生Skill不可用时内容注入

## REQ-047 · 自定义与升级

M3 · P0 · 发布类 required · 目标 v1 · 成熟度 outline

内置/global/project/run按序；显示diff；运行锁定旧版本直到checkpoint

**验收**

- **AC-047-01**（TEST-047-01 · 检查 `integration`）Given 用户覆盖Prompt/Skill/模板后升级；When 覆盖与升级迁移测试；Then 内置/global/project/run按序；显示diff；运行锁定旧版本直到checkpoint
  - 判定：内置/global/project/run按序；显示diff；运行锁定旧版本直到checkpoint

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

- 产品中对相同内容重复冻结：选择返回已有 candidate_id，不新增行。自举工具 tools/candidate.py 则拒绝覆盖已存在的清单文件。
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

M4 · P1 · 发布类 required · 目标 v1 · 成熟度 outline

结构化索引+对象文件；分页虚拟列表；日志轮换；引用证据不被清理

**验收**

- **AC-064-01**（TEST-064-01 · 检查 `integration`）Given 百万event与大日志积累；When 百万事件/磁盘满/慢客户端测试；Then 结构化索引+对象文件；分页虚拟列表；日志轮换；引用证据不被清理
  - 判定：结构化索引+对象文件；分页虚拟列表；日志轮换；引用证据不被清理

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
