# CR-038 · 执行记录区分命令未启动与已启动后退出 127

ID：CR-038
状态：DRAFT
批准人：待主会话在独立 Review 后依 CR-003 D1 批准
日期：2026-11-24
关联：M3LD-100，REQ-052（AC-052-04）、REQ-053（AC-053-02），ADR-027 原则一与 M2-T18 投影

## 问题

`127` 同时表示 runner 未能启动命令，以及已经运行的命令自身返回 127（例如脚本调用了不存在的二进制）。旧规则把两者都标为 FAIL/NOT_STARTED、视为无结论。因此真实失败在同一候选上重跑通过后变成 PASS，违反失败历史不能被后来的成功覆盖的要求。cgroup adoption 失败只暴露了已有歧义，见 `reports/reviews/M3-cleanup.md` 的 L7/L8/L9。

## 决定

1. `contracts/execution.schema.json` 增加必填 boolean `started`，与退出码独立。保留 schema_version 1.1；候选已绑定 schema 字节与源码，新旧证据不得跨候选沿用。缺失字段或字段非 boolean 一律 INVALID，不从退出码猜测、不补默认值、不改写历史记录。旧记录仍可保留供诊断，但不能按新规则通过验证；需要新候选与新执行记录。升级前写下、记录缺 `started` 的产品待入库事实（`.hoh/pending/`）不是永久的恢复失败：对账按已启动读入（从不按未启动），并把原样的记录交给规则 1 核对 schema，记为 RECORD_INVALID 证据，候选的历史照常继续（M3LS-02）。
2. Python runner 在成功 spawn 后立即记录 `started=true`，只有未成功 spawn 为 false；共同 execute 路径提供实际 spawn 通知，避免把启动后的 I/O 错误误记为未启动。spawn 期间到达的 SIGINT/SIGTERM/SIGHUP 先被扣住，Popen 返回、spawn 通知记下之后在同一个结束进程组的 try 里抛出：启动之后的中断结束并回收进程组，记为 130、started=true（INTERRUPTED）；启动之前（spawn 未成功）的中断记为 127、started=false（NOT_STARTED）。这是对 run_check 既有约定的更正：启动前被中断原记为 130/INTERRUPTED，现为 127/started=false（M3LS-03）。产品 runner 的 spawn 失败、命令运行前 cgroup adoption 被拒、启动闸门 exec 失败为 false；闸门 exec 错误经一个 close-on-exec 描述符传回（不解析退出码或 stderr 猜测，M3LS-01/-51）：runner 建一对 Unix socket，只在 `hoh_core::spawn` 挡住本进程其他一切启动期间把一端的可继承副本（编号 ≥3）交给闸门（`__check-gate --exec-error-fd <n> --`）；闸门在读 go 行之前用 `pidfd_getfd` 取得它的 close-on-exec 副本并关闭继承的编号，exec 失败时写 `exec_failed`，exec 成功则由内核关闭，所以已启动的命令从不持有它、也不能经 /proc 重新打开（socket 不能经 /proc 打开，管道可以）；runner 在闸门结束后不等待地读另一端，只有恰好读到该报告才记 false，读不到、写不进或取不到描述符都保守记为已启动的失败并保留诊断。早先写在记录目录里、命令也写得到的失败标记文件不再使用。命令已启动的其余结束路径为 true。仅作 attempt 完整性否决、从未执行检查命令的合成记录为 false/127，仍保留 source_unchanged=false 的否决。
3. `started=false` 必须同时满足 exit_code=127、timed_out=false、artifacts 为空、duration_seconds≤5 秒、起止区间在 [0,5] 秒内。这里选择严格读法：有声明工件、耗时或区间超过 5 秒即 INVALID，不能成为可被重跑抹去的无结论失败；5 秒取既有时长一致性容差的绝对下限，边界含 5。stdout/stderr 仍须存在并校验摘要，可保存 runner 启动诊断，不算命令工件。真实慢启动失败保留实测耗时，不截短、不伪造，按 INVALID 交回诊断。
4. 原则一在既有否决与超时规则之后，仅 false/127 标记 FAIL/NOT_STARTED（无结论）；true/127 在没有更早规则成立时为普通 FAIL、reason 为空，与其他普通测试失败同样参与历史聚合。同一检查 [已启动的 127 FAIL, PASS] 为 FLAKY；[未启动的 127, PASS] 为 PASS。既有合规降级优先级不变，false 记录的工件约束阻止其伪装成降级。
5. Python release gate、traceability 原则一复刻与 Rust verification/检查级投影保持一致。release gate 不接受未启动记录作 PASS 或 DEGRADED。ADR-027 表格与 outcome (7) 添加本次日期的更正，不修改旧审查事实。

## 受锁文件与实现影响

本次修改的受锁文件逐项如下：

- `contracts/execution.schema.json`：started 必填类型与未启动形态约束。
- `tools/common.py`：execute 的成功 spawn 通知，以及 spawn 期间扣住中断、在结束进程组的 try 里抛出。
- `tools/run_check.py`：写入启动事实。
- `tools/release_gate.py`：检查未启动形态及拒绝未启动记录通过发布。

批准后通过 `tools/baseline.py relock` 更新 `contracts/baseline-lock.json`，不手改摘要。该工具当前拒绝 DRAFT；本委派保留 DRAFT 并如实报告 relock、bundle、pre-pr 的退出状态，由主会话审查批准后重锁及重跑。

非受锁实现包括 `scripts/product/traceability.py`、Rust verification（含待入库事实按原样核对的 `Evidence::read_as`）、产品检查驱动、`hoh_core::spawn::inheriting` 与 `crates/hoh-app/src/gate.rs` 的 exec 失败描述符、相关 bundle/Rust 测试及其合成夹具、ADR-027 与 ADR-028 决定 7 的 `__check-gate` 协议。host runner 沿用同一产品检查驱动。requirements 的行为 6 是必含字段清单而非禁止扩展的穷举，不变更 requirements、AC 或生成视图。

## CR-003 D1 判断

判断为满足 D1：补齐缺失的执行事实字段，修正规格将真实失败当作未运行的不一致；缺字段与矛盾记录拒绝，证据规则加严。没有删除需求、AC、测试、检查或发布范围，没有把 required 改为 degradable，没有降低阈值或放宽门禁，不改 CONTRACT.md 的架构或交互决定。本判断是开发者提案，不是独立 Review 或批准。

## 验证方式与交付

先在原实现运行相关测试留基线，再添加并实际观察 127/started 分类、同候选历史、缺字段与矛盾记录的红灯；实现后运行相同测试与相关回归。测试名按 AC-052-04、AC-053-02 标记；runner 测试含真实进程的缺失命令、脚本内部缺失命令、正常、超时/取消与 adoption 失败路径，均为自测，非验收，不调用 live agents。

原始命令、退出码与日志保存在 `.hoh-dev/self-tests/m3ld100/`。提交前执行 bundle，最后前台执行 pre-pr（--no-deployment）；审批前的基线阻塞保留，不降低检查或临时伪造 APPROVED。不推送。

## 审查记录

待主会话补录；本 CR 保持 DRAFT。
