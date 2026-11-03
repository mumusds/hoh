---
name: hoh-task-planning
description: 可验证增量规划；只在关联工程阶段加载。
---
# 可验证增量规划

## 输入
当前goal、版本化spec/AC、当前任务与范围、候选/证据引用、interaction mode、可用工具和剩余预算。

## 执行规范
读当前spec、candidate、有效证据、open/reopened issue，选择一个局部完整可观察的增量。任务带REQ/AC、依赖、范围、不可破坏行为、输出契约和预算。严重回归优先，稳定阶段允许只修复。不在计划里规定模型所有思考步骤，约束可验证结果。

## 输出
提交可机器校验的TaskResult或该阶段声明的结构化报告，带task/attempt/role/spec/candidate身份、证据路径和状态。必要事实未知须注明，输出缺失不能补造。

## 完成与失败
本阶段达到输入契约的观察标准才完成；无证据或外部前提不足时显式失败/阻塞。阶段完成不等同整个产品验收。所有最终质量判断以Runtime门禁为准。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
