# CR-003 · 打通自主开发堵点：任务图、门禁、M2 规格缺口与细化 CR 预授权

ID：CR-003
状态：APPROVED
批准人：项目所有者（2026-11-03 在 Claude Code 会话中批准“全部同意，按推荐执行”：本 CR 的范围、预授权规则与测试基础设施授权）
日期：2026-11-03
基线：HOH-BASELINE-1.1 revision 3 → revision 4

## 背景

M1 开工前做了一次三路只读审查（规格一致性、门禁工具、计划可行性，详见 [docs/23 审计报告](../23-pre-m1-audit.md)）。结论是 M1 大体可写，但有三类问题会让无人值守开发卡住或走偏：

1. **流程堵点。** M3–M7 的 5 个 spec 任务都需要用户批准 CR，但文档没有预授权规则，同时又要求“不等用户确认”。任务图有几个任务的完成条件依赖排在它后面的能力（M1-T06、M1-T08、M2-T02），v0.1 依赖被推迟的 Codex 适配器与团队配置，HTTP/SSE API 在 M6 之前没有任何任务负责，deployment 检查排在 v0.1 的 QA 之后。工具上，Pi 当不了独立 QA（会话 ID 抓不到），瞬时重试会把已完成的会话判为失败，独立报告里的哈希 Agent 自己无法得知。
2. **M2 规格缺口。** SQL、协议 schema、openapi 缺少 M2 AC 直接断言的字段，有几套词汇互相矛盾（重放返回 200 还是 202、`stale_epoch` 与 `STALE_ROLE_EPOCH`、Run 级 BLOCKED 的含义等）。不先补齐就只能猜。
3. **门禁漏洞。** 同一份日志可以给某检查下的所有 AC 当证据；live 检查不核对实际用了哪些 Agent；发布产物可以是任意非空文件；独立报告不是检查的声明产物，检查结束后仍可改写。

## 决定

| # | 决定 | 影响 |
|---|---|---|
| D1 | **细化 CR 预授权规则。** 满足以下全部条件的 CR，由 CTO（主开发会话）在独立 Review 判定 PASS 后批准，状态写 APPROVED，批准人写“CTO 依 CR-003 D1 预授权（独立 Review 会话 id）”：①只把 outline 细化为 detailed，或修正不一致、补齐缺失字段；②不删除任何需求或 AC，不把 required 改为 degradable，不缩小 release_targets；③不放宽任何门禁阈值、检查或证据规则（加严可以）；④不改变 CONTRACT 中的架构与交互决定。超出这些条件的 CR 仍须项目所有者批准。项目所有者可随时否决已批准的 CR，否决后按新 CR 回退 | M3-T01…M7-T01 不再因等待审批而停工；docs/21 记录该规则 |
| D2 | **v0.1 在 M2 全部完成后发布。** v0.1 的 AC 实际依赖 Codex 适配器（AC-004-04 回放 Codex 鉴权失败）、团队绑定与 role_epoch（AC-035-01、AC-036-03、AC-054-01）、Commander 监督（AC-002-01 的 NEED_DECISION）。v0.1 的需求范围（18 项）与门槛不变，只是发布点后移到 M2 之后 | V01-T01 依赖 M2-T11；M3-T01 依赖 V01-T02 |
| D3 | 测试基础设施授权：允许用 sudo 安装 Playwright 依赖与浏览器、strace、openssh-server、qemu/KVM，把开发用户加入 kvm 组，并启用 user linger | 部署、PTY、远程与 soak 检查有可执行环境 |

## 修正清单

**任务图（contracts/development-tasks.json，不在锁内）**

- 任务增加 `acceptance`（负责的 AC）、`checks`（落地的检查）与 `profile`（v0.1/v1 收口任务）字段；validate_bundle 强制：每条 detailed AC 恰好归属一个实现任务；任务所需检查由它自己或祖先任务落地；带 profile 的任务以该 profile 全部 AC 与检查的归属任务为祖先；占位检查命令里写的任务名必须是检查的归属任务。用旧任务图回测，规则报出 36 处违规。
- 新增任务：M0-T07（本 CR）、M2-T12（Engine host 骨架与本地命令/查询 API）、M2-T13（CLI/TUI 端到端）、M2-T14（安装与部署、deployment 检查）、M2-T15（能力匹配与缺员）、M2-T16（QA/Review barrier）、M2-T17（角色交接）、M2-T18（结果投影与独立报告）、M2-T19（重试与预算）。共 58 个任务。
- M1-T06 只负责参数解析与退出码映射，端到端 AC 移到 M2-T13；M1-T08 负责 TUI 外壳与 tui-pty 检查；AC-036-02/03 从 M2-T02 移到 M2-T05/M2-T16；M1-T04、M1-T05 依赖 M1-T03（adapter-live 检查）；Mx-T01 的完成条件增加“按 ≤2 项需求、≤8 条 AC 拆分本里程碑实现任务”。

**工具**

