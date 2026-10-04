//! Typed v2 delivery serialization, independent of CLI dispatch and storage.
//!
//! Borrowed wire adapters keep JSON out of the application models. Counting and
//! encoding use the same serializer: even rejected records are counted exactly
//! without allocating an encoded copy. Only a complete, budgeted envelope is
//! buffered before output. The budget covers the JSON value, not its newline.

use std::{
    error::Error,
    fmt,
    io::{self, Write},
};

use serde::{
    Serialize, Serializer,
    ser::{SerializeSeq, SerializeStruct},
};

use crate::{
    application::{
        ActorKind, EventActor, EventExecution, ItemHistoryEvent, ItemProjection, ProjectedItemRow,
        ReadPage,
    },
    domain::{AssigneeId, EventType, EventValue, ItemId, Priority, Provenance, SourceHost, Status},
};

pub use crate::limits::MAXIMUM_RESPONSE_BYTES;
const MAXIMUM_PAGE_RECORDS: usize = 100;

/// A response may use a smaller delivery budget, but never exceed the contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResponseBudget(usize);

impl ResponseBudget {
    pub fn new(maximum_bytes: usize) -> Result<Self, ReadError> {
        if maximum_bytes > MAXIMUM_RESPONSE_BYTES {
            return Err(ReadError::InvalidInput);
        }
        Ok(Self(maximum_bytes))
    }

    pub const fn maximum_bytes(self) -> usize {
        self.0
    }
}

impl Default for ResponseBudget {
    fn default() -> Self {
        Self(MAXIMUM_RESPONSE_BYTES)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadErrorCode {
    InvalidInput,
    NotFound,
    Unauthorized,
    UnsupportedVersion,
    NotInitialized,
    StorageBusy,
    InvalidCursor,
    PayloadTooLarge,
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Item,
    Event,
}

/// Exact stable details for a complete record that cannot fit an empty page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PayloadTooLarge {
    pub record_kind: RecordKind,
    pub record_id: String,
    pub maximum_response_bytes: usize,
    pub minimum_required_bytes: usize,
}

/// Transport-stable failures, with details constrained by the frozen schema.
///
/// Application/storage error classification remains the caller's responsibility.
/// Messages are fixed diagnostics, not a matching API; they are always nonempty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadError {
    InvalidInput,
    NotFound,
    Unauthorized,
    UnsupportedVersion,
    NotInitialized,
    StorageBusy,
    InvalidCursor {
        reason: String,
        restart_required: bool,
    },
    PayloadTooLarge(PayloadTooLarge),
    Internal,
}

impl ReadError {
    pub const fn code(&self) -> ReadErrorCode {
        match self {
            Self::InvalidInput => ReadErrorCode::InvalidInput,
            Self::NotFound => ReadErrorCode::NotFound,
            Self::Unauthorized => ReadErrorCode::Unauthorized,
            Self::UnsupportedVersion => ReadErrorCode::UnsupportedVersion,
            Self::NotInitialized => ReadErrorCode::NotInitialized,
            Self::StorageBusy => ReadErrorCode::StorageBusy,
            Self::InvalidCursor { .. } => ReadErrorCode::InvalidCursor,
            Self::PayloadTooLarge(_) => ReadErrorCode::PayloadTooLarge,
            Self::Internal => ReadErrorCode::Internal,
        }
    }

    pub const fn message(&self) -> &'static str {
        match self {
            Self::InvalidInput => "The read request is invalid",
            Self::NotFound => "The item was not found",
            Self::Unauthorized => "The read request is not authorized",
            Self::UnsupportedVersion => "The API version is not supported",
            Self::NotInitialized => "The store is not initialized",
            Self::StorageBusy => "The store is busy",
            Self::InvalidCursor { .. } => "The continuation cursor is invalid",
            Self::PayloadTooLarge(details) => match details.record_kind {
                RecordKind::Item => "A complete item exceeds the maximum response size",
                RecordKind::Event => "A complete event exceeds the maximum response size",
            },
            Self::Internal => "The read could not be completed",
        }
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl Error for ReadError {}

impl Serialize for ReadError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct EmptyDetails {}
        #[derive(Serialize)]
        struct CursorDetails<'a> {
            reason: &'a str,
            restart_required: bool,
        }
        let mut object = serializer.serialize_struct("ReadError", 3)?;
        object.serialize_field("code", &self.code())?;
        object.serialize_field("message", self.message())?;
        match self {
            Self::InvalidCursor {
                reason,
                restart_required,
            } => {
                object.serialize_field(
                    "details",
                    &CursorDetails {
                        reason,
                        restart_required: *restart_required,
                    },
                )?;
            }
            Self::PayloadTooLarge(details) => object.serialize_field("details", details)?,
            _ => object.serialize_field("details", &EmptyDetails {})?,
        }
        object.end()
    }
}

