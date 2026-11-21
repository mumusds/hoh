# CR-030 · M5 证据、恢复与 Pi 插件需求细化（第 1 稿）

ID：CR-030
状态：DRAFT
批准人：待 CTO 决定与独立 Review；本稿不批准
日期：2026-11-21
输入基线：`8d9ae599d30982fd431b950dec229786cdc15379`；工作树 `/home/ai/dev/hoh-wt/m5-spec`；分支 `prep/M5-T01-spec`
适用任务：M5-T01 准备；正式应用仍须 M4-T04 完成、领取 M5-T01、独立审查通过及 CTO 按 CR-003 D1 批准

## 背景与授权边界

本稿只提交这份 CR，不改真实工作树的受锁文件、产品源码、检查计划、任务板或全局 PROGRESS，不 relock、不运行 live Agent、不推送。下文是可机械应用的提案，39 条新细化 AC 的产品行为均未在本次实现。输入没有 Runtime 身份头、输出文件或自测目录，不虚构 run_id/attempt_id/role_epoch/candidate_id 或 TaskResult；唯一写权限由委派载荷授予，起始工作区干净。

九项需求现在均为 M5/P1/outline，每项一条占位 AC。保留每个对象的 id、category、priority、milestone、title、required_release、release_class、release_targets、summary，保留全部旧 AC/TEST 标识及其行为范围；新增 behavior/failure_modes/decisions/doc_refs 和可判定场景。只有 REQ-048/049/050 仍为 degradable，其余 required；全部仍含 v1。既有70项需求、14项检查、72h/20轮、三Agent、Web、Pi插件、独立QA/Review和证据规则均不降低。细化是否符合 D1 由 CTO 与独立审查判定；发现需要改 CONTRACT 架构或删范围时停止对应应用并升级，草案不能替代决定。

依据为 CONTRACT（含 CR-001）、goal、CLAUDE、docs/02、06、07 §6、10、19、21、24、CR-026/018/020/024、ADR-007/009/016/018/021/027/028/029/031/032、当前 requirements/scenarios/development-tasks/check-plan/release-policy。CR-026 提供对象/场景/逐AC依赖闭包格式。本稿读取它的“所有者已定的事项”“应用时的更正”，不以旧记忆替代当前契约。

### 已有决定，不重新询问

- CONTRACT 的 Rust 单体、原生 Agent、单写者、冻结候选、CLI 面、TUI/Web、插件职责不变。Pi Commander 按 CR-024 禁扩展，插件只在其它 Pi 职位加载。
- CR-026 O1–O6/C22：只有用户批准产品规格；无人值守仅用已授权默认，方向/范围/非目标不能假设；用户未提交改动先阻塞、发现不自动构建；Markdown 为语义权威；Skill不写原生用户目录；任何规格修订令全部旧验收失效，新候选重验；首规格起草受既有 spec_draft 规则约束。
- CLAUDE/docs/19 的所有者审查收敛决定、只追加已推送历史与隔离VM非root规则仍适用。本任务明确不push。
- CR-028 的已绑定 CTO 指令作为 M4 前提：单check owner/verify无acceptance、真实进度而非todo计数、公开流、证据对象引用保护、低空间明确失败、脱离不取消、真实终端/容量测量不降门槛；不重新讨论终端键位或宽度方案。

### 未应用或尚未实施规格的假设（应用时逐项重核）

| ID | 精确输入与假设 | M5 消费方 / 重核条件 |
|---|---|---|
| A1 | CR-028 在 `prep/M4-T01-spec` 已 APPROVED、尚未应用；M4-T04 仍是最终 verify，涵盖其 T02/T03/T05..T11；M4 在 M5 前落地 | 全部任务通过 M5-T01→M4-T04 取得前置；应用时重新读取实际 main 的 CR、任务图、实现/验收，不凭本表宣称能力已存在 |
| A2 | CR-028 M4-T03 提供不可变对象、证据引用/GC保护、低空间及查询；T09 提供公开流/活动身份，T02/T06 提供设置/Focus/真实进度 | T05补备份pin，T06恢复，T03插件设置，T10映射，T08真实PTY；若实际接口/保护边界不同先修AC/ADR再审 |
| A3 | CR-028 迁移003只是预计的先行schema；CR-026迁移002已在输入基线，但M3实现任务尚需真实完成 | backup/migration测试选应用时实际最新迁移及真实旧binary，不固化猜测的后续编号；检查全量Rust契约测试 |
| A4 | CR-026规范已应用；M3-T02/06/09/04/07/10/08行为通过M3-T05提供，不能借已APPROVED文字当产品PASS | STEER投递、规格失效、追踪、停滞、四层Prompt均在依赖闭包；M3A-01/02在本稿补齐 |
| A5 | M6/M7尚未细化；M6负责HoH自身Web GUI与Git/文档，M7负责v1检查profile切换及真实soak/最终发布；本稿不假设它们提前实现 | REQ-057采用Q1目标项目解释；REQ-056用product判定器测试，最终HoH验收仍M7。任何后续CR改此分工，须重核M5依赖，无先签后补 |
| A6 | M6/M7的并行草案必须把本文件当前DRAFT作为未批准输入；本M5稿不反向依赖未来M6/M7草案。config/plugins.json的版本是本仓库锁定值，并非“上游最新/兼容”结论 | 后续草案记录本稿commit/hash；应用时重新对照M5最终AC/task/检查owner。插件接口及hook须实现时读取精确安装版本并真实验证，不能以草案或本次离线检查证明 |

CR-028 来源分支 HEAD：`dde5109f43a2081b5d5e5c507b91a14b12c62143`；所读文件 sha256：`e442f111eb14227ba2fc5496dafe812e8eca1ee070b3ee7208f9c3ca560e1800`。本次机械应用以输入基线独立模拟 M5，未声称把 CR-028 的所有需求/OpenAPI/SQL先行实施；另有仅任务图的 A1 闭包探针，结果见验证方式。最终 M4+M5 合并应用仍须完整重验。

## docs/24 逐项吸收与范围

| 条目 | 本稿处置 | AC / 责任 |
|---|---|---|
| 1.1 机器完成标准 | 中间里程碑、模型claim、v0.1不能冒充完整发布 | 056-01/02/05 |
| 1.2 恢复点与外部阻塞 | 活动工作、精确身份、预算、下一动作、唤醒条件并按实时状态对账 | 044-02/03；原REQ-042等待/额度规则不改 |
| 1.3 完成通知丢失 | M3的no_progress仍保留；恢复清单核对真实活动，不靠摘要或pgrep模式循环 | 044-03；不另造全局循环 |
| 2.1 角色分离/只读审查 | 候选外缓存、证据根、前后hash、污染否决 | 055-03、056-02 |
| 2.2 互补审查 | 不相交编号、来源保留、冲突按复现证据 | 055-04、056-04 |
| 2.3 收敛（CR-026 M3A-01） | 连续两轮同类复发即设计修正，不依赖净进展；四轮/第五轮上限与low包，不豁免AC | 055-04，M5-T02；发布继续严格056-01 |
| 2.4 provider失败分类 | provider_content_filter显式诊断，非能力不足；缺认证/额度/503沿用REQ-020/027/042 | 051-04；不修改底座适配器协议或自动切换额度耗尽者 |
| 2.5 消息交叉 | M2去重/回执与M4未确认指令已负责，M5补代码目标而不复制全局队列 | 044-04/05 |
| 2.6 资源归属 | 只删登记精确路径/只停带启动身份进程 | 055-03、051-03、049-03 |
| 2.7 版本绑定（CR-026 M3A-02） | 入队固定code_target，HCP携带，过期拒收及重启不重绑 | 044-04/05，M5-T06 |
| 3.1 持久委派说明 | M3 Prompt持久引用已有范围；恢复清单补候选与下一动作 | 044-03，不改REQ-045 |
| 3.2 结构化结论 | 沿用受锁报告与原生会话验证，不从聊天猜PASS | 056-01/02 |
| 3.3 压缩摘要会过期 | Runtime/Git核对后继续，重复事件幂等 | 048-02 |
| 4.1 分类记忆与时效 | source/project/confidence/validity/时间/分类；撤回活动召回，不删证据 | 049-01/02/04 |
| 4.2 进度与任务板 | 局部todo不造全局领取与accepted；映射活动、候选、证据、下一动作 | 050-01/02/03 |
| 5 临时盘/低空间、systemd环境 | M4 REQ-064与REQ-006已有范围；M5副本在项目盘，备份/安装也测低空间 | 063-03、051-03、057-03；不重写doctor全范围 |
| 5 Python跨版本 | 回归矩阵及路径边界，不用单机结果盖过矩阵失败 | 055-02 |
| 5 打包旧binary、Git强推 | 原REQ-069/M7与REQ-065/M6保留；M5校验发布物/构建hash但不抢交付实现 | 056-05、057-01；A5 |
| 5 信号与内外证据串扰 | 默认信号与检查变量隔离、按归属清理 | 055-03 |
| 5 schema与Rust枚举漏测 | 正式应用跑完整cargo test --workspace --locked，并核对应源码；本稿无schema枚举变动 | M5-T01 done_when与应用清单 |

## 共同约定与待 CTO 审定的细化方案

1. **证据层次。** `given/when/then`是拟议产品标准。下文每AC一个scenario与现有check id。原TEST-*-01保留原check；新增检查映射不新增/删减release-policy检查。任务的`checks`只表示维护者，执行者通过依赖使用。unit/integration=M0-T03、recovery=M2-T05、deployment=M2-T14、tui-pty=M1-T08、independent/traceability=M2-T10均不变；plugin-live唯一维护者仍M5-T03。
2. **product/mock/live。** 本地registry/FakeAgent/合成发布输入只是product测试内的故障夹具，不能记成真实Agent/插件/发布。`plugin-live`必须有真实Pi原生session与已加载plugin列表。认证/临时故障不是插件兼容性的替身。只对上游不可用/不兼容/无法安装按CR-001降级，execution与AC引用的diagnostic/upstream_ref均需合法。检查结局DEGRADED时引用它的AC不能是PASS；每AC按docs/10与test-result规则产出匹配的降级行，原始PASS子例可保留，不能伪称整个组合通过。REQ-051在product检查中始终required。
3. **未运行的表达。** `contracts/test-result.schema.json`行状态只有PASS/FAIL/DEGRADED/SKIPPED，不能写NOT_RUN/BLOCKED/FLAKY冒充合法test行。这些词是执行/汇总状态；未运行缺项在汇总中明确、不能进入满足集合。每个场景的多个子例用同TEST id但不同name，全部通过才满足；缺设施非零且不能全行PASS。
4. **审查系列（Q3）。** 分别持久设计/实现review series、轮号、候选与spec、finding来源、缺陷family、处置和设计修正ArtifactRef；重启、修正或新提交不清同系列轮号，范围正式变更须明确记录新系列与原因，不能为逃避上限重置。连续两轮同family有可核对发现即在继续修复前要求设计修正，即使别的AC增长。初审全范围，复审聚焦修复diff及调用链/回归；范围外新medium/low记待办，不扩本轮；critical/high仍阻断。前3轮无critical/high且medium已处置可收敛；第4轮无critical/high时medium须修复并红绿、主会话核对，low入包，不另开完整审查；第4轮仍critical/high先设计修正，只再开一次新会话第5轮核这些项，仍有即BLOCKED。任何required AC FAIL或发布门禁不满足仍阻断，审查收敛不篡改已有报告、不免独立验收。low包在verify前完成或按AC记有证据的延期。缺陷family由持久分类及证据支撑，不能单靠模型声称“不同问题”重置。
5. **STEER（Q2）。** 代码身份与role_epoch/spec并列。受理事务在写者安全点取当前git_commit/tree/candidate，固化到input_queue自由JSON、input.accepted与路由引用；HCP既有candidate字段承载非null目标，正文引用补commit/tree，投递拒绝沿用CANDIDATE_MISMATCH。任何异步等待后再校验；与写提交串行的派发边界防TOCTOU，检测外部编辑停写。无candidate可按commit/tree，无commit不能STEER。原生通道不可撤回的已投递效果不能谎称没执行；失联走UNKNOWN_EFFECT。客户端明示受理时的目标；本稿不声称能识别受理前用户所见旧代码版本，Q2列显式客户端目标的后续选择。
6. **恢复/备份（Q7）。** 每次维护动作operation_id绑定项目/Run/Attempt/epoch、登记资源、effect阶段与manifest；完成仅原子提交后可见。备份固定DB快照与引用闭包，复制期pin对象防GC；恢复独占项目锁、停派发并收尾所有Attempt，再校验/原子替换/reconcile。维护从现有交互/诊断入口接入；不加Shell子命令。不可恢复外部效果不盲重放。
7. **全范围发布验证（Q6）。** 不伪造或重开增量round；Runtime独立持久义务绑定release_validation_id和全范围。使用独立只读QA/Review Attempt，报告原生身份/独立性/hash规则照旧，收取不得借round_of_task猜归属；实现ADR须说明报告验证、scope、确认条目和当前身份CAS。该义务不迁移源Task或候选状态；损坏证据继续走既有INVALID事实否决，不能为了“不迁移”吞掉完整性违规，若现有record_evidence会迁移候选且无法分离，Q6在应用前阻塞设计决定。发布表/manifest与最终run.result的提交须与最新身份、Issue快照一致；后续变更不能复用旧发布结论。
8. **真实部署范围（Q1）。** M5的浏览器测试是HoH验证受控工程目标的实际浏览器流程，目标fixture有真实进程、DOM/网络、数据与升级回滚断言。它不冒充HoH尚未完成的M6 Web GUI。HoH自身完整Web仍由M6 web-e2e及M7全量验收。若原范围解释要求M5先验证HoH Web，Q1必须改任务图，不能继续按当前默认签收。

## 需求对象：拟替换 requirements.json 的九个对象

以下每个JSON fence是一个完整需求对象。其余需求的字段值、数组与字段顺序保留；不改schema、policy或check-plan。本次隔离副本的应用脚本统一为UTF-8、两空格、LF，因输入文件为一空格缩进，会产生额外的纯格式diff；探针逐对象核对未改内容。正式应用宜保留原一空格缩进，避免无关diff，并重新生成/重锁/验证，不能照搬本次副本锁。

### REQ-044 · 可恢复版本（5 条 AC）

