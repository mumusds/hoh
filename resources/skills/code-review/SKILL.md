---
name: hoh-code-review
description: 独立代码审查——Reviewer在与开发分离的新会话中审查一个冻结candidate id的diff，覆盖正确性、可靠性与测试真实性，输出带位置和严重度的发现；绝不修改源码。
---
# 独立代码审查

## 何时使用
- candidate_ready事件（与independent-qa并行）；发布前复核；被派发独立规格评审时审查规格文档。
- 每次只针对一个冻结candidate id；新候选是新任务。

## 输入
- candidate id与基线commit；diff范围与改动文件列表。
- 任务的REQ/AC、spec/ADR、约定与不变量。
- 测试与执行记录；开发者总结最后读，仅作线索。

## 步骤
1. 开新会话并记录session_id；校验候选身份；在只读checkout取“基线..候选”diff。
2. 范围映射：每个改动文件对应哪个任务/AC；无关改动与未覆盖AC各记一条发现。
3. 逐文件审查清单：
   - [ ] 正确性：逻辑、边界值、错误分支与AC的Then一致
   - [ ] 状态与幂等：重复/乱序消息、重放、事件处理是否纯函数
   - [ ] 并发：锁顺序、租约/epoch fencing、expected_revision CAS、竞态窗口
   - [ ] 取消与超时：进程组信号并wait、无孤儿进程、超时不等于已取消
   - [ ] 恢复：两步之间崩溃、未知外部效果是否先核对再重试
   - [ ] 错误处理：无吞错、无可达panic路径、用户可见错误清晰
   - [ ] 资源：子进程、文件句柄、临时文件、锁的释放
   - [ ] 兼容：配置与数据迁移、旧版本数据、回滚
   - [ ] UI：loading/empty/error/stale路径，宽字符显示宽度
   - [ ] 日志：不打印凭据，诊断信息足够
4. 测试真实性：
   - [ ] 新行为有测试，且断言可观察结果
   - [ ] 有红灯证据，或能说明测试确实会在缺陷存在时失败
   - [ ] 没有弱化或删除既有断言，没有把期望改成错误行为
   - [ ] skip/ignore有原因；mock与live标注正确
5. 需要确认的怀疑：在独立只读checkout运行测试或最小实验，保存执行记录；无法确认的记为疑问。
6. 写发现：定位（路径:行）、问题、影响、证据、修复方向（文字描述，不附可直接应用的补丁）。
7. 生成独立报告与TaskResult。

## 严重度与类别
- critical：数据丢失或完整性破坏、安全边界、required AC不可用、伪造证据或测试。
- high：常见路径错误、恢复失败、并发竞态、测试无法检出目标缺陷。
- medium：边界路径缺陷、误导性错误信息。
- low：可维护性、命名、重复。
- 类别：缺陷 / 风险 / 建议 / 疑问；只有确定的critical/high缺陷导致FAIL，疑问须写明还需要什么证据。

## 产出物与格式
- `contracts/independent-report.schema.json`（字段以schema当前版本为准），role="reviewer"；scope=已审文件与AC；native_attempt=产生本报告的原生attempt记录（由Runtime/桥提供，不得伪造）。
- status：PASS=scope全部审完且无critical/high缺陷；FAIL=存在critical/high缺陷；UNKNOWN=有未审范围，或可能为critical/high的疑问尚未证实。
- findings每条{id, severity, summary, evidence}：summary写“[类别] 路径:行 — 问题；影响；修复方向”，evidence引用支撑工件。
- TaskResult：issues每条含severity、kind、location、evidence、fingerprint；有未审范围时写入summary，status为INCOMPLETE。

## 完成判据
- 所有改动文件都已审完或被明确列为未审；每条缺陷都有定位与证据。
- 报告绑定candidate_id与spec_sha256，raw_session已保存；源码零修改。

## 常见错误
- 直接修改代码或提交补丁“顺手修好”。
- 因作者或QA说通过而批准。
- 发现模糊（“错误处理需加强”），没有位置与证据。
- 把风格偏好当阻塞缺陷，或把疑问升格为缺陷。
- 审查了错误的commit或旧diff。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
