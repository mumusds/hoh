---
name: hoh-release
description: 完整交付——按发布profile冻结最终spec与candidate，逐项核对REQ/AC、必需检查、独立报告与交付物，运行release_gate并原样采用其结论（COMPLETED/COMPLETED_DEGRADED/INTERIM_RELEASED或BLOCKED/INCOMPLETE/FAILED）。
---
# 完整交付

## 何时使用
- release_request事件、里程碑交付或最终交付。
- 分工：Commander汇总并提交请求；QA汇总证据清单；Reviewer复核清单与报告。任何角色都不能覆盖release_gate的结论。

## 输入
- `contracts/release-policy.json`（字段以当前版本为准）：各检查的mode与是否degradable、发布profile（如v1、v0.1）及其required_checks与是否要求soak、soak门槛、零容忍严重度、各profile通过后的发布状态。
- `contracts/requirements.json` 中属于所选profile的REQ/AC（按release_targets筛选），以及每项的release_class（required/degradable）。
- 最终spec revision与candidate manifest；冻结的检查计划；各检查执行记录、观察记录、QA与Reviewer独立报告及其原生attempt记录；open issue列表；Agent与插件版本锁。

## 步骤
1. 确定profile：完整交付用v1；中间版本用v0.1，其通过状态是INTERIM_RELEASED，永远不是COMPLETED。
2. 冻结：最终spec与candidate，记录spec_sha256、candidate_id、requirements_sha256。冻结后任何合并、变基或重构建导致hash变化→回到本步重新绑定并重验。
3. 追溯：对profile内每个REQ→AC→TEST，找到当前candidate/spec上、由该AC引用检查产出的有效证据（hash一致、producer真实、执行前后源码未变）；旧候选上的证据只算历史。
4. 必需检查逐项核对（profile的required_checks；mode以release-policy为准，执行记录须与检查计划argv一致）：
   - [ ] build、static、unit、integration
   - [ ] adapter-live（live，附live会话记录）；plugin-live（live；仅在标记degradable时可为带诊断的DEGRADED）
   - [ ] tui-pty、web-e2e
   - [ ] recovery；deployment（在干净VM上实际执行）
   - [ ] soak（profile要求时）：live，时长≥minimum_soak_seconds，有效迭代≥minimum_live_iterations，时间线由soak检查自身产出并绑定被测候选
   - [ ] independent-qa、independent-review：同一candidate、两个不同的非开发会话，报告status=PASS且引用已完成的原生attempt
   - [ ] traceability
5. 问题：critical/high open issue必须为零；其余未解决项逐条列出，并说明不破坏任何required AC。DEGRADED只出现在degradable项上，且每项有诊断与上游引用。
6. 交付物：源码、锁文件、Linux发布包、Web资产、资源包、部署/用户/升级/回滚文档（命令已实测）、SBOM与第三方notice、原始测试记录、QA/Review报告、证据清单、release hash。
7. 生成发布清单（`contracts/release.schema.json`），运行release_gate（自举期 `python3 tools/release_gate.py --manifest <清单路径>`，参数以 `--help` 为准）。
8. 原样采用结论：
   - 门禁通过：按gate输出的release_status——COMPLETED（v1全部PASS）、COMPLETED_DEGRADED（v1含合规DEGRADED）、INTERIM_RELEASED（v0.1）
   - BLOCKED：缺关键外部前置，列出精确缺项与可执行的恢复动作
   - INCOMPLETE：必要检查未通过或预算/时间耗尽，列出已完成、未完成、风险与续跑点
   - FAILED：不可恢复的运行失败或数据完整性异常，保存证据，不包装为部分成功
9. 写changelog与里程碑报告；发布后发现的缺陷进入维护输入，不改写本次结论。

## 产出物与格式
- 发布清单（`contracts/release.schema.json`）与release_gate原始输出。
- `resources/templates/` 中的changelog、milestone-report、deployment、user-guide。
- TaskResult：门禁通过时status=COMPLETED，summary原样写出release_status（不得把COMPLETED_DEGRADED或INTERIM_RELEASED说成COMPLETED）；未通过时status为BLOCKED/INCOMPLETE/FAILED；evidence引用清单、gate输出与各报告；issues列全部缺项、降级项与未解决问题。

## 完成判据
- release_gate已在最终冻结身份上运行，TaskResult状态与summary和其结论一致。
- 每个缺项都有精确描述与恢复动作；没有任何required检查以NOT_RUN、SKIPPED、BLOCKED、UNKNOWN或mock结果计为通过。

## 常见错误
- 降低门槛、修改baseline、删除required检查或把required需求改成degradable来变绿。
- 用mock结果顶替live，或用短时运行填写soak字段。
- 用PR头的旧证据证明合并后的commit。
- 把空检查列表当作全部通过；存在critical/high问题仍报告COMPLETED。
- 删除失败记录以伪造零回归。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
