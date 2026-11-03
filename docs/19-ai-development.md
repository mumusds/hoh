# AI自主开发执行手册

你现在开发的是 HoH 本身，不是另一个示例产品。阅读顺序：CONTRACT（含 CR-001）→ 当前任务关联的需求（`contracts/requirements.json`，生成视图见 `docs/02-functional-spec.md`）→ 架构、数据 → 编排、算法 → 适配 → UX → 验收。按需读全文，不要一次把全部文档塞进上下文。

## 启动

在已选择的隔离 VM 中，以非 root 用户打开主开发会话（默认 Claude Code），粘贴 LAUNCH_PROMPT。该会话先执行 `bash scripts/bootstrap.sh --yolo`；之后每个新 Shell 先 `source .hoh-dev/env.sh`（也会加载可选的 `.env`），再运行包自测和 preflight。已有 CLI/模型配置保留；缺工具可以安装；缺真实认证这类只有用户能提供的条件，精确记录为阻塞。

## 自举编排（CR-001）

- **M0–M2：** 一个主开发会话（顶层，单写者）加独立 Review 会话。Review 通过 `python3 tools/agent_task.py --agent codex --role reviewer --prompt-file <渲染后的prompt>` 发起一个全新会话；Codex 不可用时用另一个全新 Claude 会话。Review 只读冻结候选、不改代码。
- **v0.1 发布：** QA 与 Review 必须是两个不同的独立会话，且都不同于开发会话（门禁会核对原生 attempt 记录中的 session_ids）。
- **M2 通过后：** 按产品默认团队接入 Codex 指挥与 Pi QA，并先用小任务与外部结果比对，不让未验证的 Runtime 自己签收自己。
- **防嵌套：** `agent_task.py` 给每个原生子进程设置 `HOH_BOOTSTRAP_PARENT`；带这个变量的会话不能再以 `--top-level` 或 `start.sh` 启动顶层会话。

## 执行节奏

```bash
python3 tools/taskboard.py list
python3 tools/taskboard.py next
python3 tools/taskboard.py claim M1-T01 --owner main-dev
# …TDD、实现、提交、冻结候选、独立 Review、修复…
python3 tools/taskboard.py set M1-T01 done --owner main-dev --note ".hoh-dev/evidence/… 与 review 报告路径"
```

一个任务的流程：引用 REQ/AC → 先写会失败的测试并确认失败原因 → 最小实现 → 局部检查 → 提交 → `candidate.py freeze` → `run_check.py` 运行计划内的检查 → 独立 Review → 修复 → 更新任务状态。依赖 outline 需求的任务会被 `claim` 拒绝：先完成该里程碑的 spec 任务（见 docs/08）。每个任务完成后继续下一个，不等用户确认下个里程碑，也不在 v0.1 停下。

每次会话结束或压缩前写 `.hoh-dev/PROGRESS.md`：当前任务、分支、commit、测试命令与结果、未解决的 issue、下一动作、外部阻塞。不要只留自由聊天记忆。RESUME_PROMPT 按事实重建现场，不能因为上次 Agent 说“完成”就相信。

## 自举限制

原生 CLI 可能因服务、token 或实现限制而终止；本包不假称一个提示词能强迫 Agent 无限持续。依靠底座原生的长任务和续会话能力，加上明确的 checkpoint；不允许用 while true 空转追加“继续”。

允许自主做小范围实现决策，并留下 ADR；不重复询问用户已采纳的架构与交互。实现细节未知时，先做源码研究或小实验。只有重大范围改变、身份认证和外部费用授权才需要升级给用户。

## 不能走的捷径

- 只交 MVP、v0.1 或架构文档就称完工。
- 用 mock 冒充三 Agent 集成；用 `--mode` 自报 live。现在 mode 只能来自冻结的检查计划。
- 删除 Web、插件、自定义角色或时间线。插件只能按 CR-001 带证据降级。
- 让开发者自己当唯一验收者；把 BUNDLE 自测当产品测试。
- 降低或跳过 required gate；编辑受锁文件而不写 CR。
- 捏造官方接口、执行结果、用量或模型名称。

## 提交与报告

小批提交，使用 feature branch，commit 关联任务与 REQ；有 remote 就走 PR/CI，没有就用本地候选。每个阶段按真实执行结果列出 PASS/FAIL/BLOCKED/NOT_RUN 与 mock/product/live。只有 release_gate 对 v1 profile 通过才能报告 COMPLETED（或 COMPLETED_DEGRADED）；否则如实报告 INCOMPLETE/BLOCKED 与恢复点。

## 子任务防串岗与基线状态

单次桥默认添加 `HOH_DELEGATED_TASK` 信封。所有原生 Agent 读取根 AGENTS 时，先按这个信封进入局部执行，不能因为项目总目标而自动接管整个工程。指定的 role 不是装饰标签：输出必须保留 task/attempt/role 身份，QA 与 Review 不得转成开发。

`contracts/` 下的需求、场景、policy 与 schema 受基线锁保护，不保存实现进度。实施进度写 `.hoh-dev` 台账与报告；产品上线后写 Runtime 投影。正式范围或验收修订走 CR 和新的基线锁。
