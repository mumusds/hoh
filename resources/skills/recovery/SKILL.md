---
name: hoh-recovery
description: 故障诊断与恢复；只在关联工程阶段加载。
---
# 故障诊断与恢复

## 输入
当前goal、版本化spec/AC、当前任务与范围、候选/证据引用、interaction mode、可用工具和剩余预算。

## 执行规范
先保存日志与工作区，定位故障分类。读取attempt/effect/outbox/lease/epoch，确认外部动作是否发生，再选择resume/retry/repair/rollback/BLOCKED。未知副作用不要盲重跑。保留失败候选与证据，写新decision而非覆盖旧历史。取消必须终止正确进程组并wait。

## 输出
提交可机器校验的TaskResult或该阶段声明的结构化报告，带task/attempt/role/spec/candidate身份、证据路径和状态。必要事实未知须注明，输出缺失不能补造。

## 完成与失败
本阶段达到输入契约的观察标准才完成；无证据或外部前提不足时显式失败/阻塞。阶段完成不等同整个产品验收。所有最终质量判断以Runtime门禁为准。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
