# CR-027 · AC-021-06 与 TEST-021-06 的一致性更正：Commander 从一开始缺员的 run 与该 Agent 的 Developer turn

ID：CR-027
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权（team-lead 于 2026-11-17 批准本更正的内容：它是与 ADR-025 决定 5 的一致性更正，不放宽；独立 Review 随 M2-T20 的两路实施审查在冻结候选 m2t20-001 上进行，结论记入 reports/reviews/）
日期：2026-11-17
基线：改受锁文件 `contracts/requirements.json`（REQ-021 的 AC-021-06 的 when 与 then）、`contracts/acceptance-scenarios.json`（TEST-021-06 的 steps 与 pass_criteria）；`python3 tools/gen_spec_views.py` 重新生成视图；`python3 tools/baseline.py relock --cr docs/cr/CR-027-ac-021-06-commander-vacancy-run.md`

本 CR 更正 CR-024（基线修订 19）写入的 AC-021-06 与 TEST-021-06。原意是把它作为 CR-024 的带日期修订、以 `relock --amend` 重新上锁；但 CR-024 的锁记录已经提交，`tools/baseline.py` 拒绝修改已提交的 CR（“record further changes in a new CR”），锁也核对 CR 文件的 sha256，所以按工具的规则另写本 CR。

## 背景

M2-T20 实施 TEST-021-06 时发现（ADR-032 的 TEST-021-06 实施注记）：在 (b)、(c) 的 unconfined 与 unavailable、(d) 的只走 exec 三处，commander 绑定从一开始就 BLOCKED（`<agent>:CAPABILITY_UNSUPPORTED:read_only_workspace`）。绑定被阻塞本身产生 `team.binding_blocked`，它唤醒 Commander；Commander 不可用而 run 欠它一次唤醒，所以 run 按 ADR-025 决定 5（REQ-021 行为 8）为 BLOCKED（COMMANDER_UNAVAILABLE），已就绪的 Developer 任务被撤回，同一 run 里不启动任何 turn。AC-021-06 原文要求“该 Agent 作为 Developer 的 turn 启动并 SETTLED”，与这条既有规则矛盾，在同一个 run 里无法成立。

AC 要证明的是：只读档的缺失与主机的事实只作用于 Commander 的选人，该 Agent 在其他职位照常可用。这一点不因 run 被阻塞而改变：该 Agent 的 Developer 绑定照常得到会话（ACTIVE）；在 Commander 为另一个可用 Agent 的 run S 里，它的 Developer turn 照常启动并 SETTLED；unavailable 恢复为 confined 之后，原 run 解除阻塞，其 Developer turn 在原会话中 SETTLED。

同时按 team-lead 的决定写明“证据记下 unknown 与原因”的落点：可用的判决不写 staffing 证据事件，所以由每个 Commander Attempt 自己的记录（其 `context.packed` 的 `read_only`，在它 RUNNING 的提交里写）写下本机检查的结论与原因（不加新的事件种类）。

## 决定

### `contracts/requirements.json` · REQ-021 · AC-021-06

**when**：在末尾的 (d) 之后追加 run S 的说明。

- 旧文：(d) 只走 exec 的 codex，以及运行中降级的 codex，各同时为 Commander 与 Developer
- 新文：(d) 只走 exec 的 codex，以及运行中降级的 codex，各同时为 Commander 与 Developer；(b)、(c) 的 unconfined 与 unavailable、(d) 的只走 exec 中 commander 绑定从一开始即 BLOCKED，各另有 run S：Commander 为另一个可用的 Agent，Developer 为该 Agent

**then**：三处。

- 旧文：没有 Commander Attempt 启动；该 Agent 作为 Developer 的 turn 启动并 SETTLED。
- 新文：没有 Commander Attempt 启动；该 Agent 的 Developer 绑定照常得到会话；commander 绑定被阻塞本身唤醒 Commander，所以该 run 为 BLOCKED（COMMANDER_UNAVAILABLE，ADR-025 决定 5），其中不启动 turn；该 Agent 在 run S 中作为 Developer 的 turn 启动并 SETTLED。

- 旧文：会话记录的 capabilities_json 不变（主机的事实只作用于选人）；codex 作为 Developer 照常 SETTLED；unknown 时 codex 照常作为 Commander 运行，证据记下 unknown 与原因；检查从 unavailable 改为 confined 后，下一次刷新使 commander 绑定解除阻塞、Commander Attempt 启动；
- 新文：会话记录的 capabilities_json 不变（主机的事实只作用于选人）；codex 作为 Developer 在 run S 中照常 SETTLED；unknown 时 codex 照常作为 Commander 运行，每个 Commander Attempt 自己的记录（其 context.packed 的 read_only）写下 unknown 与原因；检查从 unavailable 改为 confined 后，下一次刷新使 commander 绑定解除阻塞、Commander Attempt 启动，该 run 的 Developer turn 在其原会话中 SETTLED；

