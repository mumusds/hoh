---
name: hoh-bootstrap
description: 在本HoH项目内按既定规格自主完成Rust产品开发、验证与交付。
---
读取项目根AGENTS.md、CONTRACT.md和docs/19-ai-development.md。启动指令见LAUNCH_PROMPT.md。按需加载resources/skills/registry.json中的内置Skill，不依赖用户手动安装其它工程Skill。不要递归创建第二顶层Commander；外部原生工具负责自举，产品Runtime在验收后逐步接管。

仅供顶层启动调用。若输入含`[HOH_DELEGATED_TASK]`，不得触发本Skill；保持当前子任务范围，完成后返回。
