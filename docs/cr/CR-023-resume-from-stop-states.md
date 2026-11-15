# CR-023 · ResumeRun 从停止状态恢复的例外边界与它的承接

ID：CR-023
状态：草案，待独立审查
批准人：（待：独立审查判定属 CR-003 D1 的细化后，由 CTO 依 CR-003 D1 预授权批准）
日期：2026-11-15
基线：改受锁文件 `contracts/requirements.json`（REQ-036 的行为 3、失败模式、第 2 条决定）与不在锁内的 `contracts/development-tasks.json`（M2-T13、M2-T11 的 done_when）；`python3 tools/gen_spec_views.py` 重新生成视图；批准后 `python3 tools/baseline.py relock --cr docs/cr/CR-023-resume-from-stop-states.md`

## 背景

- REQ-036 的第 2 条决定写明“INCOMPLETE是否终态未定；本选择终态仅COMPLETED/CANCELLED/FAILED，INCOMPLETE/BLOCKED/WAITING_EXTERNAL/PAUSED可经ResumeRun回到resume_state”，行为 3 要求“恢复回到resume_state并先reconcile”；REQ-036 属 v0.1 与 v1 的 required 范围。
- 这条决定与另外几条受锁的规定冲突：REQ-042 行为 6 规定预算耗尽或 deadline 到达即停止新派发、Run=INCOMPLETE，行为 2 规定 `next_retry_at` 之前不得派发；ADR-011 与 docs/06（CR-003）规定 Run 进入任何工作态都要求阻塞集合为空。照字面实现，预算仍耗尽的 INCOMPLETE、重试未到期的 WAITING_EXTERNAL 被 ResumeRun 放回工作态之后，pace 会在下一 tick 把它停回去，中间没有任何工作，只多一对状态与结果（ADR-031 未采用 18）；BLOCKED 则根本不能在阻塞集合非空时回到工作态。本版本没有提高一个 Run 的预算或期限的入口（二者只在 StartRun 写入，persistence/team.rs）。
- REQ-036 的 AC-036-01..04 分属 M2-T02、M2-T05、M2-T16、M2-T12，没有一条覆盖从这四种状态的 ResumeRun；产品今天只从 PAUSED 恢复（ADR-021 决定 7），没有任务的 done_when 承接它。
- ADR-031（M2-T13，已采纳）的设计审查第 1 轮（M2T13D-07、-50）要求承接有契约载体，第 2 轮（M2T13D-151）要求例外边界写进受锁的决定本身，第 3 轮（M2T13D-251）要求行为 3 与失败模式随之改写——否则受锁的行为“恢复回到 resume_state 并先 reconcile”仍是无条件的，与改后的决定并列矛盾。设计见 ADR-031 决定 16。

## 决定

1. **修正不一致**：`contracts/requirements.json` 的 REQ-036 三处一起改，使受锁的行为、失败模式与决定一致。
   - 行为 3 由“Run终态为COMPLETED/CANCELLED/FAILED且无出边；COMPLETED只能来自VERIFYING；PAUSED只来自PAUSE_REQUESTED；CANCELLED只来自CANCEL_REQUESTED；恢复回到resume_state并先reconcile。”改为“Run终态为COMPLETED/CANCELLED/FAILED且无出边；COMPLETED只能来自VERIFYING；PAUSED只来自PAUSE_REQUESTED；CANCELLED只来自CANCEL_REQUESTED；恢复（ResumeRun）先reconcile，该停止状态自己的停止条件不再成立时回到resume_state，阻塞集合非空时回到BLOCKED（CR-023）。”
   - 失败模式在末尾加一条：“当ResumeRun时该停止状态自己的停止条件仍成立（INCOMPLETE的预算或期限仍耗尽、WAITING_EXTERNAL没有到期的Task且仍有Task在等待next_retry_at、BLOCKED的阻塞集合非空）时，返回409 ILLEGAL_TRANSITION并在details写明是哪一项，不写任何行，state/revision不变（CR-023）。”
   - 第 2 条决定由“INCOMPLETE是否终态未定；本选择终态仅COMPLETED/CANCELLED/FAILED，INCOMPLETE/BLOCKED/WAITING_EXTERNAL/PAUSED可经ResumeRun回到resume_state。”改为“INCOMPLETE是否终态未定；本选择终态仅COMPLETED/CANCELLED/FAILED，INCOMPLETE/BLOCKED/WAITING_EXTERNAL/PAUSED可经ResumeRun回到resume_state：先reconcile；该停止状态自己的停止条件仍成立时拒绝（ILLEGAL_TRANSITION，说明是哪一项），不写任何行——INCOMPLETE为使它停下的预算或期限仍耗尽，WAITING_EXTERNAL为没有到期的Task且仍有Task在等待next_retry_at，BLOCKED为阻塞集合非空，PAUSED没有停止条件；否则回到resume_state，阻塞集合非空时回到BLOCKED（CR-023）。”

   例外边界恰是 pace 自己维持这些状态的谓词（REQ-042 行为 2、6；阻塞集合的规则），恢复不越过它们，也不另设新的条件。

