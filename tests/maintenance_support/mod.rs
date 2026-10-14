use std::path::{Path, PathBuf};

use bif::{
    application::{
        Actor, ActorKind, CaptureIdentity, CaptureInput, CaptureRequest, CaptureStore, Execution,
    },
    domain::{ItemContent, ProjectId, Provenance, RequesterId, Timestamp},
    storage::{self, CaptureRepository},
};
use rusqlite::{Connection, types::Value};

pub struct Fixture {
    pub directory: tempfile::TempDir,
    pub store: PathBuf,
    pub backup: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = directory.path().join(".bif/bif.sqlite");
        std::fs::create_dir(store.parent().unwrap()).unwrap();
        let mut connection = storage::open(&store).unwrap();
        capture(&mut connection, "one", "original");
        capture(&mut connection, "two", "second");
        Self {
            backup: directory.path().join("backup.sqlite"),
            directory,
            store,
        }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }
}

pub fn capture(connection: &mut Connection, key: &str, title: &str) {
    CaptureRepository::new(connection)
        .unwrap()
        .capture(
            CaptureRequest {
                idempotency_key: key.into(),
                input: CaptureInput {
                    requester: RequesterId::new("ALICE").unwrap(),
                    project: ProjectId::new("bif").unwrap(),
                    content: ItemContent::new(title, None, vec!["done".into()]).unwrap(),
                    provenance: Provenance::default(),
                },
            },
            key.into(),
            Actor {
                kind: ActorKind::Human,
                id: "ALICE",
                surface: "cli",
                host: "local",
            },
            Execution::Direct {
                surface: "cli",
                host: "local",
            },
            Timestamp::new("now"),
            CaptureIdentity {
                operation_id: key.into(),
                event_id: key.into(),
            },
        )
        .unwrap();
}

/// Full persisted logical values, excluding SQLite's backup-managed schema cookie.
pub fn inventory(connection: &Connection) -> Vec<(String, Vec<Vec<Value>>)> {
    let mut names = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap();
    let names = names
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut result = vec![];
    for name in names {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"))
            .unwrap();
        let count = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..count)
                    .map(|index| row.get(index))
                    .collect::<Result<Vec<Value>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        result.push((name, rows));
    }
    result
}

pub fn read(path: &Path) -> Connection {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

#[allow(dead_code)]
pub fn generation(connection: &Connection) -> String {
    connection
        .query_row("SELECT generation FROM journal_metadata", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[allow(dead_code)]
pub fn schema(connection: &Connection) -> Vec<(String, String, String, Option<String>)> {
    connection
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
