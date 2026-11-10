# CR-014 · 角色交接包在 Run 尚无 spec 时以 null 表示 spec

ID：CR-014
状态：APPROVED
批准人：项目所有者（2026-11-10，M2-T17 第 2 轮审查期间所问，选择「CR：spec 可为 null」）
日期：2026-11-10
基线：修改受锁文件 `contracts/protocol.schema.json`（ContextHandoff）与 `contracts/requirements.json`（REQ-019 行为 4），重新上锁

## 背景

REQ-018 行为 5 要求团队修改在下一个 checkpoint 生效，没有“Run 已有 spec”的前提。REQ-019 行为 2、4 要求每个职位换给新持有者时在同一事务里写交接事件与新会话启动的 outbox，新持有者的首个输入是 checkpoint 交接包，包含“当前 spec_sha256/revision”。而 `protocol.schema.json` 的 ContextHandoff 对所有种类都要求 `spec_sha256`（64 位十六进制）与 `spec_revision`（≥1）。

产品中 StartRun 不写 spec（spec 由 ProposeSpecChange/ApproveSpec 带来，属 M3），所以产品里的 Run 在批准 spec 之前都没有 spec。这时三条规定不能同时满足：要么做不出合法的交接包，要么修改不能生效。M2-T17 起初让 Commander 的修改以 NO_SPEC 拒绝，第 1 轮审查后扩展到所有职位；第 2 轮审查判定这使产品里任何团队修改都无法生效，违反 REQ-018 行为 5（M2T17R-100，high；M2T17R-155）。PLAN 阶段（尚无 spec）更换 Commander 也会无法开始。

## 决定

1. ContextHandoff 的 `spec_sha256` 可为 string 或 null，`spec_revision` 可为 integer 或 null。`handoff_kind` 为 `verification` 或 `task` 时两者必须有值（与原来相同）；为 `role_change` 时两者要么都有值，要么都为 null。两者为 null 只表示 Run 此刻没有 spec，不编造摘要。
2. REQ-019 行为 4 的“当前 spec_sha256/revision”补一句：Run 尚无 spec 时两者为 null。
3. 实现：checkpoint 不再因 Run 没有 spec 拒绝修改（去掉 NO_SPEC 拒绝），任何职位的修改照常在 checkpoint 生效并交接；交接包的 spec 取 Run 此刻的 spec，没有就是 null。

## 影响

- `contracts/protocol.schema.json`：ContextHandoff 的两个属性与三个条件分支。
- `contracts/requirements.json`：REQ-019 `behavior[3]`。重新生成 `docs/02`、`docs/18`，重新上锁。
- `tests/fixtures/schema/corpus.json`：新增四个用例（role_change 两者为 null 有效；role_change 只有一个为 null 无效；verification、task 为 null 无效）。
- `hoh_core::protocol::ContextHandoff` 的两个字段改为 Option；验证与任务交接包照旧填值；consume 对交接包的 spec 核对按 Option 比较。
- docs/adr/ADR-024 决定 10 改写；M2-T07 中为“非 Commander 修改”加 spec 的测试夹具恢复原来无 spec 的前提。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 验证与任务交接包对 spec 的要求不变；consume 的 SPEC_MISMATCH 栅栏对它们不变。
- 交接包仍只放 id，不读原始日志与会话。

## 验证方式

- schema 用例集在 Rust（schema_corpus）与 Python（bundle 自检）两边判定一致。
- 无 spec 的 Run：任何职位的修改在 checkpoint 生效，交接包通过 schema 校验、spec 为 null；有 spec 的 Run 行为不变（TEST-019-02）。
- M2-T07 的测试在无 spec 的 Run 上照原样通过。
