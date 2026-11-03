# 开发计划与依赖

机器任务板：`contracts/development-tasks.json`（58个任务）。它是可细化的计划，不在基线锁内；任务状态只写主 worktree 的 `.hoh-dev/task-state.json`，由 `tools/taskboard.py` 维护。完成任务不等于需求验收，需求验收以冻结候选上的证据为准。

每个任务可带三个归属字段（CR-003）：`acceptance` 列出它负责的 AC（缺省为其 `requirements` 的全部 AC），`checks` 列出它落地的检查，`profile` 标记 v0.1/v1 的收口任务。`validate_bundle` 强制：每条 detailed AC 恰好归属一个实现任务；任务所需的检查由它自己或祖先任务落地；带 profile 的任务以该 profile 全部 AC 与检查的归属任务为祖先；检查计划里的占位命令写的任务名必须是该检查的归属任务。

## 任务类型与规则

|kind|负责人|写者|说明|
|---|---|---|---|
|setup|developer/researcher|视任务|环境、workspace、检查计划骨架、协议样例采集|
|spec|cto|是|把本里程碑 outline 需求细化为 detailed，写 CR 并重锁基线|
|implement|developer|是|只能领取所关联需求均为 detailed 的任务（Readiness）|
|verify|reviewer（M0–M2）/ qa（M3 起）|否|同一冻结候选上的独立 QA/Review；不得修改被测源码|
|release|developer|是|按 release profile 跑全部检查并执行 release_gate|

`taskboard claim <id> --owner <会话>` 原子领取任务：依赖必须已完成，readiness 必须满足，且同一时间只能有一个 writer 任务在运行（单写者）。`set` 只允许合法迁移：done/blocked 必须写证据或阻塞说明，只有领取者能结束 running 任务。重新打开已完成的任务时，已开始的下游任务自动转为 blocked。列表顺序即默认优先级。

## 阶段与出口

|阶段|主要产物|出口|
|---|---|---|
|M0|基线核对、真实CLI探测与协议样例、Cargo workspace、`tests/check-plan.json` 与检查脚本骨架|包检查通过；真实环境记录；缺认证记为外部阻塞|
|M1|领域类型/HCP、进程底座、三Agent适配器、CLI命令面、doctor、基础TUI shell|M1 detailed AC 在冻结候选上有证据；fixture 与 live 分开|
|M2|SQLite/outbox/租约、状态机、DAG、reconcile/重试/预算、host 与 pause/cancel、团队/监督/上下文、候选与 QA/Review barrier|crash/replay 与 recovery 检查；最小闭环无错误验收|
|**v0.1**|M1+M2 全部能力上的中间发布：CLI/TUI、三适配器、持久 Runtime 与 API、冻结候选/独立 QA/Review|`release_gate --profile v0.1` → `INTERIM_RELEASED`；不是完成，继续推进|
|M3|规格追踪/CR/readiness、Prompt/Skill/模板、Plan/Build/Auto 与运行中对话|先 spec 任务细化；需求→任务→实现→验收全链路|
|M4|Workspace/Team/Focus/Timeline、状态栏、展开块、终端兼容、容量|真实 PTY、宽度/Unicode、大日志快照|
|M5|证据/回归/Flaky/最终门禁、备份迁移、三 Pi 插件（degradable）|插件真实组合测试；不可用时带诊断 DEGRADED；门禁负例|
|M6|Web GUI、重连、多客户端、Git 交付、安装文档|浏览器与新 VM 部署、冲突与重连|
|M7|性能、交付真实性、长期维护、72h soak|`release_gate --profile v1` → `COMPLETED`/`COMPLETED_DEGRADED`；否则 INCOMPLETE|

## v0.1 路径（CR-003 D2）

M0 → M1（T01→T02→T03→T04/T05→T06→T08→T07→T09）→ M2（T01→T02→T12→T03/T04→T05→T19→T06→T14、T07→T17→T08→T09→T10→T16→T18→T15→T13）→ M2-T11（M2 独立 QA+Review）→ V01-T01（v0.1 候选全量检查）→ V01-T02（发布）→ M3-T01。实际顺序以 `taskboard next` 为准（列表顺序加依赖）。

v0.1 的需求范围仍是 `release_targets` 含 `v0.1` 的 18 项，但这些需求的 AC 依赖 Codex 适配器（AC-004-04）、团队绑定与 role_epoch（AC-035-01、AC-036-03、AC-054-01）和 Commander 监督（AC-002-01），所以 v0.1 在 M2 全部完成后发布。原“最短路径”把这些能力排在 v0.1 之后，导致任务无法诚实完成（docs/23）。

## 规格滚动细化

M1/M2 的 27 项需求已是 detailed。M3–M7 的需求是 outline，各里程碑第一个任务（Mx-T01，kind=spec）负责：

1. 每项写 2–5 条可判定 AC（确有必要可多于 5 条，在 CR 中说明）（Given/When/Then 具体到命令、状态、阈值，至少一条失败路径），以及 behavior、failure_modes、测试场景（check_id、步骤、判定、需保存的观测）。
2. 不删范围、不降门槛；文档没写到的地方按更严格、可验证的行为选择，并记入该需求的 decisions，在 CR 中请项目所有者确认。
3. 按每个实现任务 ≤2 项需求、≤8 条 AC 拆分本里程碑的实现任务，填好 `acceptance`/`checks`/`depends_on`。
4. 运行 `tools/gen_spec_views.py`；写 CR 记录（只细化、不删范围、不降门槛时按 CR-003 D1 经独立 Review 后由 CTO 批准，其余需要用户批准，见 docs/21）；执行 `tools/baseline.py relock --cr`；`scripts/check.sh --bundle` 必须通过。

## 每个任务的执行契约

先读相关 REQ/AC 与模块文档；先写能失败的测试，再做最小实现，然后局部测试、提交 commit、冻结候选，交给独立 Review。记录候选、未解决问题和下一步，不在最后只写“已做完”。使用 feature branch，不在 main 上直接大改。

M0 发现协议与文档不一致时，查对应版本的官方资料，并补兼容记录；不把设计硬改成另一套框架。模型名使用本机实际默认配置。

## 自举切换

M0–M2 由一个主开发会话完成，并经单次原生 CLI 桥派发独立 Review；桥不是生产调度器。M2 通过后，先选小任务交给新 HoH 执行，并与外部结果比对。未通过幂等与证据测试的新 HoH 不得接管整仓写入。

## 外部阻塞

M0-T04（真实协议样例/认证）可能因外部账户受阻。M1/M2 的类型、离线 fixture 和 CLI 都不依赖它；M5 插件实现依赖它；live 验收在最终汇合。不得让一个缺失的账号阻塞所有无关开发，也不得把 blocked 任务改成 done 来解锁验收。

## 范围控制

新平台、多机、插件商店进入 future backlog，不替代已承诺的 Web 或角色自定义。性能门槛如需修订，提交带基准证据的 CR，不得为了“完成”自行降低。
