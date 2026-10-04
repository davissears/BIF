//! Bounded read ports alongside the unchanged v1 complete-item ports.
//!
//! These are application/storage contracts, not wire results. View expansion,
//! effective-filter normalization, sort comparisons, and cursor codecs are
//! separate follow-up work; decoded keys here only preserve their required data.

use std::{error::Error, fmt};

use crate::domain::{ItemId, NamedView, Priority, RequesterId, Revision, Timestamp};

use super::{
    AuthorizationRequest, Command, InvalidPagination, ItemAudit, ItemHistoryError,
    ItemHistoryEvent, ItemHistoryStoreError, ItemListFilters, ItemListOrdering, ItemSummary,
    ItemWork, PageSize, ReadItemError, Unauthorized, authorize,
};

/// The requested complete current-state projection, independent of transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemProjectionKind {
    Summary,
    Work,
    Audit,
}

/// A complete replacement of exactly one requested projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ItemProjection {
    Summary(ItemSummary),
    Work(ItemWork),
    Audit(ItemAudit),
}

/// Validated page size and an optional exclusive, decoded keyset boundary.
///
/// Opaque cursor authentication/binding must happen before constructing a
/// continuation request. An absent boundary starts at the beginning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadPageRequest<Key> {
    pub limit: PageSize,
    pub after: Option<Key>,
}

impl<Key> ReadPageRequest<Key> {
    pub fn new(limit: usize, after: Option<Key>) -> Result<Self, InvalidPagination> {
        Ok(Self {
            limit: PageSize::new(limit)?,
            after,
        })
    }

    /// Maximum primary rows selected, including one lookahead, before hydration.
    pub const fn row_limit(&self) -> usize {
        self.limit.get() + 1
    }
}

/// Bounded, ordered complete records with primary-row lookahead metadata.
///
/// Select at most the request's `row_limit()` primary rows, derive `has_more`
/// from whether selection exceeded `limit`, and strip that sentinel before
/// hydrating child data. Return at most `limit` complete records, never a
/// hydrated lookahead record. Later response assembly may further reduce this
/// page for its byte budget and must use the last *emitted* key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadPage<Record> {
    pub records: Vec<Record>,
    /// True exactly when primary-row selection found a row beyond the limit.
    pub has_more: bool,
}

/// Internal sort metadata shared by list/next rows and their exclusive boundary.
///
/// Summary/work do not expose captured time, but continuation still needs it.
/// `id` retains canonical requester/project and a numeric u64 sequence; it must
/// never be replaced by lexical display-ID comparison. Priority is used by next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemReadKey {
    pub id: ItemId,
    pub captured_at: Timestamp,
    pub priority: Option<Priority>,
}

/// Projection payload and continuation metadata from the same storage snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedItemRow {
    pub item: ItemProjection,
    pub key: ItemReadKey,
}

/// Single-item reads have no pagination or ordering input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionGetRequest {
    pub item_id: ItemId,
    pub projection: ItemProjectionKind,
}

/// Explicit scope, projection, ordering, and bound for list/next selection.
///
/// View and filters are intersected, as in v1. Keep the configured requester
/// explicit for `mine`. V2-009 will provide their normalized effective form;
/// these fields are not yet a canonical query fingerprint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemProjectionPageRequest {
    pub view: NamedView,
    pub configured_requester: RequesterId,
    pub filters: ItemListFilters,
    pub projection: ItemProjectionKind,
    pub ordering: ItemListOrdering,
    pub page: ReadPageRequest<ItemReadKey>,
}

/// Required direct projection reads; no full-item loader or default fallback.
///
/// Every returned projection must match the request, and every row's key must
/// describe its payload. Pages must apply scope/filters, ordering, the exclusive
/// boundary, and `page.row_limit()` in storage. Derive `has_more` from those
/// primary rows and strip the sentinel *before* hydrating child data; return
/// at most `page.limit` complete projections under the `ReadPage` contract.
/// Never materialize the complete matching collection and then slice it.
pub trait ItemProjectionStore {
    type Error;

    fn read_projection(
        &self,
        request: &ProjectionGetRequest,
    ) -> Result<Option<ItemProjection>, Self::Error>;

    fn select_projection_page(
        &self,
        request: &ItemProjectionPageRequest,
    ) -> Result<ReadPage<ProjectedItemRow>, Self::Error>;
}

/// Stable invalid filter combination for projection page selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidProjectionFilters;

impl fmt::Display for InvalidProjectionFilters {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("assignee and unassigned cannot both be specified")
    }
}

