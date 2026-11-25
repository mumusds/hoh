# CR-037 · 离线模板审计允许可关联的内核 SOCK_DIAG 连接

ID：CR-037
状态：APPROVED
批准人：项目所有者（2026-11-25，在聊天中答复 M3T08-S6-001，选择选项 1：窄范围 CR）
日期：2026-11-25
关联：M3-T08 S6、REQ-029（AC-029-03、TEST-029-03）、ADR-040

## 问题

AC-029-03 原文要求两个 strace 的全部 connect 目标只有 AF_UNIX 或回环地址。
宿主的批准来源识别 `host/origin.rs::unix_peer_inode` 使用
AF_NETLINK / NETLINK_SOCK_DIAG socket（本次修改前类型为 SOCK_DGRAM|SOCK_CLOEXEC），随后 connect 到
`sa_family=AF_NETLINK, nl_pid=0` 的内核端点，查询 UNIX_DIAG_PEER。
客户端可能把已连接的描述符交给别的进程，SO_PEERCRED 本身不能证明当前持有者；
该查询是既有来源核对所需的本机内核通信，没有网络流量离开机器。
原白名单把这项本机查询与外部网络连接一并拒绝，阻塞安装版离线实例化检查。

## 决定

1. 仅扩展 AC-029-03 的 then：在 AF_UNIX 和回环之外，允许目标
   `sa_family=AF_NETLINK` 且 `nl_pid=0` 的 connect，但其描述符必须由同一进程此前的
   `socket(AF_NETLINK, SOCK_RAW|..., NETLINK_SOCK_DIAG)` 创建。
2. 两个 strace 同时记录 socket、close 与 connect，宿主继续记录 execve。
   审计按日志中的 pid 和 fd 关联；以该 pid 中 connect 之前最近一次返回该 fd 的
   socket 为准。close 后旧关联失效；fd 复用必须核对新 socket，不能沿用旧协议。
3. 其他 netlink 协议、非零 nl_pid、没有可关联 socket 的 fd、无法解码的记录，
   以及原有非本地连接仍失败。不同 pid 的相同 fd 不构成关联；失败的 socket
   不创建关联。审计不得仅凭 AF_NETLINK 地址就放行。
4. 实现将来源查询的 socket 类型对齐为 SOCK_RAW，保留 SOCK_CLOEXEC、SOCK_DIAG、
   nl_pid=0、查询报文及全部来源核对。此项不扩展已批准的白名单：本机 netlink(7)
   明确说明 RAW 与 DGRAM 都有效，netlink 协议不区分这两种类型；本机独立 syscall
   探针也确认两者连接同一内核端点成功。回归仍须核对真实 TCP/UDS 来源与连接生命周期。

## 否决的方案

- 削弱 `origin.rs` 的批准来源核对，或只依赖 SO_PEERCRED：会丢失描述符交接后
  的当前连接归属证据，本次保留该核对路径及其断言。
- 把 connect 替换成 sendto 以躲开审计：只是改变审计可见性，没有解决规格与
  实际本机通信之间的矛盾，因此不采用。

## 影响与边界

受锁内容只修改 `contracts/requirements.json` 中 AC-029-03 的 then，并用
`tools/gen_spec_views.py` 更新生成视图、用本 CR 正式重锁
`contracts/baseline-lock.json`。这是所有者明确批准的白名单窄范围扩展，
不冒称无需所有者批准的纯实现修复。其余 AC、检查计划、release-policy、发布范围、
门禁阈值与批准来源识别不变；不启动真实 Agent。

## 验证方式

执行 `python3 tools/gen_spec_views.py`、
`python3 tools/baseline.py relock --cr docs/cr/CR-037-netlink-sock-diag-connect.md`
和 `bash scripts/check.sh --bundle`。
审计单元测试先红后绿，覆盖可关联的内核 SOCK_DIAG、close 后 fd 复用、不同 pid、
其他协议、非零 nl_pid、缺失关联、非回环 INET 和无法解码的记录，保留原有拒绝项。
最后前台执行完整 `bash scripts/dev/pre-pr.sh`，包括运行 TEST-029-03 的部署检查。
每次命令、退出码与原始输出记录在 `.hoh-dev/self-tests/m3t08s6/`；自测，非验收。
