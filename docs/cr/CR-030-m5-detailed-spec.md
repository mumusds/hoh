# CR-030 · M5 证据、恢复与 Pi 插件需求细化（第 2 稿）

ID：CR-030
状态：DRAFT
批准人：本稿不批准；CTO 默认已于2026-11-22采纳，修订稿待独立复审通过后按 CR-003 D1 批准
日期：2026-11-22（沿用所引用 CTO/所有者决定的标注日期；执行日志保留实际 UTC 时间）
原稿输入基线：`8d9ae599d30982fd431b950dec229786cdc15379`；本次修订起点：`6c0fc5b67aeec72579f1cc2a5cf1070e9e17ee47`；工作树 `/home/ai/dev/hoh-wt/m5-spec`；分支 `prep/M5-T01-spec`
适用任务：M5-T01 准备；正式应用仍须 M4-T04 完成、领取 M5-T01、独立审查通过及 CTO 按 CR-003 D1 批准

## 背景与授权边界

本稿只提交这份 CR，不改真实工作树的受锁文件、产品源码、检查计划、任务板或全局 PROGRESS，不 relock、不运行 live Agent、不推送。下文是可机械应用的提案，39 条新细化 AC 的产品行为均未在本次实现。输入没有 Runtime 身份头或 TaskResult 输出文件；文档诊断证据按本轮委派保存在主仓库指定目录，不虚构 run_id/attempt_id/role_epoch/candidate_id 或 TaskResult。唯一写权限由委派载荷授予，起始工作区干净。

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
| A2 | CR-028 M4-T03 提供不可变对象、证据引用/GC保护、低空间及查询；T09 提供公开流/活动身份，T08 提供设置 CAS/原子写，T02/T06 提供Workspace/Focus/真实进度；T03/T10 已使用产品一致性备份克隆夹具库，T05须复用并扩展 | T05补备份pin，T06恢复，T03插件设置，T10映射，T08真实PTY；若实际接口/保护边界不同先修AC/ADR再审 |
| A3 | CR-028 迁移003只是预计的先行schema；CR-026迁移002已在输入基线，但M3实现任务尚需真实完成 | backup/migration测试选应用时实际最新迁移及真实旧binary，不固化猜测的后续编号；检查全量Rust契约测试 |
| A4 | CR-026规范已应用；M3-T02/06/09/04/07/10/08行为通过M3-T05提供，不能借已APPROVED文字当产品PASS | STEER投递、规格失效、追踪、停滞、四层Prompt均在依赖闭包；M3A-01/02在本稿补齐 |
| A5 | M6/M7并行草案尚未应用；M6负责HoH自身Web GUI与Git/文档，M7负责v1检查profile切换及真实soak/最终发布；本稿不假设它们提前实现；所有者2026-11-22已批准CR-031 C1（静态外壳无token、Host/Origin校验、每个/v1有token且不进日志/Referer），不再视为待定/阻塞 | REQ-057采用Q1目标项目解释；REQ-056用product判定器测试，最终HoH验收仍M7。任何后续CR改此分工，须重核M5依赖，无先签后补 |
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

## 共同约定与已采纳默认的细化方案

1. **证据层次。** `given/when/then`是拟议产品标准。下文每AC一个scenario与现有check id。原TEST-*-01保留原check；新增检查映射不新增/删减release-policy检查。任务的`checks`只表示维护者，执行者通过依赖使用。unit/integration=M0-T03、recovery=M2-T05、deployment=M2-T14、tui-pty=M1-T08、independent/traceability=M2-T10均不变；plugin-live唯一维护者仍M5-T03。
2. **product/mock/live。** 本地registry/FakeAgent/合成发布输入只是product测试内的故障夹具，不能记成真实Agent/插件/发布。`plugin-live`必须有真实Pi原生session与已加载plugin列表。认证/临时故障不是插件兼容性的替身。只对上游不可用/不兼容/无法安装按CR-001降级，execution与AC引用的diagnostic/upstream_ref均需合法。检查结局DEGRADED时引用它的AC不能是PASS；每AC按docs/10与test-result规则产出匹配的降级行，原始PASS子例可保留，不能伪称整个组合通过。REQ-051在product检查中始终required。
3. **未运行的表达。** `contracts/test-result.schema.json`行状态只有PASS/FAIL/DEGRADED/SKIPPED，不能写NOT_RUN/BLOCKED/FLAKY冒充合法test行。这些词是执行/汇总状态；未运行缺项在汇总中明确、不能进入满足集合。每个场景的多个子例用同TEST id但不同name，全部通过才满足；缺设施非零且不能全行PASS。 本稿场景的steps/pass_criteria逐一列必需 test_NNN_MM_<suffix>；实现任务在各现有检查驱动内维护对应清单并核对运行结果，少一个子例/矩阵格必须有该TEST的FAIL覆盖行且检查非零（删除一个子例验证此断言），不能利用“至少一行”的既有证据规则漏测。它是测试完整性断言，不改schema或gate。序列子例内部复用同一库/候选/栅栏，只有相互独立的子例使用独立项目。
4. **审查系列（Q3）。** 分别持久设计/实现review series、轮号、候选与spec、finding来源、缺陷family、处置和设计修正ArtifactRef；重启、修正或新提交不清同系列轮号，范围正式变更须明确记录新系列与原因，不能为逃避上限重置。连续两轮同family有可核对发现即在继续修复前要求设计修正，即使别的AC增长。初审全范围，复审聚焦修复diff及调用链/回归；范围外新medium/low记待办，不扩本轮；critical/high仍阻断。前3轮无critical/high且medium已处置可收敛；第4轮无critical/high时medium须修复并红绿、Runtime核对证据+只读Commander决定，low入包，不另开完整审查；第4轮仍critical/high先设计修正，只再开一次新会话第5轮核这些项，仍有即BLOCKED。任何required AC FAIL或发布门禁不满足仍阻断，审查收敛不篡改已有报告、不免独立验收。low包在verify前完成或按AC记有证据的延期。缺陷family由持久分类及证据支撑，不能单靠模型声称“不同问题”重置。 series的一轮是一次约定范围的独立审查及其有版本处置记录，设计/实现分别编号；ADR-028 verification round是每次冻结自动建立的QA/Review义务，二者以series_id/series_round与verification_round_id显式关联，互不替代。一轮处置可产生多个修复候选，每个候选仍完整走新验证轮次和Candidate Gate；重验不据此重开全范围设计讨论，缺独立Review/QA仍不能accepted。第4轮medium修复后的独立Review只核修复diff/调用链/回归并属于该轮处置，不构成第5轮全量设计审查；Runtime核对red/green与绑定，Commander只读决定收敛，Developer持唯一写租约。
5. **STEER（Q2，已采纳）。** code_target是受理时的来源，不是要求投递时HEAD逐字相等的锁：仅绑定发给当前repo写者（Developer或执行spec_draft的CTO等持写租约职位）的STEER；Commander、QA/Reviewer/Research等只读职位不加此绑定，仍遵守既有epoch/spec及QA/Review冻结候选交接。来源包含run/task、branch、lineage_generation、git_commit、tree_sha256、candidate_id或null，unborn时git_commit=null/unborn=true并记录只读工作区摘要。受理/写者提交登记/派发在安全点串行，任何异步等待后重核；允许原版本，或同Task同分支同generation且由Runtime登记的写者提交组成的后继链（含起草任务的首提交），不能仅凭Git祖先关系认后继。恢复（虽为追加提交）、切换分支、其它Task线或未知外部提交都使旧来源失效，原始来源不改写；外部未提交编辑仍先停写。HCP envelope携带投递时真实当前candidate，正文引用保留source code_target与证明链，REQ-022当前目标校验保持严格；无candidate不造id。支持turn者按turn投递，不支持的Claude/Pi按next_attempt投递，本写者完成C2不会单凭hash变化拒绝；跨Task意图须新指令，不能暗转。拒绝为CANDIDATE_MISMATCH并留expected/actual；role/spec按原原因码；不可确认原生投递只令Attempt/effect UNKNOWN_EFFECT，input_queue保持DELIVERED。客户端显示受理来源及生效边界，不声称保护受理前的旧用户视图；无新增SendInput外部字段。
6. **恢复/备份（Q7）。** 每次维护动作operation_id绑定项目/Run/Attempt/epoch、登记资源、effect阶段与manifest；完成仅原子提交后可见。备份固定DB快照与引用闭包，复制期pin对象防GC；恢复独占项目锁、停派发并收尾所有Attempt，再校验/原子替换/reconcile。维护从现有交互/诊断入口接入；不加Shell子命令。不可恢复外部效果不盲重放。
7. **全范围发布验证（Q6，已采纳并补齐接线）。** 按release_validation_id持久绑定run/spec/candidate/environment/test_plan/profile、两个独立只读Attempt、scope与实际送达确认集；以请求幂等键和当前身份CAS建立一个活动义务。不得伪造verification.round_opened或用round_of_task猜归属。record_evidence入口先验证真实生产者的持久归属，再分增量轮次、release义务、既有非轮次三种路径；宣称发布生产者却未关联、或同时关联两种义务均拒绝；既有合法非轮次生产者仍走原路径。共用同一证据结论函数、原生身份/独立性/hash校验，证据/evidence.recorded照旧入库；release的AC行和报告以release_validation_id写入独立事件/工件投影，不落入候选的acceptance_results，重启按该id/record_id去重回放。普通PASS/FAIL不迁移C及源Task、不改变C的增量验收投影；FAIL登记带release_validation_id/证据/归属的Issue并保持发布阻断。**唯一例外是完整性INVALID：仍在record_evidence同一事务按ADR-027决定5否决C（包括ACCEPTED→INVALID）并写integrity_violation/唤醒Commander，绝不吞掉否决；既有INVALID行及全局否决保留，禁止向INVALID候选新增PASS/DEGRADED行。** 这澄清CR-018“不迁移”是普通发布结论不回写增量状态，而不是完整性豁免；ADR-028非轮次默认写验收行分支明确排除release生产者，原非轮次夹具/其它路径不变。发布QA/Review报告复用验证器但按release身份收取，报告失败只阻断并登记Issue，完整性违规走同一否决；结果收取及issues::settle_all在同一事务。无结论可在同C新建关联的重验义务，候选上发布失败/否决历史按原规则保留，不能换id洗白；普通FAIL的返工由Commander决定并通过既有写任务路径产生新候选，不由发布收取偷改ACCEPTED源Task；INVALID只允许新提交/新候选经完整Candidate Gate后重新发布。换spec/candidate使旧义务只作历史；取消停止派发/收取推进，保全证据；无法恢复/身份不明维持BLOCKED。manifest/releases/run.result提交再次CAS当前身份与Issue快照。M5-T07的ADR只补函数和必要新迁移的映射/契约回归，不把本接线延期。
8. **真实部署范围（Q1）。** M5的浏览器测试是HoH验证受控工程目标的实际浏览器流程，目标fixture有真实进程、DOM/网络、数据与升级回滚断言。它不冒充HoH尚未完成的M6 Web GUI。HoH自身完整Web仍由M6 web-e2e及M7全量验收。Q1已于2026-11-22采纳，不再阻塞本稿应用；实现/验收仍必须有双OS真实记录。

Q6 的现有接线核对（本次修订起点 `6c0fc5b6`，只是读源码，不声称新能力已实现）：`crates/hoh-core/src/persistence/evidence.rs` 从生产者 Task 查 `round_of_task`，非轮次会即时写 `acceptance_results`，随后在同事务处理 `INVALID`、`evidence.recorded`、`integrity_violation` 与 `issues::settle_all`；`crates/hoh-core/src/persistence/reports.rs::intake_in` 目前要求生产者属于验证轮次。因此 T07 必须在这两个入口按持久 release 身份明确分路，不能只给非轮次 Attempt 加一个字段后沿用旧写入路径。共同约定7规定结果去向，新增的派发、完成、reconcile 和报告收取分支均需沿此归属，重启不能退回非轮次默认分支。正式实现的契约/迁移/事件枚举影响闭包要包含这些调用者与完整 Rust 回归。

## 需求对象：拟替换 requirements.json 的九个对象

