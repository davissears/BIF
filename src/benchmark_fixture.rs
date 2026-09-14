//! Canonical logical identity for generated benchmark fixtures.

use std::collections::BTreeMap;

use rusqlite::Connection;

pub const FORMAT: &str = "bif-v2-benchmark-store-v1";
pub const DIGEST_ALGORITHM: &str = "fnv1a64-framed-canonical-tables-v2";

pub struct Summary {
    pub digest: u64,
    pub row_counts: BTreeMap<String, u64>,
    pub distributions: BTreeMap<String, BTreeMap<String, u64>>,
}

pub fn summarize(connection: &Connection) -> rusqlite::Result<Summary> {
    const TABLES: [&str; 8] = [
        "store_metadata",
        "projects",
        "requester_project_counters",
        "items",
        "item_acceptance_criteria",
        "item_provenance",
        "operations",
        "events",
    ];
    let mut digest = 0xcbf29ce484222325_u64;
    for table in TABLES {
        let columns: Vec<String> = connection
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        let expression = columns
            .iter()
            .map(|column| format!("t.\"{}\"", column.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT json_array({expression}) FROM {table} t ORDER BY {expression}");
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        hash(&mut digest, table.as_bytes());
        for row in rows {
            hash(&mut digest, row?.as_bytes());
        }
    }
    let mut row_counts = BTreeMap::new();
    for table in TABLES {
        let count: i64 =
            connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        row_counts.insert(table.into(), count as u64);
    }
    let mut distributions = BTreeMap::new();
    for (name, expression) in [
        ("projects", "project_id"),
        ("statuses", "status"),
        ("priorities", "coalesce(priority, 'null')"),
        ("assignees", "coalesce(assignee, 'null')"),
    ] {
        let mut values = BTreeMap::new();
        let mut statement = connection.prepare(&format!(
            "SELECT {expression}, count(*) FROM items GROUP BY {expression} ORDER BY 1"
        ))?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (key, count) = row?;
            values.insert(key, count as u64);
        }
        distributions.insert(name.into(), values);
    }
    let mut sparse_markers = BTreeMap::new();
    for (name, predicate) in [
        ("present", "context_excerpt = 'sparse-marker'"),
        ("absent", "context_excerpt IS NULL"),
    ] {
        let count: i64 = connection.query_row(
            &format!("SELECT count(*) FROM item_provenance WHERE {predicate}"),
            [],
            |row| row.get(0),
        )?;
        sparse_markers.insert(name.into(), count as u64);
    }
    distributions.insert("sparse_markers".into(), sparse_markers);
    Ok(Summary {
        digest,
        row_counts,
        distributions,
    })
}

fn hash(state: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *state ^= u64::from(*byte);
        *state = state.wrapping_mul(0x100000001b3);
    }
    *state ^= 0xff;
}
