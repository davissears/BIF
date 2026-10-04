//! Direct bounded current-state reads. Selection and child hydration share one
//! short SQLite snapshot; v1's complete-item loaders remain independent.

use std::collections::HashMap;

use rusqlite::{Connection, Row, params_from_iter, types::Value};

use crate::{
    application::{
        ItemAudit, ItemProjection, ItemProjectionKind, ItemProjectionPageRequest,
        ItemProjectionStore, ItemReadKey, ItemSummary, ItemWork, ProjectedItemRow,
        ProjectionGetRequest, ReadPage,
        read_semantics::{
            AssigneeFilter, EffectiveItemFilters, ItemSortField, SortDirection, priority_rank,
        },
    },
    domain::{
        AssigneeId, ItemId, MessageId, ProjectId, Provenance, RepositoryReference, RequesterId,
        Revision, RevisionReference, SourceUrl, ThreadId, Timestamp,
    },
};

use super::{ItemStorageError, invalid_item, parse_priority, parse_source_host, parse_status};

/// SQLite adapter for summary/work/audit reads; never reconstructs a full Item.
pub struct ProjectionRepository<'connection> {
    connection: &'connection Connection,
}

impl<'connection> ProjectionRepository<'connection> {
    pub fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }

    /// Developer evidence: explain the exact primary query used by page reads.
    /// No schema change, hydration, or alternate query implementation is used.
    #[doc(hidden)]
    pub fn explain_projection_page(
        &self,
        request: &ItemProjectionPageRequest,
    ) -> Result<Vec<String>, ItemStorageError> {
        let query = page_query(request)?;
        let mut statement = self
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {}", query.sql))?;
        Ok(statement
            .query_map(params_from_iter(query.parameters), |row| row.get(3))?
            .collect::<rusqlite::Result<_>>()?)
    }
}

impl ItemProjectionStore for ProjectionRepository<'_> {
    type Error = ItemStorageError;

    fn read_projection(
        &self,
        request: &ProjectionGetRequest,
    ) -> Result<Option<ItemProjection>, Self::Error> {
        let transaction = self.connection.unchecked_transaction()?;
        let query = Query {
            sql: format!(
                "{} WHERE i.item_id = ?1 COLLATE BINARY LIMIT 1",
                select(request.projection)
            ),
            parameters: vec![Value::Text(request.item_id.to_string())],
        };
        let rows = primary_rows(&transaction, &query, request.projection)?;
        let result = hydrate(&transaction, rows, request.projection)?
            .pop()
            .map(|row| row.item);
        transaction.commit()?;
        Ok(result)
    }

    fn select_projection_page(
        &self,
        request: &ItemProjectionPageRequest,
    ) -> Result<ReadPage<ProjectedItemRow>, Self::Error> {
        // Normalize before opening the snapshot, including invalid combinations.
        let query = page_query(request)?;
        let transaction = self.connection.unchecked_transaction()?;
        let mut rows = primary_rows(&transaction, &query, request.projection)?;
        let has_more = rows.len() > request.page.limit.get();
        // Discard the lookahead before parsing payload or loading any children.
        rows.truncate(request.page.limit.get());
        let records = hydrate(&transaction, rows, request.projection)?;
        transaction.commit()?;
        Ok(ReadPage { records, has_more })
    }
}

// The priority CASE is deliberately stable for expression-index matching.
const PRIORITY_RANK: &str = "CASE i.priority WHEN 'P0' THEN 0 WHEN 'P1' THEN 1 WHEN 'P2' THEN 2 WHEN 'P3' THEN 3 WHEN 'P4' THEN 4 ELSE 5 END";
const SUMMARY_COLUMNS: &str = "i.item_id, i.requester, i.project_id, i.sequence, i.title, i.status, i.priority, i.assignee, i.revision, i.captured_at";
const WORK_COLUMNS: &str = "i.description, i.status_reason";
const AUDIT_COLUMNS: &str = "i.updated_at, p.item_id, p.source_host, p.thread_id, p.message_id, p.url, p.repository_reference, p.revision_reference, p.context_excerpt";

