# 示例不是实际报告

JSON用于Schema、文档与测试样例；hash/ID都是显式示意，不是运行证据。不要将examples中的数据复制到最终release manifest冒充实际结果。生产实现需从真实run产生记录。

`check-plan.json` 是检查计划的结构示例（`contracts/check-plan.schema.json`）：M0-T03 把它复制为 `tests/check-plan.json`，并把每个命令替换为真实存在的命令；不存在的脚本必须让检查失败，不能换成 echo。
