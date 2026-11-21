# CR-031 · M6 的 6 项需求细化为 detailed，与 M6 实现任务的拆分

ID：CR-031
状态：DRAFT
批准人：待定（CTO 依 CR-003 D1 预授权，须在独立设计审查 PASS 之后；本稿作者不批准自己）。C1 已由所有者于 2026-11-22 批准，C2–C20 已由 CTO 采纳（见“CTO 的问题与决定”）
日期：2026-11-22（第 2 稿；第 1 稿为 `05b2ef6c`，设计审查第 1 轮的处置见“审查记录”）
基线：拟改受锁文件 `contracts/requirements.json`（REQ-058、059、060、061、065、066 由 outline 细化为 detailed，共 23 条 AC；REQ-018 增加一条决定：`[git]` 配置键）、`contracts/acceptance-scenarios.json`（这 6 项的 23 个场景）、`contracts/protocol.schema.json`（`$defs.ErrorCode` 增加 `WEB_ASSETS_UNAVAILABLE`）、`contracts/openapi.json`（增加 Web 外壳的两条路径 `/` 与 `/assets/{asset}`，`/v1/events` 的说明增加 cursor_ahead 一句）；不在锁内、随 CR 一起改：`contracts/development-tasks.json`（M6 实现任务的拆分）。不新增受锁契约，不新增 SQL 迁移。批准后 `python3 tools/gen_spec_views.py` 重新生成 docs/02 与 docs/18，`python3 tools/baseline.py relock --cr docs/cr/CR-031-m6-detailed-spec.md`；同一提交须把 `WEB_ASSETS_UNAVAILABLE` 加进 Rust 的 `ErrorCode` 枚举（`crates/hoh-core/src/protocol.rs`，protocol_roundtrip 要求与 schema 逐项相等）并运行完整的 `cargo test --workspace --locked`（docs/24 第 5 节）

本 CR 是 M6-T01（“细化 M6 需求为 detailed：每项 2–5 条可判定 AC 与场景，写 CR 并 relock 基线”）的 CR，**草稿，未批准、未应用**。M6-T01 依赖 M5-T04，尚未领取；本 CR 的提交不改任何受锁文件。下文第 3–7 节的片段由应用脚本从本文的 JSON 块直接取出，已在仓库之外的副本上应用并通过 `tools/validate_bundle.py` 与 bundle 自检（见“验证方式”）。本稿的输入：main `8d9ae599`（基线 revision 22，CR-026 已应用）、已批准未应用的 CR-028（`prep/M4-T01-spec` 的 `dde5109f`），以及仍在设计审查中的 CR-030 第 1 稿（M5，`prep/M5-T01-spec` 的 `6c0fc5b6`）。凡依赖 CR-028 或 CR-030 的地方都列在“对未应用规格的假设”中，M6-T01 领取时逐条重核；行号只作 `8d9ae599` 上的提示，以引文与函数名为锚。

## 背景

M6 的 6 项需求（REQ-058、059、060、061、065、066）都是 `spec_maturity=outline`、`required`、`release_targets=[v1]`、P1：每项只有 summary 与一条占位 AC（例如 AC-058-01 的 then 照抄 summary，TEST-065-01 的 fixture 为“待该里程碑 spec 任务细化”），没有 behavior、failure_modes、decisions、doc_refs。CONTRACT 第 17 条要求 outline 在实现前细化为 detailed，不得删范围、降门槛。M6-T01 的 done_when 另要求按每个实现任务至多 2 项需求、8 条 AC 拆分本里程碑的实现任务，每条 AC 只分给依赖闭包已提供其所需能力的任务（CR3R-02），verify 任务在最后。

`8d9ae599` 上与 M6 相关的现状（逐项核对过）：

- `hoh web` 只解析参数，执行时以 3 退出并打印 NOT_IMPLEMENTED（`crates/hoh-client/src/lib.rs` 的 `Invocation::Web` 分支）；`hoh web --help` 已写“启动或连接本项目的 host，并给出本机 Web 界面的地址”。
- host 已在 `127.0.0.1:<port>`（缺省 8787，被占用时取空闲端口并写进 host.json）与 `.hoh/host.sock` 上提供同一组路由；TCP 上每个请求的 Host 必须恰为 `127.0.0.1:<port>`、Origin 缺省或为 `http://127.0.0.1:<port>`，除 `/healthz` 外都要 `X-HoH-Token`（`crates/hoh-app/src/host/http.rs` 的 `check_request`）。`[web] host` 只接受 loopback 地址，`0.0.0.0` 被配置加载器拒绝（`crates/hoh-core/src/config/tests.rs`）。
- host 每次启动都生成新的 token（`crates/hoh-app/src/host/mod.rs` 的 `token()`：32 个随机字节的十六进制，写进 host.json），旧 token 在新 host 上一律 403；CLI 与 TUI 重读 host.json 取得新 token，浏览器页面读不到 host.json（第 1 稿审查 CR31D-01）。`/healthz` 不要 token，只返回 `{status}`。
- openapi 的 `localToken` 规定：`hoh web` 用 URL 片段中的 token 打开浏览器，页面只把它留在内存中；SSE 也用该头。`/v1/events` 的 409 与 410（`CURSOR_EXPIRED` 带顶层 `snapshot_watermark`）已声明；ADR-016 决定 7 写明 M2 不清理事件，所以 410 不会出现；`after` 大于当前水位的游标今天被接受，之后 seq 不超过它的新事件会被跳过。
- 写命令都带 `expected_revision`，不符为 409 `STALE_REVISION`（Run 与团队两种实体），幂等重放与 `KEY_REUSE` 已有（ADR-016 决定 4、6）；CLI 在 `STALE_REVISION` 时重读重提（`hoh-client/src/client.rs` 的 `fresh`，ADR-031 决定 9）。
- 产品没有任何分支、push、PR 的代码；候选从工作树的 `HEAD` 冻结（`crates/hoh-app/src/verify/freeze.rs`），验证在 detached 的运行目录中进行。`[delivery]` 表已被 HCP 消息投递占用（`max_delivery_attempts`），所以本 CR 的 Git 配置放在新的 `[git]` 表中。
- build 检查在有 `web/package.json` 时执行 `npm ci` 与构建，否则只打印提示（`scripts/product/build.sh`，注释写明 M6-T02 须把缺少 web/ 改为失败）；`bundle.sh` 已在 `web/dist` 存在时把它打进 `hoh/web`。web-e2e 检查的 argv 是 `not-implemented.sh web-e2e M6-T02`，归属 M6-T02。
- deployment 检查在私有 `systemd --user` 管理器与临时 HOME 中跑 TEST-005-06，没有全新 VM；`scripts/dev/test-infra.sh` 已装 qemu/KVM、cloud-image-utils 与 Playwright 的系统依赖（`.hoh-dev/test-infra.json`：`playwright_install_deps` 1.64.1、`kvm_group_configured`）。
- 本 VM 为 Ubuntu 26.04.1，`kernel.apparmor_restrict_unprivileged_userns = 1`：`unshare -rn` 以 “write failed /proc/self/uid_map: Operation not permitted” 失败，无特权的网络命名空间不可用（C11 的依据）。

## 所有者已定的事项与约束（照录，不再讨论）

| 来源 | 决定 | 本 CR 中的落点 |
|---|---|---|
| CONTRACT 第 2、3 条 | Web 用 TypeScript/React，由 Axum host 服务；Shell 面只有 `hoh`、`hoh -p`、`attach`、`web`、`doctor` | REQ-058 行为 1、5；不增加子命令 |
| CONTRACT 第 13 条 | TUI/Web 是客户端，关闭客户端不停止已接受的工作 | REQ-058 行为 3 与 AC-058-01；REQ-060 行为 1 |
| CONTRACT 第 15 条 | 首发支持 Ubuntu 24.04/26.04 x86_64 VM，其他平台不得无测试宣称支持 | REQ-066 行为 5 与 AC-066-01 |
| docs/11、docs/03 | 默认监听 127.0.0.1，远程经 SSH 端口转发，不加多租户网关；每个 API 请求带 host 的本地 token 并校验 Host/Origin | REQ-061 行为 4；REQ-058 行为 2（外壳见 C1） |
| 所有者 O1（CR-026） | 只有用户批准规格；对端无法确认即拒绝 | Web 的批准走 CR-026 的同一批准应用点；AC-061-03 经 SSH 转发的批准来源为 user |
| 所有者，“已推送分支只追加提交”（docs/24 第 5 节） | 不改写已发布的历史，不 force push | REQ-065 行为 4、7 与 AC-065-02、-05 |
| CR-024 | Commander 只读，不写工作区 | 交付由 Runtime 执行，不经 Commander；CI 失败只唤醒 Commander |
| CR-028 D1（已批准） | 读模型绑定 watermark H；SSE 的 409/410 形状保留；v1 不删除事件，SSE 的 410 在产品中不可达；分页游标的 generation 失效为可达的 410；超过已提交水位的分页游标按无效处理 | REQ-059 行为 4、5 与 AC-059-03；C3 |
| 所有者的保守缺省（O1–O6 的同一取向） | 对外、不可逆的动作要用户显式选择 | REQ-065：缺省本地交付，HoH 不合并 PR（C6、C7） |

## 决定

### 1. 共同约定（6 项共用）

- **同一 Engine**：Web 是 host 的又一个客户端，与 TUI 同一 CommandService（docs/03）。页面只经 `/v1` 读写，不读 SQLite、不读项目文件、不持有租约；`hoh web` 不启动第二个 host 或第二个循环。读模型完全沿用 CR-028 的约定：快照 H、按需分页、SSE `after=H`、块以 `block_id` 为身份、客户端缓存上限（1000 个块描述、4 MiB 正文、2 个在途页），本 CR 不另建 Web 专用的读接口。
- **外壳与 token（C1，所有者已于 2026-11-22 批准）**：`GET /` 与 `GET /assets/{asset}` 提供构建产物，不要 token、仍校验 Host/Origin；每个 `/v1` 请求（含 SSE）都要 token；token 只在 URL 片段与页面内存中，从不进入请求行、host 日志或 Referer。docs/03 第 51 行的“每个请求都要携带……token”在 M6-T01 应用本 CR 时改写为“每个 API 请求”，并写明外壳不含项目数据。
- **规范地址（C22）**：`[web] host` 的取值 v（加载器只接受 `localhost` 与 loopback IP，REQ-018）决定绑定地址、规范 authority 与打印的地址：IPv4 loopback 字面量绑定该地址，authority 为 `<v>:<port>`；`::1` 只绑定 IPv6 的 `::1`，authority 为 `[::1]:<port>`；`localhost` 绑定 `127.0.0.1`（固定映射，不经解析器或 hosts 文件），authority 为 `localhost:<port>`。缺省 v=127.0.0.1。Host 必须逐字节等于规范 authority，Origin 缺省或等于 `http://<规范 authority>`；其他任何 authority（包括同一机器的其他 loopback 写法）都 403。`hoh web` 打印 `http://<规范 authority>/#token=<token>`。
- **重新授权（C21）**：host 每次启动换 token，页面持有的 token 在 host 重启后失效。此前有效的 token 被 `/v1` 以 403 拒绝时，页面进入“需要重新授权”状态（不清除投影与草稿，不重试该 token），提示运行 `hoh web` 并在新标签页打开它打印的地址；新页面读出片段 token、以它请求 getRun 成功后，经同源的 `BroadcastChannel("hoh-auth")` 发出 `{project_id, token}`；同源的旧页面收到后先核对消息中的 `project_id` 与自己的相同，再以该 token 请求自己的 Run 的 getRun，成功且返回的 `project_id` 相同才采用（否则不采用、不显示其数据），然后以 `Last-Event-ID` 续接 SSE（随后可能进入 409 cursor_ahead 的快照重同步）。`BroadcastChannel` 只在同一浏览器的同源文档之间传递，token 不写入任何存储、不发给 host 以外的地方。host 换了端口（原端口被占用，现有规则取空闲端口）时新地址与旧页面不同源，旧页面收不到授权：它保持“需要重新授权/不可达”并提供“复制草稿”，草稿只在用户操作时进入剪贴板。
- **Web 资产的位置（C2）**：发布包与安装版本目录中的 `web/`，与二进制并列（`hoh/bin/hoh` 与 `hoh/web/`）；host 由自身可执行文件的位置找到它，不加环境变量入口。`web/manifest.json` 记录版本、commit 与每个文件的 sha256 与 Content-Type，host 在第一次提供外壳前核对，并按记录的 Content-Type 提供资产（CR31D-06）。
- **检查**：AC 只用 v1 档已有、且由 M6 实现任务的祖先或其自身交付的检查：web-e2e（M6-T02 交付）、integration（M0-T03）、recovery（M2-T05）、deployment（M2-T14）。web-e2e 在 run_check 下同时运行 web/ 的单元测试与 Playwright 场景；deployment 增加全新 VM 场景（M6-T03），argv 不变。没有 AC 用 adapter-live、plugin-live、soak。
- **测试替身与模式**：FakeAgent、假 gh、本地 bare remote、debug 构建的接缝与标明 stub 的拦截都只在 product 检查中使用，结果记为 product，从不记为 live（docs/09）。host 的 debug 接缝只在 debug 构建中存在（`release_seams.sh` 核对）；页面的测试接缝只在 web/ 的 e2e 构建（`vite build --mode e2e`，输出到与 `web/dist` 不同的目录）中存在，web-e2e 核对发布用的 `web/dist` 不含接缝的标识（C24）。
- **新事件**（`$defs.Event.kind` 是开放字符串，不需 CR；AC 中出现的为定名）：`run.work_branch`、`delivery.completed`；另有交付过程的 `delivery.*` 事件，名称由 M6-T06 的 ADR 定。新的 Run 级问题原因：`work_branch_moved`、`delivery_conflict`、`delivery_pr_closed`、`delivery_merge_unverified`。`candidate.frozen` 的载荷增加 `branch` 与 `source`（`work_branch` 或 `merged`，第 2 节第 6 条）；候选清单（`contracts/candidate.schema.json`，`additionalProperties: false`）与 `candidates` 表不变。
- **可配置性**：新增 `[git]` 表三个键（第 4 节）；它们进入 `config/hoh.example.toml` 的时间是加载器接受它们的同一提交（CR26D-103 的规则），本 CR 不改 example。

### 2. 交付模型（REQ-065 的设计依据）

**要解决的**：REQ-065 要求“分支候选、PR/CI 可选、merge 后重新绑定最终 SHA、无 remote 本地 release 可验收”，同时守住单写者与单一工作树（写者在用户的工作树中工作，REQ-028 的脏文件规则以此为前提）、冻结候选（REQ-052）、effect ledger 与 kill -9 恢复（REQ-041）、不改写已发布历史（所有者）。

**取舍**：

1. **工作分支而不是每 Run 一个 worktree**：每个 Run 一个 `git worktree` 会把写者搬出用户的工作树，改变 M2/M3 已验收的写入位置与 REQ-028 的脏文件判定，超出细化的范围。改为：Run 在第一次写入时 HEAD 所在的分支上工作（HoH 不切换用户的分支）；只有 HEAD 游离，或 PR 交付而当前分支就是目标分支时，才新建 `hoh/run-<id>`。之后 HEAD 不在工作分支上就不派发写 Attempt 并提问，而不是由 HoH 切回去（C10）。
2. **本地交付为缺省**：release 绑定通过发布验证的候选；不触网。这就是“无 remote 本地 release 可验收”（C6）。
3. **PR 交付经 gh**：推送与建 PR 是 Runtime 的外部效果（不经 Agent），每步一条 effect；建 PR 前按 run 标记查找（docs/07 §5）。HoH 不合并 PR（C7）：合并对外且不可逆，由用户或仓库规则完成；HoH 观察合并，在目标分支的最终提交上重新冻结并完整验证（C18），这就是“merge 后重新绑定最终 SHA”。等待 CI 与合并时 Run 为 WAITING_EXTERNAL（已有状态，不改 Run 的迁移表与 AC-036-01）。
4. **目标分支前移用 merge**：rebase 会改写已推送的提交；merge 产生新提交、推送不需 force（C8）。
5. **载体**：交付的外部操作记在一个由 Runtime 执行、`attempts.agent` 为空的交付 Attempt 上（001 的 `attempts.agent` 可空，`effects.attempt_id` 必填），其 Task 为 `kind='work'`、由 Runtime 建立、不进 Agent 派发。具体形状由 M6-T06 的 ADR 定；若实现证明必须给 `tasks.kind` 增加取值，那需要新的迁移，须另写 CR（M4 的 CR-028 已预定 003），不在本 CR 中预设（C19）。
6. **两类候选（C23）**：*工作分支候选*由写者在用户工作树中提交后冻结，commit 是工作分支的头，按 REQ-052 的规则从用户工作树冻结（未提交或未跟踪的文件即拒绝）；本地交付与 PR 头的验证都用它。*最终交付候选*只在 PR 交付且观察到合并后出现，commit 是目标分支上的最终提交 F，而用户工作树的 HEAD 仍在工作分支（merge、squash、rebase 三种合并后都可能仍是 H1）：Runtime (a) 以 `git fetch <remote> +refs/heads/<目标分支>:refs/hoh/delivery/<run_id>/base` 取得目标分支（只写 `refs/hoh/` 下 HoH 自己的引用，不改用户的分支、`refs/remotes/`、HEAD、index 与工作树）；(b) 核对 F：`gh pr view` 报告 state=MERGED 与 mergeCommit=F，F 存在于取得的对象中且是取得的目标分支头的祖先或其本身，PR 的 baseRefName 等于 run.work_branch 当前记录的目标分支；任一不符则不冻结，Run 以 NEED_DECISION 提问（delivery_merge_unverified，列出不符项）；(c) 在 HoH 自己的路径（`.hoh/` 下，按 run 与 F 命名，登记确切路径）以 `git worktree add --detach` 建立 F 的只读核验 checkout，任何写者都不在其中写入；(d) 对该 checkout 按 REQ-052 的同一清洁规则冻结（commit=F，tree 为 F 的 tree，`source_path` 为该 checkout），candidate.frozen 载荷为 `{branch: 目标分支, source: "merged", pr_url, pr_head}`；(e) 在其上运行完整的发布验证（C18），通过后绑定 release；(f) release 绑定或 Run 结束后，按登记的确切路径移除该 checkout 与 `refs/hoh/delivery/<run_id>/` 下的引用。最终交付候选验证失败时写 Issue（绑定该候选）并唤醒 Commander，Run 不 COMPLETED；修复仍在工作分支上按第 4 条 merge 目标分支后进行，产生新的 PR。

**未采用**：HoH 自动合并 PR（对外、不可逆，且会替用户改目标分支）；在本地自动把工作分支合并进 base（改用户的分支）；rebase 加 `--force-with-lease`（违反所有者规则）；不经 gh、自己实现 GitHub REST 客户端与认证（要处理凭证，超出“不读写凭证”）；合并后把用户工作树切到 F 再冻结（改用户的 HEAD 与工作树，且与写者所在的工作分支冲突）；把 PR 头候选的证据转给 F（docs/07 §7）。

### 3. 新增的原因码（`contracts/protocol.schema.json`，CR-003 D1 第 1 条）

| 码 | 位置 | 含义 |
|---|---|---|
| WEB_ASSETS_UNAVAILABLE | ErrorCode（503） | Web 外壳不可提供：`web/manifest.json` 缺失（details.reason=missing）、版本或 commit 与二进制不同（mismatch）、文件摘要不符（corrupt）；`/v1` 不受影响（REQ-058、REQ-066） |

SSE 的 cursor_ahead 沿用已有的 409 `CONFLICT`（openapi Error 的通用码），只加 `details`，不加新码（C3）。

### 4. REQ-018 的配置键表

REQ-018 的 decisions 末尾增加一条（行为与 AC 不变）：

```json
"（CR-031）[git] 表：delivery（\"local\" 或 \"pr\"，缺省 \"local\"）、remote（remote 名，缺省 \"origin\"）、base（可选，PR 的目标分支名；缺省为 Run 第一个写 Attempt 准备时 HEAD 所在的分支）；其他值与未知键使配置被拒；它与 [delivery]（HCP 消息投递）无关（REQ-065）；example 中的缺省值与加载器的支持在同一提交加入（M6-T06）。"
```

### 5. `contracts/openapi.json` 的补充

在 `paths` 中增加下面两条（Web 外壳，`security: []`；`/assets/{asset}` 的 Content-Type 按 manifest 记录的类型，CR31D-06），并在 `/v1/events` 的 `get.description` 末尾追加一句；其余不变。CR-028 应用时也会改 openapi（新路由与命名 schema）：两者改的是不同的路径，M6-T01 应用时在 CR-028 之后的文件上增加这两条并核对没有同名路径。

