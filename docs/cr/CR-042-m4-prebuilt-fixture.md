# CR-042 · M4 预建、来源绑定的百万事件 fixture

ID：CR-042
状态：DRAFT
批准人：项目所有者 2026-11-28 选定选项 A（决定记录见“依据”）；决定 1“锁”一项是 CTO 在这一选择之下对选项 A 原文 “under the measurement locks” 的读法；条文待独立 Review，之后由主会话置 APPROVED 并 relock
日期：2026-11-28
关联：REQ-064（AC-064-01 / TEST-064-01），CR-028 §5.1（CR28D-205、CR28D-301、CR28D-302、CR28D-303、C5、预算表），M4-T03、M4-T10，ADR-042（M4 release 测量阶段）

## 背景与依据

CR-028 §5.1 要求每次正式 `integration` 检查（3600 s，工作阶段合计 ≤3000 s）在自己的临时根中，用已记录的 pre-003 产品提交（pin）构建 release 旧 writer，由它的生产写入路径造出 002 百万事件库，`fixture_build` ≤600 s，并且不复用别次检查造的库（CR28D-301、CR28D-205、预算表）。C5 规定：五阶段装不进 3600 s 且留 600 s 余量时，CTO 须以正式 CR 重规划 required check，降阈值或缩范围交项目所有者决定。

M4-T03 的 C5 可行性探针 R0/R0b 实测了这一条（只读引用，未在本任务中重跑）：

- 记录：分支 `task/M4-T03-release-r0` 的提交 `c361ada9e3715fe8e503336395fed75d477b846b`（“M4-T03: R0b CALLS.md: the team file, adapter.item at attempt ends, COMPLETED's gate and the full-scan cost (REQ-064)”）中 `scripts/product/m4measure/fixture/CALLS.md` 的 “Cost” 一节；探针摘要 `r0b-spike-summary.json`（sha256 `eaf5f8ffe7e43d62315a4487042c7c4b1d1e001a58b90422e9698a9fcf8235eb`）与 `r0b-cpu4-spike-summary.json`（sha256 `a294a8ac658df681d63de32986b836c20488cb98d400d219902cf7658cdbb79f`）。两处各有一份 sha256 相同的副本：探针工作树的 `/home/ai/dev/hoh-wt/m4t03-r0/.hoh-dev/self-tests/m4t03/`，以及主 checkout 中保留的 `/home/ai/dev/hoh/.hoh-dev/evidence/m4t03-r0b/`。
- pin：`cc2c2df6fb2d5dccfdefc0af5d4170093f2223c2`（“FLK-17: review record (REQ-042)”），tree `0a76ea48c6cef443956de61a17e5dc9a9c57c59d`，`contracts/sql/002_spec_drafting.sql` sha256 `950b4e57403d8fca46625ff9def64987f0494134e1884b60d266ccab7174865b`，`Cargo.lock` sha256 `1e745cf42c9f5b03d83a425ebfd93b11e0bae5d1c68174ec42e9f025db0d0098`（本任务从仓库对象重算，与探针记录一致）。
- 根因：pin 的旧 writer 每个 iteration 约有 68 条语句扫描整张 `events` 表（按 `kind` 与 `json_extract(payload_json, …)` 过滤、不带 `run_id`），而 pin 的 `events` 只有 `(project_id, seq)` 与 `(run_id, kind, seq)` 两个索引（pin 的 `contracts/sql/001_initial.sql:51-52`），EXPLAIN QUERY PLAN 为 `SCAN events`。每行约 100 ns，即每个 iteration 对库中已有的每条事件约 7–8.5 µs；事件在真实 iteration 内产生，所以造库时间随事件数平方增长。
- 实测（12 vCPU 开发 VM，release 构建）：10/20/40 个 Run（每 Run 10 iteration，同比例事件与对象）分别 68/241/788 s；按 20/40 两点拟合，全规模约 4,270 s；即使按 40 Run 线性外推也约 1,970 s。4 CPU 探针 10 Run 为 64 s（12 vCPU 为 68 s），不受 CPU 限制。构建顺序的调整已去掉可避免的成本；旧 API 没有绕开这些扫描的调用，旧 schema 没有能服务它们的索引。
- 结论：检查内 `fixture_build` ≤600 s 不可能满足，除非用 SQL 灌库、仿造旧 writer 或让候选 writer 产出 002，而 CR-028 禁止这三者。

