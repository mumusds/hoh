# 从这里开始

## 1. 解压位置与前置条件

在专用Linux VM中进入 `hoh/`。建议Ubuntu 24.04或具有Python3.11+的兼容环境；使用**普通开发用户**，需要安装系统依赖时该用户应有可用的`sudo -n`。所有脚本与Agent桥都拒绝以root运行（原生Agent使用bypass参数）。

正常初始化需要可访问官方发行源、npm与模型服务。Codex、Claude和Pi的账号认证或自定义API配置由你已有的环境提供。脚本保留已有工具和默认模型，不替你生成账号、密钥或额度。

## 2. 最简单的启动方法

在当前目录打开你已经登录的 Claude Code（自举期主开发会话；也可用 Codex/Pi），以其原生YOLO选项运行；然后粘贴：

```text
读取AGENTS.md、CONTRACT.md和LAUNCH_PROMPT.md，执行其中的HoH完整自主开发任务。
这是专用隔离VM，授权YOLO安装、开发、测试和修复。保持已确认架构与全部功能范围。
先自检启动包及本机真实接口，再按M0–M7依赖持续实现；不只交MVP，不伪造验收。
缺账号或外部服务时记录准确阻塞，继续可做的任务；最终用真实证据判断是否完成。
现在开始。
```

没有必要先手动运行一长串安装命令。启动指令已要求AI执行 `scripts/bootstrap.sh --yolo`，并在每次新Shell中source `.hoh-dev/env.sh`。

自举期（M0–M2）只需一个主开发会话加独立 Review 会话（`tools/agent_task.py --agent codex --role reviewer`）；Codex 指挥与 Pi QA 在产品 Runtime 可用后接入（CR-001）。**已有顶层会话（`HOH_BOOTSTRAP_PARENT` 已设置）时不得再启动另一个。**

## 3. 可选的脚本入口

已经装好原生Agent时，`bash scripts/start.sh`（默认 claude，可选 codex/pi）会调用原生交互CLI并传入完整启动指令；`--headless`则只启动一次原生非交互任务，记录日志，不额外套无限循环。

先检查命令、不执行模型：

```bash
bash scripts/start.sh --dry-run
bash scripts/start.sh --headless --dry-run
```

环境本身可以先检查：`bash scripts/bootstrap.sh --yolo --dry-run`。实际准备使用同一命令去掉`--dry-run`。`--skip-install`只做离线目录准备，不意味着环境已就绪。

## 4. 运行时看哪里

`.hoh-dev/PROGRESS.md`：当前任务与恢复点。  
`.hoh-dev/task-state.json`：自举工作包状态（`tools/taskboard.py list`），不是最终产品数据库。  
`.hoh-dev/attempts/`：原生子任务Prompt、stdout、stderr与真实结束状态。  
`.hoh-dev/preflight.json`：工具与参数探测；帮助输出不能证明认证成功。  
`contracts/requirements.json`：70项最终要求；不能删掉难做的项。outline 需求在其里程碑的 spec 任务中细化。

## 5. 中断以后

重新打开同一项目，用 `RESUME_PROMPT.md`。根据文件、Git、台账和证据续做，不要求重新讨论产品架构。原生CLI仍可能受到服务、额度和会话限制；此包不承诺一个Prompt能强制它永久不断运行。

## 6. 什么才算完成

包自测通过 ≠ HoH产品完成；v0.1 的 `INTERIM_RELEASED` 也不是完成。M0–M7全部实现，真实三Agent、TUI/Web、部署恢复、72小时运行和全部AC有证据，且 `release_gate.py` 对 v1 profile 通过，才允许`COMPLETED`（Pi 插件按 CR-001 降级时为 `COMPLETED_DEGRADED`）。否则报告`INCOMPLETE/BLOCKED`和下一恢复动作。