```json
{
  "/": {
    "get": {
      "operationId": "webShell",
      "description": "The Web GUI shell (index.html of the build in the install's web/ directory; CR-031, REQ-058). It holds no project data and submits nothing, so it carries no token: the page reads the token from the URL fragment that hoh web prints, and the fragment never reaches the server, its logs or a Referer. Host and Origin are checked as for /v1 (the canonical authority of [web] host). 503 WEB_ASSETS_UNAVAILABLE (details.reason missing, mismatch or corrupt) when web/manifest.json is absent, names another version or commit than the binary, or a file's sha256 differs.",
      "security": [],
      "responses": {
        "200": {
          "description": "index.html; Content-Security-Policy default-src 'self', Referrer-Policy no-referrer, Cache-Control no-store",
          "content": {
            "text/html": {
              "schema": {
                "type": "string"
              }
            }
          }
        },
        "403": {
          "$ref": "#/components/responses/Forbidden"
        },
        "503": {
          "$ref": "#/components/responses/Error"
        }
      }
    }
  },
  "/assets/{asset}": {
    "get": {
      "operationId": "webAsset",
      "description": "A file of the Web GUI build listed in web/manifest.json (CR-031, REQ-058); no token, Host and Origin checked. The Content-Type is the one web/manifest.json records for the file, fixed at build time from its extension: .js text/javascript; charset=utf-8, .css text/css; charset=utf-8, .svg image/svg+xml, .png image/png, .woff2 font/woff2 (the build fails on any other extension); every response carries X-Content-Type-Options: nosniff. 404 for a name the manifest does not list; 503 WEB_ASSETS_UNAVAILABLE as for /.",
      "security": [],
      "parameters": [
        {
          "name": "asset",
          "in": "path",
          "required": true,
          "schema": {
            "type": "string",
            "pattern": "^[A-Za-z0-9._-]{1,200}$"
          }
        }
      ],
      "responses": {
        "200": {
          "description": "The file, with the Content-Type its manifest entry records",
          "content": {
            "text/javascript": {
              "schema": {
                "type": "string"
              }
            },
            "text/css": {
              "schema": {
                "type": "string"
              }
            },
            "image/svg+xml": {
              "schema": {
                "type": "string"
              }
            },
            "image/png": {
              "schema": {
                "type": "string",
                "contentMediaType": "image/png"
              }
            },
            "font/woff2": {
              "schema": {
                "type": "string",
                "contentMediaType": "font/woff2"
              }
            }
          }
        },
        "403": {
          "$ref": "#/components/responses/Forbidden"
        },
        "404": {
          "$ref": "#/components/responses/Error"
        },
        "503": {
          "$ref": "#/components/responses/Error"
        }
      }
    }
  }
}
```

`/v1/events` 的 `get.description` 追加：

```text
An after or Last-Event-ID greater than H is 409 CONFLICT with details {reason: "cursor_ahead", watermark: H}: the cursor is not from this store's history, and the client resyncs from a snapshot (CR-031, REQ-059).
```

### 6. 各需求的细化

每项给出替换后的整个需求对象与它的全部场景。AC 与 TEST 的 id 按 validate_bundle 的规则依次编号；原有的 AC-xxx-01 保留 id，改写为可判定的内容（原文各项的覆盖见“不变的部分”）。id、category、priority、milestone、title、required_release、release_class、release_targets、summary 都不变。顺序按实现任务分组。

#### REQ-058 Web GUI入口（3 条 AC；实现：M6-T02）

`contracts/requirements.json` 中 REQ-058 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-058-01；第 2 稿改写行为 1、2、5，失败模式与 AC-058-01，CR31D-06、-07）：

```json
{
  "id": "REQ-058",
  "category": "WEB",
  "priority": "P1",
  "milestone": "M6",
  "title": "Web GUI入口",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "同一Engine，不启动第二任务循环；可查看规格/团队/时间线/证据",
  "behavior": [
    "`hoh web [--project <目录>]` 是 Web 的唯一入口（CONTRACT 第 3 条）：按与 attach 相同的发现规则找到该项目正在运行的 host（.hoh/host.json 与 .hoh/host.sock），没有时按 ADR-023 的方式拉起（已安装时经 systemd --user unit）；已有 host 时绝不启动第二个 host，也不启动第二个调度或监督循环。它不创建 Run、Task、Attempt 或命令。成功时向 stdout 写恰一行地址 http://<规范 authority>/#token=<host.json 中的 token 原文>（规范 authority 由 [web] host 与实际端口按 REQ-061 行为 4 决定，缺省配置下为 127.0.0.1:<port>；token 只在 URL 片段中，片段不进请求行、host 日志或 Referer），退出码 0；有桌面环境（DISPLAY、WAYLAND_DISPLAY 或 BROWSER 非空）时另尝试打开浏览器，打开失败只在 stderr 说明，不改变退出码；没有桌面环境时不尝试（REQ-061）。host 不可用时与 attach 使用同一退出码（REQ-004 的客户端退出码表）。",
    "Web 外壳：host 在同一 loopback 源上提供构建产物：GET /（index.html）与 GET /assets/<文件名>（文件名带内容摘要）。外壳只含构建时的静态文件，不含任何项目数据，也不提交任何命令；它与 /v1 一样经 Host/Origin 校验，但不要求 X-HoH-Token（页面先加载，再从 URL 片段读出 token，C1）。外壳的响应带 Content-Security-Policy: default-src 'self'（没有内联脚本）、Referrer-Policy: no-referrer、X-Content-Type-Options: nosniff，index.html 另带 Cache-Control: no-store；/assets/ 的 Content-Type 取 web/manifest.json 为该文件记录的类型（.js 为 text/javascript; charset=utf-8，.css 为 text/css; charset=utf-8，.svg、.png、.woff2 为 image/svg+xml、image/png、font/woff2；构建遇到其他扩展名即失败），使模块脚本与样式表在 nosniff 下可被浏览器执行与应用。所有 /v1 请求（含 SSE）照旧要求 X-HoH-Token（openapi 的 localToken）。页面只把 token 留在内存中，读出后以 history.replaceState 从地址栏去掉片段，不写入 localStorage、sessionStorage、cookie、IndexedDB 或查询串；host 重启后的重新授权按 REQ-059 行为 7（同源 BroadcastChannel 交接，C21）。",
    "同一 Engine（docs/04）：页面的全部数据来自 host 的 /v1 读接口（getRun 的快照与 watermark H、M4 的分页与 /v1/timeline、tasks、acceptance、issues、evidence、team、specs、artifacts）与 SSE，不读 SQLite 或项目文件；写操作只经 POST /v1/commands 的类型化命令（与 TUI 同一 CommandService，docs/03），带 expected_revision 与 idempotency_key（REQ-060）。打开、浏览与切换页面不提交任何命令；关闭页面不暂停、不取消任何工作（CONTRACT 第 13 条）。",
    "七个页面（docs/03）：Overview 显示 Run 状态、已生效模式与待生效模式、阶段/任务、n/m 已验收需求与 blocked/open/reopened，取 M4 的同一 getRun 投影，与 TUI 状态栏的四组相同；Specs 显示已批准修订号与规格摘要、REQ→AC 树与每条 AC 的 check、待批问题与批准入口；Team 显示职位→Agent、模型、epoch、会话、可用性与待生效变更；Timeline 显示项目、轮次、Agent 三级，取 M4 的 /v1/timeline，块以 block_id 为身份，正文按需取 artifact；Quality 显示当前候选每条 AC 的标签、Issue 的 open/reopened、证据与 artifact 下载；History 列出 Run（最新在前）；Settings 显示有效配置（只读），职位绑定的修改经 ConfigureTeam 在 checkpoint 生效并先显示差异，Prompt 各层与修订只读（C5）。",
    "web/ 是 React、TypeScript、Vite 工程（CONTRACT 第 2 条），package-lock.json 锁定依赖；API 客户端由 contracts/openapi.json 生成并提交，重新生成必须逐字节相同（docs/04）。build 检查执行 npm ci 与构建，缺少 web/ 使 build 失败（docs/11、docs/23）。构建产物 web/dist 带 manifest.json（hoh 版本、构建 commit、每个文件的 sha256 与 Content-Type），进入发布包的 hoh/web/（REQ-066）。",
    "未知的事件 kind 显示为诊断块（保留 seq、kind 与公开摘要），不使页面崩溃（docs/07 §8）；Agent 产生的公开文本中的 HTML 与控制字符按文本显示，不作为标记或脚本执行。"
  ],
  "failure_modes": [
    "已有 host 时再运行 hoh web 不起新 host：host.json 的 pid 与 host_epoch 不变，hoh __host 进程仍只有一个。",
    "Web 资产缺失、web/manifest.json 的版本或 commit 与二进制不同、或文件摘要不符：GET / 返回 503 WEB_ASSETS_UNAVAILABLE（details.reason 依次为 missing、mismatch、corrupt），hoh web 以 1 退出、stdout 为空并在 stderr 说明；/v1、TUI 与 hoh -p 不受影响。",
    "页面从未取得有效 token（片段缺失或错误）：/v1 返回 403，页面显示“请用 hoh web 给出的地址打开”，不重试、不猜测；此前有效的 token 被拒（host 已重启换了 token）：进入 REQ-059 行为 7 的重新授权，不清除投影与草稿。",
    "Host 不是规范 authority，或 Origin 既非缺省也非 http://<规范 authority>（含同机的其他 loopback 写法与经 DNS 重绑定的名字）：外壳与 /v1 都返回 403（REQ-061 行为 4）。",
    "页面打开期间 host 重启：旧 token 失效，页面进入重连与重新授权（REQ-059 行为 3、7），保留草稿，不重复应用事件。"
  ],
  "doc_refs": [
    "CONTRACT.md",
    "docs/03-ux.md",
    "docs/04-architecture.md",
    "docs/05-data-api.md",
    "docs/07-algorithms.md",
    "docs/11-deployment.md",
    "docs/12-user-guide.md",
    "contracts/openapi.json",
    "docs/adr/ADR-016.md",
    "docs/adr/ADR-021.md",
    "docs/adr/ADR-023.md"
  ],
  "acceptance": [
    {
      "id": "AC-058-01",
      "given": "夹具 git 项目，产品 host 已为它运行（FakeAgent 世界），Run R 处于 PAUSED，有已批准修订、两条 Task 与若干事件；[web] 为缺省配置；环境中没有 DISPLAY、WAYLAND_DISPLAY 与 BROWSER",
      "when": "连续执行两次 hoh web，以 Chromium 与 Firefox 打开第一次给出的地址并浏览七个页面 60 s 后关闭，记录浏览器的网络日志与控制台；另以去掉 token 片段的地址打开一次，并以 Host: rebind.test:<port> 请求 GET / 与 GET /v1/runs 各一次",
      "then": "两次都退出 0，stdout 各恰一行 http://127.0.0.1:<port>/#token=<token> 且相同；hoh __host 进程恰一个，host.json 的 pid 与 host_epoch 不变；浏览期间没有 POST，commands、tasks、attempts 行数与 R 的 revision 不变；关闭页面后 R 仍为 PAUSED；读出 token 后地址栏不含片段，localStorage、sessionStorage 与 cookie 中没有 token；无 token 时外壳可加载、/v1 返回 403、页面显示重新打开的提示；Host 为 rebind.test 时两个请求都 403；外壳响应带 Content-Security-Policy: default-src 'self' 与 Referrer-Policy: no-referrer；页面加载的每个 /assets/ 响应为 200 且 Content-Type 等于 web/manifest.json 对该文件的记录（脚本为 text/javascript，样式为 text/css），带 nosniff；页面的根组件已渲染（data-testid=app-ready 可见），控制台没有脚本加载、MIME 或 CSP 违规的错误",
      "test_id": "TEST-058-01"
    },
    {
      "id": "AC-058-02",
      "given": "夹具项目的 Run R（FakeAgent 世界）：已批准修订 2（3 条 REQ、7 条 AC），候选 C 上 4 条 AC 为 PASS、1 条为 FAIL，一个 OPEN 与一个 REOPENED 的 Issue，六个职位中 developer 有待 checkpoint 生效的变更，项目、轮次、Agent 三级时间线都有块，一份证据 artifact；TUI 在 120×32 的 PTY 中附着 R",
      "when": "R 暂停于固定的 watermark H 后，页面依次打开 Overview、Specs、Team、Timeline、Quality、History、Settings，读出每页以 data-testid 标注的事实，同时在同一 H 读取对应的 /v1 接口与 TUI 状态栏；在 Quality 中下载该 artifact",
      "then": "每页的事实与同一 H 的 API 响应逐字段相等；Overview 的模式、待生效模式、n/m 与 blocked/open/reopened 与 TUI 状态栏的四组相同；Specs 的 REQ→AC 树与每条 AC 的 check 与规格索引一致；Team 显示 developer 的待生效变更；Timeline 三级的 block_id 与 /v1/timeline 相同且按 seq 递增；Quality 显示 4 条 PASS、1 条 FAIL、open=1、reopened=1；下载字节的 sha256 等于 artifact 的登记值；History 最新在前；Settings 的 Prompt 区没有可写控件",
      "test_id": "TEST-058-02"
    },
    {
      "id": "AC-058-03",
      "given": "两个同构的夹具项目 P1、P2（同一提交与同一 FakeAgent 脚本），各有一个 RUNNING 的 Run",
      "when": "在 P1 用 TUI、在 P2 用 Web 页面依次完成同一组意图：切到 PLAN 模式、向 commander 发送输入“继续核对”、暂停、恢复、把 qa 职位改为 pi；记录页面发出的全部 HTTP 请求；随后在 web/ 中重新生成 API 客户端",
      "then": "P1 与 P2 的 commands 行按顺序 kind 两两相同、payload 去掉 id、idempotency_key 与时间后相同，两个项目的事件 kind 序列相同；页面的请求只有外壳的 GET 与 openapi 声明的 /v1 操作，写请求只有 POST /v1/commands；重新生成的客户端与提交的文件逐字节相同",
      "test_id": "TEST-058-03"
    }
  ],
  "decisions": [
    "外壳（GET / 与 /assets/）不要求 token、仍校验 Host/Origin，/v1 全部要求 token：openapi 的 localToken 把 token 放在 URL 片段里，外壳必须先于 token 加载；外壳不含项目数据、不调用命令；token 从不进入 host 日志或 Referer（CR-031，C1，所有者 2026-11-22 批准）。",
    "Web 资产放在发布包与安装版本目录的 web/ 下，由 web/manifest.json 与二进制的版本、commit 核对，不嵌入二进制（workspace 不允许构建脚本，CR-031，C2）。",
    "Web 的 Prompt 区只读：编辑仍按 REQ-047 的文件覆盖进行，不新增写命令（受锁的命令种类不变，CR-031，C5）。",
    "web-e2e 在 Chromium 上运行全部场景，在 Firefox 上运行 AC-058-01 与 AC-061-01；docs/12 只声明这两种测过的浏览器（CR-031，C4）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-058 的场景（旧：只有 TEST-058-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-058-01",
    "requirement_id": "REQ-058",
    "acceptance_id": "AC-058-01",
    "check_id": "web-e2e",
    "fixture": "临时 git 项目 + 由 hoh -p --detach 拉起的产品 host（FakeAgent）+ 已暂停的 Run R + Playwright（Chromium、Firefox），无桌面环境变量",
    "steps": [
      "记录 host.json、进程表与 commands/tasks/attempts 行数",
      "运行 hoh web 两次并收集 stdout、stderr 与退出码",
      "两种浏览器打开地址并浏览七页 60 s 后关闭",
      "以无 token 地址打开一次",
      "以 Host: rebind.test:<port> 请求 GET / 与 GET /v1/runs",
      "再次读取 host.json、进程表与行数"
    ],
    "pass_criteria": [
      "两次退出 0、各恰一行相同地址",
      "host 进程恰一个且 pid、host_epoch 不变",
      "浏览期间无 POST、行数与 revision 不变，R 仍 PAUSED",
      "地址栏无片段、存储与 cookie 中无 token",
      "无 token 时外壳 200、/v1 403、页面提示",
      "rebind.test 的两个请求 403",
      "外壳的 CSP 与 Referrer-Policy 头",
      "/assets/ 的 Content-Type 与 manifest 一致且带 nosniff",
      "app-ready 可见、控制台无加载/MIME/CSP 错误"
    ],
    "observables": [
      "stdout/stderr 与退出码",
      "host.json 与进程表前后",
      "浏览器网络日志与控制台",
      "表行数前后",
      "响应头",
      "判定结果"
    ]
  },
  {
    "id": "TEST-058-02",
    "requirement_id": "REQ-058",
    "acceptance_id": "AC-058-02",
    "check_id": "web-e2e",
    "fixture": "夹具项目（FakeAgent）达到给定状态后暂停于 watermark H + TUI 附着在 120×32 PTY + Playwright（Chromium）",
    "steps": [
      "暂停并记录 H",
      "依次打开七页并读出 data-testid 事实",
      "在同一 H 读取 getRun、specs、team、timeline、acceptance、issues、evidence、runs",
      "读取 TUI 状态栏",
      "下载 artifact 并计算 sha256"
    ],
    "pass_criteria": [
      "七页事实与 API 逐字段相等",
      "Overview 四组与 TUI 状态栏相同",
      "Specs 树与索引一致",
      "Team 显示待生效变更",
      "Timeline block_id 与 API 相同且 seq 递增",
      "Quality 计数 4/1、open=1、reopened=1",
      "artifact sha256 相等",
      "History 顺序",
      "Prompt 区无写控件"
    ],
    "observables": [
      "页面事实导出",
      "API 响应",
      "TUI 屏幕文本",
      "artifact 摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-058-03",
    "requirement_id": "REQ-058",
    "acceptance_id": "AC-058-03",
    "check_id": "web-e2e",
    "fixture": "两个同构夹具项目 P1、P2（同一提交与 FakeAgent 脚本）+ P1 的 TUI（PTY）+ P2 的 Playwright 页面 + web/ 的客户端生成器",
    "steps": [
      "P1 在 TUI 中完成五个意图",
      "P2 在页面中完成同样五个意图并记录全部请求",
      "导出两个项目的 commands 与事件 kind 序列",
      "重新生成 API 客户端并比较"
    ],
    "pass_criteria": [
      "commands 的 kind 序列与规范化 payload 两两相同",
      "事件 kind 序列相同",
      "页面请求只有外壳 GET 与 openapi 声明的 /v1 操作，写只有 POST /v1/commands",
      "重新生成的客户端逐字节相同"
    ],
    "observables": [
      "commands 导出",
      "事件导出",
      "浏览器网络日志",
      "生成文件 diff",
      "判定结果"
    ]
  }
]
```

#### REQ-061 无障碍和远程体验（4 条 AC；实现：M6-T02（AC-061-01、-02、-04）与 M6-T03（AC-061-03））

`contracts/requirements.json` 中 REQ-061 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-061-01；第 2 稿改写行为 4 并增加 AC-061-04，CR31D-07）：

