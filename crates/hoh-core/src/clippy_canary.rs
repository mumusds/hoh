//! Clippy canary (AC-062-05): compiled only with `--cfg hoh_clippy_canary`, by `scripts/product/static.sh`. Outside the
//! Persistence module it calls every SQLite method that clippy.toml disallows, one per line; the static check fails
//! unless clippy refuses each of them, which proves the boundary is in force in the environment the check runs in.
#![allow(dead_code)]

use fallible_iterator_use::*;
mod fallible_iterator_use {
    pub use rusqlite::fallible_iterator::FallibleIterator;
    pub use rusqlite::fallible_streaming_iterator::FallibleStreamingIterator;
}
use rusqlite::{
    Connection, DropBehavior, OpenFlags, PrepFlags, Rows, Savepoint, Statement, Transaction,
};

fn connection(c: &Connection, m: &mut Connection, n: &mut Connection, o: &mut Connection) {
    let _ = Connection::open("canary.db");
    let _ = Connection::open_with_flags("canary.db", OpenFlags::default());
    let _ = Connection::open_with_flags_and_vfs("canary.db", OpenFlags::default(), c"unix");
    let _ = Connection::open_in_memory();
    let _ = Connection::open_in_memory_with_flags(OpenFlags::default());
    let _ = Connection::open_in_memory_with_flags_and_vfs(OpenFlags::default(), c"unix");
    let _ = c.execute("SELECT 1", []);
    let _ = c.execute_batch("SELECT 1");
    let _ = c.prepare("SELECT 1");
    let _ = c.prepare_cached("SELECT 1");
    let _ = c.prepare_with_flags("SELECT 1", PrepFlags::default());
    let _: rusqlite::Result<i64> = c.query_row("SELECT 1", [], |r| r.get(0));
    let _: rusqlite::Result<i64> = c.query_row_and_then("SELECT 1", [], |r| r.get(0));
    let _: rusqlite::Result<i64> = c.query_one("SELECT 1", [], |r| r.get(0));
    let _ = c.pragma(None, "user_version", 1, |_| Ok(()));
    let _ = c.pragma_query(None, "user_version", |_| Ok(()));
    let _: rusqlite::Result<i64> = c.pragma_query_value(None, "user_version", |r| r.get(0));
    let _ = c.pragma_update(None, "user_version", 1);
    let _: rusqlite::Result<i64> = c.pragma_update_and_check(None, "user_version", 1, |r| r.get(0));
    let _ = m.transaction();
    let _ = n.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate);
    let _ = c.unchecked_transaction();
    let _ = o.savepoint();
    let _ = o.savepoint_with_name("canary");
    let _ = rusqlite::Batch::new(c, "SELECT 1");
}

fn statement(s: &mut Statement<'_>) {
    let _ = s.execute([]);
    let _ = s.insert([]);
    let _ = s.raw_execute();
    let _ = s.query([]);
    let _ = s.query_map([], |r| r.get::<_, i64>(0));
    let _ = s.query_and_then([], |r| r.get::<_, i64>(0));
    let _: rusqlite::Result<i64> = s.query_row([], |r| r.get(0));
    let _: rusqlite::Result<i64> = s.query_one([], |r| r.get(0));
    let _ = s.exists([]);
    let _ = s.raw_query();
}

fn rows(
    a: &mut Rows<'_>,
    b: Rows<'_>,
    c: Rows<'_>,
    d: Rows<'_>,
    e: &mut Rows<'_>,
    f: &mut Rows<'_>,
) {
    let _ = a.next();
    let _ = b.map(|r| r.get::<_, i64>(0));
    let _ = c.mapped(|r| r.get::<_, i64>(0));
    let _ = d.and_then(|r| r.get::<_, i64>(0));
    let _ = FallibleStreamingIterator::advance(e);
    let _ = FallibleStreamingIterator::next(f);
}

fn mapped(mut m: rusqlite::Map<'_, fn(&rusqlite::Row<'_>) -> rusqlite::Result<i64>>) {
    let _ = FallibleIterator::next(&mut m);
}

#[allow(clippy::too_many_arguments)]
fn transaction(
    c: &mut Connection,
    d: &mut Connection,
    t1: Transaction<'_>,
    t2: Transaction<'_>,
    t3: Transaction<'_>,
    mut t4: Transaction<'_>,
    s1: Savepoint<'_>,
    mut s2: Savepoint<'_>,
    s3: Savepoint<'_>,
    mut s4: Savepoint<'_>,
) {
    let _ = Transaction::new(c, rusqlite::TransactionBehavior::Deferred);
    let _ = Transaction::new_unchecked(d, rusqlite::TransactionBehavior::Deferred);
    let _ = t1.commit();
    let _ = t2.rollback();
    let _ = t3.finish();
    let _ = t4.savepoint();
    let _ = t4.savepoint_with_name("canary");
    t4.set_drop_behavior(DropBehavior::Commit);
    let _ = Savepoint::new(c);
    let _ = Savepoint::with_name(d, "canary");
    let _ = s1.commit();
    let _ = s2.rollback();
    let _ = s3.finish();
    let _ = s4.savepoint();
    let _ = s4.savepoint_with_name("canary");
    s4.set_drop_behavior(DropBehavior::Commit);
}
