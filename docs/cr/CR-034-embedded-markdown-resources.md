# CR-034 · 持久化边界扫描：产品可以嵌入已登记的 resources/ Markdown

ID：CR-034
状态：APPROVED
批准人：项目所有者（2026-11-22，对 M3-T08 嵌入内置 Skill 文本被持久化边界扫描挡住一事，明确批准本条窄例外）
日期：2026-11-22
基线：不改受锁文件；改 `scripts/product/persistence_scan.py`（不在锁内）、`scripts/product/persistence-whitelist.json` 与 `docs/22-tooling.md`（均不在锁内）

## 背景

AC-062-05 的持久化边界扫描（`scripts/product/persistence_scan.py`，计划检查 `static`）的第 4 条规定：成员非测试 Rust 源码以 `include_str!`/`include_bytes!` 嵌入的文件，以及字面量所指向的仓库文件，都不得提及 SQLite 或含 SQL 写语句，用来防止产品代码借嵌入的脚本或被运行的文件越过持久化边界（M2T01R-41、-45）。CR-019 只为“字面量指向、按数据读取的 JSON/Markdown 文件”开了 `read_as_data` 例外，嵌入规则没有对应例外。

M3-T08 按 REQ-046 与绑定决定 Q11 把内置 Skill 文本（`resources/skills/*/SKILL.md`）嵌入二进制。其中 `architecture`、`recovery`、`requirements` 三份的正文把 SQLite 作为技术名词或故障类别提到（它们早已列在白名单 `files` 中），于是 `objects.rs` 得到 3 条 included-file 命中与 6 条 named-file 命中（嵌入路径字面量与 Skill 路径字面量各一），`static` 退出 1，M3-T08 S1 无法提交（阻塞分析见 M3-T08 早期自测记录 BLOCKER.md）。这些文件是说明文字，产品把它们的字节作为提示内容交给 Agent，从不运行它们，也不可能借它们到达 SQLite（rusqlite 只在 Persistence 模块内可用，见第 1、2 条）。

这次修改放宽了一条检查，不满足 CR-003 D1 的第 3 条，所以由项目所有者批准。

## 决定

- 白名单配置 `scripts/product/persistence-whitelist.json` 新增 `embedded_markdown`：路径到原因的映射，逐个列出产品代码可以嵌入的 Markdown 资源。
- 文件身份按其自身路径判定：条目路径必须是规范形式（无 `.`、`..`、重复分隔符）、位于 `resources/` 下、以 `.md` 结尾；该路径在仓库内是普通文件（不是符号链接、目录或其他类型），其真实路径就是它自己（路径上没有任何符号链接），且不可执行。
- 第 4 条的嵌入规则：`include_str!`/`include_bytes!` 所写的路径（与源文件目录拼接，未规范化）从仓库根起逐段核对：在应用每个 `..` 之前，已走过的每一段（目录或文件）都必须存在且不是符号链接，`..` 只撤回真实目录，在仓库根处的 `..` 即离开仓库；这样走到的普通文件若是一个合格的 `embedded_markdown` 条目，就不算命中。只比较规范形式与解析后的真实路径不够：`resources/skills/jump/../listed/SKILL.md` 在 `jump -> listed` 时解析结果恰是它自己的规范形式，却经过了链接（审查发现 CR34-50）。named-file 规则同样放过以同样方式指向合格条目的字面量（嵌入路径本身就是字面量，Skill 的路径字面量也指向同一文件）。
- 合格条目必须不含 SQL 写语句（INSERT/REPLACE INTO、UPDATE … SET、DELETE FROM、UPSERT、ON CONFLICT … DO UPDATE），只允许正文提到 SQLite。
- 第 6 条逐项核对 `embedded_markdown`。下列条目本身就是命中：没有写原因的；不是 `resources/` 下 Markdown 的（按规范路径）；不是上述普通文件的（不存在、是链接、经过链接或离开仓库）；可执行的；含 SQL 写语句的；根本不提 SQLite 的（无需条目）。
- 其余情形照旧命中：未列出的文件、列出的脚本或非 Markdown 文件、`resources/` 之外的文件、含 SQL 写语句的文件、符号链接（以及第 6 条对候选中任何符号链接的命中）、仓库之外的路径、无法核对的计算路径。
- 本 CR 不预先登记任何条目（`embedded_markdown` 为空）：M3-T08 在嵌入这三份 Skill 时按本规则逐个加入条目并写明原因，由它的审查核对。

## 影响

- 只有逐个列出、带原因、位于 `resources/` 下、只含说明文字的 Markdown 资源可以被产品嵌入或指向；脚本、含 SQL 写语句的文件与经链接到达的文件不受影响。
- 嵌入命中与 named-file 命中的报告格式不变（嵌入命中仍报解析后的文件名）。
- 列入 `embedded_markdown` 的文件仍须同时列在 `files` 中（第 6 条的内容规则不变）。

