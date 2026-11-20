//! The `hoh` command line (REQ-003): `hoh`, `hoh -p <text>`, `hoh attach [run_id]`, `hoh web`, `hoh doctor`, `help`,
//! `--help` and `--version`, nothing more. Anything else is a usage error (exit 64), decided here from the arguments
//! alone, before any host, file or database is touched. The argument after `-p` is always its text.

use std::ffi::OsString;
use std::path::PathBuf;

/// `--output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Text,
    Json,
    StreamJson,
}

/// What `--help` describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    Top,
    Attach,
    Web,
    Doctor,
}

/// A valid command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Help on `topic`; `project` is kept so that a missing `--project` directory is still refused.
    Help {
        topic: Topic,
        project: Option<PathBuf>,
    },
    Version,
    /// Bare `hoh`: the TUI workspace.
    Workspace {
        project: Option<PathBuf>,
    },
    /// `hoh -p <text>`: one non-interactive run, or input for `resume`.
    Prompt {
        text: String,
        output: Output,
        detach: bool,
        resume: Option<String>,
        project: Option<PathBuf>,
    },
    Attach {
        run_id: Option<String>,
        output: Output,
        project: Option<PathBuf>,
    },
    Web {
        project: Option<PathBuf>,
    },
    Doctor {
        live: bool,
        output: Output,
        project: Option<PathBuf>,
    },
}

/// Why a command line is refused (exit 64).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

fn help_on(topic: Topic) -> Invocation {
    Invocation::Help {
        topic,
        project: None,
    }
}

fn usage<T>(message: impl Into<String>) -> Result<T, UsageError> {
    Err(UsageError(message.into()))
}

#[derive(Default)]
struct Options {
    prompt: Option<String>,
    output: Option<String>,
    project: Option<String>,
    resume: Option<String>,
    detach: bool,
    live: bool,
    help: bool,
    version: bool,
}

/// Set an option at most once.
fn once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), UsageError> {
    if slot.replace(value).is_some() {
        return usage(format!("{name} 只能给一次"));
    }
    Ok(())
}

fn flag(slot: &mut bool, name: &str) -> Result<(), UsageError> {
    if std::mem::replace(slot, true) {
        return usage(format!("{name} 只能给一次"));
    }
    Ok(())
}

fn output(value: Option<&str>, allowed: &[Output]) -> Result<Output, UsageError> {
    let parsed = match value {
        None => Output::Text,
        Some("text") => Output::Text,
        Some("json") => Output::Json,
        Some("stream-json") => Output::StreamJson,
        Some(other) => {
            return usage(format!(
                "--output 不支持 {other}：可选 text、json、stream-json"
            ));
        }
    };
    if allowed.contains(&parsed) {
        Ok(parsed)
    } else {
        usage(format!(
            "这里的 --output 不支持 {}",
            value.unwrap_or("text")
        ))
    }
}

