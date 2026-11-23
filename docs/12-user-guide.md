# 使用手册（目标行为规范）

本手册描述AI完成后的HoH行为，不代表本启动包已提供hoh程序。开发启动见 START_HERE.md。

## 安装与卸载

解开发布包后执行 `hoh/install.sh`（不需要 root，也不调用 sudo）：`hoh` 装到 `~/.local/bin`，资源按版本放在 `~/.local/share/hoh/versions/`，`current` 原子切换到新版本；同时安装 systemd 用户 unit 模板 `hoh-host@.service`。加 `--project <目录>`（可重复）为该项目启用 host unit：开机或用户管理器启动时拉起它，崩溃后由 systemd 重启（5 分钟内最多 5 次，每次间隔 5 秒）。启用需要用户已开启 linger，否则 host 会随最后一个登录会话一起停止；`--linger` 经 logind 为你开启。不加 `--project` 时不启用任何项目；已开启 linger 时，第一次在项目里执行 `hoh` 照样经 unit 拉起 host（没有 linger 时不经 unit，host 崩溃后不会自动重启，`hoh doctor` 会说明）。

`install.sh --uninstall` 停止并禁用所有 `hoh-host@` 实例，删除二进制、模板与安装的文件。以下情况它以 69 退出、列出原因，什么也不删：还有已安装的 hoh 在运行（不经 unit 拉起的 host、打开着的 TUI、`hoh attach`、`hoh doctor` 都算）；有实例停不下来；管理器不可达而仍有启用的实例；安装的锁 `~/.local/share/hoh/.lock` 打不开、加不上锁或无法确认（这时它照实说明，不说有程序在运行）；安装器自己的锁建不了、打不开、加不上锁，或 60 秒内一直被另一个 install.sh 持有；卸载期间又被启用的实例停用不了；删除所依据的某项无法判断（`~/.local/bin/hoh` 读不到或所在目录不可搜索、unit 配置目录或其中的 `.wants`、`.requires`、`.upholds` 目录列不出、路径上某个符号链接指向不可搜索之处、管理器不回答实例是否启用或是否已停止、扫描时有一个进程说不出它运行的是什么程序（cannot tell what process N runs，说出内核的原话））；某个实例被 `systemctl --user mask` 屏蔽（先 `unmask` 再卸载）。没有安装时它报告 `not installed`，以 0 退出，什么也不写；安装目录 `~/.local/share/hoh` 是指向不存在之处的链接时，它照常删除模板与 `~/.local/bin/hoh`，留下这个链接。与 HoH 无关的 drop-in 目录（`*.d/`）读不了、`~/.config/systemd/user` 是普通文件、`.wants` 目录中有同名的普通文件（不是链接），都不妨碍卸载。卸载最后删不掉已清空的安装目录时（例如 `~/.local/share` 不可写），其余照删，说明原因，以 74 退出；它与 daemon-reload 的失败同时发生时两者都说明，以 74 退出。卸载删除模板后让管理器重新载入（`systemctl --user daemon-reload`）失败时，其余照删并逐项列出，最后说明这一失败、以 69 退出：请自己运行一次 `systemctl --user daemon-reload`。安装或卸载遇到 install.sh 没有专门说法的失败（例如磁盘写满）时，给出失败的命令、它的退出码与行号，以 74 退出。卸载进行期间，任何已安装的 hoh（包括 `hoh --version`）都以 69 退出，说明本安装正在卸载；同时运行的两个 install.sh（安装或卸载）轮流进行，卸载后留下一个空文件 `~/.local/share/hoh.lock`，是安装器自己的锁；若 `~/.local/share/hoh/.lock` 丢失，已安装的 hoh 会说明这一点并以 69 退出，重新运行发布包里的 install.sh 即可补上；各项目的 `.hoh/`、`~/.config/hoh/`、你写的 drop-in 与 linger 设置原样保留。升级：再执行新包的 `install.sh`；正在运行的 host 继续用旧版本，直到它重启。unit 托管的 host 只得到项目路径与执行 `hoh` 时的 PATH，不复制其他环境变量：靠环境变量（API key、代理）认证的 Agent，需要把变量写进 `~/.config/environment.d/*.conf` 或 `systemctl --user edit hoh-host@.service` 的 drop-in。`hoh doctor` 报告 host 是否由 unit 托管，不是时说明原因。

