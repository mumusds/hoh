# 需求—验收—任务追踪

> 生成视图：由 `tools/gen_spec_views.py` 从 `contracts/requirements.json`、`contracts/acceptance-scenarios.json` 与 `contracts/development-tasks.json` 生成，请勿手工编辑。需求变更走 CR（见 [规格治理](21-spec-governance.md)）。

|需求|功能|阶段|发布类|目标|成熟度|验收（测试·检查·实现任务）|任务|
|---|---|---|---|---|---|---|---|
|REQ-001|默认交互入口|M1|required|v1, v0.1|detailed|AC-001-01（TEST-001-01·tui-pty·M2-T13）<br>AC-001-02（TEST-001-02·tui-pty·M2-T13）<br>AC-001-03（TEST-001-03·integration·M1-T08）|M1-T08, M1-T09, M2-T13, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-002|非交互入口|M1|required|v1, v0.1|detailed|AC-002-01（TEST-002-01·integration·M2-T13）<br>AC-002-02（TEST-002-02·integration·M2-T13）<br>AC-002-03（TEST-002-03·integration·M2-T13）|M1-T06, M1-T09, M2-T13, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-003|极简命令面|M1|required|v1, v0.1|detailed|AC-003-01（TEST-003-01·integration·M1-T06）<br>AC-003-02（TEST-003-02·integration·M1-T06）<br>AC-003-03（TEST-003-03·integration·M2-T13）|M1-T06, M1-T09, M2-T13, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-004|标准输出契约|M1|required|v1, v0.1|detailed|AC-004-01（TEST-004-01·unit·M1-T06）<br>AC-004-02（TEST-004-02·integration·M2-T13）<br>AC-004-03（TEST-004-03·integration·M2-T13）<br>AC-004-04（TEST-004-04·integration·M2-T13）|M1-T06, M1-T09, M2-T13, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-005|脱离与附着|M2|required|v1, v0.1|detailed|AC-005-01（TEST-005-01·tui-pty·M2-T06）<br>AC-005-02（TEST-005-02·integration·M2-T06）<br>AC-005-03（TEST-005-03·integration·M2-T06）<br>AC-005-04（TEST-005-04·recovery·M2-T14）<br>AC-005-05（TEST-005-05·integration·M2-T06）<br>AC-005-06（TEST-005-06·deployment·M2-T14）|M2-T06, M2-T14, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-006|环境诊断|M1|required|v1|detailed|AC-006-01（TEST-006-01·integration·M1-T07）<br>AC-006-02（TEST-006-02·integration·M1-T07）<br>AC-006-03（TEST-006-03·adapter-live·M1-T07）|M1-T07, M1-T09, M7-T04|
|REQ-007|Slash 与统一操作|M3|required|v1|outline|AC-007-01（TEST-007-01·tui-pty·M3-T04）|M3-T01, M3-T04, M3-T05, M7-T04|
|REQ-008|Plan 互动与文档|M3|required|v1|outline|AC-008-01（TEST-008-01·tui-pty·M3-T04）|M3-T01, M3-T04, M3-T05, M7-T04|
|REQ-009|Build 与 Auto|M3|required|v1|outline|AC-009-01（TEST-009-01·integration·M3-T04）|M3-T01, M3-T04, M3-T05, M7-T04|
|REQ-010|Workspace 工作台|M4|required|v1|outline|AC-010-01（TEST-010-01·tui-pty·M4-T02）|M4-T01, M4-T02, M4-T04, M7-T04|
|REQ-011|Team/Focus 布局|M4|required|v1|outline|AC-011-01（TEST-011-01·tui-pty·M4-T02）|M4-T01, M4-T02, M4-T04, M7-T04|
|REQ-012|可展开内容块|M4|required|v1|outline|AC-012-01（TEST-012-01·tui-pty·M4-T03）|M4-T01, M4-T03, M4-T04, M7-T04|
|REQ-013|三级时间线|M4|required|v1|outline|AC-013-01（TEST-013-01·tui-pty·M4-T02）|M4-T01, M4-T02, M4-T04, M7-T04|
|REQ-014|真实进度状态栏|M4|required|v1|outline|AC-014-01（TEST-014-01·tui-pty·M4-T02）|M4-T01, M4-T02, M4-T04, M7-T04|
|REQ-015|输入与终端兼容|M4|required|v1|outline|AC-015-01（TEST-015-01·tui-pty·M4-T03）|M4-T01, M4-T03, M4-T04, M7-T04|
|REQ-016|无障碍与键位|M4|required|v1|outline|AC-016-01（TEST-016-01·tui-pty·M4-T03）|M4-T01, M4-T03, M4-T04, M7-T04|
|REQ-017|运行中对话|M3|required|v1|outline|AC-017-01（TEST-017-01·integration·M3-T04）|M3-T01, M3-T04, M3-T05, M7-T04|
|REQ-018|默认与自定义团队|M2|required|v1|detailed|AC-018-01（TEST-018-01·unit·M2-T07）<br>AC-018-02（TEST-018-02·integration·M2-T07）<br>AC-018-03（TEST-018-03·integration·M2-T07）<br>AC-018-04（TEST-018-04·integration·M2-T07）<br>AC-018-05（TEST-018-05·integration·M2-T07）|M2-T07, M2-T11, M7-T04|
|REQ-019|角色交接|M2|required|v1|detailed|AC-019-01（TEST-019-01·integration·M2-T17）<br>AC-019-02（TEST-019-02·integration·M2-T17）<br>AC-019-03（TEST-019-03·integration·M2-T17）<br>AC-019-04（TEST-019-04·integration·M2-T17）<br>AC-019-05（TEST-019-05·recovery·M2-T17）|M2-T17, M2-T11, M7-T04|
|REQ-020|能力匹配与缺员|M2|required|v1|detailed|AC-020-01（TEST-020-01·unit·M2-T15）<br>AC-020-02（TEST-020-02·integration·M2-T15）<br>AC-020-03（TEST-020-03·integration·M2-T15）<br>AC-020-04（TEST-020-04·integration·M2-T15）<br>AC-020-05（TEST-020-05·unit·M2-T15）|M2-T15, M2-T11, M7-T04|
|REQ-021|Commander 监督|M2|required|v1|detailed|AC-021-01（TEST-021-01·unit·M2-T08）<br>AC-021-02（TEST-021-02·unit·M2-T08）<br>AC-021-03（TEST-021-03·recovery·M2-T08）<br>AC-021-04（TEST-021-04·integration·M2-T08）<br>AC-021-05（TEST-021-05·integration·M2-T16）|M2-T08, M2-T16, M2-T11, M7-T04|
|REQ-022|通信与交接|M2|required|v1|detailed|AC-022-01（TEST-022-01·integration·M2-T04）<br>AC-022-02（TEST-022-02·integration·M2-T12）<br>AC-022-03（TEST-022-03·integration·M2-T04）<br>AC-022-04（TEST-022-04·integration·M2-T04）<br>AC-022-05（TEST-022-05·integration·M2-T04）|M2-T12, M2-T04, M2-T11, M7-T04|
|REQ-023|局部咨询与升级|M3|required|v1|outline|AC-023-01（TEST-023-01·integration·M3-T04）|M3-T01, M3-T04, M3-T05, M7-T04|
|REQ-024|Codex 原生接口|M1|required|v1|detailed|AC-024-01（TEST-024-01·integration·M1-T04）<br>AC-024-02（TEST-024-02·integration·M1-T04）<br>AC-024-03（TEST-024-03·integration·M1-T04）<br>AC-024-04（TEST-024-04·adapter-live·M1-T04）|M1-T04, M1-T09, M7-T04|
|REQ-025|Claude 原生接口|M1|required|v1, v0.1|detailed|AC-025-01（TEST-025-01·integration·M1-T03）<br>AC-025-02（TEST-025-02·integration·M1-T03）<br>AC-025-03（TEST-025-03·integration·M1-T03）<br>AC-025-04（TEST-025-04·adapter-live·M1-T03）|M1-T03, M1-T09, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-026|Pi 原生接口|M1|required|v1|detailed|AC-026-01（TEST-026-01·integration·M1-T05）<br>AC-026-02（TEST-026-02·integration·M1-T05）<br>AC-026-03（TEST-026-03·integration·M1-T05）<br>AC-026-04（TEST-026-04·adapter-live·M1-T05）|M1-T05, M1-T09, M7-T04|
|REQ-027|协议版本与能力|M1|required|v1, v0.1|detailed|AC-027-01（TEST-027-01·integration·M1-T01）<br>AC-027-02（TEST-027-02·integration·M1-T07）<br>AC-027-03（TEST-027-03·integration·M2-T05）<br>AC-027-04（TEST-027-04·integration·M2-T15）|M1-T01, M1-T07, M1-T09, M2-T05, M2-T15, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-028|项目发现|M3|required|v1|outline|AC-028-01（TEST-028-01·integration·M3-T02）|M3-T01, M3-T02, M3-T05, M7-T04|
|REQ-029|内置模板|M3|required|v1|outline|AC-029-01（TEST-029-01·unit·M3-T03）|M3-T01, M3-T03, M3-T05, M7-T04|
|REQ-030|规格收敛|M3|required|v1|outline|AC-030-01（TEST-030-01·integration·M3-T02）|M3-T01, M3-T02, M3-T05, M7-T04|
|REQ-031|Readiness Gate|M3|required|v1|outline|AC-031-01（TEST-031-01·unit·M3-T02）|M3-T01, M3-T02, M3-T05, M7-T04|
|REQ-032|需求追踪|M3|required|v1|outline|AC-032-01（TEST-032-01·traceability·M3-T02）|M3-T01, M3-T02, M3-T05, M7-T04|
|REQ-033|规格变更|M3|required|v1|outline|AC-033-01（TEST-033-01·integration·M3-T02）|M3-T01, M3-T02, M3-T05, M7-T04|
|REQ-034|文档事实源|M3|required|v1|outline|AC-034-01（TEST-034-01·integration·M3-T02）|M3-T01, M3-T02, M3-T05, M7-T04|
|REQ-035|双层编排|M2|required|v1, v0.1|detailed|AC-035-01（TEST-035-01·unit·M2-T02）<br>AC-035-02（TEST-035-02·integration·M2-T03）<br>AC-035-03（TEST-035-03·integration·M2-T16）<br>AC-035-04（TEST-035-04·unit·M2-T02）<br>AC-035-05（TEST-035-05·integration·M2-T16）|M2-T02, M2-T03, M2-T16, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-036|持久生命周期|M2|required|v1, v0.1|detailed|AC-036-01（TEST-036-01·unit·M2-T02）<br>AC-036-02（TEST-036-02·recovery·M2-T05）<br>AC-036-03（TEST-036-03·integration·M2-T16）<br>AC-036-04（TEST-036-04·integration·M2-T12）|M2-T02, M2-T12, M2-T05, M2-T16, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-037|单写者与并行验证|M2|required|v1, v0.1|detailed|AC-037-01（TEST-037-01·integration·M2-T03）<br>AC-037-02（TEST-037-02·integration·M2-T16）<br>AC-037-03（TEST-037-03·integration·M2-T16）<br>AC-037-04（TEST-037-04·integration·M2-T16）|M2-T03, M2-T16, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-038|任务DAG调度|M2|required|v1, v0.1|detailed|AC-038-01（TEST-038-01·unit·M2-T03）<br>AC-038-02（TEST-038-02·unit·M2-T03）<br>AC-038-03（TEST-038-03·integration·M2-T03）<br>AC-038-04（TEST-038-04·unit·M2-T03）<br>AC-038-05（TEST-038-05·integration·M2-T03）<br>AC-038-06（TEST-038-06·unit·M2-T03）|M2-T03, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-039|上下文渐进披露|M2|required|v1|detailed|AC-039-01（TEST-039-01·unit·M2-T09）<br>AC-039-02（TEST-039-02·unit·M2-T09）<br>AC-039-03（TEST-039-03·integration·M2-T09）<br>AC-039-04（TEST-039-04·integration·M2-T09）|M2-T09, M2-T11, M7-T04|
|REQ-040|停滞与诊断|M3|required|v1|outline|AC-040-01（TEST-040-01·unit·M3-T04）|M3-T01, M3-T04, M3-T05, M7-T04|
|REQ-041|恢复与幂等|M2|required|v1, v0.1|detailed|AC-041-01（TEST-041-01·recovery·M2-T05）<br>AC-041-02（TEST-041-02·recovery·M2-T05）<br>AC-041-03（TEST-041-03·recovery·M2-T05）<br>AC-041-04（TEST-041-04·integration·M2-T12）<br>AC-041-05（TEST-041-05·recovery·M2-T05）|M2-T12, M2-T05, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-042|预算与等待|M2|required|v1, v0.1|detailed|AC-042-01（TEST-042-01·unit·M2-T19）<br>AC-042-02（TEST-042-02·recovery·M2-T19）<br>AC-042-03（TEST-042-03·integration·M2-T19）<br>AC-042-04（TEST-042-04·integration·M2-T13）<br>AC-042-05（TEST-042-05·unit·M2-T19）<br>AC-042-06（TEST-042-06·integration·M2-T19）|M2-T19, M2-T13, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-043|暂停取消分离|M2|required|v1, v0.1|detailed|AC-043-01（TEST-043-01·integration·M2-T06）<br>AC-043-02（TEST-043-02·integration·M2-T06）<br>AC-043-03（TEST-043-03·tui-pty·M2-T06）<br>AC-043-04（TEST-043-04·integration·M2-T06）<br>AC-043-05（TEST-043-05·unit·M2-T06）|M2-T06, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-044|可恢复版本|M5|required|v1|outline|AC-044-01（TEST-044-01·recovery·M5-T02）|M5-T01, M5-T02, M5-T04, M7-T04|
|REQ-045|四层Prompt版本|M3|required|v1|outline|AC-045-01（TEST-045-01·unit·M3-T03）|M3-T01, M3-T03, M3-T05, M7-T04|
|REQ-046|内置Skill|M3|required|v1|outline|AC-046-01（TEST-046-01·integration·M3-T03）|M3-T01, M3-T03, M3-T05, M7-T04|
|REQ-047|自定义与升级|M3|required|v1|outline|AC-047-01（TEST-047-01·integration·M3-T03）|M3-T01, M3-T03, M3-T05, M7-T04|
|REQ-048|Smart Compact集成|M5|degradable|v1|outline|AC-048-01（TEST-048-01·plugin-live·M5-T03）|M5-T01, M5-T03, M5-T04, M7-T04|
|REQ-049|Hermes Memory集成|M5|degradable|v1|outline|AC-049-01（TEST-049-01·plugin-live·M5-T03）|M5-T01, M5-T03, M5-T04, M7-T04|
|REQ-050|rpiv Todo集成|M5|degradable|v1|outline|AC-050-01（TEST-050-01·plugin-live·M5-T03）|M5-T01, M5-T03, M5-T04, M7-T04|
|REQ-051|三插件兼容与安装|M5|required|v1|outline|AC-051-01（TEST-051-01·integration·M5-T03）|M5-T01, M5-T03, M5-T04, M7-T04|
|REQ-052|冻结候选与证据|M2|required|v1, v0.1|detailed|AC-052-01（TEST-052-01·integration·M2-T10）<br>AC-052-02（TEST-052-02·integration·M2-T10）<br>AC-052-03（TEST-052-03·integration·M2-T10）<br>AC-052-04（TEST-052-04·unit·M2-T10）<br>AC-052-05（TEST-052-05·traceability·M2-T10）|M2-T10, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-053|独立QA|M2|required|v1, v0.1|detailed|AC-053-01（TEST-053-01·integration·M2-T18）<br>AC-053-02（TEST-053-02·unit·M2-T18）<br>AC-053-03（TEST-053-03·integration·M2-T16）<br>AC-053-04（TEST-053-04·integration·M2-T18）<br>AC-053-05（TEST-053-05·adapter-live·M2-T18）|M2-T16, M2-T18, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-054|独立Review|M2|required|v1, v0.1|detailed|AC-054-01（TEST-054-01·integration·M2-T18）<br>AC-054-02（TEST-054-02·integration·M2-T18）<br>AC-054-03（TEST-054-03·integration·M2-T16）<br>AC-054-04（TEST-054-04·integration·M2-T18）<br>AC-054-05（TEST-054-05·unit·M2-T18）|M2-T16, M2-T18, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-055|回归与Flaky|M5|required|v1|outline|AC-055-01（TEST-055-01·integration·M5-T02）|M5-T01, M5-T02, M5-T04, M7-T04|
|REQ-056|最终发布门禁|M5|required|v1|outline|AC-056-01（TEST-056-01·integration·M5-T02）|M5-T01, M5-T02, M5-T04, M7-T04|
|REQ-057|运行时UX与部署验证|M5|required|v1|outline|AC-057-01（TEST-057-01·deployment·M5-T02）|M5-T01, M5-T02, M5-T04, M7-T04|
|REQ-058|Web GUI入口|M6|required|v1|outline|AC-058-01（TEST-058-01·web-e2e·M6-T02）|M6-T01, M6-T02, M6-T04, M7-T04|
|REQ-059|事件重连|M6|required|v1|outline|AC-059-01（TEST-059-01·web-e2e·M6-T02）|M6-T01, M6-T02, M6-T04, M7-T04|
|REQ-060|多客户端操作|M6|required|v1|outline|AC-060-01（TEST-060-01·web-e2e·M6-T02）|M6-T01, M6-T02, M6-T04, M7-T04|
|REQ-061|无障碍和远程体验|M6|required|v1|outline|AC-061-01（TEST-061-01·web-e2e·M6-T02）|M6-T01, M6-T02, M6-T04, M7-T04|
|REQ-062|持久化一致性|M2|required|v1, v0.1|detailed|AC-062-01（TEST-062-01·integration·M2-T12）<br>AC-062-02（TEST-062-02·integration·M2-T12）<br>AC-062-03（TEST-062-03·integration·M2-T04）<br>AC-062-04（TEST-062-04·integration·M2-T12）<br>AC-062-05（TEST-062-05·static·M2-T01）<br>AC-062-06（TEST-062-06·integration·M2-T12）|M2-T01, M2-T12, M2-T04, M2-T11, V01-T01, V01-T02, M7-T04|
|REQ-063|备份与迁移|M5|required|v1|outline|AC-063-01（TEST-063-01·recovery·M5-T02）|M5-T01, M5-T02, M5-T04, M7-T04|
|REQ-064|可观测性与容量|M4|required|v1|outline|AC-064-01（TEST-064-01·integration·M4-T03）|M4-T01, M4-T03, M4-T04, M7-T04|
|REQ-065|Git集成与交付|M6|required|v1|outline|AC-065-01（TEST-065-01·integration·M6-T03）|M6-T01, M6-T03, M6-T04, M7-T04|
|REQ-066|安装使用文档|M6|required|v1|outline|AC-066-01（TEST-066-01·deployment·M6-T03）|M6-T01, M6-T03, M6-T04, M7-T04|
|REQ-067|跨天稳定性|M7|required|v1|outline|AC-067-01（TEST-067-01·soak·M7-T03）|M7-T01, M7-T03, M7-T04|
|REQ-068|性能预算|M7|required|v1|outline|AC-068-01（TEST-068-01·integration·M7-T02）|M7-T01, M7-T02, M7-T04|
|REQ-069|交付真实性|M7|required|v1|outline|AC-069-01（TEST-069-01·traceability·M7-T02）|M7-T01, M7-T02, M7-T04|
|REQ-070|长期维护与升级|M7|required|v1|outline|AC-070-01（TEST-070-01·deployment·M7-T02）|M7-T01, M7-T02, M7-T04|

合计：70 项需求（27 项 detailed），166 条验收。v0.1 范围：18 项需求。
