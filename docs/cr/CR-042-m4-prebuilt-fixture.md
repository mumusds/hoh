# CR-042 · M4 预建、来源绑定的百万事件 fixture

ID：CR-042
状态：DRAFT
批准人：项目所有者 2026-11-28 选定选项 A（决定记录见“依据”）；条文待独立 Review，之后由主会话置 APPROVED 并 relock
日期：2026-11-28
关联：REQ-064（AC-064-01 / TEST-064-01），CR-028 §5.1（CR28D-205、CR28D-301、CR28D-303、C5、预算表），M4-T03、M4-T10

## 背景与依据

CR-028 §5.1 要求每次正式 `integration` 检查（3600 s，工作阶段合计 ≤3000 s）在自己的临时根中，用已记录的 pre-003 产品提交（pin）构建 release 旧 writer，由它的生产写入路径造出 002 百万事件库，`fixture_build` ≤600 s，并且不复用别次检查造的库（CR28D-301、CR28D-205、预算表）。C5 规定：五阶段装不进 3600 s 且留 600 s 余量时，CTO 须以正式 CR 重规划 required check，降阈值或缩范围交项目所有者决定。

M4-T03 的 C5 可行性探针 R0/R0b 实测了这一条（只读引用，未在本任务中重跑）：

- 记录：分支 `task/M4-T03-release-r0` 的提交 `c361ada9e3715fe8e503336395fed75d477b846b`（“M4-T03: R0b CALLS.md: the team file, adapter.item at attempt ends, COMPLETED's gate and the full-scan cost (REQ-064)”）中 `scripts/product/m4measure/fixture/CALLS.md` 的 “Cost” 一节；`/home/ai/dev/hoh-wt/m4t03-r0/.hoh-dev/self-tests/m4t03/r0b-spike-summary.json`（sha256 `eaf5f8ffe7e43d62315a4487042c7c4b1d1e001a58b90422e9698a9fcf8235eb`）与同目录 `r0b-cpu4-spike-summary.json`（sha256 `a294a8ac658df681d63de32986b836c20488cb98d400d219902cf7658cdbb79f`）。
- pin：`cc2c2df6fb2d5dccfdefc0af5d4170093f2223c2`（“FLK-17: review record (REQ-042)”），tree `0a76ea48c6cef443956de61a17e5dc9a9c57c59d`，`contracts/sql/002_spec_drafting.sql` sha256 `950b4e57403d8fca46625ff9def64987f0494134e1884b60d266ccab7174865b`，`Cargo.lock` sha256 `1e745cf42c9f5b03d83a425ebfd93b11e0bae5d1c68174ec42e9f025db0d0098`（本任务从仓库对象重算，与探针记录一致）。
- 根因：pin 的旧 writer 每个 iteration 约有 68 条语句扫描整张 `events` 表（按 `kind` 与 `json_extract(payload_json, …)` 过滤、不带 `run_id`），而 pin 的 `events` 只有 `(project_id, seq)` 与 `(run_id, kind, seq)` 两个索引（pin 的 `contracts/sql/001_initial.sql:51-52`），EXPLAIN QUERY PLAN 为 `SCAN events`。每行约 100 ns，即每个 iteration 对库中已有的每条事件约 7–8.5 µs；事件在真实 iteration 内产生，所以造库时间随事件数平方增长。
- 实测（12 vCPU 开发 VM，release 构建）：10/20/40 个 Run（每 Run 10 iteration，同比例事件与对象）分别 68/241/788 s；按 20/40 两点拟合，全规模约 4,270 s；即使按 40 Run 线性外推也约 1,970 s。4 CPU 探针 10 Run 为 64 s（12 vCPU 为 68 s），不受 CPU 限制。构建顺序的调整已去掉可避免的成本；旧 API 没有绕开这些扫描的调用，旧 schema 没有能服务它们的索引。
- 结论：检查内 `fixture_build` ≤600 s 不可能满足，除非用 SQL 灌库、仿造旧 writer 或让候选 writer 产出 002，而 CR-028 禁止这三者。

主会话的决定说明 `/home/ai/dev/hoh/.hoh-dev/m4t03/owner-decision-fixture-build.md`（sha256 `a2c3f43c9e22c7c02b25f2ff417c9a322bfcf0cb02d6e75968bc94f14e9daa38`）列出 A（预建、来源绑定的 fixture）、B（放宽检查时限）、C（缩小 fixture）三个选项。项目所有者于 2026-11-28 选择 **A**；B 放宽 CR 明确不得放宽的门禁，C 缩小 AC-064-01 的规模要求，均未采用。

