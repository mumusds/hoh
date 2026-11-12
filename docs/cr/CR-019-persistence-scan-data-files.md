# CR-019 · 持久化边界扫描：产品可以按数据读取已登记的规格文件

ID：CR-019
状态：APPROVED
批准人：项目所有者（2026-11-12，在 M2-T16 主开发会话中对“static 检查与规格文件”问题明确选择本方案）
日期：2026-11-12
基线：不改受锁文件；改 `scripts/product/persistence_scan.py`（不在锁内）与 `scripts/product/persistence-whitelist.json`

## 背景

AC-062-05 的持久化边界扫描（`scripts/product/persistence_scan.py`，计划检查 `static`）的第 4 条规定：成员 Rust 源码里的字符串字面量若指向仓库中一个提到 SQLite 或含 SQL 写语句的文件，就算命中（named-file，M2T01R-45），用来防止产品代码借另一个文件（例如 `python3 scripts/db_helper.py`）越过持久化边界。

M2-T16 的写者完成按 CR-018 的项目布局约定，在 M3-T02 给出规格事实源的位置之前，从 `contracts/requirements.json` 冻结规格（ADR-028 决定 4）。这份规格的正文提到 SQLite（需求文字），所以 `crates/hoh-app/src/host/completion.rs` 被判命中，候选 m2t16-001 的 `static` 检查失败。规格是 JSON 数据，冻结只把它的字节存为对象、解析其 JSON，从不运行它，也不可能借它到达 SQLite（rusqlite 只在 Persistence 模块内可用，见第 1、2 条）。

这次修改放宽了一条检查，不满足 CR-003 D1 的第 3 条，所以由项目所有者批准。

## 决定

- 白名单配置 `scripts/product/persistence-whitelist.json` 新增 `read_as_data`：路径到原因的映射，列出成员字面量可以指向的、按数据读取的文件。
- 第 4 条的 named-file 规则：字面量指向的文件若列在 `read_as_data`、是 JSON 或 Markdown 文件，且不含 SQL 写语句，就不算命中。其余情形照旧命中：没有列出的文件、列出的脚本、列出但含 SQL 写语句的文件。
- 第 6 条逐项核对 `read_as_data`。下列条目本身就是命中：没有写原因的、不是 JSON 或 Markdown 的、不是仓库文件的（或是链接）、含 SQL 写语句的。
- 首个条目：`contracts/requirements.json`，原因是“写者完成按 CR-018 的布局路径冻结的规格，只按字节与 JSON 读取”。
- 测试代码不使用这一放宽：M2-T16 的单元测试改为经 `inputs()` 取得布局路径，夹具源文件改用不与仓库文件同名的路径。

## 影响

- 只有列出的、带原因的数据文件可以被产品字面量指向；脚本与含 SQL 写语句的文件不受影响。
- M3-T02 给出规格事实源的位置后，若产品不再指向 `contracts/requirements.json`，该条目也应删除（第 6 条不自动报告“不再被指向”，删除由 M3-T02 的审查核对）。

## 不变的部分

- 第 1 至 3 条（依赖、clippy 的 disallowed-methods 边界、rusqlite 特性）、第 5 条与第 6 条的其余规则不变；Persistence 模块之外依然不能打开数据库或执行 SQL。
- 不删除任何需求或 AC，不改 required/degradable，不改 CONTRACT 的架构与交互决定。

## 验证方式

- `tests/bundle/test_tools.py::test_a_listed_data_file_read_as_data_is_no_named_file_hit_and_nothing_else_is`：列出的 JSON 与 Markdown 规格不命中；未列出的数据文件、列出但含 SQL 写语句的文件、列出的脚本照旧命中；含写语句的条目、脚本条目和没有原因的条目都是白名单命中。改动前该测试失败（红）。
- 在 M2-T16 的候选上，`bash scripts/product/static.sh` 退出 0；测试夹具不再指向任何提到 SQLite 的仓库文件。
