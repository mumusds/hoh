---
name: hoh-independent-qa
description: 独立质量验收——QA在独立会话中针对一个冻结candidate id，先依据spec/AC设计检查再真实执行，逐AC分类并产出独立报告；绝不修改被测源码。
---
# 独立质量验收

## 何时使用
- candidate_ready事件（与code-review并行）；发布前全量验收；spec变更后重验。
- 每次只针对一个冻结candidate id；候选变化即为新任务，旧结论不得沿用。

## 输入（按顺序读取）
1. candidate id及commit/tree/spec/environment/test_plan摘要、acceptance_ids、范围与预算。
2. spec、AC、test plan、UX规格。
3. 运行环境说明（工具版本、认证可用性）。
4. 最后：开发者自测与历史issue——仅作诊断线索。

## 步骤
1. 开新会话并记录session_id；不得复用开发会话。
2. 校验候选：Runtime候选校验（自举期 `tools/candidate.py verify <manifest>`）；不一致即INVALID，停止并报告。
3. 准备与写目录分离的运行目录；证据写入被忽略的证据目录，绝不在源码树内写文件。
4. 设计检查（在阅读开发者材料之前完成），每个AC至少覆盖：
   - [ ] 黑盒场景：按Given建立前置、按When操作、按Then写出可判定的观察
   - [ ] 边界与错误输入
   - [ ] 取消/超时/重试/恢复路径（适用时）
   - [ ] 白盒补充：读代码找高风险分支与并发点，补针对性检查
   - [ ] 适用的故障注入（kill -9时序、重复/乱序消息、断线、磁盘满等）
5. 执行：只运行冻结检查计划（`contracts/check-plan.schema.json`）中的检查，经run_check类工具在冻结候选上执行并生成执行记录；argv、mode、cwd由计划决定，不自报。live检查须产出其会话记录（如live-sessions.json）。TUI用真实PTY，Web先DOM/可达性断言后截图（详见ux-validation）。若AC缺少可用检查，记为NOT_RUN并开Issue，不临时改计划。
6. 每次执行后：确认source_unchanged=true；退出码只是进程事实，必须检查输出是否满足Then。
7. 分类每个AC：
   - PASS：Then被观察满足，证据身份与当前candidate/spec一致
   - FAIL：观察与Then不符
   - DEGRADED：仅限release_class=degradable的需求；须有绑定执行记录、诊断工件与上游引用（第三方限制的issue/版本/commit）
   - BLOCKED：外部前提缺失（认证、工具、环境），写精确缺项
   - UNKNOWN：无法判定（规格含糊或观察不足），向CTO/Commander发query
   - NOT_RUN：未执行（预算不足、依赖阻塞）
   - INVALID：候选在测试中被修改或身份不符
   - FLAKY：首次失败、重试通过；保留首次失败，不计PASS
8. FAIL与FLAKY写Issue：severity、ac_id、最小复现步骤、期望、观察、证据路径、fingerprint。
9. 再读开发者自测：与你结论不一致之处写入findings（例：开发者称PASS而你观察到FAIL）。
10. 写观察记录与独立报告，提交TaskResult。需要修复时只开Issue，不等待开发者修改当前候选。

## 产出物与格式
- 每个AC一条 `contracts/acceptance-observation.schema.json` 记录：status取schema枚举（PASS/DEGRADED/FAIL/BLOCKED/FLAKY/INVALID/UNKNOWN/NOT_RUN）；BLOCKED在observed写明精确缺项，FLAKY保留首次失败与重试记录；DEGRADED附degradation{reason, upstream_ref, diagnostic}；raw_evidence引用原始输出（未执行时引用阻塞诊断）。
- 执行记录：`contracts/execution.schema.json`，由工具生成，不手写。
- 独立报告：`contracts/independent-report.schema.json`（字段以schema当前版本为准），role="qa"，含native_attempt（产生本报告的原生attempt记录，由Runtime/桥提供，不得伪造）。status：PASS=scope内每个AC为PASS或合规DEGRADED；FAIL=任一FAIL/INVALID；UNKNOWN=无FAIL但有UNKNOWN/NOT_RUN。findings每条{id, severity, summary, evidence}，summary以“AC-NNN-MM 状态：”开头，severity按对required AC的影响取值。
- 汇总文档可沿用 `resources/templates/acceptance.md` 与 `test-plan.md` 的结构。
- TaskResult：COMPLETED=报告完整产出（无论候选PASS或FAIL）；BLOCKED=候选无效或前提缺失；INCOMPLETE=预算耗尽并列出NOT_RUN项。

## 完成判据
- scope内每个AC都有上述状态之一，以及证据或缺项说明。
- 报告、观察与执行记录绑定同一candidate_id与spec_sha256，raw_session已保存。
- 被测源码零修改（所有执行记录的source_unchanged均为true）。

## 常见错误
- 在候选checkout里修bug、改测试或格式化代码。
- 先读开发者的“全部通过”再设计检查，形成确认偏差。
- 把mock当live、把退出码0当PASS、把空检查列表当全部通过。
- 重试直到通过并丢弃首次失败。
- 测的是另一个commit或已失效的spec revision。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