## 第一次打开

在项目目录执行 `hoh`。若缺配置，TUI引导检测Codex/Claude/Pi、模型可用性与项目目录，不要求用户先记住init命令。已有文档自动发现；不存在时内置规格模板可用。

默认Auto；复杂产品先用 `/plan` 讨论目标，或Shift+Tab切换。Plan完成后在同会话选择开始实施。工作模式和权限模式不同，用户无需理解底座CLI的权限枚举。

## 常用操作

`hoh -p "依据docs/PRD.md完成项目"`：前台非交互；`--output json`最终机器结果，`--output stream-json`实时事件。`--detach`先输出run_id和提交确认，任务后台继续。`hoh attach <id>`打开该运行。`hoh web`连接相同host。`hoh doctor`看环境能力：默认不调用模型，退出码0只表示没有缺失项，不代表已认证；`hoh doctor --live`才用一次小任务确认真能工作（可能产生费用）。

没有已批准规格的项目：`hoh -p` 新建的 Run 在同一次提交里以 BLOCKED（NEED_DECISION，原因 `no_spec`）停下，`-p` 以 3 退出并列出这个问题（`--detach` 照常输出 run_id）。用 `hoh -p draft --resume <run_id>` 或在 TUI 中选择 draft，HoH 先只读地了解项目，再由 Plan 起草规格并请你批准；起草只能写文档和 `.hoh/`，不能实施产品代码。起草完成后，`-p` 在 `spec_approval` 问题处以 3 退出，不会自行批准。可在 TUI 明确选择 approve，或用 `hoh -p approve --resume <run_id> --detach` 提交批准并取回 run_id（去掉 `--detach` 会继续跟随 Run）。其他答复退回起草，正文作为修改意见。只有用户能批准；从 Agent 进程或其可识别后代发出的批准被拒为 SPEC_APPROVAL_NOT_USER。批准前 specs 没有修订，批准时核对当前 Markdown 生成的摘要与问题中的摘要，内容冲突返回 409 SPEC_CONFLICT。项目已经有已批准修订时，no_spec 问题的 draft 与 adopt 都采用当前最高修订，不再建首次起草 Task；仍没有规格时 adopt 保持阻塞。当前自动起草入口支持首次规格；已有修订的修改仍可通过 ProposeSpecChange API 提出 CR 后批准。

直接 `ApproveSpec` 只允许用户来源。host 从 TCP/Unix socket 的真实对端确认进程，检查
Attempt 标记、同 uid 祖先标记和 Attempt cgroup；Agent 工具中的 shell 也在此范围内。
无法确认对端、同 uid 祖先的环境或 cgroup 不可读、映射不唯一、已识别的服务或代理请求，
均以 403 SPEC_APPROVAL_NOT_USER 拒绝，details.reason 给出 marker、ancestor_marker、
attempt_cgroup 或 origin_unknown；持有 token 或重放用户曾成功的批准都不能绕过。

来源检查的剩余风险：同 uid 进程若去掉标记、离开 Attempt cgroup 并脱离 Attempt 祖先链，
可能无法与用户进程区分。祖先扫描在第一个其他 uid 祖先或 PID 1 停止，因此跨 uid 再返回
的链（Attempt → sudo/su → 原用户 → approve）对祖先扫描不可见；在 cgroup 模式下，
仍在 Attempt cgroup 的进程会被成员归属检查识别。服务/代理识别只覆盖可观察到的证据，
不保证区分任意无标记同 uid 服务；不可读的无关进程若持有被转交的 socket fd，也不能由
扫描完全排除。此机制不是 OS 身份隔离。

TUI 的 `/plan`、`/build`、`/auto` 切换模式，Shift+Tab 按 `[ui] mode_cycle` 轮换（默认 PLAN→BUILD→AUTO，可配置为两个或三个不重复的合法模式）。未附着时只选择下一次新建 Run 的模式，不改变项目默认；已附着时切换当前 Run。写任务正在执行时，切换会等待安全点，顶栏在生效后更新。未知 Slash 显示错误，不作为普通输入发送；模式切换本身不生成或批准规格。