```json
{
  "id": "REQ-061",
  "category": "WEB",
  "priority": "P1",
  "milestone": "M6",
  "title": "无障碍和远程体验",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "核心功能可用；默认loopback；没有桌面环境也可访问",
  "behavior": [
    "键盘：全部核心流程（在七个页面间导航、改模式、发送输入、暂停/恢复/取消、答复问题与批准规格、修改职位绑定、打开证据）只用键盘可以完成；焦点始终可见，顺序与视觉顺序一致，没有键盘陷阱；Esc 关闭对话框与抽屉等临时层而不取消工作（与 TUI 一致，docs/03）；每个交互元素有可访问名称；冲突、重连与重同步经 aria-live 区域播报。",
    "可达性基线：锁定版本的 axe-core 对七个页面的默认状态与打开的批准对话框检查 WCAG 2.1 A 与 AA 规则，违规为 0；状态不只用颜色表达（带文字）；遵从 prefers-reduced-motion。",
    "小视口：360×640、768×1024、1280×800 三种视口与 1280×800 下 200% 缩放时页面没有横向滚动（文档的 scrollWidth 不大于视口宽度）；核心动作可见，或在可由键盘打开的菜单中；长文本在块内换行或滚动，不撑破布局。",
    "默认 loopback 与规范地址：[web] host 的取值 v（缺省 127.0.0.1；加载器只接受 localhost 与 loopback IP，REQ-018）决定三件事，三者一致（C20、C22；今天 host 不读这个键、恒绑 127.0.0.1）：(1) 绑定地址：IPv4 loopback 字面量绑定该地址；::1 只绑定 IPv6 的 ::1；localhost 绑定 127.0.0.1（固定映射，不经解析器或 hosts 文件）；端口为 [web] port，被占用时取空闲端口并写进 host.json（现有规则）。(2) 规范 authority：IPv4 为 <v>:<port>，::1 为 [::1]:<port>（IPv6 加方括号），localhost 为 localhost:<port>，<port> 为实际监听端口。(3) TCP 上 Host 必须逐字节等于规范 authority、Origin 缺省或等于 http://<规范 authority>，其他任何 authority（包括同机的其他 loopback 写法，例如配置为 ::1 时的 127.0.0.1:<port>）都 403；hoh web 打印 http://<规范 authority>/#token=<token>。没有配置能让它监听非 loopback 地址；外部只能经 SSH 端口转发访问，且本地端口必须与远端端口相同（缺省配置下为 ssh -L <port>:127.0.0.1:<port> <vm>，docs/11），因为 Host 校验绑定的是规范 authority。",
    "没有桌面环境：hoh web 在 DISPLAY、WAYLAND_DISPLAY 与 BROWSER 都为空时只打印地址、不尝试启动浏览器并退出 0；docs/12 给出在没有桌面的 VM 上经 SSH 转发打开地址的步骤，这些步骤按 REQ-066 实跑。"
  ],
  "failure_modes": [
    "经转发访问时本地端口与远端不同：Host 校验返回 403，页面与 docs/12 说明必须用相同端口转发。",
    "有桌面但浏览器启动失败：stderr 说明，仍打印地址并退出 0。",
    "窄屏下被收起的动作：仍可从菜单到达，不会消失。"
  ],
  "doc_refs": [
    "docs/03-ux.md",
    "docs/11-deployment.md",
    "docs/12-user-guide.md",
    "docs/22-tooling.md",
    "contracts/openapi.json",
    "config/hoh.example.toml"
  ],
  "acceptance": [
    {
      "id": "AC-061-01",
      "given": "Run R 处于有待答问题、待批修订与证据的夹具状态；Chromium 与 Firefox",
      "when": "只用键盘（Tab、Shift+Tab、Enter、Space、Esc、方向键）依次完成：在 Specs 批准修订、在 Overview 把模式改为 PLAN、发送输入、暂停并恢复、在 Settings 把 qa 改为 pi、在 Quality 打开一份证据；在七个页面默认状态与打开的批准对话框上运行 axe-core",
      "then": "commands 依次新增 ApproveSpec、SetMode、SendInput、PauseRun、ResumeRun、ConfigureTeam 且都被接受，过程中没有鼠标事件；每次 Tab 后获得焦点的元素可见（outline 或 box-shadow 不为 none）并有可访问名称；Esc 关闭对话框前后 Run 的状态与 revision 不变；两种浏览器中 axe-core 的 WCAG 2.1 A/AA 违规都为 0",
      "test_id": "TEST-061-01"
    },
    {
      "id": "AC-061-02",
      "given": "同一夹具状态，时间线有 500 个块，其中一个块含 2 KB 不换行的字符串",
      "when": "以 360×640、768×1024、1280×800 三种视口与 1280×800 下 200% 缩放打开七个页面，并在 360×640 下只用键盘改模式与发送输入",
      "then": "每种视口的每一页 document.scrollingElement.scrollWidth 不大于 window.innerWidth；Overview 的四组状态（模式、阶段/任务、n/m、blocked/open/reopened）在每种视口都可见；360×640 下两项动作经菜单完成且命令被接受；长字符串在块内换行或滚动",
      "test_id": "TEST-061-02"
    },
    {
      "id": "AC-061-03",
      "given": "全新 Ubuntu 26.04 云镜像 VM（无桌面、qemu/KVM），已安装本候选的发布包，VM 中的夹具项目配置 [web] port=P（P 为宿主上空闲的端口）并有待批的规格修订；宿主可用密钥 ssh 到 VM，VM 的 PATH 中 xdg-open 是记录调用的垫片",
      "when": "经非交互 ssh 在 VM 中执行 hoh web --project <夹具>；在 VM 中列出监听套接字；从宿主直接连接 VM 的网络地址:P；建立 ssh -N -L P:127.0.0.1:P 后在宿主用无头 Chromium 打开打印的地址，浏览 Overview、把模式改为 PLAN 并批准待批修订；再经 ssh -N -L Q:127.0.0.1:P（Q≠P）访问一次",
      "then": "hoh web 退出 0，stdout 恰一行地址，xdg-open 垫片未被调用；TCP 监听只有 127.0.0.1:P，没有 0.0.0.0、:: 或 VM 的网络地址；直连被拒绝；经同端口转发页面可用，两个命令被接受，批准的来源判定为 user 且写入新修订；经 Q 转发时请求 403，页面显示必须用相同端口转发的提示",
      "test_id": "TEST-061-03"
    },
    {
      "id": "AC-061-04",
      "given": "同一夹具项目依次以三种 [web] 配置启动产品 host：缺省（无 host 键）、host=\"::1\"、host=\"localhost\"；各配置的 port 为测试取得的空闲端口 P；Playwright（Chromium）",
      "when": "每种配置下运行 hoh web 并列出 host 进程的 TCP 监听套接字，用 Chromium 打开打印的地址并浏览 Overview；再以 Host 依次为 127.0.0.1:P、[::1]:P、localhost:P、127.0.0.2:P 与 rebind.test:P（Origin 与 Host 同名）请求 GET / 与 GET /v1/runs（带正确 token）",
      "then": "三种配置打印的地址依次为 http://127.0.0.1:P/、http://[::1]:P/、http://localhost:P/（各带 #token= 片段）；监听套接字依次恰为 127.0.0.1:P、[::1]:P、127.0.0.1:P，没有 0.0.0.0 或 ::；每种配置下页面可用，Overview 显示 Run；Host 等于该配置的规范 authority 时 GET / 为 200、GET /v1/runs 为 200，其余四种 Host 的两个请求都 403",
      "test_id": "TEST-061-04"
    }
  ],
  "decisions": [
    "可达性以 axe-core 的 WCAG 2.1 A/AA 规则零违规加键盘全流程判定（CR-031，C14）。",
    "远程访问在全新的无桌面 Ubuntu 云镜像 VM 中验证：Host 精确校验使同机转发无法使用相同端口，本机也不允许无特权用户命名空间（CR-031，C11）。",
    "绑定地址、规范 authority、Host/Origin 校验与 hoh web 打印的地址都由 [web] host 决定：localhost 固定绑定 127.0.0.1、authority 为 localhost:<port>；::1 的 authority 加方括号；其他 authority 一律 403（CR-031，C20、C22）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-061 的场景（旧：只有 TEST-061-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-061-01",
    "requirement_id": "REQ-061",
    "acceptance_id": "AC-061-01",
    "check_id": "web-e2e",
    "fixture": "产品 host（FakeAgent）在有待答问题、待批修订与证据的状态 + Playwright（Chromium、Firefox）+ 锁定版本的 axe-core",
    "steps": [
      "只用键盘完成六项动作",
      "记录每次 Tab 后的焦点元素样式与可访问名称",
      "在对话框中按 Esc",
      "七页与对话框运行 axe-core",
      "导出 commands"
    ],
    "pass_criteria": [
      "六个命令依次被接受且无鼠标事件",
      "焦点可见且有可访问名称",
      "Esc 前后状态与 revision 不变",
      "两浏览器 axe 违规为 0"
    ],
    "observables": [
      "键盘事件日志",
      "焦点记录",
      "axe 报告",
      "commands 导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-061-02",
    "requirement_id": "REQ-061",
    "acceptance_id": "AC-061-02",
    "check_id": "web-e2e",
    "fixture": "产品 host + 时间线 500 块（含 2 KB 不换行字符串）+ Playwright（Chromium）四种视口设置",
    "steps": [
      "四种设置下打开七页并读取 scrollWidth 与 innerWidth",
      "读取 Overview 四组状态的可见性",
      "360×640 下用键盘改模式与发送输入"
    ],
    "pass_criteria": [
      "无横向滚动",
      "四组状态可见",
      "两项动作经菜单完成且被接受",
      "长字符串换行或块内滚动"
    ],
    "observables": [
      "尺寸读数",
      "截图与 DOM 快照",
      "commands 导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-061-03",
    "requirement_id": "REQ-061",
    "acceptance_id": "AC-061-03",
    "check_id": "deployment",
    "fixture": "全新 Ubuntu 26.04 云镜像 VM（qemu/KVM，cloud-init 只建非 root 用户与 ssh 密钥）+ 本候选的发布包 + VM 中的夹具项目 + xdg-open 垫片 + 宿主的无头 Chromium",
    "steps": [
      "在 VM 中运行 hoh web",
      "列出 VM 的监听套接字",
      "从宿主直连 VM 地址:P",
      "同端口转发后在宿主打开地址并完成两项动作",
      "以不同本地端口转发访问"
    ],
    "pass_criteria": [
      "退出 0、一行地址、垫片未调用",
      "只监听 127.0.0.1:P",
      "直连被拒",
      "转发后页面可用、两个命令被接受、批准来源为 user 并写入新修订",
      "不同端口 403 并有提示"
    ],
    "observables": [
      "ssh 会话的 stdout/stderr 与退出码",
      "监听套接字列表",
      "连接结果",
      "浏览器网络日志",
      "specs 导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-061-04",
    "requirement_id": "REQ-061",
    "acceptance_id": "AC-061-04",
    "check_id": "web-e2e",
    "fixture": "夹具项目 + 产品 host 依次以缺省、host=\"::1\"、host=\"localhost\" 三种 [web] 配置启动（空闲端口 P）+ Playwright（Chromium）",
    "steps": [
      "每种配置下运行 hoh web 并记录打印的地址",
      "列出 host 进程的 TCP 监听套接字",
      "Chromium 打开地址并浏览 Overview",
      "以五种 Host（与同名 Origin）请求 GET / 与 GET /v1/runs"
    ],
    "pass_criteria": [
      "三种配置打印的地址依次为 127.0.0.1、[::1]、localhost 的规范形式",
      "监听套接字依次恰为 127.0.0.1:P、[::1]:P、127.0.0.1:P",
      "页面可用",
      "规范 authority 的两个请求 200，其余四种 Host 403"
    ],
    "observables": [
      "hoh web 的 stdout 与退出码",
      "监听套接字列表",
      "HTTP 状态",
      "页面截图",
      "判定结果"
    ]
  }
]
```

#### REQ-059 事件重连（5 条 AC；实现：M6-T05）

`contracts/requirements.json` 中 REQ-059 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-059-01；第 2 稿增加行为 7 与 AC-059-05，改写 AC-059-03、-04，CR31D-01、-04、-05）：

```json
{
  "id": "REQ-059",
  "category": "WEB",
  "priority": "P1",
  "milestone": "M6",
  "title": "事件重连",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "序号去重、replay/live交接无遗漏；过期cursor走snapshot resync",
  "behavior": [
    "启动：页面先取 getRun 快照（watermark H），加载不超过 H 的最新一页时间线摘要，再以 after=H 订阅 SSE（M4 的读模型约定）；之后每条事件按 seq 应用。",
    "事件存储：按 seq 去重（已应用的 seq 再到达不改变状态）、按 seq 递增应用；过滤造成的 seq 空隙不当作丢失；未知 kind 保留为诊断；投影是 (state, event)→state' 的纯函数，一次重放、分批重放与快照加增量得到逐字段相同的状态（docs/07 §8）。",
    "断线与重连：SSE 结束或出错时页面立即显示“重连中，数据可能过期”与最后已应用的 seq，保留输入草稿、展开状态与锚点；按 1 s 起、加倍至 30 s 的退避（±20% 抖动）重连，以 Last-Event-ID=最后已应用的 seq 续接，补齐 (last, H'] 后接 live，补齐之前不清除标记（docs/03）。重连期间写命令照常提交（命令通道与 SSE 独立），结果以之后到达的事件为准。",
    "游标失效时按快照重同步：SSE 以 409 CONFLICT 且 details.reason=cursor_ahead 拒绝游标（游标超过 host 已提交的水位，例如项目数据库被换成较早的副本），或以 410 CURSOR_EXPIRED 拒绝（v1 不删除事件，这一分支在产品中不可达，只以标明 stub 的注入响应覆盖页面的处理，CR-028），或分页游标以 410 CURSOR_EXPIRED 过期（M4 的派生索引 generation）：页面丢弃该视图的本地投影与缓存块，重新取快照与页，从新快照的 watermark 续接，并显示“已按快照重新同步”与原因；草稿保留；不把旧投影与新快照拼接。",
    "host 端：SSE 的 after 或 Last-Event-ID 大于连接时已提交的水位 H 时返回 409 CONFLICT，details 为 {reason: \"cursor_ahead\", watermark: H}，什么也不推送（今天这样的游标被接受，之后 seq 不超过游标的新事件被跳过）；after 与 Last-Event-ID 矛盾的 409 与 410 的形状不变（ADR-016，CR-028）。",
    "慢客户端：页面待应用的事件队列至多 2000 条且正文合计至多 4 MiB，超过即主动关闭 SSE、丢弃未应用的部分，并从最后已应用的 seq 续接（这是续接，不是快照重同步）；host 端每条 SSE 按页从数据库读取（ADR-016 决定 7），慢连接不在内存中缓冲无界数据，不阻塞 writer、命令或其他连接。",
    "重新授权（C21）：host 每次启动换 token（M2 的现状）。页面此前有效的 token 被 /v1 以 403 拒绝时，页面进入“需要重新授权”：停止以该 token 重试，保留投影、草稿、展开状态与锚点，显示“host 已重启：运行 hoh web 并在新标签页打开它给出的地址”。同源的新页面读出片段 token 并以它请求 getRun 成功后，经 BroadcastChannel(\"hoh-auth\") 发出 {project_id, token}；旧页面收到后先核对消息中的 project_id 与自己的相同，再以该 token 请求自己的 Run 的 getRun，成功且返回的 project_id 相同才采用该 token，然后按行为 3 以 Last-Event-ID 续接（游标超过新水位时按行为 4 重同步）；project_id 不同或 getRun 失败（例如 404）则不采用、不显示其数据。token 只在页面内存与这条同源通道中，不写入任何存储、URL 或日志。host 换了端口时新地址与旧页面不同源，旧页面收不到授权：它保持“需要重新授权/不可达”，继续按行为 3 的退避探测原地址，并提供“复制草稿”（只在用户操作时写入剪贴板）。有未提交的草稿时离开页面触发浏览器的离开确认。"
  ],
  "failure_modes": [
    "重连后服务端重发已应用的 seq：不重复显示、不重复计数。",
    "快照与旧投影来自不同 watermark：整体替换，禁止拼接。",
    "断线期间提交的命令响应丢失：按 REQ-060 以同一 idempotency_key 重试，不当作新命令。",
    "事件 data 无法解析：页面停止跟随，显示该 SSE id 与诊断，不跳过（与 TUI 相同，ADR-031 决定 10），提示重新加载。",
    "重新授权收到另一项目的 token（同一端口上换成了另一项目的 host）：消息或 getRun 的 project_id 不同、或 getRun 失败，不采用，页面不显示其数据，保持需要重新授权。"
  ],
  "doc_refs": [
    "docs/03-ux.md",
    "docs/05-data-api.md",
    "docs/07-algorithms.md",
    "docs/09-testing.md",
    "docs/adr/ADR-016.md",
    "docs/adr/ADR-031.md",
    "contracts/openapi.json"
  ],
  "acceptance": [
    {
      "id": "AC-059-01",
      "given": "事件夹具 seq 1..200：run_id 过滤造成的空隙、3 个未知 kind、带 HTML 标记与控制字符的公开文本",
      "when": "以三种方式送入页面的事件存储并比较规范化投影：一次顺序送入；以 7 条与 23 条为批并重复送入 seq 50..80；先送 1..100，再以 watermark=100 的快照替换后续送 101..200",
      "then": "三种方式的投影逐字段相同；每个 seq 恰应用一次；空隙不产生丢失诊断；3 个未知 kind 各有一个保留 seq 的诊断块；HTML 标记与控制字符作为文本出现在 DOM 文本节点中、没有生成元素或执行脚本",
      "test_id": "TEST-059-01"
    },
    {
      "id": "AC-059-02",
      "given": "页面附着 RUNNING 的 Run，FakeAgent 每 50 ms 提交一条事件；debug 构建的 host 接缝可关闭全部 SSE 连接而不影响命令；另一个页面始终在线",
      "when": "页面已应用到 seq=S 时关闭 SSE 并使其后 3 s 内的连接失败（其间 host 再提交约 60 条事件，用户在输入框中写入草稿“继续核对”），之后恢复并持续 20 s",
      "then": "断开期间页面显示“重连中”与 S；第一次成功的重连带 Last-Event-ID=S；结束时页面已应用的 seq 集合等于该 Run 在页面附着后提交的事件 seq 集合（与 /v1/events 及数据库导出一致），无重复、无缺失、递增；两个页面的最终投影相同；草稿保留；标记在补齐后消失",
      "test_id": "TEST-059-02"
    },
    {
      "id": "AC-059-03",
      "given": "Run R 运行到项目 seq=120 时暂停（静止：没有 Attempt 在运行，也没有客户端提交命令），停止 host 并离线复制项目数据库（含 WAL），再启动 host；恢复 R 运行到项目 seq 不小于 200 后再次暂停；页面已应用到 L（L≥200），输入框中有草稿“继续核对”；时间线页持有一个分页游标",
      "when": "(a) 停止 host，用副本替换数据库文件，重启 host（它在提供 HTTP 前照常 reconcile，写入的恢复事件不被关闭）；页面显示需要重新授权后运行 hoh web 并在同一浏览器上下文的新标签页打开打印的地址；等旧页面续接后恢复 R 并让它提交 5 条事件；(b) 经 M4 的重建路径（debug 接缝）递增派生索引的 generation 后翻页；(c) 以标明 stub 的拦截使下一次 SSE 返回 410 CURSOR_EXPIRED{snapshot_watermark}",
      "then": "(a) 记重启后 409 响应时的已提交水位为 H（夹具静止，H 等于 409 之后立即读出的数据库最大 seq），且 120≤H<L；旧页面以新 token 发出的 SSE 请求带 Last-Event-ID=L，得到 409 CONFLICT 且 details={reason: cursor_ahead, watermark: H}；页面丢弃旧投影，加载快照（watermark H'，H'≥H）并以 after=H' 续接，显示重同步与原因；之后页面的可见块与 H' 的快照加其后事件得到的投影逐字段相同，旧投影中 seq 大于 H 的块不再出现；恢复后的 5 条事件各显示一次；(b) 翻页得到 410 CURSOR_EXPIRED 与顶层 snapshot_watermark，页面重取快照与页后锚点块仍在视窗内；(c) 页面按 snapshot_watermark 重同步，该子例的证据标为 stub；三种情况草稿都保留",
      "test_id": "TEST-059-03"
    },
    {
      "id": "AC-059-04",
      "given": "两个页面 A、B 附着同一 RUNNING 的 Run：A 以 web/ 的 e2e 构建加载（只有它带消费暂停接缝 holdApply/releaseApply，C24：暂停的只是把已收到的事件应用到投影，接收、排队、溢出关闭与续接仍是发布构建的同一代码），B 以发布构建加载；FakeAgent 脚本可按测试的指令提交给定条数与大小的事件",
      "when": "(a) 条数界限：A 调用 holdApply，host 提交 2100 条正文约 200 B 的事件，A 第一次溢出后调用 releaseApply；(b) 字节界限：A 再调用 holdApply，host 提交 300 条正文约 16 KiB 的事件（合计约 4.7 MiB，少于 2000 条），A 第一次溢出后调用 releaseApply；(c) 双客户端：A 不暂停，host 在 30 s 内提交 5000 条小事件，期间每 5 s 从 B 提交一次 SendInput；每个子例在 releaseApply（(c) 为最后一条事件提交）后至多等待 30 s",
      "then": "(a)、(b) 各至少发生一次溢出，且 (a) 的第一次溢出发生在待应用队列将超过 2000 条时、(b) 的第一次发生在将超过 4 MiB 时；每次溢出时 A 关闭当时的 SSE、丢弃未应用的部分，下一个 SSE 请求带 Last-Event-ID=溢出时最后已应用的 seq，期间没有 getRun 快照请求（是续接，不是快照重同步）；诊断面板记录的队列最大值不超过 2000 条且不超过 4 MiB；三个子例都在 30 s 期限内使 A 已应用的 seq 集合等于数据库中该 Run 的事件 seq 集合，无重复；(c) B 在每条事件提交后 2 s 内显示它，B 的每个 SendInput 在 1 s 内得到 202，A 是否溢出不作要求；全程 host 的 RSS 增量不超过 64 MiB",
      "test_id": "TEST-059-04"
    },
    {
      "id": "AC-059-05",
      "given": "产品 host 以缺省配置在端口 P 上为夹具项目 X 运行，另有项目 Y 的夹具（[web] port 也为 P）；页面 A（Chromium）由 X 的 hoh web 打开并附着 RUNNING 的 Run，已应用到 seq=S，输入框中有草稿“继续核对”",
      "when": "(a) 同址：停止并重启 X 的 host（仍在 P 上），A 显示需要重新授权后，在 X 中运行 hoh web 并在同一浏览器上下文的新标签页打开打印的地址；(b) 换项目：停止 X 的 host，在 P 上启动 Y 的 host，在 Y 中运行 hoh web 并在新标签页打开；然后停止 Y 的 host，按 (a) 回到 X；(c) 换地址：停止 X 的 host，测试占用 P（接受 TCP 连接后立即关闭），重启 X 的 host，在 X 中运行 hoh web 并在新标签页打开，在 A 上触发“复制草稿”",
      "then": "每次重启后 host.json 的 token 与此前的都不同，旧 token 对新 host 的 /v1/runs 为 403；(a) A 在第一个 403 之后显示需要重新授权，不再以旧 token 发请求，投影与草稿保留；新标签页打开后 A 采用新 token 并以 Last-Event-ID=它最后已应用的 seq 续接，最终 A 已应用的 seq 集合等于数据库中该 Run 的事件 seq 集合且无重复，草稿仍为“继续核对”；(b) Y 的授权消息带 Y 的 project_id，A 不采用（不以 Y 的 token 请求 X 的 Run 以外的任何数据），A 不显示 Y 的任何数据并保持需要重新授权；回到 X 后结果同 (a)；(c) hoh web 打印的地址的端口为 P2≠P；A 保持需要重新授权或不可达，复制得到的剪贴板文本为“继续核对”；P2 上的新页面可用；全程 token 不出现在任一页面的 localStorage、sessionStorage、cookie、IndexedDB、请求 URL 与 Referer 中，也不出现在 .hoh/ 下 host 的日志中",
      "test_id": "TEST-059-05"
    }
  ],
  "decisions": [
    "SSE 游标超过已提交水位是无效游标：409 CONFLICT 加 details.reason=cursor_ahead 与 watermark，不用 410；与 CR-028 对超过水位的分页游标按无效处理一致，SSE 的 410 在 v1 仍不可达（CR-031，C3）。",
    "页面事件队列上限 2000 条或 4 MiB，与 M4 客户端缓存上限（1000 个块描述、4 MiB 正文、2 个在途页）同一量级；溢出续接而不是重同步（CR-031，C15）；溢出以 e2e 构建的消费暂停接缝确定地触发，双客户端时延与 RSS 另作子例（CR-031，C24）。",
    "host 重启换 token；页面以同源 BroadcastChannel 从新打开的页面取得新 token，按 project_id 核对后采用；不持久化 token、不固定 token（CR-031，C21）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-059 的场景（旧：只有 TEST-059-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-059-01",
    "requirement_id": "REQ-059",
    "acceptance_id": "AC-059-01",
    "check_id": "web-e2e",
    "fixture": "页面事件存储模块（web/ 的单元测试，在 web-e2e 检查中运行）+ 固定事件夹具 seq 1..200",
    "steps": [
      "顺序送入",
      "分批并重复送入 50..80",
      "快照 100 加增量 101..200",
      "比较规范化投影与 DOM 文本"
    ],
    "pass_criteria": [
      "三种投影逐字段相同",
      "每个 seq 恰一次",
      "空隙无丢失诊断",
      "3 个未知 kind 各一个诊断块",
      "HTML 与控制字符为文本、无脚本执行"
    ],
    "observables": [
      "规范化投影导出",
      "诊断列表",
      "DOM 快照",
      "判定结果"
    ]
  },
  {
    "id": "TEST-059-02",
    "requirement_id": "REQ-059",
    "acceptance_id": "AC-059-02",
    "check_id": "web-e2e",
    "fixture": "debug 构建的产品 host（SSE 断开接缝）+ FakeAgent 每 50 ms 一条事件 + 两个 Playwright 页面",
    "steps": [
      "页面附着并记录 S",
      "关闭 SSE 并使 3 s 内重连失败",
      "写入草稿",
      "恢复并运行 20 s",
      "导出两页的已应用 seq 与数据库事件"
    ],
    "pass_criteria": [
      "断开期间显示重连与 S",
      "首次重连带 Last-Event-ID=S",
      "seq 集合与数据库一致、无重复无缺失、递增",
      "两页投影相同",
      "草稿保留、标记消失"
    ],
    "observables": [
      "页面状态时间线",
      "SSE 请求头",
      "已应用 seq 导出",
      "events 导出",
      "判定结果"
    ]
  },
  {
    "id": "TEST-059-03",
    "requirement_id": "REQ-059",
    "acceptance_id": "AC-059-03",
    "check_id": "web-e2e",
    "fixture": "产品 host + Playwright 页面（同一浏览器上下文）+ Run 在 seq=120 暂停时停机离线复制的数据库文件 + M4 派生索引重建的 debug 接缝 + 标明 stub 的 SSE 410 拦截",
    "steps": [
      "用副本替换数据库并重启 host",
      "运行 hoh web 并在新标签页打开以重新授权",
      "记录 409 与其后读出的数据库最大 seq H",
      "恢复 R 并提交 5 条事件",
      "触发索引重建后翻页",
      "拦截 SSE 返回 410",
      "检查草稿"
    ],
    "pass_criteria": [
      "120≤H<L，409 CONFLICT 且 details 为 cursor_ahead 与 watermark H",
      "页面以 H'≥H 的快照重同步，可见块与 H' 的投影一致，旧投影中 seq>H 的块不再出现，新事件各一次",
      "翻页 410 与顶层 snapshot_watermark，锚点仍在视窗",
      "stub 410 触发重同步且证据标为 stub",
      "草稿保留"
    ],
    "observables": [
      "HTTP 状态与错误体",
      "页面状态与可见块",
      "SSE 请求头",
      "stub 标记",
      "判定结果"
    ]
  },
  {
    "id": "TEST-059-04",
    "requirement_id": "REQ-059",
    "acceptance_id": "AC-059-04",
    "check_id": "web-e2e",
    "fixture": "产品 host + 按指令提交事件的 FakeAgent 脚本 + Chromium 页面 A（web/ 的 e2e 构建，带消费暂停接缝）与 B（发布构建）+ host RSS 采样",
    "steps": [
      "开始 RSS 采样",
      "holdApply 后提交 2100 条小事件，第一次溢出后 releaseApply",
      "holdApply 后提交 300 条约 16 KiB 的事件，第一次溢出后 releaseApply",
      "A 不暂停，30 s 内提交 5000 条事件并每 5 s 从 B 发送输入",
      "每个子例至多等待 30 s，导出队列读数、SSE 请求、已应用 seq、B 的显示时延与命令时延"
    ],
    "pass_criteria": [
      "条数与字节两种界限各触发溢出，首次溢出发生在对应界限",
      "溢出后以 Last-Event-ID=最后已应用的 seq 续接，没有快照请求",
      "A 的队列最大值不超过 2000 条与 4 MiB",
      "每个子例 30 s 内 A 的 seq 集合与数据库一致无重复",
      "B 的显示时延不超过 2 s，B 的 202 不超过 1 s",
      "host RSS 增量不超过 64 MiB"
    ],
    "observables": [
      "诊断面板读数",
      "SSE 请求头",
      "已应用 seq 导出",
      "时延记录",
      "RSS 采样",
      "判定结果"
    ]
  },
  {
    "id": "TEST-059-05",
    "requirement_id": "REQ-059",
    "acceptance_id": "AC-059-05",
    "check_id": "web-e2e",
    "fixture": "两个夹具项目 X、Y（[web] port 都为 P）+ 产品 host + Playwright（Chromium，同一浏览器上下文、授予剪贴板权限）+ 占用端口的 TCP 监听器",
    "steps": [
      "A 由 X 的 hoh web 打开并写入草稿",
      "重启 X 的 host，运行 hoh web 并在新标签页打开",
      "在 P 上换成 Y 的 host 并打开它的地址，再回到 X",
      "占用 P 后重启 X 的 host，运行 hoh web 并在新标签页打开，在 A 上复制草稿",
      "导出各 host.json 的 token、页面存储、网络日志、host 日志与已应用 seq"
    ],
    "pass_criteria": [
      "每次重启 token 都变，旧 token 403",
      "同址时 A 重新授权后续接，seq 集合与数据库一致无重复，草稿保留",
      "另一项目的授权不被采用，A 不显示其数据",
      "换地址时打印 P2，A 保持需要重新授权并可复制草稿",
      "token 不在存储、URL、Referer 与 host 日志中"
    ],
    "observables": [
      "host.json 前后",
      "页面状态时间线",
      "浏览器网络日志与存储导出",
      "剪贴板读数",
      "host 日志",
      "判定结果"
    ]
  }
]
```

