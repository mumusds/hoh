# 调研结论与成熟技术借鉴

来源索引见research/SOURCES.md；所有具体产品接口按本机版本再次探测。研究结论不是实跑成绩。

## 方法底座

HoH论文的增量工件+证据继承、独立QA、角色契约和渐进披露是直接参考的机制 [S01]。官方Lite是轻量实现，完整演示另有工具/技能/集成 [S02]。我们不承诺混合三个Agent后自动得到论文同等增益，也不需要在生产中保留另一Python调度层。

## 可复用组件

|对象|采用|不采用|
|---|---|---|
|Codex App Server|原生事件、会话、steer、cancel|靠抓取TUI文本判断状态|
|Claude stream-json|原生开发能力与结构化输出|替代成自写Anthropic API coding loop|
|Pi RPC/扩展|原生会话/压缩/记忆/局部todo|把Pi footer UI直接嵌进Rust|
|Grok Build|可展开块/全屏/团队监视的交互思路|整套工具与模型runtime|
|DeepSeek Harness|Web主机权威状态/客户端思路|Cordis与第二全局workflow|
|Ratatui/Tokio/SQLite/Axum|成熟基础能力|微服务/消息中间件/云数据库先行|
|Matt Skills|需求澄清/决策记录/领域建模的方法|未经适配把HITL流程塞到headless|

以上分别有[S03–S25]的官方或作者来源。具体HoH模块、协议、恢复算法与UX是本包原创建议。

## 本次尚未取得的事实

不能在当前容器进行GitHub克隆/下载/真实cargo构建；没有用户认证，不跑真实模型/插件。来源未获取固定commit不伪造hash，bootstrap会在目标VM记录真实版本；未做PDF图表视觉复核不复述其精确表格数据。

因此定义M0协议探测与M7真实验证是必要工程工作，而不是把未核实点留给AI随意猜测。以下风险文档提供每项关闭动作。
