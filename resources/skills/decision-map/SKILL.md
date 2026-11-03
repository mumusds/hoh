---
name: hoh-decision-map
description: 跨会话决策地图——存在多项相互依赖的未知时，把它们组织为持久的决策DAG，逐项研究、实验或交真人决定并落ADR；headless只用既有授权，绝不假扮用户。
---
# 跨会话决策地图

## 何时使用
- requirements或architecture产生多项相互依赖的开放问题，一次会话无法关闭。
- 规格就绪前需要明确“哪些已定、哪些待定、由谁来定”。
- 会话恢复或角色交接后需要重建决策进度（不依赖聊天记忆）。

## 输入
- 开放问题清单与假设台账；已有ADR与来源清单（research/SOURCES.md）。
- interaction mode与既有授权默认；预算与截止。
- 已有的决策地图文件（若存在）。

## 决策项格式
地图只做索引，正文写在ADR或研究记录中。每项字段：
| 字段 | 说明 |
|---|---|
| id | DEC-xxx，稳定、不复用 |
| question | 一句话问题 |
| type | research（资料可关闭）/ experiment（原型可关闭）/ human（必须真人） |
| depends_on | 前置决策id列表 |
| status | open / in_progress / proposed / decided / blocked / superseded |
| blocks | 受影响的REQ/AC/任务 |
| reversibility | 可逆 / 代价高 / 不可逆 |
| owner | user / cto / commander / researcher |
| ref | ADR或研究记录的path+sha256 |

## 步骤
1. 载入：读取地图文件并核对每个ref的hash；不一致的项退回proposed并记录原因。
2. 建项：为每个未知建项，填type与depends_on；检查无环（有环则拆分或合并问题）。
3. 选前沿：取依赖均已decided的open项，按“解阻数量×不可逆程度”优先。
4. 关闭一项：
   - research：派Researcher或查官方文档/固定版本源码；结论按事实/推断/未知标注。
   - experiment：在临时目录做最小原型，记录命令、版本与输出。
   - human，interactive：向用户给出选项、推荐与影响，记录原话与时间。
   - human，headless：仅当既有授权默认明确覆盖时，按假设决定并写入假设台账（注明可逆性）；否则该项blocked，写出精确问题，继续处理其他独立项。
5. 写ADR（`resources/templates/adr.md`：状态和背景、决定、比较与拒绝的方案、理由和后果、验证证据、重新考虑条件），地图项更新为decided并引用ADR。
6. 关闭过程中暴露的新问题作为新项加入（记录parent），不在当前项里无限扩展。
7. 收敛：同一项最多2轮意见+1次决策（MOA默认上限）；没有新证据即选择可回滚方案，或标blocked。
8. 保存地图，输出当前前沿、blocked项与下一步。

## 交互模式要点
- interactive中真人选择必须真实发生；Agent之间的一致意见不能代替用户批准。
- headless下“未被授权覆盖的human项”只能blocked，不得写成decided。

## 产出物与格式
- 决策地图文件（上表字段；Markdown表或JSON均可，但须稳定可解析），在TaskResult.evidence中引用。
- 每个decided项对应一份ADR；human项附用户原话或假设台账条目。
- TaskResult：summary列出本次decided/blocked数量与当前前沿；issues列blocked项（精确问题、decision_owner、受阻REQ/任务）。

## 完成判据
- 当前readiness所需的决策项全部decided，或明确blocked并附恢复动作。
- 地图无环，每个decided项都有ADR与证据引用。
- 没有human项是由Agent自问自答关闭的。

## 常见错误
- 在地图里复制ADR正文，导致两处漂移。
- 把无限调查当进展；同一问题反复研究却没有新证据。
- headless下替用户选择并标记为“已批准”。
- 只依赖会话记忆，交接后地图丢失。
- 声称原样执行上游Wayfinder：本技能为HoH原创流程，不宣称原样执行Wayfinder。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
