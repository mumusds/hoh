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
- 第 4 条的嵌入规则：`include_str!`/`include_bytes!` 所写的路径（与源文件目录拼接后的规范形式）若是一个合格的 `embedded_markdown` 条目，且按内核解析的真实路径与之相同（不经任何符号链接），就不算命中。named-file 规则同样放过以同样方式指向合格条目的字面量（嵌入路径本身就是字面量，Skill 的路径字面量也指向同一文件）。
- 合格条目必须不含 SQL 写语句（INSERT/REPLACE INTO、UPDATE … SET、DELETE FROM、UPSERT、ON CONFLICT … DO UPDATE），只允许正文提到 SQLite。
- 第 6 条逐项核对 `embedded_markdown`。下列条目本身就是命中：没有写原因的；不是 `resources/` 下 Markdown 的（按规范路径）；不是上述普通文件的（不存在、是链接、经过链接或离开仓库）；可执行的；含 SQL 写语句的；根本不提 SQLite 的（无需条目）。
- 其余情形照旧命中：未列出的文件、列出的脚本或非 Markdown 文件、`resources/` 之外的文件、含 SQL 写语句的文件、符号链接（以及第 6 条对候选中任何符号链接的命中）、仓库之外的路径、无法核对的计算路径。
- 本 CR 不预先登记任何条目（`embedded_markdown` 为空）：M3-T08 在嵌入这三份 Skill 时按本规则逐个加入条目并写明原因，由它的审查核对。

## 影响

- 只有逐个列出、带原因、位于 `resources/` 下、只含说明文字的 Markdown 资源可以被产品嵌入或指向；脚本、含 SQL 写语句的文件与经链接到达的文件不受影响。
- 嵌入命中报告的文件名改为源码所写路径的文件名（此前为解析后的文件名）；无符号链接时二者相同，现有用例不变。
- 列入 `embedded_markdown` 的文件仍须同时列在 `files` 中（第 6 条的内容规则不变）。

## 不变的部分

- 第 1 至 3 条、第 5 条、第 7 条，以及第 4、6 条的其余规则不变；CR-019 的 `read_as_data` 规则与其测试不变；Persistence 模块之外依然不能打开数据库或执行 SQL。
- 不删除任何需求或 AC，不改 required/degradable，不改 CONTRACT 的架构与交互决定，不改受锁文件。

## 验证方式

- `tests/bundle/test_tools.py::PersistenceScan::test_listed_embedded_markdown_under_resources_is_no_hit_and_nothing_else_is`：列出的、正文提到 SQLite 的 `resources/` Markdown 被嵌入与指向时不命中；未列出的 Markdown、列出但含 SQL 写语句的、指向列出文件的符号链接、经链接离开仓库的路径、可执行文件、脚本与 `resources/` 之外的 Markdown 照旧命中；对应的白名单条目（含无原因、不提 SQLite 的）本身命中。改动前该测试失败（红）。
- `tests/bundle/test_tools.py::PersistenceScan::test_listed_embedded_markdown_holding_any_sql_write_is_refused`：列出的 Markdown 含 INSERT、UPDATE 或 DELETE 语句时，嵌入、指向与条目都命中；去掉写语句后扫描通过。改动前该测试失败（红）。
- `tests/bundle/test_tools.py::PersistenceScan::test_a_listed_data_file_read_as_data_is_no_named_file_hit_and_nothing_else_is`（CR-019）与其余扫描用例保持通过。
- `bash scripts/check.sh --bundle`、`bash scripts/product/static.sh` 与 `bash scripts/dev/pre-pr.sh --no-deployment` 退出 0。

## 审查记录

（待独立审查后填写：候选、审查者、发现与处理。）
