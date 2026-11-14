use super::*;
use crate::ids::{MAX_ID, member, series};
use crate::persistence::{Options, Store, selftest};

type R = Result<(), Box<dyn std::error::Error>>;

/// A store's database with project `project` and the events `ids` (kind `x`), and a connection to it.
fn world(
    dir: &tempfile::TempDir,
    project: &str,
    ids: &[String],
) -> Result<Connection, Box<dyn std::error::Error>> {
    let path = dir.path().join(".hoh/state.db");
    drop(Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::default()
        },
    )?);
    let conn = Connection::open(&path)?;
    conn.execute(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES (?1, '/repo', '2026-11-14T00:00:00Z')",
        params![project],
    )?;
    for id in ids {
        conn.execute(
            "INSERT INTO events(project_id, event_id, kind, payload_json, created_at) \
             VALUES (?1, ?2, 'x', '{}', '2026-11-14T00:00:00Z')",
            params![project, id],
        )?;
    }
    Ok(conn)
}

#[test]
fn count_takes_only_the_series_own_members() -> R {
    // M2T15D-202, -352: the members are `<stem>/<digits>`; another family numbered under a member (`<stem>/1/…`), a
    // rest that is not all digits, `<stem>/` alone and ids beside the range are not.
    for parent in ["p1".to_owned(), "é".repeat(MAX_ID / 2), "q".repeat(MAX_ID)] {
        let stem = series(&parent, "compat-suite-opened/1");
        let ids = vec![
            member(&stem, 1),
            member(&stem, 2),
            member(&stem, 12),
            format!("{stem}/1/started"),
            format!("{}/staffing/qa/1", member(&stem, 1)),
            format!("{stem}/x"),
            format!("{stem}/1x"),
            format!("{stem}/"),
            format!("{stem}0"),
            format!("{stem}.1"),
            stem.clone(),
            member(&series(&parent, "compat-suite-ended/1"), 1),
        ];
        let dir = tempfile::tempdir()?;
        let conn = world(&dir, &parent, &ids)?;
        assert_eq!(count(&conn, &stem)?, 3, "{parent:?}");
        assert_eq!(
            count(&conn, &series(&parent, "compat-suite-ended/1"))?,
            1,
            "{parent:?}"
        );
        assert_eq!(count(&conn, &series(&parent, "staffing/qa/1"))?, 0);
    }
    Ok(())
}