```json
{
  "id": "REQ-044",
  "category": "WORK",
  "priority": "P1",
  "milestone": "M5",
  "title": "可恢复版本",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "保存失败分支，回到已验证基线；不reset用户未提交改动",
  "acceptance": [
    {
      "id": "AC-044-01",
      "given": "同一项目已有经验证的基线 B、严重回归候选 C；原工作区含用户 tracked 修改、暂存修改与 untracked 文件；失败记录已登记",
      "when": "请求恢复 B，先在脏目录观察拒绝，再经用户处置后在 HoH 登记的干净工作区执行恢复",
      "then": "脏目录逐文件字节、index、HEAD、untracked 清单不变，停止新写派发并给出待用户处置原因；干净路径先建立指向 C 的可追溯失败分支，保留日志与证据，再建立以 B 的 tree 为基础的新恢复提交 R；B/C 的 ref 与证据 hash 不变；R 不是已验收候选，冻结 R 后必须重验，不能复用 B 的 PASS",
      "test_id": "TEST-044-01"
    },
    {
      "id": "AC-044-02",
      "given": "恢复动作具有 operation_id，持写租约；有一个由该 Attempt 登记的 worktree 与一个不属于它的同前缀目录",
      "when": "分别在保存失败 ref 后、创建恢复 worktree 后、写恢复提交后但登记前 kill host 并重启；另在安全点取消",
      "then": "同一 operation_id 最多一条完成事实和一个恢复提交；恢复先对账实际 ref/commit 与 effect，确认未知则 UNKNOWN_EFFECT、无新写派发；取消后保存可续跑点、不把部分恢复标成功；不删除失败 ref、不删除旁观目录、不结束非本 Attempt 的进程",
      "test_id": "TEST-044-02"
    },
    {
      "id": "AC-044-03",
      "given": "恢复清单绑定 run/task/attempt、spec_sha256、candidate_id、commit/tree、host/role/lease epoch；含活动工作身份、证据路径、剩余预算、next_action 和 wake_condition；摘要故意称后台工作已完成",
      "when": "中断后由新会话载入恢复清单，对照 Runtime DB、Git 与进程归属证明恢复；分别令摘要过时、引用缺失、基线摘要不符",
      "then": "实时状态为准；正在运行的工作仍列为活动且有可执行唤醒条件；next_action 随对账结果生成而非照抄；缺引用或hash不符明确阻塞恢复、不重置预算/重试、不重复投递已观察效果；历史清单保留",
      "test_id": "TEST-044-03"
    },
    {
      "id": "AC-044-04",
      "given": "代码为 C1，role_epoch 与 spec 不变；用户 SendInput STEER 已受理入队；随后新提交 C2，另有未提交的外部编辑变体",
      "when": "在 C1 下正常投递一次并重投；另在 C2 或外部编辑出现后投递 C1 入队消息，最后重新提交针对当前状态的新指令",
      "then": "入队同事务固定 code_target={git_commit,tree_sha256,candidate_id或null} 到 input_queue.payload_json 与 input.accepted，并传入 HCP/交接引用；正常消息最多一次 APPLIED；迟到消息 REJECTED，reason=CANDIDATE_MISMATCH，保留 expected/actual 身份，不原生 steer、不改变任务；外部编辑先按用户修改规则阻塞；新指令独立 input_id 才能投递；ASK/REPLY/CONTROL 的既有语义不变",
      "test_id": "TEST-044-04"
    },
    {
      "id": "AC-044-05",
      "given": "C1 上的 STEER 已入队并绑定 code_target；分别缺 candidate、尚无任何 commit、以及投递后未记回执崩溃",
      "when": "重启后重放队列，切换 role_epoch，或使 spec 修订变化，再尝试消费",
      "then": "有 commit 无 candidate 时按 commit/tree 校验而不造 candidate；无 commit 的 STEER 拒绝且给出无法绑定原因，可用 ASK 继续对话；原 code_target 永不重绑；候选/commit不符用 CANDIDATE_MISMATCH，角色不符沿用 STALE_ROLE_EPOCH，规格不符沿用 SPEC_MISMATCH；已投递但效果不明进入 UNKNOWN_EFFECT，不盲目重投；可核对的重复回执不重复 APPLIED",
      "test_id": "TEST-044-05"
    }
  ],
  "behavior": [
    "恢复以新分支/新提交保全失败版本，禁止 reset --hard、clean、覆盖用户未提交文件；用户目录脏时遵守 CR-026 O3，先停止写入并请求处置。基线须有可核对的同版本验证事实；恢复提交仍需新候选验证。",
    "恢复清单是持久 ArtifactRef，包含活动工作、精确资源归属、证据、预算、下一动作与唤醒条件；它是恢复线索，不是 Runtime/Git 的替代事实源。",
    "运行中 STEER 在受理时绑定当前代码，补齐 CR-026 M3A-02；投递前与原生调用前在同一受控投递边界复核代码及既有 epoch/spec。写者安全点序列化绑定/提交/投递；未提交的外部修改按用户修改规则停写。",
    "本稿不加 SendInput 外部字段：code_target 为 host 产生的内部元数据，存于既有自由 JSON 与事件中；客户端显示受理时绑定的版本。针对已展示旧版本的显式选择作为 Q2，不虚称入队前的用户意图已获校验。"
  ],
  "failure_modes": [
    "失败分支无法保存、无有效验证基线、Git hash 不一致或空间不足：先保留现场并阻塞，不切换目录。",
    "新身份不接收旧 STEER；不能把消息重投时的新 HEAD 补写成原目标。不可确认的原生投递不声称 exactly-once。"
  ],
  "doc_refs": [
    "docs/14-resilience.md",
    "docs/06-orchestration.md",
    "docs/cr/CR-026-m3-detailed-spec.md",
    "docs/adr/ADR-018.md",
    "docs/adr/ADR-031.md"
  ],
  "decisions": [
    "Q2 推荐受理时的代码快照，覆盖 M3A-02 明确要求；需要客户端显式历史目标时先做新的协议细化，不默许重定向。",
    "不新增 Shell 子命令；恢复使用现有 Run/问题答复及内部恢复能力。"
  ]
}
```

### REQ-048 · Smart Compact集成（4 条 AC）

```json
{
  "id": "REQ-048",
  "category": "PLUGIN",
  "priority": "P1",
  "milestone": "M5",
  "title": "Smart Compact集成",
  "required_release": true,
  "release_class": "degradable",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "使用指定作者插件，保留任务/证据引用；自动压缩后继续；长期记忆不双开",
  "acceptance": [
    {
      "id": "AC-048-01",
      "given": "锁定 alpertarhan/pi-smart-compact 的真实 Pi 非 Commander 会话，任务/AC/候选/证据引用齐全，Hermes 为唯一长期记忆所有者",
      "when": "使会话达到安装版本的自动压缩条件，观察真实 compact 开始/完成，再执行一个仍属于原 Task 的工程步骤",
      "then": "会话自动继续，任务/AC/spec/candidate 与证据引用可重新解析且hash相同；日志证明加载指定来源版本并发生真实压缩，不能仅手工写摘要；Smart Compact 的可选跨会话记忆后端未运行，Hermes 仍是唯一长期记忆后端",
      "test_id": "TEST-048-01"
    },
    {
      "id": "AC-048-02",
      "given": "压缩摘要称 Task A 未完成、候选 C1 有效，Runtime 已接受 A 并采用 C2；另有旧 role_epoch 输入",
      "when": "压缩恢复后读取实时状态再继续，并重复恢复通知",
      "then": "恢复工作依据 Runtime/Git 的 C2 与当前 epoch；不重做 A、不把 C1 的验收计 current；摘要冲突可见并有来源，重复通知最多一次状态应用；旧 epoch 输入只记历史",
      "test_id": "TEST-048-02"
    },
    {
      "id": "AC-048-03",
      "given": "真实 Pi 会话有已 observed 的工具效果与尚未确认的外部效果，正在压缩",
      "when": "分别在 compact 期间中断、取消、重启后续会话，注入重复 compact 通知",
      "then": "已观察效果不重复执行；未知效果先 reconcile，不确认就 UNKNOWN_EFFECT；取消不产生完成claim或新写派发；恢复后保留任务/证据/预算身份，不能以压缩清零失败次数",
      "test_id": "TEST-048-03"
    },
    {
      "id": "AC-048-04",
      "given": "已安装版本的 Compact 单插件、Compact+Hermes、三插件组合；记录 hook 与模型调用计数",
      "when": "在每个组合触发一次实际压缩并在 hook 内重入触发，另使 Hermes flush 失败",
      "then": "每个压缩周期 flush 成功先于 compact 完成；同周期只进入一次 compact，重入被拒或合并且有日志；额外模型调用逐次可追溯，不产生无限递归；flush失败不丢经验，不把未完成压缩标成功；不支持所需hook的真实版本按CR-001带证据降级",
      "test_id": "TEST-048-04"
    }
  ],
  "behavior": [
    "仅 alpertarhan 指定来源负责压缩连续性；版本和配置来自安装包可核对内容，不发明设置键。",
    "压缩后强制加载 Runtime 的当前任务、候选、证据、队列与预算，摘要只作线索（docs/24 §3.3）。",
    "与 Hermes 约定 flush→compact→resume 顺序并有重入保护；所有组合经真实 Pi 会话验证。"
  ],
  "failure_modes": [
    "缺插件、上游不兼容或 hook 能力不足只按共同降级规则处理；没有真实会话不能报 live PASS。",
    "认证缺失或provider临时不可用不自动冒充上游插件不兼容；保留 BLOCKED/NOT_RUN 与恢复条件。"
  ],
  "doc_refs": [
    "docs/15-prompts-skills-memory.md",
    "docs/adr/ADR-007.md",
    "config/plugins.json",
    "docs/24-dev-lessons-to-product.md"
  ],
  "decisions": [
    "Q4：使用当前锁定来源作为起点；版本是否能兑现hook由实现时实测决定。",
    "Pi Commander 按 CR-024 禁扩展，这三插件仅服务其他 Pi 职位。"
  ]
}
```

### REQ-049 · Hermes Memory集成（4 条 AC）

```json
{
  "id": "REQ-049",
  "category": "PLUGIN",
  "priority": "P1",
  "milestone": "M5",
  "title": "Hermes Memory集成",
  "required_release": true,
  "release_class": "degradable",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "经验标记来源/项目/失效条件；关键验收不靠召回；并发维护可恢复",
  "acceptance": [
    {
      "id": "AC-049-01",
      "given": "项目 A 的真实 Hermes 会话记录一次已验证失败经验，项目 B 含同名文件；经验带 source ArtifactRef、project、confidence、validity 与观察时间",
      "when": "在 A 的新会话召回并在 B 中作相同查询；另显式授权一条可跨项目的通用经验",
      "then": "A 可召回原经验且引用hash/观察时间可核对；B 默认不见 A 的项目专属内容，只有显式通用项可跨项目；召回不产生 acceptance或Issue解决事实；无来源/缺适用范围的经验标未验证且不进硬上下文",
      "test_id": "TEST-049-01"
    },
    {
      "id": "AC-049-02",
      "given": "用户、反馈、项目、参考四类经验，各带 observed_at、validity、source；一条过时，一条与现规格冲突，一条收到用户纠正",
      "when": "下一会话召回，应用纠正/失效，重建索引后再次查询",
      "then": "保留分类与适用条件，显示观察时间或可核对年龄；失效项从活动召回撤下，纠正项取代旧活动内容且可追溯来源；不能修改HoH append-only证据；冲突以当前规格/Runtime为准，硬规格未召回仍由强制上下文提供",
      "test_id": "TEST-049-02"
    },
    {
      "id": "AC-049-03",
      "given": "两个真实 Pi 会话维护同一项目 Hermes 存储，另有不属于本会话的锁/目录",
      "when": "并发写不同经验，持锁时杀掉一个维护进程后恢复，另注入空间不足",
      "then": "已确认写入不丢且不重复；冲突有可观察等待/拒绝，恢复先核对锁持有者完整进程身份；不删除他人活锁/目录；失败可重试且保留原经验；恢复后的存储通过安装版本支持的完整性核对",
      "test_id": "TEST-049-03"
    },
    {
      "id": "AC-049-04",
      "given": "Hermes 召回文本声称某 required AC 已PASS及某 high Issue 已修复，但当前候选没有对应证据",
      "when": "将召回注入真实 Pi 上下文并提交其总结；再提供真实检查失败作为反例",
      "then": "Runtime的accepted、Issue状态及发布门禁不受记忆claim改变；缺证据仍未满足，失败仍阻断；能定位召回的来源与冲突原因；无召回时规格/AC/候选强制项仍完整",
      "test_id": "TEST-049-04"
    }
  ],
  "behavior": [
    "Hermes 是项目/跨会话经验检索，不是任务或验收权威；事实证据仍在HoH。",
    "经验带来源、项目、confidence、validity、时间及类别；撤回过时活动记忆可以整理索引，但不可删除HoH证据链。",
    "并发维护遵守安装版本的锁与恢复协议；重启前先对账，不按目录名模式清理。"
  ],
  "failure_modes": [
    "缺来源、项目串读、存储损坏及锁冲突明确诊断；无法保证边界的版本按共同规则降级。",
    "provider 过滤截断保留原始原因，不能说成记忆成功或Agent能力不足。"
  ],
  "doc_refs": [
    "docs/15-prompts-skills-memory.md",
    "docs/24-dev-lessons-to-product.md",
    "docs/adr/ADR-007.md",
    "docs/14-resilience.md"
  ],
  "decisions": [
    "Q5 推荐四类元数据由HoH适配层补齐，使用已核对的插件存储接口，不能另建全局经验事实账本。"
  ]
}
```

### REQ-050 · rpiv Todo集成（3 条 AC）

```json
{
  "id": "REQ-050",
  "category": "PLUGIN",
  "priority": "P1",
  "milestone": "M5",
  "title": "rpiv Todo集成",
  "required_release": true,
  "release_class": "degradable",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "保持会话todo；可映射展示；todo完成不代表全局任务验收",
  "acceptance": [
    {
      "id": "AC-050-01",
      "given": "真实 Pi 非Commander会话的 @juicesharp/rpiv-todo 含3个局部todo，绑定run/task/attempt/native session；HoH任务尚未验收",
      "when": "完成一项后reload和compact，恢复同会话，再开独立新会话",
      "then": "同会话3项身份/顺序/状态保持，新会话不自动继承；展示映射可定位全局Task和原会话；local completed不写Task ACCEPTED或requirement accepted；安装清单无整个rpiv pipeline",
      "test_id": "TEST-050-01"
    },
    {
      "id": "AC-050-02",
      "given": "Pi局部todo全部完成，HoH对应AC有FAIL、Task已有正式领取记录，恢复清单有活动工作与next_action",
      "when": "读取TUI只读映射与状态栏，再让Pi把所有todo改completed",
      "then": "视图明确local todo和全局Task两套语义；展示领取身份、候选、证据与下一动作；accepted仍按Runtime验收不增长，FAIL仍显示；无Task领取/映射时显示未关联，不伪造全局进度",
      "test_id": "TEST-050-02"
    },
    {
      "id": "AC-050-03",
      "given": "两个Pi会话相同todo文本、一个旧epoch会话和一个当前会话；当前会话在写todo时中断",
      "when": "重放旧会话消息并恢复当前会话，取消全局Task后再接收迟到todo",
      "then": "不同session的局部id不碰撞；旧epoch与取消后内容只能作为历史展示、不更新当前映射；恢复无重复todo、不把未确认写入当完成；局部失败不删除全局Task/证据",
      "test_id": "TEST-050-03"
    }
  ],
  "behavior": [
    "只安装 rpiv-todo，不安装整个rpiv工作流；todo只表示Pi会话内子步骤。",
    "映射键含Run/Task/Attempt/session及局部id；只读展示可关联证据与恢复点，但不允许todo驱动全局acceptance。",
    "compact/reload恢复与跨会话隔离都在真实组合中测；docs/24 §4.2 的开工领取属于HoH，todo不能补造。"
  ],
  "failure_modes": [
    "插件状态不可读显示未知，不补成completed；映射缺失显示未关联。",
    "新会话、旧epoch、取消后迟到输出不覆盖当前局部映射。"
  ],
  "doc_refs": [
    "docs/15-prompts-skills-memory.md",
    "docs/adr/ADR-007.md",
    "docs/24-dev-lessons-to-product.md"
  ],
  "decisions": [
    "依赖三插件组合与M4状态栏，验证时展示真实Pi todo导出，不用Fixture冒充插件。"
  ]
}
```

### REQ-051 · 三插件兼容与安装（5 条 AC）

