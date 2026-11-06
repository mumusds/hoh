# AI自主开发执行手册

你现在开发的是 HoH 本身，不是另一个示例产品。阅读顺序：CONTRACT（含 CR-001）→ 当前任务关联的需求（`contracts/requirements.json`，生成视图见 `docs/02-functional-spec.md`）→ 架构、数据 → 编排、算法 → 适配 → UX → 验收。按需读全文，不要一次把全部文档塞进上下文。

## 启动

在已选择的隔离 VM 中，以非 root 用户打开主开发会话（默认 Claude Code），粘贴 LAUNCH_PROMPT。该会话先执行 `bash scripts/bootstrap.sh --yolo`；之后每个新 Shell 先 `source .hoh-dev/env.sh`（也会加载可选的 `.env`），再运行包自测和 preflight；每台 VM 还要运行一次 `bash scripts/dev/test-infra.sh` 准备测试基础设施（CR-003 D3）。已有 CLI/模型配置保留；缺工具可以安装；缺真实认证这类只有用户能提供的条件，精确记录为阻塞。

## 自举编排（CR-001）

- **M0–M2：** 一个主开发会话（顶层，单写者）加独立 Review 会话。Review 通过 `python3 tools/agent_task.py --agent codex --role reviewer --prompt-file <渲染后的prompt>` 发起一个全新会话；Codex 提示“模型容量已满”（服务端临时拥挤，不是额度用完）时，先换模型重试 Codex（如 `--model gpt-6.1-sol`），仍不行再改用全新的 Claude 或 Grok 会话；需要时可让另一个 Agent 的全新会话在同一候选上并行做第二份独立 Review。四个 Agent（Codex、Claude、Pi、Grok）都是足额订阅（项目所有者，2026-11-05）。Grok 只用于自举阶段的 Review/QA，不是产品 Agent，也不能在 run_check 检查内运行（`contracts/live-session.schema.json` 只接受 codex、claude、pi，桥会以 64 拒绝），检查内的会话用这三个 Agent。Review 只读冻结候选、不改代码。
- **v0.1 发布：** 在 M2 全部完成后（CR-003 D2）。QA 与 Review 必须是两个不同的独立会话，且都不同于开发会话（门禁会核对原生 attempt 记录中的 session_ids）。检查内的独立会话用 `agent_task.py --report-out "$HOH_EVIDENCE_DIR/independent-report.json"` 运行：Agent 在最后一条消息里给出 ```hoh-report 代码块，身份与哈希由桥在会话结束后填写。
- **M2 通过后：** 按产品默认团队接入 Codex 指挥与 Pi QA，并先用小任务与外部结果比对，不让未验证的 Runtime 自己签收自己。
- **防嵌套：** `agent_task.py` 给每个原生子进程设置 `HOH_BOOTSTRAP_PARENT`；带这个变量的会话不能再以 `--top-level` 或 `start.sh` 启动顶层会话。
- **会话身份：** `HOH_BOOTSTRAP_PARENT` 只表示“已在自举会话之内”，`start.sh` 启动的顶层会话自己也带它，不能单凭它判定为委派会话。委派会话带 `HOH_DELEGATED_TASK=<role>`（输入以 `[HOH_DELEGATED_TASK]` 开头），或其 `HOH_BOOTSTRAP_PARENT` 不等于顶层会话设置并向下继承的 `HOH_TOP_LEVEL_SESSION`；用户直接启动、没有这些标记的会话是顶层主会话（docs/22）。

## 执行节奏

```bash
python3 tools/taskboard.py list
python3 tools/taskboard.py next
python3 tools/taskboard.py claim M1-T01 --owner main-dev
# …TDD、实现、提交、冻结候选、独立 Review、修复…
python3 tools/taskboard.py set M1-T01 done --owner main-dev --note ".hoh-dev/evidence/… 与 review 报告路径"
```

一个任务的流程：引用任务的 `acceptance`（缺省为其需求的全部 AC）与 `checks` → 先写会失败的测试并确认失败原因（覆盖 TEST-NNN-MM 的测试名含 `test_NNN_MM`，检查脚本据此写出 `test-results.jsonl`）→ 最小实现 → 局部检查 → 提交 → `candidate.py freeze` → `run_check.py` 运行计划内的检查 → 独立 Review → 修复 → 推送、PR、CI 通过后合并 → 更新任务状态。依赖 outline 需求的任务会被 `claim` 拒绝：先完成该里程碑的 spec 任务（见 docs/08）；只细化不减范围的 CR 按 CR-003 D1 经独立 Review 后由 CTO 批准（docs/21）。每个任务完成后继续下一个，不等用户确认下个里程碑，也不在 v0.1 停下。

每次会话结束或压缩前按“恢复”一节的格式更新 `.hoh-dev/PROGRESS.md`。不要只留自由聊天记忆。RESUME_PROMPT 按事实重建现场，不能因为上次 Agent 说“完成”就相信。

## 持续推进与等待（只对顶层会话）

- 持续执行依靠原生能力（Claude Code 的 `/goal` 或用户的指令），不另造无限发送“继续”的外层循环，也不叠加功能重复的继续执行钩子；会话告警钩子（StopFailure/SessionEnd，docs/22）只记录与提醒，不是续跑机制。一轮正常结束（Stop）不挂钩子。
- 长命令、检查与外部审查用 Claude 管理的后台任务（`run_in_background`），在 PROGRESS 的“Current state”里记下原生 task ID、attempt 目录、候选、结果路径与收取条件；不用 `nohup`/`&` 后口头说“会等通知”。新环境先用一个无副作用的短任务验证“启动→可见→完成通知→读取结果→继续”。
- 每次准备结束回复前依次检查：已完成但未处理的结果（先消费）、可执行的任务（就执行）、真实等待项（必须有有效的托管任务及其唤醒条件）。没有任务、没有唤醒条件而目标未完成时，不得以“等待”结束；状态更新不等于结束长任务。
- 委派的 Review/QA/Research 会话完成指定任务后正常结束，不适用本节。

## 审查收敛

- 每轮事先写明：候选（candidate_id、commit、tree）、diff 范围、REQ/AC、风险范围、通过条件（模板：范围审完且无 critical/high）；多个审查者事先分配不相交的编号段。
- 首轮完整审查；修复后的复审聚焦修复 diff、相关调用链与回归，有新证据才合理扩展范围。一般一个独立 reviewer；文件删除、进程生命周期、并发等高风险变更用两个互补的 reviewer（不同 Agent、不同侧重点）。
- critical/high 必须修复并复审；medium 对照实际 AC 与用户可观察行为决定本轮修复或记录延期（写进 PROGRESS 的 Follow-ups）；low 与风格建议默认进后续清单，不自动触发“修 low→完整重审”。已解决项单独列出，不当作新发现；保留审查者给的等级，开发者可记下不同看法但不得自行降级。报告结论冲突时按证据裁决（可复现的缺陷优先），不按多数表决。
- 同一类问题连续两轮再现时，暂停在同一路径上叠补丁：复盘完整的不变量与资源生命周期，写一份简短的设计修正（必要时先让只读助手对抗性检查），验证后再恢复实现。这不是停工，也不设强制过审轮数。

## 并行与分层验证

- 始终只有一个主写者。需要时最多并行 2～3 个只读助手，分别负责规格/影响分析、最小复现与测试设计、独立审查；每个子任务写明范围、输入版本（冻结的 commit 或 worktree）、输出与停止条件，不递归启动顶层会话。等待审查时主会话只做不修改受审候选的工作；下一版本若在另一 worktree 开发，必须独立冻结并取得自己的检查与审查证据。不为额度利用率制造任务。
- 验证分层：先最小复现与受影响测试，再计划内的完整检查，最后正式候选的 QA/Review。变异测试用来核实关键断言能变红，不为杀死率无限加变异。检查全部通过、版本未变且无新证据时，不机械重跑整套检查。恢复时核对历史证据的候选、规格与环境并标明是历史结果；新候选绝不沿用旧候选的验收。

## 恢复

先对账再行动：`python3 tools/taskboard.py list`、`git worktree list` 与各 worktree 的分支/dirty 状态、`gh pr list` 与 CI、仍在运行的进程、`.hoh-dev/attempts/` 中已结束但未处理的结果。running 任务先核实实际 owner；旧会话确已结束才按台账规则接管。台账以 taskboard 为准，不新增第二套台账。`.hoh-dev/PROGRESS.md` 顶部只写最新现场：主分支与工作区、当前任务/owner、候选、已收/待收结果（含托管任务 ID）、阻塞、下一条具体动作（`## Next`，SessionStart 钩子读取它）；旧轮次移到历史区并保留证据链接。