- 旧文：没有在 exec 上启动的 Commander turn；codex 作为 Developer 照常 SETTLED
- 新文：没有在 exec 上启动的 Commander turn；只走 exec 的 codex 作为 Developer 在 run S 中照常 SETTLED，运行中降级的 codex 作为 Developer 在降级的那个 run 中经 exec 照常 SETTLED

### `contracts/acceptance-scenarios.json` · TEST-021-06

**steps**：三条中的四处。

- 旧文：(b) 对三组缺项变体启动 run，该 Agent 同时为 Commander 与 Developer；读取兼容清单、团队视图与 attempts
- 新文：(b) 对三组缺项变体各起两个 run：run R 中该 Agent 同时为 Commander 与 Developer，run S 中 Commander 为另一个 Agent、Developer 为该 Agent；读取兼容清单、团队视图、runs 与 attempts

- 旧文：(c) 依次把本机检查设为 unconfined、unavailable、unknown，codex 同时为 Commander 与 Developer；再从 unavailable 改为 confined；
- 新文：(c) 依次把本机检查设为 unconfined、unavailable、unknown，codex 同时为 Commander 与 Developer（unconfined、unavailable 时另起 run S：Commander 为 claude，Developer 为 codex）；再从 unavailable 改为 confined；

- 旧文：读取团队视图、判决证据、events、attempts 与 sessions
- 新文：读取团队视图、判决证据、events（含 Commander Attempt 的 context.packed）、attempts 与 sessions

- 旧文：(d) 只走 exec 的 codex，以及在一次 Developer turn 中降级的 codex，各同时为 Commander 与 Developer；
- 新文：(d) 只走 exec 的 codex（run R 与 run S，同 (b)），以及在一次 Developer turn 中降级的 codex（同一个 run），各同时为 Commander 与 Developer；

**pass_criteria**：三条中的三处。

- 旧文：Commander Attempt 数为 0；该 Agent 的 Developer Attempt 为 SETTLED
- 新文：Commander Attempt 数为 0；run R 为 BLOCKED（COMMANDER_UNAVAILABLE），其 Developer 绑定有会话；run S 中该 Agent 的 Developer Attempt 为 SETTLED

- 旧文：sessions.capabilities_json 不变；Developer Attempt 为 SETTLED；unknown：Commander Attempt 启动，证据含 unknown 与原因；改为 confined 后的下一次刷新：commander 绑定解除阻塞，Commander Attempt 启动
- 新文：sessions.capabilities_json 不变；run S 中 codex 的 Developer Attempt 为 SETTLED；unknown：Commander Attempt 启动，其 context.packed 的 read_only.host_check 为 unknown 并带原因；改为 confined 后的下一次刷新：commander 绑定解除阻塞，Commander Attempt 启动，run R 的 Developer Attempt 在原会话中 SETTLED

- 旧文：fake 记录中没有 Commander 的 exec 启动；codex Developer Attempt 为 SETTLED
- 新文：fake 记录中没有 Commander 的 exec 启动；codex 的 Developer Attempt 为 SETTLED（只走 exec 时在 run S 中，降级时在同一个 run 中）

## 影响

- TEST-021-06 的实现（crates/hoh-app/tests/commander_readonly.rs）已按本文：(b)、(c)、(d) 的只走 exec 各起 run R 与 run S；(c) 的 unknown 断言每个 Commander Attempt 的 `context.packed` 记下 unknown 与原因。
- 产品行为不变；只是 AC 与场景如实描述 ADR-025 决定 5 下的可观察结果，并把一条证据落到持久的记录上。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 不放宽任何检查：受检的行为不变——主机的事实与只读档的缺失只作用于 Commander 的选人，该 Agent 的 Developer 职位照常运行；run S 与恢复后的原会话把它证明到 SETTLED；unknown 的证据由“证据记下”加严为写进 Attempt 的持久记录。
- 不改 CONTRACT、协议 schema、SQL、openapi、门禁工具；不新增原因码与事件种类。
- AC-021-06 的 (a) 与 AC-021-07、TEST-021-07 不变。

## 是否属于 CR-003 D1 预授权

属于：
1. 修正规格之间的不一致（AC-021-06 与 REQ-021 行为 8、ADR-025 决定 5）。
2. 不删除需求或 AC，不改 required，不缩小 release_targets。
3. 不放宽检查；unknown 的证据加严。
4. 不改 CONTRACT 的架构与交互决定。

## 验证方式

1. 按上文逐字改两个受锁文件；`python3 tools/gen_spec_views.py`；`python3 tools/baseline.py relock --cr docs/cr/CR-027-ac-021-06-commander-vacancy-run.md`（修订 20）；`bash scripts/check.sh --bundle` 通过。
2. TEST-021-06 的 12 个测试在 integration 检查中通过（候选 m2t20-001）。
3. 两路独立实施审查在同一候选上核对本 CR 与实现一致。
