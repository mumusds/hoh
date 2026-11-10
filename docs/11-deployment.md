# 环境、构建、部署与回滚

## 两套环境不要混淆

本包开发环境：Ubuntu 24.04/26.04非root用户，可免交互sudo装系统包；Git、Python3、Rust、Node、三Agent，网络/认证由用户提供。正式HoH运行环境：发布二进制+内置资源+可用Agent，Python不是生产Runtime依赖。

用户明确选择YOLO隔离VM。本包不新增隐私/权限沙箱工程，也不改全局遥测设置。启动第三方工具使用探测后确认的YOLO参数；无需把进程以root运行。若native CLI不支持root下bypass，使用VM普通开发用户，这是工具兼容要求。

## 开发初始化

入口 `scripts/bootstrap.sh --yolo`。默认幂等：检查环境，缺依赖自动准备；已有Agent不擅自升级；项目内建Skill/模板始终存在；安装Pi插件到项目可控范围。插件安装失败时记为DEGRADED，bootstrap继续（CR-001）。写 `.hoh-dev/env.sh`（同时加载可选的 `.env`）、分项version.lock、python-lock.txt、plugin-lock.json（含npm integrity）和preflight.json，保存实际版本/帮助检查状态，不写凭证。以root运行时脚本直接拒绝（含 `--skip-install`）。

`--dry-run` 不写项目、不安装、不联网，只显示将进行的步骤。`--skip-install` 只准备本地开发资源，明确缺依赖，适合离线审核；`--skip-plugins` 只用于诊断，记录未安装状态，native help检查可以单独通过，但不能据此宣告插件或最终验收通过。

Node：当前使用满足>=22.19的可用版本；缺失时从官方发布索引选择受支持LTS，校验SHASUMS后解压至用户目录。Rust：官方rustup [S28]；解析实际stable后写锁定文件。不会在活跃长任务中反复跑update。

Claude优先原生官方安装 [S30]；Codex官方npm包；Pi按当前官方README选择包 [S10]，所有实际解析结果写lock。网络不可用不能号称安装完成。Python测试工具仅在开发期使用。

## 认证与费用

认证是一次性真实外部条件：使用三个CLI各自原生登录或已有provider配置。脚本不得伪造/复制账户，也不在日志打印API key。版本检查不是登录检查；由 `tools/preflight.py --live` 小任务调用判断真正可工作。该模式会产生模型费用；用户本次已授权开发使用，初始任务预算通过环境/config限定。

## 生产构建目标

Cargo workspace 已在 M0-T02 建立；build 检查（`scripts/product/build.sh`）执行 `cargo build --workspace --release --locked`，在 run_check 下把二进制、resources 与（M6 起的）Web 资产打成可复现的 `hoh-release.tar.gz`（CR-003）。M6-T02 起执行 `npm ci --prefix web && npm run build --prefix web`，并把缺少 web/ 改为构建失败；v0.1 的发布包不含 Web 资产。构建资产版本写同一manifest。安装器支持安装到 ~/.local/bin，资源按版本目录，current原子切换；现有用户覆盖文件不覆盖。

当前 `hoh` 只是不提供功能的骨架；检查计划中尚未实现的检查以 NOT_IMPLEMENTED（退出码3）失败，不以占位程序假装已有产品。安装器（M2-T14，ADR-023）把二进制装到 ~/.local/bin、安装 systemd --user unit，卸载时保留项目 .hoh/（AC-005-06）：发布包内的 `install.sh [--project <目录>]... [--linger]` 把包复制到 `$XDG_DATA_HOME/hoh/versions/<semver>-<二进制 sha256 前 12 位>/`（写成后不再改），以新链接 rename 覆盖的方式切换 `current` 与 `~/.local/bin/hoh`，并由已安装的二进制生成 unit 模板 `$XDG_DATA_HOME/systemd/user/hoh-host@.service`；同一个包再装一次什么也不改。`~/.local/bin/hoh` 已存在且不是本安装的链接时拒绝（退出码 1），以 root 运行时拒绝（77），全程不调用 sudo。拒绝类检查（含包内缺少 `bin/hoh`，64）都在写入任何东西之前。安装与卸载彼此轮流（各自持有安装器锁 `$XDG_DATA_HOME/hoh.lock` 的排他锁，最多等 60 s；这个空文件在卸载后保留）；只有发现什么也没装的卸载不取锁，它报告 `not installed`、什么也不写。`--uninstall` 停止并禁用全部实例，再取安装锁 `$XDG_DATA_HOME/hoh/.lock`（已安装的 hoh 每次运行都持有它的共享锁，卸载持排他锁期间新起的 hoh 以 69 退出）；它被持有，或打不开、加不上锁、无法确认（照实说明，不说有程序在运行），或仍有进程在运行安装的二进制（已 exec、尚未取锁的也算），或有实例没停下、管理器不可达而仍有启用的实例时，以 69 退出，什么也不删；否则再删除模板、实例文件、版本目录与链接，不动 drop-in、`~/.config/hoh/`、linger 与任何项目的 `.hoh/`。`hoh --version` 的第二行是构建时的 commit（`scripts/product/build-commit.sh`）。deployment 检查（`scripts/product/deployment.sh`）用候选构建发布包，在私有的用户级 systemd 管理器中跑 TEST-005-06。

