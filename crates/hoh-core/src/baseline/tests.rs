use super::*;

#[test]
fn porcelain_lists_changed_tracked_files_and_untracked_ones_and_skips_a_renames_old_path() {
    let out =
        b" M lib/a.rs\0M  README.rst\0R  new.rs\0old.rs\0?? notes.txt\0!! target/x\0 D gone.rs\0";
    assert_eq!(
        status(out),
        Status {
            dirty: vec![
                "README.rst".to_owned(),
                "gone.rs".to_owned(),
                "lib/a.rs".to_owned(),
                "new.rs".to_owned(),
            ],
            untracked: vec!["notes.txt".to_owned()],
        }
    );
    assert_eq!(status(b""), Status::default());
}

#[test]
fn every_convention_names_its_commands_and_a_side_with_none_is_unknown() {
    let none = Conventions::default();
    assert_eq!(
        commands(&none),
        (vec!["unknown".to_owned()], vec!["unknown".to_owned()])
    );
    let all = Conventions {
        cargo_toml: Some("[package]\n".to_owned()),
        package_json: Some(r#"{"scripts": {"test": "jest"}}"#.to_owned()),
        makefile: Some("all: x\n\tcc x\ntest:\n\t./t\n".to_owned()),
        pyproject_toml: Some("[tool.pytest.ini_options]\n".to_owned()),
    };
    assert_eq!(
        commands(&all),
        (
            vec!["cargo build".to_owned(), "make".to_owned()],
            vec![
                "cargo test".to_owned(),
                "npm test".to_owned(),
                "make test".to_owned(),
                "pytest".to_owned()
            ]
        )
    );
    // A package.json without scripts, a Makefile without targets and a pyproject without pytest name nothing.
    let bare = Conventions {
        package_json: Some("{}".to_owned()),
        makefile: Some("# nothing\n".to_owned()),
        pyproject_toml: Some("[project]\nname = \"x\"\n".to_owned()),
        ..Conventions::default()
    };
    assert_eq!(
        commands(&bare),
        (vec!["unknown".to_owned()], vec!["unknown".to_owned()])
    );
}

#[test]
fn documents_are_the_readmes_and_the_existing_doc_directories() {
    let root = vec![
        "README.rst".to_owned(),
        "Cargo.toml".to_owned(),
        "README.zh.md".to_owned(),
    ];
    assert_eq!(
        docs(&root, &["docs", "adr"]),
        ["README.rst", "README.zh.md", "adr/", "docs/"]
    );
}
