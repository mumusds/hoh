# CR-011 · Attempt 的进程以 cgroup 证明终结

ID：CR-011
状态：APPROVED
批准人：项目所有者（2026-11-08 在主开发会话中选定方案 A；决策简报 `.hoh-dev/evidence/m2t06-prep/decision-cgroup.md`）
日期：2026-11-08
基线：HOH-BASELINE-1.1，修改受锁文件 `contracts/requirements.json`（REQ-043 决定 3），重新上锁为 revision 10

## 背景

REQ-043 决定 3 原文：「以 setsid 脱离进程组的后代不受 pgid 信号覆盖；本选择额外按 /proc 父子链终止该 Attempt 后代，需实现时确认可行。」

M2-T06 按此实现了组外进程的终结证明。这个证明用于四条会结束 Attempt 或释放写者租约的路径：

- 取消；
- 执行器报告 leader 结束；
- 租约到期的 settle；
- host 启动时的 reconcile。

它保障 CONTRACT 第 7 条的单一主写者。独立审查连续四轮都找到能让证明误判「已终结」的 critical 竞态，ADR-021 决定 8d 的第 2、3、4 版各被下一轮击穿：

- M2T06R-01：扫描之后才脱离进程组的后代；
- M2T06R-100、-101：其余结束路径只看进程组；进程名不是 UTF-8 的进程读不出来；
- M2T06R-200：在列出与读取环境之间 fork 后退出的进程；
- M2T06R-300、-301：最后一次读 `ns_last_pid` 之后的 fork；主线程退出而写线程仍带标记。

`/proc` 不是原子快照，只靠它不能证明一个可能躲避的进程树已经结束。可行性的结论是：只按 `/proc` 不可行。

本机实测：在 `systemd-run --user --scope -p Delegate=yes` 内，可以为每个 Attempt 建子 cgroup，并把不断 fork、退出的进程树移入其中。之后 `cgroup.kill` 一次杀尽，`cgroup.events` 的 `populated 0` 给出由内核维护的空证明。

不受委托的 cgroup（由登录会话直接拉起的 host 所在的 session scope 属 root）不能建子 cgroup。本平台禁止非特权 user/PID namespace（`kernel.apparmor_restrict_unprivileged_userns=1`）。

## 决定

REQ-043 决定 3 改为：

> 以 setsid 脱离进程组的后代不受 pgid 信号覆盖。host 运行在受委托的 cgroup v2 中时（systemd --user 的 unit 带 `Delegate=yes`，或客户端经 `systemd-run --user --scope -p Delegate=yes` 拉起 host），每个 Attempt 的进程在其启动门打开前移入它专属的子 cgroup：以 `cgroup.kill` 停止，以 `populated 0` 证明终结。没有受委托的 cgroup 时，退回按 /proc 父子链与继承的环境标记查找的尽力方案，并把该 Attempt 明确记为降级（`tracking: proc`），由 `hoh doctor` 报告；该方案对蓄意躲避的进程不成立。

## 影响

- `contracts/requirements.json`：REQ-043 的 `decisions[2]`。重新生成 `docs/02`、`docs/18`，重新上锁。
- ADR-021：
  - 决定 2（客户端拉起 host 时优先经 `systemd-run --user --scope -p Delegate=yes`，不可用时直接拉起）；
  - 决定 8d（第 5 版：cgroup 为证明；/proc 为降级）；
  - 决定 11（cgroup 路径的测试在受委托 scope 内运行）。
- M2-T14 的 systemd unit 带 `Delegate=yes`。
- CI 需要用户级 systemd 管理器，才能覆盖 cgroup 路径（开启 linger）。

## 不变内容

- REQ-043 的行为、失败模式与全部 AC 不变。
- 决定 1、2 不变，没有放宽任何门禁或证据规则。
- 没有受委托 cgroup 的环境照样能运行，只是降级被记录且可见，不悄悄当作已证明。

## 验证方式

- 受委托 scope 内的测试：
  - 一个不断 fork、退出的组外写进程，在取消、执行器报告结束、租约到期 settle、启动 reconcile 四条路径上，都在 Attempt 结束之前被 `cgroup.kill` 停止；
  - 证明来自 `populated 0`。
- 不受委托时，每个 Attempt 记有 `tracking: proc`。
- `hoh doctor` 报告是否可得受委托的 cgroup。