## 后台运行

正式host与TUI分进程，通过Unix socket和锁文件发现。TUI关闭/SSH断开只是detach。systemd --user负责host异常重启，Restart=on-failure +有界StartLimit（StartLimitBurst=5、StartLimitIntervalSec=300、RestartSec=5 s；以 64、69、78 退出时不重启）；ExecStart只用已安装受支持隐藏daemon入口（`hoh __host`，ADR-023 决定 1）。实例名是项目规范路径 sha256 的前 16 位（`hoh-host@<hash>.service`），项目路径与 PATH 在每实例一个的环境文件中（`$XDG_DATA_HOME/hoh/hosts/<hash>.env`，在安装根之下；HoH 只写、只删安装根之下的文件，从不写管理器配置给出的其他路径，ADR-023 设计修正 1）。已安装的 `hoh` 拉起 host 时优先经该 unit（`systemctl --user start`），于是 host 崩溃后由 systemd 重启并先 reconcile；StartLimit 已满时客户端以 69 退出，不绕过限速。不是已安装的 `hoh`（开发构建、测试）或 unit 不可用时，按下面的受委托 scope 或直接拉起，原因写进 `host.json`，由 `hoh doctor` 报告（ADR-023 决定 6、7）。unit 托管的 host 只得到项目路径与 PATH，不复制凭证与代理等环境变量。unit 带 `Delegate=yes`：host 在受委托的 cgroup 中运行，每个 Attempt 以其专属子 cgroup 证明终结（CR-011）。客户端直接拉起 host 时，只在用户级 systemd 管理器可达且已开启 linger 时经 `systemd-run --user --scope -p Delegate=yes` 拉起；否则直接拉起，Attempt 的跟踪降级为 /proc 尽力方式并被记录，原因写进 `host.json`，由 `hoh doctor` 报告（ADR-021 决定 2、8d）。终结证明只覆盖 Attempt 自己的进程（cgroup、进程组与继承的环境标记）；同一用户的进程有意迁出并隐藏标记或躲开扫描、把工作交给早已运行的同用户服务，或篡改 host 本身（数据库与文件、启动方式与命名空间视图）时无法归属，这些余项由项目所有者接受，`hoh doctor` 写明（REQ-043 决定 3，CR-012、CR-013）。用户logout后持续运行需linger，脚本只在明确启用时配置（本开发 VM 已由 `scripts/dev/test-infra.sh` 经项目所有者授权启用）。

自举开发可选 `scripts/start.sh codex --headless`，只是一个原生Agent调用，不宣称它本身拥有完整HoH持续恢复。原生Agent达到限制退出后记录checkpoint；使用RESUME_PROMPT继续，不用无限“继续”循环掩盖退出。

## Web访问

默认监听127.0.0.1，浏览器UI和API同源；远程通过 `ssh -L 8787:127.0.0.1:8787 vm`。这是简化本地开发产品的连接方式，不增加多租户网关。外部部署到公共网络不属于本版已验证默认配置。

## 备份/升级/回滚

先暂停新派发→drain安全点→在线SQLite backup→备份resources/config/对象索引→新版本schema验证→原子切换→smoke；失败恢复旧binary和对应DB备份。活跃WAL不能只拷贝主文件 [S22]。保留最终证据引用的对象；空间不足先阻止新任务，不先删证据。

回滚代码保持失败branch/worktree供分析；不reset用户dirty工作区。GitHub merge产生新SHA要重新绑定验证。无remote时完成本地发布，不因没有GitHub强制宣称失败或自动创建远程账户。