不存在hoh plan/hoh mode plan/hoh run子命令。Shell帮助保持短。TUI 输入 `/` 打开命令选择器，列出 `/plan`、`/build`、`/auto`、`/team`、`/model`、`/status`、`/resume`、`/help`；继续输入可模糊筛选，↑↓ 选择、Enter 执行、Esc 返回。未知命令显示“未知命令（unknown command）”，不提交任何命令。`/status` 显示当前 Run 状态，`/help` 显示键位与命令说明。

## 观察团队

Workspace看统一进展与Commander决策；Team按需分屏；选中Agent进入Focus；Timeline按项目/轮次/角色筛选。点击任务、diff、测试或证据查看来源。长日志默认折叠，不挤占对话。

**长时间没有输出的 Agent**：一个 Attempt 连续 15 分钟既没有输出、也没有它启动的工具进程在运行时，HoH 记一条 `attempt.inactive` 事件（`attach`、TUI 与 `--output stream-json` 都能看到），之后它再有输出时记 `attempt.inactive_cleared`。这只是提示：HoH 不会因此终止它，也不重试、不改状态；跑着长测试的 Agent 不算无输出。确认它卡住了再取消 Run。三种情况它报不准：Agent 常驻的子进程（例如以 stdio 运行的 MCP server）一直算作在运行的工具，这样的 Attempt 不会出现这条事件；同一用户在这个 Agent 之后启动、又不让别人读它环境的进程（设了 PR_SET_DUMPABLE=0，例如 ssh 客户端或某些代理程序），即使与这个 Attempt 无关，只要它还在运行，HoH 也分不出它是不是这个 Attempt 的工具，按有工具处理，这期间同样不会出现这条事件；而 Agent 自己在网络上等待（没有子进程）与卡住看起来一样，都会记这条事件。

“已验收18/25”只统计当前规格有效验收。新需求导致分母变化会显示规格版本；回归使已验收变少但保留历史。模型/上下文属于选中Agent，不显示虚假的全团队上下文百分比。

## 与团队交流

普通输入默认交给 Commander。`@cto`、`@qa`、`@developer` 指定稳定职位，以 STEER 入队；使用品牌名时，例如 `@claude 先写测试`，客户端读取当前团队和正在运行任务的职位。只有一个匹配的当前职位时直接选择，否则弹出职位选择器；Esc 返回原草稿，不发送。消息栏显示实际目标，例如“→ developer”，队列保存该职位与受理时的 epoch，换绑后的旧输入拒绝为 STALE_ROLE_EPOCH。

HoH 的 Slash 不转给底座。要定向发送底座命令，使用 `/agent developer /compact`；目标必须是当前已绑定的职位，缺目标、缺正文或未知目标均不提交。转发走同一 STEER 通道：支持时在当前 turn 内交付，否则等待该职位下一 Attempt。TUI 不新增 ASK/CHANGE/CONTROL 专门语法，输入种类通过共享 SendInput API 表达；CHANGE/CONTROL 的实现由后续切片交付。ASK 查询不会打断编码。暂停/取消通过控制面，不等模型同意。

## 团队、模型、Prompt与Skill

当前 `/team` 和 `/model` 显示附着 Run 的职位、Agent 与模型，并说明项目设置在 checkpoint 生效；它们不提供编辑入口，TUI 编辑属后续任务。设置页中的自定义职位、Prompt 覆盖、diff 和恢复默认属于后续界面范围；运行中的 attempt 使用锁定版本。项目内置 Skill 和文档模板不要求另装 Matt 工具；Pi 原生插件由 HoH 引导自动准备。

## 退出与恢复

关闭窗口/断开SSH不取消任务；明确选择cancel才终止工作。取消或Attempt结束时，HoH确认Attempt的进程都已结束，才结束Attempt、释放写者租约：host在受委托的cgroup中运行时用Attempt专属的cgroup，否则按/proc尽力查找（降级，`hoh doctor`会提示，例如用户没有开启linger；附着Run时状态栏也显示「跟踪降级 N」，即该Run中这样跟踪的Attempt数）。这一保证只覆盖Attempt自己的进程：Agent以同一用户把工作交给早已运行的服务（systemd --user、HoH之外启动的tmux server、cron、ssh localhost），或有意迁出cgroup、脱离进程组并隐藏标记、躲开尽力的扫描时，或同用户进程篡改host本身（改写它的数据库与文件、在自己持有特权的命名空间里重新启动它并伪造进程视图）时，这部分工作无法归属，可能在Attempt结束后继续写（REQ-043决定3，CR-012、CR-013）。暂停表示待安全点停止新派发，界面显示PAUSE_REQUESTED直至PAUSED。`/resume`打开历史选择器；非交互通过--resume指定run_id，不能无人值守随意选最近run。