```json
{
  "id": "REQ-051",
  "category": "PLUGIN",
  "priority": "P1",
  "milestone": "M5",
  "title": "三插件兼容与安装",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "解析固定版本、校验依赖、无手动Skill安装；不支持RPC UI有明确降级",
  "acceptance": [
    {
      "id": "AC-051-01",
      "given": "隔离的空插件目录、本地fixture registry含有效/损坏包；固定Pi与三插件版本/完整性；RPC无custom UI；两个compact hook",
      "when": "通过产品准备默认组合，依次注入无UI、hook冲突和安装失败，再运行doctor",
      "then": "成功时校验来源/版本/integrity/依赖，原子发布整套锁；无需手工安装Skill；HoH设置入口使用已核实配置adapter，不要求Pi custom TUI；失败时保留上一可用组合，每个插件有明确diagnostic/上游引用，继续不依赖插件的任务；双hook不悄悄运行两套压缩",
      "test_id": "TEST-051-01"
    },
    {
      "id": "AC-051-02",
      "given": "本地registry有篡改hash、缺依赖、不支持Pi版本、错误作者来源四组包，活动Run已固定旧组合",
      "when": "分别准备新组合，并尝试活动Run中升级；维护窗口在安全点后再重试",
      "then": "四个坏组合全部拒绝，不写有效锁、不执行未经验证包；活动Run继续旧锁；只有维护窗口且无使用中会话时发布新组合，新Run采用新锁；doctor列出期望/实际与修复动作，不能报告兼容PASS",
      "test_id": "TEST-051-02"
    },
    {
      "id": "AC-051-03",
      "given": "已安装旧组合，升级operation_id已登记；另有其它会话同前缀目录及活进程",
      "when": "在下载、校验、暂存完成、原子切换前后分别中断/取消/kill并恢复",
      "then": "每次恢复只见完整旧组合或完整新组合；不出现混合锁；取消不发布未完成组合；只清理本operation登记的精确路径和带启动身份的进程，旁观资源不变；失败记录保留且重试幂等",
      "test_id": "TEST-051-03"
    },
    {
      "id": "AC-051-04",
      "given": "本地故障注入上游404/不兼容/安装失败及缺认证/429/provider内容过滤；required AC和degradable AC各一组",
      "when": "生成诊断与模拟发布输入，逐项删去diagnostic/upstream_ref或把REQ-051标DEGRADED",
      "then": "仅符合CR-001的插件不可用可形成带执行绑定、diagnostic ArtifactRef与真实来源引用的降级；缺字段拒绝；REQ-051及任何required检查永不以DEGRADED满足；缺认证/额度/临时故障/内容过滤保持各自阻塞原因，过滤记录failure_class=provider_content_filter的诊断并交现有决策路径，不假称能力缺失、不自动耗尽重试",
      "test_id": "TEST-051-04"
    },
    {
      "id": "AC-051-05",
      "given": "隔离配置中启用三插件；Commander选择Pi，QA选择Pi；RPC schema与实际配置adapter匹配/不匹配各一例",
      "when": "构造启动argv与会话目录，保存插件设置，再重开并运行doctor",
      "then": "Commander argv保留CR-024禁扩展且无显式-e；QA加载锁定三插件；设置保存有校验、原子写与重开一致，不写原生用户目录；adapter不匹配拒绝并保旧配置；无UI能力明确降级说明及HoH设置入口；doctor能区分角色差异",
      "test_id": "TEST-051-05"
    }
  ],
  "behavior": [
    "安装/诊断/显式降级自身为required；以本地registry和故障注入验证确定性逻辑，实际插件行为另由plugin-live证明。",
    "组合锁绑定Pi版本、每插件来源/包名/精确版本/integrity、配置schema摘要与hook策略；Run启动快照固定，维护窗口更新不改活动Run。",
    "保留Smart Compact/Hermes/rpiv职责与Pi Commander禁扩展；no-UI通过HoH已有设置/doctor入口诊断适配，不扩展Shell命令面。",
    "新增provider过滤仅为诊断failure_class，保留原生错误与现有阻塞语义，不新增ErrorCode枚举、不猜fallback成功。"
  ],
  "failure_modes": [
    "损坏包、作者来源错、依赖冲突、RPC能力缺失：拒绝未验证组合，保留旧组合与错误。",
    "安装取消、磁盘不足、崩溃不得留下被认作有效的半安装；不写用户原生Agent配置目录。"
  ],
  "doc_refs": [
    "docs/15-prompts-skills-memory.md",
    "docs/13-adapters.md",
    "docs/adr/ADR-007.md",
    "docs/adr/ADR-032.md",
    "config/plugins.json"
  ],
  "decisions": [
    "plugin-live的唯一维护者保留M5-T03；该任务交付可失败的真实驱动，后继集成任务逐步补齐，未实现项必须NOT_RUN/失败。",
    "Q4：只在真实上游证据满足CR-001时降级；身份/认证问题不等于上游不兼容。"
  ]
}
```

### REQ-055 · 回归与Flaky（5 条 AC）

```json
{
  "id": "REQ-055",
  "category": "VERIFY",
  "priority": "P1",
  "milestone": "M5",
  "title": "回归与Flaky",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "打开regression issue；保留所有attempt；flaky不被覆盖成纯PASS",
  "acceptance": [
    {
      "id": "AC-055-01",
      "given": "B上功能a已验证；C修复b却使a回归；同一候选同一TEST有FAIL后PASS，并有一组超时无结论后PASS的对照",
      "when": "按影响闭包选择测试，运行失败与重试，再查询候选投影与issue",
      "then": "回归打开可定位a/原基线/新候选的regression Issue；全部attempt与stdout/stderr保留；FAIL→PASS为FLAKY且不满足strict；无结论超时→PASS按ADR-027/029求值，不错误混同有结论失败；运行顺序不改变历史聚合",
      "test_id": "TEST-055-01"
    },
    {
      "id": "AC-055-02",
      "given": "模块依赖图含传递依赖，另有未知影响文件；测试矩阵锁定Ubuntu24.04/26.04与工具版本，含Python3.12/3.14路径处理差异",
      "when": "对已知修改求闭包，对未知影响及release请求分别选检查，执行一个只在矩阵一格失败的真实用例",
      "then": "已知闭包包含所有传递相关AC/TEST；未知影响与发布选full required suite；矩阵每格独立记录版本/结果，任一required格失败不被其它格PASS覆盖；Python工具兼容边界及过长文件名有断言，缺环境不计PASS",
      "test_id": "TEST-055-02"
    },
    {
      "id": "AC-055-03",
      "given": "同一冻结候选上并行的QA与Reviewer，各有独立证据根、worktree、进程身份；第三方同前缀目录内有哨兵",
      "when": "运行可能生成缓存的检查，取消一方并执行清理；另注入验证者写入候选与嵌套检查继承外层证据环境",
      "then": "缓存/字节码/输出均在登记的候选外目录；前后Git tree及未跟踪清单不变；污染时候选检查无效，不签PASS；只清理本方登记资源，另一方与哨兵不变；内层剥离外层HOH检查变量，外层无串入test行；检查恢复默认信号处理并正常接收取消",
      "test_id": "TEST-055-03"
    },
    {
      "id": "AC-055-04",
      "given": "同一缺陷family连续两轮出现但其它AC仍增长；互补Reviewer使用不同编号段，对同一缺陷给出相反结论；另一序列达到4轮与5轮",
      "when": "按本稿审查收敛规则推进到上限，重启后继续并尝试仅凭多数意见解除",
      "then": "连续两轮即要求带证据的设计修正，不能被其它进展重置；发现按session+finding_id唯一，语义去重保留各来源；冲突按复现事实判定；第4轮无critical/high时medium全部修复并红绿、由主会话核对后结束审查，low入里程碑清理包；第4轮仍有critical/high则设计修正后仅一次新会话第5轮核对，仍有则BLOCKED；重启不清轮数；轮数上限从不把FAIL/未满足AC变PASS",
      "test_id": "TEST-055-04"
    },
    {
      "id": "AC-055-05",
      "given": "候选C的轮次级与汇合级FLAKY各一组，已保留FAIL/PASS历史；原栅栏J已有joined事实",
      "when": "请求处置，先尝试同内容refreeze/重跑/隔离required测试，再用新提交C2修复并独立验证",
      "then": "前3种不能使C满足；处置保留C历史并要求新候选，C2重新绑定全套相关证据；解除汇合级flaky产生新栅栏编号且不复用J（ADR-029 J9）；没有删除/改写失败记录或required测试，未通过保持阻塞",
      "test_id": "TEST-055-05"
    }
  ],
  "behavior": [
    "回归与flaky采用ADR-027/029既有历史规则；严格门禁不允许重试洗白、隔离required测试或删除失败attempt。",
    "docs/24 §2.3 的两轮同类复发与审查轮数上限是工作流收敛，不是证据豁免；精确规则见本CR共同约定。",
    "回归选择依据模块→依赖→AC/TEST闭包，未知影响全套；发布始终全部required。",
    "验证者不写冻结候选；资源清理限自己登记的精确路径和完整进程身份，嵌套执行证据与信号隔离。"
  ],
  "failure_modes": [
    "同候选相反结论保留并诊断，不能多数票改写事实。",
    "不能复现或缺环境是UNKNOWN/NOT_RUN/阻塞，不因为审查上限而通过；flaky的旧历史不可删除。"
  ],
  "doc_refs": [
    "docs/07-algorithms.md",
    "docs/19-ai-development.md",
    "docs/24-dev-lessons-to-product.md",
    "docs/adr/ADR-027.md",
    "docs/adr/ADR-028.md",
    "docs/adr/ADR-029.md",
    "docs/cr/CR-020-acceptance-hand-overs.md"
  ],
  "decisions": [
    "Q3 推荐审查系列的持久计数与设计修正工件；设计和实现系列分开，设计修正不重置计数。",
    "本稿不改变REQ-021/040已有AC，把其监督触发器作为此需求的依赖。"
  ]
}
```

### REQ-056 · 最终发布门禁（5 条 AC）

```json
{
  "id": "REQ-056",
  "category": "VERIFY",
  "priority": "P1",
  "milestone": "M5",
  "title": "最终发布门禁",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "所有required需求与检查有同版本有效证据；空集合、skip、mock不能冒充真实",
  "acceptance": [
    {
      "id": "AC-056-01",
      "given": "产品发布判定器有完整正对照输入，绑定candidate/tree/spec/environment/test_plan；另分别构造空AC/空检查、缺test、skip/mock、hash错、旧规格/候选、独立会话复用、缺原生记录、缺live agent、soak不足、遗留high",
      "when": "经Runtime请求发布，对每个负例只破坏一个条件并读取最终结果",
      "then": "正对照仅在全部规则满足才有可发布结论；全部负例逐项拒绝并给出缺项；只通过候选或里程碑/v0.1不能输出v1 COMPLETED；模型claim/截图/进程exit0不替代证据；fixture正对照只证明判定器，绝非HoH v1实测完成",
      "test_id": "TEST-056-01"
    },
    {
      "id": "AC-056-02",
      "given": "同Run的两个开发Task已在不同增量候选接受，最终C为ACCEPTED但仅覆盖最后增量；具备持久release_validation_id",
      "when": "Runtime触发全范围验证，重复release_request并在两方结果收取之间重启；另在期间换spec/candidate或取消",
      "then": "全范围包含profile每项required AC/check，不开verification.round_opened，不迁移源Task与候选状态；可对ACCEPTED的C运行；同请求只有一个活动验证义务，QA/Review为不同独立会话并保留真实绑定；重启收取去重；换身份旧结果仅历史、取消停止发布；全量结果齐全且合法之前Run不COMPLETED",
      "test_id": "TEST-056-02"
    },
    {
      "id": "AC-056-03",
      "given": "最终候选的PASS记录分别缺v1所需codex/claude/pi或plugin列表；合法插件DEGRADED对照具有诊断与上游引用；REQ-051为required",
      "when": "用v0.1/v1各自profile调用发布投影，并篡改DEGRADED检查下的AC为PASS或将required改成DEGRADED",
      "then": "RowFact.live_agents来自证据live-sessions，不来自模型claim；有profile时按规则16仅过滤outcome=PASS产生的不合要求PASS行；合规插件降级按policy满足并输出COMPLETED_DEGRADED及明细；required降级、DEGRADED检查引用PASS AC、缺诊断/上游均拒绝；v0.1最多INTERIM_RELEASED",
      "test_id": "TEST-056-03"
    },
    {
      "id": "AC-056-04",
      "given": "Run遗留OPEN/REOPENED critical/high，包括已取消源Task；物化state故意滞后；同fingerprint多Issue与超过50项确认集；当前候选比原Issue新",
      "when": "全范围QA/Review分批收到确认条目，分别确认RESOLVED/OPEN/缺观察，并在同候选追加present或INVALID事实",
      "then": "待确认集合与open_issues在同一读快照按ADR-029状态函数求值；仅两方实际送达交集、至少一方有RESOLVED观察且双方无present才写absent并同事务settle_all；验收失败Issue由新行求解；未送达/未确认/同候选present/否决均仍阻断；含取消Task且不因批次上限漏项；出口仅重验/返工/取消且阻断保持，豁免不由Agent决定",
      "test_id": "TEST-056-04"
    },
    {
      "id": "AC-056-05",
      "given": "完整发布输入的最终产物与build声明hash相同，release_validation已完成；还有最后提交前后的kill点与重复release_request",
      "when": "提交发布记录时分别kill并恢复；再修改产物hash、追加high或令候选过期后请求发布",
      "then": "manifest/发布记录/最终run.result一致且幂等，不出现只写COMPLETED无可核对manifest；提交前再次核对当前spec/candidate、Issue和产物；新high/过期/hash错拒绝；重启不重造证据、不以旧release快照接受新状态；最终状态仅由Runtime判定",
      "test_id": "TEST-056-05"
    }
  ],
  "behavior": [
    "严格区分Readiness、Candidate与Release Gate；完整目标只有Runtime全量规则成立才完成。",
    "承接CR-018全范围验证与CR-020的profile、遗留Issue、同事务settle_all；独立发布验证义务不打开增量验证轮次，不借最后Task范围代替完整目标。",
    "发布验证使用持久release_validation_id绑定run/spec/candidate/环境/计划/profile/两方Attempt；事件为权威，重新收取幂等。报告验证复用身份/hash/会话独立规则，但不得通过伪造round_of_task挂到增量轮次。具体接线ADR由M5-T07在TDD前审查。",
    "本需求的integration正反例可用受控证据fixture测试判定逻辑；最终HoH v1的三Agent/插件/Web/soak仍由M7-T04真实执行，不能从fixture导出发布声明。"
  ],
  "failure_modes": [
    "缺检查、空集合、过期身份、被否决证据、未确认critical/high均fail-closed；四轮审查上限不是门禁例外。",
    "全量验证期间换候选/规格或取消：旧结果只留历史，不能推进新身份；无法证明完整性不提交成功发布。"
  ],
  "doc_refs": [
    "docs/06-orchestration.md",
    "docs/07-algorithms.md",
    "docs/10-acceptance.md",
    "docs/adr/ADR-027.md",
    "docs/adr/ADR-028.md",
    "docs/adr/ADR-029.md",
    "docs/cr/CR-018-verification-hand-overs.md",
    "docs/cr/CR-020-acceptance-hand-overs.md"
  ],
  "decisions": [
    "保持所有release-policy阈值、release_class与v1完整范围；不提前把M7的traceability/independent检查argv换成v1。",
    "Q6 推荐独立的release验证义务，以事件/工件和既有只读Attempt承载；如果现有契约不能表达，先补细化CR并重验，不能借增量轮次绕过CR-018。"
  ]
}
```

### REQ-057 · 运行时UX与部署验证（4 条 AC）

