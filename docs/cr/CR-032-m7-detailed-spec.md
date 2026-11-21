# CR-032 · M7 跨天稳定性、性能、交付真实性与维护规格细化

ID：CR-032
状态：DRAFT
批准人：待 CTO 在独立设计审查后依 CR-003 D1 决定；本文不是批准记录
日期：2026-11-22（第 2 稿；按首轮设计审查与当日 CTO/所有者决定修订）
输入基线：`535b4469b78e08bb4c792dfa29fea252ae78d459`（第 1 稿；受锁基线仍为 `8d9ae599d30982fd431b950dec229786cdc15379` 的 revision 22），分支 `prep/M7-T01-spec`
适用任务：M7-T01 的规格准备；正式应用仍依赖 M6-T04，未领取或推进全局任务

本次只提交本文件。下文把 REQ-067、068、069、070 各细化为 4 条，共 16 条可判定 AC；保留原有四条 AC/TEST ID 与原范围，`required_release=true`、`release_class=required`、`release_targets=["v1"]`、P1 和 summary 不变。实际工作树的需求、场景、任务图、检查计划和基线锁均不改。提案仅在临时副本应用并机械验证，产品/性能/soak/升级结果全部 NOT_RUN，不能用包自测代替验收。

## 背景、已定事项与授权边界

M7-T02 原跨 REQ-068/069/070 三项，超过每实现任务两项上限；M7-T03 承担 soak，M7-T04 是全范围 verify。本稿保留这些 ID，新增 M7-T05（性能）与 M7-T06（维护）；每条 AC 只归一个实现任务。spec/verify 按原规则覆盖多需求，不受实现任务的两项/八条限制，verify 不写 acceptance，也不重复认领 check。

依据包括 CONTRACT（含 CR-001）、goal、CLAUDE、docs/02、06、07 §6–8、09、10、19、21、24，当前 requirements/scenarios/tasks/check-plan/release-policy，以及 ADR-004/009/010（独立验收、备份与真实生产验收）、ADR-019/020/021（恢复、退避与客户端生命周期）、ADR-023（安装版本）、ADR-027/028/029（候选、证据、验证汇合）、ADR-030（兼容与fallback）和 CR-029（原生会话独立）。这些来源是约束，当前代码/脚本只是现状证据。

所有者已定事项不再提问：Rust 模块化单体/native agents/单写者；Shell 面只有 hoh、hoh -p、attach、web、doctor；不加外层无限继续循环；只有用户批准产品规格（CR-026 O1），无人值守只采用已授权配置默认（O2）；未提交用户改动不覆盖（O3）；用户规格 Markdown/index 单一事实源（O4）；Skill 不装进原生用户目录（O5）；任何规格修订使全部旧验收失效（O6）。本项目 CTO 的细化 CR 预授权不等于产品可代用户批准规格。额度耗尽等重置、不自动fallback沿所有者 2026-11-16 决定；2026-11-21 审查轮数与low清理包沿 CLAUDE/docs/19，M5负责落入产品，M7不放宽门禁或擅自重定义轮数。

M4 CR-028 的已绑定性能/资源/输入/终端决定继续生效；150MiB/100ms/500ms、72h/20轮、critical/high为零、独立QA/Review、70需求/14检查、三插件降级的唯一例外全部保留。未发现必须改变 CONTRACT 才能细化的事项；如应用时出现此冲突，相关项 BLOCKED，由 CTO 提交正式范围/架构问题，不把假设变成授权。

## 前序规格假设台账与应用重核

批准不代表应用，实现任务标题也不代表能力已交付。下表记录**尚未应用的能力前提，M7-T01 应用时按最终 main 复核**；D1–D7 与 C1 的决定已采纳，不属于待回答问题；可通过修订本稿及其任务边界调整，不可在测试时放松阈值。引用的前序任务 ID 若改变，先按能力重新映射，再验证闭包。

| ID | 来源/当前状态 | 假设及影响 | 应用时的核对与不成立的处置 |
|---|---|---|---|
| A1 | CR-028，APPROVED、未应用，精确输入见下 | M4-T03/T10 提供 integration 五阶段/排他/私有manager/百万库与原始样本；M4-T07 完整终端矩阵；M4-T04 对这些任务收口。影响全部068与067观察 | 核对实际runner、环境字段/工件及各阶段预算；M7不得复制第二套测量器。不存在则前序补齐，M7性能BLOCKED |
| A2 | CR-028 §5.1、REQ-015矩阵 | 同一参考profile才可正式integration PASS；M7补本地SSD证明；M6准备24.04，M7-T04一次最终tui-pty收两平台/完整宽度交集 | 核对实际硬件/限制和平台manifest，rotational不能证明后端；证据或预算不足交D3/CTO，不改参考值来通过 |
| A3 | prep/M5-T01-spec 的 CR-030 DRAFT快照（精确绑定见下）；未提交/未批准/未应用 | M5-T05/REQ-063备份迁移，T06/REQ-044恢复与STEER，T02/REQ-055回归，T07/REQ-056发布门禁，T08/REQ-057目标项目fresh-VM验证；T03/T09/T10插件安装/集成或合规降级。影响067/069/070 | 当前草案的接口是open假设。应用时按最终CR重核逐AC、owner与产物格式；M5-T04须覆盖全部8个实现owner。M5目标项目Web验证不等于M6 HoH Web GUI；缺能力先阻塞相关任务 |
| A4 | CR-026 应用时的更正、docs/24 §2.3/2.7 | CR-030 AC-055-04/M5-T02负责两轮设计修正和审查轮数；AC-044-04/05/M5-T06固定code_target={git_commit,tree_sha256,candidate_id或null}；067恢复不得继续旧身份，069导出真实审查历史 | 逐项核对M5的AC或明确不吸收理由；M7不默认为已完成，不擅改REQ-017/021/044/055/056。未落实且影响当前验证时交CTO补前序CR |
| A5 | prep/M6-T01-spec 的 CR-031 DRAFT，第2稿工作树快照（精确绑定见下）；未批准/未应用 | M6-T02提供Web外壳/资产；M6-T05负责重连/重新授权/多客户端；M6-T06负责Git交付与合并提交身份；M6-T03负责Ubuntu 24.04/26.04全新VM、安装/文档和资产一致性；M6-T04依赖T02/T03/T05/T06收全 | 069-02与070-01..04共用deployment的3600s，按共同约定8先测全部成本、超预算交CTO重规划，不放宽timeout或删AC；现deployment.sh只强制test_005_06，新增测试的NO_TESTS责任见T02/T06。C1按所有者2026-11-22批准：静态外壳无需token、Host/Origin仍校验、每个/v1请求均需token，token不进服务日志或Referer；不再列为待批准。缺能力由M6补齐，按最终CR复核闭包 |
| A6 | M3 CR-026规格已应用；实现任务尚需完成 | M3-T02/03/07/09提供spec identity/资源版本/追踪/运行中输入；M3-T05完成是M4入口 | 依最终主分支核对实现与已批准字段；本文不把M3规格提交当可用于旧版本升级的产品release |
| A7 | CR-017/ADR-027现有移交 | M7-T02切三个检查的profile为v1；M7-T03交付soak，发布前全量运行 | 检查已应用前序计划的argv/产物；保留其他新增产物，不用本基线的整份check-plan覆盖前序变更；确认D7的实施诊断与最终验收顺序 |

M4来源：`prep/M4-T01-spec` 的提交 `dde5109f43a2081b5d5e5c507b91a14b12c62143`，`docs/cr/CR-028-m4-detailed-spec.md` 字节 sha256=`e442f111eb14227ba2fc5496dafe812e8eca1ee070b3ee7208f9c3ca560e1800`。M5开始读取时没有正文；后续已读取CR-030第1稿工作树文件，分支HEAD仍为8d9ae599d30982fd431b950dec229786cdc15379，文件未提交，sha256=035a55e8a0b55437c830e32acc34771628de8119c883654c394923105be4657c（124087字节），保存的只读快照作为本稿输入，不宣称它是最终稿或已批准。复核到的AC/owner见A3/A4及下方承接表；并行作者后续变更不自动进入本稿。所有假设的责任人为 CTO，置信度为“已有范围约束，具体接口待核”；没有把未应用前序规格当成当前行为。



CR-031输入来自 `/home/ai/dev/hoh-wt/m6-spec/docs/cr/CR-031-m6-detailed-spec.md`，分支 `prep/M6-T01-spec`，读取时HEAD为 `05b2ef6cbc00354142bc35b330a6a371b1d1fc27`，该文件有未提交修订；固定快照sha256=`008349e656768c6e7a12ade4f4afdc858f9c7ca633e48cfd5d354a2603d677d1`，保存在 `/home/ai/dev/hoh/.hoh-dev/evidence/m7-spec-d2/CR-031-input.md`。只绑定此快照，不把并行作者后续编辑或DRAFT当已应用实现。CR-031 C12原文仍含“CTO决定是否调整timeout”；本次委派明确禁止放宽检查时限，因此M7按更严格的3600s总预算、先测后重规划执行，不采用该放宽选项。M4/M5输入快照也已保存在同一主仓库证据目录，避免worktree删除使引用失效。

### CR-030 DRAFT 的具体承接（不提前批准）

| M5提案 | M7消费与未定边界 |
|---|---|
| AC-044-02/03：operation恢复、活动工作/唤醒/next_action；AC-044-04/05：code_target和UNKNOWN_EFFECT | AC-067-03/04按真实状态恢复；AC-069-03保留旧身份历史；不从恢复摘要认定完成，不重绑旧STEER |
| AC-051-02/03：活动组合锁与原子更新；051-04：failure_class=provider_content_filter | AC-070-02沿旧插件锁；067-03消费相同诊断，不新增枚举或自动耗尽过滤重试；M5范围若只覆盖插件驱动，M7扩展观察覆盖原生工程Attempt按已采纳D5由M7-T03落实原生Attempt诊断接线并核对能力 |
| AC-055-02/03/04/05：影响闭包、候选外缓存/证据隔离、审查系列、Flaky新候选 | 067负例与069审计保留首失败；采用现有系列/栅栏，不以新提交重置审查计数；所有check子环境按M5隔离 |
| AC-056-01..05/M5-T07：fixture判定器与Runtime最终全范围验证分离、原子manifest/run.result | 069测试可证明判定器且实际未齐时报告未完成；真正70需求/14检查由M7-T04收取。它提供发布后置义务，不能自动证明D7的前序traceability已通过 |
| AC-057-01/03/04、063-01..04：fresh VM目标项目、备份/迁移/拒读/维护边界 | 070复用相同一致性备份与恢复规则，HoH自身安装包/两平台由M6补齐；不固化未来migration编号或旧产品commit |

## docs/24 经验吸收

| 条目 | M7处理 | AC/归属 |
|---|---|---|
| §2.4 provider内容过滤截断 | 显式诊断原始原因，不当能力不足、不假成功、不改变额度等待决定；诊断工件字段不扩协议枚举；D5已由CTO于2026-11-22采纳 | AC-067-03；复用CR-030 AC-051-04/T03的failure_class，重核A3；不再另造原因名或双重写者 |
| §5 打包猜路径混入旧二进制 | 构建工具输出路径+源码/输入/产物哈希链，真实build包正例、旧binary与资源错配负例 | AC-069-02；沿现有release-exe.sh/bundle.sh，不回退猜路径 |
| §1.2/§2.6 恢复与资源归属（跨阶段复核） | 心跳/恢复记录有在途工作与下一步；故障与清理只按登记身份，备份保全 | AC-067-03/04、070-03/04；产品恢复点结构由M5 REQ-044提供 |
| §2.3/§2.7 CR-026移交的部分覆盖 | 不重新分配需求；显式前提A4，M7轮次审计读取前序真实版本/审查历史 | AC-067-02、069-01/03/04；本次DRAFT映射为055-04/T02、044-04/05/T06，正式应用仍需逐项重核 |
| §3.3/§4 记忆与压缩 | 不用摘要作验收事实；插件能力由M5实现/降级，M7只验证事实链 | AC-067-02、069-01；不新增第二全局账本 |
| §5 低空间、信号/嵌套证据与规格影响闭包 | 沿M4/M5容量与隔离；soak子检查去掉外层HOH_CHECK_*证据身份后再建立自身身份，外层仅收引用；应用规格时对受影响代码/测试跑完整Rust回归 | AC-067-03/04、069-01/03；正式应用清单点名cargo全套 |