- `tools/agent_task.py`：识别 Pi JSON 模式首行 `{"type":"session","id":…}`；只按终态判失败，Codex 的 `error`（Reconnecting）与 Pi 的自动重试只计入 `transient_errors`；新增 `--report-out`，会话结束后由桥从最后一条消息的 ```hoh-report 代码块生成独立报告，session_id、candidate_id、spec_sha256 与两个哈希由桥填写，Agent 自己写这些字段会被拒绝。
- `tools/taskboard.py`：台账放在主 worktree，链接 worktree（如 soak 检出）共享同一台账；`next` 在已有写任务运行时不再推荐写任务。
- `tools/run_check.py`（受锁）：子进程额外得到 `HOH_SPEC_SHA256` 与 `HOH_CANDIDATE_FILE`。
- `tools/baseline.py`（受锁）：`relock --add` 把新契约文件纳入锁。

**门禁（tools/release_gate.py、contracts/release-policy.json、docs/10，受锁；全部是加严）**

- 逐测试证据：每条 AC 的场景检查必须产出 `test-results.jsonl`（新 schema `contracts/test-result.schema.json`），其中至少有一条该 AC 的 TEST id 记录，且全部为 PASS（DEGRADED AC 允许 PASS/DEGRADED 且至少一条 DEGRADED）。测试名含 `test_NNN_MM` 即覆盖 TEST-NNN-MM。
- live 检查的 Agent 集合：v0.1 的 adapter-live 必须含 claude 会话；v1 的 adapter-live 必须含 claude、codex、pi，plugin-live 必须含 pi 且每个 Pi 会话列出已加载插件。
- 独立报告必须等于对应检查声明的 `independent-report.json` 产物；发布物必须等于 build 检查声明的 `hoh-release.tar.gz` 产物。

**M2 规格（requirements、scenarios、protocol schema、openapi、SQL，受锁）**

| 问题 | 修正 |
|---|---|
| 重放返回 200 还是 202；202 体无 run_id | 统一为 202，重放逐字节返回首次响应体并带 `Idempotent-Replayed: true`；202 体含 run_id（StartRun 在同一事务分配） |
| AC-005-01/05 的 `hoh -p --detach …` 违反 REQ-003 参数顺序 | 改为 `hoh -p '<目标>' --detach --output json` |
| commands 表无 kind/payload；runs.spec_id 在规格产生前无法非空 | commands 增加 kind、payload_json、accepted_at；runs.spec_id 可空 |
| 候选无状态、无报告表、issue 无法追溯到 finding、无 STALE | candidates.state；independent_reports 表；issues.report_id/finding_id；acceptance_results 增加 STALE 与 reason |
| Run 级 BLOCKED/WAITING_EXTERNAL 含义与原因优先级 | REQ-035 决定：BLOCKED 停 Commander 决策类派发，Runtime 自动规则继续；blocked_reason 取优先级最高项，完整集合写 blocked_reasons_json。REQ-042：只有没有其他可派发 Task 时 Run 才是 WAITING_EXTERNAL |
| 原因码两套写法 | protocol.schema.json 新增 BlockedReason、RejectionReason、ErrorCode、ReasonCode；需求中统一为大写 |
| ContextHandoff 装不下 Commander 交接包 | 增加 handoff_kind（verification/role_change/task）、spec_revision、state；candidate 与 task_id 可空，verification 时仍必填 |
| QA/Review 交接的 epoch 与消息数 | 各自接收角色的 role_epoch；candidate_ready 时恰好 2 条 kind=handoff |
| Claude 取消样例与 CANCELLED 映射矛盾 | 已请求取消一律 CANCELLED，取消后的 result 帧只作诊断 |
| “恰好 1 条未知帧”不可判定 | 每个 CLI 版本维护 known-frames.json 登记表 |
| 鉴权失败样例不存在 | Codex App Server 依 schema 合成（标 synthetic），exec 用 401 录制样例；Pi 按录制的错误文本分类 |
| compat manifest 身份 | 区分 launcher 与实际实现的路径与哈希；新增 contracts/compat-manifest.schema.json |
| supervisor_triggers 一个键只能有一行 | trigger_id 主键，event_seqs_json，同键至多一个 PENDING（部分唯一索引） |
| 调度与 Commander 输入无 schema | tasks 增加评分字段与 layer/wait_rounds；runs.config_snapshot_json；DecisionPayload 与 PlanProposal |
| `--resume` 回答 BLOCKED 的 run 用哪个命令 | SendInput input_kind=REPLY；Runtime 若恰有一个待答问题就作为答案，否则按 STEER |
| 团队配置无存储；attempt 缺 agent/model/失败原因/用量 | team_revisions 表；role_bindings.state/blocked_reason；attempts 增加相应列 |
| 遗留命名（auth_check=failed、CAPABILITY_UNSUPPORTED 作阻塞原因、LEASED attempt） | 改为 REQ-006 枚举、BLOCKED_CAPABILITY 与正确的状态名 |
| 配置键未定义 | REQ-018 列出完整配置键表；hoh.example.toml 同步 |
| Error 不允许额外字段；Event 必须有 run_id | Error.details；Event.run_id 可空 |
| v0.1 的 deployment 与 traceability 检查没有 AC | 新增 AC-005-06（安装/卸载/systemd --user）与 AC-052-05（traceability 逐 TEST id 核对） |

## 不变的内容

70 项需求、release_class、release_targets、v0.1 的 18 项范围、全部门禁阈值（72 h soak、20 个 live 迭代、critical/high 为零）与 CONTRACT 的架构和交互决定都不变。AC 从 164 增至 166，新增的都是更严格的验收。

## 验证

- `bash scripts/check.sh --bundle`：包自检与全部 bundle 测试通过（新增测试覆盖 Pi 会话 ID、瞬时重试、独立报告生成、台账共享与单写者、任务/检查归属、逐测试证据、live Agent 集合、报告与发布物归属、SQL 新约束）。
- 回归测试先在旧实现上确认失败，再在新实现上通过。
- 独立 Review（Codex 新会话）审查本 CR 的全部改动，结论记录在 `reports/reviews/CR-003.md`。
