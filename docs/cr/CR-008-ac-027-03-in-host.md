# CR-008 · AC-027-03 改归在 host 内运行适配器的任务

ID：CR-008
状态：PROPOSED
批准人：（待独立 Review）
日期：2026-11-07
基线：HOH-BASELINE-1.1 revision 8（不改受锁文件，不重新上锁）

## 背景

AC-027-03（TEST-027-03，检查 `integration`）要求：三个适配器各有一份 fixture，在若干正常帧之后跟一行 8 MiB、无 LF 的数据，**同一 host** 里另有一个正常 attempt 并行运行；结果是超大帧 attempt 为 FAILED，并行 attempt 为 SETTLED，**host 进程未退出**。

CR-004 F2 当时把它分给 M2-T05，理由是它需要"host 与并行 attempt"。M2-T05 交付了 host 内的执行循环（ADR-019 决定 8），但执行器是一个端口：在角色绑定到代理（M2-T07）之前，产品没有在 host 内运行适配器的执行器。所以 TEST-027-03 的"同 host"在 M2-T05 没有产品路径可测。第一轮独立审查 M2T05R-55 指出，M2-T05 只把它解读为"运行回合与 store 的进程"、只写在 ADR 里，这不够，属于需要 CR 的契约缺口。

## 决定

1. AC-027-03 从 M2-T05 改归 M2-T07，即首个在 host 内经角色绑定运行适配器的任务。这与 CR-004 F2 的做法相同：AC 分给了早于其所需能力的任务，就改归属。
   - M2-T05 的 requirements 去掉 REQ-027，acceptance 去掉 AC-027-03，done_when 去掉 AC-027-03 一句。
   - M2-T07：
     - depends_on 加上 M2-T05；
     - requirements 加上 REQ-027；
     - acceptance 列为 AC-018-01..05 加上 AC-027-03（TEST-027-03 的 `integration` 检查仍归 M0-T03 所有，一个检查只有一个所有者）；
     - done_when 加上：执行循环经角色绑定，用产品执行器运行三个适配器，适配器进程经启动门启动（ADR-019 决定 8 的设计修正）；TEST-027-03 在 host 内重测。
2. M2-T05 已有的三个适配器层测试保留，作为补充证据：两个回合与 store 在同一进程，超大帧只让本 attempt 失败。它们改名，不再带 TEST id，因此 TEST-027-03 只能由 M2-T07 在 host 内的测试满足，发布门禁不会把适配器层的 PASS 当作 AC-027-03 的证据。

## 影响

- AC-027-03 的文字、场景与检查不变，没有删除或降级任何 AC。门禁更严：适配器层证据不再计入 AC-027-03。
- 不改 CONTRACT 的架构与交互决定。
- 适配器在 spawn 之后才写 RUNNING 的窗口（M2T05R-52 的余项），由 M2-T07 让适配器改走启动门来关闭。

## 不变内容

REQ-027 与 AC-027-03 的文本、TEST-027-03 的场景、`integration` 检查、release policy 与基线锁。

## 验证

- `python3 tools/gen_spec_views.py` 重新生成视图，`bash scripts/check.sh --bundle` rc=0。
- `python3 tools/taskboard.py list` 显示两个任务的新定义。
- integration 检查的 test-results.jsonl 中不再出现 TEST-027-03（M2-T05 候选）。
