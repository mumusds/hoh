# CR-010 · AC-005-01 改归 CLI 端到端任务

ID：CR-010
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权（独立 Review 会话 01a1b768-9926-7151-a8a0-55bb15a51b66 判定 PASS，四项 D1 条件均满足；CR10-01 已修正）
日期：2026-11-07
基线：HOH-BASELINE-1.1，不改受锁文件，不重新上锁（审查时为 revision 8；随 M2-T06 分支落地时为 revision 9，即 CR-009 之后。第一轮审查 CR10-01）

## 背景

AC-005-01（TEST-005-01，检查 `tui-pty`）的前提是"无 host 运行；FakeAgent 使每个 Attempt 运行 20 s 并记录每次 start 调用"。host 由 `hoh -p '完成fixture目标' --detach --output json` 自己拉起。通过条件包括：

- HUP 后 60 s 内 events 的最大 seq 大于 S0；
- attach 前后，每个 Task 的 Attempt 行数与 FakeAgent 的 start 次数完全相同。

所以，由产品 CLI 拉起的产品 host 必须为 `-p` 的目标建出 Task，并经 FakeAgent 运行 Attempt。M2-T06 时这条路径不存在：

- 产品 host 没有执行器。在 M2-T07 把代理绑定到角色之前，`HostConfig::new` 的 runner 为 None（`crates/hoh-app/src/host/mod.rs`）。
- StartRun 之后没有任何东西生成 Task。Task 由 Commander 的决策产生，监督触发器在 M2-T08。
- 测试也不能给 CLI 拉起的 host 换上测试目标里的 FakeAgent 执行器：产品入口没有能换执行器的参数、环境变量或配置（CR-006 F2 对故障注入钩子作了同样的规定，但它管的只是钩子）。经正常适配器运行外部 fixture 程序的路径，要到 M2-T07 才有。

因此 TEST-005-01 在 M2-T06 无法按原文测试。M2-T13（CLI/TUI 端到端）依赖 M2-T06、M2-T08、M2-T16 与 M2-T19，是第一个 fixture 写着"host 已以 FakeAgent 配置运行"的任务（AC-001-01、AC-002-01），在那里可以按原文执行。

## 决定

1. AC-005-01 从 M2-T06 改归 M2-T13，做法与 CR-004 F2、CR-008 相同：AC 分给了早于其所需能力的任务，就改归属。
   - M2-T06：
     - acceptance 去掉 AC-005-01；
     - done_when 改为"AC-005-02/03/05 与 REQ-043 全部 AC 通过；关闭客户端不中断已接受任务。需要 host 内经 FakeAgent 运行 Attempt 的 AC-005-01 归 M2-T13（CR-010），systemd 崩溃重启的 AC-005-04 归 M2-T14。"
   - M2-T13：
     - requirements 加上 REQ-005；
     - acceptance 加上 AC-005-01；
     - done_when 加上"AC-005-01 通过（CR-010：由 `hoh -p --detach` 拉起的 host 经 FakeAgent 运行）"。
   - `tui-pty` 检查的所有者不变（一个检查只有一个所有者）。
2. M2-T06 仍按 REQ-005 行为 1、2、7 实现 detach 与独立 session，并用不带 TEST id 的补充测试验证能验证的部分。这些测试不计为 AC-005-01 的证据：它们的名字里没有 `test_005_01`，不会生成 TEST-005-01 的记录，而发布门禁按 AC、TEST、检查与候选核对证据；正式的 TEST-005-01 由 M2-T13 实现。具体为：
   - `hoh -p --detach` 在 StartRun 提交之后输出单个 JSON 对象，并以 0 退出；
   - HUP 并关闭 PTY 之后，host 仍存活，tty 为 `?`，sid 不同于 PTY 内 shell 的 sid；
   - attach 不新增 Run、Task 或 Attempt。

## 影响

- AC-005-01 的文本、TEST-005-01 的场景与检查都不变，没有删除或降级任何需求或 AC。它仍是 v1 与 v0.1 的必需项，release policy 不变。
- 不改 CONTRACT 的架构与交互决定。

## 不变内容

REQ-005 与 AC-005-01 的文本、TEST-005-01 的场景、`tui-pty` 检查、release policy 与基线锁。

## 验证

- `python3 tools/gen_spec_views.py` 重新生成视图，`bash scripts/check.sh --bundle` rc=0。
- `python3 tools/taskboard.py list` 显示两个任务的新定义。
