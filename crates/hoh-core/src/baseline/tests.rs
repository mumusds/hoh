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

#[test]
fn a_scope_holds_files_along_path_components_and_an_empty_one_none() {
    let uncommitted = vec![
        "lib/a.rs".to_owned(),
        "lib2/b.rs".to_owned(),
        "notes.txt".to_owned(),
    ];
    let scope = |paths: &[&str]| paths.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
    assert_eq!(touched(&scope(&["lib"]), &uncommitted), ["lib/a.rs"]);
    assert_eq!(touched(&scope(&["lib/"]), &uncommitted), ["lib/a.rs"]);
    assert_eq!(
        touched(&scope(&["notes.txt", "lib2"]), &uncommitted),
        ["lib2/b.rs", "notes.txt"]
    );
    assert!(touched(&scope(&[]), &uncommitted).is_empty());
    assert!(touched(&scope(&[""]), &uncommitted).is_empty());
    assert!(touched(&scope(&["li"]), &uncommitted).is_empty());
}

/// M3N-001 (docs/adr/ADR-039.md, round 1 corrections): plan scopes and baseline files compare by their normalized
/// project-relative components: `.` is the project root, `./x`, `x//y` and a trailing `/` name what `x`, `x/y` name.
#[test]
fn test_028_03_scopes_and_files_compare_by_normalized_components() {
    let uncommitted = vec!["app/page.md".to_owned(), "app2/x".to_owned()];
    let scope = |paths: &[&str]| paths.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
    for held in ["./app", "app//page.md", "./app/", "app/./page.md", "app//"] {
        assert_eq!(
            touched(&scope(&[held]), &uncommitted),
            ["app/page.md"],
            "{held}"
        );
    }
    for root in [".", "./", ".//."] {
        assert_eq!(
            touched(&scope(&[root]), &uncommitted),
            ["app/page.md", "app2/x"],
            "{root}"
        );
    }
    // A file named in another spelling is the same file.
    assert_eq!(
        touched(&scope(&["app"]), &scope(&["./app//page.md"])),
        ["./app//page.md"]
    );
    assert!(touched(&scope(&["./ap"]), &uncommitted).is_empty());
    assert!(touched(&scope(&[""]), &uncommitted).is_empty());
}

#[test]
fn a_test_that_failed_in_the_baseline_is_no_regression() {
    let out = "running 2 tests\ntest answer ... ok\ntest fails ... FAILED\ntest it::deep ... FAILED\n\nfailures:\n";
    assert_eq!(failed_tests(out), ["fails", "it::deep"]);
    assert!(failed_tests("test result: ok").is_empty());
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    assert_eq!(regressions(&s(&["fails"]), &s(&["fails", "new"])), ["new"]);
    assert!(regressions(&s(&["fails"]), &s(&["fails"])).is_empty());
    assert_eq!(regressions(&s(&[]), &s(&["fails"])), ["fails"]);
}