主会话的决定说明 `/home/ai/dev/hoh/.hoh-dev/m4t03/owner-decision-fixture-build.md`（sha256 `a2c3f43c9e22c7c02b25f2ff417c9a322bfcf0cb02d6e75968bc94f14e9daa38`）列出 A（预建、来源绑定的 fixture）、B（放宽检查时限）、C（缩小 fixture）三个选项。项目所有者于 2026-11-28 选择 **A**；B 放宽 CR 明确不得放宽的门禁，C 缩小 AC-064-01 的规模要求，均未采用。

## 术语

- **helper**：M4 release 测量的 SQLite 辅助程序 `m4-sqlite`。它是被跟踪的独立 crate `scripts/product/m4measure/sqlite-helper/`，自成 workspace（`Cargo.toml` 写空的 `[workspace]`，不进仓库 workspace），toolchain 为 `rust-toolchain.toml` 钉住的 1.99.1。唯一依赖是 `libsqlite3-sys = "=0.39.1"`，启用 `bundled` 并保留默认 feature。它的 `Cargo.lock` 中出现的每个外部包，版本与 checksum 都等于 pin 的 `Cargo.lock` 中的条目。构建后 `m4-sqlite version` 须读回 SQLite 3.54.1，以及该 crate 自带头文件中的 source id。它负责只读读回、一致性备份（`sqlite3_backup`）与 `PRAGMA integrity_check`。定义见 ADR-042 决定 3 的“SQLite helper”：分支 `task/M4-T03-release`，提交 `c69651614090210a5148fcaca624cb98c346b04c`（“M4-T03 design: ADR-042 setup proof, spike results and the C5 stop (REQ-064)”，设计第 5 稿）。包本身见“依据”中 `task/M4-T03-release-r0` 的提交 `c361ada9`。
- **准备命令**、**fixture 根**、**构建记录**、**manifest**：见决定 1。

## 决定

1. **检查外预建。** 002 百万事件 fixture 由 M4-T03 交付的一个入库命令（下称准备命令）在 `integration` 检查之外构建。规模与写入规则不变：100 Run × 10 iteration；`events` 至少 1,000,000 行，全部由 pin 的生产事件路径写入，造库驱动的事件在真实 iteration 内产生；10,000 × 16 KiB 已登记对象；关轮与引用真实可核对；CR28D-301 的 writer 规则全部照旧：不写 SQL，不仿造旧 writer，不让候选 writer 降 schema，每批 ≤500 行且 ≤1 MiB，真实 ACK/fsync，持久性 PRAGMA 不变。
   - 来源：从已记录的 pin 构建，旧树用 `git archive` 展开，用它自己的 `Cargo.lock` 与 toolchain 构建 release 旧 `hoh` 与造库驱动。
   - 位置：在参考项目盘（与 §5.1 测试根同一 mount source/FSType）上新建一个隔离根（下称 fixture 根），不用 `/tmp` 或其他 tmpfs。
   - 锁（CTO 在所有者选项 A 之下的读法）：选项 A 原文为 “under the measurement locks”。CTO 将其读作：准备命令从启动到退出，经外部包装器持有**共享** D4 测量锁。这段时间包括构建、造库、读回、manifest 写出，以及它的全部子进程。因此它运行期间，任何独占测量都不能开始，这也是 CR28D-303 对普通负载的约定。它不取开发测试槽，因为它是一次构建与造库运行，不是测试运行。本 CR 其他位置提到准备命令的锁，都按这一条读。
   - 计时：命令记录自己的耗时（无上限；只记录，不是门禁）。
   - 构建记录：命令记录下列四项，相关文件都保存在 fixture 根中。数据库不要求可逐字节重造（其中含 UUID 与时间）。
     - 自身版本：运行它的仓库提交（完整 Git ID；下列被跟踪文件在该提交上没有未提交改动），以及准备命令、造库驱动源文件与 helper 包全部被跟踪文件的 sha256。
     - toolchain：旧树与 helper 构建所用的 `rustc -vV`、`cargo -V` 输出，以及 `cc` 的解析路径与 `cc --version` 输出。
     - 每次构建的 `env -i` argv 与保留的环境变量。
     - 原始构建日志与造库日志：cargo、造库驱动与读回的 stdout/stderr，以及驱动批次报告。
   - manifest：命令写出一份 manifest，至少包含：
     - pin 的 commit/tree；`sql_002` 与 `Cargo.lock` 的 sha256；
     - 旧 `hoh`、造库驱动与 helper 二进制的 sha256 及构建 argv，helper 的 SQLite 版本与 source id；
     - 上述构建记录；fixture 参数；
     - fixture 根的完整文件清单：除 manifest 自身外，每个普通文件的相对路径、字节数与 sha256。清单覆盖数据库主文件、对象文件、逐对象 sha256 清单、二进制、驱动批次报告、构建记录与全部原始日志；
     - 读回结果：`user_version=2`、无任何 003 对象、权威表行数、最大 seq、水位、终态（没有可调度的 Task、没有未结束的 Attempt、Run 全部为终态）、iteration 与关轮数、已登记对象的数量与大小、引用数；
     - 命令自身耗时；驱动批次报告的实际上界（每批行数/字节）与 PRAGMA。
   - 写出与中断：
     - 准备命令只在新建、从未用过的 fixture 根中工作，不覆盖、也不复用已有的 fixture 根。
     - 旧进程干净关闭（不留 `-wal`/`-shm`/`-journal`）且读回完成后，最后写出 manifest：先写同目录的临时文件并 fsync，再原子 rename，然后 fsync 目录。manifest 写出后，fixture 根不再修改。
     - 被中断或失败的准备命令不留下 manifest，它的根因此不可能被 `fixture_verify` 接受。
     - 命令退出前终止并回收自己启动的子进程；共享锁随最后一个持有它的 FD 关闭而释放。子进程残留期间，独占测量仍不能开始。
