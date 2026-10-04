//! Bounded local-only continuation parameters, not credentials or snapshots.
//!
//! Hex keeps the codec dependency-free. No MAC is needed for local pagination:
//! even a modified, valid cursor only supplies a bound SQL parameter. Every read
//! must still validate its request and re-evaluate authorization independently.

use std::{error::Error, fmt, io, marker::PhantomData};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, Visitor, value::MapAccessDeserializer},
};

use crate::domain::{ItemId, NamedView, Priority, ProjectId, RequesterId, Revision, Timestamp};

use super::{
    HistoryPageRequest, HistoryReadKey, InvalidProjectionFilters, ItemListOrdering,
    ItemProjectionPageRequest, ItemReadKey,
    read_semantics::{EffectiveItemFilters, ItemQueryFingerprintInput, ItemReadOperation},
};

/// Maximum complete encoded token size, checked before decoding or allocation.
pub const MAX_CURSOR_BYTES: usize = 16_384;
const PREFIX: &str = "bifc1.";
const MAX_ENVELOPE_BYTES: usize = (MAX_CURSOR_BYTES - PREFIX.len()) / 2;
const CURSOR_VERSION: u32 = 1;
const ORDER_VERSION: u32 = 1;
const PROJECTION_SCHEMA_VERSION: u32 = 1;

/// Stable failure categories; adapters emit these as `details.reason`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidCursorReason {
    Malformed,
    Oversized,
    WrongKind,
    WrongStore,
    WrongQuery,
    UnsupportedVersion,
}

impl InvalidCursorReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Oversized => "oversized",
            Self::WrongKind => "wrong_kind",
            Self::WrongStore => "wrong_store",
            Self::WrongQuery => "wrong_query",
            Self::UnsupportedVersion => "unsupported_version",
        }
    }
}

/// Invalid continuation input always requires restarting without a cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidCursor(InvalidCursorReason);

impl InvalidCursor {
    pub const fn code(self) -> &'static str {
        "invalid_cursor"
    }

    pub const fn reason(self) -> &'static str {
        self.0.as_str()
    }

    pub const fn reason_kind(self) -> InvalidCursorReason {
        self.0
    }

    pub const fn restart_required(self) -> bool {
        true
    }
}

impl fmt::Display for InvalidCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid cursor ({}); restart pagination without a cursor",
            self.reason()
        )
    }
}

impl Error for InvalidCursor {}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CursorKind {
    List,
    Next,
    History,
}

impl<'de> Deserialize<'de> for CursorKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "list" => Ok(Self::List),
            "next" => Ok(Self::Next),
            "history" => Ok(Self::History),
            value => Err(de::Error::unknown_variant(
                value,
                &["list", "next", "history"],
            )),
        }
    }
}

/// Structurally validated continuation data, still unbound and never authority.
///
/// Parse before config/storage access. The private typed boundary is available
/// only after binding against the independently resolved current request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedCursor {
    kind: CursorKind,
    store_id: String,
    query_fingerprint: String,
    boundary: DecodedBoundary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DecodedBoundary {
    Item(ItemReadKey),
    History(HistoryReadKey),
}

