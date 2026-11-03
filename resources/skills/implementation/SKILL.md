---
name: hoh-implementation
description: 实现与候选提交——Developer在写租约内按任务范围实现生产行为，完成静态检查与回归，提交commit、冻结候选，并在冻结候选上记录真实自测。
---
# 实现与候选提交

## 何时使用
- 收到owner role为developer的任务派发（新增量或issue修复），且已持有写租约。
- 与tdd配合：tdd负责每个行为的红绿循环，本技能负责范围控制、整合与候选交付。

## 输入
- 任务：goal、REQ/AC、允许/禁止范围、不可破坏行为、预算、done_when。
- 写租约与role_epoch、基线commit；待修复issue及其复现。
- 相关spec/ADR/约定与源码（只读需要的部分）。

## 步骤
1. 前置检查：
   - [ ] 写租约有效，role_epoch为当前
   - [ ] `git status` 干净；若有非本任务改动，视为用户改动，停止并报告，不覆盖
   - [ ] 在任务分支上工作，记录基线commit
2. 阅读：定位要改的模块与接缝，列出计划修改的文件；范围外的发现记issue。
3. 实现：按tdd循环逐个行为推进；覆盖成功、错误、取消、恢复路径；释放子进程、文件与锁；错误向上传播或映射为明确的用户可见状态。
4. 静态与构建：格式、lint、类型检查、构建，使用项目约定命令（如Rust的 `cargo fmt --check`、`cargo clippy`、`cargo test`）。
5. 回归：运行影响闭包内的测试，影响不确定时全量，记录结果。
6. 文档：行为、命令或配置变化时更新相应文档，只写实际执行过的命令。
7. 提交：commit消息关联任务与REQ；确认工作树干净（证据放在被忽略的目录）。
8. 冻结候选：绑定commit/tree/spec/environment/检查计划（test_plan）摘要（产品期由Runtime完成；自举期用 `tools/candidate.py freeze`，参数以 `--help` 为准）。冻结后不再修改。
9. 自测：在冻结候选上运行done_when对应的检查计划条目（经run_check类工具，argv/mode由计划决定，生成执行记录）；每项标PASS/FAIL/BLOCKED/UNKNOWN/NOT_RUN，并写明计划给出的mode。
10. 提交TaskResult（见下）。

## 限额与升级
- 同一失败fingerprint本地修复≤3次（默认，以run配置为准）；超限停止，写诊断（尝试过的方法、证据、当前假设）并BLOCKED或请求Commander。
- 输出被协议校验拒绝时纠正≤2次。
- 规格歧义：向CTO/Commander发query，不猜；关键阻塞则BLOCKED。
- 需要修改AC、删改检查计划中已有检查或修改gate才能通过：停止并提出CR请求，不自行修改。

## 产出物与格式
- Git commit与candidate manifest（`contracts/candidate.schema.json`）。
- 执行记录（`contracts/execution.schema.json`）及stdout/stderr。
- TaskResult：role="developer"；candidate_id=冻结候选；evidence=manifest与执行记录；summary=AC→测试映射+自测状态表（注明“自测，非验收”）+改动文件；issues=剩余风险、未运行检查、范围外发现；next_action=“等待QA/Review”或阻塞的恢复动作。

## 完成判据
- 候选已冻结且身份可校验；done_when中的每项检查都有自测记录，或有明确的NOT_RUN原因。
- 工作树干净，没有修改任何验收资产（AC、test plan、gate、baseline）。

## 常见错误
- 冻结后继续对同一候选“小修”。
- 生产路径残留TODO、`unimplemented!()`、硬编码mock或被吞掉的错误。
- 大范围重写，或顺手重构无关模块。
- 把自测PASS写成“已验收”。
- 覆盖用户未提交的改动，或执行 `git reset --hard`。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
