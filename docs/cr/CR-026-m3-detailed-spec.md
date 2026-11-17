# CR-026 · M3 的 16 项需求细化为 detailed，与 M3 实现任务的拆分

ID：CR-026
状态：DRAFT
批准人：（待定。预期为“CTO 依 CR-003 D1 预授权（独立设计审查会话 …）”；见“是否属于 CR-003 D1”）
日期：2026-11-17（草稿，M3-T01 的准备稿；M3-T01 领取时按当时的 main 重新核对）
基线：改受锁文件 `contracts/requirements.json`（M3 的 16 项需求由 outline 细化为 detailed；REQ-018 的配置键表增加 `[assumptions]`）、`contracts/acceptance-scenarios.json`（这 16 项的场景）、`contracts/protocol.schema.json`（`$defs.ErrorCode`、`$defs.RejectionReason`、`$defs.BlockedReason` 补齐 7 个原因码）；不在锁内、随 CR 一起改：`contracts/development-tasks.json`（M3 实现任务的拆分）；`python3 tools/gen_spec_views.py` 重新生成 docs/02 与 docs/18；批准后 `python3 tools/baseline.py relock --cr docs/cr/CR-026-m3-detailed-spec.md`

本 CR 是 M3-T01（“细化 M3 需求为 detailed：每项 2–5 条可判定 AC 与场景，写 CR 并 relock 基线”）的准备稿。M3-T01 依赖 V01-T02，尚未领取；本稿不改任何受锁文件，下文的 JSON 片段已在仓库之外的副本上应用并通过 `tools/validate_bundle.py`（见“验证方式”）。依据是 M3 准备清单 `.hoh-dev/evidence/m3-prep/inventory.md`（写于 16ff3e1），已按 main 715e2d0 复核，更正见附录 A。

## 背景

M3 的 16 项需求（REQ-007、008、009、017、023、028、029、030、031、032、033、034、040、045、046、047）都是 `spec_maturity=outline`、`required`、`release_targets=[v1]`、P0：每项只有 summary 与一条 AC，AC 的 given/when/then 是占位（如 “when: 同 CommandHandler 的集成测试”，“then” 照抄 summary），没有 behavior、failure_modes、decisions、doc_refs；场景的 fixture 都是“待该里程碑 spec 任务细化”。CONTRACT 第 17 条要求 outline 在实现前细化为 detailed，不得删范围、降门槛。

M3-T01 的 done_when 还要求：按每个实现任务至多 2 项需求、8 条 AC 拆分本里程碑的实现任务（更新 development-tasks.json 的 requirements/acceptance/checks/depends_on，validate_bundle 的归属规则通过）；每条 AC 只分给依赖闭包已提供其所需能力的任务（CR3R-02）；REQ-039 按任务范围选代码的上下文来源分给一个实现任务（CR-016）；CR-018 交给 M3-T02、M3-T04 的各项分给一个实现任务并写进 done_when（M2T16D-1050）。

## 所有者已定的事项（2026-11-17，照录，不再讨论）

准备清单 §4 的六个所有者问题，所有者于 2026-11-17 都选了保守默认。本 CR 按下表写入需求：

| ID | 决定 | 写入 |
|---|---|---|
| O1 | 只有用户批准规格，HoH 从不代用户批准；无人值守的 Run 停在批准之前 | REQ-008 行为 2、REQ-009 行为 2 与失败模式 3、REQ-030 决定 2、REQ-034 行为 4 |
| O2 | 无人值守只假设项目配置中列出的默认值；产品方向、范围、非目标一律阻塞 | REQ-030 行为 4 与决定 1；REQ-018 的配置键表加 `[assumptions]` |
| O3 | 已有仓库：碰到用户未提交的文件先阻塞询问；发现不自动运行项目的构建与测试 | REQ-028 行为 3、4 与决定 1 |
| O4 | 用户规格是 docs/spec/ 下的 Markdown，固定标题与 id 行；HoH 生成 JSON 索引 | REQ-034 行为 1–3 与决定 1；REQ-029 行为 6 |
| O5 | Skill 不写进原生 Agent 的用户目录，只注入或写进会话自己的目录 | REQ-046 行为 1、失败模式 3 与决定 1 |
| O6 | 任何规格修订使全部验收失效，在新的候选上重验 | REQ-033 行为 4、失败模式 2 与决定 1；REQ-032 行为 5 |

## 决定

### 1. 共同约定（16 项共用，各需求的行为里引用）

- **规格根与格式**（O4）：用户项目的规格是 `docs/spec/*.md`；固定一级标题为用户、范围、非目标、决策、开放问题、需求；需求为 `## REQ-<id> <标题>`，验收为 `### AC-<req>-<nn>`，带 given、when、then、check 四个 id 行；决策条目带来源（question_id、input_id 或假设的事件 id），开放问题条目带“阻塞：是/否”。精确语法在 M3-T02 的 ADR 中定，内置模板（REQ-029）给出实例。`docs/spec/index.json` 是生成的 JSON 索引，带“生成，勿改”标记与来源摘要，不是事实源（REQ-034 行为 6）。
- **规格摘要**：规格根内全部规格文件按路径排序的 `<路径> <sha256>` 行的 sha256，与探测的树摘要同一算法。
- **已批准修订**：经 ApproveSpec（只由用户，O1）批准、sha256 为该修订规格摘要的 specs 行。只有已批准修订被采用、派发、冻结与验收引用。项目的首个规格经 ApproveSpec 成为修订 1（`spec.changed{cause:"approved"}`），之后的修订经 CR（REQ-033，`spec.changed{cause:"changed", cr_id}`）。受锁的 ApproveSpec 载荷照旧（`spec_sha256`、`decision`、`note`），ProposeSpecChange 照旧（`reason`、`spec_sha256`）。
- **新事件**（`$defs.Event.kind` 是开放的字符串，不需 CR；名称以 M3 各任务的 ADR 为准，AC 中出现的为定名）：`spec.index_failed`、`spec.generated_drift`、`task.migrated`、`task.replaced`、`readiness.passed`、`run.baseline`、`run.mode_change_pending`、`run.mode_changed`、`input.delivered`、`no_progress`（ADR-025 决定 1 已列为唤醒种类，产生方在 M3-T10）。
- **check**：AC 只用 v1 档已运行、且由 M3 实现任务的祖先交付的检查：unit、integration、tui-pty、recovery、deployment、adapter-live、traceability（M0-T03、M1-T03、M1-T08、M2-T05、M2-T10、M2-T14 交付）。没有 AC 用 web-e2e、plugin-live、soak（它们由 M5–M7 的任务交付）。

### 2. 补齐的原因码（`contracts/protocol.schema.json`，CR-003 D1 第 1 条）

追加到各自的 `enum` 末尾（旧：无），`$defs.BlockedReason.description` 末尾追加一句（旧：无）：

```json
{
  "ErrorCode": [
    "SPEC_CONFLICT",
    "SPEC_INVALID",
    "PROMPT_VARIABLE_MISSING",
    "PROMPT_TOO_LONG"
  ],
  "RejectionReason": [
    "CONSULT_CYCLE",
    "DISCUSSION_LIMIT"
  ],
  "BlockedReason": [
    "READINESS_NOT_MET"
  ]
}
```

`$defs.BlockedReason.description` 末尾追加：

> READINESS_NOT_MET (CR-026) is a task's blocked_reason only: the task's requirement or its dependency closure is not ready (REQ-031); it never blocks a run.


| 码 | 枚举 | 用处 |
|---|---|---|
| SPEC_CONFLICT | ErrorCode（409） | ApproveSpec 的摘要与工作树不同，或 CR 的 base 不是当前已批准修订（REQ-034、REQ-033） |
| SPEC_INVALID | ErrorCode（422） | 送批规格无法解析，或标为用户决定的条目没有来源（REQ-034、REQ-030） |
| PROMPT_VARIABLE_MISSING | ErrorCode | 模板引用白名单之外的变量，Attempt 不启动（REQ-045） |
| PROMPT_TOO_LONG | ErrorCode | 职位契约覆盖超过 32 KiB，保存即拒（REQ-045） |
| CONSULT_CYCLE | RejectionReason | 角色间咨询成环（REQ-023） |
| DISCUSSION_LIMIT | RejectionReason | 第三轮讨论（REQ-023） |
| READINESS_NOT_MET | BlockedReason（只用于 Task） | Task 的需求或依赖闭包不就绪（REQ-031、REQ-009） |

### 3. REQ-018 的配置键表（O2）

`contracts/requirements.json` 的 REQ-018，`decisions` 末尾追加一条（旧：无；REQ-018 的“完整配置键表（未知键一律拒绝）”由 CR-003、CR-007 给出，本条只增加一张表）：

> （CR-026）[assumptions] 表：键为决定类名（ASCII 小写字母、数字与下划线），值为字符串或字符串数组，是无人值守时允许假设的默认值（REQ-030，所有者 2026-11-17 的决定 O2）；表中键的个数与名称由项目自定，表外的未知键照旧拒绝。

### 4. 各需求的细化

每项给出替换后的整个需求对象与它的全部场景。AC 的 id 与 TEST 的 id 按 validate_bundle 的规则依次编号；每项原有的 AC-xxx-01 保留 id、改写为可判定的内容（原文的覆盖关系见“不变的部分”）。

#### REQ-034 文档事实源（4 条 AC）