/// Local encoding/output failures are not silently turned into success values.
#[derive(Debug)]
pub enum EncodeError {
    Read(ReadError),
    /// Used only when an empty envelope or error metadata itself cannot fit.
    EnvelopeTooLarge {
        maximum_response_bytes: usize,
        minimum_required_bytes: usize,
    },
    Serialization(serde_json::Error),
    Write(io::Error),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => error.fmt(formatter),
            Self::EnvelopeTooLarge {
                maximum_response_bytes,
                minimum_required_bytes,
            } => {
                write!(
                    formatter,
                    "envelope requires {minimum_required_bytes} bytes; budget is {maximum_response_bytes}"
                )
            }
            Self::Serialization(error) => error.fmt(formatter),
            Self::Write(error) => error.fmt(formatter),
        }
    }
}

impl Error for EncodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::EnvelopeTooLarge { .. } => None,
            Self::Serialization(error) => Some(error),
            Self::Write(error) => Some(error),
        }
    }
}

/// Bytes exclude newline framing; records count only complete emitted records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResponseStats {
    pub json_bytes: usize,
    pub emitted_records: usize,
}

#[derive(Serialize)]
struct Success<T> {
    api_version: u8,
    schema_version: u8,
    ok: bool,
    result: T,
}

fn success<T>(result: T) -> Success<T> {
    Success {
        api_version: 2,
        schema_version: 1,
        ok: true,
        result,
    }
}

#[derive(Serialize)]
struct Get<'a> {
    item: ProjectionWire<'a>,
}

/// Write one complete get success, or return a typed oversized-record failure.
/// On validation/counting failure, the output writer is never touched.
pub fn write_get<W: Write>(
    output: &mut W,
    item: &ItemProjection,
    budget: ResponseBudget,
) -> Result<ResponseStats, EncodeError> {
    let envelope = success(Get {
        item: ProjectionWire(item),
    });
    let bytes = encoded_len(&envelope)?;
    if bytes > budget.0 {
        return Err(oversized(
            RecordKind::Item,
            projection_id(item).to_string(),
            budget,
            bytes,
        ));
    }
    emit(output, &envelope, bytes, 1)
}

/// Write a list/next page in storage order without exposing internal sort keys.
///
/// `records` must already contain at most the requested limit (and at most 100)
/// complete rows. `has_more` is sentinel metadata, not another hydrated row.
/// Byte-budget omission also creates a continuation even when `has_more` is
/// false. Cursor construction is caller-owned, pure transport work: it must not
/// read storage, publish continuation state, or retain a read transaction.
/// It may be called for candidate prefixes that ultimately are not emitted.
pub fn write_item_page<W: Write, F: FnMut(&ProjectedItemRow) -> Result<String, ReadError>>(
    output: &mut W,
    page: &ReadPage<ProjectedItemRow>,
    budget: ResponseBudget,
    mut cursor: F,
) -> Result<ResponseStats, EncodeError> {
    write_item_page_candidates(output, page, budget, |row| {
        cursor(row).map(CursorCandidate::Token)
    })
}

/// A generated continuation candidate, including exact sizes without a token.
#[derive(Debug)]
pub enum CursorCandidate {
    Token(String),
    /// Complete unescaped ASCII token bytes (the hex codec needs only quotes
    /// in JSON). This boundary cannot be emitted, even under a larger budget.
    Oversized {
        encoded_token_bytes: usize,
    },
}

/// Candidate-aware list/next delivery, sharing the String callback serializer.
pub fn write_item_page_candidates<
    W: Write,
    F: FnMut(&ProjectedItemRow) -> Result<CursorCandidate, ReadError>,
