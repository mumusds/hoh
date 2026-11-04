# 开发工具与环境脚本

本目录的脚本已作为开发包实现；目标 Rust 产品仍须开发。Python 只是开发辅助语言，不承担正式 HoH 循环。所有工具与脚本都拒绝以 root 运行原生 Agent。

## 环境准备与锁版本

`bootstrap.sh --yolo` 按需安装系统构建依赖、Node（满足 Pi 要求）、Rust、jsonschema 开发 venv 和缺少的原生 Agent；已有安装、认证和模型默认值保留。新安装的版本只解析一次，记录到 `.hoh-dev/*version.lock`，不在正在执行的 Run 里自动追最新版。Rust 产品由 Cargo.lock 与 rust-toolchain 进一步锁定，Web 由 package-lock 锁定。Node 包下载后核对官方 SHA256。

Pi 插件由 `tools/install_pi_plugins.py` 安装：只要 Pi 支持就带 `--approve`，版本与 npm integrity 固定在受版本控制的 `config/plugins.json`（安装时与 registry 比对，不符即 DEGRADED），安装状态记录在 `.hoh-dev/plugin-lock.json`。某个插件装不上时，它被记为 DEGRADED（附错误信息），其余插件与 bootstrap 继续（CR-001）。Pi 在非离线模式下启动时会自动安装 `.pi/settings.json` 中缺失的包，这一步不经过上述校验；因此 `tools/agent_task.py` 与 `scripts/start.sh` 都以 `PI_OFFLINE=1` 启动 Pi（Pi 1.5.0 源码确认离线时跳过缺失或版本不符的包，不安装），启动前用 `python3 tools/install_pi_plugins.py --verify` 核对每个已安装的项目包：版本须与 settings 一致，integrity 须等于锁中固定的值，非 npm 来源一律拒绝；有不符即拒绝启动（退出码 78）。未安装的包只记为 absent，Pi 不加载它。剩余风险：校验的是 npm 记录的 tarball integrity，不检测安装后磁盘上的改动；用户级 `~/.pi/agent/settings.json` 的包不在项目校验范围内，离线模式只保证启动时不会安装它们。之后 AI 必须读取已安装版本的配置 schema，明确 Smart Compact 可选记忆的关闭方式、Hermes 的项目作用域，以及 RPC 扩展 UI 的响应方式。无法证实的配置不能凭空生成。

`--skip-install` 不访问发行源，但会探测本机版本；返回 3 表示能力缺失。`--dry-run` 不写入、不下载、不调用模型。bootstrap 子 Shell 不会改变父 Shell 环境，**之后每个新 Shell 都要先 `source .hoh-dev/env.sh`**；它也会加载可选的、已被 gitignore 的 `.env`（见 `.env.example`）。脚本不能提供认证；`preflight --live` 需要显式选择，并可能产生模型费用。

## 任务台账

`tools/taskboard.py list|next|claim|set`：claim 原子领取并记录 owner 与 writer 标记；同一时间只允许一个 writer 任务，以台账中运行条目记录的 writer 标记为准，没有该标记的运行条目（未知任务或旧版 taskboard 写入）一律按 writer 处理（`next` 在已有写任务运行时只推荐非写任务）；依赖 outline 需求的 implement/verify/release 任务不可领取；状态迁移受限，重新打开已完成任务会阻塞已开始的下游任务。台账固定在主 worktree 的 `.hoh-dev/task-state.json`，链接 worktree（例如冻结的 soak 检出）共享同一台账。任务的 `acceptance`/`checks`/`profile` 字段与归属规则见 docs/08。

## 原生单次任务桥

先用 `tools/render_prompt.py` 把 `resources/prompts/{role}.md` 与项目/任务/证据 JSON 渲染成完整 Prompt，再调用：

```bash
source .hoh-dev/env.sh
python3 tools/render_prompt.py --role reviewer --context .hoh-dev/review-context.json --output .hoh-dev/reviewer-prompt.md
python3 tools/agent_task.py --agent codex --role reviewer --prompt-file .hoh-dev/reviewer-prompt.md --timeout 3600
```

真实任务必须引用当前 REQ 与候选，不能拿示例上下文验收。桥会探测实际 help（按整词匹配参数），记录 CLI 版本、会话 id 和解析出的模型，不把聊天中的“完成”当最终成功。Pi 的 JSON 单次进程退出，不能替代产品 Pi RPC 中对 `agent_settled` 的协商。