2. **补齐承接**：`contracts/development-tasks.json` 的 M2-T13 的 done_when 末尾加“ResumeRun 从 PAUSED、INCOMPLETE、WAITING_EXTERNAL、BLOCKED 按 REQ-036 第 2 条决定恢复（ADR-031 决定 16）；回到工作态之后的继续由停滞探测保证（ADR-031 更正 C）；`hoh -p … --resume` 到 INCOMPLETE 的 Run 在 SendInput 之后提交 ResumeRun（CR-023）。”

3. `contracts/development-tasks.json` 的 M2-T11 的 done_when 末尾加“对 CR-023 给 M2-T13 加的 ResumeRun 条款给出 PASS/FAIL/BLOCKED/NOT_RUN 与原始证据（它没有 AC：证据是 M2-T13 的支撑测试在同一冻结候选上的结果，由独立会话复核）（CR-023）。”

不在锁内、随视图重新生成：docs/02-functional-spec.md（REQ-036 的行为、失败模式与决定；docs/18-traceability.md 不含 done_when，不变）。

## 影响

- 实现（M2-T13，ADR-031 决定 16，实施切片 S1）：`resume_reconcile` 不再写死 PAUSED，按 Run 的实际状态记 `from`；各停止状态的停止谓词在 ResumeRun 的事务里以 host 的时钟求；Run 迁移表加 INCOMPLETE→BLOCKED（hoh-core 的迁移表与 ADR-011，不在锁内）；`hoh -p … --resume` 到 INCOMPLETE 的 Run 在 SendInput 之后提交 ResumeRun。依赖本 CR 的 ResumeRun 行为在本 CR 批准并 relock 之前不合入（ADR-031 S0）。
- 测试：ADR-031 测试计划的“ResumeRun 从四种停止状态”与“`--resume` 到 INCOMPLETE 与 FAILED、CANCELLED”两组支撑测试；M2-T11 在同一冻结候选上由独立会话复核它们。
- TEST-036-01 对照的迁移表多出 INCOMPLETE→BLOCKED：它的必拒对不含这条边（lifecycle/tests.rs 的必拒对列表），合法边计数随之更新（ADR-031“实施时受影响的既有测试”）。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 不放宽任何门禁、检查或证据规则；只多一项要独立核验的义务（M2-T11）。
- 修正后的决定不给恢复加新的前提：它写明的停止条件都是受锁的 REQ-042 与阻塞集合规则本来就有的，恢复照字面执行也会立即被它们停回。
- AC 的条数与文本不变（行为 3 与新失败模式的验证落在 M2-T13 的支撑测试与 M2-T11 的复核上，见决定 3）。
- 不改 CONTRACT 的架构与交互决定；不改协议、SQL、schema 或 openapi（ResumeRun 已在命令集合中）。

## 是否属于 CR-003 D1 预授权

决定 1 属第 1 条的“修正规格之间的不一致”（REQ-036 的恢复决定与 REQ-042、阻塞集合规则之间）；决定 2、3 属第 1 条的“补齐缺失”（受锁需求已要求、却没有承接任务的行为）。都满足第 2 至 4 条（见“不变的部分”）。ADR-031 的设计审查已判定它属细化（第 2、3 轮 Codex 判 CR-023 的范围属 CR-003 D1；第 6、7 轮 Grok 核对它写明的停止条件就是 REQ-042 与阻塞集合的既有谓词，十一条 AC 都不要求被它拒绝的恢复）；仍由一次独立审查判定后批准。

## 验证方式

独立审查判定属细化后批准；改 requirements.json 的三处与 development-tasks.json 的两条 done_when，`python3 tools/gen_spec_views.py`，`python3 tools/baseline.py relock --cr docs/cr/CR-023-resume-from-stop-states.md`，`bash scripts/check.sh --bundle` 通过（含 validate_bundle 的依赖与归属规则）；M2-T13 的支撑测试证明实现符合，M2-T11 独立复核。