`contracts/requirements.json` 中 REQ-034 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-034-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-034",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "文档事实源",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "文件是语义事实源，DB是索引/状态；冲突可检测；生成页不产生第二权威源",
  "behavior": [
    "用户项目的规格是 Git 工作树中规格根 docs/spec/ 下的 Markdown 文件（O4）：固定一级标题（用户、范围、非目标、决策、开放问题、需求），需求为“## REQ-<id> <标题>”，验收为“### AC-<req>-<nn>”并带 given、when、then、check 四个 id 行；精确语法由 M3-T02 的 ADR 规定，HoH 内置模板给出（REQ-029）。",
    "规格摘要是规格根内全部规格文件（docs/spec/*.md，不含生成的索引）按路径排序的“<路径> <sha256>”行的 sha256；specs 行的 sha256 恒为它所在修订的规格摘要。",
    "DB 只是索引与状态：已批准修订的 requirements、acceptance_criteria 行由 HoH 从该修订的规格文件解析写入；运行时只读索引，任何派发、冻结与验收都引用 specs 行与其摘要，不读工作树中尚未批准的文件。",
    "修订只经批准产生（REQ-008，O1）：已批准修订是经 ApproveSpec 批准、sha256 为该修订规格摘要的 specs 行，只有已批准修订被采用、派发、冻结与验收引用；ApproveSpec 的 spec_sha256 必须等于提交时工作树的规格摘要，不等即冲突，什么也不写。",
    "索引在批准时从规格文件构建；文件无法解析时批准返回 422 SPEC_INVALID，索引构建失败，保留上一已批准修订的索引与快照，写 spec.index_failed{path, line, error}，派发依据仍是上一已批准修订，禁止基于半更新的索引派发；修好并重新批准后重建。",
    "生成页（docs/spec/index.json 等由规格派生的文件）带“生成，勿改”标记与来源规格摘要；它们不是事实源，任何读取以规格文件为准；生成时发现生成页被人工修改，写 spec.generated_drift{path} 并按规格重新生成。",
    "冻结与验证从 specs 行对应的规格文件取规格，替换 M2 的约定路径 contracts/requirements.json（ADR-028 决定 4，CR-018）。"
  ],
  "failure_modes": [
    "ApproveSpec 的 spec_sha256 与提交时工作树的规格摘要不同时，返回 409 SPEC_CONFLICT，details 含 expected 与 actual 两个摘要，specs、requirements、acceptance_criteria 与 runs 都不变。",
    "规格文件无法解析时，索引不变（仍为上一已批准修订），写 spec.index_failed，不派发依据未批准内容的任务。",
    "生成页被人工修改时，规格与索引不变，下一次生成写 spec.generated_drift 并覆盖生成页。",
    "规格根不存在或为空时，项目视为没有规格（StartRun 以 runtime:no_spec 阻塞，ADR-031 决定 1），不读任何约定路径。"
  ],
  "doc_refs": [
    "docs/05-data-api.md",
    "docs/21-spec-governance.md",
    "docs/15-prompts-skills-memory.md",
    "docs/adr/ADR-028.md",
    "docs/adr/ADR-031.md",
    "contracts/sql/001_initial.sql"
  ],
  "acceptance": [
    {
      "id": "AC-034-01",
      "given": "夹具项目 docs/spec/ 下两份规格文件共 3 条 REQ、7 条 AC，提交于 C1；用户以 ApproveSpec{spec_sha256=该规格摘要, decision:APPROVE_ONLY} 批准",
      "when": "批准后冻结一个候选",
      "then": "specs 中有 revision=1 的已批准修订，sha256 等于按“<路径> <sha256>”行计算的规格摘要；requirements 3 行、acceptance_criteria 7 行且 id 与文件一致；candidate.frozen 引用该 spec_id 与摘要；冻结读取的规格路径在 docs/spec/ 下，进程未打开 contracts/requirements.json",
      "test_id": "TEST-034-01"
    },
    {
      "id": "AC-034-02",
      "given": "已批准修订 1（摘要 S1）；用户把工作树的规格改为摘要 S2，未提交批准",
      "when": "以 ApproveSpec{spec_sha256=S1} 提交批准",
      "then": "返回 409，code=SPEC_CONFLICT，details 含 expected=S1 与 actual=S2；specs、requirements、acceptance_criteria 与 runs 行逐值不变；工作树文件不变",
      "test_id": "TEST-034-02"
    },
    {
      "id": "AC-034-03",
      "given": "已批准修订 1 的 Run R 处于 RUNNING、有 READY 的 Task；用户把一份规格文件改坏（AC 缺 then 行）后批准",
      "when": "调度轮再运行三轮，随后修好文件并再次批准",
      "then": "坏文件的批准返回 422 SPEC_INVALID、写 spec.index_failed{path, line, error}，没有新的已批准修订，三轮内派发依据仍是修订 1；修好并批准后有修订 2 的已批准修订与重建的索引",
      "test_id": "TEST-034-03"
    },
    {
      "id": "AC-034-04",
      "given": "已批准修订 1，HoH 生成了 docs/spec/index.json（含“生成，勿改”标记与来源摘要）",
      "when": "用户手工改 index.json 中一条 AC 的文字，随后触发生成",
      "then": "specs、requirements、acceptance_criteria 不变；生成时写 spec.generated_drift{path}，index.json 恢复为按规格生成的内容并带来源摘要",
      "test_id": "TEST-034-04"
    }
  ],
  "decisions": [
    "规格格式与位置取所有者 2026-11-17 的决定 O4：Markdown 放 docs/spec/，固定标题与 id 行，HoH 生成 JSON 索引（CR-026）。",
    "冲突以新码 SPEC_CONFLICT（409）报告，不复用 CONFLICT：后者指命令 id 冲突，含义不同（CR-026，C1）。",
    "生成页漂移时以规格为准覆盖并记录，不尝试合并人工修改（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-034 的场景（旧：TEST-034-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-034-01",
    "requirement_id": "REQ-034",
    "acceptance_id": "AC-034-01",
    "check_id": "integration",
    "fixture": "临时 git 项目（docs/spec/ 两份规格文件）+ 产品 host + ApproveSpec + 冻结",
    "steps": [
      "提交规格文件",
      "ApproveSpec{spec_sha256=规格摘要, decision:APPROVE_ONLY}",
      "冻结候选",
      "记录冻结进程打开的文件"
    ],
    "pass_criteria": [
      "specs 1 行且 sha256 等于独立计算的规格摘要",
      "requirements 3 行、acceptance_criteria 7 行，id 与文件一致",
      "candidate.frozen 的 spec 引用为该行",
      "未打开 contracts/requirements.json"
    ],
    "observables": [
      "specs/requirements/acceptance_criteria 导出",
      "candidate.frozen 载荷",
      "冻结进程的文件访问记录",
      "判定结果"
    ]
  },
  {
    "id": "TEST-034-02",
    "requirement_id": "REQ-034",
    "acceptance_id": "AC-034-02",
    "check_id": "integration",
    "fixture": "临时 git 项目 + 产品 host，已批准修订 1",
    "steps": [
      "改工作树规格为摘要 S2",
      "ApproveSpec{spec_sha256=S1}"
    ],
    "pass_criteria": [
      "HTTP 409 且 code=SPEC_CONFLICT，details 含两个摘要",
      "四张表逐值不变",
      "工作树文件不变"
    ],
    "observables": [
      "HTTP 状态与错误体",
      "表导出前后对比",
      "文件摘要"
    ]
  },
  {
    "id": "TEST-034-03",
    "requirement_id": "REQ-034",
    "acceptance_id": "AC-034-03",
    "check_id": "integration",
    "fixture": "临时 git 项目 + 产品 host + FakeAgent，Run R RUNNING、Task READY",
    "steps": [
      "改坏一份规格文件并批准",
      "运行三轮调度",
      "修好并再次批准"
    ],
    "pass_criteria": [
      "批准返回 422 SPEC_INVALID",
      "spec.index_failed 一条且载荷含 path、line、error",
      "坏文件批准后没有新的已批准修订",
      "三轮派发依据的 spec_id 为修订 1",
      "修好后修订 2 与新索引"
    ],
    "observables": [
      "events导出",
      "specs 与索引行",
      "调度轮派发记录",
      "判定结果"
    ]
  },
  {
    "id": "TEST-034-04",
    "requirement_id": "REQ-034",
    "acceptance_id": "AC-034-04",
    "check_id": "unit",
    "fixture": "规格生成器 + 临时目录",
    "steps": [
      "按修订 1 生成 index.json",
      "手工改一条 AC 文字",
      "再次生成"
    ],
    "pass_criteria": [
      "规格与索引不变",
      "spec.generated_drift 一条",
      "index.json 恢复且带来源摘要与标记"
    ],
    "observables": [
      "生成文件内容",
      "漂移诊断",
      "判定结果"
    ]
  }
]
```

#### REQ-033 规格变更（4 条 AC）

`contracts/requirements.json` 中 REQ-033 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-033-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-033",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "规格变更",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "生成 CR、revision、影响闭包；受影响验收过期，不静默沿用旧PASS",
  "behavior": [
    "规格变更都走 CR：ProposeSpecChange（或 CHANGE 输入，REQ-017）以当前已批准修订为 base，写一条 change_requests 行（PROPOSED），reason 为提议理由，impact_json 列出影响闭包；批准之前规格、Run 的 spec_id 与任务都不变。",
    "影响闭包按显式映射计算：被改动的 REQ（增删改）→ 其 AC → 其 TEST/check → 以 task_requirements 关联的 Task；不按文件路径猜测（C2）。",
    "批准（ApproveSpec，只由用户，O1）写新修订（revision+1）、CR 为 APPLIED、result_spec_id 指向新行，写 spec.changed{cause:\"changed\", from, to, cr_id}；REJECT 把 CR 置为 REJECTED，什么也不改。",
    "任何修订都使该 Run 全部验收结果失效（O6）：新修订下没有任何 accepted，直到在新冻结的候选上重新验收；旧结果保留为历史，显示为 STALE，不计入当前进度与门禁。",
    "验证进行中规格变化时，验证中的候选写 candidate.stale{candidate_id, reason:\"spec_changed\"}，其轮次以 superseded 关闭，验证 Task 取消，独立报告翻转为 STALE（REQ-053 行为 7，ADR-028 决定 11）。",
    "影响闭包内、以 SPEC_MISMATCH 阻塞的写 Task，批准后迁移：仍有对应 REQ 的，task_requirements 改映射到新修订的 REQ 并写 task.migrated；对应 REQ 被删除的，Task 取消、由 Commander 计划的替代 Task 承接并写 task.replaced{from, to}。任何 Task 都不再在旧修订下派发。"
  ],
  "failure_modes": [
    "批准前，规格、runs.spec_id、tasks 与 acceptance_results 不变。",
    "以修订 n 的证据判修订 n+1 的 AC 时，投影与完成门禁（EVIDENCE_GATE_UNMET）都不计为 accepted，显示 STALE；已验收计数的分母随修订变化并显示 spec revision。",
    "CR 的 base 不是当前已批准修订时（期间已有别的批准），ApproveSpec 返回 409 SPEC_CONFLICT，CR 保持 PROPOSED。",
    "被删 REQ 的写 Task 没有替代时，它保持 CANCELLED，不在旧修订下重新派发。"
  ],
  "doc_refs": [
    "docs/21-spec-governance.md",
    "docs/07-algorithms.md",
    "docs/06-orchestration.md",
    "docs/03-ux.md",
    "docs/adr/ADR-028.md",
    "docs/10-acceptance.md",
    "contracts/sql/001_initial.sql"
  ],
  "acceptance": [
    {
      "id": "AC-033-01",
      "given": "已批准修订 1 含 REQ-A（AC-A-01、AC-A-02，各有 TEST 并由 Task TA 关联）与 REQ-B；用户把 REQ-A 的 AC-A-02 改写",
      "when": "以 ProposeSpecChange{reason, spec_sha256=新摘要} 提议，查看 CR，然后以 ApproveSpec{decision:APPROVE_ONLY} 批准",
      "then": "批准前 change_requests 一行 PROPOSED、base_spec_id=修订 1、impact_json 恰含 REQ-A、AC-A-01..02、它们的 TEST 与 TA，不含 REQ-B 的任何项，specs 与 runs.spec_id 不变；批准后 specs 新增修订 2、CR 为 APPLIED 且 result_spec_id=修订 2、写 spec.changed{cause:\"changed\", from:1, to:2}",
      "test_id": "TEST-033-01"
    },
    {
      "id": "AC-033-02",
      "given": "Run R 的候选 C 在修订 1 下 VERIFYING，QA 与 Review 的验证 Task 运行中",
      "when": "批准修订 2",
      "then": "同一事务写 candidate.stale{candidate_id:C, reason:\"spec_changed\"}，C 的轮次以 superseded 关闭，两个验证 Task 为 CANCELLED，C 的独立报告为 STALE；修订 2 下 accepted AC 数为 0",
      "test_id": "TEST-033-02"
    },
    {
      "id": "AC-033-03",
      "given": "修订 1 下 REQ-A、REQ-B 的全部 AC 已 PASS 并 accepted；批准了只改 REQ-A 的修订 2",
      "when": "读 /v1/runs/{id}/acceptance 与 Run 的投影，随后 Commander 以 transition_run 申请 COMPLETED",
      "then": "两者都显示修订 2 的全部 AC（含未改动的 REQ-B）为未验收，旧结果标 STALE 并可查；已验收计数为 0/修订 2 的 AC 总数并显示 spec revision 2；完成申请被拒（EVIDENCE_GATE_UNMET），Run 不是 COMPLETED",
      "test_id": "TEST-033-03"
    },
    {
      "id": "AC-033-04",
      "given": "修订 1 下写 Task T1（REQ-A）与 T2（REQ-C）以 SPEC_MISMATCH 阻塞；修订 2 改写 REQ-A、删除 REQ-C",
      "when": "批准修订 2，Commander 为 REQ-C 的去留计划替代 Task T3，调度轮运行",
      "then": "T1 的 task_requirements 改指修订 2 的 REQ-A 并写 task.migrated，T1 可派发；T2 为 CANCELLED 并写 task.replaced{from:T2, to:T3}；之后没有任何 Task 以修订 1 的 spec_id 派发",
      "test_id": "TEST-033-04"
    }
  ],
  "decisions": [
    "证据失效的粒度取所有者 2026-11-17 的决定 O6：任何修订使全部验收失效；影响闭包只决定要做的工作，不决定保留哪些 PASS；验收在新冻结的候选上重跑全部 required 检查（与 docs/07 §6 的发布全量一致）（CR-026）。",
    "影响闭包按 REQ→AC→TEST→TASK 的显式映射，不按路径（CR-026，C2）。",
    "CR 的记录是 change_requests 行；提议的规格内容就是工作树中的规格文件，批准时以摘要锁定，不另写 CR 文件（CR-026，C3）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-033 的场景（旧：TEST-033-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-033-01",
    "requirement_id": "REQ-033",
    "acceptance_id": "AC-033-01",
    "check_id": "integration",
    "fixture": "临时 git 项目 + 产品 host，修订 1 含 REQ-A/REQ-B 与关联 Task",
    "steps": [
      "改写 AC-A-02 并 ProposeSpecChange",
      "读 change_requests",
      "ApproveSpec{decision:APPROVE_ONLY}"
    ],
    "pass_criteria": [
      "批准前 CR 为 PROPOSED，impact_json 恰为 REQ-A 的闭包",
      "批准前 specs 与 runs.spec_id 不变",
      "批准后修订 2、CR APPLIED、spec.changed{cause:changed}"
    ],
    "observables": [
      "change_requests 行",
      "specs 行",
      "events导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-033-02",
    "requirement_id": "REQ-033",
    "acceptance_id": "AC-033-02",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent 验证者，候选 C VERIFYING",
    "steps": [
      "批准修订 2"
    ],
    "pass_criteria": [
      "candidate.stale{reason:spec_changed} 一条",
      "轮次 superseded、两个验证 Task CANCELLED",
      "报告 STALE",
      "修订 2 下 accepted 为 0"
    ],
    "observables": [
      "events导出",
      "rounds/tasks/independent_reports 行",
      "acceptance_results",
      "判定结果"
    ]
  },
  {
    "id": "TEST-033-03",
    "requirement_id": "REQ-033",
    "acceptance_id": "AC-033-03",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander：修订 1 全部 accepted 后批准修订 2",
    "steps": [
      "读 acceptance API 与投影",
      "Commander 申请 COMPLETED"
    ],
    "pass_criteria": [
      "修订 2 的全部 AC 未验收、旧结果 STALE",
      "计数为 0/总数并显示 revision 2",
      "完成申请被拒（EVIDENCE_GATE_UNMET），Run 不是 COMPLETED"
    ],
    "observables": [
      "acceptance API 响应",
      "投影",
      "decisions 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-033-04",
    "requirement_id": "REQ-033",
    "acceptance_id": "AC-033-04",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander，T1/T2 以 SPEC_MISMATCH 阻塞",
    "steps": [
      "批准修订 2（改 REQ-A、删 REQ-C）",
      "Commander 计划 T3",
      "运行调度轮"
    ],
    "pass_criteria": [
      "T1 映射到修订 2 并 task.migrated，可派发",
      "T2 CANCELLED 并 task.replaced{from:T2,to:T3}",
      "无修订 1 的派发"
    ],
    "observables": [
      "tasks 与 task_requirements 行",
      "events导出",
      "调度轮派发记录",
      "判定结果"
    ]
  }
]
```

#### REQ-031 Readiness Gate（4 条 AC）

`contracts/requirements.json` 中 REQ-031 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-031-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-031",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "Readiness Gate",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "必要 AC 可观察、依赖可达、关键决定确定；仅阻断当前受影响范围",
  "behavior": [
    "Readiness 是已批准规格修订与当前计划的纯函数，由 Runtime 自动判定（C5），结果为 READY 或 NOT_READY{reasons}；Commander 不能覆盖判定结果。",
    "不就绪的原因：范围内 required REQ 没有 AC（missing_ac）；AC 缺 then 或 check（unobservable）；计划的依赖不存在或成环（DAG_MISSING_DEPENDENCY、DAG_CYCLE）；开放问题中标为阻塞的未决（undecided，标记方式见 C4）。",
    "只阻断受影响的范围：一个 Task 的 readiness 只看它的 REQ 及其依赖闭包；不就绪的 Task 为 BLOCKED（READINESS_NOT_MET，details 列出原因与对应 id），其他就绪的 Task 照常派发，Run 不因此整体停下。",
    "标为非阻塞的开放问题不阻断 readiness，列入 Run 的开放问题清单（投影与 Timeline 可查）。",
    "Run 只在 readiness 通过后从 SPECIFYING 进 READY，并写 readiness.passed{spec_revision}；规格修订变化时对受影响范围重新判定（REQ-033）。"
  ],
  "failure_modes": [
    "范围内 required REQ 缺 AC 时，该 REQ 的 Task 为 BLOCKED（READINESS_NOT_MET，details.reasons 含 missing_ac 与 REQ id），没有该 Task 的 Attempt。",
    "计划依赖成环时，计划被拒（DAG_CYCLE），不导入任何 Task。",
    "Commander 在 readiness 未过时提出 transition_run 到 READY，决策 REJECTED（ILLEGAL_TRANSITION，details 写 readiness 原因），Run 不变。"
  ],
  "doc_refs": [
    "docs/06-orchestration.md",
    "docs/07-algorithms.md",
    "docs/adr/ADR-017.md",
    "docs/adr/ADR-031.md"
  ],
  "acceptance": [
    {
      "id": "AC-031-01",
      "given": "五个夹具（规格加计划）：齐全；required REQ 缺 AC；AC 缺 then；依赖成环；开放问题中一条阻塞问题未决",
      "when": "对每个夹具调用 readiness 判定",
      "then": "齐全的为 READY；其余依次为 NOT_READY 且 reasons 恰为 missing_ac、unobservable、DAG_CYCLE、undecided，并各自给出对应的 REQ、AC 或问题 id",
      "test_id": "TEST-031-01"
    },
    {
      "id": "AC-031-02",
      "given": "已批准修订中需求组 A（齐全）与 B（一条 required REQ 缺 AC），计划含 A 的 Task TA 与 B 的 Task TB，Run 处于 RUNNING",
      "when": "运行三轮调度",
      "then": "TA 被派发；TB 为 BLOCKED、blocked_reason=READINESS_NOT_MET 且 details.reasons 含 missing_ac 与该 REQ id，TB 没有 Attempt；Run 仍为 RUNNING",
      "test_id": "TEST-031-02"
    },
    {
      "id": "AC-031-03",
      "given": "规格的开放问题中有一条标为“阻塞：否”的未决问题，其余齐全",
      "when": "调用 readiness 判定并读 Run 的投影",
      "then": "判定为 READY；该问题出现在 Run 的开放问题清单中，带其 id 与“非阻塞”标记",
      "test_id": "TEST-031-03"
    },
    {
      "id": "AC-031-04",
      "given": "Run R 处于 SPECIFYING，已批准修订 1 不就绪（missing_ac）",
      "when": "Commander 提出 transition_run 到 READY；随后用户批准补齐 AC 的修订 2",
      "then": "第一次决策 REJECTED（ILLEGAL_TRANSITION，details 含 missing_ac），R 仍为 SPECIFYING；修订 2 批准后 Runtime 写 readiness.passed{spec_revision:2}，R 进入 READY",
      "test_id": "TEST-031-04"
    }
  ],
  "decisions": [
    "readiness 由 Runtime 自动判定，Commander 只能补规格或计划（CR-026，C5）。",
    "阻塞性的标记写在规格的开放问题条目里（每条带“阻塞：是/否”），缺省为阻塞（CR-026，C4）。",
    "新码 READINESS_NOT_MET 为 BlockedReason（Task 的 blocked_reason）（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-031 的场景（旧：TEST-031-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-031-01",
    "requirement_id": "REQ-031",
    "acceptance_id": "AC-031-01",
    "check_id": "unit",
    "fixture": "hoh-core readiness 纯函数 + 五个规格与计划夹具",
    "steps": [
      "对五个夹具各判定一次"
    ],
    "pass_criteria": [
      "齐全者 READY",
      "其余四个的 reasons 恰为 missing_ac、unobservable、DAG_CYCLE、undecided 并含对应 id"
    ],
    "observables": [
      "判定返回值",
      "夹具清单",
      "判定结果"
    ]
  },
  {
    "id": "TEST-031-02",
    "requirement_id": "REQ-031",
    "acceptance_id": "AC-031-02",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent，修订含 A（齐全）与 B（缺 AC）",
    "steps": [
      "运行三轮调度"
    ],
    "pass_criteria": [
      "TA 被派发",
      "TB BLOCKED 且 READINESS_NOT_MET，details 含 missing_ac 与 REQ id",
      "TB 无 Attempt",
      "Run 仍 RUNNING"
    ],
    "observables": [
      "tasks 与 attempts 行",
      "调度轮派发记录",
      "events导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-031-03",
    "requirement_id": "REQ-031",
    "acceptance_id": "AC-031-03",
    "check_id": "unit",
    "fixture": "readiness 纯函数 + Run 投影的开放问题清单",
    "steps": [
      "判定一次",
      "读投影"
    ],
    "pass_criteria": [
      "READY",
      "清单含该问题 id 与非阻塞标记"
    ],
    "observables": [
      "判定返回值",
      "投影",
      "判定结果"
    ]
  },
  {
    "id": "TEST-031-04",
    "requirement_id": "REQ-031",
    "acceptance_id": "AC-031-04",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander，Run SPECIFYING",
    "steps": [
      "Commander 提出到 READY",
      "用户批准修订 2"
    ],
    "pass_criteria": [
      "首个决策 REJECTED 且 details 含 missing_ac",
      "readiness.passed{spec_revision:2} 后 Run 为 READY"
    ],
    "observables": [
      "decisions 行",
      "events导出",
      "runs 行",
      "判定结果"
    ]
  }
]
```

#### REQ-030 规格收敛（3 条 AC）

`contracts/requirements.json` 中 REQ-030 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-030-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-030",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "规格收敛",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "记录用户/范围/非目标/决策/开放问题；必要不确定性不伪造用户答案",
  "behavior": [
    "规格记录五类内容（REQ-034 的固定标题）：用户、范围、非目标、决策、开放问题；每类非空或明写“无”。",
    "每条决策注明来源：用户答复（question_id）、用户的交互输入（input_id），或假设（record_assumption 的事件 id）；没有来源的决策不允许。",
    "交互运行中，需要用户选择时以 request_user_decision 提问并等待答复，答复按 question_id 记录，不被记成假设。",
    "无人值守（-p）时只能假设项目配置 [assumptions] 中列出的默认值（O2）：每个假设以 record_assumption 记录，basis 写配置键；产品方向、范围、非目标与未列出的必需决定一律不假设，Run 以 NEED_DECISION 阻塞并持久化问题。",
    "规格送批（ApproveSpec）前 Runtime 校验来源：标为用户决定而在 DB 中找不到对应 AnswerQuestion 或交互输入的条目，使批准被拒（SPEC_INVALID，details 列出无来源的条目）。",
    "假设台账是事件的投影（record_assumption 的应用事件），不另建表（C6）。"
  ],
  "failure_modes": [
    "无人值守遇到产品方向这类关键未知时，Run=BLOCKED(NEED_DECISION)，问题持久化，-p 以 3 退出并列出问题，不写任何假设。",
    "把假设写成用户决定的规格被拒（SPEC_INVALID），specs 不变。",
    "配置 [assumptions] 未列出的决定类，无人值守时不假设，照样阻塞。"
  ],
  "doc_refs": [
    "docs/06-orchestration.md",
    "docs/15-prompts-skills-memory.md",
    "docs/01-prd.md",
    "docs/21-spec-governance.md",
    "config/hoh.example.toml",
    "docs/adr/ADR-031.md"
  ],
  "acceptance": [
    {
      "id": "AC-030-01",
      "given": "交互运行，目标“做一个记账工具”，FakeCommander 脚本提出一个范围问题，用户答复其选项",
      "when": "规格起草完成并送批",
      "then": "规格五个标题下各有内容或“无”；决策中来自答复的条目引用其 question_id，DB 中有对应的 AnswerQuestion 事件；批准通过",
      "test_id": "TEST-030-01"
    },
    {
      "id": "AC-030-02",
      "given": "无人值守（-p），项目配置 [assumptions] 列出 language=\"Rust\"；目标未说明语言，也未说明产品方向",
      "when": "运行至停止",
      "then": "语言以 record_assumption 记录且 basis 含 assumptions.language；产品方向未被假设：Run=BLOCKED(NEED_DECISION)、问题持久化、-p 以 3 退出并列出该问题；没有新的已批准修订",
      "test_id": "TEST-030-02"
    },
    {
      "id": "AC-030-03",
      "given": "送批的规格中一条决策标为用户决定，引用的 question_id 在 DB 中没有答复（实为假设）",
      "when": "以 ApproveSpec 批准",
      "then": "返回 422，code=SPEC_INVALID，details 列出该条目及其缺失的来源；没有新的已批准修订",
      "test_id": "TEST-030-03"
    }
  ],
  "decisions": [
    "无人值守可假设的范围取所有者 2026-11-17 的决定 O2：只有项目配置中列出的默认值；产品方向、范围与非目标一律阻塞（CR-026）。",
    "批准只由用户（O1），无人值守的 Run 停在批准之前（CR-026）。",
    "假设台账由事件投影，不新增表（CR-026，C6）；五节模板属 REQ-029 的模板（C7）。",
    "新码 SPEC_INVALID（ErrorCode，422）报告送批规格的结构或来源问题（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-030 的场景（旧：TEST-030-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-030-01",
    "requirement_id": "REQ-030",
    "acceptance_id": "AC-030-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander 脚本 + 用户答复（交互）",
    "steps": [
      "提交目标",
      "答复范围问题",
      "送批"
    ],
    "pass_criteria": [
      "五节齐全",
      "答复来源的决策引用 question_id 且 DB 有 AnswerQuestion",
      "批准通过"
    ],
    "observables": [
      "规格文件",
      "events导出",
      "specs 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-030-02",
    "requirement_id": "REQ-030",
    "acceptance_id": "AC-030-02",
    "check_id": "integration",
    "fixture": "hoh -p + 产品 host + FakeCommander，配置含 [assumptions] language",
    "steps": [
      "hoh -p 运行至停止"
    ],
    "pass_criteria": [
      "record_assumption 的 basis 含 assumptions.language",
      "Run BLOCKED(NEED_DECISION) 且问题持久化",
      "退出码 3 并列出问题",
      "没有新的已批准修订"
    ],
    "observables": [
      "events导出",
      "runs 行",
      "退出码与 stdout",
      "判定结果"
    ]
  },
  {
    "id": "TEST-030-03",
    "requirement_id": "REQ-030",
    "acceptance_id": "AC-030-03",
    "check_id": "integration",
    "fixture": "产品 host，送批规格含无来源的用户决定",
    "steps": [
      "ApproveSpec"
    ],
    "pass_criteria": [
      "HTTP 422 且 code=SPEC_INVALID，details 列出该条目",
      "没有新的已批准修订"
    ],
    "observables": [
      "HTTP 状态与错误体",
      "specs 行",
      "判定结果"
    ]
  }
]
```