>(
    output: &mut W,
    page: &ReadPage<ProjectedItemRow>,
    budget: ResponseBudget,
    cursor: F,
) -> Result<ResponseStats, EncodeError> {
    if page
        .records
        .windows(2)
        .any(|pair| std::mem::discriminant(&pair[0].item) != std::mem::discriminant(&pair[1].item))
    {
        return Err(EncodeError::Read(ReadError::Internal));
    }
    write_page(output, None, page, budget, cursor)
}

/// Write bounded complete history events; the caller binds cursors to this item.
pub fn write_history_page<W: Write, F: FnMut(&ItemHistoryEvent) -> Result<String, ReadError>>(
    output: &mut W,
    item_id: &ItemId,
    page: &ReadPage<ItemHistoryEvent>,
    budget: ResponseBudget,
    mut cursor: F,
) -> Result<ResponseStats, EncodeError> {
    write_history_page_candidates(output, item_id, page, budget, |event| {
        cursor(event).map(CursorCandidate::Token)
    })
}

/// Candidate-aware history delivery through the same bounded page core.
pub fn write_history_page_candidates<
    W: Write,
    F: FnMut(&ItemHistoryEvent) -> Result<CursorCandidate, ReadError>,
>(
    output: &mut W,
    item_id: &ItemId,
    page: &ReadPage<ItemHistoryEvent>,
    budget: ResponseBudget,
    cursor: F,
) -> Result<ResponseStats, EncodeError> {
    write_page(output, Some(item_id), page, budget, cursor)
}

/// Error delivery is explicit: success writers never write an error themselves.
pub fn write_error<W: Write>(
    output: &mut W,
    error: &ReadError,
    budget: ResponseBudget,
) -> Result<ResponseStats, EncodeError> {
    #[derive(Serialize)]
    struct Failure<'a> {
        api_version: u8,
        schema_version: u8,
        ok: bool,
        error: &'a ReadError,
    }
    let envelope = Failure {
        api_version: 2,
        schema_version: 1,
        ok: false,
        error,
    };
    let bytes = encoded_len(&envelope)?;
    ensure_envelope_fits(bytes, budget)?;
    emit(output, &envelope, bytes, 0)
}

/// Page shape is shared so counting and final serialization cannot drift.
struct PageWire<'a, R> {
    item_id: Option<&'a ItemId>,
    records: &'a [R],
    next_cursor: Option<&'a str>,
}

impl<R: WireRecord> Serialize for PageWire<'_, R> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object =
            serializer.serialize_struct("Page", if self.item_id.is_some() { 3 } else { 2 })?;
        if let Some(id) = self.item_id {
            object.serialize_field("item_id", &DisplayWire(id))?;
            object.serialize_field("events", &RecordsWire(self.records))?;
        } else {
            object.serialize_field("items", &RecordsWire(self.records))?;
        }
        object.serialize_field("next_cursor", &self.next_cursor)?;
        object.end()
    }
}

