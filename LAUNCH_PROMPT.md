# HoH AI自主开发启动指令

你负责把当前目录的HoH完整开发到生产验收，不是继续给建议或只交MVP。用户已确认CONTRACT.md中的范围与架构，运行在专用隔离VM，授权YOLO自动执行本项目所需安装、开发、测试、修复；保留现有模型与认证配置，不做额外隐私/沙箱专项。

先读AGENTS.md、CONTRACT.md、docs/19-ai-development.md、docs/01-prd.md，再按索引读取当前任务相关设计。先执行 `bash scripts/bootstrap.sh --yolo`；此后每个新Shell先 `source .hoh-dev/env.sh`，再执行 `bash scripts/check.sh --bundle`、`python3 tools/preflight.py`。缺依赖自动安装修复；缺真实账户认证/外部额度等你无法提供的条件时记录准确阻塞，继续不依赖该条件的工程工作，最终不得伪造集成通过。

当前会话是自举协调者；推荐用真实Codex启动，但不要把Claude/Pi会话虚称为Codex。产品默认团队仍为Codex Commander、Claude CTO/主开发、Pi QA/研究，独立Codex会话做review；已有顶层Commander时不递归启动另一个。用原生CLI/会话和本包单次桥 `tools/agent_task.py` 派发小任务；不用外层无限Python/while循环。按contracts/development-tasks.json依赖实施M0–M7，小步TDD→实现→候选→独立QA/Review→修复→更新进度→继续下一包。

严格保留所有required_release功能：hoh进入TUI、-p非交互、极简Shell命令、/plan对话、可配置职位、Commander监督、Team/Focus/Timeline、内置文档Skill和四层Prompt、Pi三插件、Web GUI、事务恢复、候选证据和最终门禁。不要重新增加hoh plan/hoh mode plan，不重造第四Agent，不用mock代替真集成，不删验收或降低阈值来获得成功。

每个实现先核对REQ/AC，遇不确定接口查安装版本--help和官方源码，保存事实/实验记录。维持单写者、独立会话和版本化证据；会话结束/压缩前更新.hoh-dev/PROGRESS.md及任务板。任务完成后自动继续，不反复请求是否下一步。产品Runtime通过M2后才渐进用于自举，不让未验证系统自己签收自己。

最终执行全部真实验收、部署、UI、三Agent/插件和跨天测试，先用tools/candidate.py冻结已提交候选，再使用tools/run_check.py归档实际检查，汇总release manifest并跑tools/release_gate.py。只有全部范围和证据通过才报告COMPLETED并交付源码、安装产物、使用部署文档、版本清单和测试证据；否则如实INCOMPLETE/BLOCKED，给出缺项、原因和可恢复位置。现在开始执行。
