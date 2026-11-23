#![forbid(unsafe_code)]
#![forbid(clippy::disallowed_methods)]
//! Template rendering and file publication, real filesystem and config, no agents or network.
use hoh_core::{
    config,
    resource_bundle::Bundle,
    template_files::{self, Selection},
    templates::{self, Inputs},
};
type R = Result<(), Box<dyn std::error::Error>>;

#[test]
fn test_029_02_default_profile_source_and_reprofile_preserves_edited_files() -> R {
    for config_text in [
        None,
        Some("config_version = \"1.0\"\n[project]\ndefault_mode = \"PLAN\"\n"),
        Some("config_version = \"1.0\"\n[project]\ndocument_profile = \"lite\"\n"),
    ] {
        let explicit = config_text.is_some_and(|s| s.contains("document_profile"));
        let dir = tempfile::Builder::new().prefix("m3t08-docs-").tempdir()?;
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hoh"))?;
        if let Some(text) = config_text {
            std::fs::write(root.join(config::FILE), text)?;
        }
        let cfg = config::load(root)?;
        let selection = Selection::from_snapshot(&serde_json::json!({"project": cfg.project}))?;
        assert_eq!(selection.profile, if explicit { "lite" } else { "full" });
        assert_eq!(
            selection.source,
            if explicit { "project" } else { "default" }
        );
        let templates = templates::load(root, None, "run", &Bundle::current())?;
        let inputs = Inputs {
            goal: "Build a local tool".into(),
            owners: "developer".into(),
        };
        let first = template_files::instantiate(root, &templates, &selection, &inputs)?;
        assert_eq!(first.profile, selection.profile);
        assert_eq!(first.source, selection.source);
        assert_eq!(first.created.len(), if explicit { 4 } else { 17 });
        assert!(first.skipped.is_empty());
        for file in &first.created {
            assert!(!std::fs::read_to_string(root.join(file))?.contains("{{"));
        }
        let edited = root.join("docs/feature.md");
        std::fs::write(
            &edited,
            "User edited this, with {{literal}} and no metadata.\n",
        )?;
        let digest = hoh_core::specindex::digest(&std::fs::read(&edited)?);
        let full = Selection {
            profile: "full".into(),
            source: "project".into(),
        };
        let second = template_files::instantiate(root, &templates, &full, &inputs)?;
        assert_eq!(second.created.len(), if explicit { 13 } else { 0 });
        assert_eq!(second.skipped.len(), first.created.len());
        assert_eq!(
            hoh_core::specindex::digest(&std::fs::read(&edited)?),
            digest
        );
        let retry = template_files::instantiate(root, &templates, &full, &inputs)?;
        assert!(retry.created.is_empty());
        assert_eq!(retry.skipped.len(), 17);
    }
    Ok(())
}

#[test]
fn test_029_02_invalid_render_or_link_cannot_overwrite_or_escape() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t08-doc-errors-")
        .tempdir()?;
    let root = dir.path().join("project");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(&outside)?;
    let templates = templates::load(&root, None, "run", &Bundle::current())?;
    let full = Selection {
        profile: "full".into(),
        source: "default".into(),
    };
    let bad = Inputs {
        goal: " ".into(),
        owners: "developer".into(),
    };
    assert!(template_files::instantiate(&root, &templates, &full, &bad).is_err());
    assert!(!root.join("docs").exists());
    std::os::unix::fs::symlink(&outside, root.join("docs"))?;
    let good = Inputs {
        goal: "Goal".into(),
        owners: "developer".into(),
    };
    assert!(template_files::instantiate(&root, &templates, &full, &good).is_err());
    assert_eq!(std::fs::read_dir(&outside)?.count(), 0);
    assert!(Selection::from_snapshot(&serde_json::json!({"project":{"document_profile":"invalid","document_profile_source":"project"}})).is_err());
    Ok(())
}

#[test]
fn test_029_02_retry_after_publication_error_keeps_completed_files() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t08-doc-retry-")
        .tempdir()?;
    let root = dir.path();
    std::fs::create_dir_all(root.join("docs/ux.md"))?;
    let templates = templates::load(root, None, "run", &Bundle::current())?;
    let selection = Selection {
        profile: "full".into(),
        source: "project".into(),
    };
    let inputs = Inputs {
        goal: "Goal".into(),
        owners: "developer".into(),
    };
    assert!(template_files::instantiate(root, &templates, &selection, &inputs).is_err());
    let before = std::fs::read(root.join("docs/acceptance.md"))?;
    std::fs::remove_dir(root.join("docs/ux.md"))?;
    let retried = template_files::instantiate(root, &templates, &selection, &inputs)?;
    assert!(retried.created.contains(&"docs/ux.md".into()));
    assert!(retried.skipped.contains(&"docs/acceptance.md".into()));
    assert_eq!(std::fs::read(root.join("docs/acceptance.md"))?, before);
    Ok(())
}

fn no_staged_files(root: &std::path::Path) -> R {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        assert!(
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".hoh-template-"),
            "named staging file: {:?}",
            entry.path()
        );
        if entry.file_type()?.is_dir() {
            no_staged_files(&entry.path())?;
        }
    }
    Ok(())
}

