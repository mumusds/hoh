# 测试方案

测试分四类记录：BUNDLE（本启动包工具）、MOCK（确定性模拟Agent）、LIVE（真实CLI/模型/插件）、PRODUCT（HoH实际产物）。结果必须注明类别，任何类别不能冒充另一个。

## 场景、检查与检查计划

每条 AC 恰好对应一个 TEST 场景（`contracts/acceptance-scenarios.json`），场景的 `check_id` 指明哪一项 release 检查必须产出它的原始证据：unit、integration、recovery、tui-pty、web-e2e、adapter-live、plugin-live、deployment、soak、independent-qa、independent-review、traceability、build、static。产品仓库在 `tests/check-plan.json` 为每个 check_id 声明唯一的 argv、mode（product/live）、cwd 与必须产出的文件。只能经 `tools/run_check.py` 执行。检查命令可以内部运行多个测试，但必须把场景 ID（TEST-NNN-MM）写进输出，使 AC 观察能引用具体证据。live 检查须写出 `live-sessions.json`；soak 检查须写出 `timeline.jsonl`。

## 测试金字塔

**测试命名与逐测试证据（CR-003）：** 覆盖 TEST-NNN-MM 的测试，名字里必须含 `test_NNN_MM` 片段（如 `fn test_004_01_exit_code_table`、`#[test] fn test_062_03_outbox_redelivery()`）。检查脚本把每个测试的结果写入 `$HOH_EVIDENCE_DIR/test-results.jsonl`（`contracts/test-result.schema.json`，Rust 由 `scripts/product/test_results.py` 从 libtest 输出转换）；门禁要求每条 AC 在其场景检查里至少有一条自己的 PASS 记录，同一份日志不能替多条 AC 作证。一条 AC 可以由多个测试共同覆盖，任何一个失败该 AC 就不是 PASS。

领域单元：状态合法性、DAG/优先级、预算、revision/epoch、Context装箱、hash匹配、事件投影。属性测试生成随机DAG/重复/乱序/中断。集成：SQLite事务/outbox、worktree、进程树、SSE、HTTP冲突。Adapter：固定真实协议fixture回放+有限真实smoke；UI：TestBackend快照+真实PTY+Playwright。最后跨天全系统。

## 必须的故障矩阵

1. 事务前/后、发命令前/后、收ACK前/后kill -9；恢复不得重复未确认副作用。
2. prompt accepted但没有terminal event；输出EOF/无效JSON/超大帧/JSON字符串含U+2028；stderr大量输出不能堵死。
3. Pi agent_end后retry/compact，最终settled；prompt disposition=handled不等待不存在run。
4. Claude result error、Codex turn interrupted/failed；模型输出done但缺evidence。
5. QA执行中candidate被修改；spec变更；Review旧commit；重复message与旧role_epoch；均拒绝过期结果。
6. GUI断线history/live窗口、慢客户端、cursor过期、多客户端revision冲突。
7. 磁盘满/权限只读/SQLite busy/在线备份/迁移失败；保留可诊断状态而不是半提交。
8. 外部服务429/503/auth失效；不同故障不盲重试；预算重启后不清零。
9. 终端resize/中文/emoji/paste/键位冲突/panic/SIGTERM；进程树无孤儿，终端恢复。
10. 三Pi插件一起运行、无完整TUI、压缩前flush、RPC UI request、内存并发、todo恢复。

## 真实能力测试

三个CLI分别完成“读取固定文件→编辑专用测试文件→运行单元测试→提交结构化报告”，记录版本/模型/session。QA/Review使用不同session，故意植入缺陷检测不全通过。工具能力必须执行，不凭agent名推断电脑操作。

插件测试在固定组合和相同Node/Pi版本执行：单插件、双插件、三插件；压缩恢复、进程崩溃、reload；逐项记录能否配置/运行/恢复。组合不兼容可临时degraded继续开发，但v1完整验收不得当作PASS。

## 长任务评测

固定三个场景：新建CLI工具、已有Web仓库增量、跨模块Bug修复。对照：单Agent多轮 vs HoH同预算；至少3个独立seed/初始目录作为小样本趋势，不能声称统计普遍最优。记录功能正确性、回归捕获、错误验收、总资源和人工干预。

72h soak使用真实Agent工程轮次+真实host持续运行；至少20个有效candidate循环；可混入模拟故障但明确标记。计时由启动/结束原始记录和持久心跳支撑，不能sleep几秒伪造72h字段；不能只使用模拟API而称live。

## Flaky与截图

首失败重试通过标FLAKY；保留所有attempt与重试策略 [S26]。截图证明渲染，不证明所有功能；Browser测试先DOM/可达性交互断言再截图，终端用PTY协议与终端buffer快照，OCR非默认。

## 本包自测

`python3 tools/validate_bundle.py`：文档链接、契约、AC/场景/检查映射、profile 覆盖、任务 readiness、生成视图与资源。`python3 tools/baseline.py verify`：基线与 CR。`python3 -m unittest discover -s tests/bundle -v`：参考调度、SQLite、不变量、进程桥、检查计划执行与门禁负例。`bash scripts/check.sh --bundle` 汇总以上全部。该组不需要模型账户，不表示 HoH 已开发。

## CI策略

每次PR离线core/adapter fixture/UI/API；live测试在有认证的受控VM且主动开启，不在fork PR泄漏配置。CI未配置必须明确not_run。检查不能被AI删除、skip或改成echo PASS以绿灯。发布需要独立QA和review记录，而不是单一开发会话自签。
