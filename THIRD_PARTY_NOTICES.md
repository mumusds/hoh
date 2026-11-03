# 来源、许可与再分发

本包的产品规格、工具代码、Prompt、Skill和模板为本次整理的原创实现，按根目录MIT许可使用。没有将第三方项目完整代码或Skill全文换名打包。

HoH理论来源为Harness-of-Harness论文及官方项目；HoH-lite自身子目录许可与仓库根许可不同，复用其代码时必须保留对应Apache-2.0条款/NOTICE，不能只看根LICENSE。该说明为工程来源记录，发布前仍需核对实际取用版本。

Codex、Claude Code、Pi及其模型服务不由本包再分发；安装与使用遵循各自条款。Grok Build、DeepSeek Harness、Pi Open TUI仅作交互和架构研究参考。Matt Pocock Skills作为方法参考，内置的是HoH原创requirements/decision-map等Skill，不声称原版Wayfinder已经无人值守化。

Pi三个扩展由安装脚本从其包源安装固定版本，保留各自许可证，兼容性需真实测试；npm依赖、Rust crate、Web资源由正式产品生成SBOM及第三方notice。不得把模型服务或上游项目Logo、商标、字体文件纳入本包。

完整原始资料及核验边界见research/SOURCES.md。文中项目名用于识别研究对象，不代表官方背书。正式发布仍需检查HoH名称、crate和可执行文件的重名情况。