fn write_page<W: Write, R: WireRecord, F: FnMut(&R) -> Result<CursorCandidate, ReadError>>(
    output: &mut W,
    item_id: Option<&ItemId>,
    page: &ReadPage<R>,
    budget: ResponseBudget,
    mut make_cursor: F,
) -> Result<ResponseStats, EncodeError> {
    if page.records.len() > MAXIMUM_PAGE_RECORDS || (page.records.is_empty() && page.has_more) {
        return Err(EncodeError::Read(ReadError::Internal));
    }
    let empty = success(PageWire::<R> {
        item_id,
        records: &[],
        next_cursor: None,
    });
    let empty_bytes = encoded_len(&empty)?;
    if page.records.is_empty() {
        ensure_envelope_fits(empty_bytes, budget)?;
        return emit(output, &empty, empty_bytes, 0);
    }
    // Prefix sizes live on the stack and are bounded by the contract's 100
    // records. Do not build temporary encoded records, even for oversized rows.
    let mut prefix_bytes = [0; MAXIMUM_PAGE_RECORDS];
    let mut record_bytes: usize = 0;
    let mut candidates = 0;
    for (index, record) in page.records.iter().enumerate() {
        record_bytes = record_bytes
            .checked_add(encoded_len(&RecordWire(record))?)
            .ok_or(EncodeError::Read(ReadError::Internal))?;
        prefix_bytes[index] = empty_bytes
            .checked_add(record_bytes)
            .and_then(|bytes| bytes.checked_add(index))
            .ok_or(EncodeError::Read(ReadError::Internal))?;
        // A continuation can be as small as "", two bytes rather than null's
        // four. This is a true lower bound even for caller-supplied cursors.
        let has_more = page.has_more || index + 1 < page.records.len();
        let minimum_bytes = prefix_bytes[index] - if has_more { 2 } else { 0 };
        if minimum_bytes > budget.0 {
            break;
        }
        candidates = index + 1;
    }
    // Cursor sizes need not be monotonic, and a terminal page removes cursor
    // overhead entirely. Try the longest possible prefix first, then shorten.
    // Always try the first row, even if its lower bound failed, to report its
    // exact complete-envelope minimum_required_bytes (including its cursor).
    for index in (0..candidates.max(1)).rev() {
        let record = &page.records[index];
        let has_more = page.has_more || index + 1 < page.records.len();
        let candidate_cursor = if has_more {
            Some(make_cursor(record).map_err(EncodeError::Read)?)
        } else {
            None
        };
        let (cursor_text, cursor_bytes, cursor_fits) = match &candidate_cursor {
            None => (None, 4, true),
            Some(CursorCandidate::Token(token)) => {
                (Some(token.as_str()), encoded_len(token)?, true)
            }
            Some(CursorCandidate::Oversized {
                encoded_token_bytes,
            }) => (
                None,
                encoded_token_bytes
                    .checked_add(2)
                    .ok_or(EncodeError::Read(ReadError::Internal))?,
                false,
            ),
        };
        // The empty envelope already reserves the four bytes of JSON null.
        let candidate_bytes = (prefix_bytes[index] - 4)
            .checked_add(cursor_bytes)
            .ok_or(EncodeError::Read(ReadError::Internal))?;
        if !cursor_fits || candidate_bytes > budget.0 {
            if index == 0 {
                return Err(oversized(
                    record.kind(),
                    record.record_id(),
                    budget,
                    candidate_bytes,
                ));
            }
            continue;
        }
        let envelope = success(PageWire {
            item_id,
            records: &page.records[..index + 1],
            next_cursor: cursor_text,
        });
        return emit(output, &envelope, candidate_bytes, index + 1);
    }
    Err(EncodeError::Read(ReadError::Internal))
}

fn ensure_envelope_fits(bytes: usize, budget: ResponseBudget) -> Result<(), EncodeError> {
    if bytes > budget.0 {
        return Err(EncodeError::EnvelopeTooLarge {
            maximum_response_bytes: budget.0,
            minimum_required_bytes: bytes,
        });
    }
    Ok(())
}

fn oversized(kind: RecordKind, id: String, budget: ResponseBudget, bytes: usize) -> EncodeError {
    EncodeError::Read(ReadError::PayloadTooLarge(PayloadTooLarge {
        record_kind: kind,
        record_id: id,
        maximum_response_bytes: budget.0,
        minimum_required_bytes: bytes,
    }))
}

/// Count all serialized UTF-8 bytes, without storing any of them.
#[derive(Default)]
struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("serialized byte count overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encoded_len<T: Serialize>(value: &T) -> Result<usize, EncodeError> {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value).map_err(EncodeError::Serialization)?;
    Ok(counter.0)
}

/// A final buffer with a fixed capacity; serde may not grow it unexpectedly.
struct BoundedBuffer {
    bytes: Vec<u8>,
    maximum: usize,
}

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.maximum - self.bytes.len() {
            return Err(io::Error::other(
                "serialized envelope exceeded its counted size",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn emit<W: Write, T: Serialize>(
    output: &mut W,
    envelope: &T,
    bytes: usize,
    records: usize,
) -> Result<ResponseStats, EncodeError> {
    let mut buffer = BoundedBuffer {
        bytes: Vec::with_capacity(bytes + 1),
        maximum: bytes,
    };
    serde_json::to_writer(&mut buffer, envelope).map_err(EncodeError::Serialization)?;
    if buffer.bytes.len() != bytes {
        return Err(EncodeError::Read(ReadError::Internal));
    }
    buffer.bytes.push(b'\n');
    // A generic writer cannot promise an atomic physical write. All encoding
    // failures precede this call; partial I/O still propagates as a write error.
    output
        .write_all(&buffer.bytes)
        .map_err(EncodeError::Write)?;
    Ok(ResponseStats {
        json_bytes: bytes,
        emitted_records: records,
    })
}

/// Display serialization streams the canonical ID without making a String.
struct DisplayWire<'a, T>(&'a T);

impl<T: fmt::Display> Serialize for DisplayWire<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self.0)
    }
}