#### REQ-060 多客户端操作（3 条 AC；实现：M6-T05）

`contracts/requirements.json` 中 REQ-060 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-060-01）：

```json
{
  "id": "REQ-060",
  "category": "WEB",
  "priority": "P1",
  "milestone": "M6",
  "title": "多客户端操作",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "expected_revision校验；冲突明确；只有一写者，可读多个客户端",
  "behavior": [
    "多个 TUI 与 Web 客户端可以同时附着同一 host；每个项目只有一个 host 与一个数据库 writer（REQ-062），客户端只经 /v1 读写、不持有任何租约，客户端的数量、附着与关闭不影响执行（CONTRACT 第 13 条）。",
    "每个写命令带 expected_revision（Run 的 revision；ConfigureTeam 为团队配置的 revision）与 idempotency_key：revision 不符时返回 409 STALE_REVISION，current_revision 为存储中的值，什么也不写；ApproveSpec 的摘要不符为 409 SPEC_CONFLICT（CR-026）；同 key 同载荷的重放返回首次的 202 字节并带 Idempotent-Replayed: true，同 key 不同载荷为 409 KEY_REUSE（CR-003）。",
    "Web 的冲突处理：依据所见内容的决定（SetMode、ConfigureTeam、ApproveSpec、AnswerQuestion）遇到 STALE_REVISION 或 SPEC_CONFLICT 时不自动重提，页面显示冲突与对方已生效的值（当前模式与待生效模式、团队配置的差异、两个摘要），由用户重新确认；与所见无关的控制（PauseRun、ResumeRun、CancelRun）按 ADR-031 决定 9 的规则重读 revision 并以新的 idempotency_key 重提，直到同一等待窗口结束（与 CLI 相同）。",
    "响应丢失（连接在提交后断开或超时）时，Web 以同一 idempotency_key 与同一载荷重试至多 3 次（间隔 1 s、2 s、4 s），不生成新 key；得到 202（首次或重放）之前页面显示“提交中”，成功只显示一次。",
    "各客户端由同一事件流收敛：一个客户端的写入只经已提交的事件反映到其他客户端，客户端不乐观地显示尚未被事件确认的结果（docs/05：UI 绝不能先显示事实成功）。"
  ],
  "failure_modes": [
    "两个客户端以同一 revision 并发写：恰一个 202，另一个 409 STALE_REVISION，数据库中只有一次变更。",
    "重试时载荷被改动：409 KEY_REUSE，页面显示为错误，不改用新 key 自动提交。",
    "host 在提交之后、响应之前崩溃：重启后同 key 的重试得到首次的结果，不重复生效。",
    "503 DB_BUSY、DB_FULL 或 STORAGE_LOW_SPACE（CR-028）：页面保留草稿并显示原因，只对 retryable=true 的提示重试。"
  ],
  "doc_refs": [
    "docs/03-ux.md",
    "docs/04-architecture.md",
    "docs/05-data-api.md",
    "docs/adr/ADR-016.md",
    "docs/adr/ADR-031.md",
    "contracts/openapi.json",
    "contracts/protocol.schema.json"
  ],
  "acceptance": [
    {
      "id": "AC-060-01",
      "given": "Run R 为 RUNNING、模式 BUILD、revision r；TUI 在 PTY 中附着 R，Web 页面 A、B 附着 R",
      "when": "TUI（/plan，切到 PLAN）与页面 A（选 AUTO）以同一 expected_revision=r 在同一时刻提交 SetMode，页面 B 只观看；随后页面 A 以已过期的 revision 提交 PauseRun",
      "then": "两个 SetMode 中恰一个得到 202，另一个得到 409 STALE_REVISION 且 current_revision 为存储中的值；数据库中只有获胜方的一次模式变更事件；失败方不自动重提并显示冲突与当前的模式与待生效模式；TUI 与两个页面最终显示相同的模式与待生效状态；PauseRun 经重读后恰被接受一次（一个 run.pause_requested）；全程只有一个 hoh __host 进程",
      "test_id": "TEST-060-01"
    },
    {
      "id": "AC-060-02",
      "given": "debug 构建的 host 的“提交后丢弃连接”接缝（ADR-016 决定 11 的 HostHooks）对页面的下一个命令生效；Run R 为 RUNNING",
      "when": "页面提交 SendInput“继续核对”，连接在提交后被丢弃，页面按规则重试；随后测试以同一 idempotency_key、不同的文本直接提交一次",
      "then": "页面的重试带同一 idempotency_key 与同一载荷，得到 202、带 Idempotent-Replayed: true，响应正文与 commands.result_json 逐字节相同；commands 一行、input_queue 一行、该输入的接受事件一条；页面只显示一次成功；改了文本的提交返回 409 KEY_REUSE，数据库不变",
      "test_id": "TEST-060-02"
    },
    {
      "id": "AC-060-03",
      "given": "团队配置 revision t；Run R 有待批的规格修订（spec_approval 问题，摘要 S2）；页面 A、B 都显示 Settings 与 Specs",
      "when": "页面 B 先把 qa 改为 pi（ConfigureTeam，被接受）；页面 A 随后以 revision t 把 qa 改为 codex；用户在工作树中改一份规格 Markdown 并重新生成索引（摘要 S3）后，页面 A 以 S2 批准",
      "then": "页面 A 的 ConfigureTeam 返回 409 STALE_REVISION 且 current_revision=t+1，页面显示当前绑定 qa→pi 与自己改动的差异，没有覆盖；批准返回 409 SPEC_CONFLICT 且 details 含 expected=S2 与 actual=S3，没有新修订，页面显示两个摘要并重新加载 Specs；team_revisions 只多了 B 的一行",
      "test_id": "TEST-060-03"
    }
  ],
  "decisions": [
    "过期写的重提规则按意图分两类：依据所见内容的决定不自动重提，控制命令按 ADR-031 决定 9 重读重提（CR-031，C17）；与 CR-028 中 TUI 对 SetMode 的 409 不自动重发一致。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-060 的场景（旧：只有 TEST-060-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-060-01",
    "requirement_id": "REQ-060",
    "acceptance_id": "AC-060-01",
    "check_id": "web-e2e",
    "fixture": "产品 host（FakeAgent）+ TUI 在 PTY 中附着 + 两个 Playwright 页面",
    "steps": [
      "记录 revision r",
      "TUI 与页面 A 同时以 r 提交 SetMode",
      "页面 B 观看",
      "页面 A 以过期 revision 提交 PauseRun",
      "导出命令、事件与三个客户端的显示"
    ],
    "pass_criteria": [
      "恰一个 202、一个 409 STALE_REVISION 且 current_revision 正确",
      "只有一次模式变更事件",
      "失败方不重提并显示冲突",
      "三个客户端显示一致",
      "PauseRun 恰被接受一次",
      "只有一个 host 进程"
    ],
    "observables": [
      "HTTP 响应",
      "commands 与 events 导出",
      "TUI 屏幕文本",
      "页面状态",
      "进程表",
      "判定结果"
    ]
  },
  {
    "id": "TEST-060-02",
    "requirement_id": "REQ-060",
    "acceptance_id": "AC-060-02",
    "check_id": "web-e2e",
    "fixture": "debug 构建的产品 host（提交后丢弃连接的接缝）+ Playwright 页面",
    "steps": [
      "启用接缝",
      "页面提交 SendInput",
      "观察页面的重试",
      "同 key 改文本直接提交",
      "导出 commands、input_queue 与事件"
    ],
    "pass_criteria": [
      "重试同 key 同载荷",
      "202 带 Idempotent-Replayed 且正文与 result_json 相同",
      "各表恰一行、接受事件一条",
      "页面只显示一次成功",
      "改载荷 409 KEY_REUSE 且数据库不变"
    ],
    "observables": [
      "浏览器网络日志",
      "响应头与正文",
      "表导出",
      "页面状态",
      "判定结果"
    ]
  },
  {
    "id": "TEST-060-03",
    "requirement_id": "REQ-060",
    "acceptance_id": "AC-060-03",
    "check_id": "web-e2e",
    "fixture": "产品 host + 待批规格修订的夹具项目 + 两个 Playwright 页面",
    "steps": [
      "页面 B 提交 ConfigureTeam",
      "页面 A 以旧 revision 提交 ConfigureTeam",
      "改规格 Markdown 并重新生成索引",
      "页面 A 以旧摘要批准",
      "导出 team_revisions 与 specs"
    ],
    "pass_criteria": [
      "ConfigureTeam 409 STALE_REVISION 且 current_revision=t+1，页面显示差异",
      "批准 409 SPEC_CONFLICT 且含两个摘要，无新修订",
      "页面重新加载 Specs",
      "team_revisions 只多一行"
    ],
    "observables": [
      "HTTP 响应",
      "页面状态",
      "表导出",
      "判定结果"
    ]
  }
]
```

#### REQ-065 Git集成与交付（5 条 AC；实现：M6-T06）

`contracts/requirements.json` 中 REQ-065 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-065-01；第 2 稿改写行为 2、6 与 AC-065-01、-02、-04，CR31D-02、-03、-08）：

