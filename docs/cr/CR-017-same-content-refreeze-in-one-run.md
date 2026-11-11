# CR-017 · 相同内容重复冻结限定在同一 Run

ID：CR-017
状态：PROPOSED（待 ADR-027 第二版的设计审查；属 CR-003 D1 预授权的细化，审查判定 PASS 后由 CTO 批准，再改受锁文件并重新上锁）
批准人：（待定）
日期：2026-11-11
基线：修改受锁文件 `contracts/requirements.json`（REQ-052 决定 1），重新上锁

## 背景

REQ-052 决定 1：“产品中对相同内容重复冻结：选择返回已有 candidate_id，不新增行。”候选的规范身份不含 run_id（candidate.schema.json 不允许加字段，导出须与 tools/candidate.py 的身份逐字节一致），而 `candidates` 表以 candidate_id 为主键、每行属于一个 Run（`run_id NOT NULL`，001_initial.sql:29，受锁）。另一 Run 冻结相同内容时，返回已有 id 就是把属于旧 Run 的候选行（可能已 VERIFYING、ACCEPTED，带着它的证据与验收）交给新 Run：新 Run 的 `current_candidate` 读不到它（按 run_id），读到的话又会复用旧验证与验收，违背 CONTRACT 第 7 条（验收只属于当前候选的当前验证）。M2-T10 的设计审查（M2T10D-05、-63）指出决定 1 的字面与主键、CONTRACT 第 7 条不一致。

## 决定

REQ-052 决定 1 改为：“产品中对相同内容重复冻结：同一 Run 内返回已有 candidate_id 与它的状态，不新增行；已是 STALE、INVALID、REJECTED 的不复活；另一 Run 冻结相同内容时拒绝并说明（产生新的提交后再冻结），不复用另一 Run 的候选与它的验收。自举工具 tools/candidate.py 则拒绝覆盖已存在的清单文件。”

## 影响

- `contracts/requirements.json`：REQ-052 `decisions[0]`。
- 重新生成 `docs/02`、`docs/18`，重新上锁。
- ADR-027 决定 5。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets；AC-052-01 的“两次冻结同一 ID、导出数=1”在同一 Run 内照旧成立。
- 不放宽任何检查：另一 Run 的相同内容不是被接受，而是明确拒绝；验收不跨 Run 复用。
- 不改 CONTRACT 的架构与交互决定；不改协议、SQL 或 schema。

## 是否属于 CR-003 D1 预授权

属于第 1 条“修正规格之间的不一致”（决定 1 的字面与受锁 SQL 主键、CONTRACT 第 7 条），满足第 2 至 4 条（见“不变的部分”）。由独立审查判定后批准。

## 验证方式

- TEST-052-01：同一 Run 两次冻结得到同一 ID，candidates 行数为 1。
- 未打标签的测试：另一 Run 冻结相同内容被拒且不新增行；同 Run 内 STALE 的同内容不复活。
