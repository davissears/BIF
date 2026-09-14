#![cfg(not(unix))]

use std::{fs, process::Command};

#[test]
fn generator_fails_closed_before_claiming_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("store.sqlite3");
    let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .args(["100", "--output"])
        .arg(&database)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("requires Unix NOFOLLOW and stable same-file identity semantics")
    );
    assert!(!database.exists());
    assert!(!fs::exists(format!("{}.metadata.json", database.display())).unwrap());
}
