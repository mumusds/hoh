# 文档规范与事实治理

## 单一事实源

CONTRACT是已确定约束，requirements.json是正式需求/AC主表，详细文档解释行为，traceability.md是生成视图。实现状态保存在开发任务板/运行DB，不改需求主表成一份易漂移的完成记录。Git中的项目规格语义权威，SQLite仅索引与执行状态。

## 文档元数据

每份正式规格记录id/title/revision/status/owners/related_requirements/source_refs；草稿、已审、过期、已实现行为区分。模板占位符应在成为ready前填实；本包resource模板本来是占位，不算产品实现缺陷。

## 变更流程

需求变更→CR描述原因/影响→Commander结合授权范围决定→新revision/AC更新→任务影响闭包→旧证据current失效→重验。已接受用户范围不可自动删；必要调整需显式用户批准或预先授权规则，决策留痕。

## 基线变更的机械步骤

1. 写 `docs/cr/CR-00N-<主题>.md`，包含 `ID：CR-00N`、`状态：`、`批准人：` 三行，以及背景、决定、影响、不变内容和验证方式。只有用户批准或预先授权的规则才能把状态写成 APPROVED。
2. 修改受锁文件（CONTRACT、需求、场景、policy、schema、`docs/10-acceptance.md`、门禁工具），运行 `python3 tools/gen_spec_views.py`。
3. 运行 `python3 tools/baseline.py relock --cr docs/cr/CR-00N-….md [--baseline-id HOH-BASELINE-x.y]`。新锁记录上一锁的 hash 与 CR 列表；CR 尚未提交时用 `--amend` 重算。
4. 运行 `bash scripts/check.sh --bundle` 并提交。Git 历史与 CR 记录共同构成审计链。

## AI可编辑边界

AI可以完善测试、代码、接口细节、文档措辞；不得为了让检查变绿修改验收门槛、baseline hash或测试结论。合法基线变更更新CONTRACT/requirements/acceptance一致后记录ADR/CHANGELOG并重新锁定。包的校验hash防意外漂移，不宣称能防拥有全部文件写权限的恶意改写。

## 知识与证据

事实证据append-only，可追加correction而不覆盖；知识可合并整理但必须保留引用/适用条件。未知是unknown；设计决定是decision；模型判断是assessment；执行观察是observation。四者不能混写成一个“记忆”字段。

## 生成文档质量

接口示例可由Schema生成，CLI手册从真实--help校验；部署命令在干净VM执行；用户指南来自实际流程而非早期想象。所有链接可检查，新增REQ必须加入AC和测试映射。版本变更说明哪些事实已验证、哪些仍是假设。
