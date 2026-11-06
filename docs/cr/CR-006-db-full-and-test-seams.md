# CR-006 · 数据库写满错误码与空操作钩子形式的故障注入点

ID：CR-006
状态：APPROVED
批准人：F2 由项目所有者批准（2026-11-06，主开发会话中选定空操作钩子方案）；F1、F3 由 CTO 依 CR-003 D1 预授权（独立 Review 会话 01a1b24a-79cb-7ab0-a89b-a77317613db1）
日期：2026-11-06
基线：HOH-BASELINE-1.1 revision 6 → revision 7

## 背景

M2-T12（Engine host 骨架与本地命令 API）实现 AC-036-04、AC-041-04、AC-062-06 时，发现两处规格缺口，二者互相独立。

1. **没有“写满”的错误码。** AC-036-04 要求：COMMIT 因 SQLITE_FULL 失败时，命令返回非 202 的错误，错误体 retryable=true，并且不产生任何事件。可是协议 schema 的 `ErrorCode` 里没有对应的码。
   - `DB_BUSY` 的含义是写锁等待超过 busy_timeout（AC-062-01）。把写满也报成 DB_BUSY，会让重试策略和诊断都判断错。
   - ReasonCode 是封闭集合，新增码需要 CR（CR-003）。
2. **故障注入的要求与 static 检查冲突。**
   - REQ-036 的决定是“故障注入点只在 test feature 编译，生产二进制不得包含注入路径”。七个场景的 fixture 也写成“test feature 编译的 host”：TEST-005-05、TEST-036-02、TEST-036-04、TEST-041-01、TEST-041-04、TEST-042-02、TEST-062-06。
   - 但 AC-062-05 的 static 检查规定成员不得声明 Cargo 特性（docs/22 ②），因为特性可以把代码藏在被检查的构建之外。
   - 改用自定义 cfg 同样会把注入代码藏在 clippy 与扫描之外。
   - 因此“只在特性构建中存在”与“全部构建都被检查”无法同时成立，实现只能二选一。

## 决定

| # | 内容 |
|---|---|
| F1 | `ErrorCode` 增加 `DB_FULL`：DB 所在文件系统写满或超出配额，写事务无法提交。HTTP 503，retryable=true。状态、事件、outbox 均不变（事务回滚），也不广播。不与 DB_BUSY 混用。REQ-062 的决定中记下这一码（初值） |
| F2 | REQ-036 的决定改为：故障注入点是产品代码中的空操作钩子。<br>• 产品对每个钩子的实现什么也不做。<br>• 制造故障的实现（构造写满错误、断开连接、让自检某一步失败、自发 SIGKILL、换时钟，以及 WAL 自检第二进程的失常：不加锁、计数不更新、假锁、单向锁）只在测试目标中，由测试在同一进程内传入。<br>• 产品入口（main 及其调用的一切）只使用空实现，没有环境变量、命令行参数或配置文件能换掉它。<br>• 产品二进制中没有能产生故障的代码；全部代码都在 AC-062-05 的受检构建中 |
| F3 | 七个场景的 fixture 改为“测试目标中构建的 host（某某由测试实现的钩子产生，产品的钩子为空操作）”。步骤、通过条件与观察工件不变 |
| F4 | 产品侧：<br>• `PersistError::Full`（retryable），SQLITE_FULL 映射到它，HTTP 层返回 503 DB_FULL。<br>• 钩子：persistence 的 `FaultHooks`（提交前、WAL 自检每一步前），WAL 自检第二进程的 `ProbeBehaviour`（打开、计数、各命令的应答；原先产品代码中的 `ChildFault` 开关移入测试），host 的 `HostHooks`（命令提交后、响应发出前）。产品实现分别为 `NoFaults`、`NormalProbe`、`NoHostFaults`，它们只用 trait 的默认方法。<br>• 测试：<br>　– 扫描产品源码，确认每个钩子 trait 在测试目标以外只有其产品实现，且该实现不带方法体；扫描本身另有反例测试（带方法体的产品类型、其他类型、测试模块中的实现）。<br>　– 调用产品实现，确认它们不产生任何故障，也确认 host 的产品配置带的正是它们 |

## 批准依据

- **F1、F3 及 F2 的措辞修正部分符合 CR-003 D1 预授权**：补齐缺失的原因码；修正 REQ-036 与 AC-062-05 之间的不一致；不删除需求或 AC，不改 required/degradable 与 release_targets；不改 CONTRACT 的架构与交互决定。F1 让失败更可区分，F3 不改场景的步骤、通过条件与观察工件。
- **F2 放宽了 REQ-036 的原保证，超出 D1，由项目所有者批准**（第 1 轮独立 Review CR6-01 判定）。原文要求注入点不出现在生产二进制中；按 F2，产品二进制里有钩子调用点，只是产品对它们的实现为空、能产生故障的代码只在测试目标中。不放宽就只能用 Cargo 特性或自定义 cfg，而那会让注入代码不处于 AC-062-05 的检查之下。项目所有者 2026-11-06 在主开发会话中从三个方案里选定空操作钩子（另两个为“专用 cfg、static 双构建”和“暂缓三条 AC”）。

## 不变的内容

- 需求与 AC 的数量、各 AC 的通过条件与阈值不变。
- release-policy、门禁工具与其他 schema 定义不变。
- baseline_id 仍为 HOH-BASELINE-1.1。

## 验证

- `cargo test -p hoh-core`：schema 与 Rust `ErrorCode` 枚举一致，语料往返。
- `python3 tools/gen_spec_views.py` 之后，docs/02 与 docs/18 与契约一致。
- `python3 tools/baseline.py relock --cr docs/cr/CR-006-db-full-and-test-seams.md` 之后，`python3 tools/baseline.py verify` 通过。
- `bash scripts/check.sh --bundle`。
- M2-T12 的 TEST-036-04 断言 DB_FULL，另有 F4 的源码扫描测试。

## 审查记录

- 第 1 轮（Codex，会话 01a1b21f-51c2-7491-9482-8ca75e99b17f）：FAIL。
  - CR6-01（high）：F2 放宽原保证，不能按 D1 自行批准。已改为项目所有者批准，并把方案改成空操作钩子。
  - CR6-02（medium）：另有四个场景仍写 test feature。已按 F3 统一为七个。
- 第 2 轮（Codex，会话 01a1b240-337f-7930-bf2a-3b0c537a636e）：FAIL。
  - CR6-20（high）：F4 的扫描只认 impl 行与类型名，带方法体的 `NoFaults` 也能通过。已改为：产品实现必须不带方法体；调用产品实现以确认不产生故障；并为扫描加了反例测试。
  - CR6-21（medium）：WAL 自检第二进程的 `ChildFault` 故障分支仍是产品代码。已改为 `ProbeBehaviour` 钩子，失常实现移入测试，产品只有按协议行事的 `NormalProbe`。
- 第 3 轮（Codex，会话 01a1b24a-79cb-7ab0-a89b-a77317613db1）：PASS，无发现。CR6-20、CR6-21 已解决，F2/F4 的文字与实现相符。
- 各轮记录见 reports/reviews/M2.md。F4 的实现随 M2-T12 合并，由 M2-T12 的审查一并覆盖。