impl DecodedCursor {
    /// Validate the bounded wire format and domain keys without expected context.
    pub fn parse(token: &str) -> Result<Self, InvalidCursor> {
        use InvalidCursorReason::*;
        if token.len() > MAX_CURSOR_BYTES {
            return Err(InvalidCursor(Oversized));
        }
        let hex = token.strip_prefix(PREFIX).ok_or(InvalidCursor(Malformed))?;
        if hex.is_empty() || hex.len() % 2 != 0 {
            return Err(InvalidCursor(Malformed));
        }
        let mut bytes = Vec::with_capacity(hex.len() / 2);
        for pair in hex.as_bytes().chunks_exact(2) {
            let high = hex_digit(pair[0]).ok_or(InvalidCursor(Malformed))?;
            let low = hex_digit(pair[1]).ok_or(InvalidCursor(Malformed))?;
            bytes.push(high * 16 + low);
        }
        let ObjectOnly(envelope): ObjectOnly<Envelope> =
            serde_json::from_slice(&bytes).map_err(|_| InvalidCursor(Malformed))?;
        if envelope.cursor_version != CURSOR_VERSION
            || envelope.order_version != ORDER_VERSION
            || envelope.projection_schema_version != PROJECTION_SCHEMA_VERSION
            || envelope.generation.is_some()
        {
            return Err(InvalidCursor(UnsupportedVersion));
        }
        let boundary = match envelope.boundary {
            Boundary::Item(key) => {
                let malformed = || InvalidCursor(Malformed);
                let requester = RequesterId::new(&key.requester).map_err(|_| malformed())?;
                let project = ProjectId::new(&key.project).map_err(|_| malformed())?;
                // Reject rather than silently repair noncanonical identities.
                if requester.as_str() != key.requester || project.as_str() != key.project {
                    return Err(malformed());
                }
                DecodedBoundary::Item(ItemReadKey {
                    id: ItemId::new(requester, project, key.sequence).map_err(|_| malformed())?,
                    captured_at: Timestamp::new(key.captured_at),
                    priority: key.priority.map(Priority::from),
                })
            }
            Boundary::History(key) => DecodedBoundary::History(HistoryReadKey {
                item_revision: Revision::new(key.item_revision)
                    .map_err(|_| InvalidCursor(Malformed))?,
                event_index: key.event_index,
            }),
        };
        if !matches!(
            (&envelope.kind, &boundary),
            (
                CursorKind::List | CursorKind::Next,
                DecodedBoundary::Item(_)
            ) | (CursorKind::History, DecodedBoundary::History(_))
        ) {
            return Err(InvalidCursor(WrongKind));
        }
        Ok(Self {
            kind: envelope.kind,
            store_id: envelope.store_id,
            query_fingerprint: envelope.query_fingerprint,
            boundary,
        })
    }

    /// Reject operation swaps before resolving store/query context.
    pub fn require_item_kind(&self, ordering: ItemListOrdering) -> Result<(), InvalidCursor> {
        self.require_kind(match ordering {
            ItemListOrdering::NewestFirst => CursorKind::List,
            ItemListOrdering::Next => CursorKind::Next,
        })
    }

    pub fn require_history_kind(&self) -> Result<(), InvalidCursor> {
        self.require_kind(CursorKind::History)
    }

    fn require_kind(&self, kind: CursorKind) -> Result<(), InvalidCursor> {
        if self.kind == kind {
            Ok(())
        } else {
            Err(InvalidCursor(InvalidCursorReason::WrongKind))
        }
    }
}

/// The expected operation/store/query, reconstructed from each current request.
///
/// Page size and the existing boundary do not affect binding. This object never
/// grants access; callers pass decoded keys through the authorized read use case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorContext {
    kind: CursorKind,
    store_id: String,
    query_fingerprint: String,
}

impl CursorContext {
    pub fn item_page(
        store_id: impl Into<String>,
        request: &ItemProjectionPageRequest,
    ) -> Result<Self, InvalidProjectionFilters> {
        let store_id = store_id.into();
        let (kind, operation) = match request.ordering {
            ItemListOrdering::NewestFirst => (CursorKind::List, ItemReadOperation::List),
            ItemListOrdering::Next => (CursorKind::Next, ItemReadOperation::Next),
        };
        let mut bytes = ItemQueryFingerprintInput::new(
            operation,
            &store_id,
            PROJECTION_SCHEMA_VERSION,
            request,
        )?
        .canonical_bytes();
        // Empty normalized predicates intentionally erase irrelevant filters.
        // Resolved mine ownership is nevertheless required cursor scope, even
        // when the intersection cannot match anything. Nonempty predicates
        // already retain ownership, so equivalent explicit filters still match.
        let empty_mine = request.view == NamedView::Mine
            && EffectiveItemFilters::from_request(request)?.is_empty();
        super::append_field(
            &mut bytes,
            empty_mine.then(|| request.configured_requester.as_str()),
        );
        Ok(Self {
            kind,
            store_id,
            query_fingerprint: super::sha256(&bytes),
        })
    }