其余明确分配M3/M4/M5/M6的条目不在本CR重写AC：权限/角色分离由既有Runtime，界面由M4/Web由M6，备份/Flaky/插件由M5；上表列出M7会重新观察的交界。正式应用本CR后在docs/24相关条目追加“CR-032”，本次只交CR不改经验库。

## 共同测量、证据与实现约定（D1–D7 已采纳，来源见决策节）

1. **证据层次。** 下述check_id均已存在：soak=live；integration/deployment/traceability=product。模式由冻结计划决定，fault-injection/mock是子场景的明确标签，不篡改execution.mode。`test-result.status`只有PASS/FAIL/DEGRADED/SKIPPED；NOT_RUN/BLOCKED属于汇总/原因，不写非法枚举。测试名含相应test_NNN_MM，逐TEST输出和原始工件不共用一条空泛总结。
2. **soak持续性（D1/D2）。** 观察者每60s持久记录一次，正常间隔≤120s。故障前先登记kind/目标身份/触发条件/预期恢复/最大窗口，从kill触发到reconcile完成与host-ready各≤30s；外层受控host故障观察窗口≤120s，必须包含这两项30s证明且观察者持续记录，120s不是恢复时限的替代；缺口有注入开始/结束及外部观察佐证才可视为受控故障。未登记缺口、观察者丢失、无法证明的时间均不能补心跳；取消、未结束执行或候选漂移不能延长旧窗口。没有为了达到时间而暂停deadline/预算。预定kill/recovery是已标记受控故障，任何实际未处理崩溃或重复效果FAIL。20轮必须分布在整个窗口：每个完全位于有效执行窗口内的连续24h区间均至少一个live iteration.accepted（按半开区间[t,t+86400s)及原始时间戳判定），不足不靠重复记录补数。以总数≥20但开头集中完成后空转的审计副本作为分布负例，仍须拒绝；这是已采纳D1，不改变最低20。
3. **soak故障记录。** 真实网络临时中断、host SIGKILL、重投与客户端断开在隔离项目执行。503/reset/filter等确定性分类边界可用有已验证版本/来源实录摘要、明确fault-injection的原生协议夹具；缺来源格式即相关子场景BLOCKED，不猜字段。不能声称真的向provider制造过滤/429；真实provider出现的失败按实际记录另列。三Agent live工程活动须独立有原生证据。原始时间线按既有soak-event schema，扩展数据放details，不假造新的协议字段。观察者/驱动均有deadline，没有另建产品全局编排循环。
4. **性能（D3）。** 延续M4的release、串行、唯一integration、真实service/scope/Attempt cgroup与guardian；保留debug功能测试及全五阶段成本。150MiB是host+TUI之和；input/commit到真实屏幕计时，冷样本不删，p95第ceil(0.95N)项。终端每组≥1000样本，原值不可因CPU包络、SSD未证实或预算不足降低。正式参考profile仍是M4批准的profile，不把24.04功能回归冒称参考性能。SSD证明记录宿主/虚拟化磁盘后端配置与设备映射，无法取得则BLOCKED；若必须换参考硬件，另修订并审查，不在运行中改清单。
5. **长期维护（D4）。** 复用M5恢复/备份、M6安装与版本manifest；固定回归集覆盖Greenfield/Brownfield、Plan/Build/Auto、拒绝与恢复、候选/QA/Review、TUI/Web和插件诊断。每个Adapter/Prompt/调度策略变更的旧新对照使用同一固定集；产品路径可用明确fixture，原生协议兼容和最终发布所需live检查仍真实执行。具体旧产品版本必须取已交付、可构建/安装的commit与包，不把本草案或M3规格提交充作旧产品。已有M4 pre-003生产者不自动成为全部升级路径的旧版本。
6. **计划与工件。** M7-T01不提前替换soak占位命令。M7-T03在驱动交付同一提交替换该argv，保留live/345600秒与三件既有产物。测量、维护、build清单等额外原始文件须由对应检查的已声明工件携带或在驱动交付时追加声明；不删除既有声明，不把仅落盘但未哈希的文件当门禁证据。M7-T02切换profile不改任何policy/阈值。前序计划可能已扩展，应用时只做最小字段合并。
7. **阶段与最终验收（D7）。** traceability在v1配置下会要求包括soak在内的全范围结果；不能把M7-T02“先拿全v1 traceability PASS”作为M7-T03可开工的条件，形成环。T02先交付审计实现/绑定的逐测试诊断与独立范围审查，未满足的全量检查如实保留；AC仍未验收。任务板的实现交付与产品AC accepted不同。T03驱动和真实soak完成后，T04在最终同一候选重新收全部检查、traceability及独立报告。CTO于2026-11-22已采纳此自举实施路线：AC保持未验收，只由M7-T04收同一最终候选全量证据；不伪造PASS、不改Runtime门禁，也不把任务板实现交付状态写成Runtime的accepted。将来迁入正式Runtime若仍受deps accepted约束，须另有经批准的合法批次设计；本稿未授权修改eligible规则，也不以该未来条件重新阻塞当前已选路线。实现任务之间仅依赖已提供的能力，不依赖未来最终发布结果。

8. **共享deployment预算与测试发现（D4/D6，CR32D-04）。** 唯一deployment检查总时限仍为3600s，计入原TEST-005-06、CR-031的两平台fresh-VM/文档/资源场景、TEST-069-02以及TEST-070-01..04的三类变更对照、正常/失败/取消/恢复、构建/启动/收证/清理；不能按任务各自领取3600s。M7-T06在编码完整矩阵前先记录M6现有各阶段实测成本及M7代表路径增量，再冻结完整矩阵的分段预算并实测全量；M7-T02加入069-02时再测合并成本。测量不是验收，不足/超时如实FAIL或未执行，保留首失败和耗时；装不下则在冻结正式验收候选前交CTO重规划驱动/准备步骤或前序能力，禁止放宽timeout、删除/跳过AC、把两次检查拼成一次PASS。NO_TESTS白名单只追加：T06要求test_070_01、test_070_02、test_070_03、test_070_04，T02要求test_069_02；保留test_005_06与M6加入的全部测试。没有测试通过或缺任一必需TEST的实际通过记录时以3/NO_TESTS非成功退出；已有失败仍FAIL，不能由存在函数名、空结果或ignore满足。更改驱动与对应测试在实现任务同一提交，当前草案不改脚本。

## 各需求与场景的替换片段

应用时按id替换四个需求完整对象；按requirement_id替换它们的场景（不是删除其他需求场景）。每个JSON片段均可机械解析。旧AC-NNN-01/TEST-NNN-01的ID保留；原整项由该需求的全部AC共同覆盖，不能只凭AC-01签收整项。原summary逐句承接如下，新增条目细化错误、边界与恢复，范围不减。

| 原需求条款 | 覆盖AC |
|---|---|
| REQ-067 无未处理崩溃/重复写；服务暂不可用可恢复；≥20个真实工程轮次 | 067-01/03；067-03/04；067-01/02 |
| REQ-068 idle core RSS≤150MiB；TUI输入p95≤100ms；事件可见p95≤500ms | 068-01/02；068-01/02；068-01/02，03/04核对准入/统计真实性 |
| REQ-069 区分真实/模拟/跳过；未通过绝不称生产完成 | 069-01；069-01/04，02/03核对产物与候选身份 |
| REQ-070 锁版本与固定回归；每次变更跑对照；活跃Run不自动升级 | 070-01/02；070-01；070-02，03/04覆盖升级/恢复/拒绝 |

### REQ-067 · 跨天稳定性（4 条 AC）

<!-- fragment:requirement:REQ-067 -->

