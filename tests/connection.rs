mod support;

use std::path::PathBuf;

use bif::storage::open;
use support::OwnedTestDirectory;

fn temporary_database() -> (OwnedTestDirectory, PathBuf) {
    let directory = OwnedTestDirectory::new();
    let path = directory.path().join("bif.sqlite");
    (directory, path)
}

#[test]
fn connection_factory_applies_all_sqlite_settings() {
    let (_directory, path) = temporary_database();
    let connection = open(&path).unwrap();

    let foreign_keys: i64 = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let synchronous: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    let busy_timeout_ms: i64 = connection
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .unwrap();

    assert_eq!(foreign_keys, 1);
    assert_eq!(journal_mode, "wal");
    assert_eq!(synchronous, 2);
    assert_eq!(busy_timeout_ms, 5_000);

    drop(connection);
}