以下每个JSON fence是一个完整需求对象。其余需求的字段值、数组与字段顺序保留；不改schema、policy或check-plan。第2稿隔离应用器按输入文件的一空格缩进、UTF-8、LF写入，探针逐对象核对未改内容；正式应用仍须重新生成/重锁/验证，不能照搬本次副本锁。

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
      "given": "同一项目已有经验证的基线 B、严重回归候选 C；HEAD 在 HoH 登记的工作分支 W 上；原工作区分别有用户 tracked、暂存、untracked 修改，失败记录已登记",
      "when": "请求恢复 B，先在脏目录观察拒绝，再经用户处置后在 HoH 登记的干净工作区执行恢复",
      "then": "脏目录逐文件字节、index、HEAD、untracked 清单不变，停止新写派发并给出待用户处置原因；用户处置后，先以 refs/heads/hoh/failed/<run_id>/<operation_id> 保存 C（已存在且不指向 C 则拒绝），保留日志与证据；在本 operation 登记的临时 worktree 构造恢复提交 R，parent 为 W 当前 HEAD、tree 等于 B，并在独占安全点以预期旧 HEAD 的 CAS 只追加推进 W；用户目录仍在 W，不 checkout B、不 reset/rebase/force、不改其它分支；B/C 的 ref 与证据 hash 不变；R 不是已验收候选，冻结 R 后必须重验，不能复用 B 的 PASS",
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
      "given": "同一 Run/Task 的写者在 C1 上运行，role_epoch/spec 不变；支持 turn 引导及仅支持 next_attempt 的适配器各一组；另有恢复、换分支、其它任务提交和外部编辑变体，以及默认 Commander 输入",
      "when": "在 C1 受理 STEER 后正常投递并重投；对 next_attempt 先由该写者完成 C2 再投递；分别在恢复/换分支/他人任务线后投递；发送默认 Commander 输入作为对照",
      "then": "写者输入的受理事务固定 code_target 来源（commit/tree/candidate、branch、run/task、lineage_generation）并经队列、事件与 HCP 正文引用保留，不重绑为 C2；当前仍为 C1，或 C2 是 Runtime 记录的同任务同分支同 generation 正常后继时，按 turn/next_attempt 边界恰应用一次；交接的当前 candidate 与来源分别记录，原 HCP 当前候选校验不放宽；恢复即使产生后继提交也因 generation 改变拒绝；换分支、他人任务线或无法证明后继均 REJECTED(CANDIDATE_MISMATCH)，不调用原生 steer；外部未提交编辑先阻塞写入；Commander 等只读职位的普通输入不受写者 code_target 拒绝，原 epoch/spec/候选交接规则保留；ASK/REPLY/CONTROL 不变",
      "test_id": "TEST-044-04"
    },
    {
      "id": "AC-044-05",
      "given": "三个独立起点：有 commit 无 candidate 的写者；HEAD 尚 unborn 的首规格起草写者；C1 上已投递但未记回执的写者；各有持久来源记录",
      "when": "重启后消费/去重，分别切换 role_epoch、spec；unborn 起点先保持原状态投递，再由同一起草 Attempt 产生首提交并在 next_attempt 投递；另由外部生成首提交作反例",
      "then": "有 commit 无 candidate 时不造 candidate；unborn 受理为 git_commit:null、unborn:true 并绑定分支、只读工作区摘要和 lineage_generation，允许同状态或 Runtime 可证的本起草任务首提交后继，外部/未知首提交拒绝；原 code_target 永不重绑；不合来源/后继关系用 CANDIDATE_MISMATCH，角色不符 STALE_ROLE_EPOCH，规格不符 SPEC_MISMATCH；不可确认的原生投递令 Attempt/effect 为 UNKNOWN_EFFECT，input_queue 仍 DELIVERED，不发明队列状态、不盲重投；可核对重复回执不重复 APPLIED",
      "test_id": "TEST-044-05"
    }
  ],
  "behavior": [
    "恢复只追加工作分支 W 的新提交 R（parent=当前 HEAD，tree=B），失败 ref 的命名/CAS/冲突见 AC-044-01；用户目录不换分支。临时 worktree 仅用于构造与核对，不强制把已在 W 上打开的用户目录切走；最终应用树前再次核对工作区/index 与租约，发现用户编辑保全现场并阻塞。恢复提交仍需新候选验证。",
    "恢复清单是持久 ArtifactRef，包含活动工作、精确资源归属、证据、预算、下一动作与唤醒条件；它是恢复线索，不是 Runtime/Git 的替代事实源。",
    "运行中 STEER 补齐 CR-026 M3A-02：写者来源 code_target 与投递时当前目标分开，只有共同约定5的相等或可证正常后继允许投递；受理、提交登记与投递在写者安全点串行，异步等待后重核。恢复/换分支/外部修改不可借 Git 祖先关系绕过。",
    "不加 SendInput 外部字段：code_target 是 host 的内部来源元数据，存于自由 JSON 与事件，客户端显示来源和生效边界。Commander/QA/Reviewer/Research 等只读职位不加此写者绑定；QA/Review 原冻结候选交接仍严格相等。unborn 与正常 next_attempt 不改变 REQ-017 的既定可用性。"
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
    "Q2 默认由 CTO 于2026-11-22采纳：受理时的代码快照；按首轮CR30D-02/07补充来源与正常后继判定、unborn，客户端显式选择历史目标不在本次协议中，后续需求须另立CR。",
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
      "then": "Compact 单插件周期无 Hermes flush，必须一次 compact 后正常 resume；Compact+Hermes 与三插件周期均在 Hermes flush 成功后才完成 compact；各组合重入只进入一次 compact、拒绝或合并有日志，模型调用逐次可追溯且无无限递归；仅含 Hermes 的组合注入 flush 失败时不丢经验、不把未完成压缩标成功；不支持所需 hook 的真实版本按 CR-001 带证据降级",
      "test_id": "TEST-048-04"
    }
  ],
  "behavior": [
    "仅 alpertarhan 指定来源负责压缩连续性；版本和配置来自安装包可核对内容，不发明设置键。",
    "压缩后强制加载 Runtime 的当前任务、候选、证据、队列与预算，摘要只作线索（docs/24 §3.3）。",
    "单插件无 Hermes flush；含 Hermes 的两插件/三插件组合遵守 flush→compact→resume 与重入保护；各组合经真实 Pi 会话验证。"
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
    "Q4 默认由 CTO 于2026-11-22采纳：当前锁定来源为起点，版本能否兑现 hook 由实施时实测决定，不预宣称兼容。",
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
      "then": "Runtime 的 accepted、Issue 状态及 Candidate Gate 不受记忆 claim 改变；用已具备完整 Candidate Gate 正对照的相同输入只删真实观察后，缺证据未满足、失败仍阻断；能定位召回来源与冲突原因；无召回时规格/AC/候选强制项仍完整。完整发布门禁的 claim 负例由后续 AC-056-01 验证",
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
    "Q5 默认由 CTO 于2026-11-22采纳：原生元数据足够则使用原生；缺项由HoH引用工件元数据补齐，Hermes仍负责长期召回，不另建全局经验事实账本。"
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
      "given": "本地故障注入上游404/不兼容/安装失败，以及 Codex/Claude/Pi 三适配器各自的缺认证/429/503/provider内容过滤原始错误夹具；required AC和degradable AC各一组",
      "when": "生成诊断与模拟发布输入，逐项删去diagnostic/upstream_ref或把REQ-051标DEGRADED",
      "then": "仅符合CR-001的插件不可用可形成带执行绑定、diagnostic ArtifactRef与真实来源引用的降级；缺字段拒绝；REQ-051及任何required检查永不以DEGRADED满足；缺认证/额度/临时故障/内容过滤保持各自阻塞原因，过滤记录failure_class=provider_content_filter的诊断并交现有决策路径，不假称能力缺失、不自动耗尽重试",
      "test_id": "TEST-051-04"
    },
    {
      "id": "AC-051-05",
      "given": "HoH TUI 的设置→插件表单，隔离配置启用三插件；Commander 选择 Pi、QA 选择 Pi，RPC 无 custom UI；adapter schema 匹配/不匹配各一例",
      "when": "经真实 PTY 打开表单、键盘编辑、提交无效值、合法保存后关闭重开，再构造两职位启动 argv 并运行 doctor；另提交 schema 不匹配配置",
      "then": "表单可键盘到达并显示三插件来源/版本/设置；无效值显示字段错误且磁盘配置不变，合法值经已核实 adapter 校验与 CAS 原子保存，重开值一致、不写原生用户目录；schema 不匹配保留旧配置并显示诊断；Commander argv 保留 CR-024 禁扩展且无显式 -e，QA 加载锁定三插件；RPC 无 UI 时 HoH 表单仍可用并显示适配说明，doctor 区分角色差异",
      "test_id": "TEST-051-05"
    }
  ],
  "behavior": [
    "安装/诊断/显式降级自身为required；以本地registry和故障注入验证确定性逻辑，实际插件行为另由plugin-live证明。",
    "组合锁绑定Pi版本、每插件来源/包名/精确版本/integrity、配置schema摘要与hook策略；Run启动快照固定，维护窗口更新不改活动Run。",
    "保留三插件职责与 Pi Commander 禁扩展；HoH TUI 设置→插件表单经 M4-T08 的键盘动作、配置 CAS/原子写能力接入经核实的 adapter，RPC 无 custom UI 不免除本表单 required 义务；不扩展 Shell 命令面。",
    "provider_content_filter 是所有底座适配器（Codex/Claude/Pi）的诊断 failure_class，不限插件。M5-T03 交付共用诊断映射及各底座夹具回归，保留原始错误、既有阻塞/等待语义，不新增 ErrorCode、不猜 fallback 成功；M7 可复用此分类但不得据此宣称 live 已通过。"
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
    "Q4 默认由 CTO 于2026-11-22采纳：只有真实上游证据符合 CR-001 才可降级，认证/临时 provider 问题不等于插件不兼容。"
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
      "then": "回归打开可定位a/原基线/新候选的regression Issue；全部attempt与stdout/stderr保留；FAIL→PASS为FLAKY且不满足strict；无结论超时不产生验收行，随后完整PASS时投影为PASS而非FLAKY（ADR-027/029），不错误混同有结论失败；运行顺序不改变历史聚合",
      "test_id": "TEST-055-01"
    },
    {
      "id": "AC-055-02",
      "given": "模块依赖图含传递依赖及未知影响文件；产品矩阵计划含 Ubuntu24.04/26.04 × Python3.12/3.14 四格；integration 有明确标为 fixture 的四格执行器，另锁定本机实际 Python3.12/3.14 两解释器",
      "when": "对已知修改求闭包，对未知影响/release选检查；夹具执行器仅一格返回FAIL；分别用两真实解释器执行路径/过长文件名回归，并删除一个必需解释器作反例",
      "then": "闭包含所有传递相关AC/TEST，未知影响/发布选full required suite；四格调度/聚合的fixture失败不被其它PASS掩盖，计划缺格/漏结果明确未满足；真实解释器记录实际版本、argv和结果，路径边界各有断言，缺解释器不能PASS；fixture不冒充真实双OS，双OS fresh VM实跑仍由AC-057-01/03负责",
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
      "then": "连续两轮即要求带证据的设计修正，不能被其它进展重置；发现按session+finding_id唯一，语义去重保留各来源；冲突按复现事实判定；第4轮无critical/high时medium全部修复并红绿、由 Runtime 核对证据完整性、只读 Commander 作收敛决定后结束本系列的额外设计讨论，low入里程碑清理包；第4轮仍有critical/high则设计修正后仅一次新会话第5轮核对，仍有则BLOCKED；重启不清轮数；review series 轮号与 ADR-028 验证轮次分别记录；每个修复候选仍自动开 QA/独立Review 验证轮次并完整通过 Candidate Gate，无独立报告时仍阻断；轮数上限不把FAIL/未满足AC变PASS",
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
    "Q3 默认由 CTO 于2026-11-22采纳：事件/ArtifactRef持久化 series/family，Commander确认分类、Runtime执行计数；设计和实现系列分开，设计修正不重置轮数；与自动验证轮次的映射见共同约定4。",
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
      "given": "产品发布判定器有完整正对照输入，绑定candidate/tree/spec/environment/test_plan；另分别构造空AC/空检查、缺test、skip/mock、hash错、旧规格/候选、独立会话复用、缺原生记录、缺live agent、soak不足、遗留high；另有 Hermes 召回或模型 claim 声称已PASS/已关闭high、但删去相应观察的变体",
      "when": "经Runtime请求发布，对每个负例只破坏一个条件并读取最终结果",
      "then": "正对照仅在全部规则满足才有可发布结论；全部负例逐项拒绝并给出缺项；只通过候选或里程碑/v0.1不能输出v1 COMPLETED；Hermes召回/模型claim/截图/进程exit0不替代证据；fixture正对照只证明判定器，绝非HoH v1实测完成",
      "test_id": "TEST-056-01"
    },
    {
      "id": "AC-056-02",
      "given": "同Run的两个开发Task已在不同增量候选接受，最终C为ACCEPTED但仅覆盖最后增量；具备持久release_validation_id",
      "when": "Runtime触发全范围验证，重复release_request并在两方结果收取间重启；分别换spec/candidate、取消、提交普通FAIL、篡改证据hash/源文件；最后重验或以新提交返工",
      "then": "全范围覆盖profile每项required AC/check，不开verification.round_opened；发布结果行按release_validation_id隔离、不写入或改变C的增量acceptance_results投影；正常PASS/FAIL不迁移源Task或候选，普通FAIL登记有主Issue并阻断发布；完整性INVALID是唯一例外，仍经record_evidence同事务把C从ACCEPTED等可否决状态置INVALID、写integrity_violation并唤醒Commander，禁止新增PASS/DEGRADED行且原Task状态不被发布义务偷偷迁移；同请求仅一个活动义务，独立QA/Review身份绑定及重启去重不变；换身份旧结果只作历史、取消停止发布；无结论可在同C新验证义务重验，FAIL需修复而INVALID必需新提交新候选及其完整Candidate Gate，历史不洗白，齐全合法前Run不COMPLETED",
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
    "发布验证以 release_validation_id 绑定 run/spec/candidate/environment/test_plan/profile/两个只读 Attempt；事件为权威、派发/结果收取不借 round_of_task。record_evidence 先按真实生产者关联区分增量、发布、既有非轮次路径，发布行进独立发布义务，完整性否决仍共用即时事务；普通 FAIL 只阻断并登记 Issue，不改已接受源 Task/候选，INVALID 例外及出口见共同约定7。M5-T07 的实现 ADR 细化函数/迁移映射，不再把这些设计选择后推。",
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
    "Q6 默认由 CTO 于2026-11-22采纳：独立 release_validation_id 义务；本稿共同约定7已补批准前要求的 INVALID 接线，保留所有否决与门槛。受锁 schema/新编号迁移若实施时需要，按本决定单独细化并审查，禁止借增量轮次绕过 CR-018。"
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
    "Q1 默认已由 CTO 于2026-11-22采纳：本需求实现HoH对CLI/Web/service工程目标的真实UX/部署验证能力；M5浏览器操作受控的真实目标Web应用。HoH自身完整Web GUI仍为REQ-058..061/M6与v1 web-e2e硬门禁，不能省略。",
    "fresh VM逐OS独立执行，不把容器/临时HOME替代VM；相同候选构建包、原始日志与实际版本/数据结果可核对。",
    "接入既有deployment/tui-pty/integration检查，不抢check owner、不更改policy；检查脚本与test-results逐项报告。",
    "M5-T08 负责双OS镜像、VM生命周期/清理与VM内PTY/浏览器/service驱动（供M6复用），环境清单锁定镜像来源/hash、虚拟化/驱动/浏览器版本及构建身份；先实测完整deployment所有阶段，在既有3600s内预留600s收尾余量，装不下提交CTO重规划，禁止延长timeout/省略AC或以容器替VM。"
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
    "Q1 已采纳，HoH自身Web仍在M6/M7；同步所有者2026-11-22批准的CR-031 C1：静态外壳无token但校验Host/Origin，每个/v1请求有token，token不进日志或Referer；这不是本M5目标Web夹具的HoH Web验收。",
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
      "when": "从TUI设置→维护选择备份/恢复，查看operation_id/进度；先不确认、取消确认，再确认恢复活动库，最后在停派发/所有Attempt安全收尾并取得独占项目锁后恢复正确/错误备份；过程中查看和请求取消",
      "then": "确认页展示project、备份水位/schema/hash及将替换的目标；未确认或取消确认无替换/无恢复effect；确认后活动状态仍拒绝破坏性替换并返回待静止条件；独占恢复只接受匹配项目和支持迁移链，拒绝错误备份且保原库；重启后同operation_id进度及安全点取消结果可从同一维护页读取；恢复后先reconcile、递增host_epoch/fence旧租约，不盲重投started效果；integration驱动与页面相同的维护动作/确认/查询服务，不以新Shell子命令实现",
      "test_id": "TEST-063-04"
    }
  ],
  "behavior": [
    "采用SQLite在线备份API得到DB一致性快照，再按该快照的引用闭包复制不可变对象并生成manifest；固定读水位、GC保留pin覆盖复制期，不能只复制主DB或遗漏WAL。 M4-T03/T10 已依赖的产品一致性备份基础必须复用并扩展，不重复造库；M5补manifest、完整引用闭包/pin、恢复维护与故障矩阵。",
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
    "Q7 默认由 CTO 于2026-11-22采纳：沿用TUI设置/维护和诊断入口及内部服务，明确确认与可查进度，按operation精确归属清理；不新增hoh backup/restore Shell命令。保留期随实现配置与加载器交付，引用/证据始终受保护。",
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
    "fixture": "同一项目已有经验证的基线 B、严重回归候选 C；HEAD 在 HoH 登记的工作分支 W 上；原工作区分别有用户 tracked、暂存、untracked 修改，失败记录已登记；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_044_01_protect_dirty：分别带tracked、index、untracked修改请求恢复B",
      "必需子例 test_044_01_append_restore：用户处置后保存C，构造并CAS追加R到W",
      "必需子例 test_044_01_ref_conflict：预置同operation失败ref指向其它提交，再恢复",
      "必需子例 test_044_01_head_race：构造R后使W或工作区改变，再提交恢复"
    ],
    "pass_criteria": [
      "test_044_01_protect_dirty：三个变体均阻塞，逐字节/index/HEAD/untracked清单不变",
      "test_044_01_append_restore：failed ref指向C，R父为旧W头/tree=B，用户仍在W，B/C及证据不变，R需新验收",
      "test_044_01_ref_conflict：拒绝且不覆盖旧ref/用户树，保留诊断",
      "test_044_01_head_race：CAS或安全点复核失败，不覆盖新HEAD/用户文件",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "恢复动作具有 operation_id，持写租约；有一个由该 Attempt 登记的 worktree 与一个不属于它的同前缀目录；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_044_02_crash_after_ref：保存失败ref后kill并以同operation恢复",
      "必需子例 test_044_02_crash_after_worktree：登记临时worktree后kill并恢复",
      "必需子例 test_044_02_crash_after_commit：R已写但未登记时kill并恢复",
      "必需子例 test_044_02_cancel_unknown：安全点取消并使一个外部效果无法确认"
    ],
    "pass_criteria": [
      "test_044_02_crash_after_ref：只一个失败ref及最终恢复提交，完成事实最多一次",
      "test_044_02_crash_after_worktree：按登记身份采用/清理自己的worktree，旁观同前缀目录不变",
      "test_044_02_crash_after_commit：先查真实commit/effect，不再创建第二R",
      "test_044_02_cancel_unknown：保留续跑点，未知Attempt/effect阻断新派发，无虚假完成或越权清理",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "恢复清单绑定 run/task/attempt、spec_sha256、candidate_id、commit/tree、host/role/lease epoch；含活动工作身份、证据路径、剩余预算、next_action 和 wake_condition；摘要故意称后台工作已完成；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_044_03_stale_summary：以宣称活动工作完成的摘要恢复",
      "必需子例 test_044_03_missing_reference：删除清单所引用的工件后恢复",
      "必需子例 test_044_03_hash_mismatch：将基线内容变为非清单hash后恢复"
    ],
    "pass_criteria": [
      "test_044_03_stale_summary：Runtime仍活动者保留真实身份/wake_condition，next_action据对账生成，预算不重置",
      "test_044_03_missing_reference：阻塞且指出缺失引用，历史清单保留",
      "test_044_03_hash_mismatch：阻塞且不重投已观察效果、不覆盖历史",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "同一 Run/Task 的写者在 C1 上运行，role_epoch/spec 不变；支持 turn 引导及仅支持 next_attempt 的适配器各一组；另有恢复、换分支、其它任务提交和外部编辑变体，以及默认 Commander 输入；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_044_04_turn_once：C1下受理并在支持turn的写者投递/重投",
      "必需子例 test_044_04_next_attempt_successor：不支持turn的写者在同Task正常提交C2后接下一Attempt",
      "必需子例 test_044_04_restore_reject：C1入队后在W上追加恢复R，再投递",
      "必需子例 test_044_04_branch_task_reject：分别换分支、切他人Task提交、制造无归属提交后投递",
      "必需子例 test_044_04_external_dirty：C1受理后加入外部未提交改动再投递",
      "必需子例 test_044_04_readonly_input：向Commander及QA/Reviewer/Research只读职位发输入"
    ],
    "pass_criteria": [
      "test_044_04_turn_once：来源固定且HCP当前身份正确，boundary=turn，业务应用一次",
      "test_044_04_next_attempt_successor：C2为登记的正常后继，来源仍C1，boundary=next_attempt，恰应用一次",
      "test_044_04_restore_reject：虽Git为后继仍因恢复generation改变REJECTED(CANDIDATE_MISMATCH)，无原生调用",
      "test_044_04_branch_task_reject：各变体拒绝并保留expected/actual，不靠祖先关系放行",
      "test_044_04_external_dirty：先停写保护用户修改，不投递旧指令",
      "test_044_04_readonly_input：不被写者code_target拒绝，已有epoch/spec与冻结候选校验仍执行，ASK/REPLY/CONTROL回归不变",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "三个独立起点：有 commit 无 candidate 的写者；HEAD 尚 unborn 的首规格起草写者；C1 上已投递但未记回执的写者；各有持久来源记录；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_044_05_commit_without_candidate：有提交无candidate时受理、重启后消费",
      "必需子例 test_044_05_unborn_stable：HEAD unborn的起草者受理并在相同状态投递",
      "必需子例 test_044_05_unborn_first_commit：unborn受理后本起草Attempt作首提交并在next_attempt消费",
      "必需子例 test_044_05_unborn_foreign_commit：unborn受理后外部造首提交再消费",
      "必需子例 test_044_05_stale_identity：分别改role_epoch或spec后消费旧输入",
      "必需子例 test_044_05_delivered_recovery：原生投递后回执前崩溃，分别有去重证明/无法确认效果后恢复"
    ],
    "pass_criteria": [
      "test_044_05_commit_without_candidate：按来源/后继验证，不伪造candidate且业务应用一次",
      "test_044_05_unborn_stable：来源git_commit=null/unborn=true及分支/工作区摘要持久，允许投递",
      "test_044_05_unborn_first_commit：验证Runtime首提交归属，来源不重绑，正常应用一次",
      "test_044_05_unborn_foreign_commit：CANDIDATE_MISMATCH且不调用原生steer",
      "test_044_05_stale_identity：各以STALE_ROLE_EPOCH/SPEC_MISMATCH拒绝，原code_target不变",
      "test_044_05_delivered_recovery：可证重复只应用一次；不可证Attempt/effect UNKNOWN_EFFECT、输入DELIVERED且不盲重投",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "锁定 alpertarhan/pi-smart-compact 的真实 Pi 非 Commander 会话，任务/AC/候选/证据引用齐全，Hermes 为唯一长期记忆所有者；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_048_01_real_auto_compact：在真实Pi非Commander会话达到锁定版本自动压缩条件后继续原Task一步",
      "必需子例 test_048_01_single_memory_owner：在同会话观察Compact跨会话后端与Hermes实际加载/调用"
    ],
    "pass_criteria": [
      "test_048_01_real_auto_compact：原生日志证明加载作者/版本及自动compact/resume，任务/AC/spec/candidate/证据hash可解析",
      "test_048_01_single_memory_owner：Compact可选长期记忆未启用，Hermes唯一长期后端，不用手写摘要替压缩",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "压缩摘要称 Task A 未完成、候选 C1 有效，Runtime 已接受 A 并采用 C2；另有旧 role_epoch 输入；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_048_02_runtime_wins：摘要称A未完C1有效而Runtime已接受A且为C2，压缩后恢复",
      "必需子例 test_048_02_duplicate_epoch：重复恢复通知并送旧epoch输入"
    ],
    "pass_criteria": [
      "test_048_02_runtime_wins：按当前Runtime/Git/epoch继续，不重做A、不计C1验收且显示冲突来源",
      "test_048_02_duplicate_epoch：状态最多应用一次，旧epoch仅历史，预算/身份不重置",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "真实 Pi 会话有已 observed 的工具效果与尚未确认的外部效果，正在压缩；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_048_03_interrupt_resume：真实compact期间中断后重启续会话",
      "必需子例 test_048_03_cancel_compact：真实compact期间请求取消并恢复",
      "必需子例 test_048_03_unknown_duplicate：compact时保留未知外部效果、重复通知后恢复"
    ],
    "pass_criteria": [
      "test_048_03_interrupt_resume：已observed工具效果不重做，任务/证据/预算及失败次数保持",
      "test_048_03_cancel_compact：无完成claim和新写派发，原证据/取消结果可核对",
      "test_048_03_unknown_duplicate：先reconcile，不确认就UNKNOWN_EFFECT，不重复执行",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "已安装版本的 Compact 单插件、Compact+Hermes、三插件组合；记录 hook 与模型调用计数；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_048_04_compact_only：Compact单插件触发一次并在hook内重入",
      "必需子例 test_048_04_compact_hermes：两插件触发压缩并重入",
      "必需子例 test_048_04_three_plugins：三插件触发压缩并重入",
      "必需子例 test_048_04_flush_failure：分别在两/三插件组合令Hermes flush失败"
    ],
    "pass_criteria": [
      "test_048_04_compact_only：无Hermes flush，一次compact/resume；重入拒绝/合并有日志，额外模型调用可逐次追溯",
      "test_048_04_compact_hermes：Hermes flush成功先于compact完成，仅一次compact，无递归模型调用",
      "test_048_04_three_plugins：同两插件顺序且todo连续，真实加载列表齐全，调用计数可核对",
      "test_048_04_flush_failure：经验保留，不声称compact成功；真实不兼容版本按CR-001诊断降级",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "项目 A 的真实 Hermes 会话记录一次已验证失败经验，项目 B 含同名文件；经验带 source ArtifactRef、project、confidence、validity 与观察时间；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_049_01_project_recall：A真实写经验后新会话召回，B同名文件查询",
      "必需子例 test_049_01_explicit_generic：显式授权一条跨项目通用经验后在B查询",
      "必需子例 test_049_01_missing_metadata：去掉source或适用范围再召回"
    ],
    "pass_criteria": [
      "test_049_01_project_recall：A来源hash/时间可核对，B不见A项目专属内容，不产生acceptance/Issue解决事实",
      "test_049_01_explicit_generic：仅授权通用项可跨项目，范围/来源保留",
      "test_049_01_missing_metadata：标未验证、不进入硬上下文、不虚构来源",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "用户、反馈、项目、参考四类经验，各带 observed_at、validity、source；一条过时，一条与现规格冲突，一条收到用户纠正；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_049_02_four_categories：分别召回用户/反馈/项目/参考四类经验",
      "必需子例 test_049_02_expire_correct：在同存储令一条过时并纠正一条，重建索引再查",
      "必需子例 test_049_02_spec_conflict：召回与当前规格冲突内容且使一次硬规格未召回"
    ],
    "pass_criteria": [
      "test_049_02_four_categories：分类/observed_at/validity/source均保留，可核对时间或年龄",
      "test_049_02_expire_correct：旧项撤出活动召回，纠正可追溯，append-only证据不删",
      "test_049_02_spec_conflict：规格/Runtime优先，硬规格仍由强制上下文提供",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "两个真实 Pi 会话维护同一项目 Hermes 存储，另有不属于本会话的锁/目录；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_049_03_concurrent_recovery：两个真实会话并发写同存储，持锁时杀掉一方再恢复",
      "必需子例 test_049_03_foreign_resources：在上述序列旁放他人活锁/同前缀目录",
      "必需子例 test_049_03_storage_full：真实维护存储注入空间不足再恢复"
    ],
    "pass_criteria": [
      "test_049_03_concurrent_recovery：已确认经验不丢不重，冲突明确等待/拒绝，恢复核对完整进程身份且完整性通过",
      "test_049_03_foreign_resources：活锁与旁观目录原样，清理仅本会话登记资源",
      "test_049_03_storage_full：旧经验完整、失败有诊断，可重试且无重复确认",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "Hermes 召回文本声称某 required AC 已PASS及某 high Issue 已修复，但当前候选没有对应证据；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_049_04_claim_candidate：从完整Candidate Gate正对照删观察后让真实Pi召回PASS/已修high并总结",
      "必需子例 test_049_04_contradict_failure：给同一候选真实检查FAIL并与召回冲突",
      "必需子例 test_049_04_no_recall：令Hermes无召回再构造上下文"
    ],
    "pass_criteria": [
      "test_049_04_claim_candidate：accepted/Issue/Candidate Gate均不因claim放行，缺证据未满足",
      "test_049_04_contradict_failure：FAIL继续阻断，召回来源与冲突可定位",
      "test_049_04_no_recall：规格/AC/候选强制项完整，不依赖未来Release Gate验证此AC",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "真实 Pi 非Commander会话的 @juicesharp/rpiv-todo 含3个局部todo，绑定run/task/attempt/native session；HoH任务尚未验收；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_050_01_reload_compact：真实todo三项中完成一项后reload/compact并恢复同会话",
      "必需子例 test_050_01_new_session：在新独立Pi会话查询同todo"
    ],
    "pass_criteria": [
      "test_050_01_reload_compact：三项身份/顺序/状态保持，只读映射指向原Task/session，未安装整个pipeline",
      "test_050_01_new_session：不自动继承旧会话，local completed不生成Task/requirement accepted",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "Pi局部todo全部完成，HoH对应AC有FAIL、Task已有正式领取记录，恢复清单有活动工作与next_action；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_050_02_local_global：真实todo全部完成但全局AC FAIL，读取TUI映射/状态栏",
      "必需子例 test_050_02_unmapped：令Task领取或关联映射缺失再展示"
    ],
    "pass_criteria": [
      "test_050_02_local_global：标明两套语义且accepted不涨、FAIL保留，领取身份/候选/证据/next_action来自Runtime",
      "test_050_02_unmapped：显示未关联/未知，不伪造全局进度",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "两个Pi会话相同todo文本、一个旧epoch会话和一个当前会话；当前会话在写todo时中断；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_050_03_session_epoch：两会话相同文本并回放旧epoch消息",
      "必需子例 test_050_03_interrupt_cancel：当前会话写todo中断后恢复，再取消全局Task并送迟到todo"
    ],
    "pass_criteria": [
      "test_050_03_session_epoch：局部id以session区分，旧输出仅历史不覆盖当前映射",
      "test_050_03_interrupt_cancel：无重复todo/虚假完成，取消后仅历史展示，不删全局Task/证据",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "隔离的空插件目录、本地fixture registry含有效/损坏包；固定Pi与三插件版本/完整性；RPC无custom UI；两个compact hook；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_051_01_valid_install：空目录从本地registry准备有效默认组合后doctor",
      "必需子例 test_051_01_rpc_no_ui：在RPC无custom UI情况下使用HoH配置入口",
      "必需子例 test_051_01_hook_collision：注入两个compact hook冲突",
      "必需子例 test_051_01_install_failure：注入安装失败后doctor并推进独立任务"
    ],
    "pass_criteria": [
      "test_051_01_valid_install：来源/版本/integrity/依赖通过且整套锁原子发布，无手工Skill安装",
      "test_051_01_rpc_no_ui：HoH表单/adapter可用且适配诊断明确，不要求Pi custom TUI",
      "test_051_01_hook_collision：不运行两套压缩，明确冲突/处置且保留上次有效组合",
      "test_051_01_install_failure：每插件有诊断/来源、保留旧组合，不依赖插件任务可继续",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "本地registry有篡改hash、缺依赖、不支持Pi版本、错误作者来源四组包，活动Run已固定旧组合；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_051_02_reject_hash：registry包hash篡改后准备新组合",
      "必需子例 test_051_02_reject_dependency：准备缺依赖包",
      "必需子例 test_051_02_reject_pi_version：准备不支持Pi版本包",
      "必需子例 test_051_02_reject_author：准备错误作者来源包",
      "必需子例 test_051_02_maintenance_lock：活动Run升级先拒绝/延后，再于无使用会话安全维护窗口重试"
    ],
    "pass_criteria": [
      "test_051_02_reject_hash：不执行坏包、不发布有效锁，doctor列期望/实际及动作",
      "test_051_02_reject_dependency：拒绝并保旧锁，不声称兼容",
      "test_051_02_reject_pi_version：拒绝并保旧锁，不声称兼容",
      "test_051_02_reject_author：拒绝并保旧锁，不执行未经验证包",
      "test_051_02_maintenance_lock：活动Run始终旧锁，窗口后原子发布新组合仅供新Run",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "已安装旧组合，升级operation_id已登记；另有其它会话同前缀目录及活进程；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_051_03_download_interrupt：在下载阶段分别中断/取消/kill后恢复同operation",
      "必需子例 test_051_03_verify_interrupt：在校验阶段分别中断/取消/kill后恢复",
      "必需子例 test_051_03_stage_interrupt：在暂存完成阶段分别中断/取消/kill后恢复",
      "必需子例 test_051_03_switch_interrupt：在原子切换前/后分别中断/取消/kill后恢复",
      "必需子例 test_051_03_ownership：上述每个阶段留他人同前缀目录/活进程"
    ],
    "pass_criteria": [
      "test_051_03_download_interrupt：完整旧组合可用，取消不发布，重试幂等",
      "test_051_03_verify_interrupt：无混合锁/未校验包，历史失败保留",
      "test_051_03_stage_interrupt：只见旧/完整新组合，未完成组合不发布",
      "test_051_03_switch_interrupt：每个切点仅旧或完整新组合，已完成切换可对账，不伪造取消前完成",
      "test_051_03_ownership：只清登记精确路径/启动身份进程，旁观资源不变",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "本地故障注入上游404/不兼容/安装失败，以及 Codex/Claude/Pi 三适配器各自的缺认证/429/503/provider内容过滤原始错误夹具；required AC和degradable AC各一组；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_051_04_degraded_valid：注入404/不兼容/安装失败并生成合法降级输入",
      "必需子例 test_051_04_degraded_missing：分别去掉diagnostic与upstream_ref",
      "必需子例 test_051_04_degraded_required：把REQ-051或其它required检查标DEGRADED",
      "必需子例 test_051_04_provider_classes：Codex/Claude/Pi分别注入auth/429/503/内容过滤原生错误"
    ],
    "pass_criteria": [
      "test_051_04_degraded_valid：仅CR-001插件项可降级，执行绑定/诊断ArtifactRef/真实上游引用完整，fixture不冒充live",
      "test_051_04_degraded_missing：各被拒绝，不满足降级",
      "test_051_04_degraded_required：拒绝，required不因降级满足",
      "test_051_04_provider_classes：各原因分开且原始错误保留，过滤为provider_content_filter、不当能力不足，不自动耗尽重试/假插件降级",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
      "诊断工件与hash",
      "降级结构校验",
      "错误分类及发布拒绝理由"
    ]
  },
  {
    "id": "TEST-051-05",
    "requirement_id": "REQ-051",
    "acceptance_id": "AC-051-05",
    "check_id": "tui-pty",
    "fixture": "HoH TUI 的设置→插件表单，隔离配置启用三插件；Commander 选择 Pi、QA 选择 Pi，RPC 无 custom UI；adapter schema 匹配/不匹配各一例；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_051_05_form_edit：真实PTY到设置→插件并键盘编辑三插件配置",
      "必需子例 test_051_05_invalid_value：PTY提交非法字段",
      "必需子例 test_051_05_save_reopen：合法保存后关/重开表单",
      "必需子例 test_051_05_schema_mismatch：PTY提交adapter schema不匹配配置",
      "必需子例 test_051_05_role_argv：从表单配置启动Commander=Pi和QA=Pi并doctor"
    ],
    "pass_criteria": [
      "test_051_05_form_edit：入口可达、来源/版本/字段可读，RPC无UI不妨碍HoH表单",
      "test_051_05_invalid_value：显示字段错误且旧配置字节不变",
      "test_051_05_save_reopen：经adapter校验及CAS原子写，重开一致、不写原生用户目录",
      "test_051_05_schema_mismatch：明确拒绝且保旧配置/诊断可见",
      "test_051_05_role_argv：Commander禁扩展且无-e，QA加载锁定三插件，doctor区分两角色",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "B上功能a已验证；C修复b却使a回归；同一候选同一TEST有FAIL后PASS，并有一组超时无结论后PASS的对照；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_055_01_regression_issue：在B通过a后于C修b导致a失败",
      "必需子例 test_055_01_fail_pass：同候选同TEST先FAIL后PASS并反序读取聚合",
      "必需子例 test_055_01_inconclusive_pass：同候选先无结论超时再完整PASS"
    ],
    "pass_criteria": [
      "test_055_01_regression_issue：按影响闭包运行并开含a/B/C引用的regression Issue，原始attempt/stdout/stderr均留存",
      "test_055_01_fail_pass：投影FLAKY、strict未满足，读取顺序不改变历史",
      "test_055_01_inconclusive_pass：超时不写验收行，投影PASS而非FLAKY，记录不删除",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "模块依赖图含传递依赖及未知影响文件；产品矩阵计划含 Ubuntu24.04/26.04 × Python3.12/3.14 四格；integration 有明确标为 fixture 的四格执行器，另锁定本机实际 Python3.12/3.14 两解释器；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_055_02_impact_closure：已知修改、未知文件和release分别求检查集",
      "必需子例 test_055_02_fixture_matrix：fixture执行器产生两OS×两Python四格，其中一格FAIL；再漏一格",
      "必需子例 test_055_02_python_312：实际锁定Python3.12执行路径/过长文件名夹具",
      "必需子例 test_055_02_python_314：实际锁定Python3.14执行相同边界夹具",
      "必需子例 test_055_02_missing_interpreter：移除一个必需解释器后运行"
    ],
    "pass_criteria": [
      "test_055_02_impact_closure：已知含传递相关AC/TEST，未知和release取full required suite",
      "test_055_02_fixture_matrix：聚合失败/未满足且格/版本独立，明确fixture不称双OS实跑",
      "test_055_02_python_312：实际版本/argv/结果齐全，断言正确且不以另一解释器结果代替",
      "test_055_02_python_314：实际版本/argv/结果齐全且断言通过",
      "test_055_02_missing_interpreter：明确NOT_RUN/BLOCKED或失败且不能全行PASS，不删矩阵格",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "同一冻结候选上并行的QA与Reviewer，各有独立证据根、worktree、进程身份；第三方同前缀目录内有哨兵；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_055_03_parallel_isolation：同候选QA/Reviewer并行缓存生成，取消一方清理",
      "必需子例 test_055_03_source_pollution：让验证者改候选内容",
      "必需子例 test_055_03_nested_evidence：内层带外层检查环境启动后运行失败用例",
      "必需子例 test_055_03_signal_cleanup：让父环境忽略信号再取消检查"
    ],
    "pass_criteria": [
      "test_055_03_parallel_isolation：输出均候选外登记根，tree/未跟踪清单不变，另一方/旁观哨兵不变",
      "test_055_03_source_pollution：前后核对失败、候选检查无效，不签PASS",
      "test_055_03_nested_evidence：内层剥离外层HOH变量，外层无串入test行",
      "test_055_03_signal_cleanup：检查恢复默认信号且响应取消，只结束本方pid+启动身份进程",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "同一缺陷family连续两轮出现但其它AC仍增长；互补Reviewer使用不同编号段，对同一缺陷给出相反结论；另一序列达到4轮与5轮；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_055_04_two_families：同family连续两轮出现且其它AC有进展，重启再推进",
      "必需子例 test_055_04_conflicting_reviews：互补reviewer不同编号段给相反结论，尝试多数解除",
      "必需子例 test_055_04_round_four_medium：第4轮无critical/high但有medium/low，修复后冻结新候选",
      "必需子例 test_055_04_round_five_high：第4轮仍high，设计修正后全新会话仅核这些项，第5轮仍high",
      "必需子例 test_055_04_series_mapping：对同series处置产生多个候选和verification round并重启"
    ],
    "pass_criteria": [
      "test_055_04_two_families：必须先有设计修正，series/family/轮号不因进展或重启清零",
      "test_055_04_conflicting_reviews：session+finding_id唯一，去重仍保留来源，只有复现证据可裁决",
      "test_055_04_round_four_medium：medium红绿由Runtime核对/Commander决定，low入包；新候选仍完整QA/独立Review/Candidate Gate，缺报告即阻断",
      "test_055_04_round_five_high：只允许一次最终复核，随后BLOCKED，无第6轮绕过",
      "test_055_04_series_mapping：关联可追溯、轮号不混用，修复diff核对不重开全量设计讨论，FAIL/未满足AC不因上限变PASS",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "候选C的轮次级与汇合级FLAKY各一组，已保留FAIL/PASS历史；原栅栏J已有joined事实；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_055_05_cannot_wash：在同C轮次/汇合FLAKY后分别同内容refreeze、重跑、隔离required测试",
      "必需子例 test_055_05_new_candidate_barrier：在已有joined J后修复出新提交C2并独立验证"
    ],
    "pass_criteria": [
      "test_055_05_cannot_wash：三种均不能满足，原FAIL/PASS历史与required测试保留",
      "test_055_05_new_candidate_barrier：C2绑定新证据，解除汇合flaky用新栅栏非J；未通过仍阻断，不删除旧历史",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "产品发布判定器有完整正对照输入，绑定candidate/tree/spec/environment/test_plan；另分别构造空AC/空检查、缺test、skip/mock、hash错、旧规格/候选、独立会话复用、缺原生记录、缺live agent、soak不足、遗留high；另有 Hermes 召回或模型 claim 声称已PASS/已关闭high、但删去相应观察的变体；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_056_01_valid_fixture：完整candidate/tree/spec/environment/test_plan和同版本全量观察进入Runtime发布判定器",
      "必需子例 test_056_01_empty_scope：分别置空AC、检查集合",
      "必需子例 test_056_01_missing_test：从正对照删除一个必需TEST或其命名子例",
      "必需子例 test_056_01_skip_mock：分别将必须观察换SKIPPED或mock",
      "必需子例 test_056_01_identity_hash：分别改hash、spec或candidate为旧身份",
      "必需子例 test_056_01_independence_native：分别复用开发/QA/Review会话或去掉原生记录",
      "必需子例 test_056_01_live_soak：分别缺一种required live agent、少soak时长或少live轮数",
      "必需子例 test_056_01_high_claim：分别留high、仅用模型或Hermes召回声称PASS/修复",
      "必需子例 test_056_01_interim_only：只有最后候选/里程碑或v0.1成功而无全量v1证据"
    ],
    "pass_criteria": [
      "test_056_01_valid_fixture：仅完整正对照可发布，fixture不是HoH v1完成声明",
      "test_056_01_empty_scope：各拒绝并列空集合/缺项",
      "test_056_01_missing_test：拒绝，保留缺失明细，不以同TEST剩余PASS洗白",
      "test_056_01_skip_mock：各不满足真实required义务",
      "test_056_01_identity_hash：逐一拒绝，旧证据仅历史",
      "test_056_01_independence_native：各拒绝，不只比HoH外层id",
      "test_056_01_live_soak：各拒绝，原72h/20轮及三Agent门槛不变",
      "test_056_01_high_claim：缺观察/未关闭Issue仍阻断，不改accepted或Issue事实",
      "test_056_01_interim_only：不能输出v1 COMPLETED，截图/exit0不替代证据",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "同Run的两个开发Task已在不同增量候选接受，最终C为ACCEPTED但仅覆盖最后增量；具备持久release_validation_id；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_056_02_accepted_scope：两个增量Task接受后对最终ACCEPTED C触发发布验证",
      "必需子例 test_056_02_restart_dedup：重复release_request并在QA/Review收取间及记录重放时重启",
      "必需子例 test_056_02_ordinary_fail：完整性有效的发布检查/报告FAIL",
      "必需子例 test_056_02_integrity_invalid：分别篡改证据hash、检查期间源码后收取",
      "必需子例 test_056_02_stale_cancel：分别换spec、换candidate及取消后送旧结果",
      "必需子例 test_056_02_reverify_repair：无结论后同C重验；FAIL/INVALID后尝试新id洗白，再修复新提交新候选"
    ],
    "pass_criteria": [
      "test_056_02_accepted_scope：覆盖profile全required，独立QA/Review，无verification.round_opened，源Task/C及增量验收投影不变",
      "test_056_02_restart_dedup：单活动义务，release_validation_id/record_id去重，原生身份和全部行可追溯",
      "test_056_02_ordinary_fail：登记有主Issue并阻断发布，C/源Task和增量acceptance_results不被发布行改写",
      "test_056_02_integrity_invalid：同record_evidence事务否决C为INVALID并写integrity_violation，Commander唤醒，不新增PASS/DEGRADED，源Task不被发布义务迁移",
      "test_056_02_stale_cancel：旧义务仅历史/取消不推进，Run不COMPLETED，记录保留",
      "test_056_02_reverify_repair：无结论可重验，失败/否决不能换id洗白；新候选须完整Candidate Gate和全量发布证据",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "最终候选的PASS记录分别缺v1所需codex/claude/pi或plugin列表；合法插件DEGRADED对照具有诊断与上游引用；REQ-051为required；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_056_03_live_profile：从完整v1 PASS记录分别删codex/claude/pi或Pi插件列表",
      "必需子例 test_056_03_valid_degraded：真实格式降级输入含诊断/上游，REQ-051完整PASS",
      "必需子例 test_056_03_invalid_degraded：分别required降级、DEGRADED检查引用PASS AC、删diagnostic/上游引用",
      "必需子例 test_056_03_interim_profile：同规则用v0.1 profile运行"
    ],
    "pass_criteria": [
      "test_056_03_live_profile：RowFact来自live-sessions，profile规则16拒不合要求PASS，不从claim补名单",
      "test_056_03_valid_degraded：按policy满足插件降级，结果COMPLETED_DEGRADED并列明细",
      "test_056_03_invalid_degraded：逐一拒绝，不过滤掉失败/否决行",
      "test_056_03_interim_profile：最多INTERIM_RELEASED，不是v1 COMPLETED",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "Run遗留OPEN/REOPENED critical/high，包括已取消源Task；物化state故意滞后；同fingerprint多Issue与超过50项确认集；当前候选比原Issue新；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_056_04_issue_snapshot：OPEN/REOPENED critical/high含取消Task、滞后物化state、同fingerprint多Issue及超过50项",
      "必需子例 test_056_04_confirm_absent：两方实际送达交集有至少一方RESOLVED且均无present",
      "必需子例 test_056_04_not_confirmed：分别未送达、缺观察、仅一方收到及同候选追加present",
      "必需子例 test_056_04_invalid_veto：确认过程中同候选追加INVALID"
    ],
    "pass_criteria": [
      "test_056_04_issue_snapshot：同一读快照按ADR-029状态函数生成确认集/open_issues，分批不漏项",
      "test_056_04_confirm_absent：同事务写absent并settle_all，验收失败Issue由新行求解",
      "test_056_04_not_confirmed：各继续阻断，不能用多数或物化旧state清除",
      "test_056_04_invalid_veto：否决阻断，出口只重验/返工/取消且取消不放行，Agent不能豁免",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "完整发布输入的最终产物与build声明hash相同，release_validation已完成；还有最后提交前后的kill点与重复release_request；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_056_05_atomic_release：分别在发布事务提交前/后kill并重复release_request",
      "必需子例 test_056_05_last_moment_hash：最后提交前改变产物hash",
      "必需子例 test_056_05_last_moment_issue：最后提交前追加high",
      "必需子例 test_056_05_last_moment_identity：最后提交前使候选/spec过期后重启"
    ],
    "pass_criteria": [
      "test_056_05_atomic_release：manifest/releases/run.result原子一致且幂等，无无manifest的COMPLETED",
      "test_056_05_last_moment_hash：复核失败，不发布，原记录保留",
      "test_056_05_last_moment_issue：最新Issue快照阻断，不用旧完成快照",
      "test_056_05_last_moment_identity：拒绝旧发布结论，不重造证据，最终状态仅Runtime判",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "Ubuntu24.04与26.04 x86_64的全新隔离VM，候选构建包；三个受控目标项目分别为CLI/Web/service，均有可运行的vA/vB与数据；浏览器驱动已安装；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_057_01_ubuntu_2404_cli：fresh 24.04 VM上候选HoH验证CLI目标vA安装/PTY操作→vB升级→vA回滚",
      "必需子例 test_057_01_ubuntu_2404_web：fresh 24.04 VM上验证目标Web同一安装/浏览器关键操作/升级/回滚序列",
      "必需子例 test_057_01_ubuntu_2404_service：fresh 24.04 VM上验证service同一安装/请求/升级/回滚序列",
      "必需子例 test_057_01_ubuntu_2604_cli：fresh 26.04 VM上验证CLI完整序列",
      "必需子例 test_057_01_ubuntu_2604_web：fresh 26.04 VM上验证Web完整序列",
      "必需子例 test_057_01_ubuntu_2604_service：fresh 26.04 VM上验证service完整序列"
    ],
    "pass_criteria": [
      "test_057_01_ubuntu_2404_cli：每步原始日志/版本hash、PTY输入/退出及回滚原数据断言通过",
      "test_057_01_ubuntu_2404_web：真实进程/DOM/网络/数据断言与hash齐全，非HoH自身Web验收",
      "test_057_01_ubuntu_2404_service：健康/数据/版本hash逐步验证，旧数据回滚后可用",
      "test_057_01_ubuntu_2604_cli：同24.04 CLI判据独立实跑，非换HOME/容器",
      "test_057_01_ubuntu_2604_web：同24.04 Web判据独立实跑，截图只辅证",
      "test_057_01_ubuntu_2604_service：同24.04 service判据独立实跑，缺VM/驱动不能PASS",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "已有M4完整TUI与运行中的后台host，受控工程目标的检查有PASS/FAIL/NOT_RUN与可点击证据；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_057_02_navigate_evidence：真实PTY从Task/Issue打开PASS/FAIL/NOT_RUN检查及原始证据",
      "必需子例 test_057_02_detach_attach：运行中脱离再附着",
      "必需子例 test_057_02_pause_cancel：分别暂停与取消并重附"
    ],
    "pass_criteria": [
      "test_057_02_navigate_evidence：结果文字明确、引用指向正确候选/日志，截图不掩盖FAIL",
      "test_057_02_detach_attach：host/已接受任务继续，结果一致无重复派发",
      "test_057_02_pause_cancel：只走对应控制命令，状态和证据可查询",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "与057-01相同两种fresh VM；安装包损坏、依赖缺失、端口占用、迁移失败、旧binary面对高版本DB各一例；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_057_03_ubuntu_2404_bad_package：24.04 VM注入损坏包后修复重试",
      "必需子例 test_057_03_ubuntu_2404_dependency：24.04 VM注入缺依赖后修复重试",
      "必需子例 test_057_03_ubuntu_2404_port：24.04 VM注入端口占用后解除重试",
      "必需子例 test_057_03_ubuntu_2404_migration：24.04 VM注入迁移失败后修复重试",
      "必需子例 test_057_03_ubuntu_2404_old_binary：24.04 VM让旧binary打开高schema，再用匹配旧备份回滚",
      "必需子例 test_057_03_ubuntu_2604_bad_package：26.04 VM独立执行损坏包及恢复",
      "必需子例 test_057_03_ubuntu_2604_dependency：26.04 VM独立执行缺依赖及恢复",
      "必需子例 test_057_03_ubuntu_2604_port：26.04 VM独立执行端口占用及恢复",
      "必需子例 test_057_03_ubuntu_2604_migration：26.04 VM独立执行迁移失败及恢复",
      "必需子例 test_057_03_ubuntu_2604_old_binary：26.04 VM独立执行高schema旧binary拒读及旧备份回滚"
    ],
    "pass_criteria": [
      "test_057_03_ubuntu_2404_bad_package：失败非零且诊断/动作明确，旧安装/备份保留，重试无重复资源",
      "test_057_03_ubuntu_2404_dependency：失败非零，旧安装保留，修复后同路径成功",
      "test_057_03_ubuntu_2404_port：不吞错误不宣称成功，原服务/备份保留，无重复资源",
      "test_057_03_ubuntu_2404_migration：保旧版本/备份，不宣称升级，受支持路径可恢复",
      "test_057_03_ubuntu_2404_old_binary：业务读写前拒绝且DB不变，回滚只读已校验旧备份",
      "test_057_03_ubuntu_2604_bad_package：非零诊断/旧安装与备份保护/重试无重复均断言",
      "test_057_03_ubuntu_2604_dependency：非零诊断、保旧数据、修复后同路径可重试",
      "test_057_03_ubuntu_2604_port：明确失败、原服务不误杀，解除后无重复系统资源",
      "test_057_03_ubuntu_2604_migration：旧安装/备份完整，无假升级成功，修复后可重试",
      "test_057_03_ubuntu_2604_old_binary：拒读不改库，只接受匹配旧备份，数据可用",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "目标项目计划声明PTY/浏览器/部署检查与必需观测；有只截图、零断言、缺browser动作、退出0但FAIL、未运行五组反例；各独立反例使用隔离夹具，保留正对照与原始记录。",
    "steps": [
      "必需子例 test_057_04_complete_flow：完整带必需观察的目标验证记录进入Runtime并读TUI摘要",
      "必需子例 test_057_04_screenshots_only：只提供截图",
      "必需子例 test_057_04_zero_assertions：提供零断言日志",
      "必需子例 test_057_04_missing_browser：去掉必需浏览器操作",
      "必需子例 test_057_04_exit_zero_fail：退出0但test行FAIL",
      "必需子例 test_057_04_not_run：所需检查未运行"
    ],
    "pass_criteria": [
      "test_057_04_complete_flow：合法引用可用，stdout/stderr/命令/环境/版本/逐测试齐全",
      "test_057_04_screenshots_only：不能有效PASS",
      "test_057_04_zero_assertions：不能有效PASS",
      "test_057_04_missing_browser：不能有效PASS",
      "test_057_04_exit_zero_fail：不能有效PASS，保留原FAIL",
      "test_057_04_not_run：明确NOT_RUN/BLOCKED，不伪造browser/live；HoH Web仍需M6/M7",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "真实SQLite WAL活跃写入，DB引用规格/证据/公开流对象；已确认记录集K，每笔写入带唯一序号；含不会再修改的对象与未登记临时文件；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_063_01_wal_snapshot：活跃SQLite WAL并发写唯一序号，backup后独立restore",
      "必需子例 test_063_01_object_closure：复制期有GC压力并核对DB引用规格/证据/公开流对象"
    ],
    "pass_criteria": [
      "test_063_01_wal_snapshot：快照水位H内已确认K完整，无半事务，outbox/effect/预算不丢，源库可用",
      "test_063_01_object_closure：pin保护引用，全部hash正确、integrity_check通过，未登记临时文件不成有效引用",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "上一受支持schema的真实旧binary造库与备份；当前binary含正式编号迁移；另有高于支持范围的DB；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_063_02_upgrade_roundtrip：实际旧binary造库备份→当前binary升级→重开",
      "必需子例 test_063_02_old_binary_refusal：旧binary尝试升级后的高schema库",
      "必需子例 test_063_02_rollback_backup：以旧备份和匹配binary回滚重开",
      "必需子例 test_063_02_migration_kill：在迁移各事务切点kill再重启"
    ],
    "pass_criteria": [
      "test_063_02_upgrade_roundtrip：integrity_check通过，user_version整笔成功才变且重复启动不重放",
      "test_063_02_old_binary_refusal：任何业务读写/派发前拒绝，库字节/版本不变",
      "test_063_02_rollback_backup：完整旧schema/数据可用，不用未声明down migration",
      "test_063_02_migration_kill：每切点只有完整旧/新schema，记录切点和真实恢复观测",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "backup/restore操作已登记，目标有既存库；分别令DB/对象hash损坏、对象缺失、备份目标满盘、只读及SQLITE_BUSY；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_063_03_bad_db_hash：损坏备份DBhash尝试恢复",
      "必需子例 test_063_03_bad_object_hash：损坏引用对象hash尝试恢复",
      "必需子例 test_063_03_missing_object：删除必需引用对象后恢复",
      "必需子例 test_063_03_full_readonly_busy：分别满盘、只读、SQLITE_BUSY生成备份/恢复",
      "必需子例 test_063_03_atomic_interrupt：分别在临时落盘/fsync/rename/完成登记处中断或取消并重启"
    ],
    "pass_criteria": [
      "test_063_03_bad_db_hash：拒绝且目标原库hash不变，无成功manifest",
      "test_063_03_bad_object_hash：拒绝且不替换原库",
      "test_063_03_missing_object：明确缺项且保原库",
      "test_063_03_full_readonly_busy：不标成功、不覆盖原库；BUSY有界且取消/读取可响应",
      "test_063_03_atomic_interrupt：只见旧库或全校验新库，按operation精确清理孤立资源，完成幂等",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "fixture": "活动host/Attempt存在，备份绑定project_id/run/spec/schema/资源版本；另有错误项目备份、旧schema但不支持的迁移链；同一子例内顺序/并发/重启复用其登记状态，禁止在中途重建项目或清历史；不同独立子例隔离。",
    "steps": [
      "必需子例 test_063_04_confirm_entry：经与TUI设置→维护相同动作模型选择恢复，查看确认页后不确认/取消确认",
      "必需子例 test_063_04_active_refusal：确认时host/Attempt仍活动",
      "必需子例 test_063_04_exclusive_restore：停派发且Attempt安全收尾、取独占锁后确认正确备份",
      "必需子例 test_063_04_wrong_project_chain：分别确认错误项目或不支持迁移链备份",
      "必需子例 test_063_04_progress_cancel：同operation维护页查询进度，重启再查询并请求安全点取消"
    ],
    "pass_criteria": [
      "test_063_04_confirm_entry：显示project/水位/schema/hash/替换目标，无恢复effect或数据替换",
      "test_063_04_active_refusal：明确拒绝替换/待静止条件，原库不变",
      "test_063_04_exclusive_restore：恢复后先reconcile/递增host_epoch/fence旧租约，不盲重投started效果",
      "test_063_04_wrong_project_chain：拒绝并保原库/原hash",
      "test_063_04_progress_cancel：身份/进度持久一致，取消结果可查、无假完成，不加Shell子命令",
      "覆盖断言核对上述必需子例名称全集与逐项观测；缺例/缺矩阵格写本TEST的FAIL行并使检查非零。未运行或合法降级按共同约定2/3记录，不伪造PASS。"
    ],
    "observables": [
      "必需子例清单、实际执行名称和覆盖断言",
      "候选/规格/环境/计划/生产者身份",
      "每子例原始命令、stdout/stderr、逐测试记录与断言观测",
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
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。交付DB一致性backup、对象引用闭包/GC pin、维护恢复、迁移兼容与原子发布；先审恢复身份/事务ADR。依赖CR-028对象生命周期，应用时重核最新schema。 复用/扩展M4-T03/T10的一致性备份基础；TUI设置→维护的确认、进度、取消接同一内部服务，AC-063-04以integration验证服务与动作模型。 所属场景以本CR必需子例清单生成覆盖断言；漏任一名字/矩阵格或只有聚合日志即输出该TEST的FAIL行并非零，证明删一个子例能变红，不修改release-policy的证据规则。对新增场景先测完整所属check耗时，遵守冻结timeout及既有余量，无法容纳则CTO重规划，绝不减AC。"
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
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。交付失败分支/恢复清单、用户改动保护及STEER受理/投递code_target绑定，接线input_queue、HCP、原生投递与恢复；承接CR-026 M3A-02；回归REQ-017/022/028/037/041。 明确W上parent=旧HEAD/tree=B的追加恢复与failed ref CAS；STEER区分来源与当前目标，验证同写者next_attempt后继、恢复generation拒绝、unborn首提交及只读职位豁免。 所属场景以本CR必需子例清单生成覆盖断言；漏任一名字/矩阵格或只有聚合日志即输出该TEST的FAIL行并非零，证明删一个子例能变红，不修改release-policy的证据规则。对新增场景先测完整所属check耗时，遵守冻结timeout及既有余量，无法容纳则CTO重规划，绝不减AC。"
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
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。承接CR-020：flaky处置要求新候选，解除汇合级flaky产生新栅栏编号（ADR-029 J9），保留全部历史；承接CR-026 M3A-01：两轮同类复发设计修正、四轮上限及一次最终复核、low清理包；互补审查编号/冲突按证据；候选只读与精确资源清理；矩阵与影响闭包。 review series与ADR-028验证轮次分别持久化，Runtime核对+只读Commander决定不替代每个修复候选的QA/独立Review/Candidate Gate。矩阵四格为显式fixture执行器测试聚合，另交付已锁定Python3.12/3.14多解释器路径夹具，记录版本/缺格非PASS；真实双OS矩阵由M5-T08交付。新增内容后先重测全integration成本，沿用CR-028 D4的3000s工作/600s余量及3600s总限，装不下交CTO重规划，不降门槛。 所属场景以本CR必需子例清单生成覆盖断言；漏任一名字/矩阵格或只有聚合日志即输出该TEST的FAIL行并非零，证明删一个子例能变红，不修改release-policy的证据规则。对新增场景先测完整所属check耗时，遵守冻结timeout及既有余量，无法容纳则CTO重规划，绝不减AC。"
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
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。交付plugin-live真实驱动，逐TEST输出与原生session/plugins绑定；未实现集成保持NOT_RUN/非零，不能恒PASS；此时驱动支持与REQ-051的required产品检查验收分开，不能声称三个插件已通过。真实计划argv只随可执行脚本同提交替换占位，不删mode/artifacts/timeout。运行中组合锁、原子安装、RPC设置与doctor、故障/恢复路径齐全。 AC-051-05改由tui-pty真实操作HoH设置表单，覆盖编辑/错误值/合法保存重开/schema不匹配及角色argv，复用M4-T08 CAS；共用provider_content_filter诊断覆盖Codex/Claude/Pi，原生错误保留。 所属场景以本CR必需子例清单生成覆盖断言；漏任一名字/矩阵格或只有聚合日志即输出该TEST的FAIL行并非零，证明删一个子例能变红，不修改release-policy的证据规则。对新增场景先测完整所属check耗时，遵守冻结timeout及既有余量，无法容纳则CTO重规划，绝不减AC。"
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
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。在真实Pi会话补齐plugin-live的Compact/Hermes及三组合hook场景；本任务可加载已锁定todo包测试组合hook，但不声称REQ-050完成；每项PASS或符合CR-001的DEGRADED都须实际证据，缺认证不当插件降级。 所属场景以本CR必需子例清单生成覆盖断言；漏任一名字/矩阵格或只有聚合日志即输出该TEST的FAIL行并非零，证明删一个子例能变红，不修改release-policy的证据规则。对新增场景先测完整所属check耗时，遵守冻结timeout及既有余量，无法容纳则CTO重规划，绝不减AC。"
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
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。交付rpiv-todo隔离/恢复/只读映射；全局任务领取、候选、证据、下一动作来自Runtime；真实三插件组合全部场景，不能安装整个pipeline。 所属场景以本CR必需子例清单生成覆盖断言；漏任一名字/矩阵格或只有聚合日志即输出该TEST的FAIL行并非零，证明删一个子例能变红，不修改release-policy的证据规则。对新增场景先测完整所属check耗时，遵守冻结timeout及既有余量，无法容纳则CTO重规划，绝不减AC。"
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
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。承接CR-018：Runtime触发最终候选全范围验证，不开增量轮次，可对ACCEPTED候选运行，普通发布结果不迁移候选/源Task状态（完整性INVALID仍即时否决候选），使多Task Run完成具备真实全量证据。承接CR-020：发布投影传profile，规则16仅核outcome PASS产生的PASS行，RowFact.live_agents来自live-sessions；全Run OPEN/REOPENED critical/high含取消Task，按ADR-029决定10送达/absent条目规则、同事务issues::settle_all；验收失败Issue按新行求解；待确认集与open_issues在导出读快照直接求状态函数；出口仅重验/返工/取消且阻断保持，豁免需所有者。落实共同约定7已定的生产者分类、独立发布行/投影、普通FAIL Issue与INVALID同事务否决，实施ADR列函数/迁移与回归并审查，不复用round_of_task伪造全量验证。 对record_evidence非轮次路径和报告收取加发布身份分支，覆盖两次重启/重复收取/篡改/取消/旧身份；全量正对照上测试记忆claim不能替观察。 所属场景以本CR必需子例清单生成覆盖断言；漏任一名字/矩阵格或只有聚合日志即输出该TEST的FAIL行并非零，证明删一个子例能变红，不修改release-policy的证据规则。对新增场景先测完整所属check耗时，遵守冻结timeout及既有余量，无法容纳则CTO重规划，绝不减AC。"
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
    "done_when": "指定AC在同一冻结候选的场景检查中有逐TEST原始证据；自测，非验收；独立审查无未解决critical/high；不以缺工具或fixture冒充live。按CTO Q1决定交付目标CLI/Web/service实际验证、双Ubuntu fresh VM安装升级回滚、M4 TUI证据导航与失败诊断；增强既有deployment/tui-pty检查而不转移owner。此任务包含verify前low清理包：逐项修复或按AC列出带证据延期，独立轻审；没有完成M6的HoH Web GUI不签其验收。 先实测含release构建、双OS三目标正例、逐OS故障矩阵、VM启动/销毁与既有检查的完整deployment总耗时并留600s余量；各阶段时限写入冻结environment，总计≤3000s工作+600s收尾且外层3600s不变，超预算先向CTO提交测量/重规划，不运行删例版也不加timeout。本任务拥有VM镜像准备、精确资源登记/清理和VM内浏览器驱动（供M6复用）；environment锁镜像hash/来源、虚拟化与浏览器/驱动/工具版本、vA/vB候选身份。 所属场景以本CR必需子例清单生成覆盖断言；漏任一名字/矩阵格或只有聚合日志即输出该TEST的FAIL行并非零，证明删一个子例能变红，不修改release-policy的证据规则。对新增场景先测完整所属check耗时，遵守冻结timeout及既有余量，无法容纳则CTO重规划，绝不减AC。"
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

| AC-051-01 | M5-T03 | integration | M0-T04；M2-T07/M2-T14/M2-T20；M3-T08；M4-T04 | Pi协议、配置/doctor/原生只读档、内置Skill、M4-T08设置CAS/表单能力与对象；本任务交付安装、HoH插件表单及live驱动 |

| AC-051-02 | M5-T03 | integration | M0-T04；M2-T07/M2-T14/M2-T20；M3-T08；M4-T04 | Pi协议、配置/doctor/原生只读档、内置Skill、M4-T08设置CAS/表单能力与对象；本任务交付安装、HoH插件表单及live驱动 |

| AC-051-03 | M5-T03 | recovery | M0-T04；M2-T07/M2-T14/M2-T20；M3-T08；M4-T04 | Pi协议、配置/doctor/原生只读档、内置Skill、M4-T08设置CAS/表单能力与对象；本任务交付安装、HoH插件表单及live驱动 |

| AC-051-04 | M5-T03 | integration | M0-T04；M2-T07/M2-T14/M2-T20；M3-T08；M4-T04 | Pi协议、配置/doctor/原生只读档、内置Skill、M4-T08设置CAS/表单能力与对象；本任务交付安装、HoH插件表单及live驱动 |

| AC-051-05 | M5-T03 | tui-pty | M0-T04；M2-T07/M2-T14/M2-T20；M3-T08；M4-T04 | Pi协议、配置/doctor/原生只读档、内置Skill、M4-T08设置CAS/表单能力与对象；本任务交付安装、HoH插件表单及live驱动 |

| AC-055-01 | M5-T02 | integration | M5-T06；M2-T16/M2-T18；M3-T02/M3-T10；M4-T04 | 安全恢复/资源归属、QA/Review与J9、影响闭包/监督、M4证据；T02自交付fixture矩阵及真实多解释器，双OS实跑由T08 |

| AC-055-02 | M5-T02 | integration | M5-T06；M2-T16/M2-T18；M3-T02/M3-T10；M4-T04 | 安全恢复/资源归属、QA/Review与J9、影响闭包/监督、M4证据；T02自交付fixture矩阵及真实多解释器，双OS实跑由T08 |

| AC-055-03 | M5-T02 | recovery | M5-T06；M2-T16/M2-T18；M3-T02/M3-T10；M4-T04 | 安全恢复/资源归属、QA/Review与J9、影响闭包/监督、M4证据；T02自交付fixture矩阵及真实多解释器，双OS实跑由T08 |

| AC-055-04 | M5-T02 | integration | M5-T06；M2-T16/M2-T18；M3-T02/M3-T10；M4-T04 | 安全恢复/资源归属、QA/Review与J9、影响闭包/监督、M4证据；T02自交付fixture矩阵及真实多解释器，双OS实跑由T08 |

| AC-055-05 | M5-T02 | integration | M5-T06；M2-T16/M2-T18；M3-T02/M3-T10；M4-T04 | 安全恢复/资源归属、QA/Review与J9、影响闭包/监督、M4证据；T02自交付fixture矩阵及真实多解释器，双OS实跑由T08 |

| AC-056-01 | M5-T07 | integration | M5-T02/M5-T10；M2-T13/M2-T18；M3-T02/M3-T07；M4-T04 | 回归/三插件与降级、CLI最终结果/Issue投影、规格与追踪、M4证据读取；本任务交付发布全量义务 |

| AC-056-02 | M5-T07 | integration | M5-T02/M5-T10；M2-T13/M2-T18；M3-T02/M3-T07；M4-T04 | 回归/三插件与降级、CLI最终结果/Issue投影、规格与追踪、M4证据读取；本任务交付发布全量义务 |

| AC-056-03 | M5-T07 | integration | M5-T02/M5-T10；M2-T13/M2-T18；M3-T02/M3-T07；M4-T04 | 回归/三插件与降级、CLI最终结果/Issue投影、规格与追踪、M4证据读取；本任务交付发布全量义务 |

| AC-056-04 | M5-T07 | integration | M5-T02/M5-T10；M2-T13/M2-T18；M3-T02/M3-T07；M4-T04 | 回归/三插件与降级、CLI最终结果/Issue投影、规格与追踪、M4证据读取；本任务交付发布全量义务 |

| AC-056-05 | M5-T07 | recovery | M5-T02/M5-T10；M2-T13/M2-T18；M3-T02/M3-T07；M4-T04 | 回归/三插件与降级、CLI最终结果/Issue投影、规格与追踪、M4证据读取；本任务交付发布全量义务 |

| AC-057-01 | M5-T08 | deployment | M5-T07/M5-T05；M2-T14；M4-T04 | 发布验证/备份、systemd安装与deployment、完整M4 PTY；目标Web驱动由本任务交付（Q1），无需M6 HoH Web |

| AC-057-02 | M5-T08 | tui-pty | M5-T07/M5-T05；M2-T14；M4-T04 | 发布验证/备份、systemd安装与deployment、完整M4 PTY；目标Web驱动由本任务交付（Q1），无需M6 HoH Web |

| AC-057-03 | M5-T08 | deployment | M5-T07/M5-T05；M2-T14；M4-T04 | 发布验证/备份、systemd安装与deployment、完整M4 PTY；目标Web驱动由本任务交付（Q1），无需M6 HoH Web |

| AC-057-04 | M5-T08 | integration | M5-T07/M5-T05；M2-T14；M4-T04 | 发布验证/备份、systemd安装与deployment、完整M4 PTY；目标Web驱动由本任务交付（Q1），无需M6 HoH Web |

| AC-063-01 | M5-T05 | recovery | M2-T01/M2-T05/M2-T14；M4-T04 | SQLite/WAL/事务、reconcile/锁/epoch、维护部署、M4引用对象/GC；本任务复用M4-T03/T10备份，补backup pin、维护确认与restore |

| AC-063-02 | M5-T05 | recovery | M2-T01/M2-T05/M2-T14；M4-T04 | SQLite/WAL/事务、reconcile/锁/epoch、维护部署、M4引用对象/GC；本任务复用M4-T03/T10备份，补backup pin、维护确认与restore |

| AC-063-03 | M5-T05 | recovery | M2-T01/M2-T05/M2-T14；M4-T04 | SQLite/WAL/事务、reconcile/锁/epoch、维护部署、M4引用对象/GC；本任务复用M4-T03/T10备份，补backup pin、维护确认与restore |

| AC-063-04 | M5-T05 | integration | M2-T01/M2-T05/M2-T14；M4-T04 | SQLite/WAL/事务、reconcile/锁/epoch、维护部署、M4引用对象/GC；本任务复用M4-T03/T10备份，补backup pin、维护确认与restore |


## CTO 决定与剩余问题

来源：[CTO 决策原件](/home/ai/dev/hoh/.hoh-dev/evidence/spec-drafts-cto-decisions.md)，标注日期 **2026-11-22**；本次原样留存到 `/home/ai/dev/hoh/.hoh-dev/evidence/m5-spec-d2/cto-decisions.md`。Q1–Q7 的推荐默认均为 **ADOPTED**，不是重新待决的提案；CTO采纳默认不等于本CR已经独立复审/正式批准。原文件同时记录所有者已批准 CR-031 C1，A5 与 REQ-057 已同步。

| ID | 2026-11-22 已采纳的默认 | 本稿落实与后续验证 |
|---|---|---|
| Q1 | M5验证目标项目真实浏览器流程，HoH自身Web留M6/M7 | 057-01/04、T08与A5；撤销原“阻塞应用”标签，v1 Web范围不减 |
| Q2 | host受理时固定代码快照，不加客户端expected code字段 | 044-04/05、共同约定5；来源不重绑，正常后继/unborn保留REQ-017，恢复/换线拒绝 |
| Q3 | 事件+ArtifactRef持久化series/family，Commander确认分类，Runtime计数 | 055-04；明确series与验证轮次映射、Runtime核对+Commander决定，修复候选仍完整Candidate Gate |
| Q4 | 当前锁定来源/版本为起点，实施核对schema/hook并实测 | 048/051；无法兼容以真实证据按CR-001降级，不虚称版本已经验证 |
| Q5 | 原生支持则原生，缺元数据由HoH引用工件补齐，Hermes负责长期召回 | 049；项目隔离/恢复无法满足时只降级真实集成，required安装诊断保留 |
| Q6 | 独立release_validation_id；批准前写清与INVALID的接线 | 共同约定7、056-02及T07已写明生产者分类、独立发布行、普通FAIL Issue、INVALID唯一状态迁移例外及出口；原“阻塞设计选择”关闭，修订正确性须复审 |
| Q7 | 现有交互/诊断入口到内部维护，独占恢复且显式确认；保护引用、按operation清理 | 063-04、T05；保留期数值随实际配置加载器交付，不猜值，不加Shell命令 |

本轮没有新增必须改变 CONTRACT 架构或缩减范围才能裁决的问题。**条件性的 CTO 重规划点**：T02/T07新增 integration 与 T08 的 deployment 先测全阶段成本（包含既有M4义务），保持冻结超时和既有余量；推荐默认是优化/复用或经独立审查增加 required 检查以分担工作并保持原AC全部满足，不延长原timeout、不去掉子例、不给未测项PASS。无法在这些边界内兑现时提交测量与阻塞问题，由CTO裁决；如需更改CONTRACT或降低门槛则必须升级所有者，本稿不批准这种变更。插件锁/新schema若与已采纳默认不兼容，同样先以证据说明差异再细化，不静默套用。

## 审查记录

第1轮：Claude Reviewer（解析模型 `claude-opus-5-5`），原生 session **`c648c29b-040f-47a5-895e-543094af09fd`**，attempt **`349314cc-8dfa-4912-bb5b-128f663e6387`**；来自原生 attempt 记录的 session_ids，不用桥 attempt_id冒充session。受审输入为draft1（commit `6c0fc5b67aeec72579f1cc2a5cf1070e9e17ee47`，源基线 `8d9ae599d30982fd431b950dec229786cdc15379`）；报告结论 **FAIL**（0 critical、2 high、8 medium、8 low）。启动记录 `/home/ai/dev/hoh/.hoh-dev/evidence/m5spec-review-claude-r1.out` 指向 `/home/ai/dev/hoh/.hoh-dev/attempts/349314cc-8dfa-4912-bb5b-128f663e6387/attempt.json` 及 `stdout.jsonl` 的最终 `hoh-report`；原记录、已核对stdout_sha256的原生会话、提取报告与CTO决定留存在主仓库 `/home/ai/dev/hoh/.hoh-dev/evidence/m5-spec-d2/`。NATIVE_TURN_COMPLETED仅代表报告交付，不能当设计PASS。

| finding | 原等级 | draft2处置与可复核位置 |
|---|---|---|
| CR30D-01 | high | 修复：共同约定7/056-02/T07定义record_evidence边界、独立发布投影与INVALID例外、篡改/FAIL/重启/取消子例；不再推给未来ADR决定 |
| CR30D-02 | high | 修复：共同约定5/044-04区分来源与正常后继，保留next_attempt，排除只读职位，恢复换线仍拒绝 |
| CR30D-03 | medium | 修复：共同约定4/055-04区分series与自动验证轮次，Runtime核对+只读Commander决定，每修复候选完整Candidate Gate |
| CR30D-04 | medium | 修复：049-04只测已具备的Candidate Gate/accepted/Issue；发布记忆claim负例移056-01正对照上 |
| CR30D-05 | medium | 修复：055-02/T02明确四格为fixture矩阵、真实多解释器设施自交付、双OS真实执行归057/T08 |
| CR30D-06 | medium | 修复：057/T08先测全阶段成本与600s余量、超预算CTO重规划；VM镜像与VM内驱动归T08并锁入environment，供M6复用 |
| CR30D-07 | medium | 修复：044-05独立unborn起点，git_commit:null/unborn:true与本任务首提交后继，外部首提交拒绝 |
| CR30D-08 | medium | 修复：044-01明确W上追加R的parent/tree、failed ref命名/CAS，用户目录不换分支、不改写历史 |
| CR30D-09 | medium | 修复：39场景全列必需命名子例与逐项pass_criteria；检查驱动缺例FAIL，连续/并发子例保留同库/候选/栅栏 |
| CR30D-10 | medium | 修复：051-05以tui-pty实际编辑/错误/保存重开HoH插件表单，required保留；闭包与T03同步 |
| CR30D-11 | low | 同文修复：Q1–Q7已采纳的日期/来源明确，撤销已关闭的pending/blocking语义，Q6接线补齐但不自批 |
| CR30D-12 | low | 同文修复：A2/闭包/T05更正M4-T08 CAS归属并复用T03/T10备份基础 |
| CR30D-13 | low | 同文修复：044-05 UNKNOWN_EFFECT属于Attempt/effect，输入仍DELIVERED |
| CR30D-14 | low | 同文修复：055-01明确无结论超时后PASS投影为PASS而非FLAKY |
| CR30D-15 | low | 同文修复：048-04单Compact无Hermes flush；含Hermes组合才有flush顺序/失败子例 |
| CR30D-16 | low | 修复：apply/probe参数化路径、记录git show命令；全部本轮证据在主仓库m5-spec-d2，scratch按确切路径登记后清理 |
| CR30D-17 | low | 同文修复：051-04/T03共用诊断覆盖三适配器，保留原错误，不限插件，不冒充live |
| CR30D-18 | low | 同文修复：063-04给出设置→维护、确认/取消确认、operation查询与安全点取消子例 |

本轮18项均已在稿件中处理，无剩余low延期；“修复”是作者处置，不替代独立复审结论。下一轮限定本修复diff、相关契约调用链和回归，不新增本任务范围外实现。本轮不启动任何live审查会话。

## 批准后应用清单与不变内容

| 文件/范围 | 拟改动与实施责任 |
|---|---|
| contracts/requirements.json | 只替换上述九对象，39条AC；旧九AC保留原范围/标识 |
| contracts/acceptance-scenarios.json | 只替换对应39个场景，check_id均在原计划 |
| contracts/development-tasks.json | 只替换M5四项为十项（1 spec、8 implement、1 verify）；CR-018/020承接文字完整进入T02/T07；M6/M7保持原依赖 |
| docs/02-functional-spec.md、docs/18-traceability.md | gen_spec_views生成，不手写另一套权威 |
| contracts/baseline-lock.json | 只有正式批准/应用或隔离机械实验才relock；真实工作树本次不变 |
| docs/24-dev-lessons-to-product.md | 正式应用时为吸收条目追加CR-030与AC引用，不删除原经验；本稿不编辑它 |
| 实现任务后续ADR/docs/06/07/14/15/用户指南 | 按已采纳Q决定同步已实现行为与执行过的命令；契约/SQL需要变化时先提交细化补丁与新编号迁移，不暗改旧migration |
| config/plugins.json与tests/check-plan.json | 本次应用片段均不改。M5-T03可执行driver交付时才替换plugin-live占位argv，保持mode/live、timeout/artifacts；版本锁变动须精确来源、完整性与兼容证据 |

不新增需求、不改CONTRACT、不改release_class/release_targets、policy、gate工具或任何required阈值。正式应用若涉及schema、SQL或枚举，要把源码映射列入影响闭包并运行完整`cargo test --workspace --locked`；本次不改这些文件，不把未运行的Rust/产品/live测试标PASS。

## 验证方式

本次为 **文档机械自测，非验收**。没有实现或执行39条拟议产品AC，没有运行live Agent、真实插件、双OS部署、浏览器、soak或本稿的新产品红绿；这些产品结果均为 **NOT_RUN**。不修改真实工作树的受锁契约/基线/检查计划/产品源码；不调用release_gate宣称v1完成。首轮独立设计审查是已有FAIL输入，本轮没有启动复审，CR保持DRAFT。

全部本轮证据保存在主仓库 **`/home/ai/dev/hoh/.hoh-dev/evidence/m5-spec-d2/`**（非可移除worktree的忽略目录），包括原生审查记录/原始会话、CTO决定、输入绑定、脚本、逐命令argv/退出码/耗时和原始stdout/stderr。原生审查stdout的SHA-256已与attempt记录一致；来源摘要见 `source-bindings.json`。`unchanged-source.json`记录真实工作树的CONTRACT、requirements、scenarios、tasks、policy、plan、protocol及baseline-lock与修订起点逐字节相同的核对。

本次实际以 `mktemp -d /home/ai/dev/m5spec-XXXXXX` 创建 **`/home/ai/dev/m5spec-01rvP8`**；`ownership.json`及`source-bindings.json`登记本任务与确切路径。`baseline/`与`repo/`均为输入commit `6c0fc5b67aeec72579f1cc2a5cf1070e9e17ee47` 的本地共享对象克隆，分别测试基线与M5提案应用；所有测试临时目录经TMPDIR放在该根的 `tmp/`。清理只对本任务登记的确切根进行，结果见 `cleanup.json`；证据保留在上述主仓库目录。运行用户uid=1000，显式加载本工作树 `.hoh-dev/env.sh`，Python 3.14.4、rustc 1.99.1、Git 2.53.0，版本命令输出见 `execution-environment.json`。

副本应用器仅替换9个REQ对象、39个场景与10个M5任务，按输入的一空格缩进写入；只在副本中将CR头改为APPROVED并注明“隔离副本机械验证占位；不构成正式批准”，以测试正式基线变更步骤。副本revision22→23、27个受锁文件；副本锁绝不复制回工作树。探针另将已批准但未应用的CR-028任务图覆盖到内存做闭包校验，**没有**把M4产品实现或全部M4契约应用到该副本。

复现脚本为 [m5-spec-d2/apply.py](/home/ai/dev/hoh/.hoh-dev/evidence/m5-spec-d2/apply.py)、[probe.py](/home/ai/dev/hoh/.hoh-dev/evidence/m5-spec-d2/probe.py)、[audit.py](/home/ai/dev/hoh/.hoh-dev/evidence/m5-spec-d2/audit.py)、[run-command.py](/home/ai/dev/hoh/.hoh-dev/evidence/m5-spec-d2/run-command.py)；apply/probe均显式接收副本路径，probe还接收M4原文路径，不写死临时目录名。apply要求上一级有本次注册的ownership文件并拒绝真实工作树；新复现须先注册自己的scratch根。原始M4文本由实际执行的 `git show prep/M4-T01-spec:docs/cr/CR-028-m4-detailed-spec.md` 保存为 `/home/ai/dev/hoh/.hoh-dev/evidence/m5-spec-d2/m4-cr028.md`；来源commit/hash见A1后绑定与source-bindings。下面表中的E表示`/home/ai/dev/hoh/.hoh-dev/evidence/m5-spec-d2`，S表示`/home/ai/dev/m5spec-01rvP8`；完整展开argv与cwd在各同名JSON记录中，日志在同名`.log`中。

| 位置 / 实际命令 | 退出码 | 结果与记录 |
|---|---:|---|
| S/baseline：`bash scripts/check.sh --bundle` | 0 | 570 tests，1526.701s，OK (skipped=2)；baseline-bundle.json/log |
| S/repo：`python3 E/apply.py S/repo` | 0 | 应用9REQ/39场景/10任务，仅副本；apply.json/log |
| S/repo：`python3 E/probe.py S/repo E/m4-cr028.md` | 0 | 39AC/8 implement、唯一owner、每任务≤2REQ/≤8AC、每AC闭包、M4图覆盖0错误；其它REQ/任务及policy/plan/protocol内容不变；probe.json/log |
| 工作树：`python3 E/audit.py /home/ai/dev/hoh-wt/m5-spec` | 0 | 39场景/162具名必需子例、steps/pass_criteria对应、39行闭包表、18项处置、原TEST检查归属保留；draft-audit.json/log |
| S/repo：`python3 tools/gen_spec_views.py` | 0 | 生成docs/02与docs/18；generate.json/log |
| S/repo：`python3 tools/baseline.py relock --cr docs/cr/CR-030-m5-detailed-spec.md` | 0 | 仅副本revision23；relock.json/log |
| S/repo：`python3 tools/validate_bundle.py` | 0 | BUNDLE_ONLY PASS：70REQ、52 detailed、244AC、71 tasks，product_accepted=false；validate.json/log |
| S/repo：`python3 tools/baseline.py relock --cr docs/cr/CR-030-m5-detailed-spec.md --amend` | 0 | 补齐文档叙述后、完整套件前仅副本重算；pre-suite-relock.json/log |
| S/repo：`bash scripts/check.sh --bundle` | 0 | 570 tests，1639.064s，OK (skipped=2)；applied-bundle.json/log |

两套测试的skip均为既有 `test_ledger_outline_requirements_not_ready`（M3 already detailed）与 `test_repository_packages_verified`（Pi packages not installed here）；未记为PASS，完整原文保留在日志及 `bundle-summaries.json`。本次未新增生产测试，不声称文档修订完成了生产TDD；未来实现任务仍须逐场景红→绿及独立验证。

11个JSON fence的规范化SHA-256为 **`08784ab62698bc584d534c57369998dbeb824935a836a9fde5d4b514a92e08c9`**；填入本节与源码接线/审查来源说明不改变这些片段。收尾时以最终文档同步副本，只重新计算该副本CR锁，核对fence摘要、probe/audit、validate_bundle、baseline verify及真实工作树的validate/原锁/diff；实际退出码、最终CR摘要与版本一致性记录集中在 `final-checks.json`，完整逐命令记录在 `commands.json`。本轮没有基于未应用M4/M6/M7能力宣称验收：正式应用仍须重核实际主线和CTO/独立复审结论，运行完整bundle与契约影响闭包的 `cargo test --workspace --locked`，再由各实现任务兑现产品AC。
