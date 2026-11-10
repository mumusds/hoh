# 使用手册（目标行为规范）

本手册描述AI完成后的HoH行为，不代表本启动包已提供hoh程序。开发启动见 START_HERE.md。

## 安装与卸载

解开发布包后执行 `hoh/install.sh`（不需要 root，也不调用 sudo）：`hoh` 装到 `~/.local/bin`，资源按版本放在 `~/.local/share/hoh/versions/`，`current` 原子切换到新版本；同时安装 systemd 用户 unit 模板 `hoh-host@.service`。加 `--project <目录>`（可重复）为该项目启用 host unit：开机或用户管理器启动时拉起它，崩溃后由 systemd 重启（5 分钟内最多 5 次，每次间隔 5 秒）。启用需要用户已开启 linger，否则 host 会随最后一个登录会话一起停止；`--linger` 经 logind 为你开启。不加 `--project` 时不启用任何项目；已开启 linger 时，第一次在项目里执行 `hoh` 照样经 unit 拉起 host（没有 linger 时不经 unit，host 崩溃后不会自动重启，`hoh doctor` 会说明）。

`install.sh --uninstall` 停止并禁用所有 `hoh-host@` 实例，删除二进制、模板与安装的文件。以下情况它以 69 退出、列出原因，什么也不删：还有已安装的 hoh 在运行（不经 unit 拉起的 host、打开着的 TUI、`hoh attach`、`hoh doctor` 都算）；有实例停不下来；管理器不可达而仍有启用的实例；安装的锁 `~/.local/share/hoh/.lock` 打不开、加不上锁或无法确认（这时它照实说明，不说有程序在运行）；安装器自己的锁建不了、打不开、加不上锁，或 60 秒内一直被另一个 install.sh 持有；卸载期间又被启用的实例停用不了；删除所依据的某项无法判断（`~/.local/bin/hoh` 读不到或所在目录不可搜索、unit 配置目录或其中的 `.wants`、`.requires`、`.upholds` 目录列不出、路径上某个符号链接指向不可搜索之处、管理器不回答实例是否启用或是否已停止）；某个实例被 `systemctl --user mask` 屏蔽（先 `unmask` 再卸载）。没有安装时它报告 `not installed`，以 0 退出，什么也不写；安装目录 `~/.local/share/hoh` 是指向不存在之处的链接时，它照常删除模板与 `~/.local/bin/hoh`，留下这个链接。与 HoH 无关的 drop-in 目录（`*.d/`）读不了、`~/.config/systemd/user` 是普通文件，都不妨碍卸载。卸载删除模板后让管理器重新载入（`systemctl --user daemon-reload`）失败时，其余照删并逐项列出，最后说明这一失败、以 69 退出：请自己运行一次 `systemctl --user daemon-reload`。安装或卸载遇到 install.sh 没有专门说法的失败（例如磁盘写满）时，给出失败的命令、它的退出码与行号，以 74 退出。卸载进行期间，任何已安装的 hoh（包括 `hoh --version`）都以 69 退出，说明本安装正在卸载；同时运行的两个 install.sh（安装或卸载）轮流进行，卸载后留下一个空文件 `~/.local/share/hoh.lock`，是安装器自己的锁；若 `~/.local/share/hoh/.lock` 丢失，已安装的 hoh 会说明这一点并以 69 退出，重新运行发布包里的 install.sh 即可补上；各项目的 `.hoh/`、`~/.config/hoh/`、你写的 drop-in 与 linger 设置原样保留。升级：再执行新包的 `install.sh`；正在运行的 host 继续用旧版本，直到它重启。unit 托管的 host 只得到项目路径与执行 `hoh` 时的 PATH，不复制其他环境变量：靠环境变量（API key、代理）认证的 Agent，需要把变量写进 `~/.config/environment.d/*.conf` 或 `systemctl --user edit hoh-host@.service` 的 drop-in。`hoh doctor` 报告 host 是否由 unit 托管，不是时说明原因。

## 第一次打开

在项目目录执行 `hoh`。若缺配置，TUI引导检测Codex/Claude/Pi、模型可用性与项目目录，不要求用户先记住init命令。已有文档自动发现；不存在时内置规格模板可用。

默认Auto；复杂产品先用 `/plan` 讨论目标，或Shift+Tab切换。Plan完成后在同会话选择开始实施。工作模式和权限模式不同，用户无需理解底座CLI的权限枚举。

## 常用操作

