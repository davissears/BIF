mod support;

use rusqlite::Connection;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use support::OwnedTestDirectory as TestDirectory;

fn bif(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bif"))
        .args(arguments)
        .current_dir(directory)
        .env("HOME", directory)
        .env("XDG_CONFIG_HOME", directory.join(".config"))
        .env("APPDATA", directory.join("AppData"))
        .env_remove("BIF_ROOT")
        .env_remove("BIF_REQUESTER")
        .env_remove("BIF_CONFIG")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn init_is_repeatable_and_doctor_reports_the_configured_store() {
    let directory = TestDirectory::new();
    let root = directory.path().join("root");
    fs::create_dir(&root).unwrap();
    let config = directory.path().join("config.toml");
    let arguments = [
        "init",
        "--root",
        root.to_str().unwrap(),
        "--requester",
        "Delta Agent",
        "--config",
        config.to_str().unwrap(),
    ];

    let first = bif(directory.path(), &arguments);
    assert!(first.status.success(), "{}", text(&first.stderr));
    let connection = Connection::open(root.join(".bif/bif.sqlite")).unwrap();
    let first_id: String = connection
        .query_row("SELECT store_id FROM store_metadata", [], |row| row.get(0))
        .unwrap();
    drop(connection);

    let second = bif(directory.path(), &arguments);
    assert!(second.status.success(), "{}", text(&second.stderr));
    let connection = Connection::open(root.join(".bif/bif.sqlite")).unwrap();
    let second_id: String = connection
        .query_row("SELECT store_id FROM store_metadata", [], |row| row.get(0))
        .unwrap();
    assert_eq!(first_id, second_id);

    let doctor = bif(
        directory.path(),
        &["doctor", "--config", config.to_str().unwrap()],
    );
    assert!(doctor.status.success(), "{}", text(&doctor.stderr));
    let output = text(&doctor.stdout);
    assert!(output.contains("version: 0.1.0\n"));
    assert!(output.contains(&format!("config: {}\n", config.display())));
    assert!(output.contains(&format!("store-id: {first_id}\n")));
    assert!(output.contains("schema-version: 2\n"));
    assert!(output.contains("project: bif-owned-test-directory-"));
}

#[test]
fn project_registration_is_persistent_idempotent_and_listed() {
    let directory = TestDirectory::new();
    let root = directory.path().join("root");
    let checkout = directory.path().join("checkout");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&checkout).unwrap();
    let config = directory.path().join("config.toml");
    assert!(
        bif(
            directory.path(),
            &[
                "init",
                "--root",
                root.to_str().unwrap(),
                "--requester",
                "Agent",
                "--config",
                config.to_str().unwrap(),
            ],
        )
        .status
        .success()
    );
    let register = [
        "project",
        "register",
        "My Project",
        "--path",
        checkout.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
    ];
    assert!(bif(directory.path(), &register).status.success());
    assert!(bif(directory.path(), &register).status.success());

    let listed = bif(
        directory.path(),
        &["project", "list", "--config", config.to_str().unwrap()],
    );
    assert!(listed.status.success(), "{}", text(&listed.stderr));
    assert_eq!(
        text(&listed.stdout),
        format!(
            "my-project\tpath\t{}\n",
            fs::canonicalize(checkout).unwrap().display()
        )
    );
}

#[test]
fn usage_and_operational_failures_have_stable_statuses() {
    let directory = TestDirectory::new();
    let unknown = bif(directory.path(), &["capture"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert_eq!(
        text(&unknown.stderr).lines().next(),
        Some("error: capture requires TITLE")
    );

    let unconfigured = bif(directory.path(), &["doctor"]);
    assert_eq!(unconfigured.status.code(), Some(1));
    assert_eq!(
        text(&unconfigured.stderr),
        "error: BIF is not initialized; configure both a store root and requester\n"
    );
}