2. **摘要冻结入环境。** 候选冻结前，manifest 的 sha256 写入 `.hoh-dev/environment.json` 的 `m4_reference.fixture`（CR-028 §5.1 已为 m4_reference 规定 fixture 项），candidate.environment 的摘要因此绑定它。
   - fixture 身份的输入：pin；准备命令、造库驱动源文件与 helper 包；旧二进制与 helper 的 hash；toolchain、`cc` 与构建 argv；fixture 参数。其中任一变化，下一次冻结前必须先造出新 fixture 和新 manifest，再更新 `m4_reference.fixture`。系统 `cc` 或 Rust 补丁升级改变了 helper 或旧二进制的 hash，同样属于 fixture 身份变化。检查中能观察到的变化，由 `fixture_verify` 直接判 FAIL（第 3 条）。
   - 这种情况下，CR-028:294 的“同 fixture 对照测量”在新 fixture 上进行，并记录新旧 manifest 摘要。两份 fixture 的读回在规模与形状上须相同；数据库 hash 不要求相同。
   - fixture 在 manifest 写出后从不修改：检查只从它复制，升级与回填只作用于本次检查的克隆。manifest 的查找位置由 T03 确定并记录；身份只由摘要判定，不由路径判定。
3. **检查内 `fixture_verify` 取代 `fixture_build`。** 五个串行阶段为 `debug_integration`、`release_build`、`fixture_verify`、`backfill`、`measure`。`fixture_verify` 位于原 `fixture_build` 的位置，上限 ≤600 s，计入 3000 s 工作预算；冻结的 m4_reference 为它给出正整数秒时限，CR28D-205 的强制截止与 `PHASE_TIMEOUT` 规则同样适用。阶段内依次执行下列七步：
   1. **manifest**：只读一次 manifest，它的 sha256 等于 `m4_reference.fixture`。此后只使用这份已核对的字节，并把它写入证据目录。
   2. **sidecar 与清单**：列出 fixture 根。每个文件都须在 manifest 的清单中，且字节数相符；清单中的每个文件也都须存在。下列任一情况都是 FAIL，即使数据库主文件的 hash 不变：
      - 任何未登记的文件，特别是数据库旁的 `-wal`、`-shm`、`-journal`（未登记的 WAL 能改变读到的内容）；
      - 符号链接或非普通文件。

      这次列表（路径、类型、字节数、mtime_ns）记入证据。
   3. **单一快照**：
      - 把数据库主文件与对象文件复制到本次检查的测试根（同一项目盘、检查私有的目录），在副本上重算 sha256，须与清单相等。
      - 清单中的其余文件（二进制、逐对象清单、驱动批次报告、构建记录与原始日志）原地重算 sha256。
      - 此后读回与备份只读这些副本，不打开 fixture 根中的数据库。数据库副本只以 SQLite URI 参数 `immutable=1` 打开：helper 随带的 SQLite 3.54.1 源码（`sqlite3.c` 的 `act_like_temp_file`、`pagerOpenWalIfPresent` 与 `sqlite3PagerOpenWal`）中，immutable 打开按临时文件处理，只读、不加锁、不打开 WAL。
   4. **来源与构建记录**：下列各项都须相等：
      - pin 的 commit/tree 及 `sql_002`、`Cargo.lock` 摘要按仓库对象重算，等于 `m4_reference.old_schema_producer`；
      - 候选树中准备命令、造库驱动源文件与 helper 包文件的 sha256，等于构建记录；
      - 本次 `release_build` 构建的 helper，其 sha256、SQLite 版本与 source id 等于 manifest；
      - 本次观察到的 `cc --version`、pin toolchain 的 `rustc -vV` 与 `cargo -V`，等于构建记录；
      - fixture 参数等于第 1 条的规模。
   5. **读回**：用 helper 读回数据库副本，结果与 manifest 的读回逐项相等。读回还须**独立于 manifest 的自报值**满足下列条件，期望值来自本 CR 与 CR-028 的常数，不来自 manifest：
      - `user_version=2`，`sqlite_master` 中没有任何 003 的表、索引或触发器；数据库头为 WAL 模式（文件头第 18、19 字节为 2）；
      - Run 恰好 100 个，全部为终态；每个 Run 恰好 10 个 iteration，每个都由 pin 的生产关轮事件关闭（关闭 seq 非空且大于开始 seq）；没有可调度的 Task，没有未结束的 Attempt；
      - `events` 行数与最大 seq 都 ≥1,000,000；造库驱动的事件全部随某个已结束 Attempt 的结束写入，并落在该 Attempt 所属 iteration 的开始与关闭之间；
      - 已登记对象恰好 10,000 个，每个 16,384 字节；每个对象的文件都在副本中，sha256 等于登记值与逐对象清单；每个对象都被其 Attempt 的登记事件引用，每个引用都指向存在的对象行与文件；
      - 驱动批次报告中，每批最大行数 ≤500、最大字节 ≤1,048,576，PRAGMA 为 `journal_mode=wal`、`synchronous=2`（FULL）、`foreign_keys=1`。
   6. **克隆**：用随包 helper 以 `immutable=1` 的数据库副本为源，做出本次检查所需的一致性备份克隆；每个克隆 `PRAGMA integrity_check` 为 `ok`，克隆使用对象副本。
   7. **收尾核对**：最后一次读取之后，重算数据库副本的 sha256，仍须等于清单；再列一次 fixture 根，须与第 2 步的列表相同。

   失败处理：
   - 任一不符、缺 fixture 或缺 manifest 都是 **FAIL**，不是 SKIPPED：受影响的 TEST 为 FAIL、整次非零退出，随后进入清理，剩余阶段不借用余量。
   - detail 列出检查到的每一项不符及其期望值/实际值；缺失时写查找位置。只有缺 manifest、manifest 摘要不符或副本无法建立时提前结束。其余情况下，一步不符后仍执行到第 7 步，使每项不符都被记录。
   - `m4_reference.fixture` 缺失，与其他冻结时限缺失一样在启动前 FAIL。
   - manifest 副本与逐项核对结果写入本次检查的证据目录（`m4-environment.json` 引用其相对路径与 sha256），不引用检查外的路径作为本次证据。

   **T03 必须交付的 `fixture_verify` 负例。** 每例调用生产 `fixture_verify` 代码，用准备命令以缩小参数造出的小 fixture 及其自洽的 manifest（测试环境冻结该 manifest 的摘要），标 component/fault injection，不冒称 release 测量。每例都须 FAIL，且断言对应项列在 detail 中：
   - WAL-only 改写：主文件与清单都不变，在数据库旁放一个改写了事件 payload 的 `-wal`，主文件 hash、行数与最大 seq 都不变；另有空的 `-shm`、`-journal`、任一未登记文件、符号链接，各一例；
   - 核验期间文件变化：复制期间改写源数据库（副本 hash 不符）；副本 hash 核对之后、克隆之前改写副本（收尾重算不符）；核验期间改动 fixture 根中任一文件（两次列表不同）；
   - 规模或形状不足而 manifest 自洽：按小库的真实值写出 manifest，并冻结其摘要（例如 R0b 的 40 Run 库）；另一例的参数写成全规模，库实际较小；
   - 对象缺失、多出、大小不是 16 KiB 或 hash 不符，以及引用指向不存在的对象；
   - iteration 未关闭、Attempt 未结束、Run 非终态或有可调度 Task；`user_version≠2` 或出现 003 对象；
   - 批次报告超过 500 行或 1 MiB，或 PRAGMA 不符；
   - 构建记录中的原始日志缺失或 hash 不符；候选树中准备命令或驱动源文件与构建记录不符；helper hash、`cc` 版本或 toolchain 版本不符。