#[derive(Serialize)]
struct SummaryWire<'a> {
    id: DisplayWire<'a, ItemId>,
    title: &'a str,
    status: &'static str,
    priority: Option<&'static str>,
    assignee: Option<&'a str>,
    revision: u64,
}

#[derive(Serialize)]
struct WorkWire<'a> {
    #[serde(flatten)]
    summary: SummaryWire<'a>,
    description: Option<&'a str>,
    acceptance_criteria: &'a [String],
    status_reason: Option<&'a str>,
}

#[derive(Serialize)]
struct AuditWire<'a> {
    #[serde(flatten)]
    work: WorkWire<'a>,
    requester: &'a str,
    project: &'a str,
    sequence: u64,
    captured_at: &'a str,
    updated_at: &'a str,
    provenance: ProvenanceWire<'a>,
}

struct ProjectionWire<'a>(&'a ItemProjection);

impl Serialize for ProjectionWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // All projections share one ordered rule for their common fields.
        macro_rules! summary {
            ($item:expr) => {
                SummaryWire {
                    id: DisplayWire(&$item.id),
                    title: &$item.title,
                    status: status($item.status),
                    priority: $item.priority.map(priority),
                    assignee: $item.assignee.as_ref().map(AssigneeId::as_str),
                    revision: $item.revision.get(),
                }
            };
        }
        macro_rules! work {
            ($item:expr) => {
                WorkWire {
                    summary: summary!($item),
                    description: $item.description.as_deref(),
                    acceptance_criteria: &$item.acceptance_criteria,
                    status_reason: $item.status_reason.as_deref(),
                }
            };
        }
        match self.0 {
            ItemProjection::Summary(item) => summary!(item).serialize(serializer),
            ItemProjection::Work(item) => work!(item).serialize(serializer),
            ItemProjection::Audit(item) => AuditWire {
                work: work!(item),
                requester: item.id.requester().as_str(),
                project: item.id.project().as_str(),
                sequence: item.id.sequence(),
                captured_at: item.captured_at.as_str(),
                updated_at: item.updated_at.as_str(),
                provenance: ProvenanceWire(&item.provenance),
            }
            .serialize(serializer),
        }
    }
}

fn projection_id(item: &ItemProjection) -> &ItemId {
    match item {
        ItemProjection::Summary(item) => &item.id,
        ItemProjection::Work(item) => &item.id,
        ItemProjection::Audit(item) => &item.id,
    }
}

struct ProvenanceWire<'a>(&'a Provenance);

impl Serialize for ProvenanceWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let p = self.0;
        let mut object = serializer.serialize_struct("Provenance", 7)?;
        object.serialize_field("source_host", &p.source_host().map(source_host))?;
        object.serialize_field("thread_id", &p.thread_id().map(|id| id.as_str()))?;
        object.serialize_field("message_id", &p.message_id().map(|id| id.as_str()))?;
        object.serialize_field("url", &p.url().map(|url| url.as_str()))?;
        object.serialize_field(
            "repository_reference",
            &p.repository_reference().map(|r| r.as_str()),
        )?;
        object.serialize_field(
            "revision_reference",
            &p.revision_reference().map(|r| r.as_str()),
        )?;
        object.serialize_field("context_excerpt", &p.context_excerpt())?;
        object.end()
    }
}

trait WireRecord {
    fn serialize_record<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>;
    fn kind(&self) -> RecordKind;
    fn record_id(&self) -> String;
}

impl WireRecord for ProjectedItemRow {
    fn serialize_record<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ProjectionWire(&self.item).serialize(serializer)
    }
    fn kind(&self) -> RecordKind {
        RecordKind::Item
    }
    fn record_id(&self) -> String {
        projection_id(&self.item).to_string()
    }
}

struct RecordWire<'a, R>(&'a R);

impl<R: WireRecord> Serialize for RecordWire<'_, R> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize_record(serializer)
    }
}

struct RecordsWire<'a, R>(&'a [R]);