#### REQ-032 需求追踪（3 条 AC）

`contracts/requirements.json` 中 REQ-032 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-032-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-032",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "需求追踪",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "REQ→TASK→candidate→TEST→EVIDENCE→acceptance 可双向查找",
  "behavior": [
    "产品为用户项目维护追踪链 REQ→AC→TASK→candidate→TEST→EVIDENCE→acceptance 结果，任一节点可双向查到完整链（C8：产品功能；本仓库的 traceability 工具以仿本校验产品的导出）。",
    "断链报为 broken 并指出缺哪一环：TEST 没有证据、证据没有候选、AC 没有 TEST、Task 没有 REQ。",
    "孤立验收报为 orphan：PASS 绑定的不是当前候选或当前规格修订。",
    "API 与 DB 一致：/v1/runs/{id}/acceptance 为每条 AC 给出候选、TEST 与证据引用，/v1/runs/{id}/tasks 为每个 Task 给出 REQ；产品不另造第二份追踪事实。",
    "规格修订变化后，受影响链的节点标为 stale（与 REQ-033 一致），历史可查，不静默沿用。"
  ],
  "failure_modes": [
    "证据被删或缺候选时，追踪报告列为 broken 并指明缺失环节，门禁不计该 AC 为 accepted。",
    "AC 的 PASS 绑定旧候选或旧修订时，报为 orphan，不计入当前进度。"
  ],
  "doc_refs": [
    "docs/10-acceptance.md",
    "docs/05-data-api.md",
    "docs/adr/ADR-027.md",
    "docs/adr/ADR-029.md",
    "docs/cr/CR-017-same-content-refreeze-in-one-run.md"
  ],
  "acceptance": [
    {
      "id": "AC-032-01",
      "given": "夹具项目的追踪导出：2 条 REQ、4 条 AC、3 个 Task、1 个候选；其中一条 TEST 没有证据，一条 AC 的 PASS 绑定旧候选",
      "when": "以 traceability 检查运行 tests/bundle 中名称含 test_032_01 的测试，读取产品的追踪报告",
      "then": "任一节点（REQ、AC、Task、候选、TEST、证据、结果）都能正反两个方向查到完整链；没有证据的 TEST 列在 broken 中并指出缺 evidence；绑定旧候选的 PASS 列在 orphan 中；其余链完整",
      "test_id": "TEST-032-01"
    },
    {
      "id": "AC-032-02",
      "given": "产品 host 的 Run 有 3 个 Task 与 4 条 AC 的验证结果",
      "when": "读取 /v1/runs/{id}/acceptance 与 /v1/runs/{id}/tasks 的全部分页",
      "then": "每条 AC 的候选、TEST 与证据引用，以及每个 Task 的 REQ，与直接查询 DB 的结果逐项相等",
      "test_id": "TEST-032-02"
    },
    {
      "id": "AC-032-03",
      "given": "修订 1 的追踪链完整；批准改写一条 REQ 的修订 2",
      "when": "再次导出追踪报告",
      "then": "该 REQ 链上的节点标为 stale，历史结果仍可查；修订 2 下没有节点沿用修订 1 的 PASS",
      "test_id": "TEST-032-03"
    }
  ],
  "decisions": [
    "REQ-032 是产品功能；TEST-032-01 的测试在 tests/bundle、名称含 test_032_01，以 traceability 检查校验产品导出的追踪报告（CR-017，CR-026，C8）。",
    "M3 的追踪入口是 API（Page 的 items 为任意对象，ADR-029）与 TUI 的状态摘要；Web 视图属 M6（CR-026，C9）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-032 的场景（旧：TEST-032-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-032-01",
    "requirement_id": "REQ-032",
    "acceptance_id": "AC-032-01",
    "check_id": "traceability",
    "fixture": "tests/bundle 的 test_032_01 + 产品追踪导出夹具",
    "steps": [
      "导出追踪报告",
      "逐节点正反查询",
      "检查 broken 与 orphan"
    ],
    "pass_criteria": [
      "每个节点正反可达",
      "无证据的 TEST 在 broken 中且指出缺 evidence",
      "绑定旧候选的 PASS 在 orphan 中"
    ],
    "observables": [
      "追踪报告",
      "test-results.jsonl",
      "判定结果"
    ]
  },
  {
    "id": "TEST-032-02",
    "requirement_id": "REQ-032",
    "acceptance_id": "AC-032-02",
    "check_id": "integration",
    "fixture": "产品 host + 有结果的 Run",
    "steps": [
      "读两个 API 的全部分页",
      "直接查 DB"
    ],
    "pass_criteria": [
      "两份清单逐项相等"
    ],
    "observables": [
      "API 响应",
      "DB 查询结果",
      "判定结果"
    ]
  },
  {
    "id": "TEST-032-03",
    "requirement_id": "REQ-032",
    "acceptance_id": "AC-032-03",
    "check_id": "integration",
    "fixture": "产品 host，批准修订 2",
    "steps": [
      "导出追踪报告"
    ],
    "pass_criteria": [
      "受影响节点 stale",
      "历史可查",
      "无沿用修订 1 的 PASS"
    ],
    "observables": [
      "追踪报告",
      "acceptance_results",
      "判定结果"
    ]
  }
]
```

#### REQ-028 项目发现（4 条 AC）

`contracts/requirements.json` 中 REQ-028 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-028-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-028",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "项目发现",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "建立基线，保留脏文件，不重写架构、不强制初始化远程",
  "behavior": [
    "Run 的 DISCOVERING 阶段做项目发现：记录 run.baseline{head, tree, dirty:[{path, sha256}], untracked:[{path, sha256}], build_commands, test_commands, docs}；基线随 Run 记录（C11）。",
    "发现只读：不改写、不 stash、不 reset 工作树，不新建提交，不改远程配置；git status --porcelain、脏文件与未跟踪文件的摘要在发现前后逐字相同。",
    "构建与测试命令按常见约定检测（Cargo.toml、package.json 的 scripts、Makefile、pyproject.toml），检测不到记 unknown（C10）；发现不自动运行项目的构建与测试（O3）；用户确认后运行一次，结果按命令记入基线，之后的回归以基线为对照。",
    "写 Task 的范围含有用户未提交修改的文件时，HoH 不碰这些文件：Task 为 BLOCKED（NEED_DECISION），问题列出这些文件（O3）；这些文件的摘要在 Run 全程不变。",
    "Greenfield（空目录或非 git 目录）：只在本地 git init，不加远程、不推送；首个提交只含 HoH 写的规格或脚手架；已是 git 仓库时不重复初始化。",
    "已有文档按常见位置自动发现（README、docs/、ADR 目录），列入基线的 docs。",
    "不重写架构：发现不创建任何写 Task；Brownfield 的新功能只补必要规格，计划中的 Task 只能引用用户目标范围内已批准的 REQ（REQ-031、REQ-009），不生成重构既有架构的任务。"
  ],
  "failure_modes": [
    "发现过程中任何写操作都不发生：git status --porcelain 与文件摘要不变，git remote -v 不变，提交数不变。",
    "写 Task 涉及脏文件时，Task=BLOCKED(NEED_DECISION)，问题列出文件路径，文件摘要不变。",
    "基线已失败的测试不被计为 HoH 引入的回归。"
  ],
  "doc_refs": [
    "docs/01-prd.md",
    "docs/05-data-api.md",
    "docs/12-user-guide.md",
    "docs/adr/ADR-021.md"
  ],
  "acceptance": [
    {
      "id": "AC-028-01",
      "given": "夹具 git 仓库：有代码、测试、README 与 docs/，两个已修改文件、一个未跟踪文件，一个远程",
      "when": "以新 Run 执行项目发现",
      "then": "写 run.baseline，含 head、tree、两个脏文件与一个未跟踪文件的路径与 sha256、检测到的 build_commands 与 test_commands、README 与 docs/；发现前后 git status --porcelain 逐字相同、脏与未跟踪文件的摘要不变、提交数与 git remote -v 不变；发现期间没有创建写 Task",
      "test_id": "TEST-028-01"
    },
    {
      "id": "AC-028-02",
      "given": "基线检测到测试命令 cargo test，其中一个测试本来失败；用户确认运行测试",
      "when": "发现运行测试一次，随后一个候选使同一测试仍失败",
      "then": "基线按命令记录 pass/fail 且含该失败测试；该候选的回归判定不把它算作 HoH 引入的回归；用户确认前没有运行任何项目命令",
      "test_id": "TEST-028-02"
    },
    {
      "id": "AC-028-03",
      "given": "夹具仓库中文件 F 有用户未提交的修改；计划中写 Task T 的范围含 F",
      "when": "调度轮到 T",
      "then": "T 为 BLOCKED(NEED_DECISION)，问题列出 F；没有 T 的写 Attempt；F 的 sha256 在 Run 全程不变，没有 stash 或 reset",
      "test_id": "TEST-028-03"
    },
    {
      "id": "AC-028-04",
      "given": "一个空目录与一个已有提交的 git 仓库",
      "when": "各启动一个 Run 直到首次提交",
      "then": "空目录被本地 git init，没有远程，首个提交只含 HoH 写的规格或脚手架文件；已有仓库没有被重新初始化，提交历史保留",
      "test_id": "TEST-028-04"
    }
  ],
  "decisions": [
    "已有仓库的做法取所有者 2026-11-17 的决定 O3：碰到未提交文件先阻塞询问；发现不自动运行构建与测试（CR-026）。",
    "基线随 Run 记录（run.baseline），不同 Run 可以有不同的基线（CR-026，C11）。",
    "命令检测按常见约定，检测不到为 unknown，不猜测（CR-026，C10）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-028 的场景（旧：TEST-028-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-028-01",
    "requirement_id": "REQ-028",
    "acceptance_id": "AC-028-01",
    "check_id": "integration",
    "fixture": "夹具 git 仓库（脏文件、未跟踪文件、远程）+ 产品 host",
    "steps": [
      "记录 git 状态与文件摘要",
      "执行发现",
      "再次记录"
    ],
    "pass_criteria": [
      "run.baseline 字段齐全且列出脏与未跟踪文件",
      "前后 git 状态、摘要、提交数与 remote 逐字相同",
      "发现期间无写 Task"
    ],
    "observables": [
      "events导出",
      "git 状态快照",
      "文件摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-028-02",
    "requirement_id": "REQ-028",
    "acceptance_id": "AC-028-02",
    "check_id": "integration",
    "fixture": "夹具仓库（一个失败测试）+ 产品 host + 用户确认",
    "steps": [
      "确认前检查命令运行记录",
      "确认后运行测试",
      "候选回归判定"
    ],
    "pass_criteria": [
      "确认前无项目命令运行",
      "基线记录 pass/fail 含失败测试",
      "回归判定不含该测试"
    ],
    "observables": [
      "run.baseline",
      "命令运行记录",
      "回归判定",
      "判定结果"
    ]
  },
  {
    "id": "TEST-028-03",
    "requirement_id": "REQ-028",
    "acceptance_id": "AC-028-03",
    "check_id": "integration",
    "fixture": "夹具仓库（F 有未提交修改）+ 产品 host + FakeCommander 计划",
    "steps": [
      "运行调度轮至 T"
    ],
    "pass_criteria": [
      "T BLOCKED(NEED_DECISION) 且问题列出 F",
      "无 T 的写 Attempt",
      "F 摘要全程不变"
    ],
    "observables": [
      "tasks 与 attempts 行",
      "问题",
      "F 的摘要序列",
      "判定结果"
    ]
  },
  {
    "id": "TEST-028-04",
    "requirement_id": "REQ-028",
    "acceptance_id": "AC-028-04",
    "check_id": "integration",
    "fixture": "空目录 + 已有 git 仓库 + 产品 host",
    "steps": [
      "各启动 Run 至首次提交"
    ],
    "pass_criteria": [
      "空目录本地初始化且无 remote",
      "首个提交只含 HoH 文件",
      "已有仓库未重新初始化"
    ],
    "observables": [
      "git 配置与提交清单",
      "remote",
      "判定结果"
    ]
  }
]
```

#### REQ-045 四层Prompt版本（4 条 AC）

