# CR-013 · 终结证明的余项：篡改 host 本身、躲避尽力扫描

ID：CR-013
状态：APPROVED
批准人：项目所有者（2026-11-09，选择「接受并写明」）
日期：2026-11-09
基线：HOH-BASELINE-1.1，修改受锁文件 `contracts/requirements.json`（REQ-043 决定 3），重新上锁为 revision 12

## 背景

CR-012 由项目所有者批准时，REQ-043 决定 3 写明的余项只有两类：

- 既迁出 cgroup、又脱离进程组并让标记不可见的进程；
- 交给 Attempt 之外早已运行的同用户服务的工作。

M2-T06 第 6 至 11 轮审查又发现两类问题，原文都没有写：

- **篡改 host 本身（M2T06R-900、-950、-1051）**：这类布置都要求同一用户的进程先改动 host 自己。例如：
  - 把 host 重新启动进它持有 CAP_SYS_ADMIN 的 user 与 mount 命名空间，在那里伪造 `/proc/self/ns`，覆盖 `/proc/<pid>` 或 cgroup 目录中的文件；
  - 直接改写 `.hoh/state.db`。

  实现中的命名空间记录与 cgroup 逐级 inode 核对只能识别意外的环境变化。没有特权隔离（另一个系统用户、完整沙箱），挡不住刻意的伪造。
- **躲避尽力扫描（M2T06R-554）**：迁出 cgroup 与进程组、却保留标记的进程，只由 /proc 的尽力扫描覆盖。扫描的已知竞态同样适用于它，有意利用竞态躲开扫描的进程同样无法归属。

项目所有者于 2026-11-09 决定把这两类写入已接受的余项（M2-T06 第 11 轮审查期间所问）。

## 决定

REQ-043 决定 3 的余项一句改为：

> 余项（项目所有者2026-11-08、2026-11-09接受）：Attempt的进程与用户同一uid，没有特权时无法封住。以下情形都无法归属，可能在Attempt结束、写者租约释放后继续写：
>
> - 迁出cgroup、又脱离进程组的进程，它只由尽力的/proc扫描覆盖，让标记不可见或利用扫描的已知竞态即可躲开；
> - 交给Attempt之外早已运行的同用户服务（systemd --user、HoH之外启动的tmux server、cron、ssh localhost等）的工作；
> - 同用户进程篡改host本身，例如改写它的数据库与文件，把它放进自己持有CAP_SYS_ADMIN的命名空间重新启动，或在那里伪造/proc与cgroup的视图。
>
> 命名空间记录与cgroup目录核对只识别意外的环境变化。hoh doctor写明这一界限（CR-011、CR-012、CR-013）。

## 影响

- `contracts/requirements.json`：REQ-043 的 `decisions[2]`。重新生成 `docs/02`、`docs/18`，重新上锁。
- ADR-021 决定 8d 第 7 版：已识别与按界限排除的情形分开写。
- `docs/11`、`docs/12`：取消一段与后台运行一段。
- `hoh doctor` 的 `process_tracking_limits`。

## 不变的部分

- 不删除任何需求或 AC，不改 required/degradable，不缩小 release_targets。
- 不放宽门禁。证明的条件不变；无法证明时不结束 Attempt、不释放租约的约束不变。
- 余项写明的只是原本就存在的界限，由项目所有者接受。

## 验证方式

- `hoh doctor` 的 JSON 与文本形式都写出三类余项。
- ADR-021、docs/11、docs/12 与 REQ-043 决定 3 一致。
