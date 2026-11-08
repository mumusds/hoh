# CR-012 · cgroup 终结证明的补充：完整条件、余项与 linger

ID：CR-012
状态：APPROVED
批准人：项目所有者（2026-11-08，选择「接受并写明」：余项写入 REQ-043 与用户文档，由 `hoh doctor` 提示）
日期：2026-11-08
基线：HOH-BASELINE-1.1，修改受锁文件 `contracts/requirements.json`（REQ-043 决定 3），重新上锁为 revision 11

## 背景

CR-011（项目所有者批准）把 REQ-043 决定 3 改为：host 运行在受委托的 cgroup v2 中时，以 `cgroup.kill` 停止 Attempt 的进程，以 `populated 0` 证明终结；没有受委托的 cgroup 时，按 /proc 尽力查找并记为降级。

M2-T06 第 5 轮独立审查确认了两件事，原文都没有写：

- **M2T06R-400、-450**：受委托的子树对本用户可写，同一用户的进程可以自行离开 Attempt 的 cgroup。它可以把自己的 pid 写入别的 cgroup 的 `cgroup.procs`，也可以请用户级 systemd 另建 scope（例如 tmux 3.6 把 pane 移入自己的 scope）。只看 `populated 0`，会在写进程存活时结束 Attempt、释放写者租约。
  - 实现（ADR-021 决定 8d 第 6 版）的终结证明同时要求：进程组为空；cgroup 为 `populated 0`；按 /proc 父链与继承的环境标记找到并以 pidfd 固定的进程都已结束；进程组为空之后连续两次完整扫描都没有找到组外进程。找到的进程同样收到停止信号。
- **M2T06R-453**：客户端经 `systemd-run --user --scope` 拉起的 host 属于用户级 systemd 管理器。用户没有开启 linger 时，最后一个会话结束后管理器会停止，host 随之结束，违反 REQ-005 行为 2「SSH 断开不影响已接受任务」。
  - 实现（ADR-021 决定 2）只在已开启 linger 时经 scope 拉起；否则直接拉起，按降级记录。

此外，Attempt 的进程与用户同一 uid。它可以把工作交给 Attempt 之外早已运行的同用户服务（systemd --user 的 `systemd-run`、HoH 之外启动的 tmux server、cron、`ssh localhost`）。这样的工作不在 Attempt 的进程树里，cgroup、进程组和标记都覆盖不到。PID namespace（本机 bwrap 可用）同样封不住这些通道。只有隔离 IPC、文件系统和凭据的完整沙箱，或另一个系统用户，才能封住。

于是 REQ-043 决定 3 的原文与实现及其真实界限不一致：原文把 `populated 0` 写成完整的证明，没有写其余条件、余项与 linger 条件。

## 审查与批准

- CR-003 D1 独立审查（Codex，FAIL；尝试记录 sha256 `033f6397…`，stdout sha256 `669b04ed…`，全文见 `reports/reviews/M2.md` M2-T06 第 5 轮之后的 CR-012 一节）：
  - **CR12-01 high**：接受 cgroup 方式下的余项超出 CR-011 的授权，D1 条件 1、4 不成立，须所有者决定。处置：项目所有者于 2026-11-08 决定接受并写明（本 CR 改由所有者批准）。
  - **CR12-02 medium**：证明条件漏写「进程组为空」与完整扫描，余项因而写得过宽。处置：下文决定写全四项条件；余项限定为这些归属信息都覆盖不到的情形。
  - **CR12-03 medium**：「doctor 说明未开启 linger 的原因」只对没有 host 的预检查成立，运行中的 host 只记了一般原因。处置：客户端把直接拉起的原因交给 host（环境变量 `HOH_HOST_UNSCOPED`），host 写入 `host.json`，`hoh doctor` 报告运行中 host 的这一原因；测试在 host 运行时、以预检查会通过的用户运行 doctor。
- 改正后的全文由 M2-T06 第 6 轮独立审查一并复核。

## 决定

REQ-043 决定 3 改为：

> 以 setsid 脱离进程组的后代不受 pgid 信号覆盖。host 运行在受委托的 cgroup v2 中时（systemd --user 的 unit 带 Delegate=yes，或客户端在用户已开启 linger 时经 systemd-run --user --scope -p Delegate=yes 拉起 host），每个 Attempt 的进程在其启动门打开前移入它专属的子 cgroup，以 cgroup.kill 停止。终结的证明须同时满足：进程组为空；该 cgroup 为 populated 0；按 /proc 父链与继承的环境标记找到并以 pidfd 固定的该 Attempt 进程都已结束；进程组为空之后，连续两次完整扫描都没有找到进程组与 cgroup 之外的该 Attempt 进程（不完整的扫描、找到过进程的扫描都不计入）。找到的进程同样收到停止信号。没有受委托的 cgroup 时，退回按 /proc 父子链与继承的环境标记查找的尽力方案（没有 cgroup 一项，其余条件相同），把该 Attempt 明确记为降级（tracking: proc），由 hoh doctor 报告；该方案对蓄意躲避的进程不成立。无法得到证明时，不结束 Attempt，不释放写者租约。余项（项目所有者 2026-11-08 接受）：Attempt 的进程与用户同一 uid，没有特权时无法封住。既迁出 cgroup、又脱离进程组并让标记不可见的进程，以及交给 Attempt 之外早已运行的同用户服务（systemd --user、HoH 之外启动的 tmux server、cron、ssh localhost 等）的工作，都无法归属，可能在 Attempt 结束、写者租约释放后继续写；hoh doctor 写明这一界限（CR-011、CR-012）。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 不放宽门禁：证明的条件比 CR-011 的原文更严（多了进程组、固定进程与两次完整扫描）。无法证明时不结束、不释放的约束不变。
- CONTRACT 第 7 条的单一主写者与第 13 条不变；余项是所有者接受并写明的界限。linger 条件正是为了保持 REQ-005 行为 2。

## 影响

- `contracts/requirements.json`：REQ-043 的 `decisions[2]`。重新生成 `docs/02`、`docs/18`，重新上锁。
- 代码：客户端把直接拉起的原因交给 host，host 写入 `host.json`；`hoh doctor` 的 `runtime.host` 增加 `process_tracking_limits`，文本形式列出 host 状态、跟踪方式和这一界限。
- 文档：ADR-021 决定 2、8d，ADR-015 决定 7，docs/11「后台运行」，docs/12 的取消一段。

## 验证方式

- 受委托 scope 内的测试：
  - 自行迁出 Attempt cgroup 而保留标记的写进程，在取消与执行器报告结束两条路径上都被找到并停止，Attempt 才结束；
  - 留在进程组中、已迁出 cgroup 且没有标记的无 leader 成员，使 Run 保持 CANCEL_REQUESTED。
- 用户没有开启 linger 时，host 直接拉起并记为 proc，`host.json` 写明原因；host 运行时，以预检查会通过的用户运行 `hoh doctor`，报告的仍是运行中 host 的这一原因。
- `systemd-run` 拒绝时，`host.json` 写明是它没有拉起。
- `hoh doctor` 的 JSON 与文本形式，在有无 host 时都写明这一界限。
