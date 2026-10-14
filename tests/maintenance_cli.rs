mod maintenance_support;

use maintenance_support::{Fixture, generation, inventory, read};
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

#[test]
fn explicit_prune_requires_binding_bounds_and_offline_without_config_selection() {
    let fixture = Fixture::new();
    let store = fixture.store.to_str().unwrap();
    let generation = generation(&read(&fixture.store));
    let before = inventory(&read(&fixture.store));
    let valid = vec![
        "maintenance",
        "prune",
        "--store",
        store,
        "--expected-generation",
        &generation,
        "--through",
        "1",
        "--offline",
    ];
    for (remove, replacement) in [
        (8, None),
        (2, None),
        (4, None),
        (6, None),
        (7, Some("-1")),
        (7, Some("9223372036854775808")),
        (7, Some("18446744073709551616")),
        (7, Some("1.0")),
        (5, Some("bad")),
    ] {
        let mut args = valid.clone();
        if let Some(value) = replacement {
            args[remove] = value;
        } else if remove == 8 {
            args.remove(remove);
        } else {
            args.drain(remove..remove + 2);
        }
        let result = cli(&args);
        assert_eq!(
            result.status.code(),
            Some(2),
            "{args:?}: {:?}",
            result.stderr
        );
        assert!(result.stdout.is_empty());
        assert_eq!(inventory(&read(&fixture.store)), before);
    }
    for extra in ["--offline", "--root", "--project", "--through"] {
        let mut args = valid.clone();
        args.push(extra);
        let result = cli(&args);
        assert_eq!(result.status.code(), Some(2), "{args:?}");
        assert_eq!(inventory(&read(&fixture.store)), before);
    }
    let mut future = valid.clone();
    future[7] = "3";
    let result = cli(&future);
    assert_eq!(result.status.code(), Some(1), "{:?}", result.stderr);
    assert!(result.stdout.is_empty());
    assert_eq!(inventory(&read(&fixture.store)), before);
    let result = cli(&valid);
    assert!(result.status.success(), "{:?}", result.stderr);
    assert!(String::from_utf8(result.stdout).unwrap().contains("Pruned"));
    let connection = read(&fixture.store);
    let snapshot = bif::storage::journal::JournalSnapshot::begin(&connection).unwrap();
    assert_eq!(snapshot.metadata().retention_floor, 1);
    assert_eq!(snapshot.metadata().high_water, 2);
    assert_eq!(snapshot.metadata().generation.as_str(), generation);
}
