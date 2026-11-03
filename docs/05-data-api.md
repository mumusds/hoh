# 数据模型与接口契约

机器规范：`contracts/protocol.schema.json`、`contracts/openapi.json`、`contracts/sql/001_initial.sql`。首次实现可扩展字段，但删字段/改语义需schema版本与ADR。

## 身份与职责

Project为规范化repo路径和项目ID；SpecificationRevision为Git规格内容hash；Requirement是稳定ID；AC隶属Requirement；Task是可执行工作；Run是一轮长期目标执行；Iteration是一个可验证增量；Attempt是某角色的一次可重试执行；Session是底座会话；Candidate是冻结工件；Evidence是原始观察；Decision是Commander建议；Issue是需修复的事实；Release是最终工件与证据集合。

一个Run含多个Iteration，每轮含多个Task/Attempt；重试只新增Attempt，不把旧失败覆盖。Session可以续用，但QA与开发、Commander与Reviewer必须不同session identity。配置revision与role_epoch一起绑定到attempt；role_epoch按(run, role)维护（`role_bindings`），更换Commander不会隔离Developer在途结果。每个项目一个数据库（`.hoh/state.db`），因此`events.seq`就是项目内序号。

## 状态与事务

Run: CREATED→DISCOVERING→SPECIFYING→READY→RUNNING→VERIFYING→COMPLETED。任一非终态可进入PAUSE_REQUESTED→PAUSED、WAITING_EXTERNAL、BLOCKED、CANCEL_REQUESTED→CANCELLED、INCOMPLETE、FAILED。恢复回到保存的resume_state并先reconcile。COMPLETED不得因用户新目标重新打开同run；新run引用旧baseline。

Task: PENDING/READY/LEASED/RUNNING/IMPLEMENTED/VERIFYING/ACCEPTED/BLOCKED/CANCELLED。实现完成不代表ACCEPTED。需求验收状态是按(spec_revision,candidate,AC)计算的投影，不仅一个mutable done布尔。

事务原子写：状态update(WHERE revision=expected)、event append、outbox insert。ACK在commit之后。UI绝不能先显示事实成功再异步落盘。日志文本可异步写，但关键事件不得在内存队列满时静默丢弃。

## 标识与消息

使用UUID等不依赖墙钟排序的全局ID；展示短ID仅为UI。event_seq为每项目单调序号，sort依据seq而非timestamp。timestamp用UTC RFC3339；耗时用monotonic clock，重启间不可直接相减，另存UTC/checkpoints并明确测量区间。

HCP envelope包含schema_version、message_id、run_id、task_id、attempt_id、sender_role、recipient_role、role_epoch、correlation_id、causation_id、idempotency_key、expected_revision、spec_sha256、candidate_id、kind、payload。所有命令有ack/rejected，任务有completed/failed，两者不同。

ACK重复：同idempotency_key且同payload hash返回原结果；同key不同payload返回409 KEY_REUSE。消息有效期限不应删除重要结果；超期投递改成dead-letter可诊断。消费去重表以consumer+message_id唯一。

## Command API

StartRun、SetMode、SendInput、ConfigureTeam、ProposeSpecChange、PauseRun、ResumeRun、CancelRun、GetSnapshot、GetEvents、GetArtifact、GetCapabilities为内部操作。/plan/快捷键/Web按钮映射SetMode；不由每个入口重新解释Prompt。

HTTP `/v1/commands` POST接收typed command（含 ApproveSpec、AnswerQuestion 等类型化的 Plan 交互），返回202（已持久接受）含command_id和revision，不能把202理解为任务完成；同key同payload重放返回200且replayed=true。读取 `/v1/runs`、`/v1/runs/{id}` 及其 tasks/acceptance/issues/evidence/team/timeline 分页列表、`/v1/events?after=…&run_id=…`、`/v1/artifacts/{id}`。写操作带expected_revision；冲突409、缺能力422、host不可用503、token或Host/Origin不符403。未知command返回400，不静默忽略。

## 事件流

SSE id=event_seq；先记录high-watermark H，replay(after,H)，然后live>H；缓冲不足时客户端重新replay，不能遗漏窗口。Last-Event-ID与query after同时存在时拒绝矛盾值。保留期外cursor返回明确resync提示和snapshot_watermark，客户端装snapshot后续接。异步日志块不是逐字符都落DB：有界合并token delta，但tool/start/end、decision、acceptance必须完整。

## 规格、证据与备份

Git规格文件权威，DB保存索引及引用hash；索引构建失败保留旧snapshot并标stale，禁止基于半更新索引派发。对象文件写临时、fsync、rename后登记；清理仅删除未被活跃候选/证据/发布引用的对象。

SQLite在线备份API或暂停writer一致性备份 [S22]。升级前backup+integrity_check；迁移可逆与不可逆分开，回滚二进制时如schema更高必须恢复对应备份。禁止复制DB主文件丢WAL。

## Runtime记录与自举证据导出

protocol.schema.json的Candidate是运行时轻量引用（id+Git/spec/environment/test_plan身份）；candidate.schema.json是本开发工具的完整导出清单（candidate_id及source_files/原始ArtifactRef）。二者不是同一个序列化对象。导出器必须显式映射id→candidate_id并保留所有身份；不得把轻量引用当完整源代码校验证据。正式Rust实现可以采用同等或更强导出器，但门禁字段语义必须一致。