```json
{
  "id": "REQ-067",
  "category": "NFR",
  "priority": "P1",
  "milestone": "M7",
  "title": "跨天稳定性",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "无未处理崩溃/重复写；服务暂不可用可恢复；至少20个有证据的真实工程轮次",
  "acceptance": [
    {
      "id": "AC-067-01",
      "given": "同一冻结候选、真实 host、三种已认证原生 Agent 和已批准的工程夹具规格，soak 检查有连续观察者",
      "when": "经冻结计划运行一次不少于 72h 的 soak 并在结束后核对全部记录",
      "then": "min(墙钟跨度,duration_seconds)≥259200 秒，心跳满足共同约定；至少 20 个唯一且可追溯的 live iteration.accepted，且每个完全位于有效执行窗口内的连续24h半开区间[t,t+86400s)至少一个live iteration.accepted；没有未处理崩溃、重复写或未关闭 critical/high；每条时间线行绑定本 HoH 候选且有序并位于执行窗口，任一条件缺失不 PASS",
      "test_id": "TEST-067-01"
    },
    {
      "id": "AC-067-02",
      "given": "同一次 soak 中实际产生的成功、失败、取消轮次及不计数的 fault-injection 记录",
      "when": "从 Runtime 已持久的汇合事实导出轮次清单，再对审计输入的副本分别加入重复轮次、其他候选行、无独立报告行、mock 行和人工补写成功行",
      "then": "原始有效 live 轮次一一关联工程变更/测试/候选/QA/Review，覆盖新建CLI、已有Web增量、跨模块Bug修复三类固定工程场景与至少3个独立seed/初始目录，并逐轮记录功能正确性、回归捕获、错误验收、总资源和人工干预（无发生项有明确零值/不适用依据，资源未知标unknown，不猜成本），单Agent同预算对照单列不计HoH轮次，三种原生 Agent 的真实身份可核验；五种伪计数变体均不能增加有效轮次或满足门槛；审计副本不污染正式时间线，原始失败仍可查",
      "test_id": "TEST-067-02"
    },
    {
      "id": "AC-067-03",
      "given": "登记了精确资源身份的真实 host/隔离工程仓库、已开始的写 Attempt，以及独立 fault-injection 子场景",
      "when": "依次执行 host SIGKILL 后重启、同一效果消息重投、断网后恢复和客户端重连；再通过标注的协议夹具观察 503/Retry-After、额度 reset 与内容过滤截断",
      "then": "host 恢复沿既有 reconcile，从kill触发到reconcile完成与host-ready各≤30s；持续外部观察的受控故障窗口≤120s且包含这两项30s证明，120s不替代恢复时限；原效果至多提交一次、旧 writer 被 fence；不能确定的效果保持 UNKNOWN_EFFECT 并阻断重复执行；客户端断开不取消已接受任务；503 与额度不早于持久重试时刻派发，重启不清预算，额度不切 fallback；过滤原文与 failure_class=provider_content_filter 诊断可查且不记能力不足/成功，诊断子场景不贡献 live 轮次",
      "test_id": "TEST-067-03"
    },
    {
      "id": "AC-067-04",
      "given": "单独的短时负例执行与最终 72h 正例分目录，均有冻结身份和已写原始记录",
      "when": "负例分别取消、超时、结束观察者、变更 HoH 源码/计划的隔离副本；尝试把其记录接到另一个执行窗口或候选",
      "then": "每个负例非成功结束并保留已产生的输出、失败与清理记录，未执行部分不写 PASS；观察者缺口超 120 秒或候选变化使该窗口不满足；拼接被拒；新候选只能重新从零计时，正常结束清理只涉及登记资源",
      "test_id": "TEST-067-04"
    }
  ],
  "behavior": [
    "同一冻结 HoH 候选在真实 host 上持续运行至少 259200 秒；时长取执行记录 duration_seconds 与墙钟结束减开始的较小值，另以连续持久心跳交叉核验。禁止改时钟、拼接不同执行窗口、短时 sleep 后填大时长。所有 timeline 行绑定 harness_candidate_id，不与被开发夹具项目的 candidate_id 混用。",
    "至少 20 个互不重复的真实工程轮次通过候选 QA/Review 汇合；工程夹具覆盖 docs/09 的新建 CLI、已有 Web 增量、跨模块 Bug 修复，至少 3 个独立 seed/初始目录，逐轮记录功能正确性、回归捕获、错误验收、总资源和人工干预（未知资源明确unknown），并记录同预算单 Agent 多轮对照（对照不计 HoH accepted 轮次、不宣称统计普遍最优）；iteration.accepted 来自 Runtime 持久事实，能追到 run/iteration/task/attempt、原生会话、夹具候选、逐测试记录及独立报告。启动一个 Agent、提交一次文件、mock 轮次、失败/取消轮次都不计 accepted。开发、QA、Reviewer 的独立身份沿 CR-029，三种原生 Agent 的真实使用有原始记录。",
    "外部观察者持续记录 host 的 boot_id/PID/start_time、UTC 和单调时长、最后事件 seq、在途 Attempt、心跳和资源归属；按本 CR 共同约定每 60 秒采样，缺口超过 120 秒无法解释为已登记故障时不满足。预先登记的故障注入单列 fault-injection，其恢复不能掩盖未处理崩溃、重复副作用或证据丢失。 每个连续24h区间的分布条件按AC-067-01独立核对，轮次总数通过不能覆盖分布失败。",
    "用真实 host/持久层执行 host SIGKILL、重复投递、客户端断开/重连、临时服务不可达的恢复；旧 writer fencing、effect ledger 对账及 UNKNOWN_EFFECT 阻塞沿 ADR-019/020/021。只能清理本次登记的路径/unit/进程身份；取消与超时保留部分原始记录，不能把失败历史删掉后续跑成 PASS。 kill到reconcile完成、host-ready均≤30s，外层受控故障观察≤120s且须包含该证明。",
    "provider 内容过滤截断单列诊断 failure_class=provider_content_filter（外部诊断工件的原因，不在本 CR 增加协议枚举）；有明确原生证据才分类，不从任意错误猜测。它不表示能力不足，不计成功，也不触发自动降级模型。过滤不自动耗尽重试，交既有决策路径；其它故障的 Retry-After/退避/预算/重试上限保持；额度耗尽按已观测重置时刻等待、不切 fallback。内容过滤在 CTO 决策后才调整后续任务，未决状态不伪装为进展。",
    "soak 驱动是有截止时间的一次受控验收运行，可调度已授权固定工程任务并收取 Runtime 事件，不成为产品外层无限 continue/全局编排器；未知成本、认证或工具条件按既有规则阻塞。源、规格、环境描述或计划变更必须新候选并重新完整 72h，不能借旧窗口通过。"
  ],
  "failure_modes": [
    "时长不足、缺原始心跳、时间倒置、跨窗口/候选拼接、重复 iteration、只 mock 或少于 20 个有效 live 轮次、任一完整连续24h区间无有效轮次：soak 不满足。",
    "故障后重复写、旧租约继续生效、未处理崩溃、UNKNOWN_EFFECT 被当成功或资源归属无法证明：FAIL，保留首失败。",
    "缺认证/额度/工具或监控中断：明确 BLOCKED/NOT_RUN 诊断，逐测试用 SKIPPED；取消/超时不补写 PASS，不删除已经产生的 FAIL。"
  ],
  "decisions": [
    "保持 docs/10、release-policy 的 72h/20 次、live 模式和 ADR-027 原则一规则 18；96h 检查超时不扩大，恢复必须在同一有效窗口内。",
    "心跳、轮次分布与故障观察/恢复窗口按CTO于2026-11-22已采纳D1/D2，provider原因与处置边界按同日已采纳D5。来源见决策节；所有fault-injection记录显式标注，不贡献live accepted数。"
  ],
  "doc_refs": [
    "docs/09-testing.md",
    "docs/10-acceptance.md",
    "docs/14-resilience.md",
    "docs/adr/ADR-019.md",
    "docs/adr/ADR-020.md",
    "docs/adr/ADR-027.md"
  ]
}
```

对应场景（各场景唯一check_id见JSON）：

<!-- fragment:scenarios:REQ-067 -->

```json
[
  {
    "id": "TEST-067-01",
    "requirement_id": "REQ-067",
    "acceptance_id": "AC-067-01",
    "check_id": "soak",
    "fixture": "同一冻结候选、真实 host、三种已认证原生 Agent 和已批准的工程夹具规格，soak 检查有连续观察者",
    "steps": [
      "test_067_01：记录真实起点/候选/环境/会话并启动有界观察者",
      "持续采样并收取真实工程轮次，完成预登记故障表",
      "核对起止、每行身份、唯一轮次及完整性后产生 TEST-067-01 结果",
      "对审计副本构造总数≥20但集中在开头的轮次，断言24h分布失败；不把它计为正式live窗口"
    ],
    "pass_criteria": [
      "min(墙钟跨度,duration_seconds)≥259200 秒，心跳满足共同约定；至少 20 个唯一且可追溯的 live iteration.accepted，且每个完全位于有效执行窗口内的连续24h半开区间[t,t+86400s)至少一个live iteration.accepted；没有未处理崩溃、重复写或未关闭 critical/high；每条时间线行绑定本 HoH 候选且有序并位于执行窗口，任一条件缺失不 PASS"
    ],
    "observables": [
      "execution.json 与 timeline.jsonl",
      "持久心跳、UTC/单调时钟原始样本",
      "live-sessions.json、原始 Agent 输出",
      "原始 issue/effect/lease 事件及逐测试结果"
    ]
  },
  {
    "id": "TEST-067-02",
    "requirement_id": "REQ-067",
    "acceptance_id": "AC-067-02",
    "check_id": "soak",
    "fixture": "同一次 soak 中实际产生的成功、失败、取消轮次及不计数的 fault-injection 记录",
    "steps": [
      "test_067_02：按持久事件提取成功轮次并与原生记录逐项交叉核对",
      "在隔离副本构造五种错误记录并调用同一生产审计逻辑",
      "分别保存真实轮次与负例审计输出，不把负例当 live 工程轮次"
    ],
    "pass_criteria": [
      "原始有效 live 轮次一一关联工程变更/测试/候选/QA/Review，覆盖新建CLI、已有Web增量、跨模块Bug修复三类固定工程场景与至少3个独立seed/初始目录，并逐轮记录功能正确性、回归捕获、错误验收、总资源和人工干预（无发生项有明确零值/不适用依据，资源未知标unknown，不猜成本），单Agent同预算对照单列不计HoH轮次，三种原生 Agent 的真实身份可核验；五种伪计数变体均不能增加有效轮次或满足门槛；审计副本不污染正式时间线，原始失败仍可查"
    ],
    "observables": [
      "轮次至候选/报告/TEST/原生会话的引用表",
      "五种审计负例和拒绝原因",
      "原始事件与输入文件摘要",
      "三类工程场景、seed与逐轮五项观测及单Agent对照清单"
    ]
  },
  {
    "id": "TEST-067-03",
    "requirement_id": "REQ-067",
    "acceptance_id": "AC-067-03",
    "check_id": "soak",
    "fixture": "登记了精确资源身份的真实 host/隔离工程仓库、已开始的写 Attempt，以及独立 fault-injection 子场景",
    "steps": [
      "test_067_03：按预登记触发点逐项注入并保存注入起止、事件水位",
      "重启 host 后从产品恢复路径观察 lease/effect/attempt，而非直接改 DB",
      "原生格式夹具逐项驱动错误分类并标 fault-injection；核对所有非本次资源未被清理"
    ],
    "pass_criteria": [
      "host 恢复沿既有 reconcile，从kill触发到reconcile完成与host-ready各≤30s；持续外部观察的受控故障窗口≤120s且包含这两项30s证明，120s不替代恢复时限；原效果至多提交一次、旧 writer 被 fence；不能确定的效果保持 UNKNOWN_EFFECT 并阻断重复执行；客户端断开不取消已接受任务；503 与额度不早于持久重试时刻派发，重启不清预算，额度不切 fallback；过滤原文与 failure_class=provider_content_filter 诊断可查且不记能力不足/成功，诊断子场景不贡献 live 轮次"
    ],
    "observables": [
      "故障清单、host 与进程身份",
      "reconcile/effect/lease/input 事件",
      "Retry-After/reset/内容过滤的原始协议证据和分类结果",
      "资源清理清单",
      "kill/reconcile完成/host-ready单调时刻，≤30s及外层≤120s窗口证明"
    ]
  },
  {
    "id": "TEST-067-04",
    "requirement_id": "REQ-067",
    "acceptance_id": "AC-067-04",
    "check_id": "soak",
    "fixture": "单独的短时负例执行与最终 72h 正例分目录，均有冻结身份和已写原始记录",
    "steps": [
      "test_067_04：以测试夹具启动短时受控执行，分别触发四种中止",
      "核对部分证据与已登记进程/unit 的清理证明",
      "用生产审计器尝试跨执行/候选拼接并保存拒绝；完整正例另行实跑 72h"
    ],
    "pass_criteria": [
      "每个负例非成功结束并保留已产生的输出、失败与清理记录，未执行部分不写 PASS；观察者缺口超 120 秒或候选变化使该窗口不满足；拼接被拒；新候选只能重新从零计时，正常结束清理只涉及登记资源"
    ],
    "observables": [
      "各负例单独的执行记录与首失败",
      "取消/超时/观察缺口记录",
      "跨窗口拒绝及资源终结证明"
    ]
  }
]
```

### REQ-068 · 性能预算（4 条 AC）

<!-- fragment:requirement:REQ-068 -->