## 决定

1. **检查外预建。** 002 百万事件 fixture 由 M4-T03 交付的一个入库命令（下称准备命令）在 `integration` 检查之外构建。规模与写入规则不变：100 Run × 10 iteration；1,000,000 条事件由 pin 的生产事件路径在真实 iteration 内产生；10,000 × 16 KiB 已登记对象；关轮与引用真实可核对；CR28D-301 的 writer 规则全部照旧：不写 SQL，不仿造旧 writer，不让候选 writer 降 schema，每批 ≤500 行且 ≤1 MiB，真实 ACK/fsync，持久性 PRAGMA 不变。
   - 来源：从已记录的 pin 构建，旧树用 `git archive` 展开，用它自己的 `Cargo.lock` 与 toolchain 构建 release 旧 `hoh` 与造库驱动。
   - 位置：在参考项目盘（与 §5.1 测试根同一 mount source/FSType）上它自己的隔离根中运行，不用 `/tmp` 或其他 tmpfs。
   - 锁：与其他 cargo/测试负载一样经外部包装器取**共享**测量锁，不取独占测量锁。它的耗时只记录、不设门，因此不能让本机其他测试停顿约 70 分钟；按同一理由它也不长时间占用开发测试槽。持共享锁期间独占的测量无法开始，这正是 CR28D-303 对普通负载的约定。
   - 计时：命令记录自己的耗时（无上限；只记录，不是门禁）。
   - manifest：命令写出一份 manifest，至少包含：pin 的 commit/tree；`sql_002` 与 `Cargo.lock` 的 sha256；旧 `hoh` 与造库驱动二进制的 sha256 及构建 argv，造库驱动源文件的 sha256；helper 的 sha256 及其 SQLite 版本与 source id；fixture 参数；干净关闭（不留 WAL）后数据库文件的 sha256；逐对象 sha256 清单的摘要；读回结果：`user_version=2`、无任何 003 对象、权威表行数、最大 seq、水位、终态（没有可调度的 Task、没有未结束的 Attempt、Run 全部为终态）；命令自身耗时与造库驱动的批次报告（每批行数/字节的实际上界）。
   - fixture 根保存数据库、对象文件、旧 `hoh` 与造库驱动二进制、逐对象清单、驱动报告与命令的原始日志；manifest 写出后它们都不再修改。
2. **摘要冻结入环境。** 候选冻结前，manifest 的 sha256 写入 `.hoh-dev/environment.json` 的 `m4_reference.fixture`（CR-028 §5.1 已为 m4_reference 规定 fixture 项），candidate.environment 的摘要因此绑定它。pin、造库驱动、helper 或 fixture 参数任一变化，下一次冻结前必须先造出新 fixture 和新 manifest，并更新 `m4_reference.fixture`。fixture 在 manifest 写出后从不修改：检查只读打开它，升级与回填只作用于本次检查的克隆。manifest 的查找位置由 T03 确定并记录；身份只由摘要判定，不由路径判定。
3. **检查内 `fixture_verify` 取代 `fixture_build`。** 五个串行阶段为 `debug_integration`、`release_build`、`fixture_verify`、`backfill`、`measure`，`fixture_verify` 位于原 `fixture_build` 的位置，上限 ≤600 s，计入 3000 s 工作预算；冻结的 m4_reference 为它给出正整数秒时限，CR28D-205 的强制截止与 `PHASE_TIMEOUT` 规则同样适用。阶段内依次：
   - manifest 的 sha256 等于 `m4_reference.fixture`；
   - 核对 manifest 记录的每一个 hash：pin 的 commit/tree 及 `sql_002`、`Cargo.lock` 摘要按仓库对象重算，并等于 `m4_reference.old_schema_producer`；fixture 根中旧 `hoh` 与造库驱动二进制的 sha256；候选树中造库驱动源文件的 sha256；本次 `release_build` 构建的 helper 的 sha256 及其 SQLite 版本与 source id；数据库文件 sha256；逐对象重算的清单摘要；fixture 参数等于本 CR 第 1 条的规模；
   - 用 helper 重新读回并与 manifest 的读回逐项相等；
   - 用随包 helper 做出本次检查所需的一致性备份克隆，每个克隆 `PRAGMA integrity_check` 为 `ok`。

   任一不符、缺 fixture 或缺 manifest 都是 **FAIL**，不是 SKIPPED：受影响的 TEST 为 FAIL、整次非零退出，detail 写明不符的项及期望值/实际值（缺失时写查找位置），随后进入清理，剩余阶段不借用余量。`m4_reference.fixture` 缺失与其他冻结时限缺失一样在启动前 FAIL。manifest 副本与逐项核对结果写入本次检查的证据目录（`m4-environment.json` 引用其相对路径与 sha256），不引用检查外的路径作为本次证据。
