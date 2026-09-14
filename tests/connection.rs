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

#[cfg(unix)]
#[test]
fn nofollow_factory_rejects_an_intermediate_symbolic_component() {
    use std::{fs, os::unix::fs::symlink};

    use bif::storage::open_nofollow;

    let directory = OwnedTestDirectory::new();
    let canonical_root = fs::canonicalize(directory.path()).unwrap();
    let real = canonical_root.join("real");
    let alias = canonical_root.join("alias");
    fs::create_dir(&real).unwrap();
    symlink(&real, &alias).unwrap();

    assert!(open_nofollow(alias.join("bif.sqlite")).is_err());
    assert!(
        !real.join("bif.sqlite").exists(),
        "the Unix VFS must not follow the intermediate symbolic component"
    );
}