```json
{
  "id": "REQ-065",
  "category": "RELEASE",
  "priority": "P1",
  "milestone": "M6",
  "title": "Git集成与交付",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "分支候选、PR/CI可选；merge后重新绑定最终SHA；无remote本地release可验收",
  "behavior": [
    "工作分支：Run 的第一个写 Attempt 准备时，Runtime 写 run.work_branch{branch, base_branch, base_commit}。HEAD 在分支 B 上时工作分支就是 B，HoH 不切换用户的分支；HEAD 游离、或 [git] delivery=\"pr\" 而 B 就是 PR 的目标分支时，Runtime 在当前提交上新建并切换到 hoh/run-<run_id 的前 12 个十六进制字符>（git switch -c，保留未提交的改动；无法切换时提问），记为该 Attempt 的 effect（C10）。目标分支为 [git] base，未配置时为 B。之后每个写 Attempt 派发前核对 HEAD 在工作分支上；不在时不派发，Run 以 NEED_DECISION 提问（问题原因 work_branch_moved：回到工作分支后继续，或取消）。",
    "候选绑定分支，两类候选（C23）：工作分支候选从用户工作树冻结（REQ-052 的清洁规则不变：有未提交或未跟踪的文件即拒绝，被 git 忽略的文件不在此列），commit 必须是工作分支的头，candidate.frozen 的载荷加 {branch: 工作分支, source: \"work_branch\"}；最终交付候选只在 PR 交付观察到合并后按行为 6 产生，commit 是目标分支上的最终提交 F，从只读核验 checkout 冻结，载荷为 {branch: 目标分支, source: \"merged\", pr_url, pr_head}。本地交付与 PR 头的验证只用工作分支候选。",
    "本地交付（[git] delivery=\"local\"，缺省，C6）：Run 的最终候选通过发布验证（REQ-056）后，Runtime 写 delivery.completed{mode: \"local\", branch, commit, tree, candidate_id}，releases 行绑定该候选；不 push、不调用 gh、不增删 remote、不改其他分支的引用；没有 remote 的仓库照样完成（docs/11）。",
    "PR 交付（[git] delivery=\"pr\"；[git] remote 缺省为 origin，须指向 github.com；PATH 中有 gh 且 gh auth status 成功）：最终候选通过发布验证后，Runtime (a) 以 git push <remote> <工作分支> 推送（不带 --force、--force-with-lease，从不推送目标分支）；(b) 先以 gh pr list --head <工作分支> --state all 按 run 标记（PR 正文中的 hoh-run: <run_id>）查找已有 PR，没有才 gh pr create --base <目标分支> --head <工作分支>；(c) Run 进入 WAITING_EXTERNAL（resume_state 为当时的状态），轮询 PR 与 CI 的状态（间隔 30 s 起加倍，至多 600 s；常量）。每个外部操作是交付 Attempt 的一条 effect（PREPARED→STARTED→OBSERVED→COMMITTED，REQ-041）；重启后按远端引用与 run 标记核对，不重复推送或建 PR（docs/07 §5），无法确认的为 UNKNOWN_EFFECT 并交诊断。",
    "CI 可选：PR 没有检查时不等待；有检查时等全部结束；任一失败时写 Issue（severity high，fingerprint ci:<检查名>，绑定 PR 头的候选）并唤醒 Commander，交付停止直到出现新的候选；CI 全部成功不等于合并。",
    "合并后重新绑定最终 SHA：HoH 不合并 PR（C7）。观察到 PR 已合并后，Runtime 取得并核对最终提交 F（merge 提交、squash 或 rebase 合并产生的新提交）后冻结最终交付候选（C23）：(a) git fetch <remote> +refs/heads/<目标分支>:refs/hoh/delivery/<run_id>/base，只写 refs/hoh/ 下 HoH 自己的引用，不改用户的分支、refs/remotes/、HEAD、index 与工作树；(b) gh pr view 报告 state=MERGED、mergeCommit=F、baseRefName 等于 run.work_branch 当前记录的目标分支，且 F 是取得的目标分支头的祖先或其本身；任一不符则不冻结，Run 以 NEED_DECISION 提问（delivery_merge_unverified，列出不符项）；(c) 在 .hoh/ 下按 run 与 F 命名、登记确切路径的位置以 git worktree add --detach 建立 F 的只读核验 checkout（任何写者都不在其中写入）；(d) 对它按 REQ-052 的同一清洁规则冻结（commit=F，tree 为 F 的 tree，source_path 为该 checkout）；(e) 在其上运行完整的发布验证，不复用 PR 头候选的任何证据，即使 tree 相同（docs/07 §6、§7，C18）；(f) release 绑定或 Run 结束后按登记的路径移除该 checkout 与 refs/hoh/delivery/<run_id>/ 下的引用。用户工作树的 HEAD、分支与文件在整个过程中不变。通过后 releases 行与 delivery.completed{mode: \"pr\", pr_url, merged_commit: F, candidate_id} 绑定新候选，Run 才能 COMPLETED；PR 头候选的证据保留为历史。最终交付候选的验证失败：写 Issue（绑定该候选）并唤醒 Commander，Run 不 COMPLETED，修复在工作分支上按行为 7 先 merge 目标分支再进行，并产生新的 PR。PR 被关闭而未合并：NEED_DECISION（delivery_pr_closed：重新打开后继续、改为本地交付或取消）。",
    "目标分支前移与改目标：PR 落后于目标分支，或 PR 的目标被改为另一分支时，Runtime 在持有写者租约、工作树干净、HEAD 在工作分支上时 fetch，并把目标分支 merge 进工作分支（新提交，第一父为原分支头）；不 rebase、不改写已推送的提交、不 force push（docs/24 第 5 节；未推送过的工作分支也用 merge，一条规则，C8）；推送后在新提交上冻结候选并重新验证，旧候选不再是交付候选。merge 冲突：git merge --abort，HEAD 与工作树回到 merge 之前，不推送，Run 以 NEED_DECISION 提问（delivery_conflict，列出冲突路径）。",
    "只用 git 与 gh 的 argv（不经 shell，docs/04）；不读写凭证，认证由 git 与 gh 自己的配置负责；hoh doctor 报告 git 与 gh 的版本与 gh auth status 的结论，不输出凭证。"
  ],
  "failure_modes": [
    "delivery=\"pr\" 而 gh 缺失：Run BLOCKED(BLOCKED_CAPABILITY)；gh 未认证：BLOCKED(BLOCKED_AUTH)；问题给出“修好后继续”或“改为本地交付”，不自动降级为本地交付。",
    "推送被拒（远端工作分支有本地没有的提交）：不 force；fetch 后按 merge 规则产生新提交与新候选。",
    "推送或建 PR 期间 kill -9：重启后 STARTED 的 effect 按远端核对（远端分支已是该提交、按标记找到 PR 即采用而不重做；确认远端没有该效果才执行），无法确认的（例如 forge 查询失败）为 effect UNKNOWN、交付 Attempt UNKNOWN_EFFECT 并交诊断，不盲目重做（REQ-041）。",
    "合并后的 F 核对不符（PR 不是 MERGED、mergeCommit 不在取得的目标分支中、目标分支名不符）：不冻结最终交付候选，NEED_DECISION（delivery_merge_unverified）。",
    "工作分支候选冻结时用户有普通未跟踪文件：REQ-052 拒绝冻结，HoH 不提交、不删除、不忽略该文件，Run 不能以它完成；用户删除、提交或忽略它之后才能继续。",
    "工作分支被用户切走或删除：不派发写 Attempt，以 work_branch_moved 提问。",
    "delivery=\"local\" 时完全不触网，有无 remote 都一样。"
  ],
  "doc_refs": [
    "docs/04-architecture.md",
    "docs/07-algorithms.md",
    "docs/11-deployment.md",
    "docs/12-user-guide.md",
    "docs/14-resilience.md",
    "docs/24-dev-lessons-to-product.md",
    "contracts/sql/001_initial.sql",
    "docs/adr/ADR-028.md",
    "config/hoh.example.toml"
  ],
  "acceptance": [
    {
      "id": "AC-065-01",
      "given": "三个没有 remote 的夹具 git 仓库：G1 的 HEAD 在分支 feature、工作树干净，另有用户文件 notes.local，它由 .git/info/exclude 忽略（不改任何已跟踪文件）；G2 的 HEAD 游离、工作树干净；G3 同 G1，但另有一个未被忽略的用户未跟踪文件 notes.txt；PATH 中的 gh 是记录调用的垫片；产品 host 与 FakeAgent 可把 BUILD Run 跑到 COMPLETED（含发布验证），FakeAgent 不触碰 notes.local 与 notes.txt",
      "when": "在 G1、G2 各运行一个 Run 直到 COMPLETED；在 G3 运行同样的 Run 直到它不再派发写 Attempt；前后导出全部引用（git for-each-ref）、git remote -v 与用户文件的 sha256",
      "then": "G1：run.work_branch 的 branch=feature，没有新分支，候选 commit 是 feature 的头，candidate.frozen 的 source=work_branch，releases 行绑定该候选，delivery.completed 的 mode=local、branch=feature、commit 为候选 commit；notes.local 字节不变，未被跟踪，也不在任何提交中；G2：新建 hoh/run-<run_id 前 12 位> 并在其上提交；G1、G2 的 git remote -v 前后为空，gh 垫片被调用 0 次，没有 push，引用导出前后只有工作分支变化；G3（反例，REQ-052 不变）：每次冻结都被拒且拒绝原因列出 notes.txt，candidates 行数为 0，没有 delivery.completed 与 releases 行，Run 不是 COMPLETED；notes.txt 字节不变、未被跟踪、不在任何提交中，.gitignore 与 .git/info/exclude 不变",
      "test_id": "TEST-065-01"
    },
    {
      "id": "AC-065-02",
      "given": "夹具仓库的 origin 是形如 https://github.com/o/r.git 的地址，经 git 的 url.<path>.insteadOf 指向本地 bare 仓库；PATH 中的 git 是记录每次 argv 后转调真实 git 的垫片，gh 是按录制的 gh JSON 字段形状实现的假 forge（记录每次 argv，可设为查询失败）；debug 构建的 host 有交付屏障接缝：在指定 effect 已为 STARTED 而外部调用之前，或外部调用已返回成功而 OBSERVED 尚未提交时停住并通知测试；[git] delivery=\"pr\"、base=main，工作分支 feature；Run 的最终候选 C 已通过发布验证",
      "when": "五次独立运行，各在一个屏障处对 host kill -9 并重启，直到 PR 建成或 Run 停下：(a) push 的外部调用之前；(b) push 已在 bare 仓库生效、OBSERVED 未提交；(c) gh pr create 之前；(d) forge 已建成 PR、OBSERVED 未提交；(e) 同 (d)，但重启后 forge 的查询（gh pr list）返回失败",
      "then": "(a)–(d)：bare 仓库中 feature 指向 C 的提交，main 未被推送，所有 git push 都不带 --force 或 --force-with-lease；(a) 重启后 git push 恰执行一次；(b) 重启后不再执行 git push（远端分支已是该提交即采用）；(c) gh pr create 恰被调用一次；(d) 重启后按标记的 gh pr list 找到已建的 PR 并采用，gh pr create 总共恰一次，forge 上只有一个 PR；每个 gh pr create 之前都有按标记查找的 gh pr list；PR 正文含 hoh-run: <run_id>；push 与 create 两条 effect 为 COMMITTED，create 那条的 external_ref 为 PR 地址；没有 UNKNOWN；Run 为 WAITING_EXTERNAL；(e)（反例）：create 那条 effect 为 UNKNOWN，交付 Attempt 为 UNKNOWN_EFFECT，有含该 effect_key 的诊断事件，重启后 gh pr create 被调用 0 次，forge 上只有一个 PR，Run 既不是 WAITING_EXTERNAL 也不是 COMPLETED",
      "test_id": "TEST-065-02"
    },
    {
      "id": "AC-065-03",
      "given": "AC-065-02 的 PR 已建成；假 forge 有三种情形：PR 没有检查；两个检查先 pending 后都 success；两个检查中一个 failure",
      "when": "以可控时钟推进轮询直到每种情形稳定",
      "then": "没有检查：不等待 CI，直接等待合并；pending→success：轮询间隔依次为 30、60、120…s 且不超过 600 s，成功后 Run 仍为 WAITING_EXTERNAL 等待合并；failure：写一条 Issue（severity high，fingerprint ci:<检查名>，candidate_id 为 PR 头的候选），Commander 被唤醒，交付停止、不再观察合并；三种情形下假 forge 都没有收到 gh pr merge",
      "test_id": "TEST-065-03"
    },
    {
      "id": "AC-065-04",
      "given": "PR 头候选 C（commit H1、tree T1）已通过发布验证；用户工作树的 HEAD 在 feature=H1、工作树干净，用户的本地分支 main 指向 M0；假 forge 分别以 rebase 合并（bare 仓库的 main 上得到 tree 相同、SHA 不同的 F1）、merge 提交（F2，两个父）与 squash（F3）合并该 PR，gh pr view 报告 state=MERGED 与 mergeCommit；第四种情形中 forge 报告的 mergeCommit 不在 bare 仓库的 main 中",
      "when": "host 观察到每种合并（各一次独立运行）",
      "then": "三种合并都先把目标分支取到 refs/hoh/delivery/<run_id>/base，再在 .hoh/ 下 F 的只读核验 checkout 上冻结新候选 C'（commit=F，tree 为 F 的 tree，source_path 为该 checkout，candidate.frozen 的 source=merged、branch=main），并在 C' 上运行发布验证，新的检查记录与证据都属于 C'，C 的证据不计入 C' 的验收；C' 通过之前 Run 不是 COMPLETED；通过后 releases 行与 delivery.completed 绑定 C' 与 F，Run 为 COMPLETED；rebase 情形中 F1≠H1 而 tree 相同，仍重新验证；从观察到合并到结束，用户工作树的 HEAD 仍为 feature=H1，git status 与文件摘要不变，refs/heads/ 与 refs/remotes/ 下的引用不变（本地 main 仍为 M0）；结束后核验 checkout 与 refs/hoh/delivery/<run_id>/ 都已移除；第四种情形：不冻结任何候选，Run 为 BLOCKED(NEED_DECISION)，问题原因 delivery_merge_unverified 并列出不符项",
      "test_id": "TEST-065-04"
    },
    {
      "id": "AC-065-05",
      "given": "PR 已建成，工作分支 feature 的头 H1 已推送；假 forge 依次：main 前移一个不冲突的提交；把 PR 的目标改为 release；把目标改回 main 并使 main 前移一个与 feature 冲突的提交",
      "when": "host 观察到每一次变化",
      "then": "第一次：feature 上出现第一父为 H1、第二父为新 main 头的 merge 提交 M1，推送不带 force，bare 仓库中 feature 的历史包含 H1，在 M1 上冻结新候选并重新验证，旧候选不再是交付候选；第二次：run.work_branch 的 base_branch 记为 release，release 被 merge 进 feature 并产生新候选；第三次：merge 冲突后执行 merge --abort，HEAD 不变、git status 干净，没有推送，Run 为 BLOCKED(NEED_DECISION)，问题原因 delivery_conflict 并列出冲突路径；全程没有 force push 与 rebase",
      "test_id": "TEST-065-05"
    }
  ],
  "decisions": [
    "交付缺省为本地（[git] delivery=\"local\"）；PR 交付须显式配置，因为推送与建 PR 是对外的动作（CR-031，C6）。",
    "HoH 不合并 PR，只观察合并并在最终提交上重新冻结与验证（CR-031，C7、C18）。",
    "两类候选：工作分支候选从用户工作树冻结，最终交付候选从 F 的只读核验 checkout 冻结，F 先经 forge 报告与目标分支的祖先关系核对；用户的 HEAD、分支与工作树不因交付而改变（CR-031，C23）。",
    "目标分支前移时以 merge 更新工作分支，从不 rebase 或 force push 已推送的分支（docs/24 第 5 节，所有者的“已推送分支只追加提交”，CR-031，C8）。",
    "HoH 在当前分支上工作，只在 HEAD 游离或 PR 交付而当前分支就是目标分支时新建 hoh/run-<id> 分支，之后不切换分支（CR-031，C10）。",
    "PR 路径的测试以本地 bare 仓库作 remote、以按真实 gh 只读录制的 JSON 字段形状实现的假 gh 作 forge，结果记为 product 而不是 live；不新增 live 检查（CR-031，C9）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-065 的场景（旧：只有 TEST-065-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-065-01",
    "requirement_id": "REQ-065",
    "acceptance_id": "AC-065-01",
    "check_id": "integration",
    "fixture": "三个无 remote 的临时 git 仓库（G1：feature 分支、干净、info/exclude 忽略的 notes.local；G2：游离 HEAD、干净；G3：同 G1 加未忽略的未跟踪 notes.txt）+ 产品 host + FakeAgent + gh 记录垫片",
    "steps": [
      "导出引用、remote 与用户文件摘要",
      "G1、G2 各跑一个 BUILD Run 到 COMPLETED",
      "G3 跑同样的 Run 直到不再派发写 Attempt",
      "再次导出引用、remote 与用户文件摘要",
      "读取 run.work_branch、candidate.frozen、冻结拒绝原因、delivery.completed 与 releases"
    ],
    "pass_criteria": [
      "G1 工作分支为 feature 且无新分支",
      "候选为分支头、source=work_branch 且 releases 绑定它",
      "delivery.completed 为 local 且分支与提交正确",
      "notes.local 不变、未跟踪、不在提交中",
      "G2 新建 hoh/run-<前 12 位>",
      "G1、G2 remote 为空、gh 0 次、无 push、引用只有工作分支变化",
      "G3 冻结被拒并列出 notes.txt，无候选、无 release，Run 未 COMPLETED，notes.txt 不变且未进入提交"
    ],
    "observables": [
      "引用导出前后",
      "events 导出",
      "releases 行",
      "垫片日志",
      "文件摘要",
      "判定结果"
    ]
  },
  {
    "id": "TEST-065-02",
    "requirement_id": "REQ-065",
    "acceptance_id": "AC-065-02",
    "check_id": "recovery",
    "fixture": "临时仓库 + 本地 bare 仓库作 origin（insteadOf 改写的 GitHub 地址）+ 记录 argv 的 git 垫片 + 假 gh forge（可设查询失败）+ debug 构建的产品 host 与交付屏障接缝",
    "steps": [
      "(a) 在 push 外部调用之前 kill -9 并重启",
      "(b) 在 push 生效而 OBSERVED 未提交时 kill -9 并重启",
      "(c) 在 pr create 之前 kill -9 并重启",
      "(d) 在 PR 已建成而 OBSERVED 未提交时 kill -9 并重启",
      "(e) 同 (d) 且重启后 forge 查询失败",
      "每次导出 bare 仓库引用、git 与 forge 的 argv 记录、effects 与诊断事件"
    ],
    "pass_criteria": [
      "(a)–(d) feature 指向 C 的提交、main 未推送、无 force",
      "(a) push 一次；(b) 重启后不再 push",
      "(c)、(d) pr create 总共恰一次且之前有按标记的 pr list，forge 上一个 PR",
      "PR 正文含 run 标记",
      "(a)–(d) 两条 effect COMMITTED 且 external_ref 为 PR 地址，无 UNKNOWN，Run 为 WAITING_EXTERNAL",
      "(e) effect UNKNOWN、Attempt UNKNOWN_EFFECT、有诊断、重启后 pr create 0 次"
    ],
    "observables": [
      "bare 仓库引用",
      "git 与 forge 的 argv 记录",
      "effects 与诊断事件导出",
      "Run 状态",
      "判定结果"
    ]
  },
  {
    "id": "TEST-065-03",
    "requirement_id": "REQ-065",
    "acceptance_id": "AC-065-03",
    "check_id": "integration",
    "fixture": "已建成的 PR + 假 gh forge 的三种 CI 情形 + 可控时钟",
    "steps": [
      "推进时钟直到稳定（无检查）",
      "推进时钟直到稳定（pending→success）",
      "推进时钟直到稳定（一个 failure）",
      "导出轮询时刻、issues 与 forge 记录"
    ],
    "pass_criteria": [
      "无检查时不等待 CI",
      "轮询间隔 30、60、120…且不超过 600 s，成功后仍等待合并",
      "失败时一条 high Issue 绑定 PR 头候选且唤醒 Commander、交付停止",
      "没有 gh pr merge"
    ],
    "observables": [
      "轮询时刻",
      "issues 导出",
      "监督触发器",
      "forge argv 记录",
      "判定结果"
    ]
  },
  {
    "id": "TEST-065-04",
    "requirement_id": "REQ-065",
    "acceptance_id": "AC-065-04",
    "check_id": "integration",
    "fixture": "已通过发布验证的 PR 头候选 C + 用户工作树在 feature=H1、本地 main=M0 + 假 gh forge 的 rebase、merge、squash 三种合并与 mergeCommit 不符的一种情形 + 产品 host（FakeAgent 验证者）",
    "steps": [
      "导出用户工作树状态与全部引用",
      "假 forge 合并 PR（每种方式一次独立运行）",
      "等待 host 取得并核对 F、在核验 checkout 上冻结新候选并验证",
      "mergeCommit 不符的情形",
      "导出 candidates、evidence、acceptance_results、releases、事件、引用与用户工作树状态"
    ],
    "pass_criteria": [
      "新候选 commit=F、tree 为 F 的 tree、source_path 为核验 checkout、source=merged",
      "新证据都属于 C'，C 的证据不计入",
      "C' 通过前 Run 未 COMPLETED",
      "releases 与 delivery.completed 绑定 C' 与 F",
      "rebase 情形 tree 相同仍重新验证",
      "用户 HEAD、工作树、refs/heads/ 与 refs/remotes/ 不变，核验 checkout 与 refs/hoh/delivery/ 结束后移除",
      "不符情形不冻结并以 delivery_merge_unverified 提问"
    ],
    "observables": [
      "引用与工作树状态前后",
      "候选与证据导出",
      "releases 行",
      "events 导出",
      "Run 状态时间线",
      "判定结果"
    ]
  },
  {
    "id": "TEST-065-05",
    "requirement_id": "REQ-065",
    "acceptance_id": "AC-065-05",
    "check_id": "integration",
    "fixture": "已建成的 PR + bare 仓库 + 假 gh forge（main 前移、改目标为 release、改回 main 并制造冲突）+ 产品 host",
    "steps": [
      "main 前移不冲突的提交",
      "PR 目标改为 release",
      "目标改回 main 并制造冲突",
      "每步后导出本地与 bare 仓库的历史、候选、问题与 forge 记录"
    ],
    "pass_criteria": [
      "M1 的父依次为 H1 与新 main 头，推送无 force，bare 历史含 H1，新候选并重新验证",
      "base_branch 记为 release 且产生新候选",
      "冲突后 merge --abort、HEAD 不变、工作树干净、无推送",
      "Run BLOCKED(NEED_DECISION) 且问题列出冲突路径",
      "全程无 force push 与 rebase"
    ],
    "observables": [
      "git log --graph 导出",
      "bare 仓库引用",
      "forge argv 记录",
      "questions 与事件导出",
      "判定结果"
    ]
  }
]
```

#### REQ-066 安装使用文档（3 条 AC；实现：M6-T03）

`contracts/requirements.json` 中 REQ-066 的整个对象换为下文（旧：outline，只有 summary 与一条 AC-066-01）：

