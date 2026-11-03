# 变更记录

## 基线 HOH-BASELINE-1.1 revision 3 — 2026-11-03（CR-002）

项目所有者确认 CR-001 批准人、v0.1 范围（18 项需求）和 M1/M2 全部暂定选择（字段更名为 `decisions`）。复审后修正四处设计缺陷：
- 调度改为有界等待，防止饥饿（REQ-038）；
- 配置了费用上限但价格未知时 fail-closed（REQ-042）；
- SQLite 在启动时做 WAL 自检，不再只靠文件系统名单（REQ-062）；
- 按 Pi 1.5.0 官方文档剥离 CR（REQ-026）。

验收数从 161 增至 164。详见 `docs/cr/CR-002-confirm-m1-m2-decisions.md`。

## 开发启动包 1.1.0 — 2026-11-03（CR-001，基线 HOH-BASELINE-1.1）

项目所有者批准的决定：Pi 插件真实集成改为 degradable（COMPLETED_DEGRADED）；新增 v0.1 中间版本（INTERIM_RELEASED）；自举期 M0–M2 改为主开发会话 + 独立 Review 会话。详见 `docs/cr/CR-001-kit-hardening.md`。

- 规格：M1/M2 的 27 项需求细化为 detailed，共 161 条可判定 AC，附行为、失败模式和测试场景；M3–M7 标为 outline，由各里程碑 spec 任务细化。AC/TEST ID 改为 `AC-NNN-MM`/`TEST-NNN-MM`；`docs/02`、`docs/18` 改为由 `tools/gen_spec_views.py` 生成。
- 门禁：新增检查计划、live 会话、soak 事件和原生 attempt schema。run_check 只执行冻结计划中的 argv/mode/cwd。门禁强制 AC 与检查的绑定、证据目录归属、独立报告与原生 attempt 的绑定、soak 时间线来源，并支持 v1/v0.1 profile 和 DEGRADED。状态枚举已统一。
- 基线：锁定门禁工具、`docs/10` 和全部 schema；任务计划移出锁；新增 `tools/baseline.py`（CR 校验、relock）。
- 工具：pi 安装补 `--approve` 与 npm integrity，失败按插件降级；全部 Agent 拒绝 root；防止嵌套顶层会话；taskboard 增加 claim/owner/合法迁移/单写者/readiness/重开级联；校验器排除生成目录；候选清单排他写入；按整词匹配参数；`.env` 自动加载；CI 固定 action SHA，并加入 Python 3.14 与 shellcheck。
- 契约：SQL 增加 user_version、事务、NOT NULL、外键索引，以及团队绑定、预算、CR、发布、交接、监督触发器、Prompt 修订等表；OpenAPI 增加本地 token 与 Host/Origin 校验、列表端点、事件过滤、410 body 与幂等重放；协议增加 ApproveSpec/AnswerQuestion。
- 资源：6 个角色 Prompt 与 12 个 Skill 重写为角色专属的完整契约与流程。
- 独立复核：不参与开发的会话复核本次改动，发现 3 个高、4 个中等级问题（均已复现）以及若干低等级问题。已全部修复并补上负例测试，详见 `reports/REVIEW.md`。

## 开发启动包 1.0.0 — 2026-10-13

整理全部已采纳约束为可追踪基线；建立70项功能/非功能需求、AC和24个工作包；统一CLI最小面；补齐HCP、事务outbox、role_epoch、spec revision、候选证据、UI重放、恢复与版本升级设计；内置原创工程Skill和模板；提供YOLO开发环境初始化、原生单次Agent桥、任务板、包检查与发布门禁工具。

此版本是开发启动资料与工具，不是HoH产品1.0发布。产品功能初始NOT_IMPLEMENTED，产品验收初始NOT_RUN。本次真实执行结果只见reports/中的包级审核记录。

后续实现按日期、版本、REQ、commit、测试类型记录变更；不得把未执行的真实Agent/跨天测试写成已通过。
