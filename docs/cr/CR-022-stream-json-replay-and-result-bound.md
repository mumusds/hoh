# CR-022 · stream-json 的回放标记、集合相等的边界与 TEST-002-02 的 assumption 措辞

ID：CR-022
状态：草案，待独立审查
批准人：（待：独立审查判定属 CR-003 D1 的细化后，由 CTO 依 CR-003 D1 预授权批准）
日期：2026-11-15
基线：改受锁文件 `contracts/protocol.schema.json`（`$defs.Event`）、`contracts/requirements.json`（REQ-004 行为 3）、`contracts/acceptance-scenarios.json`（TEST-002-02 的 pass_criteria 第 5 条）；`python3 tools/gen_spec_views.py` 重新生成视图；批准后 `python3 tools/baseline.py relock --cr docs/cr/CR-022-stream-json-replay-and-result-bound.md`

## 背景

三处受锁的规定之间不一致，ADR-031（M2-T13，已采纳）的设计审查逐一指出，草案在 ADR-031 的“移交”一节：

- **回放标记**：REQ-002 的决定（CR-003）要求“stream-json 回放的历史事件带 replay=true”；REQ-004 行为 2 要求 stream-json 的每行都是符合 `protocol.schema.json#/$defs/Event` 的对象，而 `$defs.Event` 为 `additionalProperties:false`、没有 `replay`。二者不能同时成立（ADR-031 决定 10）。
- **TEST-002-02 的措辞**：通过条件第 5 条写“每个 run 的 decisions 至少1行 payload 含 assumption=true 与依据引用”，而受锁的 `$defs.DecisionPayload` 的 record_assumption 分支只有 `kind`、`statement`、`basis` 且封闭（`additionalProperties:false`），合规的 payload 不能有 `assumption` 键（ADR-031 决定 14）。
- **集合相等的边界**：REQ-004 行为 3 要求 stream-json“输出该 run 全部已持久化事件，与 DB events 的 seq 集合一致”，而 CR-003（docs/06-orchestration.md:25）规定 BLOCKED 只停 Commander 决策类派发、已运行的 Attempt 继续到安全点、Runtime 的自动规则照常：客户端在 BLOCKED 的 run.result 处以 3 退出之后，DB 中该 run 还可以有之后的 Runtime 事件，“全部”没有时间上的边界，独立核验无从判定（ADR-031 E2，设计审查第 6 轮 M2T13D-551）。PAUSED 同理：它是 ADR-022 决定 8 的检查点，暂停中团队变更照常生效（REQ-018 行为 5、REQ-020 行为 4），结果之后同样还有事件（设计审查第 7 轮 M2T13D-602）。

## 决定

1. `contracts/protocol.schema.json` 的 `$defs.Event.properties` 加：

   ```json
   "replay": {
     "const": true,
     "description": "Present only on a line of a client's stream-json output that replays an event committed before the client's own command was accepted (or before an attach began); never stored, never sent by the host (REQ-002, CR-022)."
   }
   ```

   不加入 `required`；`additionalProperties:false` 不变。

2. `contracts/acceptance-scenarios.json` 的 TEST-002-02，pass_criteria 第 5 条由“每个 run 的 decisions 至少1行 payload 含 assumption=true 与依据引用”改为“每个 run 的 decisions 至少1行 state=APPLIED 的 assumption 记录，其 basis 非空（依据引用）”。assumption 记录即受锁 `$defs.DecisionPayload` 的 record_assumption 分支（`kind`、`statement`、`basis`，ADR-031 决定 14）。措辞沿用 AC-002-02 自己的“assumption 记录”，不写 `kind=record_assumption`：tools/validate_bundle.py 的规则（CR7R-01，validate_bundle.py:210-216）要求场景只断言它的 AC 点名的 kind，AC-002-02 的 then 没有点名 record_assumption；ADR-031 移交一节的草案写的是 `payload.kind=record_assumption`，按它改场景时 `bash scripts/check.sh --bundle` 报“TEST-002-02: asserts kind=record_assumption that AC-002-02 does not name”，所以本 CR 改用 AC 的词，判定内容不变。

