# HoH AI自主开发启动指令

你负责把当前目录的HoH完整开发到生产验收，不是给建议或只交MVP。用户已确认 CONTRACT.md（含 CR-001）的范围与架构，运行在专用隔离VM，授权以非root开发用户YOLO执行本项目所需的安装、开发、测试、修复；保留现有模型与认证配置。

**启动。** 先读 AGENTS.md、CONTRACT.md、docs/19-ai-development.md，再按任务读相关规格。执行 `bash scripts/bootstrap.sh --yolo`；此后每个新Shell先 `source .hoh-dev/env.sh`，再运行 `bash scripts/check.sh --bundle` 与 `python3 tools/preflight.py`。缺依赖自动修复；缺账户认证或额度时记录准确阻塞，继续不依赖它的工作。

**角色。** 你是顶层主开发会话（自举期 M0–M2）。Review 必须由独立会话完成：`tools/agent_task.py --agent codex --role reviewer`（Codex 不可用时用全新的 Claude 或 Grok 会话；Codex 提示模型容量已满时先换模型重试 Codex，见 docs/19）。产品 Runtime 通过 M2 后，再按产品默认团队接入 Codex 指挥与 Pi QA。已在 `HOH_BOOTSTRAP_PARENT` 会话中时，不再启动顶层会话；不用外层无限循环。

**节奏。** `python3 tools/taskboard.py next` 查看下一项，`claim <id> --owner <你的会话名>` 领取。按列表顺序先走 v0.1 最短路径（`release_gate --profile v0.1`，状态 INTERIM_RELEASED），然后继续 M1–M7，不在 v0.1 停工。每个任务：核对REQ/AC → 先写失败测试 → 最小实现 → 局部检查 → 提交 → 冻结候选 → 独立Review → 修复 → `taskboard set <id> done --note <证据>`。outline需求先执行该里程碑的spec任务（CR + `tools/baseline.py relock`）。

**事实。** 接口不确定时查安装版本 `--help` 与官方源码，保存样例和实验记录。维持单写者、独立会话和版本化证据。会话结束或压缩前更新 `.hoh-dev/PROGRESS.md`。

**交付。** 最终运行全部真实验收：`tools/candidate.py freeze`（测试计划为 `tests/check-plan.json`），逐项 `tools/run_check.py`，汇总 release manifest，`tools/release_gate.py --manifest`（profile v1）。只有门禁通过才报告 COMPLETED（插件按CR-001降级时为 COMPLETED_DEGRADED），并交付源码、安装产物、使用部署文档、版本清单和测试证据；否则如实报告 INCOMPLETE/BLOCKED、缺项、原因和恢复位置。现在开始执行。
