# 使用手册（目标行为规范）

本手册描述AI完成后的HoH行为，不代表本启动包已提供hoh程序。开发启动见 START_HERE.md。

## 第一次打开

在项目目录执行 `hoh`。若缺配置，TUI引导检测Codex/Claude/Pi、模型可用性与项目目录，不要求用户先记住init命令。已有文档自动发现；不存在时内置规格模板可用。

默认Auto；复杂产品先用 `/plan` 讨论目标，或Shift+Tab切换。Plan完成后在同会话选择开始实施。工作模式和权限模式不同，用户无需理解底座CLI的权限枚举。

## 常用操作

`hoh -p "依据docs/PRD.md完成项目"`：前台非交互；`--output json`最终机器结果，`--output stream-json`实时事件。`--detach`先输出run_id和提交确认，任务后台继续。`hoh attach <id>`打开该运行。`hoh web`连接相同host。`hoh doctor`看环境能力。

不存在hoh plan/hoh mode plan/hoh run子命令。Shell帮助保持短；TUI输入/可搜索命令，低频设置从面板进入。

## 观察团队

Workspace看统一进展与Commander决策；Team按需分屏；选中Agent进入Focus；Timeline按项目/轮次/角色筛选。点击任务、diff、测试或证据查看来源。长日志默认折叠，不挤占对话。

“已验收18/25”只统计当前规格有效验收。新需求导致分母变化会显示规格版本；回归使已验收变少但保留历史。模型/上下文属于选中Agent，不显示虚假的全团队上下文百分比。

## 与团队交流

默认问Commander。@cto、@qa或@developer定向咨询；品牌名有多个职位会显示实际路由。问进度不会打断编码。优先级调整在下个checkpoint生效；需求变更有CR和影响分析。暂停/取消通过控制面，不等模型同意。

## 团队、模型、Prompt与Skill

/team选择默认职位与Agent/模型，也可新增自定义职位。角色切换提示什么时候生效。设置页可查看默认Prompt、创建覆盖、diff、恢复默认；运行中的attempt用锁定版本。项目内置Skill和文档模板不要求另装Matt工具；Pi原生插件由HoH引导自动准备。

## 退出与恢复

关闭窗口/断开SSH不取消任务；明确选择cancel才终止工作。暂停表示待安全点停止新派发，界面显示PAUSE_REQUESTED直至PAUSED。`/resume`打开历史选择器；非交互通过--resume指定run_id，不能无人值守随意选最近run。

## 异常解释

BLOCKED_AUTH：需要原生登录；BLOCKED_CAPABILITY：当前版本不支持所需接口（对应错误码 CAPABILITY_UNSUPPORTED，API 返回 422）；WAITING_EXTERNAL：已保存next_retry；INVALID_EVIDENCE：版本或文件已变；INCOMPLETE：剩余需求/预算不满足。状态不是“AI坏了”的泛化报错；每项提供诊断和恢复动作。

## 最终报告

包含产物版本、发布状态（COMPLETED / COMPLETED_DEGRADED 及降级清单 / INTERIM_RELEASED / INCOMPLETE / BLOCKED）、已验收范围、未解决事项、测试真实/模拟/未运行分类、费用实际/估算/未知、部署与回滚步骤。不能只给一句完成。模型退出与HoH完成分别显示。