impl Error for InvalidProjectionFilters {}

/// Application failures; delivery adapters map these to their error contracts.
#[derive(Debug)]
pub enum ProjectionPageError<StorageError> {
    InvalidFilters(InvalidProjectionFilters),
    Unauthorized(Unauthorized),
    Storage(StorageError),
}

impl<StorageError: fmt::Display> fmt::Display for ProjectionPageError<StorageError> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFilters(error) => error.fmt(formatter),
            Self::Unauthorized(error) => error.fmt(formatter),
            Self::Storage(error) => write!(formatter, "projection storage error: {error}"),
        }
    }
}

impl<StorageError: Error + 'static> Error for ProjectionPageError<StorageError> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidFilters(error) => Some(error),
            Self::Unauthorized(error) => Some(error),
            Self::Storage(error) => Some(error),
        }
    }
}

/// Authorizes a single get without requiring a complete-item storage API.
pub fn read_item_projection<S: ItemProjectionStore>(
    store: &S,
    authorization: &AuthorizationRequest<'_>,
    request: &ProjectionGetRequest,
) -> Result<ItemProjection, ReadItemError<S::Error>> {
    authorize_read(authorization).map_err(ReadItemError::Unauthorized)?;
    store
        .read_projection(request)
        .map_err(ReadItemError::Storage)?
        .ok_or(ReadItemError::NotFound)
}

/// Validates and authorizes bounded selection, including every continuation.
pub fn read_item_projection_page<S: ItemProjectionStore>(
    store: &S,
    authorization: &AuthorizationRequest<'_>,
    request: &ItemProjectionPageRequest,
) -> Result<ReadPage<ProjectedItemRow>, ProjectionPageError<S::Error>> {
    if request.filters.assignee.is_some() && request.filters.unassigned {
        return Err(ProjectionPageError::InvalidFilters(
            InvalidProjectionFilters,
        ));
    }
    authorize_read(authorization).map_err(ProjectionPageError::Unauthorized)?;
    store
        .select_projection_page(request)
        .map_err(ProjectionPageError::Storage)
}

/// Exclusive history boundary; both coordinates remain full-width unsigned data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryReadKey {
    pub item_revision: Revision,
    pub event_index: u64,
}

impl From<&ItemHistoryEvent> for HistoryReadKey {
    fn from(event: &ItemHistoryEvent) -> Self {
        Self {
            item_revision: event.item_revision,
            event_index: event.event_index,
        }
    }
}

/// The canonical history order, explicitly passed to the storage adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryOrdering {
    RevisionThenEventIndex,
}

/// Bounded history input; item identity and order are explicit on every page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryPageRequest {
    pub item_id: ItemId,
    pub ordering: HistoryOrdering,
    pub page: ReadPageRequest<HistoryReadKey>,
}

/// Required bounded event selection, independent of v1's complete-history port.
///
/// Apply the exclusive boundary, ascending revision/event-index order, and
/// `page.row_limit()` in storage. Derive `has_more` from selected primary rows
/// and strip the sentinel before hydrating event payloads or child data. Return
/// at most `page.limit` complete typed events under the `ReadPage` contract.
/// A missing item is NotFound; an existing item without events is an empty
/// successful page with `has_more = false`.
pub trait ItemHistoryPageStore {
    type Error;

    fn select_history_page(
        &self,
        request: &HistoryPageRequest,
    ) -> Result<ReadPage<ItemHistoryEvent>, ItemHistoryStoreError<Self::Error>>;
}

/// Authorizes each bounded history read without invoking complete history.
pub fn read_item_history_page<S: ItemHistoryPageStore>(
    store: &S,
    authorization: &AuthorizationRequest<'_>,
    request: &HistoryPageRequest,
) -> Result<ReadPage<ItemHistoryEvent>, ItemHistoryError<S::Error>> {
    authorize_read(authorization).map_err(ItemHistoryError::Unauthorized)?;
    store
        .select_history_page(request)
        .map_err(|error| match error {
            ItemHistoryStoreError::NotFound => ItemHistoryError::NotFound,
            ItemHistoryStoreError::InvalidPersistedData(error) => {
                ItemHistoryError::InvalidPersistedData(error)
            }
            ItemHistoryStoreError::Storage(error) => ItemHistoryError::Storage(error),
        })
}

fn authorize_read(request: &AuthorizationRequest<'_>) -> Result<(), Unauthorized> {
    if request.command != Command::Read {
        return Err(Unauthorized);
    }
    authorize(request)
}