3. `contracts/requirements.json` 的 REQ-004 行为 3 由“stream-json 必须输出该 run 全部已持久化事件，与 DB events 的 seq 集合一致；慢消费者不得造成缺口或重排。”改为“stream-json 必须输出该 run 截至客户端据以退出的那条 run.result（含）的全部已持久化事件，与 DB 中该 run seq ≤ 该 run.result 的 last_event_seq 的 events 的 seq 集合一致；慢消费者不得造成缺口或重排（CR-022）。”COMPLETED、FAILED、CANCELLED、INCOMPLETE 之后 Runtime 不再写该 run 的事件（ADR-031 更正 D），对它们这与原文等价；BLOCKED 与 PAUSED 时那条结果之后的事件——BLOCKED 上 Runtime 的自动规则（CR-003），PAUSED 上 ADR-022 的检查点使团队变更生效、监督的纳入、投递与证据的恢复——属于这次快照之外的延续。

不在锁内、随视图重新生成：docs/02-functional-spec.md（REQ-004 行为 3 与 TEST-002-02 的判定文字）。

## 影响

- 客户端：ADR-031 决定 10 的 stream-json 在本 CR 批准并 relock 之后才写 `replay` 键（实施切片 S3）；在那之前不写。新建的 `hoh -p` 没有回放的历史事件，它的每一行都不带该键（TEST-004-02 另断言这一点）。
- 测试：TEST-002-02 按新的第 5 条断言（APPLIED 的 record_assumption 行且 basis 非空）；回放标记的支撑测试（ADR-031 测试计划“回放标记”）在 relock 之后运行；REQ-004 行为 3 的边界由 ADR-031 E2 与它的支撑测试覆盖。
- 产品数据：`replay` 从不写进 DB，也不由 host 发出；现有事件与 DB 不变。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- `$defs.Event` 只多一个可选键，现有事件与现有的有效、无效实例的判定都不变。
- TEST-002-02 的第 5 条与原意等价且可判定：受锁 DecisionPayload 唯一能表达“假设与依据”的就是 record_assumption 分支与非空的 `basis`；另要求 state=APPLIED，比原文更严；AC-002-02 的文本不变。场景的检查方式（integration）、其余通过条件与观察量不变。
- REQ-004 行为 3 只给集合相等补上时间边界：AC-004-02（TEST-004-02）的结局是 INCOMPLETE，之后没有该 run 的事件，它仍要求与 DB 中该 run 的全部事件相等；AC 的条数与文本不变。
- 不改任何场景的断言强度（TEST-001-01 的终端模拟快照照原文，不提供退回：ADR-031 第二版草案曾有的“vt100 不可得时退回 `pty::squash`”已删去，M2T13D-05、-57）。
- 不改 CONTRACT 的架构与交互决定；不改 SQL 或 openapi。

## 是否属于 CR-003 D1 预授权

属于第 1 条“修正规格之间的不一致”：(1) REQ-002 的回放决定与 REQ-004 行为 2 的封闭 Event；(2) TEST-002-02 的措辞与受锁的 DecisionPayload；(3) REQ-004 行为 3 与 CR-003 的 BLOCKED 规则、ADR-022 的检查点。满足第 2 至 4 条（见“不变的部分”）。ADR-031 的设计审查已判定它属细化（第 1 轮 Codex 判 (1)(2) 属 D1 的细化；第 7 轮 Grok 判加入 (3) 之后仍属细化、必要且够用）；仍由一次独立审查判定后批准。

## 验证方式

独立审查判定属细化后批准；改上述三个受锁文件，`python3 tools/gen_spec_views.py`，`python3 tools/baseline.py relock --cr docs/cr/CR-022-stream-json-replay-and-result-bound.md`，`bash scripts/check.sh --bundle` 通过（含 examples 对 protocol.schema.json 的校验）；M2-T13 的 TEST-002-02、TEST-004-02 与回放标记的支撑测试证明实现符合。