4. **`release_build` 的内容。** `release_build` 串行构建候选（`hoh` 与 `m4_measure` example）及 helper。旧树的构建移入准备命令，其二进制 hash 记在 manifest 中；检查不再构建旧树。
5. **不变的内容（明确列出）。**
   - `backfill` ≤600 s，从候选第一次打开克隆起计，两个克隆（未中断与回填中 kill）共享这 600 s；kill 窗口 `0 < indexed_through < H`；kill→reconcile 与 kill→host-ready 各 ≤30 s；首次 ready ≤30 s。
   - 检查超时 3600 s，工作阶段合计 ≤3000 s，至少 600 s 余量（清理 ≤300 s，调度余量 ≥300 s）。
   - 全部样本数与阈值；AC/TEST 集合、check 归属与 owner；D4 资源包络；两锁（开发测试槽与 D4 测量锁）及其顺序、继承与贯穿范围（只是阶段名换为 `release_build`、`fixture_verify`）；噪声准入。
   - C5 对 T05、T07、T10 的重测规则适用于含 `fixture_verify` 的五阶段：每个冻结候选重测全部五阶段，不复用别次时长。
   - 旧生产者仍是版本绑定的 fixture 来源，不能把其行为记为候选 PASS；M4 的 public_segments 只在升级后生成；debug 小库功能测试与迁移故障 seam 不变。
6. **检查不再证明的内容及其理由。** 检查不再证明百万库是在同一次执行中造出的。代之以：fixture 的来源经 manifest 摘要、冻结环境与 candidate.environment 的摘要链绑定，并在每次检查中由 `fixture_verify` 按字节重新核对。可接受的理由：
   - 被测对象是候选对 002 库的升级、回填、查询与界面，不是旧 writer；旧 writer 的造库耗时是 pin 的性质，不是候选的产品阈值，每次检查重造它不增加任何关于候选的证据，却让检查无法执行；
   - 候选面对的输入就是准备命令造出并记录的那份字节：hash、读回与 `integrity_check` 在本次执行内核对，规模、来源与写入规则与同次造库相同；摘要冻结在候选绑定的环境中，后来的检查不能悄悄换用另一份库；
   - 准备命令仍受 CR28D-301 的全部 writer 规则约束，其记录（manifest、驱动批次报告、原始日志）随 fixture 保存，供审查与 QA 核对。

   余项：生成过程本身（库确实由旧 writer 写出）由准备命令及其记录证明，不再由检查内观察证明；检查只证明字节身份。fixture 不保证可逐字节重造（含 UUID 与时间），独立复核时比较读回而不是数据库 hash。

## 对 CR-028 条文的替代

CR-028 文件不改（已重锁的 CR 不可修改）；本 CR 批准后，下列条文按本 CR 读，其余条文不变：

