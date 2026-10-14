mod maintenance_support;

use maintenance_support::{Fixture, inventory, read};
use std::{fs, process::Command};

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bif"))
        .args(args)
        .env("BIF_ROOT", "/not/a/store")
        .env("BIF_CONFIG", "/not/a/config")
        .output()
        .unwrap()
}

#[test]
fn explicit_cli_backup_restore_and_offline_acknowledgement() {
    let fixture = Fixture::new();
    let store = fixture.store.to_str().unwrap();
    let backup = fixture.backup.to_str().unwrap();
    let preserve = fixture.path("later.sqlite");
    let preserve = preserve.to_str().unwrap();
    for args in [
        vec![
            "maintenance",
            "backup",
            "--store",
            store,
            "--output",
            backup,
        ],
        vec!["maintenance", "backup", "--offline", "--output", backup],
        vec![
            "maintenance",
            "restore",
            "--store",
            store,
            "--from",
            backup,
            "--offline",
        ],
        vec![
            "maintenance",
            "backup",
            "--store",
            store,
            "--output",
            backup,
            "--offline",
            "--offline",
        ],
        vec![
            "maintenance",
            "backup",
            "--store",
            store,
            "--output",
            backup,
            "--offline",
            "--root",
            "ignored",
        ],
    ] {
        let result = cli(&args);
        assert_eq!(result.status.code(), Some(2), "{args:?}");
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains("maintenance backup"));
        assert!(error.contains("stop/reap"));
        assert!(error.contains("disable"));
        assert!(result.stdout.is_empty());
        assert!(!fixture.backup.exists());
    }
    let result = cli(&[
        "maintenance",
        "backup",
        "--store",
        store,
        "--output",
        backup,
        "--offline",
    ]);
    assert!(result.status.success(), "{:?}", result.stderr);
    assert_eq!(
        inventory(&read(&fixture.backup)),
        inventory(&read(&fixture.store))
    );
    let bytes = fs::read(&fixture.backup).unwrap();
    let result = cli(&[
        "maintenance",
        "restore",
        "--store",
        store,
        "--from",
        backup,
        "--preserve-later",
        preserve,
        "--offline",
    ]);
    assert!(result.status.success(), "{:?}", result.stderr);
    assert!(std::path::Path::new(preserve).exists());
    let result = cli(&[
        "maintenance",
        "restore",
        "--store",
        store,
        "--from",
        backup,
        "--preserve-later",
        preserve,
        "--offline",
    ]);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert_eq!(fs::read(&fixture.backup).unwrap(), bytes);
}