4. **`release_build` 的内容。** `release_build` 串行构建候选（`hoh` 与 `m4_measure` example）及 helper。旧树的构建移入准备命令，其二进制 hash 记在 manifest 中；检查不再构建旧树。候选自身的 release 二进制 hash 仍按 CR-028:294 在检查内生成，记入 `m4-environment.json`。manifest 只含旧树、准备时的 helper 与 fixture 的 hash；AC-064-01 then 中“旧源码/tree/锁文件/产物hash……经manifest可核对”指旧树及其产物。
5. **不变的内容（明确列出）。**
   - `backfill` ≤600 s，从候选第一次打开克隆起计，两个克隆（未中断与回填中 kill）共享这 600 s；kill 窗口 `0 < indexed_through < H`；kill→reconcile 与 kill→host-ready 各 ≤30 s；首次 ready ≤30 s。
   - 检查超时 3600 s，工作阶段合计 ≤3000 s，至少 600 s 余量（清理 ≤300 s，调度余量 ≥300 s）。
   - 全部样本数与阈值；AC/TEST 集合、check 归属与 owner；D4 资源包络；两锁（开发测试槽与 D4 测量锁）及其顺序、继承与贯穿范围（只是阶段名换为 `release_build`、`fixture_verify`）；噪声准入。
   - C5 对 T05、T07、T10 的重测规则适用于含 `fixture_verify` 的五阶段：每个冻结候选重测全部五阶段，不复用别次时长。
   - 旧生产者仍是版本绑定的 fixture 来源，不能把其行为记为候选 PASS；M4 的 public_segments 只在升级后生成；debug 小库功能测试与迁移故障 seam 不变。