```json
{
  "id": "REQ-057",
  "category": "VERIFY",
  "priority": "P1",
  "milestone": "M5",
  "title": "运行时UX与部署验证",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "执行PTY/浏览器/安装/升级/回滚实际流程；不是截图看起来不错就通过",
  "acceptance": [
    {
      "id": "AC-057-01",
      "given": "Ubuntu24.04与26.04 x86_64的全新隔离VM，候选构建包；三个受控目标项目分别为CLI/Web/service，均有可运行的vA/vB与数据；浏览器驱动已安装",
      "when": "通过HoH验证流程实际安装vA、运行CLI PTY/浏览器关键操作/service请求，再升级vB并回滚vA，保存每步原始输出",
      "then": "两个OS分别产生安装/启动/功能/升级/回滚记录及版本hash；PTY输入响应与退出、浏览器DOM/网络行为、service健康与数据均有断言；回滚后旧版本及原数据可用；截图只作辅证；不是在开发机换HOME冒充fresh VM，缺VM/驱动不能PASS",
      "test_id": "TEST-057-01"
    },
    {
      "id": "AC-057-02",
      "given": "已有M4完整TUI与运行中的后台host，受控工程目标的检查有PASS/FAIL/NOT_RUN与可点击证据",
      "when": "用真实PTY操作查看检查、打开证据、脱离并重附，分别暂停与取消",
      "then": "用户可从Task/Issue定位原始证据与候选，三种结果文字可区分；脱离不停止host或已接受任务，暂停/取消只走对应命令；截图成功不掩盖检查FAIL；关闭/重附后结果一致且无重复派发",
      "test_id": "TEST-057-02"
    },
    {
      "id": "AC-057-03",
      "given": "与057-01相同两种fresh VM；安装包损坏、依赖缺失、端口占用、迁移失败、旧binary面对高版本DB各一例",
      "when": "通过实际部署/升级路径逐项触发故障，再恢复前置条件并重试",
      "then": "每例非成功退出且有原因/恢复动作，保留上一可用安装与备份；不宣称升级/回滚成功，不吞DB版本错误；修复后同路径可重试且不重复系统资源；旧binary不读高版本DB，回滚只对经校验旧备份进行",
      "test_id": "TEST-057-03"
    },
    {
      "id": "AC-057-04",
      "given": "目标项目计划声明PTY/浏览器/部署检查与必需观测；有只截图、零断言、缺browser动作、退出0但FAIL、未运行五组反例",
      "when": "Runtime收取检查记录并构造验证/发布汇总，再读TUI证据摘要",
      "then": "五组均不能成为有效PASS；保留stdout/stderr/命令/环境/版本与逐测试状态；完整流程正对照可被引用；无法运行明确NOT_RUN/BLOCKED，不伪造browser/live；HoH自身完整Web e2e仍归M6及最终v1验收，不被此目标项目fixture替代",
      "test_id": "TEST-057-04"
    }
  ],
  "behavior": [
    "Q1推荐解释：本需求实现HoH对CLI/Web/service工程目标的真实UX/部署验证能力；M5浏览器操作受控的真实目标Web应用。HoH自身完整Web GUI仍为REQ-058..061/M6与v1 web-e2e硬门禁，不能省略。",
    "fresh VM逐OS独立执行，不把容器/临时HOME替代VM；相同候选构建包、原始日志与实际版本/数据结果可核对。",
    "接入既有deployment/tui-pty/integration检查，不抢check owner、不更改policy；检查脚本与test-results逐项报告。"
  ],
  "failure_modes": [
    "只有截图或退出0无行为断言不能通过；测试基础设施缺失报告未运行并阻断required验收。",
    "升级/回滚失败保留旧安装与已验证备份；禁止旧binary直接打开高schema。"
  ],
  "doc_refs": [
    "docs/09-testing.md",
    "docs/10-acceptance.md",
    "docs/11-deployment.md",
    "docs/adr/ADR-009.md",
    "docs/adr/ADR-021.md"
  ],
  "decisions": [
    "Q1若CTO判定原AC必须在M5验证HoH完整Web GUI，则本任务依赖不闭合，须重排M5/M6而不能把该部分降级或记PASS。",
    "安装/升级/回滚的验收标准不含未经执行的用户命令示例；实际文档随实施实测更新。"
  ]
}
```

### REQ-063 · 备份与迁移（4 条 AC）

```json
{
  "id": "REQ-063",
  "category": "DATA",
  "priority": "P1",
  "milestone": "M5",
  "title": "备份与迁移",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "一致性备份并校验；新旧schema兼容策略；旧binary不读高版本DB",
  "acceptance": [
    {
      "id": "AC-063-01",
      "given": "真实SQLite WAL活跃写入，DB引用规格/证据/公开流对象；已确认记录集K，每笔写入带唯一序号；含不会再修改的对象与未登记临时文件",
      "when": "通过产品一致性backup能力并发写入，完成后在独立目录restore并执行integrity_check与逐引用hash核对",
      "then": "恢复DB是一个可说明水位H的完整事务快照，H内已确认记录与outbox/effect/预算不丢，无半事务；所有被引用对象齐全且hash一致，未登记临时文件不变成有效引用；源库继续工作；不是只copy主DB文件",
      "test_id": "TEST-063-01"
    },
    {
      "id": "AC-063-02",
      "given": "上一受支持schema的真实旧binary造库与备份；当前binary含正式编号迁移；另有高于支持范围的DB",
      "when": "依次升级、重开校验、用旧binary打开升级库，再执行回滚到旧备份并重开；迁移中逐点kill",
      "then": "迁移前后integrity_check通过，user_version只在整笔成功后改变，重复启动不重放迁移；旧binary打开高版本DB在任何业务读写/派发前拒绝且库字节/版本不变；回滚恢复旧备份与匹配binary，不做未声明的down migration；kill后是完整旧或完整新schema",
      "test_id": "TEST-063-02"
    },
    {
      "id": "AC-063-03",
      "given": "backup/restore操作已登记，目标有既存库；分别令DB/对象hash损坏、对象缺失、备份目标满盘、只读及SQLITE_BUSY",
      "when": "尝试生成备份及恢复，在临时落盘、fsync、rename和完成登记处中断后重启",
      "then": "无完整manifest和校验的备份不能标成功；恢复校验失败不替换原库，原数据hash不变；取消/重启只见旧库或全部验证的新库，孤立临时资源按operation归属清理；BUSY有界重试/可诊断退出，不阻塞取消与读取",
      "test_id": "TEST-063-03"
    },
    {
      "id": "AC-063-04",
      "given": "活动host/Attempt存在，备份绑定project_id/run/spec/schema/资源版本；另有错误项目备份、旧schema但不支持的迁移链",
      "when": "先请求恢复活动库，再在停派发/所有Attempt安全收尾并取得独占项目锁后恢复正确/错误备份",
      "then": "活动状态拒绝破坏性替换，返回待静止条件；独占恢复只接受匹配项目与受支持迁移链；恢复后先reconcile、递增host_epoch/fence旧租约，不盲目重投started效果；不同项目/不支持版本拒绝并保原库；恢复进度/取消可查",
      "test_id": "TEST-063-04"
    }
  ],
  "behavior": [
    "采用SQLite在线备份API得到DB一致性快照，再按该快照的引用闭包复制不可变对象并生成manifest；固定读水位、GC保留pin覆盖复制期，不能只复制主DB或遗漏WAL。",
    "备份manifest绑定project/run/spec/schema、工具及资源版本、水位、每个文件hash与完成状态；原子发布后才可供恢复；操作归属与effect可对账。",
    "恢复是维护操作：停止新派发、等待或明确取消所有活动Attempt、持项目独占锁，在临时目录完整校验再原子替换；恢复后执行正常reconcile与epoch fencing。",
    "迁移采用新编号、事务与user_version；应用时重核M4迁移003及现有最新schema，绝不编辑已采纳旧迁移。"
  ],
  "failure_modes": [
    "损坏/缺失对象、满盘、BUSY、不同项目、未知schema均明确失败且保原库；旧binary不能读高schema业务数据。",
    "不在活动库上用copy覆盖，不靠down migration假称回滚；删除/清理不越operation资源归属。"
  ],
  "doc_refs": [
    "docs/14-resilience.md",
    "docs/adr/ADR-009.md",
    "docs/adr/ADR-016.md",
    "contracts/sql/001_initial.sql"
  ],
  "decisions": [
    "Q7 推荐通过现有维护/诊断入口及内部服务接口提供backup/restore；不新增hoh backup/restore Shell命令。",
    "M4对象不可变与GC引用保护是未应用前提A2，实施前必须重核并补backup pin。"
  ]
}
```

## 场景对象：拟替换/追加 acceptance-scenarios.json

保持其余场景不变；本数组精确替换九项需求的所有场景。原始日志必须来自指定check的执行目录；子例不得仅检查日志包含“PASS”。

