# CR-007 · 调度参数的配置与快照、租约占用与重复任务的原因码

ID：CR-007
状态：PROPOSED
批准人：（待独立 Review 后按 CR-003 D1 预授权批准）
日期：2026-11-06
基线：HOH-BASELINE-1.1 revision 7 → revision 8

## 背景

M2-T03（任务 DAG 调度、租约 CAS 与 epoch fencing）实现 AC-035-02、AC-037-01、AC-038-01..06 时，发现三处规格缺口。

1. **调度权重无处配置，也无法提交变更。**
   - REQ-038 b7 要求权重与参数写入 Run 配置快照；活跃 Run 中不得修改，变更只对新 Run 生效。
   - AC-038-05 与 REQ-038 failure_modes 4 要求：对活跃 Run 提交权重变更时返回 409，快照不变。
   - 但 REQ-018 的完整配置键表（未知键一律拒绝）没有调度参数，`config/hoh.example.toml` 也没有。
   - 也没有任何命令能携带调度参数：ConfigureTeam 的 payload 只有 `bindings` 与 `apply_at`（additionalProperties=false）。
   - 快照里参数放在哪个键下，同样没有规定（`runs.config_snapshot_json` 默认为 `{}`）。
2. **写租约被占用时，派发提议没有拒绝原因码。**
   - REQ-037 b2 与 failure_modes 0 规定，未获写租约的 Task 保持 READY，CAS 失败。
   - AC-035-02 规定写租约在派发提议的同一事务中取得。
   - 因此 Commander 在写租约被占用时提议派发写任务，提议必须 REJECTED。可是 `RejectionReason` 的 12 个值里没有对应的码；用 ILLEGAL_TRANSITION 会误导（迁移本身合法）。
3. **重复 task_id 没有错误码。** REQ-038 b1 规定，计划中有重复 task_id 时整个计划拒绝。环与缺失依赖各有 DAG_CYCLE、DAG_MISSING_DEPENDENCY，重复却没有。

## 决定

| # | 内容 |
|---|---|
| S1 | REQ-018 的完整配置键表与 `config/hoh.example.toml` 增加：<br>• `[scheduler] max_wait_rounds = 20`（整数 1..1000）；<br>• `[scheduler.weights] user_value = 4、unblock_value = 3、risk_reduction = 3、information_gain = 2`（各为整数 0..10）。<br>默认值与 docs/07 及 CR-002 的初值相同。age_bonus 截断在 0..10，缺估计的 cost 取中位桶，二者是 REQ-038 的固定规则，不作为配置键 |
| S2 | Run 配置快照 `runs.config_snapshot_json` 的 `scheduler` 键，形状与 S1 相同：`{"weights":{"user_value":4,"unblock_value":3,"risk_reduction":3,"information_gain":2},"max_wait_rounds":20}`。<br>• StartRun 时取当时的有效项目配置写入。<br>• 快照缺这个键或缺某一项时，按 S1 的默认值读（StartRun 实现之前建立的 Run、测试 fixture）。<br>• 每一轮调度的事件 payload 记下本轮所用的权重与 max_wait_rounds |
| S3 | ConfigureTeam 是唯一修改配置的命令（REQ-018；team_revisions 是项目配置的历史）。它的 payload 增加可选的 `scheduler`，形状与 S2 相同，各项可选，至少一项，不允许其他键。<br>• payload 必须含 `bindings` 或 `scheduler`，二者可同时出现；`apply_at` 不变。<br>• 带 `scheduler`、且 `run_id` 指向一个已存在的 Run 时（不论 Run 处于什么状态，快照从不改变）：返回 409 `RUN_CONFIG_FROZEN`，retryable=false，current_revision=null，details.key_path="scheduler"。整个命令被拒，不写任何东西（ADR-016 决定 4：只有被接收的命令才有 commands 行），快照与之后的 score 仍按原权重计算。<br>• `run_id` 指向不存在的 Run：409 RUN_NOT_FOUND（与其他命令一致）。<br>• `run_id` 为 null：形成新的项目配置 revision，只对之后启动的 Run 生效。这部分随 team revision 一起由 M2-T07 实现；在那之前与其他未实现的 ConfigureTeam 一样，返回 422 CAPABILITY_UNSUPPORTED |
| S4 | `RejectionReason` 增加 `LEASE_UNAVAILABLE`。派发提议在所需租约被占用时被拒，Task 保持 READY，tasks 与 leases 不变，只写一条拒绝事件。<br>• 写任务所需的写租约被占用，指的是：未过期，或者持有它的 Attempt 尚未结束（REQ-037 b7：授予新租约前必须确认上一持有者已结束或已被 fence）。<br>• 读槽全部占用时同理 |
| S5 | `ErrorCode` 增加：<br>• `DAG_DUPLICATE_TASK`：计划中同一 task_id 出现多次，或与该 Run 已有的 Task 重名；details 为 `{task_id}`。整个计划拒绝，不部分入库。<br>• `RUN_CONFIG_FROZEN`：见 S3，HTTP 409 |

