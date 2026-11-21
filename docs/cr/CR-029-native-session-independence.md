# CR-029 · 原生会话独立性覆盖完整验证集合

ID：CR-029
状态：DRAFT
批准人：待主开发会话在独立 Review 后批准
日期：2026-11-21
关联：M2-T11、M2R-001、REQ-053、REQ-054、AC-054-01

## 背景

M2-T11 的独立审查发现：不同的 HoH session id 仍可能指向同一个底座原生会话。产品只把验证报告的原生会话与候选的开发任务比较，遗漏 Commander 和另一验证角色；发布门禁只比较两份报告的主 session id，未比较各自 native attempt 中的完整 `session_ids` 集合。合成 v0.1 夹具在 QA/Reviewer 的集合相交时仍返回 `PASS / INTERIM_RELEASED`。

这违反 REQ-054 行为 1 要求的独立会话；开发者自测不得充当独立验收。受锁发布门禁本次仅补齐拒绝条件，不删除需求、AC 或检查，不降低门槛。产品侧另有一项有意的比较范围收窄：原生 id 从仅比较字符串改为比较 `(agent, native session id)`，因此不同底座的相同字符串不再构成原生身份冲突。原因是不同底座各自分配原生 id，相同字符串不代表同一会话；产品已保存 agent，可区分该身份。此项收窄不影响受锁门禁，门禁因缺少每个 session 成员的 agent 信息，仍保守地按字符串比较（M2R-1002）。

## 拟决定

1. `tools/release_gate.py` 在原有校验之外，要求 QA 与 Reviewer 的 native attempt `session_ids` 集合互不相交；两者也必须分别排除 `developer_session_id`。集合比较包括主 session 和其他已观察到的 session，报告排列顺序不影响拒绝结果。
2. 保留报告主 session 各不相同、原生 attempt 完成状态、候选绑定、原始输出摘要、live session 列表与 blocking finding 等全部既有校验。发现集合相交立即拒绝，不进入发布通过状态。
3. 当前 bootstrap native-attempt 契约不为集合中的每个成员提供 agent 字段，因此门禁按现有字符串身份集合保守拒绝交集，不推断底座、不新增契约字段。

## 产品实现与历史

产品已存储底座身份，按 `(agent, native session id)` 比较。QA/Reviewer 入库时检查同一 run 其他角色的当前绑定、历史 attempt 与 `session.native`，覆盖 Commander、Developer 和另一验证角色。报告方自身已存储的原生身份和历史也参与比较，本次导出遗漏某个原生 id 不会擦除已知身份。报告额外提供的原生 session 集合以内部事件 `report.native_sessions` 与报告行在同一事务保存；后续同一候选的报告也与该集合比较，换绑、后续原生 session 更新或数据库重开不会擦除冲突。

冲突沿用 `NATIVE_SESSION_NOT_INDEPENDENT` 原因码与 `REJECTED` 出口，不计入 `report_status` 的当前汇合。冲突集合同时保留上述源任务及其他角色会话的 HoH id，不按 agent 分区；`native_attempt.session_ids`（排除报告方自身 HoH id）含其中任一项，同样拒绝。报告主 `session_id` 的 HoH 冲突另由 `SESSION_NOT_INDEPENDENT` 检查，不能代替对导出集合各成员的检查（M2R-1001）。相同字符串在不同底座、或仅在另一个 run 中出现，不构成这里的原生身份冲突。已有开发者冲突测试保留拒绝断言，并明确让夹具使用同一底座。

此项实现是 REQ-053/054 的既有约束修复；本次同步 ADR-029 决定 7 中仅描述开发任务原生会话比较的旧文字，并记录内部事件的事务与重放语义。

## 影响与不变内容

- 受锁文件只修改 `tools/release_gate.py`；`contracts/baseline-lock.json` 保持原样，尚未重锁。
- 增加 bundle 负例与独立的正对照；Rust 测试覆盖不同 HoH id、同原生身份、两个到达顺序、历史记录、重开后的拒绝和不参与汇合。
- 不修改 requirements、AC、release-policy、check-plan 或 schema；不改变 CONTRACT 的架构、角色分工和发布范围。
- 不把合成夹具或开发者自测写成 live 验收，不批准或关闭 M2R-001 的独立审查结论。

## CR-003 D1 与批准边界

这是修正规格与门禁之间的不一致，并加严证据规则的细化，符合 CR-003 D1 的范围。本文保持 DRAFT，由主开发会话在独立 Review 后决定批准并重锁；委派开发者不批准、不 relock、不 push。重锁前真实仓库的 bundle 基线检查应失败，不能把该失败隐藏成通过。

## 验证记录与后续

- M2R-1001：在 `83b1e9c1` 的产品实现上新增 `test_054_01_native_attempt_rejects_other_roles_hoh_sessions`，确认 QA 的 `native_session_ids` 含开发者 HoH id `sd` 时错误返回 `Current`（期望 `Rejected`）。修复恢复 HoH id 的冲突集合；用例覆盖 QA/Reviewer 对 Developer、Commander、另一验证角色的当前及历史会话、幂等拒绝与不参与汇合。红/绿及回归记录见 `.hoh-dev/evidence/m2r001-followup/`，均为自测，非验收。
- M2R-1003（low，延期，需规格决定）：目前仅在报告入库时判断独立性。若其他角色在报告成为 `CURRENT` 后才恢复到相同原生会话，现有报告不会重新判断。是否在候选进入 `ACCEPTED` 时重新核对独立性、发现冲突后如何处置已有报告，需先由主开发会话明确规格；本次不改变入库后的生命周期，也不声称已覆盖此情形。
- 旧产品实现的红测：不同 HoH id、同原生 session 被记为 `Current`；QA→Reviewer 与 Reviewer→QA 分别复现。第一步修复后报告模块 24 项通过，持久化回归 589 项通过（5 项既有忽略），报告集成 2 项通过，QA/Review 集成 86 项通过；随后追加报告方自身原生历史的红测与修复，最终结果以交接记录为准。均为自测，非验收。
- 旧门禁红测：5 个合成用例中 3 个负例失败，因为门禁实际返回 `PASS / INTERIM_RELEASED`；开发者 session 排除和不相交集合两个对照通过。
- 原始执行日志与诊断清单位于本 worktree 的 `.hoh-dev/evidence/m2r001/`；完整验证结果以该目录的最终交接记录为准。
- 修改后的门禁使用原有已批准资产构造合成 envelope，111 项门禁测试全部通过；夹具资产、门禁模块和真实仓库的基线结果分开记录，未替换真实基线验证函数。最终持久化回归为 591 项通过、5 项既有忽略；报告集成 2 项通过、1 个 WAL 辅助用例按既有方式忽略；QA/Review 集成 86 项通过；fmt 与 workspace clippy（拒绝警告）通过。
- 负责人批准并重锁后，应重新执行完整 bundle，在新的冻结候选上进行独立 QA/Review；本文件不宣称这些待办已执行。
