---
name: hoh-release
description: 完整交付；只在关联工程阶段加载。
---
# 完整交付

## 输入
当前goal、版本化spec/AC、当前任务与范围、候选/证据引用、interaction mode、可用工具和剩余预算。

## 执行规范
冻结最终spec与candidate，逐条检查required AC、真实Agent/插件、构建/测试/部署/soak和独立报告，运行release_gate。缺项输出精确INCOMPLETE/BLOCKED，不能改门槛。合并/重构建后hash变更重验。交付源码/产物/文档/版本清单/原始证据/已知问题与恢复方案。

## 输出
提交可机器校验的TaskResult或该阶段声明的结构化报告，带task/attempt/role/spec/candidate身份、证据路径和状态。必要事实未知须注明，输出缺失不能补造。

## 完成与失败
本阶段达到输入契约的观察标准才完成；无证据或外部前提不足时显式失败/阻塞。阶段完成不等同整个产品验收。所有最终质量判断以Runtime门禁为准。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
