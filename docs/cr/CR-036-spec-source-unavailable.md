# CR-036 · 规格源读取失败的可重试原因码与 CR-029 历史更正

ID：CR-036
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权（独立 Review 会话见审查记录）
日期：2026-11-24
关联：M3 low-cleanup L2、M3B-07、M2R-501、REQ-034（AC-034-03）、REQ-004、REQ-053、REQ-054

## 背景

M3-T02 S1–S3 的独立审查 M3B-07（`reports/reviews/M3.md`，Claude attempt
`d9717739-1bef-41e1-a155-fffa6d9dfa84`）指出：读取工作区规格时，`git ls-files`
失败和文件系统 I/O 错误也成为 `IndexError`，随后返回不可重试的 422
`SPEC_INVALID` 并写 `spec.index_failed`。这把暂时无法取得源文件误报成了规格内容无效。
现有 ErrorCode 没有适合此类失败的值，不能借用数据库原因码。

## 决定

1. `contracts/protocol.schema.json` 的 ErrorCode 增加 `SPEC_SOURCE_UNAVAILABLE`。
   `ApproveSpec`、`ProposeSpecChange` 及答复规格批准问题所需的工作区索引读取中，
   Git 枚举或文件系统读取失败返回 HTTP 503、`retryable=true`；不接受该命令，
   不写 CR、修订或 `spec.index_failed`，恢复读取后可用同一命令重试。
2. HTTP 响应沿用 DB_BUSY 的 Error 结构：`code`、`message`、`retryable`、
   `current_revision`。现有 DB_BUSY 路径没有 `Retry-After`，本次也不增加该头。
   OpenAPI 的命令 503 响应说明补齐此原因码；Error.code 已引用协议 ReasonCode，
   无需另建一份枚举。客户端沿用 503 重试与退出码 69，TUI 保留具体错误码和消息。
3. 真正无效的规格（语法、文件形状等索引校验失败）仍为 422 `SPEC_INVALID`，
   保留原有诊断事件与原子性。提议已提交后索引发布失败仍沿用已有的恢复路径，
   不把已提交命令伪装成“未写入”。
4. Rust ErrorCode 同步增加枚举镜像，保持 schema roundtrip 一致；行为修复另行
   先红后绿提交，不改变现有检查的断言。

## M2R-501：CR-029 的历史更正

`reports/reviews/M2.md` 的 M2R-501 指出 CR-029 正文仍写“尚未重锁”和“本文保持
DRAFT”。这些是起草阶段遗留文字，不代表最终状态：提交 `c72a6773`
（`CR-029 APPROVED by the CTO under CR-003 D1 after its independent review; baseline relocked at revision 21 (M2R-001, REQ-053, REQ-054)`）
已批准 CR-029，并将它纳入 `contracts/baseline-lock.json` revision 21 的
`change_requests`。该历史锁记录的 CR-029 SHA-256 为
`fc5e72eace73adef738c44e76142e48ebfe6867800a7ba3830d7b59259c641dd`。
据此更正为“已批准并完成 revision 21 重锁”。CR-029 是不可变历史记录，本次不修改它。

## 影响与不变内容

- 本次受锁内容仅修改 `contracts/protocol.schema.json` 与 `contracts/openapi.json`，
  并通过正式 relock 更新 `contracts/baseline-lock.json`、登记本 CR。
- requirements/AC 没有需要扩展的规格命令错误码穷举表；它们描述的语法失败、冲突与
  冻结失败保持原行为。REQ-004 的退出码按客户端结果分类，既有 host 不可用 69
  已覆盖 503，不新增退出码行，不修改 requirements 或生成的 AC 视图。
- 不删除需求、AC、测试或检查，不改变 release-policy、检查计划、发布范围、证据
  规则或任何门禁阈值；不改变 CONTRACT 的架构和交互决定。
- 属于 CR-003 D1 第 1 项“补齐缺失的原因码”，并更正历史状态说明，不缩小范围、
  不降低门槛。审查记录由主会话补录，不把开发者自测写成独立验收。

## 验证方式

在本工作区执行 `python3 tools/gen_spec_views.py`，随后执行
`python3 tools/baseline.py relock --cr docs/cr/CR-036-spec-source-unavailable.md`，
以 `bash scripts/check.sh --bundle` 退出 0 为 CR 提交前提。
协议镜像用 `cargo test -p hoh-core --locked --test protocol_roundtrip` 核对。
行为提交以读取权限故障和 Git 索引读取故障复现 422，再验证 503、原因码、可重试、
不写诊断或业务状态、恢复后同一命令成功；保留非法索引的 422 对照。
最终提交运行 `bash scripts/dev/pre-pr.sh --no-deployment`，结果和原始日志保存在
`.hoh-dev/self-tests/m3low-e/`。以上均为自测，非验收。

## 审查记录

| 轮 | 冻结提交 | 审查者 | 结论 | 发现与处置 |
|---|---|---|---|---|