fn select(kind: ItemProjectionKind) -> String {
    match kind {
        ItemProjectionKind::Summary => format!("SELECT {SUMMARY_COLUMNS} FROM items AS i"),
        ItemProjectionKind::Work => {
            format!("SELECT {SUMMARY_COLUMNS}, {WORK_COLUMNS} FROM items AS i")
        }
        // LEFT JOIN distinguishes a missing item from corrupt missing provenance.
        ItemProjectionKind::Audit => format!(
            "SELECT {SUMMARY_COLUMNS}, {WORK_COLUMNS}, {AUDIT_COLUMNS} FROM items AS i \
             LEFT JOIN item_provenance AS p ON p.item_id = i.item_id"
        ),
    }
}

struct Query {
    sql: String,
    parameters: Vec<Value>,
}

impl Query {
    fn bind(&mut self, value: impl Into<Value>) -> String {
        self.parameters.push(value.into());
        format!("?{}", self.parameters.len())
    }
}

fn coordinate(field: ItemSortField) -> &'static str {
    match field {
        ItemSortField::PriorityRank => PRIORITY_RANK,
        ItemSortField::CapturedAt => "i.captured_at COLLATE BINARY",
        ItemSortField::Requester => "i.requester COLLATE BINARY",
        ItemSortField::Project => "i.project_id COLLATE BINARY",
        ItemSortField::Sequence => "i.sequence",
    }
}