6. **检查不再证明的内容及其理由。** 检查不再证明百万库是在同一次执行中造出的。代之以两点：fixture 的来源经 manifest 摘要、冻结环境与 candidate.environment 的摘要链绑定；每次检查中，`fixture_verify` 在同一不可变快照上按字节重新核对 fixture，并独立核对它的规模与形状。可接受的理由：
   - 被测对象是候选对 002 库的升级、回填、查询与界面，不是旧 writer；旧 writer 的造库耗时是 pin 的性质，不是候选的产品阈值，每次检查重造它不增加任何关于候选的证据，却让检查无法执行；
   - 候选面对的输入就是准备命令造出并记录的那份字节：hash、读回、规模断言与 `integrity_check` 在本次执行内针对同一快照核对，规模、来源与写入规则与同次造库相同；摘要冻结在候选绑定的环境中，后来的检查不能悄悄换用另一份库；
   - 准备命令仍受 CR28D-301 的全部 writer 规则约束，其记录（manifest、构建记录、驱动批次报告、原始日志）随 fixture 保存并由摘要绑定，供审查与 QA 核对。

   余项：生成过程本身（库确实由旧 writer 写出）由准备命令及其被摘要绑定的记录证明，不再由检查内观察证明；检查证明字节身份与实际规模、形状。fixture 不保证可逐字节重造（含 UUID 与时间），独立复核时比较读回，不比较数据库 hash。

## 对 CR-028 条文的替代

CR-028 文件不改（已重锁的 CR 不可修改）。本 CR 批准后，下表有效条款中的原表述按本 CR 读，CR-028 其余条文不变。“本 CR 批准后的 AC-064-01”指下文“受锁文件的改动”所列的 given 与 then 文字。

### 有效条款