```json
[
  {
    "id": "TEST-044-01",
    "requirement_id": "REQ-044",
    "acceptance_id": "AC-044-01",
    "check_id": "recovery",
    "fixture": "同一项目已有经验证的基线 B、严重回归候选 C；原工作区含用户 tracked 修改、暂存修改与 untracked 文件；失败记录已登记",
    "steps": [
      "请求恢复 B，先在脏目录观察拒绝，再经用户处置后在 HoH 登记的干净工作区执行恢复",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "脏目录逐文件字节、index、HEAD、untracked 清单不变，停止新写派发并给出待用户处置原因；干净路径先建立指向 C 的可追溯失败分支，保留日志与证据，再建立以 B 的 tree 为基础的新恢复提交 R；B/C 的 ref 与证据 hash 不变；R 不是已验收候选，冻结 R 后必须重验，不能复用 B 的 PASS"
    ],
    "observables": [
      "Git refs/tree/index与逐文件hash",
      "失败分支与恢复事件",
      "候选及证据清单"
    ]
  },
  {
    "id": "TEST-044-02",
    "requirement_id": "REQ-044",
    "acceptance_id": "AC-044-02",
    "check_id": "recovery",
    "fixture": "恢复动作具有 operation_id，持写租约；有一个由该 Attempt 登记的 worktree 与一个不属于它的同前缀目录",
    "steps": [
      "分别在保存失败 ref 后、创建恢复 worktree 后、写恢复提交后但登记前 kill host 并重启；另在安全点取消",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "同一 operation_id 最多一条完成事实和一个恢复提交；恢复先对账实际 ref/commit 与 effect，确认未知则 UNKNOWN_EFFECT、无新写派发；取消后保存可续跑点、不把部分恢复标成功；不删除失败 ref、不删除旁观目录、不结束非本 Attempt 的进程"
    ],
    "observables": [
      "effect ledger与operation_id",
      "host恢复日志",
      "ref/commit数量",
      "旁观目录hash与进程身份"
    ]
  },
  {
    "id": "TEST-044-03",
    "requirement_id": "REQ-044",
    "acceptance_id": "AC-044-03",
    "check_id": "recovery",
    "fixture": "恢复清单绑定 run/task/attempt、spec_sha256、candidate_id、commit/tree、host/role/lease epoch；含活动工作身份、证据路径、剩余预算、next_action 和 wake_condition；摘要故意称后台工作已完成",
    "steps": [
      "中断后由新会话载入恢复清单，对照 Runtime DB、Git 与进程归属证明恢复；分别令摘要过时、引用缺失、基线摘要不符",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "实时状态为准；正在运行的工作仍列为活动且有可执行唤醒条件；next_action 随对账结果生成而非照抄；缺引用或hash不符明确阻塞恢复、不重置预算/重试、不重复投递已观察效果；历史清单保留"
    ],
    "observables": [
      "恢复清单及hash",
      "reconcile前后快照",
      "活动身份与唤醒条件",
      "派发和预算记录"
    ]
  },
  {
    "id": "TEST-044-04",
    "requirement_id": "REQ-044",
    "acceptance_id": "AC-044-04",
    "check_id": "integration",
    "fixture": "代码为 C1，role_epoch 与 spec 不变；用户 SendInput STEER 已受理入队；随后新提交 C2，另有未提交的外部编辑变体",
    "steps": [
      "在 C1 下正常投递一次并重投；另在 C2 或外部编辑出现后投递 C1 入队消息，最后重新提交针对当前状态的新指令",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "入队同事务固定 code_target={git_commit,tree_sha256,candidate_id或null} 到 input_queue.payload_json 与 input.accepted，并传入 HCP/交接引用；正常消息最多一次 APPLIED；迟到消息 REJECTED，reason=CANDIDATE_MISMATCH，保留 expected/actual 身份，不原生 steer、不改变任务；外部编辑先按用户修改规则阻塞；新指令独立 input_id 才能投递；ASK/REPLY/CONTROL 的既有语义不变"
    ],
    "observables": [
      "input.accepted/outbox及receipt",
      "原生steer调用次数",
      "Git状态",
      "CANDIDATE_MISMATCH诊断"
    ]
  },
  {
    "id": "TEST-044-05",
    "requirement_id": "REQ-044",
    "acceptance_id": "AC-044-05",
    "check_id": "recovery",
    "fixture": "C1 上的 STEER 已入队并绑定 code_target；分别缺 candidate、尚无任何 commit、以及投递后未记回执崩溃",
    "steps": [
      "重启后重放队列，切换 role_epoch，或使 spec 修订变化，再尝试消费",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "有 commit 无 candidate 时按 commit/tree 校验而不造 candidate；无 commit 的 STEER 拒绝且给出无法绑定原因，可用 ASK 继续对话；原 code_target 永不重绑；候选/commit不符用 CANDIDATE_MISMATCH，角色不符沿用 STALE_ROLE_EPOCH，规格不符沿用 SPEC_MISMATCH；已投递但效果不明进入 UNKNOWN_EFFECT，不盲目重投；可核对的重复回执不重复 APPLIED"
    ],
    "observables": [
      "队列持久JSON",
      "HCP身份与回执",
      "恢复事件",
      "原生调用日志"
    ]
  },
  {
    "id": "TEST-048-01",
    "requirement_id": "REQ-048",
    "acceptance_id": "AC-048-01",
    "check_id": "plugin-live",
    "fixture": "锁定 alpertarhan/pi-smart-compact 的真实 Pi 非 Commander 会话，任务/AC/候选/证据引用齐全，Hermes 为唯一长期记忆所有者",
    "steps": [
      "使会话达到安装版本的自动压缩条件，观察真实 compact 开始/完成，再执行一个仍属于原 Task 的工程步骤",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "会话自动继续，任务/AC/spec/candidate 与证据引用可重新解析且hash相同；日志证明加载指定来源版本并发生真实压缩，不能仅手工写摘要；Smart Compact 的可选跨会话记忆后端未运行，Hermes 仍是唯一长期记忆后端"
    ],
    "observables": [
      "live-sessions.json的pi/plugins/版本",
      "原生compact事件",
      "压缩前后引用清单",
      "配置与后端调用记录"
    ]
  },
  {
    "id": "TEST-048-02",
    "requirement_id": "REQ-048",
    "acceptance_id": "AC-048-02",
    "check_id": "plugin-live",
    "fixture": "压缩摘要称 Task A 未完成、候选 C1 有效，Runtime 已接受 A 并采用 C2；另有旧 role_epoch 输入",
    "steps": [
      "压缩恢复后读取实时状态再继续，并重复恢复通知",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "恢复工作依据 Runtime/Git 的 C2 与当前 epoch；不重做 A、不把 C1 的验收计 current；摘要冲突可见并有来源，重复通知最多一次状态应用；旧 epoch 输入只记历史"
    ],
    "observables": [
      "实时快照水位",
      "摘要差异诊断",
      "派发/验收计数",
      "原生恢复记录"
    ]
  },
  {
    "id": "TEST-048-03",
    "requirement_id": "REQ-048",
    "acceptance_id": "AC-048-03",
    "check_id": "plugin-live",
    "fixture": "真实 Pi 会话有已 observed 的工具效果与尚未确认的外部效果，正在压缩",
    "steps": [
      "分别在 compact 期间中断、取消、重启后续会话，注入重复 compact 通知",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "已观察效果不重复执行；未知效果先 reconcile，不确认就 UNKNOWN_EFFECT；取消不产生完成claim或新写派发；恢复后保留任务/证据/预算身份，不能以压缩清零失败次数"
    ],
    "observables": [
      "effect ledger",
      "取消与恢复记录",
      "实际工具调用计数",
      "预算与身份前后快照"
    ]
  },
  {
    "id": "TEST-048-04",
    "requirement_id": "REQ-048",
    "acceptance_id": "AC-048-04",
    "check_id": "plugin-live",
    "fixture": "已安装版本的 Compact 单插件、Compact+Hermes、三插件组合；记录 hook 与模型调用计数",
    "steps": [
      "在每个组合触发一次实际压缩并在 hook 内重入触发，另使 Hermes flush 失败",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "每个压缩周期 flush 成功先于 compact 完成；同周期只进入一次 compact，重入被拒或合并且有日志；额外模型调用逐次可追溯，不产生无限递归；flush失败不丢经验，不把未完成压缩标成功；不支持所需hook的真实版本按CR-001带证据降级"
    ],
    "observables": [
      "组合版本清单",
      "hook时序",
      "模型调用计数",
      "flush失败原始记录"
    ]
  },
  {
    "id": "TEST-049-01",
    "requirement_id": "REQ-049",
    "acceptance_id": "AC-049-01",
    "check_id": "plugin-live",
    "fixture": "项目 A 的真实 Hermes 会话记录一次已验证失败经验，项目 B 含同名文件；经验带 source ArtifactRef、project、confidence、validity 与观察时间",
    "steps": [
      "在 A 的新会话召回并在 B 中作相同查询；另显式授权一条可跨项目的通用经验",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "A 可召回原经验且引用hash/观察时间可核对；B 默认不见 A 的项目专属内容，只有显式通用项可跨项目；召回不产生 acceptance或Issue解决事实；无来源/缺适用范围的经验标未验证且不进硬上下文"
    ],
    "observables": [
      "真实写入/召回记录",
      "经验字段与hash",
      "项目作用域",
      "验收/Issue快照"
    ]
  },
  {
    "id": "TEST-049-02",
    "requirement_id": "REQ-049",
    "acceptance_id": "AC-049-02",
    "check_id": "plugin-live",
    "fixture": "用户、反馈、项目、参考四类经验，各带 observed_at、validity、source；一条过时，一条与现规格冲突，一条收到用户纠正",
    "steps": [
      "下一会话召回，应用纠正/失效，重建索引后再次查询",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "保留分类与适用条件，显示观察时间或可核对年龄；失效项从活动召回撤下，纠正项取代旧活动内容且可追溯来源；不能修改HoH append-only证据；冲突以当前规格/Runtime为准，硬规格未召回仍由强制上下文提供"
    ],
    "observables": [
      "召回索引前后",
      "纠正与失效记录",
      "强制上下文引用",
      "原证据hash"
    ]
  },
  {
    "id": "TEST-049-03",
    "requirement_id": "REQ-049",
    "acceptance_id": "AC-049-03",
    "check_id": "plugin-live",
    "fixture": "两个真实 Pi 会话维护同一项目 Hermes 存储，另有不属于本会话的锁/目录",
    "steps": [
      "并发写不同经验，持锁时杀掉一个维护进程后恢复，另注入空间不足",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "已确认写入不丢且不重复；冲突有可观察等待/拒绝，恢复先核对锁持有者完整进程身份；不删除他人活锁/目录；失败可重试且保留原经验；恢复后的存储通过安装版本支持的完整性核对"
    ],
    "observables": [
      "两个session与维护锁身份",
      "提交记录",
      "存储校验",
      "空间错误与恢复日志"
    ]
  },
  {
    "id": "TEST-049-04",
    "requirement_id": "REQ-049",
    "acceptance_id": "AC-049-04",
    "check_id": "plugin-live",
    "fixture": "Hermes 召回文本声称某 required AC 已PASS及某 high Issue 已修复，但当前候选没有对应证据",
    "steps": [
      "将召回注入真实 Pi 上下文并提交其总结；再提供真实检查失败作为反例",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "Runtime的accepted、Issue状态及发布门禁不受记忆claim改变；缺证据仍未满足，失败仍阻断；能定位召回的来源与冲突原因；无召回时规格/AC/候选强制项仍完整"
    ],
    "observables": [
      "prompt引用清单",
      "原生最终消息",
      "前后投影与门禁拒绝",
      "真实检查记录"
    ]
  },
  {
    "id": "TEST-050-01",
    "requirement_id": "REQ-050",
    "acceptance_id": "AC-050-01",
    "check_id": "plugin-live",
    "fixture": "真实 Pi 非Commander会话的 @juicesharp/rpiv-todo 含3个局部todo，绑定run/task/attempt/native session；HoH任务尚未验收",
    "steps": [
      "完成一项后reload和compact，恢复同会话，再开独立新会话",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "同会话3项身份/顺序/状态保持，新会话不自动继承；展示映射可定位全局Task和原会话；local completed不写Task ACCEPTED或requirement accepted；安装清单无整个rpiv pipeline"
    ],
    "observables": [
      "实际todo前后导出",
      "session身份",
      "映射与全局状态",
      "安装清单"
    ]
  },
  {
    "id": "TEST-050-02",
    "requirement_id": "REQ-050",
    "acceptance_id": "AC-050-02",
    "check_id": "plugin-live",
    "fixture": "Pi局部todo全部完成，HoH对应AC有FAIL、Task已有正式领取记录，恢复清单有活动工作与next_action",
    "steps": [
      "读取TUI只读映射与状态栏，再让Pi把所有todo改completed",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "视图明确local todo和全局Task两套语义；展示领取身份、候选、证据与下一动作；accepted仍按Runtime验收不增长，FAIL仍显示；无Task领取/映射时显示未关联，不伪造全局进度"
    ],
    "observables": [
      "TUI/查询快照",
      "todo更新日志",
      "任务领取与验收事实"
    ]
  },
  {
    "id": "TEST-050-03",
    "requirement_id": "REQ-050",
    "acceptance_id": "AC-050-03",
    "check_id": "plugin-live",
    "fixture": "两个Pi会话相同todo文本、一个旧epoch会话和一个当前会话；当前会话在写todo时中断",
    "steps": [
      "重放旧会话消息并恢复当前会话，取消全局Task后再接收迟到todo",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "不同session的局部id不碰撞；旧epoch与取消后内容只能作为历史展示、不更新当前映射；恢复无重复todo、不把未确认写入当完成；局部失败不删除全局Task/证据"
    ],
    "observables": [
      "session+todo复合身份",
      "旧消息与映射",
      "重放计数",
      "任务/证据hash"
    ]
  },
  {
    "id": "TEST-051-01",
    "requirement_id": "REQ-051",
    "acceptance_id": "AC-051-01",
    "check_id": "integration",
    "fixture": "隔离的空插件目录、本地fixture registry含有效/损坏包；固定Pi与三插件版本/完整性；RPC无custom UI；两个compact hook",
    "steps": [
      "通过产品准备默认组合，依次注入无UI、hook冲突和安装失败，再运行doctor",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "成功时校验来源/版本/integrity/依赖，原子发布整套锁；无需手工安装Skill；HoH设置入口使用已核实配置adapter，不要求Pi custom TUI；失败时保留上一可用组合，每个插件有明确diagnostic/上游引用，继续不依赖插件的任务；双hook不悄悄运行两套压缩"
    ],
    "observables": [
      "安装请求与完整性核对",
      "组合锁与doctor输出",
      "RPC能力表",
      "失败诊断"
    ]
  },
  {
    "id": "TEST-051-02",
    "requirement_id": "REQ-051",
    "acceptance_id": "AC-051-02",
    "check_id": "integration",
    "fixture": "本地registry有篡改hash、缺依赖、不支持Pi版本、错误作者来源四组包，活动Run已固定旧组合",
    "steps": [
      "分别准备新组合，并尝试活动Run中升级；维护窗口在安全点后再重试",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "四个坏组合全部拒绝，不写有效锁、不执行未经验证包；活动Run继续旧锁；只有维护窗口且无使用中会话时发布新组合，新Run采用新锁；doctor列出期望/实际与修复动作，不能报告兼容PASS"
    ],
    "observables": [
      "包hash与来源",
      "Run快照",
      "安装目录原子切换",
      "doctor机器输出"
    ]
  },
  {
    "id": "TEST-051-03",
    "requirement_id": "REQ-051",
    "acceptance_id": "AC-051-03",
    "check_id": "recovery",
    "fixture": "已安装旧组合，升级operation_id已登记；另有其它会话同前缀目录及活进程",
    "steps": [
      "在下载、校验、暂存完成、原子切换前后分别中断/取消/kill并恢复",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "每次恢复只见完整旧组合或完整新组合；不出现混合锁；取消不发布未完成组合；只清理本operation登记的精确路径和带启动身份的进程，旁观资源不变；失败记录保留且重试幂等"
    ],
    "observables": [
      "effect ledger",
      "前后包/锁hash",
      "取消恢复日志",
      "旁观资源"
    ]
  },
  {
    "id": "TEST-051-04",
    "requirement_id": "REQ-051",
    "acceptance_id": "AC-051-04",
    "check_id": "integration",
    "fixture": "本地故障注入上游404/不兼容/安装失败及缺认证/429/provider内容过滤；required AC和degradable AC各一组",
    "steps": [
      "生成诊断与模拟发布输入，逐项删去diagnostic/upstream_ref或把REQ-051标DEGRADED",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "仅符合CR-001的插件不可用可形成带执行绑定、diagnostic ArtifactRef与真实来源引用的降级；缺字段拒绝；REQ-051及任何required检查永不以DEGRADED满足；缺认证/额度/临时故障/内容过滤保持各自阻塞原因，过滤记录failure_class=provider_content_filter的诊断并交现有决策路径，不假称能力缺失、不自动耗尽重试"
    ],
    "observables": [
      "诊断工件与hash",
      "降级结构校验",
      "错误分类及发布拒绝理由"
    ]
  },
  {
    "id": "TEST-051-05",
    "requirement_id": "REQ-051",
    "acceptance_id": "AC-051-05",
    "check_id": "integration",
    "fixture": "隔离配置中启用三插件；Commander选择Pi，QA选择Pi；RPC schema与实际配置adapter匹配/不匹配各一例",
    "steps": [
      "构造启动argv与会话目录，保存插件设置，再重开并运行doctor",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "Commander argv保留CR-024禁扩展且无显式-e；QA加载锁定三插件；设置保存有校验、原子写与重开一致，不写原生用户目录；adapter不匹配拒绝并保旧配置；无UI能力明确降级说明及HoH设置入口；doctor能区分角色差异"
    ],
    "observables": [
      "真实构造argv",
      "会话配置目录",
      "设置前后hash",
      "doctor输出"
    ]
  },
  {
    "id": "TEST-055-01",
    "requirement_id": "REQ-055",
    "acceptance_id": "AC-055-01",
    "check_id": "integration",
    "fixture": "B上功能a已验证；C修复b却使a回归；同一候选同一TEST有FAIL后PASS，并有一组超时无结论后PASS的对照",
    "steps": [
      "按影响闭包选择测试，运行失败与重试，再查询候选投影与issue",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "回归打开可定位a/原基线/新候选的regression Issue；全部attempt与stdout/stderr保留；FAIL→PASS为FLAKY且不满足strict；无结论超时→PASS按ADR-027/029求值，不错误混同有结论失败；运行顺序不改变历史聚合"
    ],
    "observables": [
      "逐test原始记录",
      "Issue及fingerprint",
      "历史聚合与候选门禁"
    ]
  },
  {
    "id": "TEST-055-02",
    "requirement_id": "REQ-055",
    "acceptance_id": "AC-055-02",
    "check_id": "integration",
    "fixture": "模块依赖图含传递依赖，另有未知影响文件；测试矩阵锁定Ubuntu24.04/26.04与工具版本，含Python3.12/3.14路径处理差异",
    "steps": [
      "对已知修改求闭包，对未知影响及release请求分别选检查，执行一个只在矩阵一格失败的真实用例",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "已知闭包包含所有传递相关AC/TEST；未知影响与发布选full required suite；矩阵每格独立记录版本/结果，任一required格失败不被其它格PASS覆盖；Python工具兼容边界及过长文件名有断言，缺环境不计PASS"
    ],
    "observables": [
      "依赖闭包",
      "检查选择列表",
      "矩阵环境摘要",
      "路径边界stdout/stderr"
    ]
  },
  {
    "id": "TEST-055-03",
    "requirement_id": "REQ-055",
    "acceptance_id": "AC-055-03",
    "check_id": "recovery",
    "fixture": "同一冻结候选上并行的QA与Reviewer，各有独立证据根、worktree、进程身份；第三方同前缀目录内有哨兵",
    "steps": [
      "运行可能生成缓存的检查，取消一方并执行清理；另注入验证者写入候选与嵌套检查继承外层证据环境",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "缓存/字节码/输出均在登记的候选外目录；前后Git tree及未跟踪清单不变；污染时候选检查无效，不签PASS；只清理本方登记资源，另一方与哨兵不变；内层剥离外层HOH检查变量，外层无串入test行；检查恢复默认信号处理并正常接收取消"
    ],
    "observables": [
      "两方执行根与hash",
      "候选前后清单",
      "进程boot_id/pid/start_time",
      "嵌套环境与test-results"
    ]
  },
  {
    "id": "TEST-055-04",
    "requirement_id": "REQ-055",
    "acceptance_id": "AC-055-04",
    "check_id": "integration",
    "fixture": "同一缺陷family连续两轮出现但其它AC仍增长；互补Reviewer使用不同编号段，对同一缺陷给出相反结论；另一序列达到4轮与5轮",
    "steps": [
      "按本稿审查收敛规则推进到上限，重启后继续并尝试仅凭多数意见解除",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "连续两轮即要求带证据的设计修正，不能被其它进展重置；发现按session+finding_id唯一，语义去重保留各来源；冲突按复现事实判定；第4轮无critical/high时medium全部修复并红绿、由主会话核对后结束审查，low入里程碑清理包；第4轮仍有critical/high则设计修正后仅一次新会话第5轮核对，仍有则BLOCKED；重启不清轮数；轮数上限从不把FAIL/未满足AC变PASS"
    ],
    "observables": [
      "review series与计数",
      "design correction ArtifactRef",
      "finding来源及处置",
      "阻塞事件与原始报告"
    ]
  },
  {
    "id": "TEST-055-05",
    "requirement_id": "REQ-055",
    "acceptance_id": "AC-055-05",
    "check_id": "integration",
    "fixture": "候选C的轮次级与汇合级FLAKY各一组，已保留FAIL/PASS历史；原栅栏J已有joined事实",
    "steps": [
      "请求处置，先尝试同内容refreeze/重跑/隔离required测试，再用新提交C2修复并独立验证",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "前3种不能使C满足；处置保留C历史并要求新候选，C2重新绑定全套相关证据；解除汇合级flaky产生新栅栏编号且不复用J（ADR-029 J9）；没有删除/改写失败记录或required测试，未通过保持阻塞"
    ],
    "observables": [
      "旧新candidate与栅栏",
      "拒绝同内容记录",
      "FAIL历史hash",
      "新候选QA/Review记录"
    ]
  },
  {
    "id": "TEST-056-01",
    "requirement_id": "REQ-056",
    "acceptance_id": "AC-056-01",
    "check_id": "integration",
    "fixture": "产品发布判定器有完整正对照输入，绑定candidate/tree/spec/environment/test_plan；另分别构造空AC/空检查、缺test、skip/mock、hash错、旧规格/候选、独立会话复用、缺原生记录、缺live agent、soak不足、遗留high",
    "steps": [
      "经Runtime请求发布，对每个负例只破坏一个条件并读取最终结果",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "正对照仅在全部规则满足才有可发布结论；全部负例逐项拒绝并给出缺项；只通过候选或里程碑/v0.1不能输出v1 COMPLETED；模型claim/截图/进程exit0不替代证据；fixture正对照只证明判定器，绝非HoH v1实测完成"
    ],
    "observables": [
      "逐条件负例表",
      "Runtime拒绝与manifest",
      "执行身份/hash",
      "最终RunResult"
    ]
  },
  {
    "id": "TEST-056-02",
    "requirement_id": "REQ-056",
    "acceptance_id": "AC-056-02",
    "check_id": "integration",
    "fixture": "同Run的两个开发Task已在不同增量候选接受，最终C为ACCEPTED但仅覆盖最后增量；具备持久release_validation_id",
    "steps": [
      "Runtime触发全范围验证，重复release_request并在两方结果收取之间重启；另在期间换spec/candidate或取消",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "全范围包含profile每项required AC/check，不开verification.round_opened，不迁移源Task与候选状态；可对ACCEPTED的C运行；同请求只有一个活动验证义务，QA/Review为不同独立会话并保留真实绑定；重启收取去重；换身份旧结果仅历史、取消停止发布；全量结果齐全且合法之前Run不COMPLETED"
    ],
    "observables": [
      "release验证身份与范围",
      "round事件计数",
      "源Task/candidate快照",
      "原生会话/报告与重启记录"
    ]
  },
  {
    "id": "TEST-056-03",
    "requirement_id": "REQ-056",
    "acceptance_id": "AC-056-03",
    "check_id": "integration",
    "fixture": "最终候选的PASS记录分别缺v1所需codex/claude/pi或plugin列表；合法插件DEGRADED对照具有诊断与上游引用；REQ-051为required",
    "steps": [
      "用v0.1/v1各自profile调用发布投影，并篡改DEGRADED检查下的AC为PASS或将required改成DEGRADED",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "RowFact.live_agents来自证据live-sessions，不来自模型claim；有profile时按规则16仅过滤outcome=PASS产生的不合要求PASS行；合规插件降级按policy满足并输出COMPLETED_DEGRADED及明细；required降级、DEGRADED检查引用PASS AC、缺诊断/上游均拒绝；v0.1最多INTERIM_RELEASED"
    ],
    "observables": [
      "带profile投影",
      "live-sessions插件名",
      "降级明细",
      "两profile判定矩阵"
    ]
  },
  {
    "id": "TEST-056-04",
    "requirement_id": "REQ-056",
    "acceptance_id": "AC-056-04",
    "check_id": "integration",
    "fixture": "Run遗留OPEN/REOPENED critical/high，包括已取消源Task；物化state故意滞后；同fingerprint多Issue与超过50项确认集；当前候选比原Issue新",
    "steps": [
      "全范围QA/Review分批收到确认条目，分别确认RESOLVED/OPEN/缺观察，并在同候选追加present或INVALID事实",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "待确认集合与open_issues在同一读快照按ADR-029状态函数求值；仅两方实际送达交集、至少一方有RESOLVED观察且双方无present才写absent并同事务settle_all；验收失败Issue由新行求解；未送达/未确认/同候选present/否决均仍阻断；含取消Task且不因批次上限漏项；出口仅重验/返工/取消且阻断保持，豁免不由Agent决定"
    ],
    "observables": [
      "两方confirm_ids/观察",
      "absent/present事实",
      "同事务Issue事件",
      "快照水位与open_issues"
    ]
  },
  {
    "id": "TEST-056-05",
    "requirement_id": "REQ-056",
    "acceptance_id": "AC-056-05",
    "check_id": "recovery",
    "fixture": "完整发布输入的最终产物与build声明hash相同，release_validation已完成；还有最后提交前后的kill点与重复release_request",
    "steps": [
      "提交发布记录时分别kill并恢复；再修改产物hash、追加high或令候选过期后请求发布",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "manifest/发布记录/最终run.result一致且幂等，不出现只写COMPLETED无可核对manifest；提交前再次核对当前spec/candidate、Issue和产物；新high/过期/hash错拒绝；重启不重造证据、不以旧release快照接受新状态；最终状态仅由Runtime判定"
    ],
    "observables": [
      "发布manifest/hash",
      "releases与run.result",
      "恢复点",
      "提交前水位和拒绝证据"
    ]
  },
  {
    "id": "TEST-057-01",
    "requirement_id": "REQ-057",
    "acceptance_id": "AC-057-01",
    "check_id": "deployment",
    "fixture": "Ubuntu24.04与26.04 x86_64的全新隔离VM，候选构建包；三个受控目标项目分别为CLI/Web/service，均有可运行的vA/vB与数据；浏览器驱动已安装",
    "steps": [
      "通过HoH验证流程实际安装vA、运行CLI PTY/浏览器关键操作/service请求，再升级vB并回滚vA，保存每步原始输出",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "两个OS分别产生安装/启动/功能/升级/回滚记录及版本hash；PTY输入响应与退出、浏览器DOM/网络行为、service健康与数据均有断言；回滚后旧版本及原数据可用；截图只作辅证；不是在开发机换HOME冒充fresh VM，缺VM/驱动不能PASS"
    ],
    "observables": [
      "VM镜像/OS/架构",
      "包与binary hash",
      "PTY字节/浏览器trace/HTTP",
      "安装升级回滚日志"
    ]
  },
  {
    "id": "TEST-057-02",
    "requirement_id": "REQ-057",
    "acceptance_id": "AC-057-02",
    "check_id": "tui-pty",
    "fixture": "已有M4完整TUI与运行中的后台host，受控工程目标的检查有PASS/FAIL/NOT_RUN与可点击证据",
    "steps": [
      "用真实PTY操作查看检查、打开证据、脱离并重附，分别暂停与取消",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "用户可从Task/Issue定位原始证据与候选，三种结果文字可区分；脱离不停止host或已接受任务，暂停/取消只走对应命令；截图成功不掩盖检查FAIL；关闭/重附后结果一致且无重复派发"
    ],
    "observables": [
      "PTY原始流及动作记录",
      "host/Attempt身份",
      "命令与事件",
      "证据hash"
    ]
  },
  {
    "id": "TEST-057-03",
    "requirement_id": "REQ-057",
    "acceptance_id": "AC-057-03",
    "check_id": "deployment",
    "fixture": "与057-01相同两种fresh VM；安装包损坏、依赖缺失、端口占用、迁移失败、旧binary面对高版本DB各一例",
    "steps": [
      "通过实际部署/升级路径逐项触发故障，再恢复前置条件并重试",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "每例非成功退出且有原因/恢复动作，保留上一可用安装与备份；不宣称升级/回滚成功，不吞DB版本错误；修复后同路径可重试且不重复系统资源；旧binary不读高版本DB，回滚只对经校验旧备份进行"
    ],
    "observables": [
      "退出码与stderr",
      "systemd状态",
      "安装和DB hash",
      "重试资源清单"
    ]
  },
  {
    "id": "TEST-057-04",
    "requirement_id": "REQ-057",
    "acceptance_id": "AC-057-04",
    "check_id": "integration",
    "fixture": "目标项目计划声明PTY/浏览器/部署检查与必需观测；有只截图、零断言、缺browser动作、退出0但FAIL、未运行五组反例",
    "steps": [
      "Runtime收取检查记录并构造验证/发布汇总，再读TUI证据摘要",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "五组均不能成为有效PASS；保留stdout/stderr/命令/环境/版本与逐测试状态；完整流程正对照可被引用；无法运行明确NOT_RUN/BLOCKED，不伪造browser/live；HoH自身完整Web e2e仍归M6及最终v1验收，不被此目标项目fixture替代"
    ],
    "observables": [
      "逐test结果",
      "原始工件清单",
      "验收与发布拒绝",
      "TUI摘要"
    ]
  },
  {
    "id": "TEST-063-01",
    "requirement_id": "REQ-063",
    "acceptance_id": "AC-063-01",
    "check_id": "recovery",
    "fixture": "真实SQLite WAL活跃写入，DB引用规格/证据/公开流对象；已确认记录集K，每笔写入带唯一序号；含不会再修改的对象与未登记临时文件",
    "steps": [
      "通过产品一致性backup能力并发写入，完成后在独立目录restore并执行integrity_check与逐引用hash核对",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "恢复DB是一个可说明水位H的完整事务快照，H内已确认记录与outbox/effect/预算不丢，无半事务；所有被引用对象齐全且hash一致，未登记临时文件不变成有效引用；源库继续工作；不是只copy主DB文件"
    ],
    "observables": [
      "backup manifest/H/schema",
      "SQLite integrity_check",
      "对象列表/hash",
      "事务序号与源host日志"
    ]
  },
  {
    "id": "TEST-063-02",
    "requirement_id": "REQ-063",
    "acceptance_id": "AC-063-02",
    "check_id": "recovery",
    "fixture": "上一受支持schema的真实旧binary造库与备份；当前binary含正式编号迁移；另有高于支持范围的DB",
    "steps": [
      "依次升级、重开校验、用旧binary打开升级库，再执行回滚到旧备份并重开；迁移中逐点kill",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "迁移前后integrity_check通过，user_version只在整笔成功后改变，重复启动不重放迁移；旧binary打开高版本DB在任何业务读写/派发前拒绝且库字节/版本不变；回滚恢复旧备份与匹配binary，不做未声明的down migration；kill后是完整旧或完整新schema"
    ],
    "observables": [
      "旧新binary/迁移hash",
      "user_version/integrity",
      "恢复前后DB",
      "启动拒绝与无派发证明"
    ]
  },
  {
    "id": "TEST-063-03",
    "requirement_id": "REQ-063",
    "acceptance_id": "AC-063-03",
    "check_id": "recovery",
    "fixture": "backup/restore操作已登记，目标有既存库；分别令DB/对象hash损坏、对象缺失、备份目标满盘、只读及SQLITE_BUSY",
    "steps": [
      "尝试生成备份及恢复，在临时落盘、fsync、rename和完成登记处中断后重启",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "无完整manifest和校验的备份不能标成功；恢复校验失败不替换原库，原数据hash不变；取消/重启只见旧库或全部验证的新库，孤立临时资源按operation归属清理；BUSY有界重试/可诊断退出，不阻塞取消与读取"
    ],
    "observables": [
      "逐故障原始记录",
      "manifest状态",
      "原库与目标hash",
      "operation/effect及资源清单"
    ]
  },
  {
    "id": "TEST-063-04",
    "requirement_id": "REQ-063",
    "acceptance_id": "AC-063-04",
    "check_id": "integration",
    "fixture": "活动host/Attempt存在，备份绑定project_id/run/spec/schema/资源版本；另有错误项目备份、旧schema但不支持的迁移链",
    "steps": [
      "先请求恢复活动库，再在停派发/所有Attempt安全收尾并取得独占项目锁后恢复正确/错误备份",
      "保存下列可观察项，逐项核对判定；每个故障子例使用独立项目，防止前例的否决污染正对照"
    ],
    "pass_criteria": [
      "活动状态拒绝破坏性替换，返回待静止条件；独占恢复只接受匹配项目与受支持迁移链；恢复后先reconcile、递增host_epoch/fence旧租约，不盲目重投started效果；不同项目/不支持版本拒绝并保原库；恢复进度/取消可查"
    ],
    "observables": [
      "项目锁与活动Attempt",
      "manifest绑定",
      "host_epoch/lease/effect",
      "恢复进度与拒绝原因"
    ]
  }
]
```