```json
{
  "id": "REQ-066",
  "category": "RELEASE",
  "priority": "P1",
  "milestone": "M6",
  "title": "安装使用文档",
  "required_release": true,
  "release_class": "required",
  "release_targets": [
    "v1"
  ],
  "spec_maturity": "detailed",
  "summary": "二进制、内置资源、Web资产版本一致；安装配置命令经实跑",
  "behavior": [
    "发布包（build 检查产出的 hoh-release.tar.gz）含 hoh/bin/hoh、hoh/resources、hoh/web（含 web/manifest.json）、install.sh、LICENSE、THIRD_PARTY_NOTICES.md 与 hoh/manifest.json；hoh/manifest.json 记录 hoh 版本、构建 commit、resource_bundle_version、二进制与 web/manifest.json 的 sha256，由同一次构建写出（ADR-023 决定 8 的 commit 规则）。",
    "版本一致：hoh --version 的版本与 commit、内置资源的 resource_bundle_version、web/manifest.json 的版本与 commit 必须相同；host 第一次提供外壳之前核对 web/manifest.json 与其中每个文件的 sha256，不一致或缺失时外壳返回 503 WEB_ASSETS_UNAVAILABLE（REQ-058）；hoh doctor 增加 web_assets 一项（ok、missing、mismatch 或 corrupt）并报告 resource_bundle_version；页面页脚显示版本与 commit。",
    "发布包的 THIRD_PARTY_NOTICES.md 列出打包进 web/ 的每个 npm 包（名称、版本、许可证，取自 web/package-lock.json）；许可证不在允许清单（与 deny.toml 的允许清单相同）中时构建失败。",
    "文档实跑：docs/12 中安装、配置、Web 访问、诊断与卸载各节的每个 shell 代码块，都由文档执行器在全新 VM 中按原文执行，经非交互、非登录的 ssh 会话（与用户经 SSH 使用无桌面 VM 的方式相同，docs/24 第 5 节的 XDG_RUNTIME_DIR 教训），退出码与文档写明的一致；不能在没有 Agent 凭证的环境中执行的块必须带机器可读的不实跑标注与理由，执行器逐个列出；没有既未执行又未标注的块。",
    "支持的平台：Ubuntu 24.04 与 26.04 x86_64（CONTRACT 第 15 条）各在一台全新的官方云镜像 VM 中实跑，记录镜像名、版本与 sha256（与官方 SHA256SUMS 一致）；文档不声明其他平台。",
    "卸载保留数据：install.sh --uninstall 不动项目的 .hoh/、~/.config/hoh/ 与 drop-in（ADR-023）；重新安装同一个包后，原 Run 的 state、revision 与事件数不变且可以读出。"
  ],
  "failure_modes": [
    "文档中的命令失败或退出码与文档不符：执行器指出文件、行号与命令，检查失败；不以“示例”为由跳过。",
    "web/ 与二进制来自不同 commit（混装两个版本）：外壳 503 WEB_ASSETS_UNAVAILABLE{mismatch}，doctor 报告 mismatch，hoh web 以 1 退出；TUI 与 hoh -p 不受影响。",
    "VM 镜像取不到或摘要不符：检查失败，说明镜像与摘要，不记为 PASS。",
    "安装或文档命令需要 sudo：检查失败（安装不需要 root，docs/11）。"
  ],
  "doc_refs": [
    "CONTRACT.md",
    "docs/10-acceptance.md",
    "docs/11-deployment.md",
    "docs/12-user-guide.md",
    "docs/21-spec-governance.md",
    "docs/22-tooling.md",
    "docs/23-pre-m1-audit.md",
    "docs/adr/ADR-023.md",
    "deny.toml"
  ],
  "acceptance": [
    {
      "id": "AC-066-01",
      "given": "本候选构建的发布包；Ubuntu 24.04 与 26.04 的官方云镜像（sha256 与官方 SHA256SUMS 一致）各起一台全新 VM（qemu/KVM；cloud-init 只建一个非 root 用户与 ssh 密钥，不预装 hoh、Node、Rust 或任何 Agent）",
      "when": "把发布包复制进 VM，文档执行器经非交互、非登录的 ssh 依次执行 docs/12 安装、配置、Web 访问、诊断与卸载各节的全部 shell 代码块",
      "then": "两台 VM 中每个未标注的块都以文档写明的退出码完成（含 install.sh、hoh --version、hoh doctor --output json、hoh web 打印地址、install.sh --uninstall），全程没有调用 sudo；hoh --version 的 commit 等于候选 commit；标注不实跑的块逐个列出理由；没有既未执行又未标注的块；记录两镜像的名称、版本与 sha256",
      "test_id": "TEST-066-01"
    },
    {
      "id": "AC-066-02",
      "given": "本候选的发布包在临时 HOME 中安装（私有 systemd --user 管理器）；另有一个由不同 commit 构建的包的 web/ 目录",
      "when": "读取 hoh/manifest.json、hoh --version、hoh doctor --output json 的 resource_bundle_version 与 web/manifest.json，并在页面页脚读出版本；之后把已安装版本目录中的 web/ 换成另一 commit 的 web/，请求 GET / 并运行 hoh web 与 hoh doctor --output json；再把原 web/ 换回后改掉一个文件的一个字节，重复一次",
      "then": "四处的版本与 commit 相同，manifest 中每个文件的 sha256 与实际文件一致；THIRD_PARTY_NOTICES.md 列出 web/ 中打包的全部 npm 包及其许可证；换入另一 commit 的 web/ 后 GET / 为 503 WEB_ASSETS_UNAVAILABLE 且 details.reason=mismatch，hoh web 以 1 退出且 stdout 为空，doctor 的 web_assets 为 mismatch；改一个字节后三处都为 corrupt；两种情况下 /v1/runs 与 hoh -p 都照常",
      "test_id": "TEST-066-02"
    },
    {
      "id": "AC-066-03",
      "given": "AC-066-01 的 26.04 VM 在卸载之前：夹具 git 项目中以 hoh -p 'x' --detach 创建的 Run（没有规格，为 BLOCKED），记下它的 run_id、state、revision 与事件数",
      "when": "执行 install.sh --uninstall，列出 ~/.local/bin、安装版本目录、unit、项目 .hoh/ 与 ~/.config/hoh/；重新安装同一个包后经 hoh attach <run_id> --output json 读出该 Run",
      "then": "卸载退出 0，二进制、版本目录与 unit 都不在；项目 .hoh/ 的全部文件与 ~/.config/hoh/ 都在；重新安装后读出同一 run_id、state 与 revision，事件数不变；全程没有调用 sudo",
      "test_id": "TEST-066-03"
    }
  ],
  "decisions": [
    "文档实跑以 docs/12 的代码块为单位，未执行的块须带不实跑标注与理由（CR-031）。",
    "两个支持平台都在全新官方云镜像 VM 中实跑；VM 的启动与镜像缓存由 deployment 检查的脚本完成，镜像放在项目盘而不是 /tmp（docs/19，CR-031，C11、C12）。",
    "Web 资产不一致的码为新增的 WEB_ASSETS_UNAVAILABLE（503，details.reason 为 missing、mismatch、corrupt；CR-031，C13）。"
  ]
}
```

`contracts/acceptance-scenarios.json` 中 REQ-066 的场景（旧：只有 TEST-066-01，fixture 为“待该里程碑 spec 任务细化”）换为：

```json
[
  {
    "id": "TEST-066-01",
    "requirement_id": "REQ-066",
    "acceptance_id": "AC-066-01",
    "check_id": "deployment",
    "fixture": "本候选的发布包 + Ubuntu 24.04 与 26.04 官方云镜像（摘要与 SHA256SUMS 一致，缓存在项目盘）+ qemu/KVM + cloud-init（非 root 用户与密钥）+ 文档执行器 + sudo 记录垫片",
    "steps": [
      "校验镜像摘要并启动两台 VM",
      "复制发布包",
      "经非交互 ssh 执行 docs/12 相关各节的全部代码块",
      "收集每块的退出码与输出",
      "列出标注不实跑的块"
    ],
    "pass_criteria": [
      "未标注块全部以文档写明的退出码完成",
      "没有 sudo 调用",
      "hoh --version 的 commit 等于候选",
      "不实跑块都有理由",
      "没有未执行也未标注的块",
      "记录镜像名、版本与摘要"
    ],
    "observables": [
      "文档执行器报告",
      "每块的 stdout/stderr 与退出码",
      "镜像摘要",
      "sudo 垫片日志",
      "判定结果"
    ]
  },
  {
    "id": "TEST-066-02",
    "requirement_id": "REQ-066",
    "acceptance_id": "AC-066-02",
    "check_id": "deployment",
    "fixture": "本候选的发布包在临时 HOME 中安装（私有 systemd --user 管理器）+ 另一 commit 构建的 web/ 目录 + Playwright（Chromium）",
    "steps": [
      "读取四处版本与 commit",
      "核对 manifest 摘要与第三方声明",
      "换入另一 commit 的 web/ 后请求外壳、运行 hoh web 与 doctor",
      "换回后改一字节并重复",
      "请求 /v1/runs 并运行 hoh -p"
    ],
    "pass_criteria": [
      "四处版本与 commit 相同、摘要一致",
      "第三方声明覆盖全部打包的 npm 包",
      "mismatch 时 503 且 reason=mismatch、hoh web 退出 1 且无输出、doctor 为 mismatch",
      "改字节后三处为 corrupt",
      "/v1 与 hoh -p 照常"
    ],
    "observables": [
      "manifest 与 --version 输出",
      "doctor JSON",
      "HTTP 状态与错误体",
      "THIRD_PARTY_NOTICES.md",
      "判定结果"
    ]
  },
  {
    "id": "TEST-066-03",
    "requirement_id": "REQ-066",
    "acceptance_id": "AC-066-03",
    "check_id": "deployment",
    "fixture": "AC-066-01 的 26.04 VM（卸载前）+ 夹具 git 项目中以 hoh -p --detach 创建的 BLOCKED Run + sudo 记录垫片",
    "steps": [
      "记录 run_id、state、revision 与事件数",
      "执行 install.sh --uninstall",
      "列出安装物与数据目录",
      "重新安装同一个包",
      "以 hoh attach --output json 读出该 Run"
    ],
    "pass_criteria": [
      "卸载退出 0 且安装物不在",
      ".hoh/ 与 ~/.config/hoh/ 都在",
      "重新安装后 run_id、state、revision 与事件数不变",
      "没有 sudo 调用"
    ],
    "observables": [
      "目录列表前后",
      "attach 的 JSON 与退出码",
      "sudo 垫片日志",
      "判定结果"
    ]
  }
]
```

### 7. M6 实现任务的拆分（`contracts/development-tasks.json`）

M6-T02、M6-T03 保留 id，新增 M6-T05、M6-T06；M6-T01（spec）不变，M6-T04（verify）只把 `depends_on` 改为全部四个实现任务。每个实现任务至多 2 项需求、8 条 AC，以 `acceptance` 写明它拥有的 AC（REQ-061 的三条分在两个任务中）：

| 任务 | 需求（本任务的 AC 数） | AC | 检查（AC 用到的） | depends_on |
|---|---|---|---|---|
| M6-T02 | REQ-058（3）、REQ-061（3） | 6 | web-e2e（本任务交付，`tasks[].checks`） | M6-T01 |
| M6-T05 | REQ-059（5）、REQ-060（3） | 8 | web-e2e | M6-T02 |
| M6-T06 | REQ-065（5） | 5 | integration、recovery | M6-T01 |
| M6-T03 | REQ-066（3）、REQ-061（1） | 4 | deployment | M6-T02、M6-T05、M6-T06 |
| 合计 | 6 项 | 23 |  |  |

替换后的四个实现任务（M6-T02、M6-T03 的旧条目由下文取代；M6-T05、M6-T06 插在 M6-T03 之前）：

```json
[
  {
    "id": "M6-T02",
    "milestone": "M6",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Web 外壳与 hoh web 入口（同一 Engine、七个页面、OpenAPI 生成的客户端）、键盘与小视口；web-e2e 检查",
    "depends_on": [
      "M6-T01"
    ],
    "requirements": [
      "REQ-058",
      "REQ-061"
    ],
    "acceptance": [
      "AC-058-01",
      "AC-058-02",
      "AC-058-03",
      "AC-061-01",
      "AC-061-02",
      "AC-061-04"
    ],
    "checks": [
      "web-e2e"
    ],
    "done_when": "所列需求中本任务的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。交付 web-e2e 检查：scripts/product/web-e2e.sh 取代 tests/check-plan.json 中的占位 argv（not-implemented.sh web-e2e M6-T02），在 run_check 下运行 web/ 的单元测试与 Playwright 场景，把每个 TEST id 的结果写入 test-results.jsonl，没有测试通过或缺少本任务的任一 TEST id 时以 3 失败（与 deployment.sh 的 NO_TESTS 规则相同）；Playwright 与浏览器的版本锁定，缓存放在项目盘而不是 /tmp（docs/19）；被测 host 在去掉外层检查环境变量（HOH_EVIDENCE_DIR 等）的环境中运行（docs/24 第 5 节）。scripts/product/build.sh 把缺少 web/ 改为构建失败（docs/23 的移交），bundle.sh 把 web/dist（含 web/manifest.json）打进 hoh/web。host 的外壳路由（openapi 的 / 与 /assets/{asset}，CR-031）、按 web/manifest.json 记录的 Content-Type 提供资产（构建遇到表外扩展名即失败）、WEB_ASSETS_UNAVAILABLE 的产生与 web/manifest.json 的核对。web/ 的 API 客户端由 contracts/openapi.json 生成并提交，web-e2e 中有一项核对重新生成逐字节相同。host 的绑定、规范 authority、Host/Origin 校验与 hoh web 打印的地址都取 [web] host（缺省 127.0.0.1；localhost 固定绑定 127.0.0.1，::1 的 authority 加方括号，CR-031 C20、C22），另以 hoh-app 的 integration 测试覆盖 127.0.0.1、::1、localhost 三种取值与其他 authority 的 403。"
  },
  {
    "id": "M6-T05",
    "milestone": "M6",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "事件重连与多客户端：事件存储、断线续接、游标失效的快照重同步、慢客户端、过期写与幂等重试",
    "depends_on": [
      "M6-T02"
    ],
    "requirements": [
      "REQ-059",
      "REQ-060"
    ],
    "acceptance": [
      "AC-059-01",
      "AC-059-02",
      "AC-059-03",
      "AC-059-04",
      "AC-059-05",
      "AC-060-01",
      "AC-060-02",
      "AC-060-03"
    ],
    "done_when": "所列需求中本任务的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。host 端 SSE 对超过已提交水位的游标返回 409 CONFLICT 与 details{reason: cursor_ahead, watermark}（REQ-059 行为 5），另有 hoh-app 的 integration 测试覆盖它与 after/Last-Event-ID 矛盾的既有 409；debug 接缝（关闭 SSE 连接）只在 debug 构建中存在，release_seams.sh 核对 release 中没有。页面的重新授权（REQ-059 行为 7，C21）：同源 BroadcastChannel 交接与 project_id 核对，token 不进入任何存储，以真实的 host 重启换 token 测试（AC-059-05），不使用固定的测试 token。web/ 的 e2e 构建（vite build --mode e2e，输出目录与 web/dist 不同）提供消费暂停接缝，web-e2e 中有一项核对发布用的 web/dist 不含该接缝的标识（C24）。本任务改连接生命周期与并发写，按 docs/19 由两名互补的审查者审查。"
  },
  {
    "id": "M6-T06",
    "milestone": "M6",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "Git 集成与交付：工作分支、本地交付、PR 与 CI、合并后重新绑定最终 SHA、目标分支前移",
    "depends_on": [
      "M6-T01"
    ],
    "requirements": [
      "REQ-065"
    ],
    "acceptance": [
      "AC-065-01",
      "AC-065-02",
      "AC-065-03",
      "AC-065-04",
      "AC-065-05"
    ],
    "done_when": "所列需求中本任务的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。配置键 [git] delivery、remote、base 的加载器支持（解析、合法值、缺省、快照）与 config/hoh.example.toml 中的缺省值在同一提交加入，并在该提交上重跑 TEST-018-01（CR26D-103 的规则）；假 gh 的 JSON 字段形状来自对本仓库已合并 PR 的只读 gh 录制（gh pr view、gh pr list 的 --json 输出），录制所用的 gh 版本与命令写在夹具旁，录制不创建、不修改任何远端对象；hoh doctor 增加 git 与 gh 的版本与 gh auth status 的结论（不输出凭证）；交付 Attempt 的 effect 接入启动 reconcile（REQ-041）；debug 构建的交付屏障接缝（外部调用之前、外部调用成功而 OBSERVED 未提交）只在 debug 构建中存在，release_seams.sh 核对。TDD 之前写 ADR 定下最终交付候选的取得、核对、只读核验 checkout、冻结与清理（REQ-065 行为 6，C23），以及交付 Attempt 的形状与 delivery.* 事件名（C19）。HoH 不合并 PR、从不 force push。本任务有外部效果与恢复，按 docs/19 由两名互补的审查者审查。"
  },
  {
    "id": "M6-T03",
    "milestone": "M6",
    "kind": "implement",
    "owner_role": "developer",
    "writer": true,
    "title": "安装、版本一致与文档实跑；全新 VM（Ubuntu 24.04/26.04）与无桌面的远程访问",
    "depends_on": [
      "M6-T02",
      "M6-T05",
      "M6-T06"
    ],
    "requirements": [
      "REQ-066",
      "REQ-061"
    ],
    "acceptance": [
      "AC-066-01",
      "AC-066-02",
      "AC-066-03",
      "AC-061-03"
    ],
    "done_when": "所列需求中本任务的全部AC在冻结候选上有PASS证据（degradable需求可为带诊断与上游引用的DEGRADED）。deployment 检查（scripts/product/deployment.sh，归属仍为 M2-T14）增加全新 VM 场景：官方云镜像按 SHA256SUMS 校验并缓存在项目盘，cloud-init 种子与 VM 磁盘放在 mktemp -d 建在项目盘的目录中、结束即删；qemu 以 KVM 运行，KVM 不可用时检查失败（不以 TCG 降级冒充）；scripts/dev/test-infra.sh 负责预取镜像；四个 TEST 的结果写入 deployment 的 test-results.jsonl。deployment 检查的总时长须在 tests/check-plan.json 的 3600 s 内，本任务先测成本，超出时由 CTO 决定（CR-031 C12）。准备隔离的 Ubuntu 24.04 实例与版本记录，供 M7-T04 的 tui-pty 两平台收取（CR-028 应用清单对 M6-T03 的追加，以 M4-T01 实际写入的文字为准）。docs/12 的安装、配置、Web 访问（含相同端口的 ssh -L）、Git 交付与卸载章节按实际行为改写，不能实跑的块带不实跑标注；THIRD_PARTY_NOTICES.md 由 web/package-lock.json 生成，许可证用 deny.toml 的允许清单核对；hoh doctor 增加 web_assets 与 resource_bundle_version。"
  }
]
```

M6-T04 的 `depends_on`：

- 旧：`["M6-T02", "M6-T03"]`
- 新：`["M6-T02", "M6-T03", "M6-T05", "M6-T06"]`

拆分的理由：

- M6-T02 先行：它交付 web-e2e 检查（占位 argv 已写明 M6-T02，validate_bundle 要求占位名与归属一致，所以 web-e2e 的归属与 id 都不动）、`web/` 工程、外壳与 `hoh web`；REQ-061 中只依赖页面本身的两条（键盘、小视口）放在这里。build 缺少 web/ 即失败（docs/23 的移交）也在这里。
- M6-T05 依赖 M6-T02：重连与多客户端是页面的行为，需要 web-e2e 与页面；它同时改 host 的 SSE（cursor_ahead），是连接生命周期与并发写的改动，按 docs/19 用两名互补审查者。
- M6-T06 只依赖 M6-T01：Git 交付不需要 Web；它有外部效果与恢复，同样两名审查者。
- M6-T03 最后：文档实跑要覆盖 Web 访问与 Git 交付的配置（依赖 M6-T02、M6-T06），版本一致与远程访问要已完成的页面（M6-T05 之后页面才完整）；全新 VM 的场景同时承担 CR-028 给 M6-T03 的 24.04 准备，所以保留 M6-T03 这个 id。

### 8. 每条 AC 的依赖闭包（CR3R-02）

下表按每条 AC 实际用到的能力列出提供者（该 AC 所属任务本身或其祖先）。在本稿的副本中，表里点名的每个任务都已由脚本核对为所属任务本身或其祖先（结果见“验证方式”）；M4-T06 与 M4-T09 只在 CR-028 应用之后存在，标为“待 CR-028”；发布验证按 CR-030 第 1 稿归 M5-T07（A5-1），它只在 CR-030 应用之后存在，标为“待 CR-030”；两者都在 M6-T01 领取时重核。所需检查的交付者（validate_bundle 的归属规则）都在本任务或其祖先中。

| 任务 | AC | 检查 | 实际用到的能力与提供者（本任务或其祖先） |
|---|---|---|---|
| M6-T02 | AC-058-01 | web-e2e | hoh web 入口、外壳路由、资产的 Content-Type 与 token 处理（本任务）；host 的发现与拉起、Host/Origin/token、SSE（M2-T12、M2-T06、M2-T14）；PauseRun（M2-T06）；hoh -p --detach 与 FakeAgent 世界（M2-T13） |
| M6-T02 | AC-058-02 | web-e2e | 七个页面（本任务）；getRun 的 progress 与 ui 投影、TUI 状态栏四组（M4-T06（待 CR-028）、M4-T02）；/v1/timeline 与 BlockDescriptor（M4-T03、M4-T09（待 CR-028））；evidence 路由与 artifact 下载（M4-T03）；tasks 路由与规格索引（M3-T07、M3-T02）；acceptance 与 issues（M2-T18）；team 与待生效变更（M2-T07、M2-T17） |
| M6-T02 | AC-058-03 | web-e2e | Web 的命令提交与生成的客户端（本任务）；SetMode、SendInput 与 TUI 的模式切换和输入（M3-T09、M4-T02）；PauseRun、ResumeRun（M2-T06、M2-T13）；ConfigureTeam（M2-T07） |
| M6-T02 | AC-061-01 | web-e2e | 键盘流程、焦点与 axe-core（本任务）；ApproveSpec 与来源判定（M3-T02、M3-T04）；SetMode、SendInput（M3-T09）；PauseRun、ResumeRun（M2-T06、M2-T13）；ConfigureTeam（M2-T07）；证据与 artifact（M4-T03） |
| M6-T02 | AC-061-02 | web-e2e | 响应式布局与菜单（本任务）；时间线分页（M4-T03）；SetMode、SendInput（M3-T09） |
| M6-T02 | AC-061-04 | web-e2e | 规范地址、按 [web] host 绑定与 Host/Origin 校验、hoh web 打印的地址（本任务）；[web] 配置的加载与快照（M2-T07）；host 的拉起、端口与 host.json（M2-T12）；getRun 与 Overview（M2-T12、本任务） |
| M6-T05 | AC-059-01 | web-e2e | 页面的事件存储（本任务）；web-e2e 检查与 web/ 工程（M6-T02） |
| M6-T05 | AC-059-02 | web-e2e | 断线续接（本任务）；页面与外壳（M6-T02）；SSE 的 replay/live（M2-T12）；FakeAgent 世界（M2-T13） |
| M6-T05 | AC-059-03 | web-e2e | 快照重同步、host 的 cursor_ahead 与重新授权（本任务）；分页游标的 generation 与 410（M4-T03）；getRun 的 watermark（M2-T12）；启动 reconcile 的恢复事件（M2-T05）；PauseRun、ResumeRun（M2-T06、M2-T13）；页面与 hoh web（M6-T02） |
| M6-T05 | AC-059-04 | web-e2e | 队列上界、溢出续接与 e2e 构建的消费暂停接缝（本任务）；SSE 按页读取（M2-T12）；SendInput（M3-T09）；页面与 web-e2e 检查（M6-T02） |
| M6-T05 | AC-059-05 | web-e2e | 重新授权、同源交接与 project_id 核对（本任务）；hoh web 与外壳（M6-T02）；host 每次启动换 token、host.json 与 SSE 续接（M2-T12）；getRun 的 project_id（M4-T03，按 A4-10 待 CR-028）；FakeAgent 世界（M2-T13） |
| M6-T05 | AC-060-01 | web-e2e | 冲突显示与控制命令的重提（本任务）；SetMode 与 STALE_REVISION（M3-T09、M2-T12）；TUI 的 /plan 与 409 处理（M3-T09、M4-T02）；PauseRun（M2-T06）；页面（M6-T02） |
| M6-T05 | AC-060-02 | web-e2e | 同 key 重试（本任务）；幂等重放、KEY_REUSE 与 HostHooks（M2-T12）；SendInput 与 input_queue（M3-T09）；页面（M6-T02） |
| M6-T05 | AC-060-03 | web-e2e | 冲突显示（本任务）；ConfigureTeam 与团队 revision（M2-T07）；ApproveSpec 与 SPEC_CONFLICT（M3-T02）；spec_approval 问题（M3-T04）；Settings 与 Specs 页面（M6-T02） |
| M6-T06 | AC-065-01 | integration | 工作分支、候选的 source 与本地交付（本任务）；BUILD Run 跑到 COMPLETED（M3-T04、M2-T13）；冻结与未跟踪文件的拒绝（M2-T10、M2-T16）；发布验证与 releases 行（M5-T07，待 CR-030） |
| M6-T06 | AC-065-02 | recovery | push 与建 PR 的 effect、交付屏障接缝及其核对（本任务）；effect ledger、UNKNOWN_EFFECT、kill -9 reconcile 与 recovery 检查（M2-T05）；WAITING_EXTERNAL（M2-T19）；发布验证（M5-T07，待 CR-030） |
| M6-T06 | AC-065-03 | integration | CI 轮询与 Issue（本任务）；Issue 与 Commander 唤醒（M2-T18、M2-T08）；可控时钟与等待（M2-T19） |
| M6-T06 | AC-065-04 | integration | 合并观察、F 的取得与核对、只读核验 checkout 与最终交付候选（本任务）；冻结（M2-T10、M2-T16）；验证与汇合（M2-T16、M2-T18）；NEED_DECISION 的问题（M2-T13）；发布验证（M5-T07，待 CR-030） |
| M6-T06 | AC-065-05 | integration | merge 更新与冲突提问（本任务）；写者租约（M2-T03）；NEED_DECISION 的问题（M2-T13）；冻结与重新验证（M2-T16） |
| M6-T03 | AC-066-01 | deployment | 全新 VM 与文档执行器（本任务）；install.sh 与 deployment 检查（M2-T14）；hoh web（M6-T02）；hoh doctor（M1-T07）；文档中的 Git 交付配置（M6-T06） |
| M6-T03 | AC-066-02 | deployment | manifest、web_assets 与第三方声明（本任务）；外壳与 WEB_ASSETS_UNAVAILABLE（M6-T02）；安装（M2-T14）；hoh doctor（M1-T07） |
| M6-T03 | AC-066-03 | deployment | 全新 VM（本任务）；卸载保留数据与 unit（M2-T14）；hoh -p --detach 与 no_spec（M2-T13）；attach（M2-T06） |
| M6-T03 | AC-061-03 | deployment | VM 与 ssh 转发（本任务）；hoh web 与外壳（M6-T02）；loopback 绑定与 Host 校验（M2-T12）；SetMode（M3-T09）；ApproveSpec 的 TCP 来源判定（M3-T04）；安装（M2-T14） |

