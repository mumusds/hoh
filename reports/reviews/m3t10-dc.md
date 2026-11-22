# M3-T10 设计修正与第三轮返工

范围：基线 `7c0f87cabc439cd093b1625419b690e819d9e9d0`，只处理第三轮 M3K-200 与同路径的 M3K-201。设计提交 `8ef8ef4f` 采用 CTO 的 DC-T10，见 ADR-037。本文是开发者自测记录，不是第四轮独立审查或验收。

| 发现 | 处理 | 回归证据 |
| --- | --- | --- |
| M3K-200 high | `budget_view` 在事务内构造统一快照，分开普通预留与逐咨询的持有量、消费量。咨询忽略全部未消费咨询预留，普通工作仍受保护。两条派发入口、pace、恢复、stall (e) 与两处 wake 门使用同一规则。全部持有者被自身边界挡住时不能用它们阻止预算终止。 | `test_023_01_dc_two_default_consults_in_critical_interval`、`…three…`、`…no_admissible_holder_stops_run`、`…stall_and_both_wake_gates_share_admission`、`…budget_call_sites_use_one_view` |
| M3K-201 low | Runtime 停止以 NEED_DECISION 标记待处置，诊断仍保留 budget_exhausted 和具体边界；父任务当前 Attempt 可通过 dismiss_consult query 结束自己的已停止咨询。拒绝未停止、非本人、过期 epoch/规格；重放不重复结算。投递文本含处置请求。 | `test_023_01_dc_parent_can_dismiss_only_its_stopped_consult`、`test_023_01_exhausted_consultation_returns_a_diagnostic_and_releases_reservation` |

预算视图的边界回归还覆盖：消费位于临界区间起点、中间、接近上限；普通预留；task deadline；重启；实际用量释放后仍保留；下一 Attempt 的已知估算为 1，剩余额度不足 1 时不派发，恰好为 1 时可派发。秒、token、美元没有逐次估算字段，不把调度评分 cost_estimate 当成这些单位。M3K-100/101 的 iterations 排除、自身预留、运行中时长、已消费额度、两条派发入口与重启用例保持。

持久账本只有 wall_seconds 与候选 iterations 原有转账。咨询的 attempts、tokens、usd 消费从 task/Attempt 事实读入统一视图，包括已结束咨询；释放只移除预留，不抹去消费。未知用量不按零消费放行。

证据在 `.hoh-dev/self-tests/m3t10-dc/`：

- `baseline-m3t10`：基线既有集成用例通过。
- `red-200-holders`：两个、三个持有者与全部持有者受限的三个断言失败，exit 101。
- `red-200-sites`：源码搜索发现旧的分散计算，exit 101。
- `red-201-dismiss`、`red-201-reason`、`red-201-parent-text`：缺少处置入口、原因为空、投递文本缺少处置指引，均 exit 101。
- `red-200-consumed`、`red-200-estimate`：释放抹去消费、未计入下一 Attempt 的单位，均 exit 101。
- `green-focused-1` 是实现中的 u64/SQLite 类型编译失败，不计行为红灯。后续修正未改变测试期望。
- 每次运行的 `.json` 保存 argv、HEAD、时间、退出码及 stdout/stderr SHA-256。最终提交前检查、冻结候选与计划内检查由同目录的后续记录绑定。

AC 映射：本次行为修复覆盖 AC-023-01，并保护 REQ-040 的监督与停滞路径。AC-023-02/03/04 和 AC-040-01/02/03/04 沿用现有同 TEST 编号测试；`m3t10` 集成、核心库及最终全工作区回归均包含它们。FakeAgent/脚本化夹具仍标为产品自测，不作为 live Agent 证据。

不修改 AC、requirements、检查计划、baseline 或 Gate；不运行 live Agent、不推送、不推进全局 taskboard。M3K-103 的既有查询性能余项仍按第二轮记录留给 M3 low 清理包。本次交付供第四轮审查，验收与项目完成状态由 Commander/QA/Reviewer 的独立流程判定。
