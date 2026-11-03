---
name: hoh-independent-qa
description: 独立质量验收；只在关联工程阶段加载。
---
# 独立质量验收

## 输入
当前goal、版本化spec/AC、当前任务与范围、候选/证据引用、interaction mode、可用工具和剩余预算。

## 执行规范
从正式AC和冻结候选出发，而非开发者自述。确认source/spec/env/testplan身份，设计黑盒与白盒观察，执行真实命令/PTY/浏览器；记录失败和复现。candidate被改则INVALID。未测=NOT_RUN，mock≠live，flaky不抹首失败。不在被测目录修复源码；提交Issue给下一候选。

## 输出
提交可机器校验的TaskResult或该阶段声明的结构化报告，带task/attempt/role/spec/candidate身份、证据路径和状态。必要事实未知须注明，输出缺失不能补造。

## 完成与失败
本阶段达到输入契约的观察标准才完成；无证据或外部前提不足时显式失败/阻塞。阶段完成不等同整个产品验收。所有最终质量判断以Runtime门禁为准。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
