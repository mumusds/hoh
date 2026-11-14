# CR-021 · 命令执行能力进入能力列表；兼容套件的 fixture 集 hash 覆盖的范围

ID：CR-021
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权（ADR-030 设计审查第 5 轮 Codex 01a1d9d8-a595-7d93-9cee-2fc8c1225271 与 Grok 01a1d9d8-af1f-7da2-96cb-f3c8ae926797 都判 PASS，并都判 CR-021 属 CR-003 D1 的细化、可由 CTO 批准；草案在第 5 轮审查的冻结提交 86b44ea 中，批准时文字未改）
日期：2026-11-14
基线：修改受锁文件 `contracts/requirements.json`（REQ-027 行为 2、行为 3），重新生成视图，重新上锁

## 背景

- REQ-020 决定 1 要求 developer、qa 有“经 probe 实测的命令执行能力”，而 REQ-027 行为 2 列出的 Capabilities（structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage）没有这一项。按规格它既无处记录（`sessions.capabilities_json`），也无法在选择与派发时匹配（ADR-030 设计审查 M2T15D-06）。这是两条受锁规格之间的不一致。
- REQ-027 行为 3 要求 manifest“记录所用 fixture 集 hash 与结果”。ADR-030 决定 10 把本构建的套件集摘要写进 `suite.fixture_set_sha256`：它覆盖嵌入的 fixture 与启动变体表、请求构建函数对每个启动变体实际给出的消息与 argv、hoh-agents 的套件定义（构建、校验、回放的源码与锁文件），使改了请求构建或校验的 HoH 不接受旧构建的 PASS（M2T15D-102）。按行为 3 的字面只核对 fixture 文件的哈希时，本构建合法的 PASS 会对不上（M2T15D-250）。这里写明它覆盖的范围；它只比单纯的 fixture 哈希更严。

## 决定

`contracts/requirements.json` REQ-027：

- 行为 2 的括号列表加 `command_execution`，全文为：“Capabilities（structured_events、resume、mid_turn_steer、custom_tools、images、browser、ui_dialogs、token_usage、command_execution）必须由探测与兼容套件结果得出，不按品牌硬编码，并写入 sessions.capabilities_json；command_execution 指适配器在 HoH 的启动选项下可运行命令，由被测二进制的 help/schema 实测（ADR-030 决定 12）。”
- 行为 3 全文为：“兼容套件 fixture 必须对每个 agent 覆盖成功、失败、取消、压缩、工具、未知字段6类，manifest 记录所用 fixture 集 hash（本构建的套件集摘要：覆盖嵌入 fixture、请求构建函数的实际输出与套件定义，ADR-030 决定 10）与结果。”

不在锁内、随实现同步：docs/13 的能力列表与套件集说明（ADR-030 实施切片 S9）。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 不放宽任何检查或门禁：行为 2 只多一个能力要求（developer、qa 缺它即不被选中），行为 3 只写明哈希覆盖的范围（比只哈希 fixture 更严）。
- 不改 CONTRACT 的架构与交互决定；不改协议、SQL、compat-manifest schema（`suite.fixture_set_sha256` 仍是 64 位 hex）或 openapi。

## 是否属于 CR-003 D1 预授权

属于第 1 条“修正规格之间的不一致”（REQ-020 决定 1 与 REQ-027 行为 2；ADR-030 决定 10 与 REQ-027 行为 3 的字面），满足第 2 至 4 条（见“不变的部分”）。由独立审查判定后批准。

## 验证方式

ADR-030 的设计审查判定 CR-021 属细化且可以批准；批准后改 requirements.json 的两句、`python3 tools/gen_spec_views.py` 重新生成生成视图、`python3 tools/baseline.py relock --cr docs/cr/CR-021-command-execution-capability.md`，`bash scripts/check.sh --bundle` 通过；M2-T15 的能力测试（ADR-030 测试计划“能力”）与 TEST-020-02 的正对照证明它被实测与记录。