```json
{
  "id": "REQ-068",
  "category": "NFR",
  "priority": "P1",
  "milestone": "M7",
  "title": "性能预算",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "idle core RSS≤150MiB(不含Agents/浏览器)，TUI输入p95≤100ms，事件可见p95≤500ms",
  "acceptance": [
    {
      "id": "AC-068-01",
      "given": "满足参考 profile 和 SSD 证明的 4 vCPU/8GiB release 环境，空库与百万事件库的真实 host+TUI，固定离线回放",
      "when": "在本候选同一次integration的CR-028 §5.1重测中串行采集idle RSS以及输入/事件每组至少1000次的完整路径样本；TEST-068-01/02与TEST-015-05/064-02读取同一次重测样本，不另开重复采样轮次",
      "then": "每个采样时刻的 idle host+TUI RSS 总和≤150MiB；每组输入 p95≤100ms、commit到事件可见 p95≤500ms；机器/构建/负载/候选摘要和原始样本完整，任一越界或缺项不满足",
      "test_id": "TEST-068-01"
    },
    {
      "id": "AC-068-02",
      "given": "80×24、180×40 的百万事件项目，含持续50ms增量合并、编辑/滚动/resize，原始输入与关键事件有唯一序号",
      "when": "在同一次§5.1重测中各输入子场景至少1000次并同时提交至少1000个待显示事件，按固定回放序列持续采集直到结束；TEST-068-01/02与M4对应TEST只对同组原始样本分别断言",
      "then": "每个尺寸/子场景单独达到输入100ms及事件500ms的p95门槛，无丢键/重复提交/关键事件丢失；冷启动及慢样本保留且不与其他尺寸/负载混池；活动 host/TUI 各≤256MiB，结束回idle总和≤150MiB",
      "test_id": "TEST-068-02"
    },
    {
      "id": "AC-068-03",
      "given": "同一性能驱动的隔离负例环境，分别缺SSD证据、破坏资源限制、竞争锁占用、引入未登记负载，以及有效环境下真实超时",
      "when": "逐项触发准入或执行失败并读取逐测试行和检查退出码",
      "then": "前四类未满足参考条件时逐项 SKIPPED、reason 明确、退出78且无正式性能 PASS；有效环境超阶段或超阈值保留 FAIL/非零，不被重复执行覆盖；无 DEGRADED；退出3仍仅表示既有 NO_TESTS",
      "test_id": "TEST-068-03"
    },
    {
      "id": "AC-068-04",
      "given": "保留全部原始样本的测量报告，以及独立构造的1000样本统计边界数据",
      "when": "重算p95，并分别删除慢样本、重复序号、改起止顺序、减少到999条或替换候选/binary摘要",
      "then": "统计边界准确取第950项，恰等阈值通过、大于阈值失败；五种不完整/不一致输入均拒绝，统计负例不冒充产品性能；最终报告的每组值可由原始记录重算且无跨候选复用",
      "test_id": "TEST-068-04"
    }
  ],
  "behavior": [
    "在 docs/01 的 4 vCPU/8 GiB/本地 SSD 参考环境、release 构建、离线固定回放上度量真实 host 与 TUI；沿 CR-028 §5.1 的共同 slice、私有 manager、排他锁、身份登记和五阶段预算，不以 renderer 单元测试代替完整路径。M7 另核验 SSD 后端，不能仅凭 rotational=0/1 宣称介质类型。 TEST-068-01/02与TEST-015-05/064-02消费同一候选、同一次integration、同一次§5.1重测的原始样本；不因TEST ID不同重复启动整轮采样，不复用M4历史候选的样本。各TEST仍独立断言并写各自逐测试结果/引用，原始分组不合池。",
    "核心 RSS 包含 idle host+TUI 的逐进程 VmRSS 之和，排除已明确列出的 Agent、浏览器、driver/guardian；不能把 TUI 排除，也不能用 cgroup memory.current 冒称 RSS。空项目和百万事件项目分别 idle 10 秒、100ms 采样，所有样本总和均≤150MiB；活动峰值仍遵守 M4 的 host/TUI 各≤256MiB。",
    "输入从外部 PTY 写入到屏幕出现对应改变计时；事件从事务 commit 到 TUI 对应 seq 可见计时，另保留 M4 快 SSE 观测；两种终端尺寸 80×24、180×40 及编辑/滚动/resize 子场景各≥1000 次有效样本，每组独立计算 p95≤100ms/500ms，禁止合池掩盖慢组。",
    "p95 为升序第 ceil(0.95*N) 项。原始样本携带序号、单调起止、负载、终端尺寸、candidate/environment/binary 摘要；冷启动/首次交互、慢样本不删，丢失/重复/倒序样本使结果不满足。机器参数、软件版本、fixture 摘要和全部阶段耗时随结果保留。",
    "非参考环境/竞争负载/观测缺口按 M4 逐 TEST SKIPPED，detail.reason 明确，检查退出78；有效环境里的超预算或样本错误为 FAIL，不能改成跳过或 DEGRADE。总 integration 3600秒、工作3000秒、清理300秒和调度余量300秒不放宽；加入M7负载前先测前序五阶段成本与新增路径成本，再重测完整五阶段；装不下停止并交CTO重规划，不放宽timeout或删AC。"
  ],
  "failure_modes": [
    "host/TUI RSS、输入/事件时延任一越界即 FAIL；缺任一尺寸/负载/1000样本不能 PASS。",
    "SSD/CPU/内存包络不能证明、无排他窗口或时钟/样本无效不声称性能通过；已产生 FAIL 保留。",
    "只测渲染函数、只报告均值、排除慢样本、混入 debug/不同候选结果或升级后沿用旧 PASS 均被拒。"
  ],
  "decisions": [
    "原有 150MiB/100ms/500ms、至少1000样本不变；沿用 CR-028 已批准的严格 RSS 口径，M4 证据不能代替 M7 新候选证据。",
    "测量场景细分和SSD证明按CTO于2026-11-22已采纳D3（来源见决策节）；不增Shell命令，不引入另一性能检查或放宽现有检查超时。"
  ],
  "doc_refs": [
    "docs/01-prd.md",
    "docs/02-functional-spec.md",
    "docs/07-algorithms.md",
    "docs/10-acceptance.md"
  ]
}
```

对应场景（各场景唯一check_id见JSON）：

<!-- fragment:scenarios:REQ-068 -->

```json
[
  {
    "id": "TEST-068-01",
    "requirement_id": "REQ-068",
    "acceptance_id": "AC-068-01",
    "check_id": "integration",
    "fixture": "满足参考 profile 和 SSD 证明的 4 vCPU/8GiB release 环境，空库与百万事件库的真实 host+TUI，固定离线回放",
    "steps": [
      "test_068_01：读取本候选同次integration的CR-028 §5.1唯一重测样本，独立断言与写本TEST结果，不另开一轮1000次采样；先测五阶段成本，不足按D3交CTO重规划",
      "核对参考环境/SSD/排他与 release 身份",
      "两种库分别采 idle RSS，两种尺寸及各负载收满样本",
      "逐组重算 nearest-rank p95 并核对全部阈值"
    ],
    "pass_criteria": [
      "每个采样时刻的 idle host+TUI RSS 总和≤150MiB；每组输入 p95≤100ms、commit到事件可见 p95≤500ms；机器/构建/负载/候选摘要和原始样本完整，任一越界或缺项不满足"
    ],
    "observables": [
      "逐进程 VmRSS/身份/排除项",
      "PTY 与 commit/seq 原始时刻",
      "全部样本、环境和阶段预算记录"
    ]
  },
  {
    "id": "TEST-068-02",
    "requirement_id": "REQ-068",
    "acceptance_id": "AC-068-02",
    "check_id": "integration",
    "fixture": "80×24、180×40 的百万事件项目，含持续50ms增量合并、编辑/滚动/resize，原始输入与关键事件有唯一序号",
    "steps": [
      "test_068_02：读取本候选同次integration的CR-028 §5.1唯一重测样本，独立断言与写本TEST结果，不另开一轮1000次采样；先测五阶段成本，不足按D3交CTO重规划",
      "按固定种子产生三类输入和有序关键事件",
      "测量产品完整路径并独立记录每组输入、commit和屏幕序号",
      "对每个尺寸/负载分别统计，核对全部序号及峰值"
    ],
    "pass_criteria": [
      "每个尺寸/子场景单独达到输入100ms及事件500ms的p95门槛，无丢键/重复提交/关键事件丢失；冷启动及慢样本保留且不与其他尺寸/负载混池；活动 host/TUI 各≤256MiB，结束回idle总和≤150MiB"
    ],
    "observables": [
      "各尺寸各负载结果",
      "输入/命令/事件序号对照",
      "固定回放序列与冷启动样本",
      "峰值与idle RSS"
    ]
  },
  {
    "id": "TEST-068-03",
    "requirement_id": "REQ-068",
    "acceptance_id": "AC-068-03",
    "check_id": "integration",
    "fixture": "同一性能驱动的隔离负例环境，分别缺SSD证据、破坏资源限制、竞争锁占用、引入未登记负载，以及有效环境下真实超时",
    "steps": [
      "test_068_03：在受控设施中逐项改变准入条件，保留正对照",
      "有效参考条件中触发超预算负例",
      "核对生产驱动的判定及清理，不篡改正式参考清单"
    ],
    "pass_criteria": [
      "前四类未满足参考条件时逐项 SKIPPED、reason 明确、退出78且无正式性能 PASS；有效环境超阶段或超阈值保留 FAIL/非零，不被重复执行覆盖；无 DEGRADED；退出3仍仅表示既有 NO_TESTS"
    ],
    "observables": [
      "准入输入/原因及退出码",
      "原始阶段时长和首失败",
      "锁、资源树和清理证明"
    ]
  },
  {
    "id": "TEST-068-04",
    "requirement_id": "REQ-068",
    "acceptance_id": "AC-068-04",
    "check_id": "integration",
    "fixture": "保留全部原始样本的测量报告，以及独立构造的1000样本统计边界数据",
    "steps": [
      "test_068_04：用独立期望值核对等于与超过阈值的统计边界",
      "对隔离报告副本施加五种损坏",
      "用同一结果校验路径拒绝负例，保留原始未修改数据"
    ],
    "pass_criteria": [
      "统计边界准确取第950项，恰等阈值通过、大于阈值失败；五种不完整/不一致输入均拒绝，统计负例不冒充产品性能；最终报告的每组值可由原始记录重算且无跨候选复用"
    ],
    "observables": [
      "统计边界夹具及独立期望",
      "样本摘要/序号完整性",
      "损坏报告与拒绝理由"
    ]
  }
]
```

### REQ-069 · 交付真实性（4 条 AC）

<!-- fragment:requirement:REQ-069 -->

```json
{
  "id": "REQ-069",
  "category": "NFR",
  "priority": "P1",
  "milestone": "M7",
  "title": "交付真实性",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "明确哪些测试真实、模拟、跳过；未通过绝不称生产完成",
  "acceptance": [
    {
      "id": "AC-069-01",
      "given": "生产交付审计器、同候选真实执行记录和显式标注的隔离完整/不完整证据夹具",
      "when": "经traceability的test_069_01测试生成交付汇总，并逐项对照原始执行、逐TEST行、计划mode与未运行原因",
      "then": "所有REQ/AC/TEST/check及原始引用可追溯，product/live/mock与SKIPPED/NOT_RUN/BLOCKED含原因且不混淆；缺记录、模拟冒充live、退出0但缺断言、先FAIL后PASS等负例不满足；真实候选缺任一v1项时汇总明确未完成",
      "test_id": "TEST-069-01"
    },
    {
      "id": "AC-069-02",
      "given": "本冻结候选真实build检查产出的release bundle与其构建工具输出、资源/Web/SBOM/notice清单，以及独立副本",
      "when": "核对归档和安装后的版本/摘要；在副本分别替换旧二进制、错资源、缺Web资产、缺SBOM/notice、使用另一执行重建包",
      "then": "正例的构建提交、tree、锁文件与真实工具输出可追溯，包内必需资产齐全且摘要/版本一致，release hash等于build声明产物；所有替换/缺失/来源不符均被拒，拒绝保留实际差异，不靠文件名或版本字符串单独判定",
      "test_id": "TEST-069-02"
    },
    {
      "id": "AC-069-03",
      "given": "当前候选C已有检查和报告，隔离仓库可生成源码/规格/环境/计划改变后的C2及合并后提交",
      "when": "分别把C的记录作为C2或合并提交的当前证据，再损坏一个ArtifactRef；最后查询历史记录",
      "then": "所有跨候选/规格/环境/计划/提交引用及摘要损坏都不能计当前验收，历史仍可查并标stale/invalid；新候选需要重新执行检查，汇总不能只按文件路径或最后PASS复用",
      "test_id": "TEST-069-03"
    },
    {
      "id": "AC-069-04",
      "given": "生产发布导出器的v1完整夹具、仅三插件合规降级夹具、v0.1夹具及真实候选的未齐全汇总",
      "when": "分别缺soak/平台/独立QA或Review、使QA与开发共用原生会话、给required项降级、加入critical/high，再写入模型自称COMPLETED",
      "then": "只在完整v1证据且门禁通过时导出COMPLETED；三插件合规降级为COMPLETED_DEGRADED；v0.1保持INTERIM_RELEASED；各负例和实际未齐候选不完成，模型claim不改变状态；会话独立按CR-029原生身份核对",
      "test_id": "TEST-069-04"
    }
  ],
  "behavior": [
    "交付汇总逐 REQ/AC/TEST/check 列出 candidate/spec/environment/test_plan/执行记录/原始工件及摘要，区分 product、live、mock/fault-injection 子场景和未运行原因；计划 mode 来自冻结计划，fixture不能改名为live，空证据/模型总结/进程退出0都不能代替验收。",
    "发布状态只由当前候选的 v1 全量门禁产生；70项需求、14项required checks、独立QA/Review、soak与原始产物齐全才可 COMPLETED；仅 CR-001 三插件的合规降级才可 COMPLETED_DEGRADED；v0.1 为 INTERIM_RELEASED，不算最终完成。",
    "build 交付归档的二进制由构建工具实际输出路径提供；记录构建输入提交/tree/锁文件/工具链与二进制/资源/Web资产/SBOM/notice的摘要，归档本身的release hash来自同候选build执行。不能猜 target 路径、从旧目录挑可执行文件或用另一次deployment重建包顶替本次build产物。",
    "交付文档的安装/配置/升级/回滚命令与实际执行记录对应；只宣称已验证平台。用户可由摘要清单核对下载/安装包、运行版本和内置资源。缺资产、版本不一致或引用断链明确不满足。",
    "源码/规格/环境/检查计划或合并后提交变化，旧证据只留历史、不计新候选通过；保留同候选首失败与 FLAKY，不能选最后一次 PASS 或手工改汇总。实际报告使用生产审计与导出路径；负例构造在独立副本，不能修改冻结待发布产物。"
  ],
  "failure_modes": [
    "mock冒充live、缺执行/原始记录、摘要不符、未知被写成0或PASS、失败历史被过滤：报告不满足且不能完成。",
    "旧二进制/错资源/Web缺失/SBOM或notice缺失/发布包不是本候选build输出：拒绝交付真实性结论。",
    "v0.1、独立报告交付完成、开发自测或bundle通过被写成v1完成：拒绝；非插件required项不能降级。"
  ],
  "decisions": [
    "保持 CR-017/ADR-027 移交：TEST-069-01 在 tests/bundle 中以 test_069_01 命名，并由 traceability 自测发现；TEST-069-02经deployment验证真实归档/安装，TEST-069-03/04经integration验证Runtime的失效与发布导出；各自沿check实际测试target，不增加traceability的自测ID。",
    "traceability 与 independent-* 的 argv 在 M7-T02 仅将已有profile参数从v0.1换成v1，保留检查名、mode、超时和既有产物；这是已批准移交，驱动修改随实现，M7-T01仅细化规格。",
    "真实性测试可用显式标注的隔离证据夹具覆盖拒绝分支；正例核验本候选的真实build包和生产导出器。产品发布仍须最终真实全量证据，不把夹具门禁PASS当HoH发布。"
  ],
  "doc_refs": [
    "docs/10-acceptance.md",
    "docs/21-spec-governance.md",
    "docs/adr/ADR-010.md",
    "docs/adr/ADR-027.md",
    "docs/adr/ADR-029.md"
  ]
}
```