`contracts/requirements.json` 中 REQ-045 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-045-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-045",
  "category": "PROMPT",
  "priority": "P0",
  "milestone": "M3",
  "title": "四层Prompt版本",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "合成并记录版本/内容hash；高层覆盖不改变事实证据；过长报错",
  "behavior": [
    "每个 Attempt 的 Prompt 由四层合成：职位契约（含覆盖链）、项目层（已批准规格的摘要与引用、ADR 与约定的引用）、任务层、证据层（docs/15）。",
    "每个 Attempt 记录合成文本的 sha256、每层的来源引用与版本、resource_bundle_version、截断策略；按记录重放得到逐字节相同的文本。",
    "高层覆盖只作用于职位契约与项目约定层；证据层逐字节等于事实收集的结果，任何覆盖都不能改写证据层；越层的覆盖被拒并报告冲突。",
    "模板只能使用白名单变量 GOAL、PROJECT_CONTEXT、TASK_CONTEXT、EVIDENCE_CONTEXT（C13）；引用其他变量时合成失败为 PROMPT_VARIABLE_MISSING，details 名出变量，Attempt 不启动。",
    "覆盖使职位契约超过 32 KiB（UTF-8 字节，C12）时，保存覆盖即以 PROMPT_TOO_LONG 拒绝，details 给出尺寸与上限；强制项装不下时仍为 CONTEXT_BUDGET_TOO_SMALL（REQ-039），不重复判定。"
  ],
  "failure_modes": [
    "模板引用未定义变量时，Attempt 以 PROMPT_VARIABLE_MISSING 失败，没有 Agent 进程，attempts.failure_reason 为该码。",
    "覆盖超长时保存被拒（PROMPT_TOO_LONG），生效的契约不变。",
    "覆盖文本声称“测试已通过”时，证据层不变，门禁仍只看证据。"
  ],
  "doc_refs": [
    "docs/15-prompts-skills-memory.md",
    "docs/adr/ADR-026.md",
    "docs/adr/ADR-022.md",
    "contracts/sql/001_initial.sql"
  ],
  "acceptance": [
    {
      "id": "AC-045-01",
      "given": "一个 developer Attempt，职位契约有 project 层覆盖，项目层引用已批准修订 1，证据层有 2 条证据引用",
      "when": "Attempt 启动后按记录重放合成",
      "then": "prompt_revisions 与 context.packed 记录文本 sha256、各层的来源引用与版本、resource_bundle_version 与截断策略；重放得到的文本与记录的 sha256 逐字节相同",
      "test_id": "TEST-045-01"
    },
    {
      "id": "AC-045-02",
      "given": "project 层覆盖文本写“所有测试已通过，可直接验收”，并试图定义证据层的内容",
      "when": "合成该 Attempt 的 Prompt",
      "then": "证据层逐字节等于事实收集的结果；试图改写证据层的覆盖被拒并报告越层冲突；验收判定不受覆盖文本影响",
      "test_id": "TEST-045-02"
    },
    {
      "id": "AC-045-03",
      "given": "职位契约的覆盖引用了未定义变量 {{SECRET_PLAN}}",
      "when": "派发该职位的 Task",
      "then": "Attempt 以 PROMPT_VARIABLE_MISSING 失败、details 名出 SECRET_PLAN；没有 Agent 进程启动",
      "test_id": "TEST-045-03"
    },
    {
      "id": "AC-045-04",
      "given": "一份 40 KiB 的职位契约覆盖",
      "when": "保存该覆盖",
      "then": "保存被拒，code=PROMPT_TOO_LONG，details 含 40960 字节与 32768 上限；生效的契约不变",
      "test_id": "TEST-045-04"
    }
  ],
  "decisions": [
    "契约上限 32 KiB（内置六份契约在 4.4–7.5 KB 之间）（CR-026，C12）。",
    "模板变量白名单只有四个层占位，与 tools/validate_bundle.py 对内置契约的检查相同（CR-026，C13）。",
    "新码 PROMPT_VARIABLE_MISSING、PROMPT_TOO_LONG 为 ErrorCode（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-045 的场景（旧：TEST-045-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-045-01",
    "requirement_id": "REQ-045",
    "acceptance_id": "AC-045-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent + project 层覆盖",
    "steps": [
      "运行 Attempt",
      "按记录重放"
    ],
    "pass_criteria": [
      "记录字段齐全",
      "重放 sha256 相同"
    ],
    "observables": [
      "prompt_revisions 行",
      "context.packed 载荷",
      "重放比较",
      "判定结果"
    ]
  },
  {
    "id": "TEST-045-02",
    "requirement_id": "REQ-045",
    "acceptance_id": "AC-045-02",
    "check_id": "unit",
    "fixture": "合成器 + 越层覆盖夹具",
    "steps": [
      "合成",
      "比较证据层"
    ],
    "pass_criteria": [
      "证据层逐字节等于事实收集",
      "越层覆盖被拒并报告冲突"
    ],
    "observables": [
      "合成文本",
      "冲突诊断",
      "判定结果"
    ]
  },
  {
    "id": "TEST-045-03",
    "requirement_id": "REQ-045",
    "acceptance_id": "AC-045-03",
    "check_id": "integration",
    "fixture": "产品 host + 含未定义变量的覆盖",
    "steps": [
      "派发 Task"
    ],
    "pass_criteria": [
      "Attempt 失败为 PROMPT_VARIABLE_MISSING 且 details 名出变量",
      "无 Agent 进程"
    ],
    "observables": [
      "attempts 行",
      "进程记录",
      "判定结果"
    ]
  },
  {
    "id": "TEST-045-04",
    "requirement_id": "REQ-045",
    "acceptance_id": "AC-045-04",
    "check_id": "unit",
    "fixture": "覆盖保存校验",
    "steps": [
      "保存 40 KiB 覆盖"
    ],
    "pass_criteria": [
      "拒绝为 PROMPT_TOO_LONG 并给出尺寸与上限",
      "生效契约不变"
    ],
    "observables": [
      "保存结果与错误体",
      "判定结果"
    ]
  }
]
```

#### REQ-047 自定义与升级（3 条 AC）

`contracts/requirements.json` 中 REQ-047 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-047-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-047",
  "category": "PROMPT",
  "priority": "P0",
  "milestone": "M3",
  "title": "自定义与升级",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "内置/global/project/run按序；显示diff；运行锁定旧版本直到checkpoint",
  "behavior": [
    "资源（职位契约、Skill、模板）的覆盖按 builtin→global→project→run 的顺序生效，后者优先；global 层在 $XDG_CONFIG_HOME/hoh/（缺省 ~/.config/hoh/），project 层在 .hoh/，run 层随 Run 的快照（C15）。",
    "每个 Attempt 记录覆盖链：各层的路径与内容摘要（prompt_revisions 的 layer 与 parent_revision）。",
    "用户可查看 builtin 与生效版本的统一 diff，“恢复默认”删除该层覆盖；任何编辑都不改安装的资源文件。",
    "资源包升级只给新 Attempt：Run 已开始的职位在其下一个 checkpoint（ADR-022 决定 8，C14）之前仍用旧版本；每个 Attempt 记其 resource_bundle_version。",
    "基于旧内置版本的覆盖在新版本内置已变时标为漂移，给出三方 diff（旧内置、新内置、覆盖）。"
  ],
  "failure_modes": [
    "删除 run 层覆盖后，下一个 Attempt 落到 project 层，覆盖链随之变化。",
    "编辑与恢复默认之后，安装目录的资源文件摘要不变。",
    "升级后在 checkpoint 之前的 Attempt 仍记旧的 resource_bundle_version。"
  ],
  "doc_refs": [
    "docs/15-prompts-skills-memory.md",
    "docs/03-ux.md",
    "docs/12-user-guide.md",
    "docs/adr/ADR-022.md"
  ],
  "acceptance": [
    {
      "id": "AC-047-01",
      "given": "developer 契约在 global、project、run 三层都有覆盖",
      "when": "运行一个 Attempt；删掉 run 层覆盖后再运行一个",
      "then": "第一个 Attempt 用 run 层，覆盖链依次记录 builtin、global、project、run 的路径与摘要；第二个用 project 层，链中没有 run 层",
      "test_id": "TEST-047-01"
    },
    {
      "id": "AC-047-02",
      "given": "developer 契约有 project 层覆盖",
      "when": "通过 API 取 diff，随后执行恢复默认",
      "then": "diff 为 builtin 与生效版本的统一 diff；恢复默认后 project 层覆盖文件被删除，下一 Attempt 用 builtin；安装目录中资源文件的 sha256 在全程不变",
      "test_id": "TEST-047-02"
    },
    {
      "id": "AC-047-03",
      "given": "Run R 以资源包 v1 开始，developer 契约有基于 v1 的 project 层覆盖",
      "when": "R 运行中把 HoH 升级到 developer 内置契约已变的 v2",
      "then": "R 的 developer 在下一个 checkpoint 之前的 Attempt 记 resource_bundle_version=v1，之后记 v2；该覆盖被标为漂移并给出三方 diff",
      "test_id": "TEST-047-03"
    }
  ],
  "decisions": [
    "global 层位置为 $XDG_CONFIG_HOME/hoh/（CR-026，C15）。",
    "资源升级的生效点取 ADR-022 决定 8 的职位 checkpoint，与团队变更一致（CR-026，C14）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-047 的场景（旧：TEST-047-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-047-01",
    "requirement_id": "REQ-047",
    "acceptance_id": "AC-047-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent + 三层覆盖（临时 XDG_CONFIG_HOME）",
    "steps": [
      "运行 Attempt",
      "删 run 层",
      "再运行"
    ],
    "pass_criteria": [
      "第一个用 run 层且链有四层",
      "第二个用 project 层且链无 run 层"
    ],
    "observables": [
      "prompt_revisions 行",
      "合成文本摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-047-02",
    "requirement_id": "REQ-047",
    "acceptance_id": "AC-047-02",
    "check_id": "integration",
    "fixture": "产品 host + project 层覆盖 + 安装目录",
    "steps": [
      "取 diff",
      "恢复默认",
      "运行下一 Attempt"
    ],
    "pass_criteria": [
      "统一 diff 正确",
      "覆盖文件删除且下一 Attempt 用 builtin",
      "安装文件摘要不变"
    ],
    "observables": [
      "diff 文本",
      "覆盖文件",
      "安装文件摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-047-03",
    "requirement_id": "REQ-047",
    "acceptance_id": "AC-047-03",
    "check_id": "deployment",
    "fixture": "安装 v1 + 运行中升级到 v2 的发布包",
    "steps": [
      "v1 下开始 Run",
      "升级到 v2",
      "跨 checkpoint 运行"
    ],
    "pass_criteria": [
      "checkpoint 前 v1、之后 v2",
      "覆盖标漂移并给出三方 diff"
    ],
    "observables": [
      "attempts.resource_bundle_version",
      "漂移报告",
      "判定结果"
    ]
  }
]
```

#### REQ-029 内置模板（3 条 AC）

`contracts/requirements.json` 中 REQ-029 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-029-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-029",
  "category": "SPEC",
  "priority": "P0",
  "milestone": "M3",
  "title": "内置模板",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "产品/需求/功能/UX/架构/开发/测试/验收/部署/使用规范齐全；按规模选用",
  "behavior": [
    "内置模板覆盖产品、需求、功能、UX、架构、开发、测试、验收、部署与使用规范，按 lite、standard、full 三档组织（resources/templates/profiles.json）。",
    "模板随产品分发（嵌入二进制，计入 resource_bundle_version，C17）；无网络、无外部工具也能实例化；hoh doctor 列出模板包版本。",
    "实例化填实全部必填占位（不残留占位语法），并写 docs/21 的元数据：id、title、revision、status、owners、related_requirements、source_refs。",
    "project.document_profile 指定时用指定档；未指定时用 standard，并记录选择与理由（C16）。",
    "改档时只补缺的文档，不覆盖用户已编辑的文件。",
    "规格模板含 REQ-034 的固定标题（用户、范围、非目标、决策、开放问题、需求），供 REQ-030 的五类内容使用（C7）。"
  ],
  "failure_modes": [
    "实例化结果残留占位语法或缺元数据字段时，判为失败并列出文件与位置。",
    "改档时已编辑的文件内容不变。",
    "离线环境实例化不访问网络。"
  ],
  "doc_refs": [
    "docs/21-spec-governance.md",
    "docs/15-prompts-skills-memory.md",
    "docs/00-product-definition.md",
    "config/hoh.example.toml",
    "resources/templates/profiles.json"
  ],
  "acceptance": [
    {
      "id": "AC-029-01",
      "given": "三个空的夹具项目，project.document_profile 分别为 lite、standard、full",
      "when": "各实例化一次",
      "then": "每档所列模板都生成；生成文件中没有残留的占位语法；每份都有 id、title、revision、status、owners、related_requirements、source_refs",
      "test_id": "TEST-029-01"
    },
    {
      "id": "AC-029-02",
      "given": "两个项目：一个未指定 document_profile，一个先以 lite 实例化后用户编辑了其中一份文件，再把档改为 full",
      "when": "各实例化一次",
      "then": "未指定的用 standard，并记录选择与理由；改档的只新增 full 比 lite 多出的文档，已编辑文件的 sha256 不变",
      "test_id": "TEST-029-02"
    },
    {
      "id": "AC-029-03",
      "given": "安装的发布包，测试环境不提供网络",
      "when": "执行 hoh doctor 并在新项目实例化 standard 档",
      "then": "doctor 输出列出模板包版本；实例化成功，期间没有任何对外网络连接（connect 的目标只有本地地址）",
      "test_id": "TEST-029-03"
    }
  ],
  "decisions": [
    "未指定档时用 standard：可判定，不依据目标文本猜测规模（CR-026，C16）。",
    "模板嵌入二进制，与职位契约、schema 同一资源包（CR-026，C17）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-029 的场景（旧：TEST-029-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-029-01",
    "requirement_id": "REQ-029",
    "acceptance_id": "AC-029-01",
    "check_id": "unit",
    "fixture": "三个临时项目 + 三档实例化",
    "steps": [
      "实例化",
      "扫描占位",
      "校验元数据"
    ],
    "pass_criteria": [
      "每档模板齐全",
      "无残留占位",
      "元数据字段齐全"
    ],
    "observables": [
      "生成文件清单",
      "占位扫描结果",
      "判定结果"
    ]
  },
  {
    "id": "TEST-029-02",
    "requirement_id": "REQ-029",
    "acceptance_id": "AC-029-02",
    "check_id": "integration",
    "fixture": "两个临时项目（未指定档；lite 后改 full）",
    "steps": [
      "实例化",
      "比较已编辑文件摘要"
    ],
    "pass_criteria": [
      "未指定用 standard 并记录理由",
      "只补缺的文档",
      "已编辑文件不变"
    ],
    "observables": [
      "生成清单",
      "选择记录",
      "文件摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-029-03",
    "requirement_id": "REQ-029",
    "acceptance_id": "AC-029-03",
    "check_id": "deployment",
    "fixture": "发布包安装 + strace 记录网络系统调用",
    "steps": [
      "hoh doctor",
      "在 strace 下实例化 standard"
    ],
    "pass_criteria": [
      "doctor 列出模板包版本",
      "实例化成功",
      "connect 的目标只有本地地址"
    ],
    "observables": [
      "doctor 输出",
      "生成清单",
      "strace 的 connect 记录",
      "判定结果"
    ]
  }
]
```

#### REQ-046 内置Skill（3 条 AC）

`contracts/requirements.json` 中 REQ-046 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-046-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-046",
  "category": "PROMPT",
  "priority": "P0",
  "milestone": "M3",
  "title": "内置Skill",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "内置可用无需用户手装；只加载相关技能；原生Skill不可用时内容注入",
  "behavior": [
    "13 个内置 Skill（resources/skills/registry.json）随产品分发，不要求用户安装，也不写进原生 Agent 的用户目录（O5）。",
    "每个 Attempt 收到 Skill 短索引，只含 roles 含该职位、phases 含当前阶段的 Skill（ADR-026 的短索引）。",
    "Skill 正文按阶段与任务注入（C18）：任务的计划条目或当前阶段要求的 Skill 正文作为知识类内容注入，计入 REQ-039 的知识份额，并记入 prompt 修订；已在原生会话目录中的不重复注入。",
    "注册校验：每个 Skill 有最小字段（docs/15）；依赖闭包可解析且无环；entry 存在；未知 phase 被拒。项目层自定义 Skill 照此校验，不能注册调度器或绕过门禁。"
  ],
  "failure_modes": [
    "没有网络与用户安装的 Skill 时，内置 Skill 照常可用。",
    "registry 依赖成环、缺 entry 或 phase 未知时，注册被拒并给出原因，生效的 Skill 集不变。",
    "Skill 不写入 ~/.claude、pi 扩展目录等原生用户目录；这些目录在 Run 前后的文件摘要不变。"
  ],
  "doc_refs": [
    "docs/15-prompts-skills-memory.md",
    "docs/adr/ADR-026.md",
    "docs/23-pre-m1-audit.md",
    "resources/skills/registry.json"
  ],
  "acceptance": [
    {
      "id": "AC-046-01",
      "given": "无网络，没有任何用户安装的 Skill；实现阶段的 developer Task 要求 tdd",
      "when": "运行该 Attempt，读假 Agent 记下的输入",
      "then": "短索引只含 roles 含 developer 且 phases 含实现阶段的 Skill；输入含 tdd 的正文；prompt 修订记录 tdd 的来源与版本",
      "test_id": "TEST-046-01"
    },
    {
      "id": "AC-046-02",
      "given": "绑定的 Agent 没有原生 Skill 机制；另一个 Agent 的会话目录中已有 tdd",
      "when": "各运行一个要求 tdd 的 Attempt",
      "then": "无原生机制的输入以知识类内容注入 tdd 正文并计入知识份额；会话目录已有 tdd 的输入不重复注入；运行前后 ~/.claude 与 pi 扩展目录的文件摘要不变",
      "test_id": "TEST-046-02"
    },
    {
      "id": "AC-046-03",
      "given": "四份 registry 夹具：合法；依赖成环；entry 缺失；phase 未知；另一份项目层 Skill 声称注册调度器",
      "when": "逐一注册",
      "then": "合法的通过；其余各被拒并给出原因（环成员、缺失路径、未知 phase、不允许的能力），生效的 Skill 集不变",
      "test_id": "TEST-046-03"
    }
  ],
  "decisions": [
    "Skill 的落点取所有者 2026-11-17 的决定 O5：不写原生 Agent 的用户目录，只注入或写进会话自己的目录（CR-026）。",
    "正文注入按阶段与任务，不由 Agent 自行请求（CR-026，C18）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-046 的场景（旧：TEST-046-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-046-01",
    "requirement_id": "REQ-046",
    "acceptance_id": "AC-046-01",
    "check_id": "integration",
    "fixture": "产品 host + 假 Agent（记录输入）+ 无网络、无用户 Skill",
    "steps": [
      "运行要求 tdd 的 developer Attempt",
      "读输入记录"
    ],
    "pass_criteria": [
      "短索引按职位与阶段过滤",
      "输入含 tdd 正文",
      "prompt 修订记录来源"
    ],
    "observables": [
      "假 Agent 输入",
      "prompt_revisions",
      "判定结果"
    ]
  },
  {
    "id": "TEST-046-02",
    "requirement_id": "REQ-046",
    "acceptance_id": "AC-046-02",
    "check_id": "integration",
    "fixture": "两个假 Agent（无原生机制；会话目录已有 tdd）+ 临时 HOME",
    "steps": [
      "各运行一个 Attempt",
      "比较原生目录摘要"
    ],
    "pass_criteria": [
      "注入计入知识份额",
      "不重复注入",
      "原生目录摘要不变"
    ],
    "observables": [
      "假 Agent 输入",
      "context.packed",
      "目录摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-046-03",
    "requirement_id": "REQ-046",
    "acceptance_id": "AC-046-03",
    "check_id": "unit",
    "fixture": "registry 校验 + 五份夹具",
    "steps": [
      "逐一注册"
    ],
    "pass_criteria": [
      "合法通过",
      "其余各被拒并给出原因",
      "生效集不变"
    ],
    "observables": [
      "校验结果",
      "判定结果"
    ]
  }
]
```

