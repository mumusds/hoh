# Agent适配器设计与兼容性

## 统一接口（HoH自定义，非上游API）

probe()->Capabilities；start(ExecutionRequest)->AttemptHandle；send(Input)->DeliveryReceipt；subscribe()->EventStream；cancel()->CancelReceipt；resume(SessionRef)->AttemptHandle；inspect()->ObservedState；shutdown()->Result。

能力包括structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage。按版本/测试得到，不按品牌推断。Request带cwd、role、session_ref、prompt_bundle、budget、model_optional、runtime_profile。使用argv而非shell；模型字段为空即底座实际默认，执行后记录resolved model。

## Codex

生产首选App Server stdio：initialize→initialized→thread/start或resume→turn/start→item事件→turn/completed，失败/取消区别于completed success [S03]。关联threadId/turnId/itemId；turn/steer需expectedTurnId，过期拒绝。未知字段原样留raw旁路，不panic。

早期/不支持App Server时可用exec --json一次调用；该模式不提供等价持续控制，不得静默承诺mid_turn_steer。YOLO flag以当前--help探测，示例 `--dangerously-bypass-approvals-and-sandbox`；无flag则报告能力不足，不猜成不存在参数。

## Claude

-p + --output-format stream-json + --verbose，按需求启用include-partial-messages [S05,S06]。session元数据、消息/tool、result输出分开。error/interrupt/max-turns不等成功。stdio stream-input与resume以版本帮助和真样例确认，v1至少正确实现分轮resume与有界cancel。

YOLO常见为--dangerously-skip-permissions，等价底座bypassPermissions；不将HoH Auto映射Claude permission auto。不是改掉原生system prompt，而是用官方支持的追加/Agent规则或任务上下文。保持原生工具能力。

## Pi

--mode rpc，启用真实持久session-dir/session-id；不使用--no-session。prompt命令有correlation id；stdout按LF字节分帧，不能用把U+2028也分行的阅读器。先订阅再发送，持续drain stdout/stderr，避免管道阻塞 [S07,S08]。

prompt disposition=handled表示没有启动模型run，不能等永不存在的settled。started/queued的最终静止按版本支持的agent_settled；agent_end后可能压缩/重试继续。未知版本无settled时必须通过state/队列/重试状态验证或明确不支持，不能加sleep猜空闲。

RPC ctx.hasUI=true只表示部分协议UI能力。confirm/select/input/editor需要发送response或cancel，否则死锁；footer/custom等不能按完整TUI处理 [S09]。HoH渲染自己的状态栏，rpiv overlay映射数据而非嵌入Pi画面。headless交互必须按策略取消/记录阻塞，不伪造真人选择。

## 进程与终结

独立进程组；命令超时/取消先native cancel，等待短grace后SIGTERM整组，再有界SIGKILL并wait/reap。读stdout/stderr并行；协议frame上限1MiB初值，超限保存诊断并fail，不无限buffer。wall deadline与inactivity watchdog分开，长测试无输出不直接当死机。

对象销毁不意味着子进程树已结束，Tokio句柄生命周期需显式设计 [S23]。启动新attempt前确保同写租约旧进程不再写；否则block。

## 版本锁和协议样例

实际安装版本、launcher 与实际实现的 path/hash（pi 的 launcher 是 sh 脚本，实际实现在 install/releases/<版本>；codex 的 npm 包装下是 vendored 原生二进制）、--help hash、native schema hash、录制样例来源进入compat manifest。fixture含成功/失败/取消/压缩/工具/未知字段。升级先在无写任务的维护窗口跑compat suite，再用于新attempt。脚本安装成功不等协议兼容。

**compat manifest 结构**：`contracts/compat-manifest.schema.json`（CR-003）。只有实现、help、schema 三个哈希都匹配且兼容套件 status=PASS 的版本才可用于新 attempt，否则 COMPAT_UNVERIFIED。

**已知帧登记**：每个 CLI 版本在 `tests/fixtures/protocol/<agent>/<version>/known-frames.json` 列出已知帧（含已知但忽略的辅助帧：Claude 的 `rate_limit_event`、`system/task_*`；Codex 的 `remoteControl/status/changed`、`deprecationNotice`、`thread/goal/cleared` 等），由录制样例与官方文档汇总（M1-T07）。只有不在登记表中的帧才产生 `adapter.unknown_event`，这样“未知帧”可判定。

**鉴权失败分类**（CR-003）：Codex App Server 为 `turn/completed` status=failed 且 `codexErrorInfo=unauthorized`（0.166.0 schema 的枚举值，录制中未出现，fixture 依 schema 合成并标 `synthetic=true`）；Codex exec 为 `turn.failed`，其消息或之前的 `error` 事件含 `unexpected status 401`。Pi 1.5.0 没有结构化错误码，`success:false` 的 error 文本以 `No API key found` 开头为 BLOCKED_AUTH，以 `Model not found` 开头为 MODEL_UNAVAILABLE，其余保存原文并按 PermanentExternal 处理。

**取消优先**（CR-003）：已请求取消的 attempt 一律以 CANCELLED 结束。claude 2.1.317 的样例显示 SIGINT 后仍会输出 `result(subtype=error_during_execution, is_error=true)` 且退出码 0，这个帧只作诊断保存。

## 交接和Fallback

fallback只能满足相同角色契约；没有独立QA时可以继续实现但不能通过最终验收。native进程CLI的最终回答只作为claim；产物/测试证据由HoH独立验证。没有自定义工具时，TaskResult结构化输出作为协作桥，不假设所有底座都支持同一种MCP/Skill注入。
