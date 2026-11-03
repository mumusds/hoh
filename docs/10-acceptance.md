# 验收规范与完成条件

## 两个不同的完成

启动包完成：文档、契约、脚本与包测试有效，可供AI实施。产品完成：HoH实现全部required_release需求且运行真实验收。二者必须分别报告。

## 产品Definition of Done

每条REQ有AC与TEST映射，每个required AC有当前spec/candidate相关的真实执行证据；所有release-policy.required_checks完成；critical/high问题为零；其他未解决问题列明但不得破坏required AC；三Agent与三插件真实联调通过；TUI/Web/新VM安装与恢复实际通过；72h soak达到门槛。

最终交付：完整源代码、Cargo.lock/package-lock/rust-toolchain、可执行Linux发布包、Web资产、资源包、使用部署升级回滚文档、SBOM/第三方notice、测试原始记录、qa/review报告、证据清单、release hash。

## Gate判定

PASS仅用于有数据的检查；FAIL、BLOCKED、INVALID、UNKNOWN、FLAKY、NOT_RUN、SKIPPED不得满足required。空检查列表不允许all([])=true；缺模型认证不允许用mock结果顶替live。成功退出码只是进程事实，不是语义完成。

Evidence至少包含check_id、candidate/spec摘要、execution record路径/sha256、stdout/stderr或测试报告、开始/结束、exit code、验证方式、producer attempt和结果。模型生成摘要可帮助解释，不能自造运行记录。最终清单由QA汇总、独立Reviewer复核。

## 验收工具

`tools/candidate.py`绑定已提交源码/环境/测试计划；`tools/run_check.py`在检查前后核对该绑定，为真实子进程检查写原始记录；`tools/release_gate.py --manifest <file>` 检查映射、hash、版本、required检查、live/soak条件。它是清单一致性门禁，不是软件正确性的数学证明；即使记录形式合法，也要审核测试是否真正覆盖需求，禁止用echo成功模拟测试。

原始baseline摘要在 `contracts/baseline-lock.json`；变更需要CR记录，不可直接改manifest跳过项。此为工程约束而非对拥有VM完全写权的恶意执行者的密码学防护。

## 发布结果

COMPLETED：全部通过且最终产物身份一致。
BLOCKED：缺关键外部前置，列出精确缺项与可执行恢复动作。
INCOMPLETE：预算/时间耗尽或必要检查未通过，报告已完成/未完成/风险与续跑点。
FAILED：不可恢复运行失败/数据完整性异常，保存证据，不能包装为部分成功发布。

存在暂存的失败分支是允许的；不得删除失败记录伪造零回归。发布后发现缺陷是正常维护输入，说明本门禁覆盖范围而不声称绝对无缺陷。