对应场景（各场景唯一check_id见JSON）：

<!-- fragment:scenarios:REQ-069 -->

```json
[
  {
    "id": "TEST-069-01",
    "requirement_id": "REQ-069",
    "acceptance_id": "AC-069-01",
    "check_id": "traceability",
    "fixture": "生产交付审计器、同候选真实执行记录和显式标注的隔离完整/不完整证据夹具",
    "steps": [
      "tests/bundle中的test_069_01调用生产审计/导出路径而非复写规则",
      "正对照与缺记录/错mode/空断言/FLAKY变体逐项核对",
      "输出本候选实际汇总，与原始记录逐项相等核查"
    ],
    "pass_criteria": [
      "所有REQ/AC/TEST/check及原始引用可追溯，product/live/mock与SKIPPED/NOT_RUN/BLOCKED含原因且不混淆；缺记录、模拟冒充live、退出0但缺断言、先FAIL后PASS等负例不满足；真实候选缺任一v1项时汇总明确未完成"
    ],
    "observables": [
      "traceability-report.json与逐TEST结果",
      "生产交付报告及原始ArtifactRef",
      "显式fixture标记和变体拒绝原因"
    ]
  },
  {
    "id": "TEST-069-02",
    "requirement_id": "REQ-069",
    "acceptance_id": "AC-069-02",
    "check_id": "deployment",
    "fixture": "本冻结候选真实build检查产出的release bundle与其构建工具输出、资源/Web/SBOM/notice清单，以及独立副本",
    "steps": [
      "deployment中的test_069_02读取本候选build执行声明的真实包和机器清单",
      "在隔离目录验证安装产物身份，再生成逐项损坏变体",
      "由生产交付审计逻辑核对原包与负例"
    ],
    "pass_criteria": [
      "正例的构建提交、tree、锁文件与真实工具输出可追溯，包内必需资产齐全且摘要/版本一致，release hash等于build声明产物；所有替换/缺失/来源不符均被拒，拒绝保留实际差异，不靠文件名或版本字符串单独判定"
    ],
    "observables": [
      "build执行记录、构建工具JSON路径",
      "二进制/资源/Web/SBOM/notice/归档摘要与安装版本",
      "损坏变体审计记录"
    ]
  },
  {
    "id": "TEST-069-03",
    "requirement_id": "REQ-069",
    "acceptance_id": "AC-069-03",
    "check_id": "integration",
    "fixture": "当前候选C已有检查和报告，隔离仓库可生成源码/规格/环境/计划改变后的C2及合并后提交",
    "steps": [
      "integration中的test_069_03生成身份不同但名称相同的隔离候选",
      "逐维复用旧证据并调用生产审计路径",
      "核对当前拒绝与历史保留，并确认首失败未删除"
    ],
    "pass_criteria": [
      "所有跨候选/规格/环境/计划/提交引用及摘要损坏都不能计当前验收，历史仍可查并标stale/invalid；新候选需要重新执行检查，汇总不能只按文件路径或最后PASS复用"
    ],
    "observables": [
      "候选清单与各身份摘要",
      "生产审计拒绝理由",
      "历史/当前报告对照"
    ]
  },
  {
    "id": "TEST-069-04",
    "requirement_id": "REQ-069",
    "acceptance_id": "AC-069-04",
    "check_id": "integration",
    "fixture": "生产发布导出器的v1完整夹具、仅三插件合规降级夹具、v0.1夹具及真实候选的未齐全汇总",
    "steps": [
      "integration中的test_069_04分别执行完整/插件降级/v0.1正对照",
      "逐项缺失、身份冲突、required降级和issue负例",
      "核对真实候选的输出与release_gate一致，夹具结果单列"
    ],
    "pass_criteria": [
      "只在完整v1证据且门禁通过时导出COMPLETED；三插件合规降级为COMPLETED_DEGRADED；v0.1保持INTERIM_RELEASED；各负例和实际未齐候选不完成，模型claim不改变状态；会话独立按CR-029原生身份核对"
    ],
    "observables": [
      "门禁输入/输出、profile与release_status",
      "独立会话身份及原始报告",
      "交付报告的mock/product/live分类"
    ]
  }
]
```

### REQ-070 · 长期维护与升级（4 条 AC）

<!-- fragment:requirement:REQ-070 -->

```json
{
  "id": "REQ-070",
  "category": "NFR",
  "priority": "P1",
  "milestone": "M7",
  "title": "长期维护与升级",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "锁版本+固定回归任务；每次变更跑对照；不在活跃run自动升级",
  "acceptance": [
    {
      "id": "AC-070-01",
      "given": "支持平台上的旧/新版本安装包、版本锁与固定回归任务集，分别改变Adapter/Prompt/调度策略的三个受控版本对",
      "when": "对每个版本对分别运行相同回归任务并生成升级比较报告",
      "then": "每次变更都有旧新版本/锁/配置/任务集摘要和独立真实执行记录，覆盖集相等、差异逐项列出受影响AC、预期/实测值、原因与证据引用，任何缺项/回归/未知都不冒称通过；报告保留原始失败，测试模式清楚，旧PASS不满足新候选",
      "test_id": "TEST-070-01"
    },
    {
      "id": "AC-070-02",
      "given": "活跃Run定义为runs.state除CANCELLED、FAILED、COMPLETED以外的全部状态：CREATED/DISCOVERING/SPECIFYING/READY/RUNNING/VERIFYING/PAUSE_REQUESTED/PAUSED/WAITING_EXTERNAL/BLOCKED/CANCEL_REQUESTED/INCOMPLETE，逐态有版本快照的矩阵包含PAUSE_REQUESTED与CANCEL_REQUESTED，至少一例有在途Attempt或未决effect",
      "when": "检测到更新并触发自动更新路径/维护请求，再重启host并查询同一Run",
      "then": "上述全部非终态均满足：自动更新不替换活跃Run的binary/CLI/Prompt/Skill/插件/调度版本；不满足维护前提的请求被拒并有原因，重启后快照和原Attempt绑定保持；显式checkpoint换绑仍走原契约，不借维护改写快照",
      "test_id": "TEST-070-02"
    },
    {
      "id": "AC-070-03",
      "given": "无活跃Run/Attempt/未决effect/写租约的隔离旧版本安装，包含DB/WAL/对象与可核验备份",
      "when": "在两种支持Ubuntu上进行正常升级，并在独立副本于迁移与切换边界失败/取消后恢复",
      "then": "正常升级保留权威事件/seq/引用对象、版本一致且健康检查与固定回归通过；核对已登记systemd用户单元的ExecStart解析到新版本二进制，通过该单元重启一次，MainPID对应的实际映像/版本/摘要等于新包且重启后仍健康；失败/取消不留下伪成功或半切换，旧版本或经校验备份可恢复，数据/hash/完整性前后核对；无自动删除失败记录或用户数据",
      "test_id": "TEST-070-03"
    },
    {
      "id": "AC-070-04",
      "given": "已升级的新schema数据、真实旧binary、有效与损坏两类备份，以及原生CLI/资源版本漂移夹具",
      "when": "尝试旧binary直接打开新数据，再按恢复文档使用有效/损坏备份；检测一次CLI/资源漂移",
      "then": "不兼容旧binary拒绝且不改新数据，损坏备份拒绝并保留现场；有效备份按既有恢复边界成功且数据/版本一致；漂移有实际版本与兼容诊断，不静默升级或宣称通过；所有操作只触及本次登记路径",
      "test_id": "TEST-070-04"
    }
  ],
  "behavior": [
    "Adapter/原生CLI/Prompt/Skill/调度策略、产品版本、锁文件与必要配置都有可复核的版本或摘要；Run使用冻结配置和资源版本，实际启动版本与兼容矩阵核对。升级候选的版本差异显式记录，不能把unknown补成当前最新。",
    "每次改变Adapter/Prompt/调度策略，使用同一组固定且有版本的回归任务在升级前后分别执行，保留版本、命令、结果/原始记录、AC覆盖、时延及未知用量；比较差异，不以两次都退出0作成功。两边源和证据独立绑定，旧成功不作为新候选通过。",
    "活跃Run定义为runs.state除CANCELLED、FAILED、COMPLETED以外的全部状态：CREATED/DISCOVERING/SPECIFYING/READY/RUNNING/VERIFYING/PAUSE_REQUESTED/PAUSED/WAITING_EXTERNAL/BLOCKED/CANCEL_REQUESTED/INCOMPLETE；矩阵逐态覆盖并至少明确观察PAUSE_REQUESTED与CANCEL_REQUESTED；不在其间自动替换二进制、底座CLI、Prompt/Skill、插件或调度快照。允许显式安全点的角色交接仍按REQ-019/047，不等于自动升级；不重写已持久Run快照或旧Attempt资源。",
    "升级由用户或已授权维护动作在无活跃Run、无在途Attempt及未决effect/写租约的维护边界执行；先核对并备份，再迁移/健康检查，成功才切换。失败/取消保留旧可用版本与完整备份并记录下一步；若新schema不可被旧binary读取，拒绝直接降级，走经过验证的备份恢复，不能丢数据硬降schema。 正常升级后由祖先M2-T14提供的systemd用户单元重启，核对单元/ExecStart实际解析路径、MainPID运行映像与新包摘要及健康，不用手工启动替代单元证明。",
    "新旧版本对照与恢复覆盖官方支持的Ubuntu 24.04/26.04 x86_64；文档写实际执行命令与边界。暂时无法获取旧版本/认证或环境时标NOT_RUN/BLOCKED，不能用mock升级代替部署证据；只操作登记的测试安装和数据。",
    "与M6和REQ-069共享deployment的3600s；按共同约定8先测成本并冻结完整矩阵分段预算，装不下交CTO重规划，不放宽timeout或删AC。test_070_01..04纳入NO_TESTS必需集，任一缺实际通过记录不能退出0。"
  ],
  "failure_modes": [
    "实际CLI/资源版本漂移、兼容性未知或锁文件缺失：明确诊断，按既有COMPAT_UNVERIFIED等规则阻塞，不能静默采用。",
    "回归任务缺失/旧新集合不等/已有失败被覆盖/活跃Run自动升级：不满足。",
    "迁移失败、中止、备份不完整、旧binary不兼容新schema：不宣称升级成功；保留恢复材料，不删除用户数据。"
  ],
  "decisions": [
    "维护入口复用已有安装器/资源管理与维护流程，不增加hoh upgrade等Shell子命令；沿REQ-044/063的备份恢复能力，具体文件布局/操作出口须与M5/M6应用结果重核（D4）。",
    "不改变REQ-019的显式checkpoint换绑与REQ-047的资源升级选择；本需求禁止静默自动切换，保留已授权显式行为。"
  ],
  "doc_refs": [
    "docs/14-resilience.md",
    "docs/15-prompts-skills-memory.md",
    "docs/10-acceptance.md",
    "docs/adr/ADR-009.md",
    "docs/adr/ADR-020.md",
    "docs/adr/ADR-030.md"
  ]
}
```

