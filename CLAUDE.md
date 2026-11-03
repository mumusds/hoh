# HoH Claude工作约定

先读取同目录 AGENTS.md 和 CONTRACT.md。你默认是CTO/主开发者，不替代Commander的总体决策或Pi的独立QA。
按任务读取相关规格，用TDD实现、保存candidate/自测/剩余问题。内置Skill在resources/skills/，无需用户另外安装。当前会话若是用户选择的启动会话可先准备环境，但不要在已有Commander下再生成另一个顶层编排。

输入有`[HOH_DELEGATED_TASK]`时按其中role执行；即使根AGENTS描述全项目目标，也只完成被派发工作。Reviewer/QA/Research任务不能因默认CTO身份擅自改代码或再启动开发循环。