## 异常解释

NEED_DECISION：缺少只能由你给出的决定，用 `hoh -p '<答复>' --resume <run_id>` 或在 TUI 中回答（原因 `no_spec`：项目还没有已批准的规格；`spec_approval`：规格待你批准，只有你能批准，Agent 进程的批准被拒为 SPEC_APPROVAL_NOT_USER；见“常用操作”）；BLOCKED_AUTH：需要原生登录（恢复见下）；BLOCKED_CAPABILITY：当前版本不支持所需接口（对应错误码 CAPABILITY_UNSUPPORTED，API 返回 422）；AGENT_UNAVAILABLE / MODEL_UNAVAILABLE：Agent 未安装或无法探测 / 配置的模型不可用（见下“职位缺员”）；COMMANDER_UNAVAILABLE：Commander 无法启动，已派发的 QA/Review 照常进行；BUDGET_UNENFORCEABLE：设置了费用上限但模型价格未知，不会继续花费；COMPAT_UNVERIFIED：原生 CLI 变了，host 在没有写任务时自动跑兼容套件（恢复见下）；DB_WAL_UNSUPPORTED / DB_ON_NETWORK_FS：项目所在文件系统不支持可靠的 SQLite WAL；WAITING_EXTERNAL：已保存next_retry；INVALID_EVIDENCE：版本或文件已变；INCOMPLETE：剩余需求/预算不满足；FAILED：不可恢复的运行失败或数据完整性异常。状态不是“AI坏了”的泛化报错；每项提供诊断和恢复动作。

**职位缺员**（ADR-030 决定 3–7、11）：职位的 Agent 未安装、不受支持、未登录、配置的模型不可用、缺该职位所需的能力（所有职位要 structured_events，commander、reviewer 另要 resume，developer、qa 另要 command_execution，commander 另要 read_only_workspace），或新版本的兼容套件 FAIL 或判不了时，HoH 按 `[team.<role>]` 的 `fallback` 顺序换到第一个可用的 Agent，commander 也一样；不在列表里的 Agent 不会被启用，默认没有 fallback。换绑等该职位正在运行的 turn 结束后生效（role_epoch+1，新会话从交接包开始；qa、reviewer 总在新会话里，不用开发者的会话），首选恢复后不自动换回。探测暂时失败、兼容待验证与 429/503 只等待，不触发 fallback。没有可用候选时该职位 BLOCKED，团队视图列出每个候选的缺项，如 `pi:not_installed`、`claude:BLOCKED_AUTH`、`codex:CAPABILITY_UNSUPPORTED:resume`（`<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace` 表示该 Agent 不能以只读工作区档当 Commander：被测 help/schema 不提供只读档、兼容清单的 read_only 用例未通过、codex 只有 exec，或这台主机上 codex 的沙箱检查为 unconfined、unavailable，或它的 App Server 在本 host 运行中降级为 exec；它在其他职位照常可用，主机的问题修好后至多 60 秒内恢复，见下“Commander 的只读档与余项”）；该职位到期的 Task 以同一原因 BLOCKED，不建 Attempt、不排重试，别的职位照常推进，没有别的事可做时 Run 才 BLOCKED；qa 或 reviewer 缺员时候选停在 VERIFYING，不会 ACCEPTED；commander 连 fallback 都不可用时才持续 COMMANDER_UNAVAILABLE。Agent 恢复可用（重新登录、升级后能力回来、兼容套件通过）后，HoH 自己给该职位新会话并把这些 Task 放回 READY，不需要重新提交。