## 实现任务拆分与逐 AC 依赖闭包

保留M5-T01/T02/T03/T04，新增T05..T10；共八个implement，全部≤2 REQ且≤8 AC。spec/verify负责全阶段，不受实现切片数量限制。M5-T04在数组和DAG中均最后；M6-T01仍依赖M5-T04，不改后续任务。

| 任务 | 需求 | AC数 | 直接依赖 |
|---|---|---:|---|

| M5-T01 | REQ-044, REQ-048, REQ-049, REQ-050, REQ-051, REQ-055, REQ-056, REQ-057, REQ-063 | spec | M4-T04 |

| M5-T05 | REQ-063 | 4 | M5-T01 |

| M5-T06 | REQ-044 | 5 | M5-T05 |

| M5-T02 | REQ-055 | 5 | M5-T06 |

| M5-T03 | REQ-051 | 5 | M5-T01, M0-T04 |

| M5-T09 | REQ-048, REQ-049 | 8 | M5-T03, M5-T06, M0-T04 |

| M5-T10 | REQ-050 | 3 | M5-T09 |

| M5-T07 | REQ-056 | 5 | M5-T02, M5-T10 |

| M5-T08 | REQ-057 | 4 | M5-T07, M5-T05 |

| M5-T04 | REQ-044, REQ-048, REQ-049, REQ-050, REQ-051, REQ-055, REQ-056, REQ-057, REQ-063 | verify | M5-T05, M5-T06, M5-T02, M5-T03, M5-T09, M5-T10, M5-T07, M5-T08 |


本数组完整替换原M5四项，按下列顺序插入；只有M5-T03含checks=[plugin-live]。未列checks不等于不跑检查，只是不重复认领检查维护权。

