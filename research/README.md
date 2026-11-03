# 调研证据与边界

核验日期：2026-10-13。来源为用户文章、当前对话最终确认的产品约束、公开论文 HTML、官方文档和作者仓库。
`sources.json` 中记录本次实际核对的入口；以下文档中的 [Sxx] 均引用该清单。未核对源码具体 commit 的来源不填写虚假的锁定 SHA。

## 可确认与不可确认

- 已确认：论文方法、官方 Lite 与完整演示系统的边界；三个 Agent 的公开调用/会话语义；Pi 插件定位；成熟 Rust 基础组件的用途。
- 本包的设计：Rust 单体、智能 Commander、团队角色、统一命令服务、需求追踪、HCP、TUI 工作台和最终门禁。这些不是论文已验证的全部实现。
- 尚未实测：你机器上的 CLI 版本/认证、真实模型调用、三插件共同长期运行、HoH 成品的跨天效果。
- 当前容器不能解析 GitHub 下载域名，未完成 git clone、Cargo 构建或真实 npm 安装。网页读取成功不等于本地源码执行成功。
- 论文 PDF 抓取因大小限制失败；本次依据官方 HTML 版本的方法与附录，不声称完成 PDF 图像/表格视觉复核，不沿用聊天中未经核验的精确分数表。
- 本包不依赖未再次确认的 Proof-or-Stop、VeriHarness、Grok Bot 特定协议。相关理念已用原创契约表达。

## 已纠正的细节

1. 不把内部通信叫 ACP；外部已有 Agent Client Protocol。内部名称 HCP（HoH Collaboration Protocol）。
2. Pi RPC `prompt` 接受、`agent_end`、`agent_settled` 和 `handled` 分支不同，安装版本需做协议探测。
3. Pi 当前官方 README 已迁移包命名；不盲装旧包。保留已有可用安装，通过实际能力探测决定是否更新。
4. Pi RPC 支持部分扩展 UI，不提供完整终端；插件配置界面不可原样嵌入 HoH。
5. 上游 Wayfinder 是有真人参与语义的规划工具；本包原创规格 Skill 将互动与非互动行为分开。
6. HoH 根仓库和 Lite 子目录许可证不同；第三方再分发需逐目录检查，不将所有资源统一改许可证。
7. Commander 提议不是事实；子进程 exit 0、模型“完成”不是交付门禁通过。
8. 非交互就是 `hoh -p`；没有 `hoh plan`/`hoh mode plan`，也不为每个面板增加 CLI 子命令。

## 如何更新调研

先读取已安装 `--version` 和 `--help`，再查对应源码 tag/协议 Schema；将解析版本和摘要写入 `.hoh-dev/preflight.json`和对应的`*version.lock`，正式产品另生成其Runtime lock manifest。
协议不兼容必须显式降级或阻塞，不可猜造 flag/event。更新仅在没有运行中的会话时执行。