impl<R: WireRecord> Serialize for RecordsWire<'_, R> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut array = serializer.serialize_seq(Some(self.0.len()))?;
        for record in self.0 {
            array.serialize_element(&RecordWire(record))?;
        }
        array.end()
    }
}

impl WireRecord for ItemHistoryEvent {
    fn serialize_record<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_struct("HistoryEvent", 13)?;
        object.serialize_field("operation_id", &self.operation_id)?;
        object.serialize_field("event_id", &self.event_id)?;
        object.serialize_field("item_revision", &self.item_revision.get())?;
        object.serialize_field("event_index", &self.event_index)?;
        object.serialize_field("event_type", event_type(self.event_type))?;
        object.serialize_field("before", &self.before.as_ref().map(EventValueWire))?;
        object.serialize_field("after", &self.after.as_ref().map(EventValueWire))?;
        object.serialize_field("actor", &ActorWire(&self.actor))?;
        object.serialize_field("execution", &ExecutionWire(&self.execution))?;
        object.serialize_field("reason", &self.reason)?;
        object.serialize_field("note", &self.note)?;
        object.serialize_field("occurred_at", self.occurred_at.as_str())?;
        object.serialize_field("schema_version", &self.schema_version)?;
        object.end()
    }
    fn kind(&self) -> RecordKind {
        RecordKind::Event
    }
    fn record_id(&self) -> String {
        self.event_id.clone()
    }
}

struct EventValueWire<'a>(&'a EventValue);

impl Serialize for EventValueWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            EventValue::Status(value) => status(*value).serialize(serializer),
            EventValue::Priority(value) => value.map(priority).serialize(serializer),
            EventValue::Assignee(value) => {
                value.as_ref().map(AssigneeId::as_str).serialize(serializer)
            }
            EventValue::Note(value) => value.serialize(serializer),
        }
    }
}

struct ActorWire<'a>(&'a EventActor);

impl Serialize for ActorWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_struct("Actor", 4)?;
        object.serialize_field(
            "kind",
            match self.0.kind {
                ActorKind::Human => "human",
                ActorKind::Agent => "agent",
            },
        )?;
        object.serialize_field("id", &self.0.id)?;
        object.serialize_field("surface", &self.0.surface)?;
        object.serialize_field("host", &self.0.host)?;
        object.end()
    }
}

struct ExecutionWire<'a>(&'a EventExecution);

impl Serialize for ExecutionWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let agent = matches!(self.0, EventExecution::Agent { .. });
        let mut object = serializer.serialize_struct("Execution", if agent { 4 } else { 3 })?;
        match self.0 {
            EventExecution::Direct { surface, host } => {
                object.serialize_field("kind", "direct")?;
                object.serialize_field("surface", surface)?;
                object.serialize_field("host", host)?;
            }
            EventExecution::Agent {
                agent_id,
                surface,
                host,
            } => {
                object.serialize_field("kind", "agent")?;
                object.serialize_field("agent_id", agent_id)?;
                object.serialize_field("surface", surface)?;
                object.serialize_field("host", host)?;
            }
        }
        object.end()
    }
}

fn status(value: Status) -> &'static str {
    match value {
        Status::Proposed => "proposed",
        Status::Ready => "ready",
        Status::InProgress => "in_progress",
        Status::Blocked => "blocked",
        Status::Done => "done",
        Status::Rejected => "rejected",
    }
}

fn priority(value: Priority) -> &'static str {
    match value {
        Priority::P0 => "P0",
        Priority::P1 => "P1",
        Priority::P2 => "P2",
        Priority::P3 => "P3",
        Priority::P4 => "P4",
    }
}

fn source_host(value: SourceHost) -> &'static str {
    match value {
        SourceHost::Delta => "delta",
        SourceHost::Codex => "codex",
        SourceHost::Local => "local",
    }
}

fn event_type(value: EventType) -> &'static str {
    match value {
        EventType::Captured => "captured",
        EventType::Approved => "approved",
        EventType::Rejected => "rejected",
        EventType::Started => "started",
        EventType::Blocked => "blocked",
        EventType::Resumed => "resumed",
        EventType::Finished => "finished",
        EventType::PriorityChanged => "priority_changed",
        EventType::AssigneeChanged => "assignee_changed",
        EventType::NoteAdded => "note_added",
    }
}