```json
[
  {
    "id": "M5-T01",
    "milestone": "M5",
    "kind": "spec",
    "owner_role": "cto",
    "writer": true,
    "title": "细化M5需求为detailed：每项2–5条可判定AC与场景，写CR并relock基线",
    "depends_on": [
      "M4-T04"
    ],
    "requirements": [
      "REQ-044",
      "REQ-048",
      "REQ-049",
      "REQ-050",
      "REQ-051",
      "REQ-055",
      "REQ-056",
      "REQ-057",
      "REQ-063"
    ],
    "done_when": "本里程碑全部需求spec_maturity=detailed；validate_bundle通过；CR已APPROVED并relock；按每个实现任务≤2项需求、≤8条AC拆分本里程碑实现任务（更新 development-tasks.json 的 requirements/acceptance/checks/depends_on，validate_bundle 的归属规则通过）；每条 AC 只分给依赖闭包已提供其所需能力（host、API、CLI 最终结果、QA/Review 循环、systemd、doctor、TUI 等）的任务（CR3R-02）。拆分时把 CR-018 与 CR-020 交给 M5-T02 的各项分给一个实现任务，写进它的 done_when（承接须有契约载体，M2T16D-1050）。 CR-030应用前重核A1–A6和CTO决定；逐条吸收docs/24及CR-026 M3A-01/02；全套cargo test --workspace --locked验证契约/代码对应，不仅bundle。"
  },
  {
    "id": "M5-T05",
    "milestone": "M5",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Consistent backup, restore and schema compatibility",
    "depends_on": [
      "M5-T01"
    ],
    "requirements": [
      "REQ-063"
    ],
    "acceptance": [
      "AC-063-01",
      "AC-063-02",
      "AC-063-03",
      "AC-063-04"
    ],
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。交付DB一致性backup、对象引用闭包/GC pin、维护恢复、迁移兼容与原子发布；先审恢复身份/事务ADR。依赖CR-028对象生命周期，应用时重核最新schema。"
  },
  {
    "id": "M5-T06",
    "milestone": "M5",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Recoverable Git versions and version-bound steering",
    "depends_on": [
      "M5-T05"
    ],
    "requirements": [
      "REQ-044"
    ],
    "acceptance": [
      "AC-044-01",
      "AC-044-02",
      "AC-044-03",
      "AC-044-04",
      "AC-044-05"
    ],
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。交付失败分支/恢复清单、用户改动保护及STEER受理/投递code_target绑定，接线input_queue、HCP、原生投递与恢复；承接CR-026 M3A-02；回归REQ-017/022/028/037/041。"
  },
  {
    "id": "M5-T02",
    "milestone": "M5",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Regression, flaky disposition and bounded review workflow",
    "depends_on": [
      "M5-T06"
    ],
    "requirements": [
      "REQ-055"
    ],
    "acceptance": [
      "AC-055-01",
      "AC-055-02",
      "AC-055-03",
      "AC-055-04",
      "AC-055-05"
    ],
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。承接CR-020：flaky处置要求新候选，解除汇合级flaky产生新栅栏编号（ADR-029 J9），保留全部历史；承接CR-026 M3A-01：两轮同类复发设计修正、四轮上限及一次最终复核、low清理包；互补审查编号/冲突按证据；候选只读与精确资源清理；矩阵与影响闭包。"
  },
  {
    "id": "M5-T03",
    "milestone": "M5",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Pinned plugin preparation, diagnostics and live check driver",
    "depends_on": [
      "M5-T01",
      "M0-T04"
    ],
    "requirements": [
      "REQ-051"
    ],
    "acceptance": [
      "AC-051-01",
      "AC-051-02",
      "AC-051-03",
      "AC-051-04",
      "AC-051-05"
    ],
    "checks": [
      "plugin-live"
    ],
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。交付plugin-live真实驱动，逐TEST输出与原生session/plugins绑定；未实现集成保持NOT_RUN/非零，不能恒PASS；此时驱动支持与REQ-051的required产品检查验收分开，不能声称三个插件已通过。真实计划argv只随可执行脚本同提交替换占位，不删mode/artifacts/timeout。运行中组合锁、原子安装、RPC设置与doctor、故障/恢复路径齐全。"
  },
  {
    "id": "M5-T09",
    "milestone": "M5",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Smart Compact and Hermes continuity integration",
    "depends_on": [
      "M5-T03",
      "M5-T06",
      "M0-T04"
    ],
    "requirements": [
      "REQ-048",
      "REQ-049"
    ],
    "acceptance": [
      "AC-048-01",
      "AC-048-02",
      "AC-048-03",
      "AC-048-04",
      "AC-049-01",
      "AC-049-02",
      "AC-049-03",
      "AC-049-04"
    ],
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。在真实Pi会话补齐plugin-live的Compact/Hermes及三组合hook场景；本任务可加载已锁定todo包测试组合hook，但不声称REQ-050完成；每项PASS或符合CR-001的DEGRADED都须实际证据，缺认证不当插件降级。"
  },
  {
    "id": "M5-T10",
    "milestone": "M5",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Session-local todo mapping and continuity",
    "depends_on": [
      "M5-T09"
    ],
    "requirements": [
      "REQ-050"
    ],
    "acceptance": [
      "AC-050-01",
      "AC-050-02",
      "AC-050-03"
    ],
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。交付rpiv-todo隔离/恢复/只读映射；全局任务领取、候选、证据、下一动作来自Runtime；真实三插件组合全部场景，不能安装整个pipeline。"
  },
  {
    "id": "M5-T07",
    "milestone": "M5",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Full-scope release validation and evidence gate",
    "depends_on": [
      "M5-T02",
      "M5-T10"
    ],
    "requirements": [
      "REQ-056"
    ],
    "acceptance": [
      "AC-056-01",
      "AC-056-02",
      "AC-056-03",
      "AC-056-04",
      "AC-056-05"
    ],
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。承接CR-018：Runtime触发最终候选全范围验证，不开增量轮次，可对ACCEPTED候选运行，不迁移候选/源Task状态，使多Task Run完成具备真实全量证据。承接CR-020：发布投影传profile，规则16仅核outcome PASS产生的PASS行，RowFact.live_agents来自live-sessions；全Run OPEN/REOPENED critical/high含取消Task，按ADR-029决定10送达/absent条目规则、同事务issues::settle_all；验收失败Issue按新行求解；待确认集与open_issues在导出读快照直接求状态函数；出口仅重验/返工/取消且阻断保持，豁免需所有者。报告收取/发布验证身份接线先出ADR并审查，不复用round_of_task伪造全量验证。"
  },
  {
    "id": "M5-T08",
    "milestone": "M5",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Runtime UX and fresh-VM deployment verification",
    "depends_on": [
      "M5-T07",
      "M5-T05"
    ],
    "requirements": [
      "REQ-057"
    ],
    "acceptance": [
      "AC-057-01",
      "AC-057-02",
      "AC-057-03",
      "AC-057-04"
    ],
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。按CTO Q1决定交付目标CLI/Web/service实际验证、双Ubuntu fresh VM安装升级回滚、M4 TUI证据导航与失败诊断；增强既有deployment/tui-pty检查而不转移owner。此任务包含verify前low清理包：逐项修复或按AC列出带证据延期，独立轻审；没有完成M6的HoH Web GUI不签其验收。"
  },
  {
    "id": "M5-T04",
    "milestone": "M5",
    "kind": "verify",
    "owner_role": "qa",
    "writer": false,
    "title": "M5独立QA（Pi）与独立Review（Codex），同一冻结候选",
    "depends_on": [
      "M5-T05",
      "M5-T06",
      "M5-T02",
      "M5-T03",
      "M5-T09",
      "M5-T10",
      "M5-T07",
      "M5-T08"
    ],
    "requirements": [
      "REQ-044",
      "REQ-048",
      "REQ-049",
      "REQ-050",
      "REQ-051",
      "REQ-055",
      "REQ-056",
      "REQ-057",
      "REQ-063"
    ],
    "done_when": "同一冻结候选上由独立Pi QA与独立Codex Reviewer（会话彼此及开发不同）核对39条AC与真实原始证据；required逐项PASS，只有REQ-048/049/050可按CR-001带诊断与上游引用DEGRADED；缺工具/认证记BLOCKED或NOT_RUN并不通过。所有critical/high已解决并有验证观察；low清理包完成或有不破坏AC的带证据处置。不签v1 COMPLETED；M6-T01仍依赖本任务。"
  }
]
```


### 每条 AC 的 owner、检查与能力提供者

表中当前任务交付的能力允许由本任务自身提供，其余必须是严格祖先。M4-T04展开为A1/A2的任务集合；机械探针同时验证输入基线闭包与CR-028任务图替换后的闭包。对未来行为的可用性仍须在应用时验收，图可达不等于行为已实现。

| AC | owner | check | 能力提供任务（祖先或本任务） | 所需能力 |
|---|---|---|---|---|

| AC-044-01 | M5-T06 | recovery | M5-T05；M3-T09；M3-T07；M2-T05/M2-T17；M4-T04 | 备份/写者与恢复、STEER/回执、发现基线、epoch、M4当前身份 |

| AC-044-02 | M5-T06 | recovery | M5-T05；M3-T09；M3-T07；M2-T05/M2-T17；M4-T04 | 备份/写者与恢复、STEER/回执、发现基线、epoch、M4当前身份 |

| AC-044-03 | M5-T06 | recovery | M5-T05；M3-T09；M3-T07；M2-T05/M2-T17；M4-T04 | 备份/写者与恢复、STEER/回执、发现基线、epoch、M4当前身份 |

| AC-044-04 | M5-T06 | integration | M5-T05；M3-T09；M3-T07；M2-T05/M2-T17；M4-T04 | 备份/写者与恢复、STEER/回执、发现基线、epoch、M4当前身份 |

| AC-044-05 | M5-T06 | recovery | M5-T05；M3-T09；M3-T07；M2-T05/M2-T17；M4-T04 | 备份/写者与恢复、STEER/回执、发现基线、epoch、M4当前身份 |

| AC-048-01 | M5-T09 | plugin-live | M5-T03/M5-T06；M3-T03/M3-T08；M2-T09；M4-T04 | 真实plugin驱动/组合锁、恢复绑定、Prompt/Skill、强制上下文、公开活动 |

| AC-048-02 | M5-T09 | plugin-live | M5-T03/M5-T06；M3-T03/M3-T08；M2-T09；M4-T04 | 真实plugin驱动/组合锁、恢复绑定、Prompt/Skill、强制上下文、公开活动 |

| AC-048-03 | M5-T09 | plugin-live | M5-T03/M5-T06；M3-T03/M3-T08；M2-T09；M4-T04 | 真实plugin驱动/组合锁、恢复绑定、Prompt/Skill、强制上下文、公开活动 |

| AC-048-04 | M5-T09 | plugin-live | M5-T03/M5-T06；M3-T03/M3-T08；M2-T09；M4-T04 | 真实plugin驱动/组合锁、恢复绑定、Prompt/Skill、强制上下文、公开活动 |

| AC-049-01 | M5-T09 | plugin-live | M5-T03/M5-T06；M2-T09/M2-T18；M4-T04 | 插件安装/原生Pi、项目身份、记忆非验收、M4对象 |

| AC-049-02 | M5-T09 | plugin-live | M5-T03/M5-T06；M2-T09/M2-T18；M4-T04 | 插件安装/原生Pi、项目身份、记忆非验收、M4对象 |

| AC-049-03 | M5-T09 | plugin-live | M5-T03/M5-T06；M2-T09/M2-T18；M4-T04 | 插件安装/原生Pi、项目身份、记忆非验收、M4对象 |

| AC-049-04 | M5-T09 | plugin-live | M5-T03/M5-T06；M2-T09/M2-T18；M4-T04 | 插件安装/原生Pi、项目身份、记忆非验收、M4对象 |

| AC-050-01 | M5-T10 | plugin-live | M5-T09；M5-T06；M4-T04；M2-T18 | 真实Compact/Hermes组合、恢复点、M4视图/进度、验收事实 |

| AC-050-02 | M5-T10 | plugin-live | M5-T09；M5-T06；M4-T04；M2-T18 | 真实Compact/Hermes组合、恢复点、M4视图/进度、验收事实 |

| AC-050-03 | M5-T10 | plugin-live | M5-T09；M5-T06；M4-T04；M2-T18 | 真实Compact/Hermes组合、恢复点、M4视图/进度、验收事实 |

| AC-051-01 | M5-T03 | integration | M0-T04；M2-T07/M2-T14/M2-T20；M3-T08；M4-T04 | Pi协议、配置/doctor/原生只读档、内置Skill、M4设置/对象；本任务交付安装及live驱动 |

| AC-051-02 | M5-T03 | integration | M0-T04；M2-T07/M2-T14/M2-T20；M3-T08；M4-T04 | Pi协议、配置/doctor/原生只读档、内置Skill、M4设置/对象；本任务交付安装及live驱动 |

| AC-051-03 | M5-T03 | recovery | M0-T04；M2-T07/M2-T14/M2-T20；M3-T08；M4-T04 | Pi协议、配置/doctor/原生只读档、内置Skill、M4设置/对象；本任务交付安装及live驱动 |

| AC-051-04 | M5-T03 | integration | M0-T04；M2-T07/M2-T14/M2-T20；M3-T08；M4-T04 | Pi协议、配置/doctor/原生只读档、内置Skill、M4设置/对象；本任务交付安装及live驱动 |

| AC-051-05 | M5-T03 | integration | M0-T04；M2-T07/M2-T14/M2-T20；M3-T08；M4-T04 | Pi协议、配置/doctor/原生只读档、内置Skill、M4设置/对象；本任务交付安装及live驱动 |

| AC-055-01 | M5-T02 | integration | M5-T06；M2-T16/M2-T18；M3-T02/M3-T10；M4-T04 | 安全恢复/资源归属、QA/Review与J9、影响闭包/监督、M4证据 |

| AC-055-02 | M5-T02 | integration | M5-T06；M2-T16/M2-T18；M3-T02/M3-T10；M4-T04 | 安全恢复/资源归属、QA/Review与J9、影响闭包/监督、M4证据 |

| AC-055-03 | M5-T02 | recovery | M5-T06；M2-T16/M2-T18；M3-T02/M3-T10；M4-T04 | 安全恢复/资源归属、QA/Review与J9、影响闭包/监督、M4证据 |

| AC-055-04 | M5-T02 | integration | M5-T06；M2-T16/M2-T18；M3-T02/M3-T10；M4-T04 | 安全恢复/资源归属、QA/Review与J9、影响闭包/监督、M4证据 |

| AC-055-05 | M5-T02 | integration | M5-T06；M2-T16/M2-T18；M3-T02/M3-T10；M4-T04 | 安全恢复/资源归属、QA/Review与J9、影响闭包/监督、M4证据 |

| AC-056-01 | M5-T07 | integration | M5-T02/M5-T10；M2-T13/M2-T18；M3-T02/M3-T07；M4-T04 | 回归/三插件与降级、CLI最终结果/Issue投影、规格与追踪、M4证据读取；本任务交付发布全量义务 |

| AC-056-02 | M5-T07 | integration | M5-T02/M5-T10；M2-T13/M2-T18；M3-T02/M3-T07；M4-T04 | 回归/三插件与降级、CLI最终结果/Issue投影、规格与追踪、M4证据读取；本任务交付发布全量义务 |

| AC-056-03 | M5-T07 | integration | M5-T02/M5-T10；M2-T13/M2-T18；M3-T02/M3-T07；M4-T04 | 回归/三插件与降级、CLI最终结果/Issue投影、规格与追踪、M4证据读取；本任务交付发布全量义务 |

| AC-056-04 | M5-T07 | integration | M5-T02/M5-T10；M2-T13/M2-T18；M3-T02/M3-T07；M4-T04 | 回归/三插件与降级、CLI最终结果/Issue投影、规格与追踪、M4证据读取；本任务交付发布全量义务 |

| AC-056-05 | M5-T07 | recovery | M5-T02/M5-T10；M2-T13/M2-T18；M3-T02/M3-T07；M4-T04 | 回归/三插件与降级、CLI最终结果/Issue投影、规格与追踪、M4证据读取；本任务交付发布全量义务 |

| AC-057-01 | M5-T08 | deployment | M5-T07/M5-T05；M2-T14；M4-T04 | 发布验证/备份、systemd安装与deployment、完整M4 PTY；目标Web驱动由本任务交付（Q1），无需M6 HoH Web |

| AC-057-02 | M5-T08 | tui-pty | M5-T07/M5-T05；M2-T14；M4-T04 | 发布验证/备份、systemd安装与deployment、完整M4 PTY；目标Web驱动由本任务交付（Q1），无需M6 HoH Web |

| AC-057-03 | M5-T08 | deployment | M5-T07/M5-T05；M2-T14；M4-T04 | 发布验证/备份、systemd安装与deployment、完整M4 PTY；目标Web驱动由本任务交付（Q1），无需M6 HoH Web |

| AC-057-04 | M5-T08 | integration | M5-T07/M5-T05；M2-T14；M4-T04 | 发布验证/备份、systemd安装与deployment、完整M4 PTY；目标Web驱动由本任务交付（Q1），无需M6 HoH Web |

| AC-063-01 | M5-T05 | recovery | M2-T01/M2-T05/M2-T14；M4-T04 | SQLite/WAL/事务、reconcile/锁/epoch、维护部署、M4引用对象/GC；本任务补backup pin与restore |

| AC-063-02 | M5-T05 | recovery | M2-T01/M2-T05/M2-T14；M4-T04 | SQLite/WAL/事务、reconcile/锁/epoch、维护部署、M4引用对象/GC；本任务补backup pin与restore |

| AC-063-03 | M5-T05 | recovery | M2-T01/M2-T05/M2-T14；M4-T04 | SQLite/WAL/事务、reconcile/锁/epoch、维护部署、M4引用对象/GC；本任务补backup pin与restore |

| AC-063-04 | M5-T05 | integration | M2-T01/M2-T05/M2-T14；M4-T04 | SQLite/WAL/事务、reconcile/锁/epoch、维护部署、M4引用对象/GC；本任务补backup pin与restore |


## CTO 问题（有推荐默认；不询问所有者）

所有默认都是DRAFT提案，下面的决定应由CTO在批准/应用前记入本CR；需升级所有者的边界仍按docs/21，不由作者擅自决定。