| CR-028 位置 | 原表述（摘录） | 按本 CR |
|---|---|---|
| :135（§2 迁移段） | release 性能测试由§5.1记录的pre-003产品提交在同次检查构建release、以其自己的writer产生002百万库，不靠debug钩子 | release 性能测试的 002 百万库由准备命令在检查外，以 §5.1 记录的 pre-003 提交构建的 release 旧 writer 产生；本次检查由 `fixture_verify` 核验并克隆；不靠 debug 钩子不变 |
| :261（CR28D-302 第 1 条） | 数据库、对象、旧版本构建与安装也在此盘 | 本次检查的数据库副本、对象副本、克隆与安装在此盘；旧版本构建与预建 fixture 在准备命令自己的 fixture 根中，位于同一 mount source/FSType 的参考项目盘（决定 1） |
| :279（CR28D-303） | 两锁……贯穿 debug、两个 release 构建、造库、回填、measure | 两锁贯穿 debug、`release_build`、`fixture_verify`、回填、measure；准备命令全程持共享测量锁、不取开发测试槽（决定 1“锁”） |
| :294（m4_reference） | 补丁升级时……做一次同 fixture/同预算的对照测量 | 补丁升级未改变 fixture 身份时，用同一 manifest 摘要的 fixture；改变了 helper、旧二进制 hash 或构建记录时，对照测量按决定 2 在新 fixture 上进行。候选 release hash 仍记 `m4-environment.json`（决定 4） |
| :304（预算表“百万事件夹具、迁移回填”行） | 构建≤600s | `fixture_verify` ≤600 s；预建耗时只记录 |
| :305（预算表“本检查总预算”行） | 实测……release构建、造库、回填……保留造库/回填各600s上界 | 实测阶段为 `release_build`、`fixture_verify`、回填……；保留 `fixture_verify`/回填各 600 s 上界 |
| :307（CR28D-205） | 五个时限 debug_integration、release_build、fixture_build、backfill、measure；fixture_build/backfill各≤600s | `fixture_build` 换为 `fixture_verify`，其余不变 |
| :309（C5） | 先实测上述五阶段（含两次 release 构建） | 先实测含 `fixture_verify` 的五阶段；`release_build` 构建候选与 helper |
| :313（CR28D-301） | 每次正式integration……构建普通release HoH及……造库驱动，并串行构建候选release，二者全部计入release_build阶段；记录实际源码树、构建argv、库/二进制hash，不复用别次检查的成品 | 旧树由准备命令构建，源码树、构建记录与二进制 hash 记入 manifest；检查以 manifest 摘要绑定，由 `fixture_verify` 核对。候选 release 仍在本次 `release_build` 构建，不复用别次检查的成品。选定并记录 `old_schema_producer`、驱动只调用旧版本生产写入接口的要求不变；“不能满足预算”只指检查内各阶段的预算 |
| :315（fixture_build 一段） | fixture_build从旧writer首次创建库到一致性备份完成计时≤600s……读回……再用产品一致性备份克隆，不能只拷DB主文件漏WAL。百万002库在这次检查只造一次 | 造库在检查外，读回与清单记入 manifest。检查内 `fixture_verify` 拒绝未登记的 sidecar，在复制到测试根的同一快照上核验 hash、读回与独立的规模断言，再用随包 helper 做一致性备份克隆（决定 3）。场景克隆只在同次 candidate/spec/environment/fixture 摘要下复用 |
| :506（REQ-064 表 AC-064-01 / TEST-064-01 行） | 同次由已记录pre-003提交构建的旧release writer生成002百万事件/对象库……两release构建均计入release_build；造库≤600s | 按本 CR 批准后的 AC-064-01 given/then 读（TEST-064-01 的 fixture/pass_criteria[0] 为同一文字） |
| :575（M4-T03 done_when “第5稿”） | 同次构建绑定pre-003提交的release旧writer，真实造002百万库 | 按本 CR 批准后 `contracts/development-tasks.json` 中 M4-T03 done_when 的“第5稿”子句读 |
| :711（M4-T10 done_when “第5稿”） | 同次旧/新release构建与002百万库，正常/回填中kill两克隆全部计入原五阶段 | 按本 CR 批准后 `contracts/development-tasks.json` 中 M4-T10 done_when 的“第5稿”子句读 |
| :835（“批准后拟应用的文件”中 check-plan 一行） | 同次旧/新release构建和真实002造库/计时 | 候选 release 构建、`fixture_verify` 与计时；造库由准备命令完成 |
| :860（D4 决定行） | 同次pre-003 release writer造002库 | 准备命令在检查外以 pre-003 release writer 造 002 库，本次检查由 `fixture_verify` 核验并克隆；首次 open 起计及中断重启共享 600 s 不变 |

