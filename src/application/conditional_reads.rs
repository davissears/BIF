//! Typed conditional reads and one-call work selection, independent of delivery.

use std::{error::Error, fmt};

use super::{
    AuthorizationRequest, Command, ItemListFilters, ItemListOrdering, ItemProjection,
    ItemProjectionKind, ItemProjectionPageRequest, ItemProjectionStore, ProjectionPageError,
    ReadPageRequest, Unauthorized, authorize, read_item_projection_page,
};
use crate::domain::{ItemId, NamedView, ProjectId, RequesterId, Revision};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConditionalGetRequest {
    pub item_id: ItemId,
    pub projection: ItemProjectionKind,
    pub known_version: Option<String>,
}

/// A hit contains no item payload; a miss is a complete replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConditionalReadOutcome {
    Modified {
        version: String,
        item: ItemProjection,
    },
    NotModified {
        version: String,
    },
}

#[derive(Debug)]
pub enum ConditionalReadError<E> {
    Unauthorized(Unauthorized),
    NotFound,
    InvalidValidator,
    Storage(E),
}

impl<E: fmt::Display> fmt::Display for ConditionalReadError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized(error) => error.fmt(formatter),
            Self::NotFound => formatter.write_str("The item was not found"),
            Self::InvalidValidator => formatter.write_str("The validator is invalid"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl<E: Error + 'static> Error for ConditionalReadError<E> {}

/// Existence, revision, validator comparison and miss hydration share a snapshot.
/// Check existence before validator syntax; never hydrate criteria/provenance on
/// a hit. Authorization is the use case's responsibility, never the token's.
pub trait ConditionalProjectionStore {
    type Error;
    fn read_conditional_projection(
        &self,
        request: &ConditionalGetRequest,
    ) -> Result<ConditionalReadOutcome, ConditionalReadError<Self::Error>>;
}

pub fn read_conditional_item_projection<S: ConditionalProjectionStore>(
    store: &S,
    authorization: &AuthorizationRequest<'_>,
    request: &ConditionalGetRequest,
) -> Result<ConditionalReadOutcome, ConditionalReadError<S::Error>> {
    if authorization.command != Command::Read {
        return Err(ConditionalReadError::Unauthorized(Unauthorized));
    }
    authorize(authorization).map_err(ConditionalReadError::Unauthorized)?;
    store.read_conditional_projection(request)
}

/// Opaque, deterministic binding to store, item, projection, schema and revision.
/// `bifv1` fixes API v2 / projection schema v1. This is a freshness validator,
/// not a capability, signature or protection against unauthorized writes.
pub struct ProjectionValidator(String);

impl ProjectionValidator {
    pub fn new(
        store: &str,
        schema: i64,
        item: &ItemId,
        projection: ItemProjectionKind,
        revision: Revision,
    ) -> Self {
        let kind = match projection {
            ItemProjectionKind::Summary => "summary",
            ItemProjectionKind::Work => "work",
            ItemProjectionKind::Audit => "audit",
        };
        Self(format!(
            "bifv1.{}.{}.{kind}.{schema}.{}",
            hex(store),
            hex(&item.to_string()),
            revision.get()
        ))
    }

    pub fn into_string(self) -> String {
        self.0
    }

    /// Malformed/unknown formats are rejected; valid bindings to another scope
    /// are ordinary misses, including wrong stores and projections.
    pub fn matches(&self, known: &str) -> Result<bool, InvalidValidator> {
        if known.len() > crate::limits::MAXIMUM_REQUEST_BYTES {
            return Err(InvalidValidator);
        }
        let parts = known.split('.').collect::<Vec<_>>();
        if parts.len() != 6 || parts[0] != "bifv1" {
            return Err(InvalidValidator);
        }
        let store = unhex(parts[1])?;
        let item = unhex(parts[2])?;
        if store.is_empty()
            || !canonical_item_id(&item)
            || !matches!(parts[3], "summary" | "work" | "audit")
        {
            return Err(InvalidValidator);
        }
        let schema = parts[4].parse::<i64>().map_err(|_| InvalidValidator)?;
        let revision = parts[5].parse::<u64>().map_err(|_| InvalidValidator)?;
        if schema < 0
            || revision == 0
            || schema.to_string() != parts[4]
            || revision.to_string() != parts[5]
        {
            return Err(InvalidValidator);
        }
        Ok(self.0 == known)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidValidator;

fn canonical_item_id(value: &str) -> bool {
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() != 3 {
        return false;
    }
    let id = RequesterId::new(parts[0])
        .ok()
        .zip(ProjectId::new(parts[1]).ok())
        .zip(parts[2].parse::<u64>().ok())
        .and_then(|((requester, project), sequence)| {
            ItemId::new(requester, project, sequence).ok()
        });
    id.is_some_and(|id| id.to_string() == value)
}

fn hex(value: &str) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    value
        .bytes()
        .flat_map(|byte| {
            [
                char::from(DIGITS[usize::from(byte >> 4)]),
                char::from(DIGITS[usize::from(byte & 15)]),
            ]
        })
        .collect()
}

fn unhex(value: &str) -> Result<String, InvalidValidator> {
    if value.len() % 2 != 0 {
        return Err(InvalidValidator);
    }
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(InvalidValidator),
    };
    let bytes = value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok(digit(pair[0])? * 16 + digit(pair[1])?))
        .collect::<Result<Vec<_>, InvalidValidator>>()?;
    String::from_utf8(bytes).map_err(|_| InvalidValidator)
}

/// Next ready work for an explicit project; never claims or mutates the item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectedWorkOutcome {
    Selected(ItemProjection),
    Empty,
}

pub fn read_selected_work<S: ItemProjectionStore>(
    store: &S,
    authorization: &AuthorizationRequest<'_>,
    requester: &RequesterId,
    project: ProjectId,
) -> Result<SelectedWorkOutcome, ProjectionPageError<S::Error>> {
    let request = ItemProjectionPageRequest {
        view: NamedView::Ready,
        configured_requester: requester.clone(),
        filters: ItemListFilters {
            project: Some(project),
            ..Default::default()
        },
        projection: ItemProjectionKind::Work,
        ordering: ItemListOrdering::Next,
        page: ReadPageRequest::new(1, None).expect("one is a valid page size"),
    };
    let page = read_item_projection_page(store, authorization, &request)?;
    Ok(match page.records.into_iter().next() {
        Some(row) => SelectedWorkOutcome::Selected(row.item),
        None => SelectedWorkOutcome::Empty,
    })
}