**Commander 的只读档与余项**（CR-024，ADR-032 决定 7）：Commander 的每个 turn 都以只读工作区档启动（命令行与参数见 docs/13），它只读项目、不写任何文件；它的决策只取它最终消息里最后一个 `hoh-decisions` 块，写进文件的内容不会被登记（出现时记一条 `supervisor.bridge_ignored`）。只读的强制方式因底座而异；`hoh doctor` 的每个 Agent 项有 `read_only`：结论 `workspace`、强制方式 `enforced_by`（codex 为 `os_sandbox`，claude、pi 为 `tool_allowlist`）、组成部分 `parts`，codex 另有本机沙箱检查的结论 `check`。下列余项不在保证之内，按所有者的决定如实记录（ADR-032 余项 R1–R7，与上文 ADR-030 的 R1–R3 无关）：R1，codex 的只读由它自己的操作系统沙箱强制，但你自己配置的 MCP 服务器、钩子与账号启用的应用连接器在沙箱之外运行，它们若能写文件，模型可经它们写；R2，codex 的本机沙箱检查为 unknown（超时，或 codex 不认这个档名）时，这台主机上没有沙箱是否建立的本机证据，codex 仍可当 Commander，host 每 60 秒重测；R3，pi 的只读只靠 pi 进程内按工具名的允许清单，没有操作系统级屏障，Commander 的 pi 会话不加载任何扩展（`--no-extensions`），所以 v1 的三个 Pi 插件只服务于其他 Pi 职位、不进 Commander 的会话，只靠扩展注册的模型提供方（如 llama.cpp）的 pi 当不了 Commander（它的 turn 失败，按监督的失败规则重试）；R4，claude 的只读只靠 Claude Code 进程内的工具集与权限判定，你的 ~/.claude 设置里的钩子与插件照常运行；R5 不适用：Commander 不经 codex exec 启动，codex 只有 exec，或它的 App Server 在本 host 运行中降级为 exec 时，codex 不当 Commander，直到 host 重启或换版本；R6，claude 的工具名 Read、Grep、Glob 不在 help 里，由每次发布的 adapter-live 检查实测；R7，同一用户的其他进程仍能写项目（同用户的边界见“退出与恢复”）。

**BLOCKED_AUTH 的恢复**：用该 Agent 自己的登录方式重新登录。host 约每 30 秒用 Agent 的状态命令查看一次登录：状态命令说未登录时，登录后的下一次查看即解除。turn 中发现的鉴权失败（这时状态命令可能仍说已登录）只在 HoH 看到一次“未登录→已登录”的转变、该 Agent 换成新的版本文件（升级、重装）或 host 重启时解除；只说“已登录”的查看不解除它，HoH 也不按时间重试（REQ-020 行为 7）。已知余项（ADR-030 决定 13 (a)）：Agent 的状态命令一直说“已登录”而令牌其实已失效时，同一版本的这次失败在本 host 运行期间不会解除；重新登录后重启该项目的 host 即可，令牌若仍无效，重启后至多再失败一个 turn 又回到 BLOCKED_AUTH。

**COMPAT_UNVERIFIED 的恢复**：原生 CLI 变了——升级、重装、链接改指或 PATH 上换了程序，即启动器或实现的路径与 sha256、`--version`、help 或 schema 与 `.hoh/compat-manifest.json` 不符，或清单不是这个 HoH 版本的套件写下的——除 R1、R2、R3（下文）之外，新 Attempt 不使用这个未验证的版本，运行中的 Attempt 照常运行到结束。host 在项目没有写租约时自动对它运行兼容套件（离线，不调用模型）；有写任务在运行时套件等它结束，事件 `compat.suite_deferred` 写明“维护窗口：写租约释放后自动运行兼容套件”，这期间不开始新的写任务，只读任务照常。套件 PASS 后清单登记新版本，相关职位换新会话，被拒的 Task 自动回到 READY；每次 CLI 变化，已有会话的职位上每个到期 Task 至多被拒一次（一个 FAILED 的 Attempt，计入 attempt_count），之后等待而不再派发；Commander 的 Agent 待验证时唤醒延后，不算 Commander 不可用。HoH 自己升级后每个 Agent 也重跑一次套件，期间新 turn 等待。PASS 之前 `hoh doctor` 把该 Agent 的 protocol 记为 unknown 并写出原因。套件 FAIL 或判不了对这个版本是最终结果，不再重跑（CLI 或 HoH 换了版本才重跑）：按 team 配置的 fallback 换绑，没有可用 fallback 时该职位 BLOCKED（COMPAT_UNVERIFIED）；换回已验证的版本或装另一版本（会自动重新验证）即可恢复。