| ID | 真正未定的选择 | 推荐默认与理由 | 不采纳时的影响 |
|---|---|---|---|
| Q1（阻塞应用） | REQ-057的Web是工程目标验证，还是要求M5先验证HoH自身完整Web？ | 目标项目验证能力在M5；HoH Web仍在M6/M7真实验收。原given为“目标为CLI/Web或服务”，这样保留能力及完整Web范围且无M5↔M6环 | 若须HoH Web在M5验，则重排实现/verify依赖并重审，不能套用本稿闭包或删浏览器步骤 |
| Q2 | STEER受理时的代码快照足够，还是新增客户端显式expected code target？ | 本稿默认host受理时固定commit/tree/candidate，内部JSON与HCP传递，投递前再次校验；恰覆盖M3A-02同epoch/spec下C1→C2迟到问题，无外部schema破坏 | 显式历史目标更强但需增加SendInput字段、客户端快照与兼容语义；先出完整契约补丁和Rust契约回归，再批准，不把受理前窗口误说已保护 |
| Q3 | review series/family如何持久化及由谁确认同类？ | 事件+ArtifactRef、稳定series_id，family为带依据的分类，经Commander确认，Runtime执行计数/设计修正阻塞；不能以其它进展清零 | 若增加专表须新迁移/索引/恢复测试；既定四轮上限、两轮设计修正规则不重新表决 |
| Q4 | 精确插件版本/兼容窗口与hook支持的基线 | 以config/plugins.json的已锁来源/版本为起点，实施时读取安装schema和hook并实测；不兼容须证据降级或维护窗口经审查升级 | 不能在草案宣称这些版本兼容；若换版本，更新环境/锁/新候选并重跑，不能只写版本号 |
| Q5 | 四类经验元数据放插件原生字段还是HoH适配层？ | 原生支持则原生；缺字段在引用工件元数据中补，HoH保存来源/适用条件，Hermes仍负责长期召回，避免另建全局账本 | 如果版本无法提供项目隔离/恢复，降级真实集成，安装诊断required仍必须通过 |
| Q6（阻塞应用） | 全范围验证独立义务、报告收取及损坏证据的候选否决如何接线？ | release_validation_id事件+既有只读Attempt，复用证据/报告验证器而不伪造round；源Task/candidate不因普通发布结论迁移，完整性INVALID事实必须继续阻断。先在M5-T07设计ADR列清与record_evidence的边界 | 若需要新受锁schema/迁移或CR-018“不迁移”不能与现有否决路径同时成立，应用前补具体细化CR并审查；本稿不能把未定实现称已闭合，严禁吞INVALID或另开增量round绕过 |
| Q7 | backup/restore维护入口及长期保留策略 | 沿用现有交互/诊断入口到内部维护服务，恢复须独占且显式确认；所有被引用备份/证据保护，未引用清理按精确operation与可配置保留期 | 不加Shell命令；若要新API/配置键必须同实现/契约测试交付，配置不能先于加载器进入example。保留期数值不在此猜测 |

## 批准后应用清单与不变内容

| 文件/范围 | 拟改动与实施责任 |
|---|---|
| contracts/requirements.json | 只替换上述九对象，39条AC；旧九AC保留原范围/标识 |
| contracts/acceptance-scenarios.json | 只替换对应39个场景，check_id均在原计划 |
| contracts/development-tasks.json | 只替换M5四项为十项（1 spec、8 implement、1 verify）；CR-018/020承接文字完整进入T02/T07；M6/M7保持原依赖 |
| docs/02-functional-spec.md、docs/18-traceability.md | gen_spec_views生成，不手写另一套权威 |
| contracts/baseline-lock.json | 只有正式批准/应用或隔离机械实验才relock；真实工作树本次不变 |
| docs/24-dev-lessons-to-product.md | 正式应用时为吸收条目追加CR-030与AC引用，不删除原经验；本稿不编辑它 |
| 实现任务后续ADR/docs/06/07/14/15/用户指南 | 按Q决定同步已实现行为与执行过的命令；契约/SQL需要变化时先提交细化补丁与新编号迁移，不暗改旧migration |
| config/plugins.json与tests/check-plan.json | 本次应用片段均不改。M5-T03可执行driver交付时才替换plugin-live占位argv，保持mode/live、timeout/artifacts；版本锁变动须精确来源、完整性与兼容证据 |

不新增需求、不改CONTRACT、不改release_class/release_targets、policy、gate工具或任何required阈值。正式应用若涉及schema、SQL或枚举，要把源码映射列入影响闭包并运行完整`cargo test --workspace --locked`；本次不改这些文件，不把未运行的Rust/产品/live测试标PASS。

## 验证方式

验证边界：本次是**自测，非验收**，只证明文档片段的结构、归属、机械应用及包自检。没有运行产品红绿、真实插件、VM部署、三Agent、Web或soak，也没有独立设计审查。所有39条拟议AC的产品状态为NOT_RUN；CR保持DRAFT。不会报告v1 COMPLETED。

应用副本位于 `/home/ai/dev/m5spec-f6tgz29b/applied/repo`（基线副本为同根下的 `repo`），来自输入基线的本地共享对象克隆（Git写入只在副本）。实际命令日志保留在忽略目录 `.hoh-dev/evidence/cr030-draft1/`；副本及其测试临时目录结束后删除。基线首次测试因未加载env.sh失败，原日志保留，未改测试；后续显式加载真实工作树env.sh，并设置TMPDIR到该临时根，避免共享tmpfs。下面脚本是实际使用的片段应用器与闭包探针；只在隔离副本运行。将本文件复制到副本docs/cr/后，以副本为cwd执行。

### 片段应用器（apply.py）

```python
from pathlib import Path
import json,re,subprocess
root=Path.cwd()
assert str(root).startswith('/home/ai/dev/m5spec-') and root.name=='repo'
cr=root/'docs/cr/CR-030-m5-detailed-spec.md'
blocks=[json.loads(x) for x in re.findall(r'```json\n(.*?)\n```',cr.read_text(),re.S)]
reqs=[x for x in blocks if isinstance(x,dict) and x.get('id','').startswith('REQ-')]
scenarios=next(x for x in blocks if isinstance(x,list) and x and x[0].get('id','').startswith('TEST-'))
tasks=next(x for x in blocks if isinstance(x,list) and x and x[0].get('id')=='M5-T01')
assert len(reqs)==9 and len(scenarios)==39 and len(tasks)==10
for path,key,new in [('contracts/requirements.json','requirements',reqs),('contracts/acceptance-scenarios.json','scenarios',scenarios)]:
 d=json.loads((root/path).read_text()); repl={x['id']:x for x in new}
 keep=[x for x in d[key] if x['id'] not in repl];d[key]=sorted(keep+new,key=lambda x:x['id'])
 (root/path).write_text(json.dumps(d,ensure_ascii=False,indent=2)+'\n')
p=root/'contracts/development-tasks.json';d=json.loads(p.read_text()); out=[]
for t in d['tasks']:
 if t['id']=='M5-T01':out.extend(tasks)
 elif not t['id'].startswith('M5-'):out.append(t)
d['tasks']=out;p.write_text(json.dumps(d,ensure_ascii=False,indent=2)+'\n')
# This approval exists only in the disposable copy. It is not a CTO decision.
s=cr.read_text().replace('状态：DRAFT','状态：APPROVED',1)
s=s.replace('批准人：待 CTO 决定与独立 Review；本稿不批准','批准人：隔离副本机械验证占位；不构成正式批准',1)
cr.write_text(s)
print('applied 9 requirements, 39 scenarios, 10 tasks; scratch approval only')
```

### 结构与能力闭包探针（probe.py）

```python
from pathlib import Path
import json,re,subprocess,sys
root=Path.cwd(); sys.path.insert(0,str(root/'tools'))
from validate_bundle import ancestors,ownership_problems
req=json.loads((root/'contracts/requirements.json').read_text())['requirements']
scen={x['id']:x for x in json.loads((root/'contracts/acceptance-scenarios.json').read_text())['scenarios']}
tasks=json.loads((root/'contracts/development-tasks.json').read_text())['tasks']; ti={t['id']:t for t in tasks}
policy=json.loads((root/'contracts/release-policy.json').read_text());plan=json.loads((root/'tests/check-plan.json').read_text())
base='8d9ae599d30982fd431b950dec229786cdc15379'
def original(p):return json.loads(subprocess.check_output(['git','show',base+':'+p],text=True))
oldreq={r['id']:r for r in original('contracts/requirements.json')['requirements']}
ids={f'REQ-{n}' for n in ('044','048','049','050','051','055','056','057','063')}
assert not ownership_problems(tasks,req,scen,policy,plan)
for r in req:
 if r['id'] not in ids: assert r==oldreq[r['id']]
 else:
  for k in ('id','category','priority','milestone','title','required_release','release_class','release_targets','summary'): assert r[k]==oldreq[r['id']][k]
  assert 2<=len(r['acceptance'])<=5 and r['spec_maturity']=='detailed'
for p in ('contracts/release-policy.json','tests/check-plan.json','contracts/protocol.schema.json'):
 assert json.loads((root/p).read_text())==original(p)
oldtasks={t['id']:t for t in original('contracts/development-tasks.json')['tasks']}
assert all(t==oldtasks[t['id']] for t in tasks if not t['id'].startswith('M5-'))
m5=[t for t in tasks if t['id'].startswith('M5-')];impl=[t for t in m5 if t['kind']=='implement']
assert len(impl)==8 and m5[-1]['id']=='M5-T04'
assert all(len(t['requirements'])<=2 and len(t['acceptance'])<=8 for t in impl)
assert all('acceptance' not in t for t in m5 if t['kind']!='implement')
assert {t['id'] for t in impl}<=ancestors(ti,'M5-T04')
providers={
'044':['M5-T05','M3-T09','M3-T07','M2-T05','M2-T17','M4-T04'],
'048':['M5-T03','M5-T06','M3-T03','M3-T08','M2-T09','M4-T04'],
'049':['M5-T03','M5-T06','M2-T09','M2-T18','M4-T04'],
'050':['M5-T09','M5-T06','M4-T04','M2-T18'],
'051':['M0-T04','M2-T07','M2-T14','M2-T20','M3-T08','M4-T04'],
'055':['M5-T06','M2-T16','M2-T18','M3-T02','M3-T10','M4-T04'],
'056':['M5-T02','M5-T10','M2-T13','M2-T18','M3-T02','M3-T07','M4-T04'],
'057':['M5-T07','M5-T05','M2-T14','M4-T04'],
'063':['M2-T01','M2-T05','M2-T14','M4-T04']}
owner={a:t['id'] for t in impl for a in t['acceptance']}
assert len(owner)==39
for ac,t in owner.items(): assert set(providers[ac[3:6]])<=(ancestors(ti,t)|{t}),(ac,t)
# Graph-only check of the exact, approved-but-unapplied M4 proposal, not its product behavior.
m4=(root.parent/'m4-cr028.md').read_text()
blocks=[json.loads(x) for x in re.findall(r'```json\n(.*?)\n```',m4,re.S)]
m4tasks=next(x for x in blocks if isinstance(x,list) and x and isinstance(x[0],dict) and x[0].get('id')=='M4-T09')
future=dict(ti); future.update({t['id']:t for t in m4tasks})
for ac,t in owner.items():
 assert set(providers[ac[3:6]])<=(ancestors(future,t)|{t})
 assert {f'M4-T{i:02}' for i in range(2,12) if i!=4}<=ancestors(future,t)
print(json.dumps({'requirements':{r['id']:len(r['acceptance']) for r in req if r['id'] in ids},'AC_count':39,'implement_tasks':8,'ownership_errors':0,'capability_closure_errors':0,'M4_graph_overlay_errors':0,'unchanged_policy_plan_protocol':True,'product_accepted':False},ensure_ascii=False,indent=2))
```

### 已执行命令与结果

执行环境：非root用户ai；加载 `/home/ai/dev/hoh-wt/m5-spec/.hoh-dev/env.sh` 后观察到Python 3.14.4、rustc 1.99.1。应用副本的CR头临时标为APPROVED并注明“隔离副本机械验证占位；不构成正式批准”，真实文件始终DRAFT。

| 位置 / 命令 | 退出码 | 实际结果 |
|---|---:|---|
| 基线副本，未加载env：`bash scripts/check.sh --bundle` | 1 | 570 tests，186 failures、31 errors、54 skips；环境缺date-time校验支持及rustc PATH等，原始记录baseline-bundle.log，不作为规格失败或产品证据 |
| 基线副本，加载env且TMPDIR在本次临时根：`bash scripts/check.sh --bundle` | 0 | 570 tests，604.366 s，2个既有skip |
| 首次应用副本：`bash scripts/check.sh --bundle` | 143 | 已观察到既有test_live_auth_does_not_block_offline_core失败后停止；该用例要求REQ-048 owner直接依赖M0-T04，初稿只有传递依赖；非一次完成的套件结果，interruption.json记录本会话runner的精确PID/start_time |
| 定向复现：`python3 -m unittest discover -s tests/bundle -p test_bundle.py -k test_live_auth_does_not_block_offline_core -v` | 1 → 0 | 给M5-T09增加显式M0-T04依赖后转绿；未修改测试、门禁或产品代码 |
| 最终片段：`python3 /home/ai/dev/m5spec-f6tgz29b/apply.py` | 0 | 9个需求、39个场景、10个M5任务已应用到副本 |
| `python3 /home/ai/dev/m5spec-f6tgz29b/probe.py` | 0 | 39 AC / 8 implement；每任务≤2REQ/≤8AC；AC/check唯一owner、每AC能力闭包及CR-028任务图overlay均0错误；verify最后且无acceptance；其它REQ/任务、policy、plan、protocol内容不变 |
| `python3 tools/gen_spec_views.py` | 0 | 副本生成docs/02与docs/18 |
| `python3 tools/baseline.py relock --cr docs/cr/CR-030-m5-detailed-spec.md` | 0 | 仅副本从revision22到23，27个受锁文件；真实工作树锁未改 |
| `python3 tools/validate_bundle.py` | 0 | BUNDLE_ONLY PASS；70 REQ、52 detailed、244 AC、71 tasks；这是M5单独应用副本，未包含M4全部规格应用；product_accepted=false |
| 最终应用副本：`bash scripts/check.sh --bundle` | 0 | 570 tests，1670.112 s，OK (skipped=2)；不含新产品AC实现或live验收 |

最终两项skip保留：`test_ledger_outline_requirements_not_ready`（M3 already detailed）、`test_repository_packages_verified`（Pi packages not installed here）。未把它们记为PASS。上述红绿只针对文档任务图的既有包断言，不是生产行为TDD。

原始日志：baseline-bundle.log、baseline-env-bundle.log、initial-applied-bundle.log/exit、auth-targeted.log、auth-targeted-green.log、applied-bundle.log/exit；逐命令argv/退出码见commands.json；probe.log、relock.log、validate.log与source-bindings.json保留具体数据。资源根仅清理本会话创建的精确路径。

补入本验证记录及格式说明之后，11个JSON fence的摘要须与完整套件测试时一致；仅验证叙述变动。最终副本再以相同占位APPROVED头执行`python3 tools/baseline.py relock --cr docs/cr/CR-030-m5-detailed-spec.md --amend`、`python3 tools/validate_bundle.py`与`python3 tools/baseline.py verify`；这四项最终核对的实际退出码均为0（final-checks.json），不把副本批准/锁带回真实工作树。


包自检通过也不证明Q1/Q6已经裁决或前置里程碑行为可用。正式应用必须重新对照最终M4/M5主线、审查CTO决定、运行完整bundle与Rust契约回归，再由各实施任务完成真实TDD/独立验证。任何漂移先修CR再relock，不把本次副本锁复制回工作树。
