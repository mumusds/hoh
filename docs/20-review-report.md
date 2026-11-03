# 方案复核与验证状态

详细实际结果见[审核报告](../reports/REVIEW.md)；kit 1.0.0 的原始审核记录保存在 `reports/history/kit-1.0.0/`。本包包含可执行检查，不把概念文档视为已开发产品。

## kit 1.1 复核（CR-001）

对 1.0.0 的复核发现：AC 模板化不可判定、门禁可被自报 mode/手写时间线绕过、插件安装缺 `--approve` 导致 bootstrap 失败、任务台账不执行单写者、schema 状态枚举互相矛盾、SQL 缺约束与索引、本地 Web API 无请求完整性保护。这些已在 1.1 修正，产品范围不变；具体见 [CR-001](cr/CR-001-kit-hardening.md) 与 CHANGELOG。M3–M7 需求仍为 outline，按滚动细化在各里程碑开始前补齐，这是有意的计划而非遗漏。

## 架构复核结果

保留用户最终采纳的CLI/TUI边界、Plan对话、双层编排、可配置团队、Rust基座、三插件与Web范围。删去早期提议但后来否定的hoh plan/mode/run子命令；不再将全部TUI功能镜像成Shell命令。

关键闭环已写成机器契约：需求/AC/TEST、DAG、事件/outbox/receipt、revision/role_epoch、候选/环境/测试计划、独立QA/Review与发布条件。可执行参考和负例测试用于约束AI实现，不代替Rust产品代码。

## 仍须在实施阶段关闭的项目

真实版本的Native事件与会话能力；Pi三插件共同生命周期；Web/TUI交互性能；Host断电与恢复；完整构建/部署/升级与跨天模型运行。所有这些都留在required_release，不能以“后续增强”跳过。

## 资料可信度

调研依据区分来源事实、作者实验声明、HoH设计建议和未实测事项；没有将用户文章的全部宣传数字移植为本系统性能承诺。全文见[研究来源](../research/SOURCES.md)和[研究边界](../research/README.md)。