对应场景（各场景唯一check_id见JSON）：

<!-- fragment:scenarios:REQ-070 -->

```json
[
  {
    "id": "TEST-070-01",
    "requirement_id": "REQ-070",
    "acceptance_id": "AC-070-01",
    "check_id": "deployment",
    "fixture": "支持平台上的旧/新版本安装包、版本锁与固定回归任务集，分别改变Adapter/Prompt/调度策略的三个受控版本对",
    "steps": [
      "test_070_01：在隔离安装根构建/安装可核验的旧新版本",
      "三个变更维度分别执行同一任务集并收取真实产品结果",
      "比较AC覆盖、输出、耗时及版本，保留未知用量和首失败"
    ],
    "pass_criteria": [
      "每次变更都有旧新版本/锁/配置/任务集摘要和独立真实执行记录，覆盖集相等、差异逐项列出受影响AC、预期/实测值、原因与证据引用，任何缺项/回归/未知都不冒称通过；报告保留原始失败，测试模式清楚，旧PASS不满足新候选"
    ],
    "observables": [
      "版本/锁文件/回归任务集摘要",
      "旧新独立执行记录及差异报告",
      "安装日志和平台证明"
    ]
  },
  {
    "id": "TEST-070-02",
    "requirement_id": "REQ-070",
    "acceptance_id": "AC-070-02",
    "check_id": "deployment",
    "fixture": "活跃Run定义为runs.state除CANCELLED、FAILED、COMPLETED以外的全部状态：CREATED/DISCOVERING/SPECIFYING/READY/RUNNING/VERIFYING/PAUSE_REQUESTED/PAUSED/WAITING_EXTERNAL/BLOCKED/CANCEL_REQUESTED/INCOMPLETE，逐态有版本快照的矩阵包含PAUSE_REQUESTED与CANCEL_REQUESTED，至少一例有在途Attempt或未决effect",
    "steps": [
      "test_070_02：逐状态建立真实产品Run和快照",
      "模拟更新可用这一外部通知，实际维护路径验证边界",
      "重启并核对配置/资源摘要和Attempt身份"
    ],
    "pass_criteria": [
      "上述全部非终态均满足：自动更新不替换活跃Run的binary/CLI/Prompt/Skill/插件/调度版本；不满足维护前提的请求被拒并有原因，重启后快照和原Attempt绑定保持；显式checkpoint换绑仍走原契约，不借维护改写快照"
    ],
    "observables": [
      "Run/Attempt快照与维护拒绝",
      "更新前后binary/资源/配置摘要",
      "重启事件与显式换绑正对照"
    ]
  },
  {
    "id": "TEST-070-03",
    "requirement_id": "REQ-070",
    "acceptance_id": "AC-070-03",
    "check_id": "deployment",
    "fixture": "无活跃Run/Attempt/未决effect/写租约的隔离旧版本安装，包含DB/WAL/对象与可核验备份",
    "steps": [
      "test_070_03：通过产品备份接口形成一致性备份而非只复制DB主文件",
      "执行正常升级和独立失败/取消矩阵，读回schema/对象/版本",
      "按实际恢复手册回到可用状态并重跑固定回归"
    ],
    "pass_criteria": [
      "正常升级保留权威事件/seq/引用对象、版本一致且健康检查与固定回归通过；核对已登记systemd用户单元的ExecStart解析到新版本二进制，通过该单元重启一次，MainPID对应的实际映像/版本/摘要等于新包且重启后仍健康；失败/取消不留下伪成功或半切换，旧版本或经校验备份可恢复，数据/hash/完整性前后核对；无自动删除失败记录或用户数据"
    ],
    "observables": [
      "升级/取消/恢复原始命令与退出码",
      "DB integrity/foreign_key与对象摘要",
      "两平台结果和备份清单",
      "systemd用户单元身份/ExecStart、重启前后MainPID与映像摘要及健康记录"
    ]
  },
  {
    "id": "TEST-070-04",
    "requirement_id": "REQ-070",
    "acceptance_id": "AC-070-04",
    "check_id": "deployment",
    "fixture": "已升级的新schema数据、真实旧binary、有效与损坏两类备份，以及原生CLI/资源版本漂移夹具",
    "steps": [
      "test_070_04：保存新数据摘要，执行旧binary拒读并再次核对",
      "先验证损坏备份拒绝，再在另一隔离根恢复有效备份",
      "触发版本漂移并核对doctor/兼容记录与阻塞原因"
    ],
    "pass_criteria": [
      "不兼容旧binary拒绝且不改新数据，损坏备份拒绝并保留现场；有效备份按既有恢复边界成功且数据/版本一致；漂移有实际版本与兼容诊断，不静默升级或宣称通过；所有操作只触及本次登记路径"
    ],
    "observables": [
      "拒读前后数据摘要",
      "备份校验/恢复/doctor日志",
      "实际CLI/资源版本和资源所有权清单"
    ]
  }
]
```

## M7 任务拆分与逐 AC 能力闭包

顺序是 T01 → {T05、T06} → T02 → T03 → T04；花括号仅表示依赖无先后，不授权两个写者并行。M7-T04在数组及拓扑顺序中最后，仍覆盖v1全部70项；新增实现任务插在它之前。

| 任务 | kind | 需求/AC数 | depends_on | check owner |
|---|---|---|---|---|
| M7-T01 | spec | 4项/不拥有AC | M6-T04 | 无 |
| M7-T05 | implement | REQ-068 / 4 | M7-T01 | 不重复认领integration |
| M7-T06 | implement | REQ-070 / 4 | M7-T01 | 不重复认领deployment |
| M7-T02 | implement | REQ-069 / 4 | M7-T05、M7-T06 | 不重复认领traceability/independent-* |
| M7-T03 | implement | REQ-067 / 4 | M7-T01、M7-T02、M7-T05、M7-T06 | soak（原归属保留） |
| M7-T04 | verify | v1全部70项/不拥有AC | 上述四个实现任务、M0-T05 | 无 |

用以下数组完整替换当前全部M7任务，其他里程碑条目保持；后续前序CR拆分必须保持其verify收全所有owner。

<!-- fragment:tasks:M7 -->

```json
[
  {
    "id": "M7-T01",
    "milestone": "M7",
    "kind": "spec",
    "owner_role": "cto",
    "writer": true,
    "title": "细化M7需求为detailed：每项2–5条可判定AC与场景，写CR并relock基线",
    "depends_on": [
      "M6-T04"
    ],
    "requirements": [
      "REQ-067",
      "REQ-068",
      "REQ-069",
      "REQ-070"
    ],
    "done_when": "本里程碑全部需求spec_maturity=detailed；validate_bundle通过；CR已APPROVED并relock；按每个实现任务≤2项需求、≤8条AC拆分本里程碑实现任务（更新 development-tasks.json 的 requirements/acceptance/checks/depends_on，validate_bundle 的归属规则通过）；每条 AC 只分给依赖闭包已提供其所需能力（host、API、CLI 最终结果、QA/Review 循环、systemd、doctor、TUI 等）的任务（CR3R-02）。 CR-032获独立设计审查与CTO批准后按最终M4/M5/M6逐项重核假设并应用；生成视图、relock与bundle通过；文档草案提交本身不算此任务完成。"
  },
  {
    "id": "M7-T05",
    "milestone": "M7",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Performance budgets and reference-environment measurements",
    "depends_on": [
      "M7-T01"
    ],
    "requirements": [
      "REQ-068"
    ],
    "acceptance": [
      "AC-068-01",
      "AC-068-02",
      "AC-068-03",
      "AC-068-04"
    ],
    "checks": [],
    "done_when": "所列AC的实现与检查驱动完成，冻结候选上逐TEST原始记录齐全并经独立QA/Review；required项不能DEGRADED，自测不作验收。沿CR-028 integration唯一串行release入口交付TEST-068-01..04，证明SSD后端，加入M7负载后重测完整五阶段且总超时/余量不放宽；缺环境为BLOCKED；不抢占既有integration check owner。 TEST-068-01/02与M4 TEST-015-05/064-02复用本候选同次§5.1唯一重测样本，各自独立判定；先测成本再冻结五阶段预算，装不下交CTO重规划，不降低任何阈值或AC。"
  },
  {
    "id": "M7-T06",
    "milestone": "M7",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Version locking, comparative regressions and maintenance upgrades",
    "depends_on": [
      "M7-T01"
    ],
    "requirements": [
      "REQ-070"
    ],
    "acceptance": [
      "AC-070-01",
      "AC-070-02",
      "AC-070-03",
      "AC-070-04"
    ],
    "checks": [],
    "done_when": "所列AC的实现与检查驱动完成，冻结候选上逐TEST原始记录齐全并经独立QA/Review；required项不能DEGRADED，自测不作验收。交付两平台升级/失败/取消/恢复与三个变更维度的固定回归；复用M5备份/M6安装能力，不增加Shell命令；测试安装与资源归属可核验。 按共同约定8先测M6全新VM与既有TEST-005-06和本任务完整矩阵的合并成本，deployment总3600s不变；超预算交CTO重规划、不删AC。向deployment既有NO_TESTS必需集追加test_070_01、test_070_02、test_070_03、test_070_04；任一没有实际通过记录时以3/NO_TESTS退出，原失败保留；T02后续追加069-02不能覆盖此集合。两请求态的活跃Run与升级后用户单元重启的证据必含。"
  },
  {
    "id": "M7-T02",
    "milestone": "M7",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Delivery evidence audit and v1 verification profile",
    "depends_on": [
      "M7-T05",
      "M7-T06"
    ],
    "requirements": [
      "REQ-069"
    ],
    "acceptance": [
      "AC-069-01",
      "AC-069-02",
      "AC-069-03",
      "AC-069-04"
    ],
    "checks": [],
    "done_when": "所列AC的实现与检查驱动完成，冻结候选上逐TEST原始记录齐全并经独立QA/Review；required项不能DEGRADED，自测不作验收。承接CR-017/ADR-027：traceability及independent-*的argv切换v1，保留mode/超时/产物；test_069_01写在tests/bundle供traceability自测发现；test_069_02由deployment、test_069_03/04由integration的真实target执行。M6真实build包及交付导出路径可审计；全v1 traceability在soak之前预期未满足，测试级诊断PASS不冒充AC验收；最终本候选全量收取由M7-T04负责，见D7。 按共同约定8实测加入test_069_02后的完整deployment成本（含M6与T06），总3600s不变，超预算交CTO重规划、不删AC；向既有NO_TESTS必需集追加test_069_02并保留test_005_06、M6全部TEST与test_070_01..04，任一缺实际通过记录时以3/NO_TESTS退出，不能只按旧test_005_06通过判成功。"
  },
  {
    "id": "M7-T03",
    "milestone": "M7",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Bounded 72-hour soak and real engineering iterations",
    "depends_on": [
      "M7-T01",
      "M7-T02",
      "M7-T05",
      "M7-T06"
    ],
    "requirements": [
      "REQ-067"
    ],
    "acceptance": [
      "AC-067-01",
      "AC-067-02",
      "AC-067-03",
      "AC-067-04"
    ],
    "checks": [
      "soak"
    ],
    "done_when": "所列AC的实现与检查驱动完成，冻结候选上逐TEST原始记录齐全并经独立QA/Review；required项不能DEGRADED，自测不作验收。保留soak唯一check owner；交付有截止时间的真实host驱动、逐事件harness_candidate_id、心跳、原生轮次链及故障矩阵，满足ADR-027原则一规则18。用正式候选经冻结计划实跑72h/20轮；任何源码/规格/环境/计划改变新候选重跑。驱动成熟后最后固定候选由M7-T04重新收全量证据，旧候选的soak不可借用。 D1每个连续24h区间的真实轮次分布、D2的kill到reconcile/host-ready各≤30s与外层观察≤120s均需独立断言；D5原生Attempt过滤诊断接线由本任务落实。"
  },
  {
    "id": "M7-T04",
    "milestone": "M7",
    "kind": "verify",
    "owner_role": "qa",
    "writer": false,
    "title": "全量真实验收：全部检查、独立QA/Review、release_gate --profile v1",
    "depends_on": [
      "M7-T02",
      "M7-T03",
      "M7-T05",
      "M7-T06",
      "M0-T05"
    ],
    "requirements": [
      "REQ-001",
      "REQ-002",
      "REQ-003",
      "REQ-004",
      "REQ-005",
      "REQ-006",
      "REQ-007",
      "REQ-008",
      "REQ-009",
      "REQ-010",
      "REQ-011",
      "REQ-012",
      "REQ-013",
      "REQ-014",
      "REQ-015",
      "REQ-016",
      "REQ-017",
      "REQ-018",
      "REQ-019",
      "REQ-020",
      "REQ-021",
      "REQ-022",
      "REQ-023",
      "REQ-024",
      "REQ-025",
      "REQ-026",
      "REQ-027",
      "REQ-028",
      "REQ-029",
      "REQ-030",
      "REQ-031",
      "REQ-032",
      "REQ-033",
      "REQ-034",
      "REQ-035",
      "REQ-036",
      "REQ-037",
      "REQ-038",
      "REQ-039",
      "REQ-040",
      "REQ-041",
      "REQ-042",
      "REQ-043",
      "REQ-044",
      "REQ-045",
      "REQ-046",
      "REQ-047",
      "REQ-048",
      "REQ-049",
      "REQ-050",
      "REQ-051",
      "REQ-052",
      "REQ-053",
      "REQ-054",
      "REQ-055",
      "REQ-056",
      "REQ-057",
      "REQ-058",
      "REQ-059",
      "REQ-060",
      "REQ-061",
      "REQ-062",
      "REQ-063",
      "REQ-064",
      "REQ-065",
      "REQ-066",
      "REQ-067",
      "REQ-068",
      "REQ-069",
      "REQ-070"
    ],
    "profile": "v1",
    "done_when": "同一最终冻结候选上全70项需求与v1全部14检查满足；独立QA与Review原生会话相互独立且不同于开发者；无未解决critical/high，medium/low按既有规则留痕。M4承接：一次最终tui-pty收Ubuntu24.04/26.04两平台完整矩阵与宽度交集原始证据，缺一不通过；M6安装文档/部署两平台实际验证。REQ-068正式integration只在已批准参考profile，含SSD核验，不借24.04普通cargo结果代替。全部证据及72h/20个live轮次均绑定本最终候选；traceability与independent-*实际argv为v1。release_gate --profile v1 PASS且release_status为COMPLETED或合规COMPLETED_DEGRADED才完成，否则INCOMPLETE/BLOCKED；verify不改源、不拥有acceptance/checks。"
  }
]
```