    pub fn history(store_id: impl Into<String>, request: &HistoryPageRequest) -> Self {
        let store_id = store_id.into();
        let mut bytes = b"BIF:history-query:1\0".to_vec();
        super::append_field(&mut bytes, Some(&store_id));
        super::append_field(&mut bytes, Some(request.item_id.requester().as_str()));
        super::append_field(&mut bytes, Some(request.item_id.project().as_str()));
        bytes.extend_from_slice(&request.item_id.sequence().to_be_bytes());
        // History currently has exactly one canonical ordering.
        bytes.extend_from_slice(&ORDER_VERSION.to_be_bytes());
        bytes.extend_from_slice(&PROJECTION_SCHEMA_VERSION.to_be_bytes());
        Self {
            kind: CursorKind::History,
            store_id,
            query_fingerprint: super::sha256(&bytes),
        }
    }

    pub fn encode_item_key(&self, key: &ItemReadKey) -> Result<String, InvalidCursor> {
        self.require_item_kind()?;
        // Bound intermediate copies as well as the final encoded representation.
        if key.captured_at.as_str().len() > MAX_ENVELOPE_BYTES
            || key.id.requester().as_str().len() > MAX_ENVELOPE_BYTES
            || key.id.project().as_str().len() > MAX_ENVELOPE_BYTES
        {
            return Err(InvalidCursor(InvalidCursorReason::Oversized));
        }
        self.encode(Boundary::Item(ItemBoundary {
            requester: key.id.requester().as_str().to_owned(),
            project: key.id.project().as_str().to_owned(),
            sequence: key.id.sequence(),
            captured_at: key.captured_at.as_str().to_owned(),
            priority: key.priority.map(WirePriority::from),
        }))
    }

    pub fn decode_item_key(&self, token: &str) -> Result<ItemReadKey, InvalidCursor> {
        self.require_item_kind()?;
        self.bind_item_key(&DecodedCursor::parse(token)?)
    }

    /// Bind already parsed input; callers must still authorize the read use case.
    pub fn bind_item_key(&self, cursor: &DecodedCursor) -> Result<ItemReadKey, InvalidCursor> {
        self.require_item_kind()?;
        let DecodedBoundary::Item(key) = self.bind(cursor)? else {
            return Err(InvalidCursor(InvalidCursorReason::WrongKind));
        };
        Ok(key.clone())
    }

    pub fn encode_history_key(&self, key: &HistoryReadKey) -> Result<String, InvalidCursor> {
        self.require_history_kind()?;
        self.encode(Boundary::History(HistoryBoundary {
            item_revision: key.item_revision.get(),
            event_index: key.event_index,
        }))
    }

    pub fn decode_history_key(&self, token: &str) -> Result<HistoryReadKey, InvalidCursor> {
        self.require_history_kind()?;
        self.bind_history_key(&DecodedCursor::parse(token)?)
    }

    pub fn bind_history_key(
        &self,
        cursor: &DecodedCursor,
    ) -> Result<HistoryReadKey, InvalidCursor> {
        self.require_history_kind()?;
        let DecodedBoundary::History(key) = self.bind(cursor)? else {
            return Err(InvalidCursor(InvalidCursorReason::WrongKind));
        };
        Ok(key.clone())
    }

    fn require_item_kind(&self) -> Result<(), InvalidCursor> {
        if self.kind == CursorKind::History {
            Err(InvalidCursor(InvalidCursorReason::WrongKind))
        } else {
            Ok(())
        }
    }

    fn require_history_kind(&self) -> Result<(), InvalidCursor> {
        if self.kind == CursorKind::History {
            Ok(())
        } else {
            Err(InvalidCursor(InvalidCursorReason::WrongKind))
        }
    }

