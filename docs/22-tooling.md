# 开发工具与环境脚本

本目录脚本已作为开发包实现；目标Rust产品仍须开发。Python是开发辅助语言，不承担正式HoH循环。

## 环境准备与锁版本

`bootstrap.sh --yolo`按需安装系统构建依赖、Node（满足Pi要求）、Rust、jsonschema开发venv和缺少的原生Agent；已有安装/认证/模型默认值保留。只解析一次新安装版本并记录`.hoh-dev/*version.lock`，不能在正在执行的Run里自动追最新版。Rust产品由Cargo.lock与rust-toolchain、Web由package-lock进一步锁定。Node包下载核对官方SHA256。

脚本默认尝试三个项目级Pi扩展；安装后兼容性仍为NOT_TESTED。AI必须读取所安装版本的配置schema，明确Smart Compact可选记忆关闭方式、Hermes项目作用域及RPC扩展UI响应。无法证实的配置不能凭空生成。第三方发布变更可能使安装失败，preflight精确报告并按源码修复。

`--skip-install`不访问发行源，但会探测本机版本；返回3说明能力缺失。`--dry-run`无写入、无下载、无模型调用。bootstrap子Shell不会改变父Shell环境，**以后每次新Shell先source `.hoh-dev/env.sh`**。脚本不能提供认证，`preflight --live`需显式选择，可能产生模型费用。

## 原生单次任务桥

先将`resources/prompts/{role}.md`与项目/任务/证据JSON通过`tools/render_prompt.py`渲染成完整Prompt。然后例如：

```bash
source .hoh-dev/env.sh
python3 tools/render_prompt.py --role developer --context examples/prompt-context.json --output .hoh-dev/developer-prompt.md
python3 tools/agent_task.py --agent claude --role developer --prompt-file .hoh-dev/developer-prompt.md --timeout 3600
```

示例上下文只用于演示；真实任务必须引用当前REQ与候选，不能直接拿示例验收。桥探测实际help，不解析聊天中的“完成”当最终成功。Codex/Claude/Pi各自原生命令与事件不同；桥的Pi JSON单次进程退出不能替代产品Pi RPC中对`agent_settled`的协商。

桥默认插入局部任务信封，阻止下级会话因根AGENTS而递归启动全项目；只有start.sh初次启动使用--top-level。桥不重试、不调度下一任务、不充当总指挥。原生子进程完成记录永远`product_accepted:false`。日志在stderr提示的位置，超时终止本次进程组并保存失败。不要给多个Writer同时派发同一目录写任务。

## 冻结候选再执行检查

实现阶段在Git保存本次源码，确保工作区干净，把日志输出放进被忽略的`.hoh-dev/`。先归档真实环境清单，再冻结：

```bash
python3 tools/preflight.py --output .hoh-dev/environment.json
python3 tools/candidate.py freeze \
  --spec docs/01-prd.md --environment .hoh-dev/environment.json \
  --test-plan docs/09-testing.md --out .hoh-dev/candidates/candidate-001.json
python3 tools/run_check.py --check-id unit \
  --candidate-file .hoh-dev/candidates/candidate-001.json --mode product \
  --out .hoh-dev/evidence/candidate-001/unit --timeout 3600 -- cargo test --workspace --locked
```

`preflight`失败需处理真实缺项；保存了JSON不等于合格。正式Candidate可使用产品导出的完整SpecManifest代替单一PRD。spec/test-plan文件里涉及其他文档时，产品必须用组合hash；工具的源码清单额外绑定所有已跟踪内容。

冻结清单保存Git commit/tree、全部普通已跟踪文件hash、spec/environment/test plan hash。检查前后均验证；候选改动、环境清单被改、未跟踪源码出现都会拒绝。自举工具拒绝子模块/符号链接，正式产品如需支持必须另做显式身份规则与测试。此限制不得被默默绕过。

## 最终Manifest

按`contracts/release.schema.json`汇总全部14类检查、70个AC观察记录、两个独立会话报告、soak时间线、发布物与CandidateManifest。每个AC observation有对应TEST、具体observed内容和原始证据引用。不可把同一句“通过”复制70次代替真实覆盖。

```bash
python3 tools/release_gate.py --manifest .hoh-dev/release/manifest.json
```

证据门禁核对文件hash、版本、来源模式、独立会话、原始报告、持续时间与真实迭代记录。**它不理解任意软件的全部语义，更不能防止拥有整个VM写权限的执行者刻意伪造所有材料。** Independent QA/Reviewer仍必须审查真实执行和需求覆盖；开发规范禁止伪造。

## 工具退出语义

自测0为启动包通过；product缺实现/清单返回3；桥原生进程失败非0，超时124、中断130、缺CLI127、不支持参数78。bootstrap用法错误64、能力缺失3/78。JSON输出和stderr诊断分开。回传0从来不意味着另一个层级的产品验收通过。