能力前提表（用于下方逐条核对；“本任务”表示该owner必须自己交付，不能向后依赖）：

| 能力 | 已有提供者与M7依赖路径 | 未应用前提 |
|---|---|---|
| K1 基础检查 | integration=M0-T03，deployment=M2-T14，traceability及independent-*=M2-T10，soak=M7-T03 | 机械探针核对当前真实tasks[].checks，不能按标题猜owner |
| K2 host/恢复/效果/预算 | M2-T01/02/05/06/07/19，候选/汇合M2-T10/16/18，CLI/TUI结果M2-T13；经M2-T11→V01-T02→M3→M4→M5→M6-T04→M7-T01 | A6；完成能力不能只靠规格存在 |
| K3 测量/界面/公开事件 | M4-T03/T07/T09/T10→M4-T04→M5-T01→M5-T04→M6-T01→M6-T04→M7-T01 | A1/A2：当前main仍旧任务表；批准CR-028应用后须重算，当前闭包仅核旧M4-T02/T03 |
| K4 发布/回归/恢复/插件 | M5-T05/T06/T02/T03/T09/T10/T07/T08→M5-T04→M6-T01→M6-T04→M7-T01 | A3/A4；当前DRAFT ID已核，不将其当最终应用ID |
| K5 Web/交付/两平台 | M6-T02（Web外壳/资产）、M6-T05（重连/多客户端）、M6-T06（Git/合并身份）、M6-T03（两平台fresh-VM/安装/文档）→M6-T04→M7-T01 | A5的CR-031快照，M6-T04必须依赖全部四owner；共享deployment先测3600s总成本，平台实测是M7-T04收取前提 |
| K6 M7自身 | 性能M7-T05；维护M7-T06；真实性/profile切换M7-T02；soak驱动M7-T03 | 本稿的16条AC，不由前序代签 |

| AC | 唯一实现owner / check | 所需能力及提供方（均为自身或依赖闭包） |
|---|---|---|
| AC-067-01 | M7-T03 / soak | K1/K2/K3/K4/K5 + 本任务真实soak/时钟/轮次驱动；T02审计、T05测量、T06版本维护均为祖先 |
| AC-067-02 | M7-T03 / soak | K2候选/独立会话/汇合；K3 iteration归属；K4审查与发布；本任务真实轮次审计，T02证据链 |
| AC-067-03 | M7-T03 / soak | K2 reconcile/lease/effect/retry/cancel；K4恢复点/备份；本任务故障注入/消费M5-T03的过滤诊断，M7-T06固定回归 |
| AC-067-04 | M7-T03 / soak | K2取消/fencing/完整性；本任务观察者/窗口审计与资源登记；T02跨候选拒绝 |
| AC-068-01 | M7-T05 / integration | K1 integration；K2 host/PTY；K3参考runner/RSS/百万库；本任务SSD核验和REQ-068完整计量 |
| AC-068-02 | M7-T05 / integration | K3编辑/流合并/公开seq与大库；本任务各尺寸负载和独立样本组 |
| AC-068-03 | M7-T05 / integration | K3锁/资源树/guardian/环境准入；本任务新增负例和全阶段预算复测 |
| AC-068-04 | M7-T05 / integration | K3原始样本与候选绑定；本任务独立统计oracle/完整性校验 |
| AC-069-01 | M7-T02 / traceability | K1 traceability；K2证据/逐TEST/独立报告；K4发布导出；K5交付；本任务审计及profile迁移 |
| AC-069-02 | M7-T02 / deployment | K5实际build包/安装/资源与Web；K4导出；T06维护版本链；本任务旧包/资产错配审计 |
| AC-069-03 | M7-T02 / integration | K2冻结/失效 + A6的M3-T02规格变更；K5的M6-T06合并提交；本任务跨身份拒绝/历史报告 |
| AC-069-04 | M7-T02 / integration | K2/CR-029独立身份；K4全量门禁/插件降级；本任务完整与未齐夹具断言，最终真实证据由T04收取 |
| AC-070-01 | M7-T06 / deployment | K1 deployment；K2兼容；M3-T03资源版本；K4回归；K5安装；本任务三维版本对照 |
| AC-070-02 | M7-T06 / deployment | K2状态/配置快照/安全点/重启；M3-T03资源锁定；本任务维护边界检查 |
| AC-070-03 | M7-T06 / deployment | K4备份/迁移/恢复；K5的M6-T03两平台安装，K1的M2-T14用户单元；本任务升级/取消故障、单元重启及健康检查 |
| AC-070-04 | M7-T06 / deployment | K2 doctor/兼容；K4备份校验/拒读；K5回滚文档；本任务漂移与损坏负例 |

机械闭包只证明图上的存在与可达，不证明前序生产实现。M4/M5/M6尚未应用时，对K3/K4/K5的语义闭包依上述假设；正式M7-T01必须按最终任务图再审每行能力，缺一则不能派发。产品AC完整接受不由任务板done推导，D7明确最终证据的收口。

## CTO 问题与已采纳决定（2026-11-22）

来源：CTO决策文件 `/home/ai/dev/hoh/.hoh-dev/evidence/spec-drafts-cto-decisions.md`，明确“CR-032 (M7) D1-D7: recommended defaults adopted”；原文快照保存在 `/home/ai/dev/hoh/.hoh-dev/evidence/m7-spec-d2/cto-decisions-2026-11-22.md`（sha256=`fb16875dd4198c86153f2b8172982a61bfbe877f32b939458204fba1a7400a2c`）。下表保留问题与推荐默认的来由，状态全部为**已采纳**，不是等待再次回答。决定采纳不等于本CR已通过独立设计审查或正式应用。

| ID / 状态 | 原问题与采纳的推荐默认 | 理由、落点与执行前提 |
|---|---|---|
| D1 已采纳 | 每60s持久心跳、正常间隔≤120s；每个连续24h区间≥1个真实accepted轮次，总数≥20 | 排除开头跑完后空转；AC-067-01/02与分布负例，区间判据见共同约定2 |
| D2 已采纳 | 受控故障外层观察≤120s、观察者不中断；kill→reconcile完成/host-ready各≤30s | 保留CR-028更严恢复界限；AC-067-03/04。观察者失效或候选漂移重开完整窗口，保留失败、不拼接、不暂停原deadline/预算；实测不符交CTO重规划，不放宽时限 |
| D3 已采纳 | 复用M4唯一reference/runner与同次重测样本；宿主后端配置+设备映射证明SSD；先测全五阶段成本再冻结预算 | AC-068-01..04；integration仍3600s（工作≤3000s、清理300s、调度300s），缺SSD证明BLOCKED，成本装不下交CTO重规划，不能改阈值/timeout或删场景 |
| D4 已采纳 | 固定版本化回归集覆盖三类变更，复用M5备份与M6安装/资源升级；旧版本取真实交付包，不兼容schema只从有效备份恢复 | AC-070-01..04；无新Shell子命令。共享deployment先测成本并保留3600s（共同约定8），接口按A3/A5最终应用结果复核，缺能力补前序 |
| D5 已采纳 | 有原生证据才记诊断failure_class=provider_content_filter，复用M5 AC-051-04；不是能力不足/成功、不加HCP枚举，过滤不自动耗尽重试，交既有决策路径；额度仍等待不fallback | AC-067-03，M7-T03落实原生Attempt观察接线；若需要改变FailureClass/协议语义，先提出前序补充CR，不借本稿授权 |
| D6 已采纳 | 复用M6最终manifest，以构建工具真实路径、提交/输入/产物hash逐文件核验；缺字段随负责驱动交付并锁定契约 | AC-069-02，M7-T02；格式按A5重核，不另建第二清单、不用版本字符串替代，受锁字段改变另有具体CR |
| D7 已采纳、问题关闭 | 沿自举路线先实现与逐TEST诊断，AC保持未验收；只有M7-T04收取同一最终候选全量证据；不伪造PASS、不改Runtime门禁 | T02/T03顺序实现不以前序全v1 traceability PASS解锁；最终产品AC与发布只由T04完整证据判定。未来迁入Runtime另核合法机制，不改变现eligible规则；当前实施路线不再标为应用前阻塞 |

CR-031 C1由**所有者于2026-11-22批准**，来源同上及本次委派：静态Web外壳无需token，Host/Origin校验保留，每个`/v1`请求token-gated；docs/03的措辞为“每个API请求”，token不进入服务日志或Referer。A5及M7对Web/交付的假设按此执行，不重新请求批准。

