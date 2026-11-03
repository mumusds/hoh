---
name: hoh-domain-modeling
description: 领域与数据建模——引入或改变实体、状态、表、事件或API时，定义身份、生命周期、不变量、事务边界与失败例，并映射到Schema、迁移与契约测试。
---
# 领域与数据建模

## 何时使用
- 新增或修改实体、状态机、数据表、事件、HCP消息或API契约。
- 出现术语冲突（同名不同义或异名同义），或同一状态被多处写入。
- architecture之前；或Developer实现涉及持久化、并发与恢复时。

## 输入
- 相关REQ/AC与已采纳ADR。
- 现有Schema与迁移历史（如 `contracts/sql/`、`contracts/*.schema.json`）。
- 现有术语表；HoH运行不变量（`docs/14-resilience.md` 的I1–I8）。

## 步骤
1. 术语表：从REQ/AC抽取名词，逐个写定义、身份、所有者、易混词。HoH常见易混对：Task（全局）与Pi本地todo；Session（底座会话）与Attempt；done（任务完成）与accepted（证据验收）；ACK（已到达）与APPLIED（已生效）。
2. 实体身份：唯一键、ID格式、不可变字段（如候选的commit/tree/spec摘要）；禁止以PID、时间戳或显示名作身份。
3. 生命周期：列出状态、合法转换、触发者与终态；非法转换必须被拒；同一事件重复应用必须幂等。
4. 不变量：逐条写成可测试断言（例：每repo至多一个有效写租约；candidate变化即其当前验收失效；失败历史append-only）。
5. 事务边界：哪些写入必须同事务（state+event+outbox）；跨事务或外部效果如何对账（effect ledger）。
6. 并发与版本：expected_revision CAS、role_epoch与lease fencing；冲突时返回什么、谁来重试。
7. 失败例先行，每项写期望行为与测试名：
   - [ ] 重复消息/重复命令
   - [ ] 乱序到达
   - [ ] 版本冲突（expected_revision过期）
   - [ ] 并发写同一实体
   - [ ] 丢ACK，或ACK后崩溃
   - [ ] 两步之间崩溃（半提交）
   - [ ] 旧epoch的结果迟到
8. Schema/SQL变更：约束、索引、外键；迁移前后完整性检查；旧数据兼容与回滚策略。
9. 契约测试清单：每条不变量与失败例对应TEST ID。

## 产出物与格式
- `resources/templates/api-data.md`：实体身份与生命周期、数据表及约束、请求/响应Schema、幂等/并发/错误码、事件与重放、迁移与兼容、契约测试。
- 术语表与状态转换表（文本表或mermaid，须可评审）。
- TaskResult：evidence引用以上文档与Schema diff；issues列未决术语冲突与迁移风险。

## 完成判据
- 每个实体都有身份、生命周期与唯一所有者。
- 每条不变量与失败例都有期望行为与TEST ID。
- 迁移有前后校验与回滚说明；对现有契约没有未说明的破坏性变化。

## 常见错误
- 把UI展示状态当领域状态；状态由多个模块各自写入。
- 用多个布尔标志代替显式状态机，产生非法组合。
- 把ACK当完成，把进程存活当进展。
- 覆盖式更新抹掉失败历史。
- Schema只覆盖成功路径，没有冲突、重放与迁移规则。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