**已接受的余项 R1、R2、R3**（ADR-030 决定 9；批准人：所有者，R1、R2 于 2026-11-13，R3 于 2026-11-14）：HoH 在启动 Agent 之前核对它的文件，并直接执行核对过的那个文件；下列情形不在这一保证之内。R1：核对完到开始执行之间，有程序就地改写同一个已安装文件（正常升级写新文件或新目录，不会这样）。R2：脚本实现的 Agent（pi、npm 安装的 claude）在解释器读到主脚本之前路径被换成别的文件；脚本之后再加载的包内文件与解释器本身不在核对范围内。R3：Agent 启动之后按路径再启动的自身或包内辅助程序（如 codex 的 `apply_patch` 与沙箱辅助程序，claude 在旧版本文件被删除后经启动器执行的搜索），在 turn 进行中有新版本装入时运行的是新版本，主进程仍是核对过的版本。另有两次再核对收窄它们：pi（RPC）与 codex App Server 在发出第一个工作输入之前再看一次启动器与实现文件，变了即不发输入、结束这个 Attempt（COMPAT_UNVERIFIED），提示词没有交付，Task 等新版本通过套件后再派发（只是权限之类的元数据变了、内容没变时按普通失败退避重试）；每个 turn 进行中每 200 ms 再看一次，变了只在该 Attempt 上记一条诊断 `IMPLEMENTATION_CHANGED_IN_TURN`，turn 照常结束（运行中的 Attempt 不受影响，AC-027-04），之后的新 Attempt 照样等套件通过。

`hoh` 与 `hoh -p` 的退出码（REQ-004）：0 COMPLETED；1 FAILED；3 BLOCKED；4 INCOMPLETE；5 CANCELLED；6 PAUSED；64 用法错误；69 host 不可用；78 配置错误；130 被 SIGINT 中断（后台任务继续）；143 被 SIGTERM 中断。

### 已有规格时继续规划

运行中输入 `/plan`，模式会在当前写入安全结束后生效。HoH 保持 Run 的工作阶段，以当前已批准规格起草变更；暂停期间不开始新草稿，恢复后继续。起草完成会提出 CR 并等待你批准；切换到 BUILD 不会替你批准，也不会跳过新修订的独立规格审查。

已经运行的 Run 中，不受规格缺项影响的就绪任务和在途工作继续推进；这些工作结束后，再处理尚未解决的缺项。AUTO 遇到缺少验收条件、无法观察的条件或待定内容时会请求补充规格，并停在批准问题前；无人值守的 `-p` 此时以 3 退出。BUILD 遇到这些缺项会保留 readiness 阻塞，提示切到 PLAN/AUTO 或修复后重试。TUI 选择退回草稿时，先输入非空修改意见再按 Enter；Esc 返回选择。取消草稿会保留已写入的文件和提交。

## 最终报告

包含产物版本、发布状态（COMPLETED / COMPLETED_DEGRADED 及降级清单 / INTERIM_RELEASED / INCOMPLETE / BLOCKED / FAILED）、已验收范围、未解决事项、测试真实/模拟/未运行分类、费用实际/估算/未知、部署与回滚步骤。不能只给一句完成。模型退出与HoH完成分别显示。

### 职位契约设置

在 TUI 输入 `/settings developer`（或 `/settings`）打开 developer 契约的统一 diff。附着 Run 后也可使用。↑↓、PgUp、PgDn 浏览；按 r 恢复默认，Esc 返回。恢复默认只删除项目的 `.hoh/prompts/developer.md`，下一 Attempt 重新读取覆盖链；若有 global 或 run 覆盖，它们仍按原优先级生效。安装资源不被修改。配置 `prompt` 指定其他文件时，恢复操作拒绝删除，须先移除该配置再操作约定路径。

覆盖文件的基准放在同名 `.base.json` 旁路，不进入 Prompt 正文。资源包升级后，设置页会标出漂移，并在 diff 末尾给出旧内置、新内置、覆盖的对照；手工放置或修改而没有匹配基准时，会明确提示旧内置未知。运行中已开始职位的资源版本到其无在途 Attempt 的 checkpoint 才更新；等待期间暂停该职位的新派发，已有 Attempt 仍用旧版本，checkpoint 推进后自动恢复派发。覆盖文件仍在每次 Attempt 开始前重新读取。