## 不变的部分

- 第 1 至 3 条、第 5 条、第 7 条，以及第 4、6 条的其余规则不变；CR-019 的 `read_as_data` 规则与其测试不变；Persistence 模块之外依然不能打开数据库或执行 SQL。
- 不删除任何需求或 AC，不改 required/degradable，不改 CONTRACT 的架构与交互决定，不改受锁文件。

## 验证方式

测试分三类（审查发现 CR34-51）：允许的正例只在新扫描器上通过；照旧的拒绝只比较已有的 included-file、named-file 与 symlink 命中，在改动前后的扫描器上都通过；第 6 条对条目的新诊断只在新扫描器上通过。

- `tests/bundle/test_tools.py::PersistenceScan::test_listed_embedded_markdown_under_resources_is_no_hit`（正例）：列出的、正文提到 SQLite 的 `resources/` Markdown 经不过链接的 `..` 路径被 `include_str!`/`include_bytes!` 嵌入、并被仓库根相对的字面量指向时，扫描通过。改动前失败（红）。
- `…::test_embedded_files_the_exception_does_not_cover_keep_their_hits`（照旧的拒绝）：未列出的 Markdown、列出但含 SQL 写语句的、指向列出文件的符号链接、经链接离开仓库的路径、可执行文件、脚本、`resources/` 之外的 Markdown，以及经过目录链接的路径（含链接后用 `..` 落到列出文件的 `jump/../listed/SKILL.md`，链接在候选中与被 git 忽略两种情形）照旧命中。改动前的扫描器通过；第 1 轮候选 `bd230012` 在 `jump/../listed` 两种情形上失败（CR34-50 的红），修复后通过。
- `…::test_embedded_markdown_holding_any_sql_write_keeps_its_hits`（照旧的拒绝）：列出的 Markdown 含 INSERT、UPDATE 或 DELETE 语句时，嵌入与指向照旧命中；改动前后都通过。
- `…::test_every_embedded_markdown_entry_is_checked`（新诊断）：无原因、非规范路径、非 `resources/` Markdown、文件链接、经目录链接、不存在、离开仓库、可执行、含 SQL 写语句、不提 SQLite 的条目各自命中，合格条目不命中。改动前失败（红）。
- `tests/bundle/test_tools.py::PersistenceScan::test_a_listed_data_file_read_as_data_is_no_named_file_hit_and_nothing_else_is`（CR-019）与其余扫描用例保持通过。
- `bash scripts/check.sh --bundle`、`bash scripts/product/static.sh` 与 `bash scripts/dev/pre-pr.sh --no-deployment` 退出 0。

## 审查记录

| 轮 | 冻结提交 | 审查者 | 结论 | 发现与处置 |
|---|---|---|---|---|
| 1 | `bd230012`（CR-034: persistence scan accepts listed embedded Markdown resources） | Grok，会话 ['01a204d5-42f1-77b0-87c8-a498edacfd5c']（attempt `0017419a-63d2-4778-bc51-fbba29082680`） | PASS | 无。 |
| 1 | `bd230012`（同上） | Codex，会话 ['01a204d5-3d9c-7023-a4a4-d000787bc404']（attempt `00ee4613-22aa-46da-9870-0af53b471446`） | FAIL | CR34-50 high：规范路径与解析路径相等不能证明未经过链接，`jump -> listed` 时 `../../../resources/skills/jump/../listed/SKILL.md` 被放过（链接被 git 忽略时整个扫描通过）。已修复：逐段核对、拒绝任何链接段（`linkless_file`），条目核对共用同一判断；补回归（链接在候选中、被忽略、链接后的 `..`）。CR34-51 medium：拒绝测试混入新配置诊断，在旧扫描器上并非只因正例而红。已修复：测试拆为正例、照旧的拒绝（只比较已有命中，新旧扫描器都绿）与新诊断三类；嵌入命中恢复报告解析后的文件名，使照旧的拒绝在新旧扫描器上输出一致。修复提交：CR-034 fix: CR34-50 CR34-51 refuse any link in an embedded path; separate refusal tests。 |
| 2 | `f8a8c435`（修复差异） | Codex，会话 `01a204ff-7752-7ed1-a78c-5803ffb22498`（attempt `ff9d204d-09ac-4720-8eeb-9461ec1d348c`） | PASS | CR34-100 medium：`..` 只排除了链接，未确认被撤回的分量是目录，`<已登记文件>/../SKILL.md` 被放过（编译器会以 ENOTDIR 拒绝，未构成持久化边界绕过）。按审查收敛规则由 CTO 先红后绿修复并核对：`..` 只撤回真实目录；回归 `test_a_dotdot_after_a_file_is_not_a_directory_undone`（文件后的 `..` 保持命中，目录后的 `..` 为正对照）。 |
