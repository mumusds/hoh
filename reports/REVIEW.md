# HoH 开发启动包 1.1：审核与测试报告

## 结论

启动包 1.1（基线 HOH-BASELINE-1.1，CR-001）结构校验通过，包内测试全部通过（数量见 `tests-bundle.json`）。这些是启动包与模拟测试，不是 HoH 产品测试。70 项产品需求仍待实现；真实三 Agent、插件、UI 和跨天验收都没有标记为通过。

## 本次复核发现并修正的问题（相对 1.0.0）

|类别|问题|处理|
|---|---|---|
|规格|70 条 AC 共用同一句 `when`；功能规格的“边界”段复制 70 次；每项只有 1 条 AC；任务的 `done_when` 全部相同|M1/M2 的 27 项细化为 161 条可判定 AC 与场景；M3–M7 标为 outline，由 readiness 门控；生成视图防止漂移|
|门禁|mode 由调用者自报；命令不与测试计划比对；`--cwd` 可指向任意位置；soak 用 `sleep` 加手写时间线即可通过；AC 可引用任意检查；独立报告不绑定原生会话|检查计划驱动 run_check；门禁比对 command/mode/cwd/产物；要求 live 会话清单；AC 必须引用场景指定的检查及其输出；soak 时间线来自 soak 检查并绑定候选；报告绑定原生 attempt|
|基线|状态字段位于受锁文件内；门禁工具本身未锁；改动锁文件不需要 CR|锁定门禁工具、`docs/10` 与全部 schema；任务计划移出锁；`tools/baseline.py` 校验 CR 并重锁|
|工具|`pi install --local` 缺 `--approve`，在 Pi 1.5.0 上导致 bootstrap 失败（已复现）；只有 Claude 拒绝 root；嵌套顶层会话无防护；taskboard 不加锁领取、不限制迁移；校验器会扫描 `node_modules`；候选不可变性检查有竞态；参数检测用子串匹配|全部修复，并有对应单元测试|
|契约|状态枚举在 schema、SQL、文档之间不一致；候选 ID 与 git id 格式不一；SQL 缺 NOT NULL、索引、user_version 及多个实体；OpenAPI 无本地请求完整性保护、无列表端点|统一并补齐（见 CHANGELOG 1.1.0）|
|资源|角色 Prompt 只有一句角色专属内容；Skill 为通用模板|重写为完整的角色契约与阶段流程|

## 独立复核（不参与开发的会话）

开发完成后，由另一个会话只读复核全部改动，并用脚本逐项尝试构造绕过。复核发现以下问题，全部已修复，并各有对应负例测试（`tests/bundle/test_gate.py`、`test_tools.py`、`test_bundle.py`）。

**高**

- `baseline.py relock --amend` 能在不新建 CR 的情况下修改已提交的门槛，门禁也不校验 CR 状态。
- AC 证据可以通过 `..` 路径、嵌套的执行目录或额外引用的检查，从无关检查取得。
- 失败的 plugin-live（DEGRADED）检查能让 required 需求的 AC 以 PASS 通过。

**中**

- QA/Review 报告中的 critical finding 被忽略。
- 报告没有绑定本候选的 live 会话，也没有绑定原生输出的哈希。
- run_check 收到 SIGTERM 时留下子进程且不写记录；候选文件在项目外时，跑完长检查才崩溃。
- validate_bundle 会解析产品自己的 JSON 和 Markdown。

**低**

- 原生 attempt 记录未经 schema 校验。
- soak 时长只取自报时长。
- 门禁接受 root 或手写的执行记录。
- 缺少 date-time 格式校验。
- 检查计划可以不提交到 Git。
- 插件前置条件缺失时没有锁记录。
- SQL 状态枚举比 schema 窄。
- start.sh 的 `--help` 失败时静默退出。

## 本地实际执行

- `bash scripts/check.sh --bundle`：校验器、基线/CR 校验与全部包内测试（本次 216 项，全部通过）。
- `shellcheck -S warning scripts/*.sh`：无告警。
- 运行环境见 `environment-preflight.json`：非 root 用户，Python 3.14；本机有 codex 与 claude，没有 pi、cargo 与 rustc。

## 未执行项

没有调用真实模型；没有编译 Rust 产品；没有进行真实插件联调、产品 PTY/Web E2E、完整 VM 部署或 72 小时运行。CI 配置没有远程执行记录。M3–M7 的需求仍是 outline，按计划在各里程碑开始前细化。

## 门禁保证的范围

门禁能拒绝以下证据：缺失、篡改、错版本、错角色、错模式、与冻结检查计划不一致、证据不来自所引检查、soak 时间线来源不对或过短、非法降级。它不能从形式上证明软件需求全部正确，也不能防止拥有整个 VM 写权限的执行者刻意伪造全部原始材料。测试覆盖仍须由独立 QA/Review 实际审查。

kit 1.0.0 的原始审核记录保存在 `history/kit-1.0.0/`。