当前没有未回答的CTO选项。前序能力、SSD证明与合并检查成本是实施时的实测条件，不是已采纳决定的pending标签；若不能在现有CONTRACT架构、范围与门禁内满足，则记录实测证据、保留未验收状态并提交阻塞CTO问题，不自行降低要求。CR-031 C12的timeout放宽选项不在本次授权内。

## 审查记录

第1轮独立设计审查：reviewer **Grok / grok-4.7**，原生session **01a1ff66-9c22-76d0-8222-4ffc486e14b6**；attempt **f7f36acb-96af-4f94-a5f2-98f4fd3ca87a**，2026-11-21 18:37:55–18:56:29 UTC。入口记录 `/home/ai/dev/hoh/.hoh-dev/evidence/m7spec-review-grok-r1.out` 指向原生attempt的`stdout.jsonl`；以其最终`hoh-report`为准，结论 **FAIL**，1 high、6 medium、3 low。受审提交为本次输入`535b4469`，CR字节sha256=`a19f912a6d116d69352a041172a701569b3dedbcef737439c4c2b11c567bf626`。原生attempt路径为 `/home/ai/dev/hoh/.hoh-dev/attempts/f7f36acb-96af-4f94-a5f2-98f4fd3ca87a/attempt.json`；本轮提取报告与attempt副本在主仓库证据目录的`review-r1-report.json`、`review-r1-attempt.json`。没有把进程退出0当成审查PASS。

| finding | 原等级 | 第2稿处置 |
|---|---|---|
| CR32D-01 | high | 已修文本：067-01 then/场景pass_criteria强制每个连续24h区间≥1真实轮次，补集中完成后空转负例，D1记已采纳 |
| CR32D-02 | medium | 已修文本：D1–D7写明2026-11-22采纳及来源，移除D7待回答/应用阻塞，保持实现先行、AC未验收、T04同候选全量收证 |
| CR32D-03 | medium | 已修文本：067-03 then/场景同时要求kill→reconcile与host-ready各≤30s；≤120s为包含证明的外层观察窗口 |
| CR32D-04 | medium | 已修文本：A5绑定CR-031快照并点名T05/T06/T03与verify闭包；共同约定8及T02/T06 done_when先测合并deployment成本、保留3600s，新增测试纳入NO_TESTS必需集 |
| CR32D-05 | medium | 已修文本：067-02 then/场景写明CLI新建、Web增量、跨模块Bug及功能正确性/回归捕获/错误验收/总资源/人工干预五项记录 |
| CR32D-06 | medium | 已修文本：070-02给出除CANCELLED/FAILED/COMPLETED外全部runs.state，逐态矩阵含PAUSE_REQUESTED、CANCEL_REQUESTED |
| CR32D-07 | medium | 已修文本：068-01/02与M4对应TEST使用本候选同次§5.1唯一重测样本，独立断言不重复采样，先测成本、不足交CTO |
| CR32D-08 | low | 同段已修：原summary由全部AC共同覆盖，新增逐条承接表，不把AC-01等同整项验收 |
| CR32D-09 | low | 同批场景已修：067/068/070各场景明确test_NNN_MM，069保留；16条均核对 |
| CR32D-10 | low | 同段已修：070-03 then/场景要求用户单元ExecStart解析到新binary、经单元重启、MainPID映像与健康证明，闭包点名M2-T14 |

本轮10项均已在文本处理，无延期low；处置属于开发者claim与机械自测，**不改写首轮FAIL，也不自签设计审查PASS**。本次未启动复审Agent；下一轮由CTO安排，聚焦此修复diff、依赖边界与回归。

## 影响、不变内容与正式应用清单

拟应用文件：`contracts/requirements.json`、`contracts/acceptance-scenarios.json`、`contracts/development-tasks.json`，生成的`docs/02-functional-spec.md`/`docs/18-traceability.md`，以及依法relock的`contracts/baseline-lock.json`。`docs/24-dev-lessons-to-product.md`只在正式应用时标注吸收。四个旧AC/TEST ID、70需求全集、现有check ID/owner与release_class/targets完整保留。

本CR不更改CONTRACT、release-policy、协议/候选/执行/soak-event schema、既有检查argv或生产代码。上文驱动、profile迁移、manifest补齐、安装/用户文档等义务由各实现任务在真实交付时更改；需受锁契约增字段时先具象化补充CR，不用本草案空白授权未来任意schema变更。既有14个检查、required gate与soak最低时长/轮次不变，任何额外声明工件只增加不删除。因规格和测试数改变，旧候选全部重新冻结/验证；本文无复用历史PASS的例外。

正式应用依次：确认M6-T04与所有能力假设、执行已采纳D1–D7及C1；独立规格审查按docs/19收敛并由CTO按docs/21批准；领取M7-T01后核对当前main/锁/任务状态与用户改动；按片段合并并再核逐AC闭包；生成视图、relock、bundle；按docs/24规格影响闭包经验，在真实应用时运行完整`cargo test --workspace --locked`及受影响静态检查，核对契约与Rust实现。本文机械演练没有改Rust/schema，不声称已运行这项未来完整Rust回归。应用中任何修正先写CR，不能为了绿灯删AC或降基线。实现改命令时文档只写实际执行过的命令。

## 验证方式

本次为 **BUNDLE_ONLY，自测，非验收**；不运行live Agent或产品验收。所有证据保存在主仓库 `/home/ai/dev/hoh/.hoh-dev/evidence/m7-spec-d2/`，下列文件名均相对该目录；不是可删除worktree的忽略目录。真实受锁文件、任务图、检查计划与release-policy未改，`product_accepted=false`。

1. 驱动 `python3 /home/ai/dev/hoh/.hoh-dev/evidence/m7-spec-d2/validate_draft2.py` 从本CR直接提取九个带`fragment:`标记的JSON块，应用4个需求、16个场景、6个M7任务。逐项核对固定需求字段与旧AC/TEST ID保留、非M7条目不变、Given/When/Then非空、fixture/判据与AC相同、TEST命名与check映射，以及每实现任务≤2REQ/≤8AC和唯一归属。原始片段与完整受测CR保存在`fragments.json`、`tested-cr.md`。
2. 真实工作树的 `python3 tools/validate_bundle.py`、`python3 tools/baseline.py verify`均退出0（`real-baseline-validate.log`、`real-baseline-verify.log`）。从输入提交`535b4469b78e08bb4c792dfa29fea252ae78d459`执行`git clone --shared --no-checkout`及`git checkout --detach`，均退出0；完整argv/cwd/退出码/耗时与日志sha256在`commands.json`。
3. 副本根由`mktemp -d /home/ai/dev/m7spec-XXXXXX`创建，本次为`/home/ai/dev/m7spec-5WLwHS`；测试TMPDIR另建在Git/Cargo树之外的`/home/ai/dev/m7spec-gS8zvi`。精确路径、创建时设备/inode与任务归属在`resources.json`。shell加载真实工作树env.sh，子进程继承；只在副本把CR状态写为APPROVED、批准人写`SCRATCH ONLY`，这是机械演练，不是CTO批准。当前检查计划的soak占位与v0.1参数均未提前修改。
4. 下列命令真实执行，bundle串行运行以免争用；未改测试、超时或门禁。

| cwd | 实际命令 | 退出码 | 原始记录 |
|---|---|---|---|
| 副本 | `python3 tools/gen_spec_views.py` | 0 | `generate.log`；0.05s |
| 副本 | `python3 tools/baseline.py relock --cr docs/cr/CR-032-m7-detailed-spec.md` | 0 | `relock.log`；0.516s |
| 副本 | `python3 tools/validate_bundle.py` | 0 | `validate-applied.log`；0.697s |
| 副本 | `bash scripts/check.sh --bundle` | 0 | `scratch-bundle.log`；1556.131s |
| 真实工作树 | `bash scripts/check.sh --bundle` | 0 | `real-bundle.log`；916.11s |

副本bundle：**570 tests / 1554.343s / OK (skipped=2)**。真实工作树bundle：**570 tests / 914.308s / OK (skipped=2)**。副本`validate_bundle`为PASS、errors=0，70项需求、47项detailed、226条AC、67个任务；只代表当前基线+CR-032，不把未应用M4/M5/M6计为实现。副本relock为revision23、27个受锁文件；真实树仍revision22。两套bundle的既有skip按原日志保留，不计产品PASS：

- `test_ledger_outline_requirements_not_ready (test_bundle.SetupAndLedger.test_ledger_outline_requirements_not_ready) ... skipped 'M3 already detailed'`
- `test_repository_packages_verified (test_tools.PiPackages.test_repository_packages_verified) ... skipped 'Pi packages not installed here'`

结构/闭包探针通过（`ownership-probe.json`）：16条AC均唯一owner与合法check，verify最后且不拥有acceptance；调用真实`validate_bundle.ownership_problems`无错误，当前前序提供者均在祖先闭包。活跃Run矩阵从`contracts/sql/001_initial.sql`的runs.state枚举提取后核对，全部12个非终态均覆盖。

组合图探针通过（`combined-graph-probe.json`）：从固定CR-028/030/031快照提取任务数组，并采用CR-031明确给出的M6-T04四项依赖后，**82个任务、无缺失依赖/环**；M4/M5提供者与M6-T02/T03/T05/T06均在M7实现owner和最终verify的祖先闭包。此项仅验证任务图，不应用前序需求/OpenAPI/SQL，不证明前序能力已实现。审查处置文本探针`finding-disposition-probe.json`逐项核对10项修复落点，通过不等于独立复审PASS。

辅助脚本准备失败另存`preparation-1/`：第一次把baseline-lock的files映射误当列表，执行命令前退出1；修正后第二次clone退出128，因为归属marker使目标目录非空。改为在主仓库登记归属后clone成功；两次均未运行产品检查、未改测试。失败日志/资源登记/精确清理记录保留，没有将准备失败报告为产品红灯。

第1稿历史记录完整复制到`draft1-history/`，包括最终成功与四次中间失败，不覆盖删除，也不作为第2稿通过证据。历史范围外`CR032-OBS-01`（medium）：ProcessLifecycle测试读取已存在但内容尚空的pid文件；原traceback在`draft1-history/bound-m5/scratch-bundle.log`。本次未修该测试，不因后续通过称竞态已解决。

最终九个JSON块按文中顺序以两个LF连接的sha256：`0ec1a5ee7863da51dbb5e9ec41644c4c834d4d1fd5a3b3e796a7472c086d7785`。补写本节后核对片段与`tested-cr.md`逐字相同，真实树27个锁定文件、任务表/检查计划摘要对照`source-integrity-before.json`不变；提交前再运行validate_bundle、baseline verify与diff检查，记录在`precommit-checks.json`。副本按`resources.json`精确身份清理，结果在`cleanup.json`。本次未运行完整Rust产品回归、live、两平台部署/升级或72h soak；正式应用时仍须按影响闭包跑完整Rust回归和各计划检查，不能借bundle替代。

## 交付边界与续作

纯文档草案按`resources/skills/tdd/SKILL.md`无需生产红绿循环；这里的机械检查是**自测，非验收**。16条AC对应的产品测试、72h soak、三Agent live、两平台升级、最终v1 release_gate均NOT_RUN；第1轮独立设计审查为FAIL，第2稿复审NOT_RUN；不报告产品COMPLETED，也不把草案提交当M7-T01正式完成。

本次委派没有Runtime身份头/run_id/attempt_id/role_epoch、TaskResult输出文件与指定诊断目录，因此不编造TaskResult或candidate_id。唯一写权限来自本次输入；起始工作树干净。诊断保留在主仓库`/home/ai/dev/hoh/.hoh-dev/evidence/m7-spec-d2/`，只提交本CR，不推送、不relock真实树、不改全局PROGRESS/taskboard。下一步由CTO组织第2稿修复diff复审，并按最终前序应用结果复核接口；M6-T04后按最终主分支复核应用。