## 对未应用规格的假设（M6-T01 领取时逐条重核）

M6 在 M4、M5 之后实现。本稿写于 CR-028 已批准未应用、CR-030 与本稿并行起草之时。下列每条若在领取时不成立，先修订本 CR 并重新审查，再应用。

**M4（CR-028，已批准于 `dde5109f`，未应用）**

| ID | 假设 | 用到它的地方 |
|---|---|---|
| A4-1 | getRun 的快照与 watermark H 在同一读事务；`projection.progress`（n/m、degraded、open、reopened）与 `projection.ui`（待生效模式、阶段/任务等）由 M4-T06、M4-T02 交付，TUI 状态栏的四组（mode、阶段/任务、n/m、blocked/open/reopened）取同一投影 | REQ-058 行为 4，AC-058-02 |
| A4-2 | `/v1/timeline`、`TimelinePage`、`BlockDescriptor` 与稳定的 `block_id` 由 M4-T03（查询）与 M4-T09（归属）交付；evidence 路由与 artifact 的 Range、ETag 由 M4-T03 交付 | AC-058-02、AC-061-02、AC-059-03 |
| A4-3 | 分页游标绑定派生索引的 generation，重建提交后旧游标为 410 `CURSOR_EXPIRED` 且顶层 `snapshot_watermark`；有 debug 接缝触发同一重建函数；超过已提交水位的分页游标按无效（400/404）处理 | AC-059-03 (b)；C3 的一致性 |
| A4-4 | v1 不删除权威事件，SSE 的 410 在产品中不可达，只以 stub 覆盖客户端处理；SSE 的 409（after 与 Last-Event-ID 矛盾）保留 | REQ-059 行为 4、5；AC-059-03 (c) |
| A4-5 | 客户端缓存上限 1000 个块描述、4 MiB 正文、2 个在途页；503 `STORAGE_LOW_SPACE`、`DB_BUSY` 的 details | REQ-059 行为 6 的量级；REQ-060 失败模式 4 |
| A4-6 | CR-028 的应用清单在 M6-T03 的 done_when 追加“准备隔离 24.04 实例与版本记录”；本 CR 的 M6-T03 已照录这一句（“以 M4-T01 实际写入的文字为准”），应用时以那时的文字替换 | 第 7 节 M6-T03 |
| A4-7 | CR-028 的迁移编号预定为 003；本 CR 不加迁移 | 第 2 节第 5 条，C19 |
| A4-8 | CR-028 改 openapi 的路径与本 CR 新增的 `/`、`/assets/{asset}` 不重名 | 第 5 节 |
| A4-9 | M4 的 TUI 对 SetMode 的 409 不自动重发、不改显示的模式（CR-028 的 AC-010-02） | AC-060-01 中 TUI 一方失败的情形 |
| A4-10 | CR-028 的 UI 读模型绑定 `project_id`（CR-028 决定部分：“UI 读模型绑定 project_id/run_id/…/watermark”），页面经 getRun 可读到它（今天 getRun 的 `projection` 是开放对象，没有 project_id）；若 CR-028 应用后 getRun 读不到 project_id，M6-T01 应用前在本 CR 中改为由 M6-T05 在 getRun 的 projection 中加入它并重新审查 | REQ-059 行为 7、AC-059-05 (b) |

**M5（CR-030 第 1 稿，`prep/M5-T01-spec` 的 `6c0fc5b6`，设计审查中、未批准）**

| ID | 假设 | 用到它的地方 |
|---|---|---|
| A5-1 | REQ-056 的发布验证（对最终候选的全范围验证，由 Runtime 触发，使 Run→COMPLETED 可以满足）按候选进行，可以在任意新冻结的候选上再做一次，包括从只读核验 checkout 冻结的最终交付候选；`releases` 行绑定通过它的候选。CR-030 第 1 稿把 REQ-056 交给新任务 M5-T07，以持久的 release_validation_id 绑定 run/spec/candidate；闭包表据此写 M5-T07（待 CR-030）。若批准的 CR-030 再改归属或不允许对新候选再做一次，M6-T01 应用前修订本 CR | REQ-065 行为 3、6；AC-065-01、-02、-04 |
| A5-2 | REQ-044（可恢复版本：保存失败分支，回到已验证基线，不 reset 用户未提交的改动；CR-030 第 1 稿规定建立指向失败候选的失败分支与新的恢复提交，归 M5-T06）若规定 HoH 创建或切换分支，本 CR 的“HoH 不切换工作分支、HEAD 离开工作分支即提问”须与之对齐：由 REQ-044 规定的切换不算“被用户切走” | REQ-065 行为 1，C10 |
| A5-3 | REQ-057（运行时 UX 与部署验证：PTY/浏览器/安装/升级/回滚的实际流程）若有用到浏览器（web-e2e）的 AC，它的实现任务只能在 M6-T02 之后；若 CR-030 把这类 AC 交给 M6 的任务，M6-T01 应用时把它并入合适的 M6 任务并保持每任务至多 2 项需求、8 条 AC；若 CR-030 先建了全新 VM 的测试设施，M6-T03 复用它而不另建（CR-030 第 1 稿：REQ-057 归 M5-T08，其 AC 用两种全新 VM 验证 HoH 对目标工程的部署与浏览器流程，不验证 HoH 自己的 Web；M6-T03 应用时核对能否复用其 VM 设施） | 第 7 节 |
| A5-4 | REQ-063（备份与迁移）的恢复会让数据库回到较早的 seq；AC-059-03 (a) 不依赖它（测试自己在停机时复制数据库文件），只要求页面对这种游标按快照重同步 | AC-059-03 |
| A5-5 | CR-030 若新增 ErrorCode，与 `WEB_ASSETS_UNAVAILABLE` 不重名；若新增 SQL 迁移，本 CR 不受影响 | 第 3 节 |
| A5-6 | CR-030 若给 M6 的任务追加 done_when（如 CR-028 对 M6-T03 那样），应用时并入 | 第 7 节 |
| A5-7 | CR-026 应用时的更正把“同类问题连续两轮即先写设计修正”“轮数上限”“运行中 STEER 绑定代码提交或候选”交给 M5-T01；本 CR 不处理它们。Web 的 SendInput 沿用 M5 定下的绑定方式，不另定 | REQ-060；docs/24 的复核 |

**M3（CR-026，已应用，实现进行中）**

| ID | 假设 | 用到它的地方 |
|---|---|---|
| A3-1 | 批准的来源判定对 TCP 连接“由本机连接表找到对端进程”（REQ-008 行为 1）：浏览器进程与经 SSH 转发时 VM 中该用户的 sshd 会话进程都不带 Attempt 标记、不在 Attempt 的 cgroup 中，判为 user | AC-061-01、AC-061-03 |
| A3-2 | ApproveSpec 的摘要不符为 409 `SPEC_CONFLICT`，details 含 expected 与 actual（REQ-034） | AC-060-03 |
| A3-3 | SetMode、SendInput、`run.mode_change_pending` 与输入四态由 M3-T09 交付 | AC-058-03、AC-060-01、-02 |

## docs/24 的复核（CLAUDE.md：细化任务写 CR 前先读 docs/24）

docs/24 明确点给 M6 的只有第 5 节“已推送分支只追加提交 → REQ-065”一条；其余与 M6 相关的条目逐条处理如下。CR-026 应用时的更正把两项部分覆盖的条目（2.3 的设计修正与轮数上限、2.7 的 STEER 绑定）交给 M5-T01，不在本 CR 中处理（A5-7）。

| docs/24 条目 | 结论 | 依据 |
|---|---|---|
| 第 5 节：主会话在已推送的 PR 分支上 rebase 后强制推送（REQ-065） | 吸收 | REQ-065 行为 4、7 与决定：从不 force push、不 rebase 已推送的分支，目标分支前移用 merge；AC-065-02 断言所有 push 不带 `--force`/`--force-with-lease`，AC-065-05 断言远端历史含原分支头、全程无 force 与 rebase |
| 第 5 节：非登录 shell 缺 `XDG_RUNTIME_DIR`（REQ-006） | 吸收进文档实跑 | REQ-066 行为 4：文档命令经非交互、非登录的 ssh 会话在全新 VM 中执行，与用户经 SSH 使用无桌面 VM 的方式相同；缺少的环境要么由产品处理，要么由文档写出并被实跑（AC-066-01） |
| 第 5 节：`/tmp` 是共享配额的 tmpfs（REQ-064） | 吸收进任务的 done_when | VM 镜像、VM 磁盘、Playwright 的浏览器与缓存都放在项目盘（M6-T02、M6-T03 的 done_when）；产品行为属 REQ-064（M4） |
| 第 5 节：嵌套的运行继承外层检查的 `HOH_EVIDENCE_DIR`（REQ-025、REQ-053） | 吸收进任务的 done_when | web-e2e 与 deployment 中被测的 host 在去掉外层检查变量的环境中运行（M6-T02 的 done_when） |
| 第 5 节：打包脚本猜测产物路径（REQ-069） | 部分覆盖，其余留 M7 | REQ-066 行为 1、2 与 AC-066-02：发布包中二进制、资源与 Web 资产的版本与 commit 两两核对，web/ 与二进制不一致时拒绝提供；“由构建工具说出产物路径”属交付真实性（REQ-069，M7-T01） |
| 第 5 节：改受锁契约后只跑了 bundle 与指名测试（REQ-033、REQ-052） | 吸收进应用步骤 | 本 CR 增加一个 ErrorCode：应用提交同时改 Rust 枚举并跑完整的 `cargo test --workspace --locked`（见“基线”行与“应用时的步骤”）；副本中已演示不改 Rust 枚举时 protocol_roundtrip 变红（见“验证方式”） |
| 2.5 消息交叉与未确认的指令（REQ-022；M4 的 Team 视图显示未确认的指令） | 沿用 M4 | Web 的 Team 页取与 TUI 相同的投影（AC-058-02 逐字段对照）；未确认指令的显示由 CR-028 定义 |
| 2.6 只清理自己登记的资源 | 吸收进测试设施 | VM、bare 仓库、sshd 与浏览器进程都按确切路径与 pid 登记、结束时只清理自己登记的；产品中最终交付候选的只读核验 checkout 与 `refs/hoh/delivery/<run_id>/` 引用也按登记的确切路径移除（REQ-065 行为 6）（M6-T03 的 done_when“结束即删”的对象是本次 `mktemp -d` 的目录） |
| 1.1–1.3、2.1–2.4、2.7、3.1–3.3、4.1–4.2 | 不在 M6 | 分别属于 M2–M5 已定或 M5、M7 的细化（各条“落地时机”）；Web 只显示 Runtime 已有的事实，不改变这些机制 |

## CTO 的问题与决定（C1–C24）

已记录的决定（来源：`/home/ai/dev/hoh/.hoh-dev/evidence/spec-drafts-cto-decisions.md`，主仓库中被 git 忽略的 CTO 决定记录，2026-11-22）：

- **C1：所有者于 2026-11-22 批准。** 静态外壳不带 token 提供，仍校验 Host/Origin；每个 `/v1` 请求都要 token；token 在 URL 片段中，从不进入 host 日志或 Referer；docs/03 第 51 行在应用本 CR 时改为“每个 API 请求”。它改写的是所有者写下的交互/安全约定，所以由所有者而不是 CTO 决定；已决定，不再询问。
- **C2–C20：CTO 采纳本稿第 1 稿的提议**，不再询问。第 2 稿按设计审查改写了 C15 的触发方式（见 C24）与 C20 的地址规则（见 C22），提议的取向不变。
- **C21–C24：第 2 稿为处置设计审查第 1 轮的发现而新增，待 CTO 决定**；它们已按“提议”写进上文的需求，CTO 改变任一项时相应文字随之修改后再送审查。它们都不改 CONTRACT 的架构或交互决定、不缩小范围、不降低门槛，也不改变 C1 的所有者决定（token 仍只在片段与页面内存中，C21 的同源交接不经过 host）。

没有需要所有者决定的未决问题。

| ID | 问题 | 提议 | 理由 |
|---|---|---|---|
| C1（所有者已批准，2026-11-22） | Web 外壳是否要 token | 外壳（`/`、`/assets/`）不要 token、仍校验 Host/Origin 并带 CSP 与 no-referrer；每个 `/v1` 请求都要 token；token 不进入 host 日志或 Referer；docs/03 的“每个请求”改为“每个 API 请求” | openapi 的 localToken 把 token 放在 URL 片段，片段不发给服务器，外壳必须先于 token 加载；外壳不含项目数据、不调用命令，API 的认证不变 |
| C2 | Web 资产嵌入二进制还是放在版本目录 | 版本目录中的 `web/`，由 `web/manifest.json` 与二进制的版本、commit、文件摘要核对 | workspace 不允许构建脚本（docs/22），`include_bytes!` 会让 cargo 构建依赖 npm 产物；docs/11 的布局已是“资源按版本目录” |
| C3 | SSE 游标超过已提交水位时的响应 | 409 `CONFLICT` 加 `details{reason: cursor_ahead, watermark}`，不用 410 | 与 CR-028 对超过水位的分页游标按无效处理一致，也不必改 CR-028 中“SSE 410 在 v1 不可达”的 AC-010-03；客户端对两者都做快照重同步 |
| C4 | 测哪些浏览器 | Chromium 跑全部 web-e2e 场景，Firefox 跑 AC-058-01 与 AC-061-01；docs/12 只声明这两种 | 覆盖两个主流引擎的入口与键盘可达性，成本可控；不宣称未测的浏览器（与 CONTRACT 第 15 条同一原则） |
| C5 | Web 能否编辑 Prompt | 只读显示各层与修订；编辑仍按 REQ-047 的文件覆盖 | 新增写命令要改受锁的命令种类（protocol 的 Command.kind 与 001 的 commands.kind CHECK）；REQ-058 的 summary 只要求“可查看” |
| C6 | 缺省的交付模式 | `[git] delivery = "local"`；PR 交付须显式配置 | push 与建 PR 是对外动作；与所有者 O1–O6 的保守取向一致 |
| C7 | 谁合并 PR | 用户或仓库规则；HoH 不合并、不改目标分支 | 合并对外且不可逆；REQ-065 只要求合并后重新绑定 |
| C8 | 目标分支前移时怎样更新工作分支 | merge（不 rebase、不 force push），未推送的分支也用 merge | 所有者“已推送分支只追加提交”（docs/24 第 5 节）；一条规则比按是否推送过分两条更可判定 |
| C9 | PR 路径用什么测 | 本地 bare 仓库作 remote、假 gh（字段形状取自对本仓库已合并 PR 的只读 gh 录制），记 product；不新增 live 检查 | 真实 GitHub 的写操作对外、要仓库与凭证；新增检查会改 release-policy 的 14 项。残余风险：gh 输出格式变化，由录制版本与 doctor 报告的 gh 版本对照，docs/12 写明 |
| C10 | 工作分支 | 在当前分支上工作；HEAD 游离或 PR 交付而当前分支就是目标分支时新建 `hoh/run-<run_id 前 12 位>`；之后 HoH 不切换分支，HEAD 离开即提问 | 不改变写者在用户工作树中工作的既有前提（REQ-028），也不在多个 Run 之间偷偷切换分支 |
| C11 | 远程访问在哪里测 | 全新 Ubuntu 26.04 云镜像 VM（qemu/KVM）中运行 host，宿主经 `ssh -L P:127.0.0.1:P` 访问 | Host 精确校验要求两端端口相同，同机做不到；本机禁止无特权用户命名空间（背景中的实测）；同一 VM 也承担 REQ-066 与 CR-028 的 24.04 准备 |
| C12 | deployment 检查的时长 | 两台 VM 加原有的 TEST-005-06 在现有的 3600 s 内；M6-T03 先测成本，超出时由 CTO 决定是否调整 `tests/check-plan.json` 的 timeout（不减少内容） | 调整 timeout 不放宽任何判定，但改变检查计划，须先有测量 |
| C13 | Web 资产不一致的错误码 | 新 ErrorCode `WEB_ASSETS_UNAVAILABLE`（503，details.reason 三值） | ReasonCode 的新码须经 CR；一个码加 details 比三个码简单 |
| C14 | 可达性怎样判定 | axe-core（锁定版本）的 WCAG 2.1 A/AA 规则零违规，加键盘全流程、焦点可见与四种视口无横向滚动 | 可机器判定；不以截图观感判定（REQ-057 的同一原则） |
| C15 | 慢客户端的界限 | 页面队列至多 2000 条或 4 MiB，溢出即续接；host RSS 增量至多 64 MiB；B 页的显示时延至多 2 s、命令 202 至多 1 s；每个子例 30 s 内追上 | 与 CR-028 的客户端缓存同一量级；时延断言宽于 REQ-068 的 500 ms，避免与 M7 的性能检查重复；溢出的触发方式见 C24 |
| C16 | CI 的来源与轮询 | gh 报告的 PR 检查；没有检查不等待；失败写 high Issue 并停止交付；轮询 30 s 起加倍至 600 s（常量） | CI 可选；常量避免再加配置键 |
| C17 | Web 遇到过期写是否自动重提 | 依据所见内容的决定不重提；Pause/Resume/Cancel 按 ADR-031 决定 9 重读重提 | “冲突明确、不悄悄覆盖”（docs/03）；控制命令与 CLI 一致 |
| C18 | 合并后的最终提交怎样验证 | 在最终提交上冻结新候选并完整跑发布验证，不复用 PR 头候选的证据，即使 tree 相同 | docs/07 §6“不得仅按文件路径相同就重用旧证据”、§7“不能用 PR 头旧证据证明 merge commit” |
| C19 | 交付 Attempt 的载体 | Runtime 建立的 `kind='work'` Task 与 `agent` 为空的 Attempt，外部操作记为它的 effect；若实现证明需要新的 `tasks.kind`，另写 CR 加迁移 | 001 与 002 已允许这一形状；不在细化 CR 中预设迁移 |
| C20 | `[web] host` 与实际绑定不一致（范围外发现：加载器接受 `::1`、`localhost` 等 loopback，host 却恒绑 `127.0.0.1` 且不读该键，`crates/hoh-app/src/host/mod.rs` 的 bind 与 `http.rs` 的 `check_request`） | M6-T02 让 host 的绑定、Host/Origin 校验与 `hoh web` 打印的地址都取 `[web] host`（缺省 127.0.0.1），不缩小加载器接受的取值 | 不缩小 REQ-018 已接受的配置；让默认 loopback 的行为与配置一致、可判定（REQ-061 行为 4） |
| C21 | host 重启换 token 后页面怎样重新授权（CR31D-01） | 此前有效的 token 被 403 即进入“需要重新授权”，保留投影与草稿；用户运行 `hoh web` 并在新标签页打开；新页面以同源 `BroadcastChannel("hoh-auth")` 交出 `{project_id, token}`，旧页面核对消息与 getRun 的 project_id 后采用并续接；端口变了则旧页面不同源，只提供“复制草稿”；以真实的 host 重启换 token 测试（AC-059-05） | 不改 host 每次启动换 token 的现状，也不持久化 token（C1 的“token 只在内存中”不变）；浏览器读不到 host.json，只有用户再次运行 `hoh web` 才能得到新 token；同源通道不经过 host，token 不进日志；project_id 核对防止同一端口上换成另一项目的 host。未采用：把 token 持久化以便跨重启复用（扩大泄露窗口、改 M2 已验收的行为）；`/healthz` 返回 token 或可换取 token 的凭据（不带认证的端点交出凭据） |
| C22 | 配置地址、监听地址、规范 authority 怎样统一（CR31D-07） | IPv4 loopback 字面量按原样绑定，authority 为 `<v>:<port>`；`::1` 只绑定 IPv6，authority 为 `[::1]:<port>`；`localhost` 固定绑定 127.0.0.1，authority 为 `localhost:<port>`；Host 逐字节等于规范 authority，其他一律 403；AC-061-04 测三种取值 | 不缩小 REQ-018 已接受的取值；localhost 不经解析器，绑定可判定；DNS 重绑定的名字不可能等于配置的规范 authority，所以仍被拒；一个 authority 使打印的地址、Host 与 Origin 校验一致 |
| C23 | 合并后的最终提交从哪里冻结（CR31D-02） | 区分工作分支候选（用户工作树、分支头）与最终交付候选（F）；F 先经 `gh pr view` 的 mergeCommit 与取得的目标分支的祖先关系核对，再在 `.hoh/` 下的只读核验 checkout（`git worktree add --detach`）上按 REQ-052 的规则冻结，之后完整重新验证并按登记路径清理；取得只写 `refs/hoh/`；用户的 HEAD、分支与工作树不变；核对不符以 `delivery_merge_unverified` 提问 | 合并后用户工作树的 HEAD 可能仍是 H1，现有冻结从调用 checkout 的 HEAD 冻结；切换用户工作树会改用户的分支并与写者冲突；`candidates.source_path` 已能记录不同的源路径，候选清单与表不需改动 |
| C24 | 慢客户端的溢出怎样确定地触发（CR31D-05） | web/ 的 e2e 构建（`vite build --mode e2e`）提供只暂停“应用”的接缝，条数与字节两种界限各一个子例；接收、排队、溢出关闭与续接用与发布构建相同的代码；发布用的 `web/dist` 不含接缝（web-e2e 核对）；真实双客户端的时延与 RSS 另作子例，不要求溢出 | CPU 降速不能保证生产快于消费，正确的快实现会被判失败；与 host 的 debug 接缝同一原则：接缝只在测试构建中 |