AC-064-02 的“§5.1同次百万库克隆”措辞不改，指本次检查 `fixture_verify` 从预建 fixture 做出的克隆。

### 审查历史记录（不是有效条款）

CR-028:977（第 3 轮处置导语，“生命周期/造库仍须遵守第4轮处置”）与 :1010（第 4 轮处置表 CR28D-301 行，“在同次检查构建旧release并用其writer造002百万库；旧/新构建计release_build”）记录的是当时的审查处置，不是有效条款，本 CR 不改读它们。它们所指的有效内容就是上表的 :135、:313、:315、:506、:575、:711、:860 等条，按上表读。

## 受锁文件的改动

只改写“百万库在同一检查中造出”“由同次构建的旧 release writer 造库”“造库≤600s”“release_build 含两次 release 构建”的子句，并写明 `fixture_verify` 的核验内容。其余字词不变：

- `contracts/requirements.json` AC-064-01 的 given 与 then：
  - given 中“同次由已记录pre-003提交构建的旧release writer生成002百万事件/对象库”改为：“检查外由准备命令以已记录pre-003提交构建的旧release writer预建、manifest摘要冻结于m4_reference.fixture的002百万事件/对象库（本次检查fixture_verify核验并克隆，CR-042）”；
  - then 中“旧源码/tree/锁文件/产物hash与002 schema可核对，两release构建均计入release_build；造库≤600s”改为：“旧源码/tree/锁文件/产物hash与002 schema经manifest可核对，release_build构建候选与helper；fixture_verify≤600s：manifest摘要等于m4_reference.fixture，fixture根无manifest未登记文件（含-wal/-shm/-journal），hash/读回/克隆只读复制到本次测试根的同一快照，逐项hash与读回相符，读回独立于manifest满足100Run×10轮、≥1,000,000事件、10,000×16KiB已登记对象、真实关轮/引用、批次≤500行/1MiB、持久性PRAGMA与终态，各一致性备份克隆integrity_check=ok，任一不符或缺夹具FAIL不SKIPPED”。
- `contracts/acceptance-scenarios.json` TEST-064-01 的 fixture 与 pass_criteria[0]：与上面 given、then 为同一文字。
- `contracts/development-tasks.json` M4-T03 与 M4-T10 done_when 的“第5稿”子句（不在基线锁内，不推进状态）。
- 由 `python3 tools/gen_spec_views.py` 重新生成 `docs/02-functional-spec.md`、`docs/18-traceability.md`。

不改产品代码、检查计划（`tests/check-plan.json`）、release-policy、门禁工具与其他 AC；REQ-064 的 behavior/failure_modes/decisions 不含被替代的条文，保持不变。

## 影响

- M4-T03：交付准备命令（含构建记录、manifest 原子写出与读回）、`fixture_verify`（含决定 3 的负例）及其失败记录；`release_build` 只构建候选与 helper；冻结前在 m4_reference 中填 `fixture` 摘要与 `fixture_verify` 时限。T03 可以现在用真实旧 writer 造一次库（约 70 分钟），再做其余阶段的 C5 实测。
- M4-T05、M4-T07、M4-T10：重测含 `fixture_verify` 的五阶段；fixture 的身份变化（决定 2 所列输入）先换 fixture 与 manifest 再冻结。
- `.hoh-dev/environment.json`：冻结前新增 `m4_reference.fixture`；不修改已冻结的环境。
- 不改 14 项检查的归属、mode、超时，不改 release-policy 或 gate。

## 是否属于 CR-003 D1

否。本 CR 去掉检查内“造库 ≤600 s”与“同次构建”的证明要求，改变了检查所证明的内容，属于证据规则的变化，因此需要项目所有者批准；所有者已于 2026-11-28 选择选项 A。所有产品阈值、样本数、AC/TEST 集合、检查时限与余量都不变；不删除需求或 AC，不改变 CONTRACT 的架构与交互决定。

## 验证

