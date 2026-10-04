//! Bounded append-only history selection; payload decoding never sees lookahead.

use rusqlite::{Connection, params_from_iter, types::Value};

use crate::application::{
    HistoryPageRequest, ItemHistoryPageStore, ItemHistoryStoreError, ReadPage,
};

use super::{
    ItemHistoryRepository, ItemHistoryStorageError, ReadSnapshot, history_event, history_sqlite,
    read_history_row,
};

struct HistoryQuery {
    sql: String,
    parameters: Vec<Value>,
}

/// The existing unique (item_id, item_revision, event_index) index supports this
/// ascending row-value seek. Unsigned keys above SQLite's range never narrow.
fn selection(request: &HistoryPageRequest) -> HistoryQuery {
    let mut parameters = vec![Value::Text(request.item_id.to_string())];
    let boundary = match request.page.after {
        None => String::new(),
        Some(after) => match i64::try_from(after.item_revision.get()) {
            Err(_) => " AND 0".to_owned(),
            Ok(revision) => {
                parameters.push(Value::Integer(revision));
                match i64::try_from(after.event_index) {
                    Ok(index) => {
                        parameters.push(Value::Integer(index));
                        " AND (item_revision, event_index) > (?2, ?3)".to_owned()
                    }
                    Err(_) => " AND item_revision > ?2".to_owned(),
                }
            }
        },
    };
    parameters.push(Value::Integer(request.page.row_limit() as i64));
    HistoryQuery {
        sql: format!(
            "SELECT operation_id, event_id, item_revision, event_index, event_type,
                    before_value, after_value, actor_kind, actor_id, actor_surface,
                    actor_host, execution_kind, execution_agent_id, execution_surface,
                    execution_host, reason, note, occurred_at, event_schema_version
             FROM events WHERE item_id = ?1{boundary} \
             ORDER BY item_revision ASC, event_index ASC LIMIT ?{}",
            parameters.len()
        ),
        parameters,
    }
}

impl ItemHistoryRepository<'_> {
    /// Developer evidence: explain the actual history primary-row selection.
    #[doc(hidden)]
    pub fn explain_history_page(
        &self,
        request: &HistoryPageRequest,
    ) -> Result<Vec<String>, ItemHistoryStorageError> {
        let query = selection(request);
        explain(self.connection, query).map_err(ItemHistoryStorageError::Sqlite)
    }
}

fn explain(connection: &Connection, query: HistoryQuery) -> rusqlite::Result<Vec<String>> {
    connection
        .prepare(&format!("EXPLAIN QUERY PLAN {}", query.sql))?
        .query_map(params_from_iter(query.parameters), |row| row.get(3))?
        .collect()
}

impl ItemHistoryPageStore for ItemHistoryRepository<'_> {
    type Error = ItemHistoryStorageError;

    fn select_history_page(
        &self,
        request: &HistoryPageRequest,
    ) -> Result<ReadPage<crate::application::ItemHistoryEvent>, ItemHistoryStoreError<Self::Error>>
    {
        let transaction = ReadSnapshot::begin(self.connection).map_err(history_sqlite)?;
        let exists = transaction
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM items WHERE item_id = ?1)")
            .map_err(history_sqlite)?
            .query_row([request.item_id.to_string()], |row| row.get::<_, bool>(0))
            .map_err(history_sqlite)?;
        if !exists {
            return Err(ItemHistoryStoreError::NotFound);
        }
        let query = selection(request);
        let mut records = Vec::with_capacity(request.page.limit.get());
        let has_more = {
            let mut statement = transaction
                .prepare_cached(&query.sql)
                .map_err(history_sqlite)?;
            let mut rows = statement
                .query(params_from_iter(query.parameters))
                .map_err(history_sqlite)?;
            while records.len() < request.page.limit.get() {
                let Some(row) = rows.next().map_err(history_sqlite)? else {
                    break;
                };
                records.push(history_event(
                    read_history_row(row).map_err(history_sqlite)?,
                )?);
            }
            // Step lookahead only: no getters, allocations, or typed decoding
            // for the sentinel, even when its payload has malformed SQL types.
            rows.next().map_err(history_sqlite)?.is_some()
        };
        transaction.commit().map_err(history_sqlite)?;
        Ok(ReadPage { records, has_more })
    }
}
