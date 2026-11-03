# AI自主开发执行手册

你现在开发的是HoH，不是开发用户另一个示例产品。阅读顺序：CONTRACT→PRD/功能规格→架构/数据→编排/算法→适配→UX→验收→当前任务关联文档。按需读源码/全文，不一次把全部文档塞满上下文。

## 启动

在已选择的隔离VM、已启动的Codex/Claude/Pi会话中粘贴LAUNCH_PROMPT。该Agent先执行 `bash scripts/bootstrap.sh --yolo`，此后每次新Shell先source `.hoh-dev/env.sh`，再运行包自测和preflight。已有CLI/模型配置保留；缺工具可安装，缺真实认证仅用户能提供时精确记录阻塞。

默认Codex Commander。当前会话若本来就是Codex，不再递归启动另一个顶层Commander。调用Claude做写任务，Pi测试/研究，独立Codex会话review。桥 `tools/agent_task.py` 仅为自举的单次原生CLI调用和日志归档，不是产品Runtime。

## 执行节奏

tools/taskboard.py list/next显示bootstrap工作包。领取一个小任务→引用REQ/AC→先测试→最小实现→局部检查→保存candidate→Pi独立测试→Codex新会话review→汇总证据→任务状态更新。每包完成后继续，不等用户确认下个里程碑。

每次会话结束/压缩前写 `.hoh-dev/PROGRESS.md`：当前任务、分支、commit、测试命令结果、未解issue、下一动作、外部阻塞。不要只留自由聊天记忆。RESUME_PROMPT按事实重建；不能因为上次Agent说完成就相信。

## 自举限制

native CLI可能因服务、token或实现限制终止；本包不假称一个提示词能强迫所有Agent无限持续。利用底座原生长任务/续会话与明确checkpoint；不允许用while true空转追加继续。M2产品Runtime通过测试后逐步迁移受控任务。

允许小范围实现性决策自主选，保留ADR；不重复询问用户已经采纳的架构/交互。若实现细节未知先源码研究/小实验；重大范围改变、身份认证和外部费用授权才是需要升级的事项。

## 不能做的捷径

不能只交MVP/架构文档就称完工；不能mock三Agent冒充集成；不能删Web/三插件/自定义角色/时间线；不能让Developer自己成为唯一验收者；不能把BUNDLE自测当产品测试；不能降低或跳过required gate；不能捏造官方接口、执行结果、用量或模型名称。

## 提交与报告

小批提交，feature branch，commit关联工作包/REQ；有remote可PR/CI，无remote本地候选。每阶段按真正执行结果列PASS/FAIL/NOT_RUN/MOCK/LIVE。最终只有release gate和完整验收都通过才COMPLETED；否则精确INCOMPLETE/BLOCKED与恢复点。

## 子任务防串岗与基线状态

单次桥默认添加HOH_DELEGATED_TASK信封。所有原生Agent读取根AGENTS时，先按该信封进入局部执行；不能因为项目总目标自动接管整个工程。start.sh的初次headless启动才显式加--top-level。指定role不是装饰标签，输出必须保留task/attempt/role身份，QA与Review不得转成开发。

contracts/requirements.json与development-tasks.json中的NOT_IMPLEMENTED/pending是发布启动包时的初始元数据，不是可随意修改的进度数据库。实施进度写.hoh-dev台账/报告；产品上线后写Runtime投影。不要改这些基线状态来获取验收，正式范围修订走CR和新baseline。