/// Parse the arguments after the program name.
pub fn parse(args: &[OsString]) -> Result<Invocation, UsageError> {
    let mut words = Vec::with_capacity(args.len());
    for arg in args {
        match arg.to_str() {
            Some(word) => words.push(word),
            None => return usage("参数不是有效的 UTF-8"),
        }
    }
    let mut o = Options::default();
    let mut positional: Vec<&str> = Vec::new();
    let mut rest = words.into_iter();
    while let Some(word) = rest.next() {
        // `--name=value` and `--name value` mean the same.
        let (name, inline) = match word.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value)),
            _ => (word, None),
        };
        let mut value = |name: &str| -> Result<String, UsageError> {
            match inline.or_else(|| rest.next()) {
                Some(v) => Ok(v.to_owned()),
                None => usage(format!("{name} 需要一个值")),
            }
        };
        match name {
            "-p" if inline.is_none() => {
                let text = value("-p")?;
                once(&mut o.prompt, text, "-p")?;
            }
            "--output" => {
                let v = value(name)?;
                once(&mut o.output, v, name)?;
            }
            "--project" => {
                let v = value(name)?;
                once(&mut o.project, v, name)?;
            }
            "--resume" => {
                let v = value(name)?;
                once(&mut o.resume, v, name)?;
            }
            "--detach" if inline.is_none() => flag(&mut o.detach, name)?,
            "--live" if inline.is_none() => flag(&mut o.live, name)?,
            "--help" if inline.is_none() => flag(&mut o.help, name)?,
            "--version" if inline.is_none() => flag(&mut o.version, name)?,
            _ if word.starts_with('-') && word != "-" => return usage(format!("未知选项 {word}")),
            _ => positional.push(word),
        }
    }
    let (command, args) = match positional.split_first() {
        Some((&first, rest)) => (Some(first), rest),
        None => (None, &[][..]),
    };
    let topic = match command {
        None => Topic::Top,
        Some("attach") => Topic::Attach,
        Some("web") => Topic::Web,
        Some("doctor") => Topic::Doctor,
        Some("help") => {
            if o.prompt.is_some()
                || o.output.is_some()
                || o.project.is_some()
                || o.resume.is_some()
                || o.detach
                || o.live
                || o.help
                || o.version
            {
                return usage("help 不接受选项");
            }
            return match args {
                [] => Ok(help_on(Topic::Top)),
                ["attach"] => Ok(help_on(Topic::Attach)),
                ["web"] => Ok(help_on(Topic::Web)),
                ["doctor"] => Ok(help_on(Topic::Doctor)),
                _ => usage(format!(
                    "没有 {} 的帮助：子命令只有 attach、web、doctor",
                    args.join(" ")
                )),
            };
        }
        Some(word @ ("plan" | "mode")) => {
            return usage(format!(
                "没有 {word} 子命令：模式在 TUI 中切换（输入 /plan，或按 Shift+Tab）"
            ));
        }
        Some(word) if o.prompt.is_some() => {
            return usage(format!(
                "-p 的文本之外不接受位置参数（多出了 {word}）；文本含空格时请加引号"
            ));
        }
        Some(word) if ["run", "resume", "init", "daemon", "start"].contains(&word) => {
            return usage(format!(
                "没有 {word} 子命令：子命令只有 attach、web、doctor"
            ));
        }
        Some(word) => {
            return usage(format!(
                "不接受位置参数 {word}：要以一段文本运行，请用 hoh -p '<文本>'；子命令只有 attach、web、doctor"
            ));
        }
    };
    // `--version` stands alone; with `--help`, every other check still applies (M1T06R-01).
    if o.version
        && (topic != Topic::Top
            || o.prompt.is_some()
            || o.output.is_some()
            || o.project.is_some()
            || o.resume.is_some()
            || o.detach
            || o.live
            || o.help)
    {
        return usage("--version 只能单独使用");
    }
    let help = o.help;
    let version = o.version;
    let project = o.project.map(PathBuf::from);
    let kept_project = project.clone();
    let only = |allowed: bool, name: &str, with: &str| {
        if allowed {
            Ok(())
        } else {
            usage(format!("{name} 只能与 {with} 一起使用"))
        }
    };
    let prompt_or_attach = o.prompt.is_some() || topic == Topic::Attach;
    only(
        o.resume.is_none() || prompt_or_attach,
        "--resume",
        "-p 或 attach",
    )?;
    only(!o.detach || o.prompt.is_some(), "--detach", "-p")?;
    only(!o.live || topic == Topic::Doctor, "--live", "doctor")?;
    if o.prompt.is_some() && topic != Topic::Top {
        return usage("-p 不能与子命令一起使用");
    }
    let invocation = match topic {
        Topic::Top => match o.prompt {
            Some(text) => {
                if text.trim().is_empty() {
                    return usage("-p 需要非空的文本");
                }
                Ok(Invocation::Prompt {
                    text,
                    output: output(
                        o.output.as_deref(),
                        &[Output::Text, Output::Json, Output::StreamJson],
                    )?,
                    detach: o.detach,
                    resume: o.resume,
                    project,
                })
            }
            None if o.output.is_some() => {
                usage("--output 只用于 -p、attach 与 doctor；交互的 hoh 打开 TUI")
            }
            None => Ok(Invocation::Workspace { project }),
        },
        Topic::Attach => {
            let run_id = match (args, o.resume) {
                ([], resume) => resume,
                ([id], None) => Some((*id).to_owned()),
                ([_], Some(_)) => {
                    return usage("attach 的 run_id 只能给一次（位置参数或 --resume）");
                }
                _ => return usage("attach 最多接受一个 run_id"),
            };
            Ok(Invocation::Attach {
                run_id,
                output: output(
                    o.output.as_deref(),
                    &[Output::Text, Output::Json, Output::StreamJson],
                )?,
                project,
            })
        }
        Topic::Web => {
            if !args.is_empty() || o.output.is_some() {
                return usage("web 只接受 --project");
            }
            Ok(Invocation::Web { project })
        }
        Topic::Doctor => {
            if !args.is_empty() {
                return usage("doctor 不接受位置参数");
            }
            Ok(Invocation::Doctor {
                live: o.live,
                output: output(o.output.as_deref(), &[Output::Text, Output::Json])?,
                project,
            })
        }
    }?;
    Ok(if help {
        Invocation::Help {
            topic,
            project: kept_project,
        }
    } else if version {
        Invocation::Version
    } else {
        invocation
    })
}

#[cfg(test)]
mod tests;