REQ-018 decisions 5、REQ-037 decisions、REQ-038 decisions 中相应补记（初值）；docs/05 与 docs/07 中补一句说明。

## 批准依据

符合 CR-003 D1 预授权的四条：

1. 只补齐缺失的配置键、字段与原因码，并修正 REQ-038/AC-038-05 与 REQ-018、协议 schema 之间的不一致（规格要求提交配置变更，却没有可提交的键与命令）。
2. 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
3. 不放宽任何门禁、阈值、检查或证据规则。AC 的通过条件不变，原有的拒绝路径也没有放宽。
4. 不改变 CONTRACT 的架构与交互决定。命令是 docs/05 中列出的内部操作；这里没有新增命令类型，也没有新增 CLI 子命令或交互入口。

## 考虑过的其他方案

- **新增命令类型 `ConfigureScheduler`。** 需要改 `commands.kind` 的 CHECK，docs/05 的操作列表也要改。而且它针对某个 Run 的形式只可能失败，只有针对项目的形式才有意义，这正是 ConfigureTeam 已有的含义。故不采用。
- **只改配置文件、不经命令。** 这样 AC-038-05 的“提交变更返回 409”无从发生，也就无法被观察。故不采用。

## 影响

- `contracts/protocol.schema.json`：ConfigureTeam payload；`RejectionReason`；`ErrorCode`。
- `contracts/requirements.json`：REQ-018 decisions 5；REQ-037、REQ-038 各补一条 decision。
- 不受锁的文件：`config/hoh.example.toml`、docs/05、docs/07；`crates/hoh-core/src/protocol.rs` 的枚举。
- `docs/02`、`docs/18` 由 `tools/gen_spec_views.py` 生成。

## 不变的内容

- 需求与 AC 的数量，各 AC 的 given/when/then 与场景的步骤、通过条件、观察工件。
- SQL（`commands.kind` 的 CHECK 不变）、openapi（Error.code 通过 ReasonCode 引用协议枚举）、release-policy、门禁工具。
- baseline_id 仍为 HOH-BASELINE-1.1。

## 验证

- `cargo test -p hoh-core`：schema 与 Rust 枚举一致；语料中带 scheduler 的 ConfigureTeam 能通过校验，不合规的会被拒（未知键、越界、空对象、两者都缺）。
- `python3 tools/gen_spec_views.py` 之后，docs/02 与 docs/18 与契约一致。
- `python3 tools/baseline.py relock --cr docs/cr/CR-007-scheduler-config-and-lease-codes.md` 之后，`python3 tools/baseline.py verify` 通过。
- `bash scripts/check.sh --bundle`。
- M2-T03 的 TEST-038-05 断言 409 RUN_CONFIG_FROZEN 与快照不变；派发测试断言 LEASE_UNAVAILABLE；TEST-038-03 之外另测 DAG_DUPLICATE_TASK。

## 审查记录

（待独立 Review）