    fn encode(&self, boundary: Boundary) -> Result<String, InvalidCursor> {
        if self.store_id.len() > MAX_ENVELOPE_BYTES {
            return Err(InvalidCursor(InvalidCursorReason::Oversized));
        }
        let envelope = Envelope {
            cursor_version: CURSOR_VERSION,
            kind: self.kind,
            store_id: self.store_id.clone(),
            generation: None,
            query_fingerprint: self.query_fingerprint.clone(),
            order_version: ORDER_VERSION,
            projection_schema_version: PROJECTION_SCHEMA_VERSION,
            boundary,
        };
        let mut writer = BoundedEnvelope(Vec::new());
        serde_json::to_writer(&mut writer, &envelope)
            .map_err(|_| InvalidCursor(InvalidCursorReason::Oversized))?;
        let mut token = String::with_capacity(PREFIX.len() + writer.0.len() * 2);
        token.push_str(PREFIX);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in writer.0 {
            token.push(HEX[(byte >> 4) as usize] as char);
            token.push(HEX[(byte & 15) as usize] as char);
        }
        Ok(token)
    }

    fn bind<'a>(&self, cursor: &'a DecodedCursor) -> Result<&'a DecodedBoundary, InvalidCursor> {
        use InvalidCursorReason::*;
        cursor.require_kind(self.kind)?;
        if cursor.store_id != self.store_id {
            return Err(InvalidCursor(WrongStore));
        }
        if cursor.query_fingerprint != self.query_fingerprint {
            return Err(InvalidCursor(WrongQuery));
        }
        Ok(&cursor.boundary)
    }
}

/// Constrains the outer JSON value without replacing derived field validation.
struct ObjectOnly<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for ObjectOnly<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        object_only(deserializer).map(Self)
    }
}

/// Derived structs also accept positional arrays; cursor objects must be maps.
fn object_only<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct ObjectVisitor<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
        type Value = T;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a cursor object")
        }

        fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<T, M::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(ObjectVisitor(PhantomData))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    cursor_version: u32,
    kind: CursorKind,
    store_id: String,
    // Reserved for the journal migration: absent/null is supported, never a
    // non-null generation that would imply restore safety we do not provide.
    generation: Option<String>,
    query_fingerprint: String,
    order_version: u32,
    projection_schema_version: u32,
    boundary: Boundary,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Boundary {
    Item(#[serde(deserialize_with = "object_only")] ItemBoundary),
    History(#[serde(deserialize_with = "object_only")] HistoryBoundary),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemBoundary {
    requester: String,
    project: String,
    sequence: u64,
    captured_at: String,
    #[serde(deserialize_with = "required_nullable")]
    priority: Option<WirePriority>,
}

fn required_nullable<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<WirePriority>, D::Error> {
    Option::deserialize(deserializer)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryBoundary {
    item_revision: u64,
    event_index: u64,
}

#[derive(Serialize)]
enum WirePriority {
    P0,
    P1,
    P2,
    P3,
    P4,
}

impl<'de> Deserialize<'de> for WirePriority {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "P0" => Ok(Self::P0),
            "P1" => Ok(Self::P1),
            "P2" => Ok(Self::P2),
            "P3" => Ok(Self::P3),
            "P4" => Ok(Self::P4),
            value => Err(de::Error::unknown_variant(
                value,
                &["P0", "P1", "P2", "P3", "P4"],
            )),
        }
    }
}

impl From<Priority> for WirePriority {
    fn from(priority: Priority) -> Self {
        match priority {
            Priority::P0 => Self::P0,
            Priority::P1 => Self::P1,
            Priority::P2 => Self::P2,
            Priority::P3 => Self::P3,
            Priority::P4 => Self::P4,
        }
    }
}

impl From<WirePriority> for Priority {
    fn from(priority: WirePriority) -> Self {
        match priority {
            WirePriority::P0 => Self::P0,
            WirePriority::P1 => Self::P1,
            WirePriority::P2 => Self::P2,
            WirePriority::P3 => Self::P3,
            WirePriority::P4 => Self::P4,
        }
    }
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Caps JSON serialization before it can allocate an oversized token.
struct BoundedEnvelope(Vec<u8>);

impl io::Write for BoundedEnvelope {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_ENVELOPE_BYTES - self.0.len() {
            return Err(io::Error::other("cursor envelope exceeds byte limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