## 自举限制

原生 CLI 可能因服务、token 或实现限制而终止；本包不假称一个提示词能强迫 Agent 无限持续。依靠底座原生的长任务和续会话能力，加上明确的 checkpoint；不允许用 while true 空转追加“继续”。

允许自主做小范围实现决策，并留下 ADR；不重复询问用户已采纳的架构与交互。实现细节未知时，先做源码研究或小实验。只有重大范围改变、降低门槛、改变 CONTRACT 决定、身份认证和外部费用授权才需要升级给用户；已授权与仍需询问的清单见 CLAUDE.md。

## 不能走的捷径

- 只交 MVP、v0.1 或架构文档就称完工。
- 用 mock 冒充三 Agent 集成；用 `--mode` 自报 live。现在 mode 只能来自冻结的检查计划。
- 删除 Web、插件、自定义角色或时间线。插件只能按 CR-001 带证据降级。
- 让开发者自己当唯一验收者；把 BUNDLE 自测当产品测试。
- 降低或跳过 required gate；编辑受锁文件而不写 CR。
- 捏造官方接口、执行结果、用量或模型名称。

## 提交与报告

小批提交，使用 feature branch（`task/<id>-<slug>`），commit 关联任务与 REQ；有 remote 就走 PR/CI（项目所有者已授权推送、开 PR，并在 CI 通过且独立 Review 无 critical/high 时 `gh pr merge --rebase`），没有就用本地候选。每个阶段按真实执行结果列出 PASS/FAIL/BLOCKED/NOT_RUN 与 mock/product/live。只有 release_gate 对 v1 profile 通过才能报告 COMPLETED（或 COMPLETED_DEGRADED）；否则如实报告 INCOMPLETE/BLOCKED 与恢复点。

## 子任务防串岗与基线状态

单次桥默认添加 `HOH_DELEGATED_TASK` 信封。所有原生 Agent 读取根 AGENTS 时，先按这个信封进入局部执行，不能因为项目总目标而自动接管整个工程。指定的 role 不是装饰标签：输出必须保留 task/attempt/role 身份，QA 与 Review 不得转成开发。

`contracts/` 下的需求、场景、policy 与 schema 受基线锁保护，不保存实现进度。实施进度写 `.hoh-dev` 台账与报告；产品上线后写 Runtime 投影。正式范围或验收修订走 CR 和新的基线锁。