| CR-028 位置 | 原表述 | 按本 CR |
|---|---|---|
| §5.1 预算表“百万事件夹具、迁移回填”行 | 构建≤600s | `fixture_verify` ≤600 s；预建耗时只记录 |
| §5.1 预算表“本检查总预算”行 | 实测 release 构建、造库、回填……保留造库/回填各600s上界 | 实测阶段为 `release_build`、`fixture_verify`、回填……；保留 `fixture_verify`/回填各 600 s 上界 |
| CR28D-205 | 五个时限 debug_integration、release_build、fixture_build、backfill、measure；fixture_build/backfill 各≤600s | `fixture_build` 换为 `fixture_verify`，其余不变 |
| C5 | 先实测上述五阶段（含两次 release 构建） | 先实测含 `fixture_verify` 的五阶段；`release_build` 构建候选与 helper |
| CR28D-301 | 每次正式 integration 展开该提交并构建旧 release HoH 与造库驱动，与候选 release 一并计入 release_build；记录源码树、构建 argv 与 hash；不复用别次检查的成品 | 旧树由准备命令构建，源码树、构建 argv 与二进制 hash 记入 manifest，检查以 manifest 摘要绑定、由 `fixture_verify` 核对；候选 release 仍在本次 `release_build` 构建，不复用别次检查的成品；选定并记录 `old_schema_producer`、驱动只调用旧版本生产写入接口的要求不变；“不能满足预算”只指检查内各阶段的预算 |
| §5.1 fixture_build 一段 | fixture_build 从旧 writer 首次建库到一致性备份完成≤600s；读回后用产品一致性备份克隆；百万002库在这次检查只造一次 | 造库在检查外，读回与 hash 记入 manifest；检查内 `fixture_verify` 核验后用随包 helper 做一致性备份克隆（不只拷数据库主文件）；场景克隆只在同次 candidate/spec/environment/fixture 摘要下复用 |
| CR28D-303 | 两锁贯穿 debug、两个 release 构建、造库、回填、measure | 两锁贯穿 debug、`release_build`、`fixture_verify`、回填、measure；准备命令只取共享测量锁 |
| “批准后拟应用的文件”中 check-plan 一行 | 同次旧/新release构建和真实002造库/计时 | 候选 release 构建、`fixture_verify` 与计时；造库由准备命令完成 |

AC-064-02 的“§5.1同次百万库克隆”措辞不改，指本次检查 `fixture_verify` 从预建 fixture 做出的克隆。

## 受锁文件的改动

只改写“百万库在同一检查中造出”“由同次构建的旧 release writer 造库”“造库≤600s”“release_build 含两次 release 构建”的子句，其余字词不变：

- `contracts/requirements.json` AC-064-01 的 given 与 then；
- `contracts/acceptance-scenarios.json` TEST-064-01 的 fixture 与 pass_criteria（同一文字）；
- `contracts/development-tasks.json` M4-T03 与 M4-T10 的 done_when（不在基线锁内，不推进状态）；
- 由 `python3 tools/gen_spec_views.py` 重新生成 `docs/02-functional-spec.md`、`docs/18-traceability.md`。

不改产品代码、检查计划（`tests/check-plan.json`）、release-policy、门禁工具与其他 AC；REQ-064 的 behavior/failure_modes/decisions 不含被替代的条文，保持不变。

## 影响

- M4-T03：交付准备命令（含 manifest 写出与读回）、`fixture_verify` 及其失败记录，`release_build` 只构建候选与 helper；冻结前在 m4_reference 中填 `fixture` 摘要与 `fixture_verify` 时限。T03 可以现在用真实旧 writer 造一次库（约 70 分钟），再做其余阶段的 C5 实测。
- M4-T05、M4-T07、M4-T10：重测含 `fixture_verify` 的五阶段；fixture 的身份变化（pin、驱动、helper、参数）先换 fixture 与 manifest 再冻结。
- `.hoh-dev/environment.json`：冻结前新增 `m4_reference.fixture`；不修改已冻结的环境。
- 不改 14 项检查的归属、mode、超时，不改 release-policy 或 gate。

## 是否属于 CR-003 D1

否。本 CR 去掉检查内“造库 ≤600 s”与“同次构建”的证明要求，改变了检查所证明的内容，属于证据规则的变化，因此需要项目所有者批准；所有者已于 2026-11-28 选择选项 A。所有产品阈值、样本数、AC/TEST 集合、检查时限与余量都不变；不删除需求或 AC，不改变 CONTRACT 的架构与交互决定。

## 验证

本分支 CR 保持 DRAFT，只改受锁文件与生成视图，不 relock；分支上的 `bash scripts/check.sh --bundle` 预期报基线不符（受锁文件已改而锁未更新），如实记录，不通过修改基线消除。证明在本任务用 `mktemp -d -p /home/ai/dev` 创建并登记的 scratch 副本中进行：临时把状态改为 APPROVED，运行 `python3 tools/baseline.py relock --cr docs/cr/CR-042-m4-prebuilt-fixture.md` 与 `bash scripts/check.sh --bundle`，记录退出码后删除副本。命令与退出码记录在 `.hoh-dev/self-tests/cr042/`；`bash scripts/dev/pre-pr.sh` 在分支上前台运行。独立 Review 与批准由主会话安排。