桥默认插入局部任务信封，并给子进程设置 `HOH_BOOTSTRAP_PARENT`，阻止下级会话递归启动顶层会话。`--top-level` 只允许 commander/developer 角色，且只能在不处于任何自举会话中时使用。桥不重试、不调度下一任务、不充当总指挥。原生子进程的完成记录永远是 `product_accepted:false`。成功判定只看终态：Codex 的 `turn.completed`/`turn.failed`，Claude 的 `result`，Pi 的最后一条助手消息与 `auto_retry_end`；原生 CLI 自己重试的瞬时错误计入 `transient_errors`，不判失败。Pi JSON 模式的会话 id 取自首行 `{"type":"session","id":…}`。

**独立报告（`--report-out`，仅 qa/reviewer）：** 会话结束后，桥从最后一条助手消息的 ```hoh-report 代码块读取 `{role, status, scope, findings}`，补上 session_id（必须恰好一个）、candidate_id 与 spec_sha256（来自 run_check 设置的 `HOH_CANDIDATE_ID`、`HOH_SPEC_SHA256`）、raw_session 与 native_attempt 的路径和哈希，按 `contracts/independent-report.schema.json` 校验后写出。Agent 自己写身份或哈希字段会被拒绝（退出码 65，原因写入 attempt.json 的 `report_error`）。超时会终止本次进程组并保存失败；收到 SIGTERM 时记录 INTERRUPTED。在 `run_check` 之下运行时（存在 `HOH_EVIDENCE_DIR`），attempt 目录默认放在该检查的证据目录内，记录被测 candidate_id 与 check_id，并把观察到的会话追加到该检查的 `live-sessions.json`。这正是独立 QA/Review 检查应有的运行方式。SIGTERM/SIGHUP 会先停止原生进程组，再写入 INTERRUPTED 记录。

## 检查计划、冻结候选与执行检查

测试计划是机器可读的检查计划（`contracts/check-plan.schema.json`，示例见 `examples/check-plan.json`，产品计划放在 `tests/check-plan.json`）。它为每个 check_id 声明 mode、argv、cwd、超时和必须产出的文件。实现阶段在 Git 中提交本次源码，保持工作区干净，日志输出放到被忽略的 `.hoh-dev/`：

```bash
python3 tools/preflight.py --output .hoh-dev/environment.json
python3 tools/candidate.py freeze \
  --spec docs/01-prd.md --environment .hoh-dev/environment.json \
  --test-plan tests/check-plan.json --out .hoh-dev/candidates/candidate-001.json
python3 tools/run_check.py --check-id unit \
  --candidate-file .hoh-dev/candidates/candidate-001.json --out .hoh-dev/evidence/candidate-001/unit