本分支 CR 保持 DRAFT，只改受锁文件与生成视图，不 relock；分支上的 `bash scripts/check.sh --bundle` 预期报基线不符（受锁文件已改而锁未更新），如实记录，不通过修改基线消除。证明在本任务用 `mktemp -d -p /home/ai/dev` 创建并登记的 scratch 副本中进行：临时把状态改为 APPROVED，运行 `python3 tools/baseline.py relock --cr docs/cr/CR-042-m4-prebuilt-fixture.md` 与 `bash scripts/check.sh --bundle`，记录退出码后删除副本。命令与退出码记录在 `.hoh-dev/self-tests/cr042/`；`bash scripts/dev/pre-pr.sh` 在分支上前台运行。独立 Review 与批准由主会话安排。

## 审查记录

| 轮 | 冻结提交 | 审查者与报告 | 结论 |
|---|---|---|---|
| 1 | “CR-042: prebuilt provenance-bound M4 fixture (REQ-064)” | Codex，编号 CR042-100..199，`/home/ai/dev/hoh/.hoh-dev/cr042/review-r1-codex-final.txt`（sha256 `64ee6762bda961421aa64c2e0fb12d0018b9c62c761d09191b0d3993517a41b9`） | FAIL：100、101 high，102、103 medium |
| 1 | 同上 | Pi，会话 `01a21fb9-2ed1-754c-aa3d-bd754060cb28`，编号 CR042-200..299，`/home/ai/dev/hoh/.hoh-dev/cr042/review-r1-pi-final.txt`（sha256 `04ab56b4c6e592501d99cfe9795cb4c54de2686da371ae11722bd823cc35d0fc`） | PASS：201、202 medium，203..207 low |

第 1 轮的处置（提交 “CR-042: round-1 revisions (REQ-064)”）：

| 发现 | 级别 | 处置 | 位置 |
|---|---|---|---|
| CR042-100 | high | 改正：未登记的 sidecar（含 `-wal`/`-shm`/`-journal`）为 FAIL；hash、读回与克隆只读复制到测试根、以 `immutable=1` 打开的同一副本，收尾重算 hash 并重列 fixture 根；负例含 WAL-only 改写与核验期间文件变化 | 决定 1 manifest 清单、决定 3 第 2、3、6、7 步与负例；AC-064-01 then |
| CR042-101 | high | 改正：读回独立于 manifest 满足规模、形状、关轮、引用、批次、PRAGMA 与终态的常数；manifest 自洽的小库或异形库为 FAIL | 决定 3 第 5 步与负例；AC-064-01 then；M4-T03 done_when |
| CR042-102 | medium | 改正：构建记录（准备命令提交与文件 sha256、toolchain 与 `cc`、`env -i` argv、原始日志）由清单绑定，`fixture_verify` 核对；输入变化先造新 fixture；数据库不要求逐字节可重造 | 决定 1 构建记录、决定 2、决定 3 第 4 步 |
| CR042-103 / CR042-202 | medium | 改正：替代表补 :135、:261、:294、:506、:575、:711、:860，各行写明按本 CR 读的文字；:977、:1010 另列为审查历史记录 | “对 CR-028 条文的替代” |
| CR042-201 | medium | 改正：CTO 在选项 A 之下的唯一读法——准备命令全程持共享 D4 测量锁、不取开发测试槽，写在决定 1，其余位置引用它；批准人一行记录这一读法的来源 | 批准人；决定 1“锁”；替代表 :279；M4-T03 done_when |
| CR042-203 | low | 改正：定义 helper 并引用 ADR-042 的分支与提交 | 术语 |
| CR042-204 | low | 改正：候选 release hash 按 CR-028:294 记 `m4-environment.json`，manifest 只含旧树、准备时的 helper 与 fixture 的 hash | 决定 4 |
| CR042-205 | low | 改正：两处证据路径都写出，sha256 相同 | 依据 |
| CR042-206 | low | 改正：helper hash 变化（含系统 `cc` 变化所致）是 fixture 身份变化，下一次冻结前重造；`fixture_verify` 核对 helper、`cc` 与 toolchain | 决定 2、决定 3 第 4 步 |
| CR042-207 | low | 改正：manifest 最后原子写出；中断不留 manifest，不覆盖已完成的 fixture 根；退出时回收子进程、释放锁 | 决定 1“写出与中断” |
