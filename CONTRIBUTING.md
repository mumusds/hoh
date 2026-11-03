# HoH开发规范

Rust核心使用类型表达状态/错误；公共接口避免未经说明的unwrap/expect；异步代码不阻塞runtime worker；进程和文件句柄明确生命周期；所有外部命令用argv数组。主路径不得含todo!/unimplemented!/伪造成功fallback。

先测试再行为实现：状态/恢复/门禁优先负例，UI快照与真实交互共同验证。保留第一失败，不用重试抹平。JSON schema/SQL/OpenAPI与Rust类型保持一致，由CI断言。

Git：新任务feature branch；小范围commit绑定REQ/工作包；无关格式化分离；不要自动reset用户修改；候选冻结再QA；合并后新SHA重新检查。远程规则按真实仓库能力配置，不猜CI检查名。

依赖：Rust精确toolchain/Cargo.lock，Web package-lock；库版本经实际解析写入，更新走独立PR和compat测试。第三方源码/Skill移植保留许可、修改声明和来源版本；本包原创Skill可直接内置。

文档与用户输出中文优先，代码/API/枚举用稳定英文；错误有machine code、可读信息、上下文与恢复动作。状态、结果和时间线不能由LLM随意文本推断。

受锁契约（CONTRACT、需求、场景、policy、schema、docs/10、门禁工具）只能随APPROVED的CR修改并执行 `tools/baseline.py relock`；`docs/02`与`docs/18`由 `tools/gen_spec_views.py` 生成，勿手改。

提交前：`scripts/check.sh --bundle`；cargo fmt --check；cargo clippy --workspace --all-targets -- -D warnings；cargo test --workspace --locked；Web类型/单测/build；相关PTY/E2E；有需要时真实Agent/插件测试。未运行准确标注。

Review检查：范围/REQ覆盖、状态机、并发、外部效果、取消、资源释放、证据、未知状态、UX错误流程、兼容性、性能、可维护性。不以代码行数或“写了很多文档”验收。
