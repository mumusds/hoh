# Prompt、内置Skill、模板与Pi记忆

## 四层构成

Role Contract固定职责和输出Schema；Project Context包含规格/ADR/约定；Task Context包含目标/范围/候选/AC；Evidence Context为不可改写的事实引用。全局/项目/run覆盖合成HoH可控prompt，不宣称能替换所有底座原生system prompt。

每个attempt记录prompt text hash、resource bundle版本、输入引用列表和截断策略。按需加载技能正文；系统只附短索引。必需信息装不下则报错，不能裁掉验收或当前spec身份。

## 内置资源

`resources/skills/` 为本包原创HoH工程技能，完整文件随产品分发。含需求澄清、决策地图、领域建模、设计、任务拆解、TDD、实现、QA、review、恢复、发布。参考原作方法但不冒充Matt原版，避免原Skill依赖未安装工具 [S18,S19]。

上游grill-with-docs仅组合两个Skill；原Wayfinder真人讨论不能由Agent自问自答当用户批准。因此本包Plan_skill明确interaction=interactive/headless：前者真实问答，后者使用既有授权默认与assumption ledger，关键未知block。

`resources/templates/`包括Lite/Standard/Full manifest，按项目等级引用规范模板。部署/用户手册早期写待验证设计，完成时对真实命令更新，不能把设想留成假文档。

## Override与升级

builtin→global→project→run，每层保留来源和diff。用户编辑复制到override路径，不直接改安装包文件。新的resource revision只给新attempt；运行中变更待checkpoint。Schema验证+Prompt契约测试验证输出形状，不把测试通过等同模型一定始终服从。

## 三插件策略

pi-smart-compact选alpertarhan Pi Continuity [S14]：上下文清理/verified compaction；可选长期记忆后端避免与Hermes重复。配置键以安装版本实际schema为准，不能照抄不确定key。

pi-hermes-memory [S15]：项目/跨会话经验；policy-only召回可能漏，硬规格永远在HoH事实层。经验有source、project、confidence、validity条件，召回命中不得直接把Issue关闭。

@juicesharp/rpiv-todo [S16,S17]：Pi局部子步骤，可映射只读展示；completed local todo不更新全局需求accepted。不同会话不天然共享任务。默认不安装整个rpiv pipeline。

## 兼容组合必须实测

版本/来源/依赖锁定，单插件→两插件→三插件逐级；压缩前Hermes flush与Compact hook顺序、重入保护、模型调用次数、会话恢复、并发DB维护都需测。RPC不能调用完整Pi custom TUI [S09]。HoH自己的TUI提供设置表单，实际改动通过经过核实的插件配置adapter。

安装失败可让不依赖插件的任务继续，但状态必须degraded且最终完整门禁阻塞；不强制用户手工安装，不偷偷把插件删出v1。升级在维护窗口，原组合锁定到run。

## Skill注册最小字段

id/version/name/description/roles/phases/interaction/entry/dependencies/output_contract/license。依赖闭包可解析且无环，entry存在；unknown phase拒绝。自定义Skill可以增加能力，不自动添加全局调度器或绕过质量门禁。