#[test]
fn test_029_02_publication_rejects_links_fifo_and_unwritable_directory() -> R {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    for case in [
        "leaf",
        "intermediate",
        "fifo",
        "readonly",
        "unwritable",
        "root",
    ] {
        let dir = tempfile::Builder::new().prefix("m3t08-files-").tempdir()?;
        let root = dir.path().join("project");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("docs"))?;
        std::fs::create_dir_all(&outside)?;
        let sentinel = outside.join("keep");
        std::fs::write(&sentinel, "outside bytes")?;
        let before = std::fs::metadata(&sentinel)?;
        let target = root.join("docs/feature.md");
        match case {
            "leaf" => symlink(&sentinel, &target)?,
            "intermediate" => symlink(&outside, root.join("docs/spec"))?,
            "fifo" => rustix::fs::mknodat(
                rustix::fs::CWD,
                &target,
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::from_raw_mode(0o600),
                0,
            )?,
            "readonly" => {
                std::fs::write(&target, "user readonly bytes")?;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444))?;
            }
            "unwritable" => {
                std::fs::set_permissions(root.join("docs"), std::fs::Permissions::from_mode(0o555))?
            }
            "root" => symlink(&root, dir.path().join("linked"))?,
            _ => unreachable!(),
        }
        let templates = templates::load(&root, None, "run", &Bundle::current())?;
        let linked = dir.path().join("linked");
        let result = template_files::instantiate(
            if case == "root" { &linked } else { &root },
            &templates,
            &Selection {
                profile: "full".into(),
                source: "default".into(),
            },
            &Inputs {
                goal: "Goal".into(),
                owners: "developer".into(),
            },
        );
        if case == "unwritable" {
            std::fs::set_permissions(root.join("docs"), std::fs::Permissions::from_mode(0o755))?;
        }
        if case == "readonly" {
            assert!(result?.skipped.contains(&"docs/feature.md".to_owned()));
            assert_eq!(std::fs::read_to_string(&target)?, "user readonly bytes");
            assert_eq!(
                std::fs::metadata(&target)?.permissions().mode() & 0o777,
                0o444
            );
        } else {
            assert!(result.is_err(), "{case}: {result:?}");
        }
        assert_eq!(std::fs::read_to_string(&sentinel)?, "outside bytes");
        let after = std::fs::metadata(&sentinel)?;
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert_eq!(std::fs::read_dir(&outside)?.count(), 1);
        no_staged_files(&root)?;
    }
    Ok(())
}

// A real write is interrupted at a deterministic byte offset. No product fault hook or live agent.
#[test]
fn test_029_02_killed_mid_write_leaves_no_named_staging_file() -> R {
    use std::os::unix::process::ExitStatusExt;
    const CHILD: &str = "HOH_M3T08_WRITE_CHILD";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = std::path::Path::new(&root);
        let templates = templates::load(root, None, "run", &Bundle::current())?;
        template_files::instantiate(
            root,
            &templates,
            &Selection {
                profile: "full".into(),
                source: "default".into(),
            },
            &Inputs {
                goal: "M3T08_WRITE_PROBE".into(),
                owners: "developer".into(),
            },
        )?;
        panic!("write interruption did not fire");
    }
    let dir = tempfile::Builder::new()
        .prefix("m3t08-kill-write-")
        .tempdir()?;
    let root = dir.path().join("project");
    std::fs::create_dir(&root)?;
    let source = dir.path().join("interrupt.c");
    std::fs::write(
        &source,
        r##"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <signal.h>
#include <string.h>
#include <unistd.h>
ssize_t write(int fd, const void *buf, size_t n) {
    ssize_t (*real_write)(int, const void *, size_t) = dlsym(RTLD_NEXT, "write");
    if (n > 20 && !memcmp(buf, "# M3T08_WRITE_PROBE", 19)) {
        ssize_t written = real_write(fd, buf, 8);
        if (written == 8) kill(getpid(), SIGKILL);
        _exit(90);
    }
    return real_write(fd, buf, n);
}
"##,
    )?;
    let library = dir.path().join("interrupt.so");
    let compiled = std::process::Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Werror"])
        .arg(&source)
        .args(["-ldl", "-o"])
        .arg(&library)
        .output()?;
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let output = std::process::Command::new(std::env::current_exe()?)
        .env(CHILD, &root)
        .env("LD_PRELOAD", &library)
        .args([
            "--exact",
            "test_029_02_killed_mid_write_leaves_no_named_staging_file",
            "--nocapture",
        ])
        .output()?;
    assert_eq!(
        output.status.signal(),
        Some(9),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    no_staged_files(&root)?;
    assert_eq!(std::fs::read_dir(root.join("docs"))?.count(), 0);
    let templates = templates::load(&root, None, "run", &Bundle::current())?;
    let recovered = template_files::instantiate(
        &root,
        &templates,
        &Selection {
            profile: "full".into(),
            source: "default".into(),
        },
        &Inputs {
            goal: "M3T08_WRITE_PROBE".into(),
            owners: "developer".into(),
        },
    )?;
    assert_eq!(recovered.created.len(), 17);
    no_staged_files(&root)?;
    Ok(())
}
