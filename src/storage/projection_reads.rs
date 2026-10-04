//! Direct bounded current-state reads. Selection and child hydration share one
//! short SQLite snapshot; legacy offset pages reuse selection and batch loading.

use std::collections::HashMap;

use rusqlite::{Connection, Row, params_from_iter, types::Value};

use crate::{
    application::{
        ItemAudit, ItemListFilters, ItemListOrdering, ItemPage, ItemProjection, ItemProjectionKind,
        ItemProjectionPageRequest, ItemProjectionStore, ItemReadKey, ItemSummary, ItemWork,
        PageOffset, Pagination, ProjectedItemRow, ProjectionGetRequest, ReadPage,
        read_semantics::{
            AssigneeFilter, EffectiveItemFilters, ItemSortField, SortDirection, priority_rank,
        },
    },
    domain::{
        AssigneeId, Item, ItemId, MessageId, NamedView, ProjectId, Provenance, RepositoryReference,
        RequesterId, Revision, RevisionReference, SourceUrl, ThreadId, Timestamp,
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
        let page = primary_page(
            &transaction,
            &query,
            request.projection,
            request.page.limit.get(),
        )?;
        let records = hydrate(&transaction, page.records, request.projection)?;
        transaction.commit()?;
        Ok(ReadPage {
            records,
            has_more: page.has_more,
        })
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
            if equal_prefix.is_empty() {
                // Give SQLite one contiguous leading-index range, then apply
                // the strict mixed-direction predicate within that range.
                // Otherwise its MULTI-INDEX OR plan can gather/sort the entire
                // remaining queue before LIMIT rather than walk index order.
                predicates.push(format!("{column} {operator}= {parameter}"));
            }
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

/// Both page APIs decode only returned payloads. Step the SQL limit+1 sentinel
/// solely for existence, without getters, typed allocation or child hydration.
fn primary_page(
    connection: &Connection,
    query: &Query,
    kind: ItemProjectionKind,
    limit: usize,
) -> Result<ReadPage<PrimaryRow>, ItemStorageError> {
    let mut statement = connection.prepare(&query.sql)?;
    let mut rows = statement.query(params_from_iter(&query.parameters))?;
    let mut records = Vec::new();
    while records.len() < limit {
        let Some(row) = rows.next()? else {
            return Ok(ReadPage {
                records,
                has_more: false,
            });
        };
        records.push(PrimaryRow::read(row, kind)?);
    }
    // Keep row-step failures visible even when stepping only the sentinel.
    let has_more = rows.next()?.is_some();
    Ok(ReadPage { records, has_more })
}

fn hydrate(
    connection: &Connection,
    rows: Vec<PrimaryRow>,
    kind: ItemProjectionKind,
) -> Result<Vec<ProjectedItemRow>, ItemStorageError> {
    let mut criteria = if kind == ItemProjectionKind::Summary {
        HashMap::new()
    } else {
        load_criteria(connection, &rows)?
    };
    rows.into_iter()
        .map(|row| {
            let children = criteria.remove(&row.item_id).unwrap_or_default();
            row.project(kind, children)
        })
        .collect()
}

/// One indexed child query for the selected page, never for the sentinel.
fn load_criteria(
    connection: &Connection,
    rows: &[PrimaryRow],
) -> Result<HashMap<String, Vec<String>>, ItemStorageError> {
    let mut criteria = HashMap::<String, Vec<String>>::new();
    if !rows.is_empty() {
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
    Ok(criteria)
}

/// v1 orders decoded identity, not the potentially noncanonical raw columns.
/// The legacy decoder requires reconstructed id.to_string() == item_id, so
/// its colon-delimited segments are canonical without duplicating normalization.
fn legacy_coordinate(field: ItemSortField) -> &'static str {
    match field {
        ItemSortField::Requester => {
            "substr(i.item_id, 1, instr(i.item_id, ':') - 1) COLLATE BINARY"
        }
        ItemSortField::Project => {
            "substr(i.item_id, instr(i.item_id, ':') + 1, \
             instr(substr(i.item_id, instr(i.item_id, ':') + 1), ':') - 1) COLLATE BINARY"
        }
        // Sequence must remain numeric: display-ID order fails at 999/1000,
        // as well as when one requester/project name is another's prefix.
        _ => coordinate(field),
    }
}

/// Legacy-only query semantics mirror ItemRepository::select_items: raw
/// membership/filter predicates, then decoded identity order before SQL paging.
/// Canonical tie expressions can require sorting within leading index groups;
/// they do not change v2's canonical indexed keyset query.
fn legacy_page_query(
    view: NamedView,
    configured_requester: &RequesterId,
    filters: &ItemListFilters,
    ordering: ItemListOrdering,
    pagination: Pagination,
    offset: i64,
) -> Query {
    let mut query = Query {
        sql: select(ItemProjectionKind::Audit),
        parameters: Vec::new(),
    };
    let predicate = match view {
        NamedView::Proposed => "i.status = 'proposed'".to_owned(),
        NamedView::Ready => "i.status = 'ready'".to_owned(),
        NamedView::Active => "i.status IN ('in_progress', 'blocked')".to_owned(),
        NamedView::Blocked => "i.status = 'blocked'".to_owned(),
        NamedView::Done => "i.status = 'done'".to_owned(),
        NamedView::Rejected => "i.status = 'rejected'".to_owned(),
        NamedView::Mine => {
            let assignee = query.bind(configured_requester.as_str().to_ascii_lowercase());
            // Unknown nonterminal status must reach the legacy decoder and
            // fail, not silently disappear through a closed-enum IN predicate.
            format!("i.status NOT IN ('done', 'rejected') AND i.assignee = {assignee}")
        }
        NamedView::All => "1 = 1".to_owned(),
    };
    let mut predicates = vec![predicate];
    for (column, value) in [
        (
            "i.project_id",
            filters.project.as_ref().map(ToString::to_string),
        ),
        (
            "i.requester",
            filters.requester.as_ref().map(ToString::to_string),
        ),
        (
            "i.assignee",
            filters.assignee.as_ref().map(ToString::to_string),
        ),
        (
            "i.status",
            filters
                .status
                .map(|status| super::status(status).to_owned()),
        ),
        (
            "i.priority",
            filters
                .priority
                .map(|priority| super::priority(priority).to_owned()),
        ),
    ] {
        if let Some(value) = value {
            let parameter = query.bind(value);
            predicates.push(format!("{column} = {parameter}"));
        }
    }
    if filters.unassigned {
        predicates.push("i.assignee IS NULL".to_owned());
    }
    if let Some(text) = &filters.text {
        let parameter = query.bind(text.as_str().to_owned());
        predicates.push(format!(
            "(instr(lower(i.title), lower({parameter})) > 0 \
              OR instr(lower(coalesce(i.description, '')), lower({parameter})) > 0 \
              OR EXISTS (SELECT 1 FROM item_acceptance_criteria AS c \
                         WHERE c.item_id = i.item_id \
                         AND instr(lower(c.criterion), lower({parameter})) > 0))"
        ));
    }
    let order = ordering
        .sort_spec()
        .iter()
        .map(|term| {
            format!(
                "{} {}",
                legacy_coordinate(term.field),
                match term.direction {
                    SortDirection::Ascending => "ASC",
                    SortDirection::Descending => "DESC",
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let limit = query
        .bind(i64::try_from(pagination.limit.get() + 1).expect("validated page size fits SQLite"));
    let offset = query.bind(offset);
    query.sql.push_str(&format!(
        " WHERE {} ORDER BY {order} LIMIT {limit} OFFSET {offset}",
        predicates.join(" AND ")
    ));
    query
}

/// SQL-bound v1 offset pages retain complete Items and the legacy decoder.
pub(super) fn legacy_page(
    connection: &Connection,
    view: NamedView,
    configured_requester: &RequesterId,
    filters: &ItemListFilters,
    ordering: ItemListOrdering,
    pagination: Pagination,
) -> Result<ItemPage, ItemStorageError> {
    // The old usize iterator skip returned an empty page above SQLite's signed
    // range. Preserve that behavior rather than narrowing or rejecting offsets.
    let Ok(offset) = i64::try_from(pagination.offset.get()) else {
        return Ok(ItemPage {
            items: Vec::new(),
            next_offset: None,
        });
    };
    let query = legacy_page_query(
        view,
        configured_requester,
        filters,
        ordering,
        pagination,
        offset,
    );
    let transaction = connection.unchecked_transaction()?;
    let page = primary_page(
        &transaction,
        &query,
        ItemProjectionKind::Audit,
        pagination.limit.get(),
    )?;
    let mut criteria = load_criteria(&transaction, &page.records)?;
    let items = page
        .records
        .into_iter()
        .map(|row| {
            let children = criteria.remove(&row.item_id).unwrap_or_default();
            row.legacy_item(children)
        })
        .collect::<Result<_, _>>()?;
    transaction.commit()?;
    let next_offset = page
        .has_more
        .then(|| PageOffset::new(pagination.offset.get() + pagination.limit.get()));
    Ok(ItemPage { items, next_offset })
}

impl PrimaryRow {
    fn legacy_item(self, criteria: Vec<String>) -> Result<Item, ItemStorageError> {
        let audit = self.audit.expect("legacy selection requests audit columns");
        if audit.provenance_id.is_none() {
            return Err(invalid_item(
                &self.item_id,
                "item disappeared during named view selection",
            ));
        }
        super::decode_item(
            &self.item_id,
            (
                self.requester,
                self.project,
                self.sequence,
                self.title,
                self.description,
                self.status,
                self.priority,
                self.assignee,
                self.status_reason,
                self.revision,
                self.captured_at,
                audit.updated_at,
                audit.source_host,
                audit.thread_id,
                audit.message_id,
                audit.url,
                audit.repository_reference,
                audit.revision_reference,
                audit.context_excerpt,
            ),
            criteria,
        )
    }
}
