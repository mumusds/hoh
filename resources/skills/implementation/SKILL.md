---
name: hoh-implementation
description: 实现与候选提交；只在关联工程阶段加载。
---
# 实现与候选提交

## 输入
当前goal、版本化spec/AC、当前任务与范围、候选/证据引用、interaction mode、可用工具和剩余预算。

## 执行规范
读取任务及相关文档，不全仓重写。遵守主写者边界，不覆盖用户dirty文件。实现成功/错误/取消/恢复路径，运行相关测试并输出CandidateResult、真实自测和剩余风险。提交可验证Git候选；自测不是独立验收；不擅自更改requirements/gates让测试通过。

## 输出
提交可机器校验的TaskResult或该阶段声明的结构化报告，带task/attempt/role/spec/candidate身份、证据路径和状态。必要事实未知须注明，输出缺失不能补造。

## 完成与失败
本阶段达到输入契约的观察标准才完成；无证据或外部前提不足时显式失败/阻塞。阶段完成不等同整个产品验收。所有最终质量判断以Runtime门禁为准。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