```

- **执行内容：** `run_check` 只执行计划中的 argv；`--` 后给出命令时必须与计划完全一致。mode 与 cwd 只能来自计划。
- **环境变量：** 子进程拿到 `HOH_EVIDENCE_DIR`、`HOH_CHECK_ID`、`HOH_CANDIDATE_ID`、`HOH_SPEC_SHA256`、`HOH_CANDIDATE_FILE`、`HOH_CHECK_RECORD_ID`。
- **逐测试结果：** 凡是某条 AC 的场景检查，都要在 `$HOH_EVIDENCE_DIR/test-results.jsonl` 写逐测试结果（`contracts/test-result.schema.json`）；build 检查产出 `hoh-release.tar.gz`；independent-qa/independent-review 产出 `independent-report.json`。validate_bundle 要求检查计划声明这些产物。
- **产物：** 计划声明的产物缺失时检查失败；证据目录只能追加，不能覆盖。
- **信号：** SIGTERM/SIGHUP/Ctrl-C 时先停止检查进程组（给内层 agent_task 留 15 s 收尾），再写入退出码 130 的执行记录。候选清单必须位于项目内。
- **候选校验：** 冻结清单保存 Git commit/tree、全部已跟踪文件的 hash 和 spec/environment/test plan 的 hash，检查前后都会校验。候选改动、环境清单被改、出现未跟踪源码都会被拒绝。
- **身份规则：** 测试计划必须是已提交的文件。自举工具拒绝子模块和符号链接，候选清单以排他方式写入。正式产品如需支持这些，必须另做显式身份规则和测试。

## 最终 Manifest 与门禁

按 `contracts/release.schema.json` 汇总以下内容：profile、profile 要求的全部检查、范围内每条 AC 的观察记录、两个独立会话报告（引用原生 attempt 记录）、soak 时间线（v1）、发布物与 CandidateManifest。每条 AC 观察都要引用其场景指定的检查，并以该检查的输出作为原始证据。不能把同一句“通过”复制给所有 AC。

```bash
python3 tools/release_gate.py --manifest .hoh-dev/release/manifest.json
```

门禁规则见 docs/10。**它不理解任意软件的全部语义，也不能防止拥有整个 VM 写权限的执行者刻意伪造所有材料。** 独立 QA/Reviewer 仍必须审查真实执行情况和需求覆盖度。

## 基线、CR 与生成视图

- `tools/baseline.py verify`：核对基线锁和 CR 记录（`docs/cr/`；必须包含 `ID：`、`状态：APPROVED`、`批准人：` 三行）。
- `tools/baseline.py relock --cr docs/cr/CR-00N-….md [--baseline-id …] [--add <新契约文件>] [--drop <非门禁文件>]`：为已批准的 CR 生成新的基线修订；CR 尚未提交时可用 `--amend` 重新计算。
- `tools/gen_spec_views.py`：由契约生成 `docs/02-functional-spec.md` 与 `docs/18-traceability.md`。`validate_bundle` 发现视图过期就会失败。

## 开发与测试基础设施

- `scripts/dev/test-infra.sh [--dry-run]`：安装 strace、sqlite3、测试用 sshd（`scripts/dev/extract-test-sshd.sh` 下载发行版 openssh-server 包并解压到 `.hoh-dev/tools/openssh-server/<版本>/`，只安装其库依赖，不作为系统包安装，因此不会创建系统 sshd 配置，也不会启用或启动 ssh.socket/ssh.service；下载或解压失败、被中断时不留临时文件；唯一会动的系统 SSH 文件是旧版脚本写过、带标记行的 `/etc/ssh/sshd_config.d/50-hoh-test-loopback.conf`，只删除它，不重启任何服务）、qemu/KVM 与 cloud-image-utils、Chromium 的 Playwright 系统依赖、cargo-deny，并把开发用户加入 kvm 组、启用 linger；结果写入 `.hoh-dev/test-infra.json`。幂等，可重跑。
  SSH 测试（REQ-015/REQ-061）按 `.hoh-dev/test-infra.json` 的 `test_sshd` 路径自行启动 sshd：临时目录里生成主机密钥与用户密钥，配置至少包含 `ListenAddress 127.0.0.1`、空闲端口、`HostKey`、`PidFile`、`AuthorizedKeysFile`（临时文件）、`PasswordAuthentication no`、`KbdInteractiveAuthentication no`、`UsePAM no`、`StrictModes no`；包里有哪个辅助程序就加哪个配置项：`sshd_session` 非 null（OpenSSH ≥ 9.8）时加 `SshdSessionPath`，`sshd_auth` 非 null（≥ 10.0）时加 `SshdAuthPath`，指向解压目录。Ubuntu 24.04 的 9.6p1 包只有 sshd，两项都不加；Ubuntu 26.04 的 10.2p1 两项都要加；用 `sshd -D -e -f <配置>` 前台运行，测试结束时终止并删除临时目录。
- `scripts/dev/session-context.sh`：Claude Code 的 SessionStart 钩子（`.claude/settings.json`），在会话启动、恢复或压缩后打印分支、未提交改动、运行中任务与 PROGRESS 的下一步；委派会话（设置了 `HOH_BOOTSTRAP_PARENT`）不输出，保持审查独立。
- `deny.toml`：依赖许可、来源与禁用规则；static 检查离线运行 `cargo deny check licenses bans sources`，CI 另外检查 RustSec 公告。
- `scripts/product/persistence_scan.py`：AC-062-05 的静态扫描，由 static 检查运行。扫描 `crates/*/src/**/*.rs`、各 crate 的 `Cargo.toml` 与 `web/`（存在时），在 `crates/hoh-core/src/persistence/` 以外查找 SQL 写语句（字符串字面量中的 INSERT/REPLACE INTO、UPDATE … SET、DELETE FROM、UPSERT）、rusqlite 写调用与任何 rusqlite 引用、hoh-core 以外的 rusqlite 依赖以及 web 的 SQLite 依赖；注释不计。命中数为 0 且扫描文件数大于 0 才 PASS；在 run_check 下把 TEST-062-05 的结果追加到 `test-results.jsonl`。

## 工具退出语义

- 包自测：0 表示启动包通过；product 缺实现或清单时返回 3。
- 原生桥：原生进程失败返回非 0；超时 124，中断 130，缺 CLI 127，不支持参数或以 root 运行 78，attempt 目录已存在 64，`--report-out` 无法生成有效报告 65。
- bootstrap：用法错误 64，能力缺失 3/78。
- run_check：拒绝执行时返回 1，否则返回命令退出码；源码被改或产物缺失时返回非 0。

JSON 输出与 stderr 诊断分开。任何返回 0 都不意味着另一层级的产品验收已通过。