## 不变的部分

- 6 项需求的 id、category、priority、milestone、title、summary、required_release、release_class（required）与 release_targets（v1）都不变；没有删除任何需求或 AC。
- 每项原有的 AC-xxx-01 保留 id，改写为可判定的内容。原 AC 的 then 照抄 summary；summary 的每一项仍由该需求自己的 AC 判定，没有被缩小：

| 需求 | summary 的各项 → 判定它的 AC |
|---|---|
| REQ-058 | 同一 Engine → 058-01、058-03；不启动第二任务循环 → 058-01；可查看规格/团队/时间线/证据 → 058-02（原 given 的“浏览器与 TUI 同状态”→ 058-02 的 TUI 状态栏对照） |
| REQ-059 | 序号去重 → 059-01、059-02；replay/live 交接无遗漏 → 059-02（慢客户端 059-04）；过期 cursor 走 snapshot resync → 059-03 |
| REQ-060 | expected_revision 校验 → 060-01、060-03；冲突明确 → 060-01、060-03；只有一写者、可读多个客户端 → 060-01（并发重试的幂等 060-02） |
| REQ-061 | 核心功能可用（键盘、小视口）→ 061-01、061-02；默认 loopback → 061-03、061-04；没有桌面环境也可访问 → 061-03 |
| REQ-065 | 分支候选 → 065-01、065-02；PR/CI 可选 → 065-01（无 PR）、065-02、065-03；merge 后重新绑定最终 SHA → 065-04；无 remote 本地 release 可验收 → 065-01；原 when 的 rebase、change-base → 065-04（rebase 合并）、065-05（目标前移与改目标） |
| REQ-066 | 二进制、内置资源、Web 资产版本一致 → 066-02；安装配置命令经实跑 → 066-01；原 when 的“全新 VM 安装和卸载保留数据” → 066-01、066-03 |

- 六个 TEST-xxx-01 的 check 都不变（058、059、060、061 为 web-e2e，065 为 integration，066 为 deployment）；新增的 TEST 用 web-e2e、integration、recovery、deployment，都在 v1 档的必跑检查中。
- 不改：CONTRACT、`docs/10-acceptance.md`、`contracts/release-policy.json`、`tools/release_gate.py` 与其余门禁工具、`tests/check-plan.json`（web-e2e 的 argv 由 M6-T02 交付时替换，这是 CR-003 的既定移交）、`contracts/sql/` 下的全部迁移、`contracts/candidate.schema.json`、各 schema 中除 ErrorCode 之外的部分、`config/hoh.example.toml`、Run 的状态迁移表（AC-036-01）、各检查的归属（web-e2e 仍归 M6-T02，deployment 仍归 M2-T14）。没有门禁阈值、检查或证据规则被放宽。
- 非 M6 的需求只有 REQ-018 增加一条决定（`[git]` 键），它的行为与 AC 不变。
- 开发流程的 low 清理包（docs/19）在 M6-T04 之前照常进行，不是 development-tasks 中的任务。

## 是否属于 CR-003 D1

1. 把 6 项 outline 细化为 detailed，补齐一个原因码（`WEB_ASSETS_UNAVAILABLE`）与 openapi 中 Web 外壳的两条路径，REQ-018 的键表增加 `[git]`（第 1 条）。
2. 不删需求或 AC；required、release_class、release_targets 不变（第 2 条）。
3. 不放宽任何门禁、检查或证据规则；PR 头候选的证据不能用于最终提交（加严）；文档命令须实跑或逐个标注理由（加严）（第 3 条）。
4. 不改 CONTRACT 的架构与交互决定：Web 是同一 host 的客户端（第 2、6、13 条），命令面不变（第 3 条），默认 loopback 与 SSH 转发照 docs/11；交付由 Runtime 执行、Commander 不写（CR-024）。C1 改写了 docs/03 中所有者写下的一句交互/安全约定（“每个请求”→“每个 API 请求”），已按 docs/21 交所有者，所有者于 2026-11-22 批准；本 CR 的其余部分（含第 2 稿新增的 C21–C24）都在 D1 之内（第 4 条）。

故本 CR 拟由 CTO 依 D1 在独立设计审查 PASS 之后批准；C1 部分已有所有者的批准。

## 审查记录

### 第 1 轮（设计审查，第 1 稿）

- 审查者：Codex（codex-cli 0.171.0，reviewer 角色，独立会话），attempt `ad5d1c51-ca3b-403d-bb00-4cbc2c92f064`，会话 `01a1ff66-a5ec-7513-a941-ae275877f90a`；2026-11-21T18:37:55Z–18:48:30Z，退出码 0。
- 对象：冻结提交 `05b2ef6c`（tree `432b4182`，基线 `8d9ae599`），只改了本文件。
- 记录：主仓库 `.hoh-dev/evidence/m6spec-review-codex-r1.out`（桥的摘要，其 `report` 字段为 null）；`hoh-report` 取自同一 attempt 的 `.hoh-dev/attempts/ad5d1c51-ca3b-403d-bb00-4cbc2c92f064/stdout.jsonl` 中最后的 agent 消息。
- 结论：FAIL，3 high（CR31D-01..03）、6 medium（CR31D-04..09）、0 low。审查者在副本中复现了第 1 稿的机械验证（validate_bundle PASS、闭包 0 处不符），发现都是结构校验查不出的语义问题。

| ID | 级别 | 发现（摘要） | 处置（第 2 稿） |
|---|---|---|---|
| CR31D-01 | high | host 每次启动换 token，页面只能从首次的 URL 片段取得 token，AC-059-03 要求的重启后自动重连会先得到 403，到不了 409 重同步 | 已修：新增重新授权协议（C21；第 1 节、REQ-059 行为 7 与失败模式、REQ-058 行为 2 与失败模式）：此前有效的 token 被 403 即进入“需要重新授权”并保留草稿，用户再运行 `hoh web` 并在新标签页打开，新页面经同源 BroadcastChannel 交出 token，旧页面按 project_id 核对后采用并续接；换了端口时提供复制草稿。新增 AC-059-05（同址、换项目、换地址三种，真实的 host 重启换 token，不用固定 token），归 M6-T05；AC-059-03 (a) 加入重新授权步骤 |
| CR31D-02 | high | 候选必须是工作分支头与“合并后冻结 F”矛盾；merge、squash、rebase 后工作树 HEAD 可能仍是 H1，现有冻结从 checkout 的 HEAD 冻结 | 已修：区分工作分支候选与最终交付候选（C23；第 2 节第 6 条、REQ-065 行为 2、6 与失败模式）：fetch 到 `refs/hoh/delivery/<run_id>/base`，以 gh 的 mergeCommit 与祖先关系核对 F，在 `.hoh/` 下的只读核验 checkout 上冻结，完整重新验证后按登记路径清理，用户的 HEAD、分支与工作树不变；AC-065-04 对三种合并都断言这些并加核对不符的反例；M6-T06 的 done_when 要求 TDD 前写 ADR |
| CR31D-03 | high | AC-065-01 的正例带普通未跟踪文件 notes.txt 却要求 COMPLETED，与 REQ-052 的冻结拒绝矛盾 | 已修：正例 G1 改为干净仓库，用户文件 notes.local 由 `.git/info/exclude` 忽略；新增反例 G3（未忽略的 notes.txt）：冻结被拒并列出该文件、无候选与 release、Run 不 COMPLETED、文件不进入提交；REQ-052 不变；REQ-065 增加对应失败模式 |
| CR31D-04 | medium | 重启时 reconcile 会写恢复事件，AC-059-03 把错误体、快照与续接水位都固定为 120 不成立 | 已修：AC-059-03 (a) 以静止夹具（Run 在 seq=120 时暂停）保证 120≤H<L，H 取 409 之后立即读出的数据库最大 seq，分别断言错误体的 watermark=H、快照 H'≥H 与续接；明写不关闭恢复事件 |
| CR31D-05 | medium | CPU 降速 20 倍与 5000 条事件不保证溢出，正确的快实现会被判失败；等待追上没有期限 | 已修：AC-059-04 改为以 e2e 构建的消费暂停接缝确定地触发条数（2100 条）与字节（300×16 KiB）两种界限，断言续接而非快照，每个子例 30 s 期限；双客户端时延与 RSS 另作子例，不要求溢出（C24；M6-T05 的 done_when 要求发布构建不含接缝） |
| CR31D-06 | medium | `/assets/{asset}` 只声明 application/octet-stream，模块脚本在 nosniff 下不会执行 | 已修：openapi 片段按 manifest 记录的类型列出 text/javascript、text/css、image/svg+xml、image/png、font/woff2，构建遇到其他扩展名即失败；REQ-058 行为 2、5；AC-058-01 断言资产的 Content-Type 与 manifest 一致、页面已渲染且控制台无 MIME/CSP 错误 |
| CR31D-07 | medium | 成功地址恒为 127.0.0.1、localhost 一律拒绝，与 C20“绑定、地址与 Host/Origin 都取 [web] host”矛盾 | 已修：规范地址规则（C22；第 1 节、REQ-061 行为 4、REQ-058 行为 1 与失败模式）：IPv4、`[::1]`、`localhost`（固定绑定 127.0.0.1）三种 authority；新增 AC-061-04（三种配置的成功例与其他 authority 的 403），归 M6-T02；AC-058-01 的拒绝例改用 `rebind.test` |
| CR31D-08 | medium | AC-065-02 的 kill 点只写“STARTED 之后”，测试可在外部调用之前杀掉 host，覆盖不到“远端已生效、本地未登记”的对账路径 | 已修：AC-065-02 以 debug 构建的交付屏障接缝分五种：push/create 的外部调用之前、远端已生效而 OBSERVED 未提交（断言采用已有远端对象、不重复 push 或 create），以及 forge 查询失败时 UNKNOWN/UNKNOWN_EFFECT、重启后 create 0 次的反例；git 垫片记录 push；M6-T06 的 done_when 列出接缝与 release_seams 核对 |
| CR31D-09 | medium | 本节仍称 C1–C20 待 CTO 决定、没有所有者问题，与已有的 CTO 决定记录不符 | 已修：“CTO 的问题与决定”记录 C1 已由所有者于 2026-11-22 批准（静态外壳不带 token、仍校验 Host/Origin、每个 /v1 请求要 token、token 不进日志或 Referer、docs/03:51 改为“每个 API 请求”），C2–C20 已由 CTO 采纳，C21–C24 为第 2 稿新增、待 CTO 决定；同步了文首的批准人、“是否属于 CR-003 D1”第 4 条与应用步骤（docs/03 第 51 行的改写） |

本轮之外的更新：CR-030 第 1 稿（`6c0fc5b6`）已提交，按它更新了 A5-1（发布验证归 M5-T07，闭包表标“待 CR-030”）、A5-2、A5-3；增加 A4-10（getRun 可读到 project_id）。

第 2 轮应审查第 2 稿相对 `05b2ef6c` 的差异：上述修正、新增的 AC-059-05 与 AC-061-04、C21–C24，以及这些修正是否引入新的矛盾。

## 验证方式

第 3–7 节的片段由应用脚本从本文的 JSON 块直接取出（每节的 ```json 块与 `/v1/events` 的 ```text 块），在仓库之外的副本上机械验证，不改工作树中的受锁文件。副本中的 CR 与本文只差“状态”一行（副本中为 APPROVED 以便 relock）以及“审查记录”“验证方式”两节的文字；片段部分逐字相同。第 2 稿重新写了应用脚本与闭包核对脚本（第 1 稿所用的脚本未保留），两者都放在副本目录中，用完与副本一并删除。

1. `mktemp -d /home/ai/dev/m6spec-XXXXXX` 建立副本目录；`git archive` 取 `prep/M6-T01-spec` 的 HEAD（`05b2ef6c`：基线 revision 22 的 main `8d9ae599` 加第 1 稿）解到其中，`git init` 后提交为基础，再放入第 2 稿的 CR 并把“状态”改为 APPROVED。
2. 应用脚本（`python3 -I <副本目录>/tools/apply_cr031.py <副本>`）按第 3–7 节整体替换六个需求对象与它们的场景（场景放在旧场景的位置）、给 REQ-018 的 decisions 末尾追加一条、给 ErrorCode 的 enum 末尾追加 `WEB_ASSETS_UNAVAILABLE`、给 openapi 增加两条路径（先核对不重名）并在 `/v1/events` 的说明末尾追加一句、替换 M6-T02 与 M6-T03 并把 M6-T05、M6-T06 插在 M6-T03 之前、把 M6-T04 的依赖从 `["M6-T02", "M6-T03"]` 改为四个实现任务；`config/hoh.example.toml` 不动。
3. 在副本中运行 `python3 tools/validate_bundle.py`（预期只有受锁文件变化与生成视图过期）、`python3 tools/gen_spec_views.py`、`python3 tools/baseline.py relock --cr docs/cr/CR-031-m6-detailed-spec.md`（不需 `--add`：没有新契约）、`python3 tools/validate_bundle.py`，再以 `source .hoh-dev/env.sh` 的环境运行 `bash scripts/check.sh --bundle`。
4. 闭包核对脚本（`python3 <副本目录>/tools/closure_cr031.py <副本>`，用 `tools/validate_bundle.py` 的 `ancestors` 与 `owned_acceptance`）核对第 8 节表中点名的每个提供者都是该 AC 所属任务本身或其祖先（只有表中标“待 CR-028”“待 CR-030”的 M4-T06、M4-T09、M5-T07 例外，它们在副本中不存在）、每条 AC 恰归一个 M6 实现任务、表中的检查与场景的 check_id 一致、表的行恰为 23 条 AC。另以一次变异确认脚本能报错：在 AC-065-03 的提供者中加入非祖先 M6-T05，脚本报 1 处不符。

结果（第 2 稿提交前的最后一次运行，2026-11-21 UTC）：

- 应用脚本：退出码 0（applied）
- 应用片段后、生成视图与 relock 之前的 `python3 tools/validate_bundle.py`：退出码 1，status=FAIL，只有三项预期的错误（受锁的 contracts/acceptance-scenarios.json 已变、docs/02 与 docs/18 过期），没有归属或格式错误
- `python3 tools/gen_spec_views.py`：退出码 0
- `python3 tools/baseline.py relock --cr docs/cr/CR-031-m6-detailed-spec.md`：退出码 0（Baseline HOH-BASELINE-1.1 revision 23 verified (27 files)）
- `python3 tools/validate_bundle.py`：退出码 0；status=PASS，errors=[]；counts：requirements=70、detailed_requirements=49（原 43）、acceptance=231（原 214）、development_tasks=67（原 65）
- 副本中改动的文件：contracts/acceptance-scenarios.json、contracts/baseline-lock.json、contracts/development-tasks.json、contracts/openapi.json、contracts/protocol.schema.json、contracts/requirements.json、docs/02-functional-spec.md、docs/18-traceability.md、docs/cr/CR-031-m6-detailed-spec.md
- 闭包核对：退出码 0；23 行、23 条 AC、121 处提供者引用（含“本任务”），5 处为“待 CR-028/CR-030”，0 处不符；M6-T02=2 项需求/6 条 AC、M6-T05=2/8、M6-T06=1/5、M6-T03=2/4，23 条 AC 都恰有一个所属任务；变异检查：报 1 处不符（not ancestor M6-T06 AC-065-03 M6-T05）
- `bash scripts/check.sh --bundle`（同一副本）：退出码 0；Ran 570 tests in 146.731s；OK (skipped=2)：`test_ledger_outline_requirements_not_ready`（“M3 already detailed”）与 `test_repository_packages_verified`（“Pi packages not installed here”），两项在 main 上同样跳过，不计为产品 PASS。此前一次未加载 `.hoh-dev/env.sh` 的运行因缺少 jsonschema 与 `XDG_RUNTIME_DIR` 失败（186 failures、31 errors），属环境错误，不计入；另一次运行在片段的最后文字修改之前被中止，结果未用
- Rust 演示（第 1 稿第 5 步）在第 2 稿中没有重跑（NOT_RUN）：第 3 节的 ErrorCode 改动与第 1 稿逐字相同，第 1 稿的结果仍适用（只改 schema 时 `cargo test -p hoh-core --locked --test protocol_roundtrip` 失败于 `rust_enums_list_exactly_the_schema_values`，加入 `WebAssetsUnavailable` 后 8 passed，`cargo check --workspace --all-targets --locked` 退出码 0）；完整的 `cargo test --workspace --locked` 是应用时的步骤（“应用时的步骤”第 3 条），也没有运行

本 CR 的 23 条 AC 都是待实现的规格，产品的红/绿与全部 web-e2e、deployment、integration、recovery 场景都 **NOT_RUN**；以上只是规格片段的机械自检，不是验收。设计审查第 1 轮为 FAIL（见“审查记录”），第 2 轮尚未进行。

## 应用时的步骤（M6-T01 领取时，M5-T04 之后）

1. 按当时的 main 重核本 CR：“对未应用规格的假设”逐条核对（CR-028、CR-030 已应用后的 requirements、openapi、protocol、development-tasks），第 8 节的闭包表把“待 CR-028”“待 CR-030”的提供者换成实际任务后重新机械核对；有变化的先写进本 CR 的更正、经审查后再应用。
2. 按 docs/21 的“基线变更的机械步骤”在工作树中应用第 3–7 节的片段（都是完整对象：六个需求对象与它们的场景整体替换，REQ-018 的 decisions 末尾追加一条，ErrorCode 的 enum 末尾追加一个值，openapi 增加两条路径并在 `/v1/events` 的说明末尾追加一句，M6 的任务按第 7 节替换与插入；本稿使用的应用脚本见“验证方式”），在同一提交中把 `WEB_ASSETS_UNAVAILABLE` 加进 `crates/hoh-core/src/protocol.rs` 的 `ErrorCode`，并把 docs/03 第 51 行的“每个请求都要携带……token”改为“每个 API 请求”（C1，所有者已批准），写明外壳不含项目数据。
3. `python3 tools/gen_spec_views.py`；`python3 tools/baseline.py relock --cr docs/cr/CR-031-m6-detailed-spec.md`；`python3 tools/validate_bundle.py`；`bash scripts/check.sh --bundle`；`cargo test --workspace --locked`（docs/24 第 5 节）；提交。