`hoh -p "依据docs/PRD.md完成项目"`：前台非交互；`--output json`最终机器结果，`--output stream-json`实时事件。`--detach`先输出run_id和提交确认，任务后台继续。`hoh attach <id>`打开该运行。`hoh web`连接相同host。`hoh doctor`看环境能力：默认不调用模型，退出码0只表示没有缺失项，不代表已认证；`hoh doctor --live`才用一次小任务确认真能工作（可能产生费用）。

不存在hoh plan/hoh mode plan/hoh run子命令。Shell帮助保持短；TUI输入/可搜索命令，低频设置从面板进入。

## 观察团队

Workspace看统一进展与Commander决策；Team按需分屏；选中Agent进入Focus；Timeline按项目/轮次/角色筛选。点击任务、diff、测试或证据查看来源。长日志默认折叠，不挤占对话。

“已验收18/25”只统计当前规格有效验收。新需求导致分母变化会显示规格版本；回归使已验收变少但保留历史。模型/上下文属于选中Agent，不显示虚假的全团队上下文百分比。

## 与团队交流

默认问Commander。@cto、@qa或@developer定向咨询；品牌名有多个职位会显示实际路由。问进度不会打断编码。优先级调整在下个checkpoint生效；需求变更有CR和影响分析。暂停/取消通过控制面，不等模型同意。

## 团队、模型、Prompt与Skill

/team选择默认职位与Agent/模型，也可新增自定义职位。角色切换提示什么时候生效。设置页可查看默认Prompt、创建覆盖、diff、恢复默认；运行中的attempt用锁定版本。项目内置Skill和文档模板不要求另装Matt工具；Pi原生插件由HoH引导自动准备。

## 退出与恢复

关闭窗口/断开SSH不取消任务；明确选择cancel才终止工作。取消或Attempt结束时，HoH确认Attempt的进程都已结束，才结束Attempt、释放写者租约：host在受委托的cgroup中运行时用Attempt专属的cgroup，否则按/proc尽力查找（降级，`hoh doctor`会提示，例如用户没有开启linger）。这一保证只覆盖Attempt自己的进程：Agent以同一用户把工作交给早已运行的服务（systemd --user、HoH之外启动的tmux server、cron、ssh localhost），或有意迁出cgroup、脱离进程组并隐藏标记、躲开尽力的扫描时，或同用户进程篡改host本身（改写它的数据库与文件、在自己持有特权的命名空间里重新启动它并伪造进程视图）时，这部分工作无法归属，可能在Attempt结束后继续写（REQ-043决定3，CR-012、CR-013）。暂停表示待安全点停止新派发，界面显示PAUSE_REQUESTED直至PAUSED。`/resume`打开历史选择器；非交互通过--resume指定run_id，不能无人值守随意选最近run。

## 异常解释

NEED_DECISION：缺少只能由你给出的决定，用 `hoh -p '<答复>' --resume <run_id>` 或在 TUI 中回答；BLOCKED_AUTH：需要原生登录；BLOCKED_CAPABILITY：当前版本不支持所需接口（对应错误码 CAPABILITY_UNSUPPORTED，API 返回 422）；COMMANDER_UNAVAILABLE：Commander 无法启动，已派发的 QA/Review 照常进行；BUDGET_UNENFORCEABLE：设置了费用上限但模型价格未知，不会继续花费；COMPAT_UNVERIFIED：原生 CLI 版本变了，需要先跑兼容套件；DB_WAL_UNSUPPORTED / DB_ON_NETWORK_FS：项目所在文件系统不支持可靠的 SQLite WAL；WAITING_EXTERNAL：已保存next_retry；INVALID_EVIDENCE：版本或文件已变；INCOMPLETE：剩余需求/预算不满足；FAILED：不可恢复的运行失败或数据完整性异常。状态不是“AI坏了”的泛化报错；每项提供诊断和恢复动作。

`hoh` 与 `hoh -p` 的退出码（REQ-004）：0 COMPLETED；1 FAILED；3 BLOCKED；4 INCOMPLETE；5 CANCELLED；6 PAUSED；64 用法错误；69 host 不可用；78 配置错误；130 被 SIGINT 中断（后台任务继续）；143 被 SIGTERM 中断。

## 最终报告

包含产物版本、发布状态（COMPLETED / COMPLETED_DEGRADED 及降级清单 / INTERIM_RELEASED / INCOMPLETE / BLOCKED / FAILED）、已验收范围、未解决事项、测试真实/模拟/未运行分类、费用实际/估算/未知、部署与回滚步骤。不能只给一句完成。模型退出与HoH完成分别显示。