fn page_query(request: &ItemProjectionPageRequest) -> Result<Query, ItemStorageError> {
    let filters = EffectiveItemFilters::from_request(request)
        .map_err(|error| invalid_item("<query>", &error.to_string()))?;
    let mut query = Query {
        sql: select(request.projection),
        parameters: Vec::new(),
    };
    let mut predicates = Vec::new();
    if filters.is_empty() {
        predicates.push("0".to_owned());
    } else if let [status] = filters.statuses() {
        predicates.push(format!("i.status = '{}'", super::status(*status)));
    } else if filters.statuses().len() != 6 {
        // These are trusted closed-enum values, not user input. Literal status
        // sets let SQLite prove applicability of future partial indexes.
        predicates.push(format!(
            "i.status IN ({})",
            filters
                .statuses()
                .iter()
                .map(|status| format!("'{}'", super::status(*status)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (column, value) in [
        ("i.project_id", filters.project().map(|id| id.as_str())),
        ("i.requester", filters.requester().map(|id| id.as_str())),
        ("i.priority", filters.priority().map(super::priority)),
    ] {
        if let Some(value) = value {
            let parameter = query.bind(value.to_owned());
            predicates.push(format!("{column} = {parameter} COLLATE BINARY"));
        }
    }
    match filters.assignee() {
        AssigneeFilter::Any => {}
        AssigneeFilter::Unassigned => predicates.push("i.assignee IS NULL".to_owned()),
        AssigneeFilter::Assigned(assignee) => {
            let parameter = query.bind(assignee.as_str().to_owned());
            predicates.push(format!("i.assignee = {parameter} COLLATE BINARY"));
        }
    }
    if let Some(text) = filters.text() {
        let parameter = query.bind(text.to_owned());
        // SQLite lower folds ASCII only; instr is literal (not LIKE wildcards).
        predicates.push(format!(
            "(instr(lower(i.title), {parameter}) > 0 \
              OR instr(lower(coalesce(i.description, '')), {parameter}) > 0 \
              OR EXISTS (SELECT 1 FROM item_acceptance_criteria AS c \
                         WHERE c.item_id = i.item_id AND instr(lower(c.criterion), {parameter}) > 0))"
        ));
    }
    if let Some(boundary) = &request.page.after {
        let mut equal_prefix = Vec::new();
        let mut alternatives = Vec::new();
        for term in request.ordering.sort_spec() {
            let column = coordinate(term.field);
            let value = match term.field {
                ItemSortField::PriorityRank => {
                    Value::Integer(i64::from(priority_rank(boundary.priority)))
                }
                ItemSortField::CapturedAt => Value::Text(boundary.captured_at.as_str().to_owned()),
                ItemSortField::Requester => {
                    Value::Text(boundary.id.requester().as_str().to_owned())
                }
                ItemSortField::Project => Value::Text(boundary.id.project().as_str().to_owned()),
                ItemSortField::Sequence => {
                    // Durable sequences are positive SQLite integers. For a u64
                    // boundary above their range none can be greater or equal.
                    // Do not narrow it or bind it as floating point/text.
                    match i64::try_from(boundary.id.sequence()) {
                        Ok(sequence) => Value::Integer(sequence),
                        Err(_) => {
                            if term.direction == SortDirection::Descending {
                                alternatives.push(if equal_prefix.is_empty() {
                                    "1".to_owned()
                                } else {
                                    equal_prefix.join(" AND ")
                                });
                            }
                            break;
                        }
                    }
                }
            };
            let parameter = query.bind(value);
            let operator = match term.direction {
                SortDirection::Ascending => ">",
                SortDirection::Descending => "<",
            };
            let mut branch = equal_prefix.clone();
            branch.push(format!("{column} {operator} {parameter}"));
            alternatives.push(format!("({})", branch.join(" AND ")));
            equal_prefix.push(format!("{column} = {parameter}"));
        }
        predicates.push(format!(
            "({})",
            if alternatives.is_empty() {
                "0".to_owned()
            } else {
                alternatives.join(" OR ")
            }
        ));
    }
    if predicates.is_empty() {
        predicates.push("1".to_owned());
    }
    let order = request
        .ordering
        .sort_spec()
        .iter()
        .map(|term| {
            format!(
                "{} {}",
                coordinate(term.field),
                match term.direction {
                    SortDirection::Ascending => "ASC",
                    SortDirection::Descending => "DESC",
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let limit = query
        .bind(i64::try_from(request.page.row_limit()).expect("validated page size fits SQLite"));
    query.sql.push_str(&format!(
        " WHERE {} ORDER BY {order} LIMIT {limit}",
        predicates.join(" AND ")
    ));
    Ok(query)
}

struct PrimaryRow {
    item_id: String,
    requester: String,
    project: String,
    sequence: i64,
    title: String,
    status: String,
    priority: Option<String>,
    assignee: Option<String>,
    revision: i64,
    captured_at: String,
    description: Option<String>,
    status_reason: Option<String>,
    audit: Option<AuditRow>,
}

struct AuditRow {
    updated_at: String,
    provenance_id: Option<String>,
    source_host: Option<String>,
    thread_id: Option<String>,
    message_id: Option<String>,
    url: Option<String>,
    repository_reference: Option<String>,
    revision_reference: Option<String>,
    context_excerpt: Option<String>,
}

impl PrimaryRow {
    fn read(row: &Row<'_>, kind: ItemProjectionKind) -> rusqlite::Result<Self> {
        Ok(Self {
            item_id: row.get(0)?,
            requester: row.get(1)?,
            project: row.get(2)?,
            sequence: row.get(3)?,
            title: row.get(4)?,
            status: row.get(5)?,
            priority: row.get(6)?,
            assignee: row.get(7)?,
            revision: row.get(8)?,
            captured_at: row.get(9)?,
            description: if kind == ItemProjectionKind::Summary {
                None
            } else {
                row.get(10)?
            },
            status_reason: if kind == ItemProjectionKind::Summary {
                None
            } else {
                row.get(11)?
            },
            audit: if kind == ItemProjectionKind::Audit {
                Some(AuditRow {
                    updated_at: row.get(12)?,
                    provenance_id: row.get(13)?,
                    source_host: row.get(14)?,
                    thread_id: row.get(15)?,
                    message_id: row.get(16)?,
                    url: row.get(17)?,
                    repository_reference: row.get(18)?,
                    revision_reference: row.get(19)?,
                    context_excerpt: row.get(20)?,
                })
            } else {
                None
            },
        })
    }

    fn project(
        self,
        kind: ItemProjectionKind,
        criteria: Vec<String>,
    ) -> Result<ProjectedItemRow, ItemStorageError> {
        let invalid = |detail: &str| invalid_item(&self.item_id, detail);
        let requester = RequesterId::new(&self.requester)
            .map_err(|_| invalid("requester is not a valid RequesterId"))?;
        let project = ProjectId::new(&self.project)
            .map_err(|_| invalid("project_id is not a valid ProjectId"))?;
        // Keys must retain the exact identity coordinates SQL compares. Silent
        // normalization of persisted identities can reorder or repeat pages.
        if requester.as_str() != self.requester {
            return Err(invalid("requester is not canonical"));
        }
        if project.as_str() != self.project {
            return Err(invalid("project_id is not canonical"));
        }
        let sequence = u64::try_from(self.sequence)
            .map_err(|_| invalid("sequence is not a positive integer"))?;
        let id = ItemId::new(requester, project, sequence)
            .map_err(|_| invalid("identity components do not form a valid ItemId"))?;
        if id.to_string() != self.item_id {
            return Err(invalid("item_id does not match its identity components"));
        }
        if self.title.is_empty() {
            return Err(invalid("content violates canonical item constraints"));
        }
        let status = parse_status(&self.status, &self.item_id)?;
        let priority = self
            .priority
            .as_deref()
            .map(|value| parse_priority(value, &self.item_id))
            .transpose()?;
        let assignee = self
            .assignee
            .map(AssigneeId::new)
            .transpose()
            .map_err(|_| invalid("assignee is not a valid AssigneeId"))?;
        let revision = Revision::new(
            u64::try_from(self.revision)
                .map_err(|_| invalid("revision is not a positive integer"))?,
        )
        .map_err(|_| invalid("revision is not a positive integer"))?;
        let captured_at = Timestamp::new(self.captured_at);
        let key = ItemReadKey {
            id: id.clone(),
            captured_at: captured_at.clone(),
            priority,
        };
        let item = match kind {
            ItemProjectionKind::Summary => ItemProjection::Summary(ItemSummary::new(
                id, self.title, status, priority, assignee, revision,
            )),
            ItemProjectionKind::Work => ItemProjection::Work(ItemWork::new(
                id,
                self.title,
                status,
                priority,
                assignee,
                revision,
                self.description,
                criteria,
                self.status_reason,
            )),
            ItemProjectionKind::Audit => {
                let audit = self.audit.expect("audit SQL always selects audit columns");
                if audit.provenance_id.is_none() {
                    return Err(invalid("item provenance row is missing"));
                }
                let provenance = Provenance::new(
                    audit
                        .source_host
                        .as_deref()
                        .map(|value| parse_source_host(value, &self.item_id))
                        .transpose()?,
                    audit.thread_id.map(ThreadId::new),
                    audit.message_id.map(MessageId::new),
                    audit.url.map(SourceUrl::new),
                    audit.repository_reference.map(RepositoryReference::new),
                    audit.revision_reference.map(RevisionReference::new),
                    audit.context_excerpt,
                );
                ItemProjection::Audit(ItemAudit::new(
                    id,
                    self.title,
                    status,
                    priority,
                    assignee,
                    revision,
                    self.description,
                    criteria,
                    self.status_reason,
                    captured_at,
                    Timestamp::new(audit.updated_at),
                    provenance,
                ))
            }
        };
        Ok(ProjectedItemRow { item, key })
    }
}

fn primary_rows(
    connection: &Connection,
    query: &Query,
    kind: ItemProjectionKind,
) -> Result<Vec<PrimaryRow>, ItemStorageError> {
    let mut statement = connection.prepare(&query.sql)?;
    Ok(statement
        .query_map(params_from_iter(&query.parameters), |row| {
            PrimaryRow::read(row, kind)
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn hydrate(
    connection: &Connection,
    rows: Vec<PrimaryRow>,
    kind: ItemProjectionKind,
) -> Result<Vec<ProjectedItemRow>, ItemStorageError> {
    let mut criteria = HashMap::<String, Vec<String>>::new();
    if kind != ItemProjectionKind::Summary && !rows.is_empty() {
        let placeholders = std::iter::repeat_n("?", rows.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut statement = connection.prepare(&format!(
            "SELECT item_id, criterion FROM item_acceptance_criteria \
             WHERE item_id IN ({placeholders}) ORDER BY item_id COLLATE BINARY, criterion_index"
        ))?;
        let mut children =
            statement.query(params_from_iter(rows.iter().map(|row| &row.item_id)))?;
        while let Some(child) = children.next()? {
            criteria
                .entry(child.get(0)?)
                .or_default()
                .push(child.get(1)?);
        }
    }
    rows.into_iter()
        .map(|row| {
            let children = criteria.remove(&row.item_id).unwrap_or_default();
            row.project(kind, children)
        })
        .collect()
}
