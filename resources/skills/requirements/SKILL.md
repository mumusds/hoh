---
name: hoh-requirements
description: 需求澄清——新目标、Brownfield增量或需求变更时，把意图整理为带稳定ID的REQ与可观察AC；interactive做真实问答，headless用假设台账并在关键未知处阻塞。
---
# 需求澄清

## 何时使用
- 新目标进入Plan，或Greenfield/Brownfield项目需要首版或增量规格。
- 用户提出需求变更（配合change-request模板），或独立规格评审指出AC不可观察。
- 不用于重新采访已采纳的决定：CONTRACT或已批准spec中的选择直接作为约束。

## 输入
- goal原文与用户提供的资料；当前spec revision、需求主表、已采纳ADR。
- interaction mode（interactive / headless）；headless时的既有授权默认（项目配置、CONTRACT、用户先前明示的规则）。
- Brownfield：现有行为的真实观察（运行记录或源码阅读），不凭印象。
- 规格profile（`resources/templates/profiles.json` 的lite/standard/full）与预算。

## 步骤
1. 盘点已定事实：列出已采纳决定及出处，它们不进入提问清单。
2. 提取产品轮廓：目标用户、使用场景、问题与价值、范围、非目标、约束、成功指标及测量方式。
3. 选定profile，确定要产出的模板文档。
4. 建立歧义清单，每项分类为“资料可答 / 研究可关闭 / 必须真人决定”，并标critical（影响产品方向、范围、外部费用、认证、不可逆数据）或normal。互相依赖的项多于3个时转decision-map。
5. 按交互模式解决歧义（见下节）。
6. 写REQ：稳定ID、优先级、用户可观察的行为；实现方案放进设计文档，不写进REQ。
7. 写AC：每条Given/When/Then可由外部观察判定，覆盖成功、失败、边界、取消/重试/恢复；每条映射TEST ID与验证方式（单元/集成/PTY/Web/live）。HoH ID格式为AC-NNN-MM/TEST-NNN-MM；把outline需求细化为detailed时，每项写2–5条可判定AC，并附行为描述、失败模式与测试场景（场景引用对应check_id）。release_class与release_targets只按用户决定或CR填写，不自行调整。
8. 非功能需求给阈值与参考环境（例：“120列终端首屏渲染<1s，Ubuntu 24.04 VM”）。
9. 自检（全部满足才提交）：
   - [ ] 每个REQ至少1个AC，每个AC有唯一TEST ID
   - [ ] AC中没有“友好/快速/合理”等不可测词
   - [ ] 非目标已写明；删除既有required范围有用户明示批准的记录
   - [ ] 已采纳决定未被改动，冲突已记为issue
   - [ ] 所有假设已入账，没有critical未知被“假设掉”
10. 输出规格diff与开放问题，请求独立规格评审。

## 交互模式
- interactive：每轮只问当前阻塞的少量问题（建议≤5），每题附选项、推荐默认与影响；原样记录用户回答（时间、原话）为decision。沉默或含糊不等于同意，应复问或保持open。
- headless：不向用户提问，也不写“用户已确认”。只使用既有授权默认，逐条写入假设台账；critical未知不做假设——相关范围BLOCKED，给出精确问题与恢复动作，不依赖它的部分可继续。
- 假设台账字段：ID｜假设内容｜依据（授权默认/现有文档/行业惯例）｜可逆性｜影响REQ/AC｜置信度｜需谁确认｜状态（open/confirmed/rejected）。

## Brownfield与变更
- Brownfield：先记录现状行为与证据，再写“现状→目标”的差异，不把现状缺陷误写成需求。
- 变更：使用 `resources/templates/change-request.md`，写明旧/新spec revision、受影响REQ/AC、影响闭包与需重验的证据。

## 产出物与格式
- 规格文档：`resources/templates/` 中的product-definition、prd、feature（Given/When/Then+TEST ID）、acceptance；变更用change-request。
- 假设台账与开放问题清单（独立工件）。
- TaskResult：evidence引用以上文件的path+sha256；issues每项含kind（open_question/assumption）、blocking、decision_owner；summary写新spec revision与REQ/AC增删。

## 完成判据
- 范围内REQ都有可观察AC与TEST映射，自检清单全部满足。
- 没有未处理的critical未知；若有，状态为BLOCKED而非COMPLETED。
- spec revision与假设台账已保存并被TaskResult引用。

## 常见错误
- 把技术方案（“用SQLite表X存储”）写成用户需求。
- headless下自问自答并标记为用户批准。
- 一次抛出几十个问题，或反复询问已采纳决定。
- AC只写“功能正常”，无法判定PASS/FAIL。
- 为缩小工作量而静默删除需求。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