#### REQ-007 Slash 与统一操作（3 条 AC）

`contracts/requirements.json` 中 REQ-007 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-007-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-007",
  "category": "UX",
  "priority": "P0",
  "milestone": "M3",
  "title": "Slash 与统一操作",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "两种入口得到同一 SetMode 行为；Shell 不需要对称子命令",
  "behavior": [
    "TUI 的 /plan、/build、/auto 与 Shift+Tab 都提交同一 SetMode 命令（CommandService 的规范化意图）；HTTP 的 SetMode 得到形状相同的事件；Shell 没有对称子命令（CONTRACT 第 3 条）。",
    "Shift+Tab 按 PLAN→BUILD→AUTO→PLAN 循环（C21）；附着 Run 时作用于该 Run，未附着时只改下一个由 TUI 启动的 Run 的模式（C20）。",
    "模式变更在安全点生效：有运行中的写 Attempt 时先写 run.mode_change_pending{from, to}、不再派发新的写 Attempt，该 Attempt 到终态后写 run.mode_changed{from, to}；没有时立即写 run.mode_changed。",
    "/ 打开模糊选择器，列出高频命令（/plan /build /auto /team /model /status /resume /help）；未知命令显示“未知命令”且不提交任何命令。",
    "/plan 等 HoH 命令永不转给底座 Agent；转给底座的斜杠须显式写目标（/agent <role> <text>，C19），没有目标时不转。"
  ],
  "failure_modes": [
    "未知斜杠命令不产生命令、事件或 Agent 输入。",
    "运行中切换时，待生效期间没有新的写派发。",
    "SetMode 的 expected_revision 过期时返回 409 STALE_REVISION，模式不变。"
  ],
  "doc_refs": [
    "docs/03-ux.md",
    "docs/12-user-guide.md",
    "CONTRACT.md",
    "docs/adr/ADR-031.md"
  ],
  "acceptance": [
    {
      "id": "AC-007-01",
      "given": "TUI（PTY）附着一个 AUTO 的 Run，另一个客户端经 HTTP 可提交命令",
      "when": "TUI 中依次输入 /plan、按两次 Shift+Tab，随后经 HTTP 提交 SetMode{mode:PLAN}",
      "then": "四次各产生恰好一条 SetMode 命令与一条 run.mode_changed{from, to}，依次为 AUTO→PLAN、PLAN→BUILD、BUILD→AUTO、AUTO→PLAN；/plan 与 HTTP 的两条命令除 id 外 payload 相同；顶栏显示当前模式",
      "test_id": "TEST-007-01"
    },
    {
      "id": "AC-007-02",
      "given": "BUILD 的 Run 中一个写 Attempt 正在运行，另有 READY 的写 Task",
      "when": "TUI 中输入 /plan，随后该 Attempt 结束",
      "then": "/plan 返回 202 并写 run.mode_change_pending{from:BUILD, to:PLAN}，状态栏显示待生效；Attempt 结束前没有新的写派发；结束后写 run.mode_changed 且 runs.mode=PLAN",
      "test_id": "TEST-007-02"
    },
    {
      "id": "AC-007-03",
      "given": "TUI 附着 Run，绑定的 Agent 为假 Agent（记录输入）",
      "when": "输入 /，再输入 /foo，再输入 /plan，最后输入 /agent developer /compact",
      "then": "/ 列出八个高频命令；/foo 显示“未知命令”且没有命令提交；/plan 不出现在任何 Agent 输入中；/agent 的文本以 developer 为目标投递，未写目标的转发不发生",
      "test_id": "TEST-007-03"
    }
  ],
  "decisions": [
    "Shift+Tab 的顺序 PLAN→BUILD→AUTO（CR-026，C21）。",
    "未附着时 /plan 不改项目默认（project.default_mode），只改下一个 Run（CR-026，C20）。",
    "转给底座的语法为 /agent <role> <text>（CR-026，C19）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-007 的场景（旧：TEST-007-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-007-01",
    "requirement_id": "REQ-007",
    "acceptance_id": "AC-007-01",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI + 产品 host + HTTP 客户端",
    "steps": [
      "/plan",
      "两次 Shift+Tab",
      "HTTP SetMode"
    ],
    "pass_criteria": [
      "四条 SetMode 与四条 run.mode_changed 依次为 AUTO→PLAN、PLAN→BUILD、BUILD→AUTO、AUTO→PLAN",
      "/plan 与 HTTP 的 payload 相同",
      "顶栏模式正确"
    ],
    "observables": [
      "命令记录",
      "events导出",
      "屏幕快照",
      "判定结果"
    ]
  },
  {
    "id": "TEST-007-02",
    "requirement_id": "REQ-007",
    "acceptance_id": "AC-007-02",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent 写 Attempt 运行中",
    "steps": [
      "提交 /plan 对应的 SetMode",
      "结束 Attempt"
    ],
    "pass_criteria": [
      "202 且 run.mode_change_pending",
      "期间无写派发",
      "结束后 run.mode_changed 且 runs.mode=PLAN"
    ],
    "observables": [
      "events导出",
      "调度轮派发记录",
      "runs 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-007-03",
    "requirement_id": "REQ-007",
    "acceptance_id": "AC-007-03",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI + 假 Agent 输入记录",
    "steps": [
      "/",
      "/foo",
      "/plan",
      "/agent developer /compact"
    ],
    "pass_criteria": [
      "选择器列八个命令",
      "/foo 无命令",
      "/plan 不进 Agent 输入",
      "/agent 以 developer 为目标"
    ],
    "observables": [
      "屏幕快照",
      "命令记录",
      "假 Agent 输入",
      "判定结果"
    ]
  }
]
```

#### REQ-008 Plan 互动与文档（4 条 AC）

`contracts/requirements.json` 中 REQ-008 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-008-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-008",
  "category": "UX",
  "priority": "P0",
  "milestone": "M3",
  "title": "Plan 互动与文档",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "产生版本化规格，允许提问，未批准实施不修改产品源码",
  "behavior": [
    "PLAN 中对话产出规格：cto 职位的写 Task 只写规格根 docs/spec/、docs/ 与 .hoh/；不派发写产品源文件的 Task，产品源文件在批准前不变（CONTRACT 第 4 条）。",
    "规格只经用户批准成为修订（O1）：项目的首个规格经 ApproveSpec 成为修订 1（sha256 为批准时的规格摘要），写 spec.changed{cause:\"approved\"}；之后的修订经 CR（REQ-033），旧修订保留；HoH 的任何职位都不能批准。",
    "没有已批准规格的 Run 停在 runtime:no_spec，规格批准后以 adopt 答复即采用最新修订（ADR-031 决定 1）。",
    "Plan 中允许提问：Commander 的 request_user_decision 成为 question.asked，TUI 显示选项；答复前 Run 不越过 SPECIFYING；答复按 question_id 记录，不被记成假设。",
    "未批准不实施：ProposeSpecChange 未批准时 Run 的 spec_id 不变；切到 BUILD 也无写派发（readiness 未过，REQ-031），既有阻塞不被清空。",
    "Plan 起草规格的写 Task 不以已批准规格为前提；它们引用的 specs 行与锚定方式由 M3-T02 的 ADR 按 CR-026 的 C22 决定，不改受锁的 SQL；起草 Task 之外，批准前不派发任何写 Task。"
  ],
  "failure_modes": [
    "PLAN 的 Run 中出现写产品源文件的派发或产品源文件变化时，判为失败（写集核对以 git 为准，不依 Agent 自述）。",
    "HoH 职位试图批准规格时被拒，specs 不变。",
    "答复前 Run 停在 SPECIFYING，不进入 READY。"
  ],
  "doc_refs": [
    "CONTRACT.md",
    "docs/03-ux.md",
    "docs/06-orchestration.md",
    "docs/15-prompts-skills-memory.md",
    "docs/adr/ADR-031.md"
  ],
  "acceptance": [
    {
      "id": "AC-008-01",
      "given": "PLAN 的 Run，FakeCommander 与 cto 的假 Agent 脚本起草规格，用户经 TUI 交互答复",
      "when": "从 StartRun 运行到用户批准",
      "then": "git 记录的写集只含 docs/spec/、docs/ 与 .hoh/ 下的路径；没有写产品源文件的派发；产品源文件的摘要不变",
      "test_id": "TEST-008-01"
    },
    {
      "id": "AC-008-02",
      "given": "项目没有已批准规格，Run R2 以 runtime:no_spec 阻塞；用户在 docs/spec/ 写好规格并提交",
      "when": "用户以 ApproveSpec{decision:APPROVE_ONLY} 批准，随后以 adopt 答复 R2 的问题",
      "then": "specs 中有 revision=1、sha256 为批准时规格摘要的已批准修订，写 spec.changed{cause:\"approved\"}；R2 采用修订 1 并离开 BLOCKED",
      "test_id": "TEST-008-02"
    },
    {
      "id": "AC-008-03",
      "given": "PLAN 的 Run 在 SPECIFYING，Commander 发出 request_user_decision（两个选项）",
      "when": "TUI 显示后等待 10 s，随后用户选择第二个选项",
      "then": "TUI 显示问题与两个选项；等待期间 Run 仍为 SPECIFYING；答复后 AnswerQuestion 事件带 question_id 与 choice，假设台账中没有该条",
      "test_id": "TEST-008-03"
    },
    {
      "id": "AC-008-04",
      "given": "PLAN 的 Run 有一个未批准的 ProposeSpecChange 与一个 NEED_DECISION 阻塞",
      "when": "用户执行 /build",
      "then": "Run 的 spec_id 不变；没有写派发；阻塞集合仍含原阻塞",
      "test_id": "TEST-008-04"
    }
  ],
  "decisions": [
    "批准权取所有者 2026-11-17 的决定 O1：只有用户批准规格（CR-026）。",
    "Plan 的写集为 docs/spec/、docs/ 与 .hoh/；原型工件也只能放在 docs/ 下（CR-026，C23）。",
    "无已批准规格时的起草依据见 CR-026 的 C22 决定。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-008 的场景（旧：TEST-008-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-008-01",
    "requirement_id": "REQ-008",
    "acceptance_id": "AC-008-01",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI + 产品 host + FakeCommander + cto 假 Agent（交互答复）",
    "steps": [
      "StartRun(PLAN)",
      "交互答复",
      "批准",
      "比较 git 写集与文件摘要"
    ],
    "pass_criteria": [
      "写集只含 docs/spec/、docs/、.hoh/",
      "无写产品源文件的派发",
      "产品源文件摘要不变"
    ],
    "observables": [
      "git diff 路径集合",
      "attempts 行",
      "文件摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-008-02",
    "requirement_id": "REQ-008",
    "acceptance_id": "AC-008-02",
    "check_id": "integration",
    "fixture": "产品 host，项目无已批准规格，R2 以 no_spec 阻塞",
    "steps": [
      "提交规格文件",
      "ApproveSpec{decision:APPROVE_ONLY}",
      "以 adopt 答复 R2"
    ],
    "pass_criteria": [
      "已批准修订 1 且 sha256 为规格摘要",
      "spec.changed{cause:approved}",
      "R2 采用修订 1 并离开 BLOCKED"
    ],
    "observables": [
      "specs 行",
      "events导出",
      "runs 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-008-03",
    "requirement_id": "REQ-008",
    "acceptance_id": "AC-008-03",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI + FakeCommander 提问",
    "steps": [
      "显示问题",
      "等待 10 s",
      "选第二项"
    ],
    "pass_criteria": [
      "TUI 显示问题与选项",
      "等待期间 SPECIFYING",
      "答复记录 question_id 与 choice",
      "台账无该条"
    ],
    "observables": [
      "屏幕快照",
      "events导出",
      "runs 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-008-04",
    "requirement_id": "REQ-008",
    "acceptance_id": "AC-008-04",
    "check_id": "integration",
    "fixture": "产品 host，PLAN 的 Run 有未批准提议与阻塞",
    "steps": [
      "SetMode{mode:BUILD}"
    ],
    "pass_criteria": [
      "spec_id 不变",
      "无写派发",
      "阻塞集合含原阻塞"
    ],
    "observables": [
      "runs 行",
      "调度轮派发记录",
      "阻塞集合",
      "判定结果"
    ]
  }
]
```

#### REQ-009 Build 与 Auto（3 条 AC）

