# 原始资料索引

## S01 · 论文正文（HTML）

https://arxiv.org/html/2609.01481v1

类型：研究原文。核验摘要：规划—开发—独立QA；继承工件与证据；渐进披露；本文未独立复现实验。

## S02 · HoH 官方仓库

https://github.com/Flesymeb/HarnessOfHarness

类型：官方源码说明。核验摘要：HoH-lite 不等同完整演示系统；根目录 MIT，hoh-lite 子目录 Apache-2.0。

## S03 · Codex App Server

https://developers.openai.com/codex/app-server/

类型：官方接口。核验摘要：initialize/initialized、thread、turn、item；turn/completed；turn/steer 需 expectedTurnId。

## S04 · Codex CLI reference

https://developers.openai.com/codex/cli/reference/

类型：官方接口。核验摘要：以安装版本 --help 为准；非交互 exec；YOLO 参数必须探测。

## S05 · Claude CLI reference

https://code.claude.com/docs/en/cli-reference

类型：官方接口。核验摘要：-p、stream-json、--verbose、会话恢复、bypassPermissions；不等同 HoH Plan/Auto。

## S06 · Claude programmatic execution

https://code.claude.com/docs/en/headless

类型：官方接口。核验摘要：结构化输出与流式事件；原生 Agent 调用而非重造模型客户端。

## S07 · Pi RPC reference

https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/rpc.md

类型：官方接口。核验摘要：JSONL 仅 LF 分帧；prompt accepted 非完成；agent_end 后可能继续；handled 分支不等 settled。

## S08 · Pi CLI reference

https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/cli.md

类型：官方接口。核验摘要：session-id/session-dir/--approve/--skill；不在正式运行使用 --no-session。

## S09 · Pi Extension UI

https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/rpc-extension-ui.md

类型：官方接口。核验摘要：RPC 的 hasUI=true 不表示有真实 TUI；custom/footer 等受限；dialog 必须响应或取消。

## S10 · Pi installation README

https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/README.md

类型：官方安装。核验摘要：当前文档链接 earendil-works/pi、@earendil-works/pi-coding-agent；Node >=22.19。

## S11 · Grok Build

https://github.com/xai-org/grok-build

类型：官方源码说明。核验摘要：Rust TUI/运行时分层；交互借鉴而非复制完整 Runtime；ACP 是外部协议。

## S12 · DeepSeek Harness

https://github.com/deepseek-ai/deepseek-harness

类型：官方源码说明。核验摘要：参考 Web 客户端和主机状态分离，不引入第二套插件编排引擎。

## S13 · pi-open-tui

https://github.com/OldSuns/pi-open-tui

类型：插件作者。核验摘要：借鉴状态栏和交互；不宣称可直接嵌入 Rust TUI。

## S14 · Pi Continuity

https://github.com/alpertarhan/pi-smart-compact

类型：插件作者。核验摘要：明确选择此作者版本；压缩和连续性；跨会话记忆与 Hermes 避免重复。

## S15 · Pi Hermes Memory

https://github.com/chandra447/pi-hermes-memory

类型：插件作者。核验摘要：长期记忆、自动整理；policy-only 不保证每次召回；不能代替验收账本。

## S16 · rpiv monorepo

https://github.com/juicesharp/rpiv-mono

类型：插件作者。核验摘要：正确包名 @juicesharp/rpiv-todo；不同时装完整 rpiv-workflow 作为第二全局调度器。

## S17 · rpiv todo overlay

https://raw.githubusercontent.com/juicesharp/rpiv-mono/main/packages/rpiv-todo/docs/overlay.md

类型：插件作者。核验摘要：会话级任务展示与恢复；全局任务仍由 HoH 管理。

## S18 · Matt grill-with-docs

https://raw.githubusercontent.com/mattpocock/skills/main/skills/engineering/grill-with-docs/SKILL.md

类型：Skill 作者。核验摘要：原文件调用 grilling 与 domain-modeling，不是独立完整工作流。

## S19 · Matt wayfinder

https://raw.githubusercontent.com/mattpocock/skills/main/skills/engineering/wayfinder/SKILL.md

类型：Skill 作者。核验摘要：有 HITL/AFK 区别；真人讨论不能由 Agent 假扮用户代答；不原样塞进无人值守循环。

## S20 · Matt Skills LICENSE

https://raw.githubusercontent.com/mattpocock/skills/main/LICENSE

类型：许可证原文。核验摘要：MIT；本包提供 HoH 原创等效 Skill，不冒称逐字打包了完整上游。

## S21 · SQLite WAL

https://www.sqlite.org/wal.html

类型：官方技术文档。核验摘要：单写者、checkpoint、WAL 同机共享内存；本地磁盘，不放 NFS。

## S22 · SQLite backup

https://www.sqlite.org/backup.html

类型：官方技术文档。核验摘要：在线备份或一致性快照；不可只拷贝正在 WAL 写入的主数据库文件。

## S23 · Tokio process

https://docs.rs/tokio/latest/tokio/process/index.html

类型：官方 API。核验摘要：子进程生命周期、管道与取消需显式管理；丢弃句柄不等于可靠终止进程树。

## S24 · Ratatui alternate screen

https://ratatui.rs/concepts/backends/alternate-screen/

类型：官方文档。核验摘要：终端 alternate screen；恢复终端状态是应用责任。

## S25 · Axum SSE

https://docs.rs/axum/latest/axum/response/sse/index.html

类型：官方 API。核验摘要：服务端事件流组件；重放、游标、去重仍需 HoH 实现。

## S26 · Playwright retries

https://playwright.dev/docs/test-retries

类型：官方文档。核验摘要：首轮失败重试成功属于 flaky，不应抹掉首轮失败。

## S27 · Agent Client Protocol

https://agentclientprotocol.com/get-started/introduction

类型：协议官网。核验摘要：ACP 已用于编辑器—Agent 通信；HoH 内部消息改名 HCP 避免混淆。

## S28 · Rust install

https://rust-lang.org/tools/install/

类型：官方安装。核验摘要：rustup；首次解析版本后固化，不在长任务中自动升级。

## S29 · Node releases

https://nodejs.org/en/about/previous-releases

类型：官方发布。核验摘要：选择支持中的 LTS，下载后校验 SHASUMS，记录实际版本。

## S30 · Claude setup

https://code.claude.com/docs/en/setup

类型：官方安装。核验摘要：优先原生安装器；认证及系统要求以实际版本检查。

## S31 · GitHub protected branches

https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches

类型：官方文档。核验摘要：检查与分支规则；同一账户不能假设能给自己满足独立人类审批。

## S32 · GitHub Actions checkout

https://github.com/actions/checkout

官方README当前示例使用actions/checkout@v7。仅用于启动包CI配置，未在远程CI执行。

限制：主分支/major tag可变；正式发布应固定实际验证的commit，不凭文档声称CI已通过。

## S33 · GitHub Actions setup-python

https://github.com/actions/setup-python

官方README提供setup-python@v7，支持显式Python版本。启动包CI选择3.11/3.12/3.13作为测试矩阵，远程结果未执行。

限制：主分支/major tag可变；正式发布应固定实际验证的commit，不凭文档声称CI已通过。
