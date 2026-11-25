# CR-039 · run_check 保留继承的 SIGINT 忽略

ID：CR-039
状态：DRAFT
批准人：待主会话依 CR-003 D1 审查批准
日期：2026-11-25
关联：M3LS-200（CR-038 第 3 轮审查的 low，M3 low 包 2），REQ-052（AC-052-04），ADR-027 2026-11-25 更正，CR-038 决定 2

## 问题

CR-038 决定 2 让 `tools/common.py` 的 `install_interrupt_handlers` 在入口为 SIGINT/SIGTERM/SIGHUP 无条件安装记录 handler。无作业控制的后台运行（例如 `sh -c 'run_check … &'`）启动时 SIGINT 已被继承为 SIG_IGN；CR-038 之前 run_check 只替换 SIGTERM/SIGHUP，这种后台检查不受终端 Ctrl-C 影响并继续运行，CR-038 之后它会把 SIGINT 记为中断并结束检查（130 或 127/started=false）。这是 CR-038 带来的行为回退，不是有意的决定。

## 决定

`install_interrupt_handlers` 读到 SIGINT 的当前处置为 SIG_IGN 时保持忽略：不安装记录 handler，也不把 SIGINT 写进返回的记录，因此 `restore_handlers` 不会改动它，执行的命令照旧继承忽略。SIGTERM 与 SIGHUP 不变，即使继承了 SIG_IGN 也照旧安装并记录（与 CR-038 之前的 `install_termination_handlers` 相同）。CR-038 的其余决定不变。

## 受锁文件与实现影响

- `tools/common.py`：`install_interrupt_handlers` 跳过继承为 SIG_IGN 的 SIGINT。

测试：`tests/bundle/test_tools.py` 的 `Signals.test_inherited_sigint_ignore_is_kept` 以继承 SIGINT 忽略的子进程运行：SIGINT 未登记、仍为忽略、收到 SIGINT 后 `execute` 正常结束（退出 0），收到 SIGTERM 仍抛出 Terminated，`restore_handlers` 之后 SIGINT 仍为忽略。修复前该测试失败（SIGINT 被登记并结束执行）。ADR-027 添加 2026-11-25 的更正。

批准后通过 `tools/baseline.py relock --cr docs/cr/CR-039-inherited-sigint-ignore.md` 更新 `contracts/baseline-lock.json`，不手改摘要。委派开发者保留 DRAFT，不重锁；只在 scratch 副本中把本 CR 设为 APPROVED 并重锁，验证批准后 bundle 可以通过，这不代表 CR 已批准。

## 不变的部分

需求、AC、检查计划、发布策略、门禁与阈值都不变；执行记录 schema 与原则一判定不变；不改 CONTRACT 的架构与交互决定。

## 是否属于 CR-003 D1

是：恢复 CR-038 之前对已忽略 SIGINT 的行为，修正一处回退；不缩小范围，不放宽任何检查（SIGTERM/SIGHUP 与非忽略的 SIGINT 仍按 CR-038 记录并结束检查）。

## 验证方式

`python3 -m unittest test_tools.Signals`（在 `tests/bundle`，修复前红、修复后绿）；批准后 `python3 tools/baseline.py relock --cr docs/cr/CR-039-inherited-sigint-ignore.md`，`bash scripts/check.sh --bundle`；`bash scripts/dev/pre-pr.sh --no-deployment`。