`contracts/requirements.json` 中 REQ-009 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-009-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-009",
  "category": "UX",
  "priority": "P0",
  "milestone": "M3",
  "title": "Build 与 Auto",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "Build 遵守既定规格；Auto 可补规划；缺必需规格均不越过 readiness",
  "behavior": [
    "BUILD 遵守既定规格：Commander 的 create_tasks 只能引用当前已批准修订中的 REQ；BUILD 中不起草规格，需要改规格时提示用户切到 PLAN 或提 CR。",
    "AUTO 可补规划：规格缺必需项时 Commander 可安排起草补充（cto 的写 Task，写集同 PLAN），但补充只经用户批准生效（O1），批准前无写产品源文件的派发。",
    "BUILD 在缺必需规格时不自行补规格，相关 Task 为 BLOCKED（READINESS_NOT_MET）并在问题中提示可切到 AUTO 或 PLAN（C24）。",
    "模式与生命周期正交：PAUSED、BLOCKED 中可改模式，恢复时按新模式；PLAN→BUILD 只在规格已批准且 readiness 已过时才有写派发；BUILD/AUTO→PLAN 在安全点生效（REQ-007）。"
  ],
  "failure_modes": [
    "BUILD 中引用未批准 REQ 的 create_tasks 被拒（ILLEGAL_TRANSITION，details 写“BUILD 不改规格，用 /plan 或 CR”），task_requirements 不变。",
    "AUTO 的补充未批准时，没有写产品源文件的派发。",
    "无人值守的 AUTO 缺规格时停在批准前（NEED_DECISION），不自批。"
  ],
  "doc_refs": [
    "docs/03-ux.md",
    "docs/06-orchestration.md",
    "CONTRACT.md",
    "docs/adr/ADR-031.md"
  ],
  "acceptance": [
    {
      "id": "AC-009-01",
      "given": "BUILD 的 Run，已批准修订含 REQ-A；Commander 提出 create_tasks，一个 Task 引用 REQ-A，另一个引用不存在于修订的 REQ-Z",
      "when": "Runtime 处理该决策",
      "then": "决策 REJECTED（ILLEGAL_TRANSITION，details 含“BUILD 不改规格，用 /plan 或 CR”与 REQ-Z）；没有导入任何 Task，task_requirements 不变",
      "test_id": "TEST-009-01"
    },
    {
      "id": "AC-009-02",
      "given": "两个 Run 在同一缺 AC 的已批准修订上，一个 AUTO、一个 BUILD",
      "when": "两者各运行至停止派发",
      "then": "AUTO 的 Run 派发 cto 起草补充的写 Task 并以问题请用户批准，批准前没有写产品源文件的派发；BUILD 的 Run 不起草，相关 Task 为 BLOCKED(READINESS_NOT_MET) 且问题提示可切到 AUTO 或 PLAN",
      "test_id": "TEST-009-02"
    },
    {
      "id": "AC-009-03",
      "given": "模式状态机的全部 (状态, 模式, 目标模式) 组合表",
      "when": "逐项调用模式迁移判定",
      "then": "与表逐项一致：PAUSED 与 BLOCKED 中可改模式且恢复后按新模式；PLAN→BUILD 只在规格已批准且 readiness 已过时允许写派发；BUILD/AUTO→PLAN 有运行中写 Attempt 时为待生效",
      "test_id": "TEST-009-03"
    }
  ],
  "decisions": [
    "BUILD 缺规格时阻塞而不自动转 AUTO（CR-026，C24）。",
    "AUTO 的补充规格同样只由用户批准（O1）（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-009 的场景（旧：TEST-009-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-009-01",
    "requirement_id": "REQ-009",
    "acceptance_id": "AC-009-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander，BUILD 的 Run",
    "steps": [
      "提交含 REQ-Z 的 create_tasks"
    ],
    "pass_criteria": [
      "决策 REJECTED 且 details 含提示与 REQ-Z",
      "无 Task 导入",
      "task_requirements 不变"
    ],
    "observables": [
      "decisions 行",
      "tasks 与 task_requirements 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-009-02",
    "requirement_id": "REQ-009",
    "acceptance_id": "AC-009-02",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander + cto 假 Agent，同一缺 AC 的修订",
    "steps": [
      "AUTO 与 BUILD 各运行至停止派发"
    ],
    "pass_criteria": [
      "AUTO 派发起草 Task 并请求批准，批准前无产品写派发",
      "BUILD 的 Task BLOCKED(READINESS_NOT_MET) 且问题提示切换"
    ],
    "observables": [
      "attempts 与 tasks 行",
      "问题",
      "events导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-009-03",
    "requirement_id": "REQ-009",
    "acceptance_id": "AC-009-03",
    "check_id": "unit",
    "fixture": "hoh-core 模式迁移判定 + 组合表",
    "steps": [
      "逐项判定"
    ],
    "pass_criteria": [
      "与表逐项一致"
    ],
    "observables": [
      "判定表",
      "判定结果"
    ]
  }
]
```

#### REQ-017 运行中对话（5 条 AC）

`contracts/requirements.json` 中 REQ-017 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-017-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-017",
  "category": "UX",
  "priority": "P0",
  "milestone": "M3",
  "title": "运行中对话",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "Ask 不改任务；steer 有生效边界；CR 改规格；控制可独立于模型执行",
  "behavior": [
    "输入按种类路由（docs/03-ux.md）：普通输入默认给 Commander，@role 是稳定目标；ASK 走只读查询；STEER 记录生效边界；CHANGE 写 CR（REQ-033）；CONTROL 转为对应的控制命令（PauseRun、ResumeRun、CancelRun，C26）。输入状态为 QUEUED→DELIVERED→APPLIED|REJECTED。",
    "ASK 不改任务：回答作为消息返回，tasks 与 runs 的 revision 不变，不新增工作 Attempt；给非 Commander 职位的 ASK 不打断其运行中的 turn，在下一个安全点或经只读查询回答（C25）。",
    "STEER 的生效边界：绑定 Agent 支持 turn 内引导（mid_turn_steer）时在当前 turn 内投递，否则排到该职位的下一个 Attempt；事件 input.delivered{boundary:\"turn\"|\"next_attempt\"} 写明边界；新持有者的首个输入付清之前不投递（M2T17R-153）。",
    "@role 解析：一个 Agent 绑定多个职位时按当前任务的职位解析并显示实际目标，含糊时弹出选择器；投递只给当前 epoch 的绑定，换绑后迟到的投递被拒（STALE_ROLE_EPOCH，M2T17R-154）。",
    "投递循环（ADR-018 决定 4）接入 host：QUEUED 的输入至少投递一次，以 (consumer, message_id) 去重；consume 的 KeyReuse 计为一次拒绝，不重试到死信（M2T17R-254）；领取永不取验证交接（ADR-028 决定 9）；host 重启后未 ACK 的照常重投；接管之后的 query/steer 路由取代 deferred 回应，历史上 deferred 的不补投（ADR-028 决定 13）。",
    "控制可独立于模型执行：PauseRun、CancelRun 在 Commander 与全部 Agent 不可用时照常生效（AC-043-04 已判，本需求不重复）。"
  ],
  "failure_modes": [
    "ASK 之后 tasks 与 runs 的 revision 不变，没有新工作 Attempt。",
    "换绑后发往旧 epoch 的投递为 REJECTED（STALE_ROLE_EPOCH）。",
    "host 在投递后、ACK 前崩溃，重启后重投且只应用一次。",
    "CHANGE 在批准前不改规格与任务。"
  ],
  "doc_refs": [
    "docs/03-ux.md",
    "docs/06-orchestration.md",
    "docs/12-user-guide.md",
    "docs/adr/ADR-018.md",
    "docs/adr/ADR-024.md",
    "docs/adr/ADR-025.md",
    "docs/adr/ADR-028.md"
  ],
  "acceptance": [
    {
      "id": "AC-017-01",
      "given": "Run R 中 Task T1 运行，FakeCommander 可回答",
      "when": "向默认目标发 ASK“进度如何”",
      "then": "input_queue 中该输入依次为 QUEUED、DELIVERED、APPLIED 并带回答消息；全部 tasks 与 runs 的 revision 不变；没有新的工作 Attempt",
      "test_id": "TEST-017-01"
    },
    {
      "id": "AC-017-02",
      "given": "developer 的 Attempt 运行中；一个绑定为支持 mid_turn_steer 的 Codex App Server 夹具，另一个为不支持的假 Agent",
      "when": "各向 developer 发 STEER",
      "then": "支持者在当前 turn 内收到引导、写 input.delivered{boundary:\"turn\"}；不支持者在下一个 Attempt 的输入中收到、写 input.delivered{boundary:\"next_attempt\"}；两者都只投递一次",
      "test_id": "TEST-017-02"
    },
    {
      "id": "AC-017-03",
      "given": "已批准修订 1 的 Run，Task 运行中；Commander 与全部 Agent 都不可用（假 CLI 不响应）",
      "when": "发 CHANGE“把导出格式改为 CSV”，再发 CONTROL“暂停”",
      "then": "CHANGE 新增一条 PROPOSED 的 change_requests（REQ-033），批准前 specs、runs.spec_id 与 tasks 的状态不变；CONTROL 转为一条 PauseRun 命令，Run 经 PAUSE_REQUESTED 进入 PAUSED；两者都没有任何模型 turn 参与",
      "test_id": "TEST-017-03"
    },
    {
      "id": "AC-017-04",
      "given": "Agent claude 绑定 cto 与 developer；当前运行的是 developer 的 Task；随后 developer 换绑到 codex（epoch+1）",
      "when": "TUI 输入“@claude 先写测试”，随后向换绑前的 epoch 投递一条迟到的消息",
      "then": "消息栏显示实际目标 developer，input_queue 的 target_role=developer；迟到的投递为 REJECTED(STALE_ROLE_EPOCH)",
      "test_id": "TEST-017-04"
    },
    {
      "id": "AC-017-05",
      "given": "投递循环接入 host；三条 QUEUED 输入，其中一条的 consume 会以 KeyReuse 失败；outbox 中有一条验证交接",
      "when": "投递一轮；在一条已投递未 ACK 时 kill -9 host 并重启",
      "then": "每条输入恰应用一次（以 consumer+message_id 去重）；KeyReuse 的一条计一次拒绝，不进死信；验证交接没有被领取；重启后未 ACK 的被重投并只应用一次",
      "test_id": "TEST-017-05"
    }
  ],
  "decisions": [
    "ASK 不打断运行中的 turn（docs/12：问进度不会打断编码）（CR-026，C25）。",
    "CONTROL 输入即转为对应的控制命令，不经模型（CR-026，C26）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-017 的场景（旧：TEST-017-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-017-01",
    "requirement_id": "REQ-017",
    "acceptance_id": "AC-017-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander，T1 运行中",
    "steps": [
      "发 ASK"
    ],
    "pass_criteria": [
      "输入状态依次 QUEUED、DELIVERED、APPLIED 并带回答",
      "revision 不变",
      "无新工作 Attempt"
    ],
    "observables": [
      "input_queue 状态序列",
      "tasks/runs revision",
      "attempts 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-017-02",
    "requirement_id": "REQ-017",
    "acceptance_id": "AC-017-02",
    "check_id": "adapter-live",
    "fixture": "Codex App Server（真实 CLI，mid_turn_steer）+ 不支持的假 Agent",
    "steps": [
      "各发 STEER"
    ],
    "pass_criteria": [
      "支持者 boundary=turn",
      "不支持者 boundary=next_attempt",
      "各只投递一次"
    ],
    "observables": [
      "适配器事件",
      "events导出",
      "假 Agent 输入",
      "判定结果"
    ]
  },
  {
    "id": "TEST-017-03",
    "requirement_id": "REQ-017",
    "acceptance_id": "AC-017-03",
    "check_id": "integration",
    "fixture": "产品 host，修订 1 的 Run，假 CLI 不响应",
    "steps": [
      "发 CHANGE",
      "发 CONTROL 暂停"
    ],
    "pass_criteria": [
      "一条 PROPOSED 的 change_requests",
      "specs、spec_id 与任务状态不变",
      "CONTROL 成为 PauseRun 且 Run 进入 PAUSED",
      "无模型 turn"
    ],
    "observables": [
      "change_requests 行",
      "commands 与 runs 行",
      "attempts 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-017-04",
    "requirement_id": "REQ-017",
    "acceptance_id": "AC-017-04",
    "check_id": "tui-pty",
    "fixture": "PTY 中的 hoh TUI + 产品 host，claude 绑定两个职位，换绑",
    "steps": [
      "@claude 输入",
      "迟到投递"
    ],
    "pass_criteria": [
      "显示实际目标 developer 且 target_role=developer",
      "迟到投递 REJECTED(STALE_ROLE_EPOCH)"
    ],
    "observables": [
      "屏幕快照",
      "input_queue 行",
      "events导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-017-05",
    "requirement_id": "REQ-017",
    "acceptance_id": "AC-017-05",
    "check_id": "recovery",
    "fixture": "产品 host 进程 + 投递循环 + kill -9 + 重启",
    "steps": [
      "投递一轮",
      "已投递未 ACK 时 kill -9",
      "重启"
    ],
    "pass_criteria": [
      "每条恰应用一次",
      "KeyReuse 计一次拒绝且无死信",
      "验证交接未被领取",
      "重启后重投只应用一次"
    ],
    "observables": [
      "outbox 与 input_queue",
      "receipts",
      "events导出",
      "判定结果"
    ]
  }
]
```

#### REQ-023 局部咨询与升级（4 条 AC）

`contracts/requirements.json` 中 REQ-023 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-023-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-023",
  "category": "TEAM",
  "priority": "P0",
  "milestone": "M3",
  "title": "局部咨询与升级",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "局部咨询不绕过全局任务账本；重大决定升级 Commander；讨论有轮数上限",
  "behavior": [
    "角色间咨询经 Router 成为全局账本中的子 Task（parent、role、预算、deadline），出现在 /v1/runs/{id}/tasks，结果回到请求方；没有账本之外的工作。",
    "wait-for 图检测环：A 等 B 的回答时 B 向 A 提问，后一请求被拒（CONSULT_CYCLE，details 列出环成员）；QA 请 Developer 改当前候选被拒并转为 Issue（ADR-028 决定 13）。",
    "讨论有轮数上限：同一争议最多两轮意见（MOA），第三轮请求被拒（DISCUSSION_LIMIT），Commander 以 need_decision 被唤醒；没有新证据时提前停止；结论产出 Decision 或 ADR 引用。",
    "重大决定升级：咨询结论触及规格、ADR、公开接口或门禁时（C27），参与职位不自行应用，升级为 Commander 的 need_decision；规格变更再走 CR（REQ-033）。"
  ],
  "failure_modes": [
    "咨询成环时后一请求被拒（CONSULT_CYCLE），不建子 Task，不死锁。",
    "第三轮讨论被拒（DISCUSSION_LIMIT），Commander 被唤醒一次。",
    "改当前候选的请求被拒并开 Issue，不派发修改。"
  ],
  "doc_refs": [
    "docs/06-orchestration.md",
    "docs/adr/ADR-028.md",
    "docs/adr/ADR-025.md",
    "resources/prompts/researcher.md"
  ],
  "acceptance": [
    {
      "id": "AC-023-01",
      "given": "developer 的 Task 运行中，researcher 绑定可用",
      "when": "developer 请求 researcher 研究一个问题",
      "then": "Router 建一个子 Task（parent=该 Task、role=researcher、带预算与 deadline），出现在 /v1/runs/{id}/tasks；researcher 的结果作为消息回到 developer；Attempt 全部属于账本中的 Task",
      "test_id": "TEST-023-01"
    },
    {
      "id": "AC-023-02",
      "given": "developer 正等待 researcher 的回答；QA 正在验证当前候选",
      "when": "researcher 向 developer 提问；QA 请求 developer 修改当前候选",
      "then": "researcher 的请求被拒，reason=CONSULT_CYCLE 且 details 列出 developer 与 researcher，没有新子 Task；QA 的请求被拒并开一条 Issue，没有修改派发",
      "test_id": "TEST-023-02"
    },
    {
      "id": "AC-023-03",
      "given": "架构争议已进行两轮意见",
      "when": "参与者发起第三轮讨论请求",
      "then": "请求被拒（DISCUSSION_LIMIT）；Commander 以 need_decision 被唤醒恰一次，唤醒摘要含两轮意见的引用；之后的结论记录为 Decision 或 ADR 引用",
      "test_id": "TEST-023-03"
    },
    {
      "id": "AC-023-04",
      "given": "researcher 与 developer 的咨询结论是“改公开接口的签名”",
      "when": "参与职位尝试直接应用该结论",
      "then": "应用被拒，结论升级为 Commander 的 need_decision；没有 Task 在升级处理前按该结论派发",
      "test_id": "TEST-023-04"
    }
  ],
  "decisions": [
    "重大决定为触及规格、ADR、公开接口或门禁者（CR-026，C27）。",
    "讨论上限两轮为初值，固定写在 Runtime，不新增配置键；要调整另写 CR（CR-026，C28）。",
    "新码 CONSULT_CYCLE、DISCUSSION_LIMIT 为 RejectionReason（CR-026）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-023 的场景（旧：TEST-023-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-023-01",
    "requirement_id": "REQ-023",
    "acceptance_id": "AC-023-01",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent（developer、researcher）",
    "steps": [
      "developer 请求研究"
    ],
    "pass_criteria": [
      "子 Task 带 parent、role、预算、deadline 并出现在 tasks API",
      "结果回到 developer",
      "无账本外 Attempt"
    ],
    "observables": [
      "tasks 行与父子关系",
      "消息",
      "attempts 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-023-02",
    "requirement_id": "REQ-023",
    "acceptance_id": "AC-023-02",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent，developer 等待中，QA 验证中",
    "steps": [
      "researcher 提问 developer",
      "QA 请求改候选"
    ],
    "pass_criteria": [
      "CONSULT_CYCLE 且 details 列出环成员",
      "无新子 Task",
      "QA 请求被拒并开 Issue"
    ],
    "observables": [
      "请求状态与错误体",
      "issues 行",
      "tasks 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-023-03",
    "requirement_id": "REQ-023",
    "acceptance_id": "AC-023-03",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander，两轮意见",
    "steps": [
      "第三轮请求"
    ],
    "pass_criteria": [
      "DISCUSSION_LIMIT",
      "need_decision 唤醒恰一次且摘要含两轮引用",
      "结论为 Decision 或 ADR 引用"
    ],
    "observables": [
      "轮次计数",
      "唤醒记录",
      "decisions 行",
      "判定结果"
    ]
  },
  {
    "id": "TEST-023-04",
    "requirement_id": "REQ-023",
    "acceptance_id": "AC-023-04",
    "check_id": "integration",
    "fixture": "产品 host + FakeAgent 咨询结论",
    "steps": [
      "尝试应用"
    ],
    "pass_criteria": [
      "应用被拒并升级 need_decision",
      "处理前无派发"
    ],
    "observables": [
      "events导出",
      "decisions 行",
      "调度轮派发记录",
      "判定结果"
    ]
  }
]
```

#### REQ-040 停滞与诊断（4 条 AC）

`contracts/requirements.json` 中 REQ-040 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-040-01；id、category、priority、milestone、title、required_release、release_class、release_targets、summary 不变）：

```json
{
  "id": "REQ-040",
  "category": "WORK",
  "priority": "P0",
  "milestone": "M3",
  "title": "停滞与诊断",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "按问题指纹/状态变化而非commit计数诊断，升级/研究/改计划",
  "behavior": [
    "净进展是向量（docs/07 §2）：新增 accepted AC、关闭的 issue、重开的 issue、新增 critical/high、未消除的决策风险；判定不读提交数。",
    "连续 3 轮验证 accepted 不增、critical/high 不降、且失败 fingerprint 重复时，写恰好一条 no_progress{window:3, fingerprints, vector, last_progress_seq}，以 High 唤醒 Commander。",
    "窗口重设：一轮出现关闭的 issue 或 accepted 增加即重设；规格新增 REQ 时重设并在事件中写原因；发布稳定阶段问题总数下降即为进展。",
    "no_progress 的唤醒带诊断（重复的 fingerprint、最后一次进展的 seq、建议：升级、研究或改计划）；Commander 的回答可建研究 Task 或改计划；Runtime 不自动循环“继续”。",
    "no_progress 管“有推进而无净进展”，ADR-031 更正 C 的 STALLED 管“没有任何推进”；同一窗口内两者不对同一指纹重复唤醒（C29）。"
  ],
  "failure_modes": [
    "只改 README 或 docs 的三轮候选、提交数增加而 accepted 不增，照样判为无进展。",
    "同一窗口不写第二条 no_progress，重启后不重复。",
    "STALLED 已为同一状况唤醒时不再写 no_progress。"
  ],
  "doc_refs": [
    "docs/07-algorithms.md",
    "docs/06-orchestration.md",
    "docs/adr/ADR-031.md",
    "docs/adr/ADR-025.md",
    "docs/adr/ADR-029.md"
  ],
  "acceptance": [
    {
      "id": "AC-040-01",
      "given": "脚本化的验证结果：连续 3 轮同一失败 fingerprint F 复现，accepted AC 不增，critical/high 不降",
      "when": "Runtime 处理第 3 轮的结果，随后再处理一轮同样的结果并重启 host",
      "then": "第 3 轮后恰写一条 no_progress{window:3, fingerprints:[F], vector, last_progress_seq} 并以 High 唤醒 Commander；第 4 轮与重启后不再写第二条",
      "test_id": "TEST-040-01"
    },
    {
      "id": "AC-040-02",
      "given": "三轮候选只改 README 与 docs/，提交数各增加，accepted 不增",
      "when": "对三轮结果调用进展向量函数与判定",
      "then": "判定为无进展；进展向量中没有提交数分量，提交数变化不改变判定",
      "test_id": "TEST-040-02"
    },
    {
      "id": "AC-040-03",
      "given": "四个结果序列：第 2 轮关闭一个 issue；第 2 轮 accepted 增加；第 2 轮规格新增一条 REQ；发布稳定阶段问题总数从 5 降到 3",
      "when": "对每个序列逐轮判定",
      "then": "前两个序列在第 2 轮重设窗口、第 3 轮不判无进展；第三个序列重设并在事件中写原因“规格新增 REQ”；第四个序列判为进展",
      "test_id": "TEST-040-03"
    },
    {
      "id": "AC-040-04",
      "given": "no_progress 已唤醒 Commander，FakeCommander 的回答建一个研究 Task",
      "when": "Commander 处理该唤醒",
      "then": "唤醒摘要含重复的 fingerprint、最后一次进展的 seq 与建议；之后出现 role=researcher 的新 Task；Runtime 没有自行派发“继续”的 Commander turn",
      "test_id": "TEST-040-04"
    }
  ],
  "decisions": [
    "窗口 3 轮为初值（docs/07 §2），固定写在 Runtime（CR-026）。",
    "no_progress 与 STALLED 分工如上，同一状况只唤醒一次（CR-026，C29）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-040 的场景（旧：TEST-040-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-040-01",
    "requirement_id": "REQ-040",
    "acceptance_id": "AC-040-01",
    "check_id": "integration",
    "fixture": "产品 host + 脚本化验证结果 + 重启",
    "steps": [
      "处理三轮结果",
      "处理第 4 轮",
      "重启"
    ],
    "pass_criteria": [
      "恰一条 no_progress 且载荷字段齐全",
      "High 唤醒",
      "第 4 轮与重启后无第二条"
    ],
    "observables": [
      "events导出",
      "唤醒记录",
      "判定结果"
    ]
  },
  {
    "id": "TEST-040-02",
    "requirement_id": "REQ-040",
    "acceptance_id": "AC-040-02",
    "check_id": "unit",
    "fixture": "进展向量函数 + 三轮只改文档的结果",
    "steps": [
      "计算向量并判定"
    ],
    "pass_criteria": [
      "判定无进展",
      "向量无提交数分量"
    ],
    "observables": [
      "向量",
      "判定结果"
    ]
  },
  {
    "id": "TEST-040-03",
    "requirement_id": "REQ-040",
    "acceptance_id": "AC-040-03",
    "check_id": "unit",
    "fixture": "进展判定 + 四个结果序列",
    "steps": [
      "逐轮判定"
    ],
    "pass_criteria": [
      "前两个第 2 轮重设",
      "第三个重设并写原因",
      "第四个判为进展"
    ],
    "observables": [
      "判定序列",
      "事件载荷",
      "判定结果"
    ]
  },
  {
    "id": "TEST-040-04",
    "requirement_id": "REQ-040",
    "acceptance_id": "AC-040-04",
    "check_id": "integration",
    "fixture": "产品 host + FakeCommander 回答建研究 Task",
    "steps": [
      "处理唤醒"
    ],
    "pass_criteria": [
      "摘要含诊断三项",
      "出现 researcher 的新 Task",
      "无自动“继续” turn"
    ],
    "observables": [
      "唤醒摘要",
      "tasks 行",
      "attempts 行",
      "判定结果"
    ]
  }
]
```


### 5. M3 实现任务的拆分（`contracts/development-tasks.json`）

M3-T02、M3-T03、M3-T04 保留 id 作为各组的第一个任务，新增 M3-T06 至 M3-T10；M3-T01（spec）与 M3-T05（verify）不变，只是 M3-T05 的 `depends_on` 改为全部八个实现任务。每个实现任务至多 2 项需求、8 条 AC：

| 任务 | 需求（AC 数） | AC | depends_on |
|---|---|---|---|
| M3-T02 | REQ-034（4）、REQ-033（4） | 8 | M3-T01 |
| M3-T06 | REQ-031（4）、REQ-030（3） | 7 | M3-T02 |
| M3-T07 | REQ-032（3）、REQ-028（4） | 7 | M3-T02 |
| M3-T03 | REQ-045（4）、REQ-047（3） | 7 | M3-T02 |
| M3-T08 | REQ-029（3）、REQ-046（3） | 6 | M3-T06、M3-T03 |
| M3-T04 | REQ-008（4）、REQ-009（3） | 7 | M3-T06、M3-T08、M3-T09 |
| M3-T09 | REQ-007（3）、REQ-017（5） | 8 | M3-T02 |
| M3-T10 | REQ-023（4）、REQ-040（4） | 8 | M3-T09 |
| 合计 | 16 项 | 58 |  |


替换后的八个任务（M3-T02、M3-T03、M3-T04 的旧条目由下文取代；新条目插在 M3-T05 之前）：

```json
[
  {
    "id": "M3-T02",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "文档事实源与规格变更（规格位置、索引、CR、影响闭包、证据失效）",
    "depends_on": [
      "M3-T01"
    ],
    "requirements": [
      "REQ-034",
      "REQ-033"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。规格变化置 STALE 时写 candidate.stale{candidate_id, reason}（ADR-028 决定 11 的持久取代事实）；规格事实源的位置替换 M2-T16 冻结使用的约定路径（ADR-028 决定 4）；以 SPEC_MISMATCH 被挡住的写 Task 经规格变更流程放行；spec.changed 的产生，并把验证中的候选按 REQ-053 行为 7 置 STALE；旧规格下的开发 Task 的迁移或替代（含 task_requirements 的需求映射，使它们在新规格下可派发）（CR-018）。规格位置不再指向 contracts/requirements.json 后，删去 persistence_scan 的对应条目（CR-019）。C22 的起草依据（CR-026）在本任务的 ADR 中定稿。"
  },
  {
    "id": "M3-T06",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Readiness Gate 与规格收敛（假设台账、来源校验）",
    "depends_on": [
      "M3-T02"
    ],
    "requirements": [
      "REQ-031",
      "REQ-030"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。按任务范围选代码的上下文来源（REQ-039，ADR-026 决定 12）在本任务实现：Task 的路径范围随计划与 readiness 出现（CR-016）。"
  },
  {
    "id": "M3-T07",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "需求追踪与项目发现",
    "depends_on": [
      "M3-T02"
    ],
    "requirements": [
      "REQ-032",
      "REQ-028"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。TEST-032-01 的测试在 tests/bundle、名称含 test_032_01（traceability 自测据此定位）（CR-017，ADR-027 的移交）。"
  },
  {
    "id": "M3-T03",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "四层Prompt版本、自定义与升级",
    "depends_on": [
      "M3-T02"
    ],
    "requirements": [
      "REQ-045",
      "REQ-047"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。"
  },
  {
    "id": "M3-T08",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "内置模板与内置Skill",
    "depends_on": [
      "M3-T06",
      "M3-T03"
    ],
    "requirements": [
      "REQ-029",
      "REQ-046"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。Skill 正文按需加载与短索引（REQ-039 行为 7 的前半，ADR-026）在本任务实现。"
  },
  {
    "id": "M3-T04",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Plan 与 Build/Auto",
    "depends_on": [
      "M3-T06",
      "M3-T08",
      "M3-T09"
    ],
    "requirements": [
      "REQ-008",
      "REQ-009"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。"
  },
  {
    "id": "M3-T09",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Slash 统一操作与运行中对话（投递循环接入 host）",
    "depends_on": [
      "M3-T02"
    ],
    "requirements": [
      "REQ-007",
      "REQ-017"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。steer/query 的产品路由与投递：ADR-018 决定 4 的投递循环接入 host 与它的消费者（领取永不取验证交接，ADR-028 决定 9），当前 epoch 的路由，M2T17R-153（新持有者首个输入之前不投递）、M2T17R-154（哪些 Commander 消息还要核对接收方的 epoch）、M2T17R-254（KeyReuse 计为一次拒绝、不重试到死信）（ADR-025 决定 10）；接管之后的 query/steer 路由（取代 M2-T16 的 deferred 回应），历史上 deferred 的消息不补投（ADR-028 决定 13）（CR-018）。用户输入交给 Commander（ADR-025 决定 1）。改动进程与并发，按 docs/19 由两名互补审查者审查。"
  },
  {
    "id": "M3-T10",
    "milestone": "M3",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "局部咨询与停滞诊断",
    "depends_on": [
      "M3-T09"
    ],
    "requirements": [
      "REQ-023",
      "REQ-040"
    ],
    "done_when": "所列需求的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。角色间咨询的 wait-for 环检测（CR-018）；no_progress、milestone_ready 与 release_request 的产生方（ADR-025 决定 1；后两者没有 M3 的 AC，其定义在本任务的 ADR 中给出，不早于它们的需求所在里程碑的细化而改变 REQ-021 的唤醒集合）。"
  }
]
```


M3-T05 的 `depends_on`：

- 旧：`["M3-T02", "M3-T03", "M3-T04"]`
- 新：`["M3-T02", "M3-T03", "M3-T04", "M3-T06", "M3-T07", "M3-T08", "M3-T09", "M3-T10"]`

拆分的理由：

- M3-T02 先行：规格的位置、索引与修订是其余各组的前提（准备清单 §6 第 1 条）。它承接 CR-018 给 M3-T02 的全部项与 CR-019 的条目删除，并在它的 ADR 中定 C22。
- M3-T06（readiness 与收敛）承接 REQ-039 按任务范围选代码（CR-016）：Task 的路径范围随计划与 readiness 出现。
- M3-T07（追踪与发现）承接 CR-017 的 TEST-032-01 位置。
- M3-T03（四层 Prompt 与覆盖）依赖 M3-T02：项目层引用已批准规格。
- M3-T08（模板与 Skill）依赖 M3-T06（规格模板的五节，C7）与 M3-T03（模板与 Skill 也按四层覆盖）；承接 ADR-026 的 Skill 短索引。
- M3-T09（SetMode、斜杠与运行中对话）依赖 M3-T02（CHANGE 写 CR）；承接 CR-018 给 M3-T04 的投递项。它把投递循环接入 host，是进程与并发改动，按 docs/19 用两名互补审查者。
- M3-T04（Plan 与 Build/Auto）依赖 M3-T06（readiness）、M3-T08（Plan 用的模板与 Skill）与 M3-T09（SetMode 的安全点）。
- M3-T10（咨询与停滞诊断）依赖 M3-T09（咨询经投递循环路由）；承接 wait-for 环检测与 no_progress 的产生方。

### 6. 每条 AC 的依赖闭包（CR3R-02）

每条 AC 所需的能力都由它的任务或其祖先提供。所需检查的交付者（validate_bundle 的归属规则）全在 M3-T01 的祖先中；下表列出各 AC 的检查与它依赖的能力：

| 任务 | AC | 检查 | 所需能力与提供者（任务或其祖先） |
|---|---|---|---|
| M3-T02 | AC-034-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T02 | AC-034-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T02 | AC-034-03 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T02 | AC-034-04 | unit | hoh-core 的纯函数与临时 SQLite（M0-T03 起） |
| M3-T02 | AC-033-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T02 | AC-033-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T02 | AC-033-03 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T02 | AC-033-04 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T06 | AC-031-01 | unit | hoh-core 的纯函数与临时 SQLite（M0-T03 起） |
| M3-T06 | AC-031-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T06 | AC-031-03 | unit | hoh-core 的纯函数与临时 SQLite（M0-T03 起） |
| M3-T06 | AC-031-04 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T06 | AC-030-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T06 | AC-030-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T06 | AC-030-03 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T07 | AC-032-01 | traceability | traceability 检查与 release_gate 仿本（M2-T10） |
| M3-T07 | AC-032-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T07 | AC-032-03 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T07 | AC-028-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T07 | AC-028-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T07 | AC-028-03 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T07 | AC-028-04 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T03 | AC-045-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T03 | AC-045-02 | unit | hoh-core 的纯函数与临时 SQLite（M0-T03 起） |
| M3-T03 | AC-045-03 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T03 | AC-045-04 | unit | hoh-core 的纯函数与临时 SQLite（M0-T03 起） |
| M3-T03 | AC-047-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T03 | AC-047-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T03 | AC-047-03 | deployment | 发布包、安装器与 deployment 检查（M2-T14） |
| M3-T08 | AC-029-01 | unit | hoh-core 的纯函数与临时 SQLite（M0-T03 起） |
| M3-T08 | AC-029-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T08 | AC-029-03 | deployment | 发布包、安装器与 deployment 检查（M2-T14） |
| M3-T08 | AC-046-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T08 | AC-046-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T08 | AC-046-03 | unit | hoh-core 的纯函数与临时 SQLite（M0-T03 起） |
| M3-T04 | AC-008-01 | tui-pty | TUI 外壳与 PTY 检查（M1-T08）、host 与 attach（M2-T06、M2-T13） |
| M3-T04 | AC-008-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T04 | AC-008-03 | tui-pty | TUI 外壳与 PTY 检查（M1-T08）、host 与 attach（M2-T06、M2-T13） |
| M3-T04 | AC-008-04 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T04 | AC-009-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T04 | AC-009-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T04 | AC-009-03 | unit | hoh-core 的纯函数与临时 SQLite（M0-T03 起） |
| M3-T09 | AC-007-01 | tui-pty | TUI 外壳与 PTY 检查（M1-T08）、host 与 attach（M2-T06、M2-T13） |
| M3-T09 | AC-007-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T09 | AC-007-03 | tui-pty | TUI 外壳与 PTY 检查（M1-T08）、host 与 attach（M2-T06、M2-T13） |
| M3-T09 | AC-017-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T09 | AC-017-02 | adapter-live | Codex App Server 适配器与 adapter-live 检查（M1-T03、M2-T07） |
| M3-T09 | AC-017-03 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T09 | AC-017-04 | tui-pty | TUI 外壳与 PTY 检查（M1-T08）、host 与 attach（M2-T06、M2-T13） |
| M3-T09 | AC-017-05 | recovery | kill -9 恢复矩阵与 recovery 检查（M2-T05） |
| M3-T10 | AC-023-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T10 | AC-023-02 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T10 | AC-023-03 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T10 | AC-023-04 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T10 | AC-040-01 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |
| M3-T10 | AC-040-02 | unit | hoh-core 的纯函数与临时 SQLite（M0-T03 起） |
| M3-T10 | AC-040-03 | unit | hoh-core 的纯函数与临时 SQLite（M0-T03 起） |
| M3-T10 | AC-040-04 | integration | host、HTTP API、FakeAgent/FakeCommander 的测试世界（M2-T12、M2-T07、M2-T13） |


## CTO 的问题（C1–C29）：提议与理由，**待 CTO 决定**

下列提议已按“推荐”写进上文的需求；CTO 改变任一项时，相应需求的文字随之修改后再送设计审查。

| ID | 问题 | 提议 | 理由 |
|---|---|---|---|
| C1 | SPEC_CONFLICT 是新码还是 CONFLICT 的 details | 新码，409 | CONFLICT 已指命令 id 冲突；同码两义会让客户端误判 |
| C2 | 影响闭包的算法 | 按 REQ→AC→TEST→TASK 的显式映射，不按路径 | 映射都在库中（requirements、acceptance_criteria、task_requirements），可判定；路径猜测不可判定 |
| C3 | CR 是否另写文件进规格根 | 不另写；CR 是 change_requests 行，提议的内容就是工作树中的规格，批准时以摘要锁定 | 避免规格之外的第二份文本，符合 REQ-034 |
| C4 | 关键决定与非阻塞未知的标记 | 规格开放问题条目带“阻塞：是/否”，缺省为阻塞 | 写在事实源里，readiness 是纯函数；缺省阻塞是保守的一侧 |
| C5 | readiness 由谁判定 | Runtime 自动判定，Commander 不能覆盖 | docs/06 的固定工程约束；可测 |
| C6 | 假设台账的载体 | record_assumption 事件的投影，不新增表 | 不改受锁 SQL；事件已有 basis |
| C7 | 五节的模板归属 | REQ-029 的规格模板 | 模板是 REQ-029 的产物，REQ-030 只校验内容 |
| C8 | REQ-032 是产品功能还是本仓库的工具 | 产品功能；tests/bundle 的 test_032_01 以 traceability 检查校验产品导出 | 同时满足 summary 与 CR-017 的测试位置 |
| C9 | 追踪的产品入口 | M3 为 API 与 TUI 的状态摘要；Web 视图属 M6 | M3 的任务闭包里没有 web-e2e |
| C10 | 构建与测试命令的检测 | 按 Cargo.toml、package.json 的 scripts、Makefile、pyproject.toml 等约定；检测不到为 unknown | 不猜；unknown 如实显示 |
| C11 | 基线随 Run 还是随项目 | 随 Run（run.baseline） | 工作树在 Run 之间会变；对照应是本 Run 开始时 |
| C12 | 契约长度上限 | 32 KiB（UTF-8 字节） | 内置六份为 4.4–7.5 KB，留四倍余量；字节可判定 |
| C13 | 模板变量 | 白名单只有 GOAL、PROJECT_CONTEXT、TASK_CONTEXT、EVIDENCE_CONTEXT | 与 validate_bundle 对内置契约的检查一致 |
| C14 | 资源升级的生效点 | ADR-022 决定 8 的职位 checkpoint | 与团队变更同一机制，已有测试的语义 |
| C15 | global 层位置 | `$XDG_CONFIG_HOME/hoh/`（缺省 `~/.config/hoh/`） | 用户级配置的惯例；不在项目里 |
| C16 | 未指定档时的选档 | standard，记录“未指定，用 standard” | 可判定；按目标文本猜规模不可判定 |
| C17 | 模板的分发方式 | 嵌入二进制，计入 resource_bundle_version | 与职位契约、schema 同一资源包；离线可用 |
| C18 | Skill 正文的注入时机 | 按阶段与任务注入，不由 Agent 自行请求 | 可判定、可复现；记入 prompt 修订 |
| C19 | 转给底座的斜杠语法 | `/agent <role> <text>` | 目标显式；/plan 等 HoH 命令永不转发 |
| C20 | 未附着时 `/plan` 的作用对象 | 只改下一个由 TUI 启动的 Run，不改 project.default_mode | 不让一次按键改项目配置 |
| C21 | Shift+Tab 的顺序 | PLAN→BUILD→AUTO→PLAN | 与 docs/03 的列举顺序一致 |
| C22 | 没有已批准规格时 Plan 的起草（tasks.spec_id NOT NULL，受锁 SQL） | 推荐“起草锚”：项目还没有已批准规格时，HoH 建一行 sha256 为 64 个 0 的 specs 行作为起草锚（与 doctor 的 scratch_task 同一约定），起草 Task 引用它；它不被采用、不建索引、不冻结；首个 ApproveSpec 把规格摘要写进这一行，使它成为修订 1。ADR-031 决定 1 的采用规则改为“最新的已批准修订”（M3-T02 的 ADR，ADR 不受锁）。有已批准规格时，起草 Task 引用它（CR 流程）。不改受锁的 SQL | 守住 tasks.spec_id 非空与 specs 的唯一性；修订号连续；no_spec 的阻塞照旧（所有者 2026-11-16 的 Q1(a)）。另一做法（让监督与起草 Task 的 spec_id 可空）要改受锁 SQL，见下节 |
| C23 | 原型工件的允许路径 | 只在 docs/ 下（与 docs/spec/、.hoh/ 共同构成 Plan 的写集） | 写集可由 git 核对 |
| C24 | BUILD 缺规格时 | 阻塞（READINESS_NOT_MET），问题提示可切到 AUTO 或 PLAN，不自动转 AUTO | 模式由用户选；不越过 readiness |
| C25 | ASK 是否打断运行中的 turn | 不打断；在下一个安全点或经只读查询回答 | docs/12：问进度不会打断编码 |
| C26 | CONTROL 输入与控制命令 | CONTROL 即转为 PauseRun、ResumeRun 或 CancelRun，不经模型 | 控制面独立于模型（AC-043-04 已判） |
| C27 | 何为重大决定 | 触及规格、ADR、公开接口或门禁者 | 可判定，覆盖会改变用户可见行为的决定 |
| C28 | MOA 轮数是否可配置 | 两轮固定在 Runtime，不新增配置键；要调整另写 CR | docs/06 的“可调”留到有需要时；不扩大 REQ-018 的键表 |
| C29 | no_progress 与 STALLED 的分工 | STALLED 管“没有任何推进”，no_progress 管“有推进而无净进展”；同一状况只唤醒一次 | 避免两套探测对同一状况两次唤醒（准备清单风险 7） |

## 需所有者决定的项（超出 CR-003 D1 时）

按本稿的推荐，没有一项超出 CR-003 D1（见下节）。只有 C22 的另一做法会超出：若 CTO 或设计审查认为“起草锚”不可取，而改为让监督与起草 Task 的 `tasks.spec_id` 可空，就要改受锁的 `contracts/sql/001_initial.sql`，这不是细化，须所有者批准另一份 CR。保守默认：采用起草锚，不改 SQL。

## 不变的部分

- 16 项需求的 id、category、priority、milestone、title、summary、required_release、release_class（required）与 release_targets（v1）都不变；没有删除任何需求或 AC。
- 每项原有的 AC-xxx-01 保留 id，改写为可判定的内容。原 AC 的 then 照抄 summary，summary 的每一项仍由该需求的 AC 判定，没有被缩小：

| 需求 | summary 的各项 → 判定它的 AC |
|---|---|
| REQ-007 | 两种入口得到同一 SetMode 行为 → 007-01；Shell 不需要对称子命令 → 行为 1（`hoh plan`、`hoh mode` 已被 cli 拒绝，M1 的测试） |
| REQ-008 | 版本化规格 → 008-02；允许提问 → 008-03；未批准实施不修改产品源码 → 008-01、008-04 |
| REQ-009 | Build 遵守既定规格 → 009-01；Auto 可补规划 → 009-02；缺必需规格均不越过 readiness → 009-02、009-03 |
| REQ-017 | Ask 不改任务 → 017-01；steer 有生效边界 → 017-02；CR 改规格、控制可独立于模型执行 → 017-03 |
| REQ-023 | 不绕过全局任务账本 → 023-01；重大决定升级 → 023-04；讨论有轮数上限 → 023-03（环检测 023-02） |
| REQ-028 | 建立基线 → 028-01、028-02；保留脏文件 → 028-01、028-03；不重写架构 → 行为 7 与 028-01；不强制初始化远程 → 028-04 |
| REQ-029 | 规范齐全 → 029-01；按规模选用 → 029-02（离线 029-03） |
| REQ-030 | 记录五类 → 030-01；不伪造用户答案 → 030-02、030-03 |
| REQ-031 | AC 可观察、依赖可达、关键决定确定 → 031-01；仅阻断受影响范围 → 031-02 |
| REQ-032 | 双向查找 → 032-01、032-02（修订后 stale 032-03） |
| REQ-033 | CR、revision、影响闭包 → 033-01；受影响验收过期、不沿用旧 PASS → 033-02、033-03 |
| REQ-034 | 文件为事实源、DB 为索引 → 034-01；冲突可检测 → 034-02；生成页不是第二权威源 → 034-04 |
| REQ-040 | 按指纹与状态变化、不按 commit 计数 → 040-01、040-02；升级、研究、改计划 → 040-04 |
| REQ-045 | 记录版本与 hash → 045-01；高层覆盖不改证据 → 045-02；过长报错 → 045-04（缺变量 045-03） |
| REQ-046 | 无需手装、只加载相关 → 046-01；原生不可用时注入 → 046-02 |
| REQ-047 | 按序覆盖 → 047-01；显示 diff → 047-02；运行锁定旧版本直到 checkpoint → 047-03 |

- 十六个 TEST-xxx-01 中有十四个的 check 不变。改了的两个都是换到更完整的检查，不是放宽：TEST-045-01 与 TEST-040-01 由 unit 改为 integration（它们现在要经 host 记录与重放 Prompt、要重启 host），这两项需求的其余 AC 仍有 unit 检查。integration 与 unit 同在 v1 与 v0.1 档的必跑检查中。
- 不改：CONTRACT、`docs/10-acceptance.md`（第 17 行要求观察绑定当前 spec，与 O6 一致）、`contracts/release-policy.json`、`tools/release_gate.py` 与其余门禁工具、`contracts/openapi.json`、`contracts/sql/001_initial.sql`、各 schema 中除三个原因码枚举之外的部分。没有门禁阈值、检查或证据规则被放宽；O6 使证据规则更严。
- 非 M3 的需求只有 REQ-018 增加一条决定（配置键表加 `[assumptions]`），它的行为与 AC 不变。

## 是否属于 CR-003 D1

1. 只把 16 项 outline 细化为 detailed，补齐 7 个原因码与 REQ-018 键表中缺的一张表（第 1 条）。
2. 不删需求或 AC；required、release_class、release_targets 不变（第 2 条）。
3. 不放宽任何门禁、检查或证据规则；两个 TEST 换到更完整的检查；O6 加严（第 3 条）。
4. 不改 CONTRACT 的架构与交互决定：/plan、Shift+Tab、Plan/Build/Auto、`-p` 与 attach 照 CONTRACT 第 3、4 条；规格批准权（O1）与证据失效（O6）是所有者的决定（第 4 条）。CR-024（Commander 只读，所有者 2026-11-17 批准）已改 CONTRACT 决定 7，本 CR 与之一致：Plan 的规格由 cto 职位的写 Task 起草，Commander 不写文件。

故本 CR 在设计审查 PASS 后可由 CTO 依 D1 批准；若采用 C22 的另一做法，则不在 D1 之内（见上节）。

## 验证方式

本稿的片段由一份数据源生成，并在仓库之外的副本上机械验证（不改工作树的受锁文件）：

1. `git archive` 取 715e2d0 的树到副本，按本稿应用：替换 16 项需求与它们的场景，追加 REQ-018 的决定与 7 个原因码，替换 M3 的任务并改 M3-T05 的依赖。
2. 在副本中运行 `python3 tools/gen_spec_views.py`、以本 CR（副本中标为 APPROVED）运行 `python3 tools/baseline.py relock`，再运行 `python3 tools/validate_bundle.py`。

结果（本稿提交前的最后一次运行）：

- `python3 tools/gen_spec_views.py`：退出码 0
- `python3 tools/baseline.py relock --cr docs/cr/CR-026-m3-detailed-spec.md`：退出码 0
- `python3 tools/validate_bundle.py`：退出码 0
- validate_bundle：status=PASS，errors=0；counts：requirements=70、detailed_requirements=43（原 27）、acceptance=209（原 167）、development_tasks=64（原 59）
- 在同一副本上运行 `bash scripts/check.sh --bundle`：Ran 502 tests in 217.148s；OK (skipped=2)（第一次运行有 3 项失败：TEST-033-03 原用 traceability 检查，而 tests/bundle/test_traceability.py 把 traceability 检查的 TEST 固定为 TEST-032-01、TEST-052-05、TEST-069-01 三项；本稿据此把 AC-033-03 改为以产品的完成门禁判定、用 integration 检查，不改该测试）


批准时的步骤（M3-T01）：按 docs/21 的“基线变更的机械步骤”在工作树中应用同样的片段，`gen_spec_views`、`baseline.py relock --cr docs/cr/CR-026-m3-detailed-spec.md`、`bash scripts/check.sh --bundle`，提交；`config/hoh.example.toml` 的 `[assumptions]` 示例随 M3-T06 的实现加入。

## 附录 A：准备清单的复核（按 main 715e2d0）

准备清单写于 16ff3e1。其后合入的是 retry-after（PR #57、#58）与 CR-025 / M2-T21；M2-T13、M2-T15 在 16ff3e1 之前已合入；CR-024 已由所有者批准，但尚未合入 main（分支 task/commander-readonly）。复核方法：从清单中取出全部 190 处 `path:line` 引用，用 `git diff -U0 16ff3e1 715e2d0` 的块把旧行号映射到新行号，并逐一比较两边的行文本；含糊的文件名按上下文定到唯一文件。

**行号**：190 处引用中，161 处在 715e2d0 上行号与文本都不变；13 处文件名含糊（只写了 `supervise.rs`、`control.rs`、`tui.rs` 之类），按上下文定为唯一文件后，除下表最后一行外文本与行号都不变；下列引用的文本不变、行号移动，以新行号为准：

| 清单中的引用 | 715e2d0 上的位置 | 该行 |
|---|---|---|
| `app_server.rs:292` | `crates/hoh-agents/src/codex/app_server.rs:380` | fn send_steer(&mut self, s: Steer) { |
| `crates/hoh-core/src/decision.rs:412` | `crates/hoh-core/src/decision.rs:459` | SpecMismatch, |
| `decide.rs:922` | `crates/hoh-core/src/persistence/decide.rs:938` | pub(super) fn requirements_passed( |
| `decision.rs:462` | `crates/hoh-core/src/decision.rs:509` | DecisionPayload::CreateTasks { .. } => Ok(Effect::CreateTask |
| `decision.rs:653` | `crates/hoh-core/src/decision.rs:726` | fn transition_run( |
| `docs/adr/ADR-022.md:85` | `docs/adr/ADR-022.md:87` | - `project.document_profile`：M3-T03（REQ-029）； |
| `docs/adr/ADR-030.md:553` | `docs/adr/ADR-030.md:555` | - 只是可供读取、不构成义务的：TUI（M3）显示团队视图的 `missing` 与 `pending.cause`；C |
| `host/product.rs:424` | `crates/hoh-app/src/host/product.rs:425` | fn pack( |
| `host/product.rs:509-513` | `crates/hoh-app/src/host/product.rs:510-514` | ) |
| `host/runner.rs:1071` | `crates/hoh-app/src/host/runner.rs:1075` | held.item(store.detect_stall(run, pending.clone(), at()).awa |
| `host/runner.rs:1216` | `crates/hoh-app/src/host/runner.rs:1220` | // A writer's workspace at its start: its HEAD, the base of  |
| `persistence/retry.rs:949` | `crates/hoh-core/src/persistence/retry.rs:1014` | pub const REVERIFY_LIMIT: u32 = 2; |
| `wakes.rs:515` | `crates/hoh-core/src/persistence/wakes.rs:528` | \| "no_progress" |
| `retry.rs:955-956`（§3 REQ-040） | `crates/hoh-core/src/persistence/retry.rs:1020-1021` | Repair::Local { .. } => "repair.local",（含糊的文件名定为 persistence/retry.rs，行已移动） |

含糊文件名的归属（文本与行号不变）：`supervise.rs:24、:28、:913` 为 crates/hoh-core/src/persistence/supervise.rs，`supervise.rs:111` 为 crates/hoh-core/src/supervise.rs；`control.rs:608` 为 crates/hoh-app/src/host/control.rs；`tui.rs:225、:552` 为 crates/hoh-app/src/tui.rs；`cli.rs:206-208` 为 crates/hoh-app/src/cli.rs；`delivery.rs:1、:231` 为 crates/hoh-core/src/delivery.rs；`verification.rs:1004` 为 crates/hoh-core/src/persistence/verification.rs；`completion.rs:806` 为 crates/hoh-core/src/persistence/completion.rs。

**“M2 已建”的判断**（逐条重查，715e2d0）：

- 仍然成立：`spec.changed` 唯一的产生方是采用（persistence/adoption.rs:51）；`candidate.stale` 唯一的产生方是 evidence_unrecoverable；specs 唯一的插入是 doctor 的 scratch_task（persistence/writer.rs:1144）；change_requests、requirements、acceptance_criteria 在产品里没有写者；SetMode、ProposeSpecChange、ApproveSpec 仍回 422 CAPABILITY_UNSUPPORTED（host/http.rs:857-864）；`Dispatcher` 仍只在 hoh-core 的测试中构造；`no_progress`、`milestone_ready`、`release_request` 仍没有产生方；Codex 的 runner 模式仍关闭 turn 内 steer（codex.rs:854）。
- 已变：`budget.risk` 现在有产生方（M2-T21，persistence/retry.rs:933，CR-025）；准备清单 §2 的 ADR-025 决定 1 一行据此只剩 `no_progress`、`milestone_ready`、`release_request` 与用户输入交给 M3：本 CR 把前三者的产生方写进 M3-T10 的 done_when（后两者没有 M3 的 AC），把用户输入交给 Commander 写进 M3-T09 的 done_when。
- 已变：§2 的 M2T17R-352 一行写“包 H / CR-025（修订中）”，CR-025 已 APPLIED（relock 第 18 版）：交接期间派发的“稍后再派”原因是 `ROLE_BINDING_PENDING`；M3-T09 的投递与它一致。§1.4 的 ErrorCode 清单不变，RejectionReason 多了 ROLE_BINDING_PENDING。
- 已变：CR-024（Commander 只读）已由所有者于 2026-11-17 批准，尚未合入 main。准备清单风险 4 写“Commander 只读（CR-024，审查中）”，现应读为已批准：Plan 的规格由 cto 职位的写 Task 起草（REQ-008 行为 1），Commander 不写文件。
- §2 中“ADR-024 决定 4；M2T17R-153、-154、-254”与“ADR-031 移交 :497、:498”的内容在 715e2d0 上不变。

