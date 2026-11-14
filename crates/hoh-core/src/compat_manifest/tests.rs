use super::*;
use serde_json::json;

type R = Result<(), Box<dyn std::error::Error>>;

fn hex(c: char) -> String {
    c.to_string().repeat(64)
}

/// A schema-valid entry.
fn entry(implementation: char) -> Value {
    json!({
        "cli_version": "codex-cli 0.166.0",
        "launcher_path": "/opt/bin/codex",
        "launcher_sha256": hex('a'),
        "implementation_path": "/opt/lib/codex",
        "implementation_sha256": hex(implementation),
        "transport": "app-server",
        "help_sha256": hex('c'),
        "schema_sha256": hex('d'),
        "capabilities": {"structured_events": true, "resume": null},
        "suite": {"status": "PASS", "fixture_set_sha256": hex('e'), "run_at": "2026-11-14T00:00:00Z",
                  "cases": [{"case": "probe", "status": "PASS"}, {"case": "live", "status": "NOT_RUN"}]},
    })
}

/// A project with `.hoh/compat-manifest.json` holding `old`.
fn project(old: &[u8]) -> Result<(tempfile::TempDir, Files), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join(".hoh"))?;
    let files = Files::of(dir.path(), "7.1");
    std::fs::write(&files.path, old)?;
    Ok((dir, files))
}

/// The names in `.hoh/`.
fn names(dir: &tempfile::TempDir) -> Result<Vec<String>, std::io::Error> {
    let mut names: Vec<String> = std::fs::read_dir(dir.path().join(".hoh"))?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()?;
    names.sort();
    Ok(names)
}

#[test]
fn a_failure_before_the_rename_leaves_the_old_manifest_whole() -> R {
    // K6: the manifest is replaced whole or not at all; a failure after the temporary file was written or synced leaves
    // the old file byte for byte, and no temporary file.
    for step in [Step::Written, Step::Synced] {
        let (dir, files) = project(b"OLD MANIFEST")?;
        let failed = replace_at(&files, b"NEW MANIFEST", &|s| {
            if s == step {
                Err(std::io::Error::other("injected"))
            } else {
                Ok(())
            }
        });
        assert!(failed.is_err(), "{step:?}");
        assert_eq!(std::fs::read(&files.path)?, b"OLD MANIFEST", "{step:?}");
        assert_eq!(names(&dir)?, [FILE], "{step:?}");
    }
    Ok(())
}

#[test]
fn a_replacement_renames_its_temporary_file_over_the_manifest() -> R {
    let (dir, files) = project(b"OLD MANIFEST")?;
    let seen = std::cell::RefCell::new(Vec::new());
    replace_at(&files, b"NEW MANIFEST", &|step| {
        // Until the rename the manifest is the old one; the temporary file holds the new bytes.
        if step != Step::Renamed {
            assert_eq!(std::fs::read(&files.path)?, b"OLD MANIFEST");
            assert_eq!(std::fs::read(&files.temp)?, b"NEW MANIFEST");
        }
        seen.borrow_mut().push(step);
        Ok(())
    })?;
    assert_eq!(*seen.borrow(), [Step::Written, Step::Synced, Step::Renamed]);
    assert_eq!(std::fs::read(&files.path)?, b"NEW MANIFEST");
    assert_eq!(names(&dir)?, [FILE]);
    // No manifest yet: the same.
    std::fs::remove_file(&files.path)?;
    replace(&files, b"FIRST")?;
    assert_eq!(std::fs::read(&files.path)?, b"FIRST");
    Ok(())
}

#[test]
fn the_new_manifest_replaces_one_entry_and_must_be_schema_valid() -> R {
    let first = replaced(None, "codex", &entry('1'), "2026-11-14T01:00:00Z")?;
    assert_eq!(first["schema_version"], "1.0");
    assert_eq!(first["agents"]["codex"], entry('1'));
    crate::schema::compat_manifest()
        .validate(&first)
        .map_err(|v| format!("{v:?}"))?;
    // Another agent's entry is kept as it was; the generation time is the publication's.
    let second = replaced(Some(&first), "claude", &entry('2'), "2026-11-14T02:00:00Z")?;
    assert_eq!(second["agents"]["codex"], entry('1'));
    assert_eq!(second["agents"]["claude"], entry('2'));
    assert_eq!(second["generated_at"], "2026-11-14T02:00:00Z");
    // A schema-invalid entry, or an agent the schema does not know, gives no manifest.
    let mut invalid = entry('3');
    invalid["implementation_sha256"] = json!("not hex");
    let refused = replaced(Some(&first), "codex", &invalid, "2026-11-14T03:00:00Z");
    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.contains("implementation_sha256")),
        "{refused:?}"
    );
    assert!(replaced(None, "gemini", &entry('4'), "2026-11-14T03:00:00Z").is_err());
    Ok(())
}

#[test]
fn temporary_files_are_named_apart() {
    let root = Path::new("/p");
    assert_eq!(path(root), Path::new("/p/.hoh/compat-manifest.json"));
    assert_eq!(
        temp(root, "12.3"),
        Path::new("/p/.hoh/.compat-manifest.json.12.3.tmp")
    );
    assert!(is_temp(".compat-manifest.json.12.3.tmp"));
    assert!(!is_temp(".compat-manifest.json..tmp"));
    assert!(!is_temp("compat-manifest.json"));
}
