---
name: hoh-domain-modeling
description: 领域与数据建模；只在关联工程阶段加载。
---
# 领域与数据建模

## 输入
当前goal、版本化spec/AC、当前任务与范围、候选/证据引用、interaction mode、可用工具和剩余预算。

## 执行规范
区分Project、Spec、Requirement、Task、Run、Iteration、Attempt、Session、Candidate、Evidence、Release。定义identity、生命周期、唯一性、事务边界与不变量，检查术语同名不同义。先写失败例：重复、乱序、版本冲突、并发、丢ACK。输出领域词汇表、Schema/SQL变更、兼容和测试。

## 输出
提交可机器校验的TaskResult或该阶段声明的结构化报告，带task/attempt/role/spec/candidate身份、证据路径和状态。必要事实未知须注明，输出缺失不能补造。

## 完成与失败
本阶段达到输入契约的观察标准才完成；无证据或外部前提不足时显式失败/阻塞。阶段完成不等同整个产品验收。所有最终质量判断以Runtime门禁为准。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
