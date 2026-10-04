//! Canonical v2 item-query semantics, independent of storage and delivery.
//!
//! Normalization describes what a query means. It does not encode, authenticate,
//! or validate opaque cursors. The unchanged v1 path is the compatibility oracle.

use std::cmp::Ordering;

use crate::domain::{
    AssigneeId, Item, ItemId, NamedView, Priority, ProjectId, RequesterId, Status, Timestamp,
};

use super::{
    InvalidProjectionFilters, ItemListFilters, ItemListOrdering, ItemProjectionKind,
    ItemProjectionPageRequest, ItemReadKey,
};

const STATUSES: [Status; 6] = [
    Status::Proposed,
    Status::Ready,
    Status::InProgress,
    Status::Blocked,
    Status::Done,
    Status::Rejected,
];

/// Effective ownership restriction; absence of a restriction is not unassigned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssigneeFilter {
    Any,
    Assigned(AssigneeId),
    Unassigned,
}

/// Normalized intersection of a named view and explicit filters.
///
/// Private fields prevent noncanonical status sets and text from entering query
/// fingerprints. Every contradiction becomes the same empty predicate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveItemFilters {
    statuses: Vec<Status>,
    project: Option<ProjectId>,
    requester: Option<RequesterId>,
    assignee: AssigneeFilter,
    priority: Option<Priority>,
    text: Option<String>,
}

impl EffectiveItemFilters {
    /// Intersects rather than overrides scope, including explicit `mine` filters.
    pub fn new(
        view: NamedView,
        configured_requester: &RequesterId,
        filters: &ItemListFilters,
    ) -> Result<Self, InvalidProjectionFilters> {
        if filters.assignee.is_some() && filters.unassigned {
            return Err(InvalidProjectionFilters);
        }
        let mut statuses = match view {
            NamedView::Proposed => vec![Status::Proposed],
            NamedView::Ready => vec![Status::Ready],
            NamedView::Active => vec![Status::InProgress, Status::Blocked],
            NamedView::Blocked => vec![Status::Blocked],
            NamedView::Done => vec![Status::Done],
            NamedView::Rejected => vec![Status::Rejected],
            NamedView::Mine => STATUSES[..4].to_vec(),
            NamedView::All => STATUSES.to_vec(),
        };
        if let Some(status) = filters.status {
            statuses.retain(|candidate| *candidate == status);
        }
        let mut assignee = if filters.unassigned {
            AssigneeFilter::Unassigned
        } else if let Some(assignee) = &filters.assignee {
            AssigneeFilter::Assigned(assignee.clone())
        } else {
            AssigneeFilter::Any
        };
        if view == NamedView::Mine {
            // RequesterId is canonical uppercase ASCII; AssigneeId is lowercase.
            let mine = AssigneeId::new(configured_requester.as_str())
                .expect("a canonical requester is a valid assignee");
            match &assignee {
                AssigneeFilter::Unassigned => statuses.clear(),
                AssigneeFilter::Assigned(explicit) if *explicit != mine => statuses.clear(),
                _ => {}
            }
            assignee = AssigneeFilter::Assigned(mine);
        }
        if statuses.is_empty() {
            return Ok(Self {
                statuses,
                project: None,
                requester: None,
                assignee: AssigneeFilter::Any,
                priority: None,
                text: None,
            });
        }
        Ok(Self {
            statuses,
            project: filters.project.clone(),
            requester: filters.requester.clone(),
            assignee,
            priority: filters.priority,
            // SQLite's built-in lower() folds ASCII only. Do not trim whitespace
            // or add Unicode folding, tokenization, or SQL wildcard semantics.
            text: filters
                .text
                .as_ref()
                .map(|text| text.as_str().to_ascii_lowercase()),
        })
    }

    pub fn from_request(
        request: &ItemProjectionPageRequest,
    ) -> Result<Self, InvalidProjectionFilters> {
        Self::new(
            request.view,
            &request.configured_requester,
            &request.filters,
        )
    }

    /// Ordered allowed states; an empty set means the query cannot match.
    pub fn statuses(&self) -> &[Status] {
        &self.statuses
    }

    pub fn is_empty(&self) -> bool {
        self.statuses.is_empty()
    }

    pub fn project(&self) -> Option<&ProjectId> {
        self.project.as_ref()
    }

    pub fn requester(&self) -> Option<&RequesterId> {
        self.requester.as_ref()
    }

    pub fn assignee(&self) -> &AssigneeFilter {
        &self.assignee
    }

    pub fn priority(&self) -> Option<Priority> {
        self.priority
    }

    /// ASCII-folded literal substring; surrounding whitespace remains significant.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Reference membership semantics, not a materialize-and-filter storage path.
    pub fn matches(&self, item: &Item) -> bool {
        self.statuses.contains(&item.status())
            && self
                .project
                .as_ref()
                .is_none_or(|project| project == item.id().project())
            && self
                .requester
                .as_ref()
                .is_none_or(|requester| requester == item.id().requester())
            && match &self.assignee {
                AssigneeFilter::Any => true,
                AssigneeFilter::Assigned(assignee) => item.assignee() == Some(assignee),
                AssigneeFilter::Unassigned => item.assignee().is_none(),
            }
            && self
                .priority
                .is_none_or(|priority| item.priority() == Some(priority))
            && self.text.as_ref().is_none_or(|needle| {
                let content = item.content();
                std::iter::once(content.title())
                    .chain(content.description())
                    .chain(content.acceptance_criteria().iter().map(String::as_str))
                    .any(|value| value.to_ascii_lowercase().contains(needle))
            })
    }
}

/// Null-last queue order: P0 through P4 map to 0 through 4; null maps to 5.
pub const fn priority_rank(priority: Option<Priority>) -> u8 {
    match priority {
        Some(Priority::P0) => 0,
        Some(Priority::P1) => 1,
        Some(Priority::P2) => 2,
        Some(Priority::P3) => 3,
        Some(Priority::P4) => 4,
        None => 5,
    }
}

/// Canonical requester/project strings followed by the full-width numeric u64.
pub fn compare_identity(left: &ItemId, right: &ItemId) -> Ordering {
    left.cmp(right)
}

/// Opaque timestamps compare as UTF-8 strings, not parsed instants.
///
/// SQLite must use BINARY text comparison, never datetime()/julianday().
pub fn compare_captured_at(left: &Timestamp, right: &Timestamp) -> Ordering {
    left.as_str().cmp(right.as_str())
}

/// Transport/storage-neutral coordinate of the canonical item ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemSortField {
    PriorityRank,
    CapturedAt,
    Requester,
    Project,
    Sequence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// One lexicographic sort coordinate. Text coordinates require binary collation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItemSortTerm {
    pub field: ItemSortField,
    pub direction: SortDirection,
}

const IDENTITY_ORDER: [ItemSortTerm; 3] = [
    ItemSortTerm {
        field: ItemSortField::Requester,
        direction: SortDirection::Ascending,
    },
    ItemSortTerm {
        field: ItemSortField::Project,
        direction: SortDirection::Ascending,
    },
    ItemSortTerm {
        field: ItemSortField::Sequence,
        direction: SortDirection::Ascending,
    },
];
const LIST_ORDER: [ItemSortTerm; 4] = [
    ItemSortTerm {
        field: ItemSortField::CapturedAt,
        direction: SortDirection::Descending,
    },
    IDENTITY_ORDER[0],
    IDENTITY_ORDER[1],
    IDENTITY_ORDER[2],
];
const NEXT_ORDER: [ItemSortTerm; 5] = [
    ItemSortTerm {
        field: ItemSortField::PriorityRank,
        direction: SortDirection::Ascending,
    },
    ItemSortTerm {
        field: ItemSortField::CapturedAt,
        direction: SortDirection::Ascending,
    },
    IDENTITY_ORDER[0],
    IDENTITY_ORDER[1],
    IDENTITY_ORDER[2],
];

impl ItemListOrdering {
    /// Shared source of truth for comparators and downstream SQL sort/boundaries.
    pub const fn sort_spec(self) -> &'static [ItemSortTerm] {
        match self {
            Self::NewestFirst => &LIST_ORDER,
            Self::Next => &NEXT_ORDER,
        }
    }

    /// Less means `left` appears earlier in the requested result order.
    pub fn compare_keys(self, left: &ItemReadKey, right: &ItemReadKey) -> Ordering {
        for term in self.sort_spec() {
            let comparison = match term.field {
                ItemSortField::PriorityRank => {
                    priority_rank(left.priority).cmp(&priority_rank(right.priority))
                }
                ItemSortField::CapturedAt => {
                    compare_captured_at(&left.captured_at, &right.captured_at)
                }
                ItemSortField::Requester => left.id.requester().cmp(right.id.requester()),
                ItemSortField::Project => left.id.project().cmp(right.id.project()),
                ItemSortField::Sequence => left.id.sequence().cmp(&right.id.sequence()),
            };
            let comparison = match term.direction {
                SortDirection::Ascending => comparison,
                SortDirection::Descending => comparison.reverse(),
            };
            if comparison != Ordering::Equal {
                return comparison;
            }
        }
        Ordering::Equal
    }

    /// Strict continuation boundary: equality never re-emits the boundary row.
    pub fn is_after(self, candidate: &ItemReadKey, boundary: &ItemReadKey) -> bool {
        self.compare_keys(candidate, boundary) == Ordering::Greater
    }
}

impl From<&Item> for ItemReadKey {
    fn from(item: &Item) -> Self {
        Self {
            id: item.id().clone(),
            captured_at: item.captured_at().clone(),
            priority: item.priority(),
        }
    }
}

/// Operation binding is independent of ordering (list and next are distinct).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemReadOperation {
    List,
    Next,
}

/// Deterministic query binding input; not a hash, cursor, or authorization token.
///
/// The caller supplies a stable store identity and projection schema version.
/// Page size, decoded boundary, transport, and raw view spelling are excluded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemQueryFingerprintInput {
    operation: ItemReadOperation,
    store_identity: String,
    projection_schema_version: u32,
    filters: EffectiveItemFilters,
    ordering: ItemListOrdering,
    projection: ItemProjectionKind,
}

impl ItemQueryFingerprintInput {
    pub fn new(
        operation: ItemReadOperation,
        store_identity: impl Into<String>,
        projection_schema_version: u32,
        request: &ItemProjectionPageRequest,
    ) -> Result<Self, InvalidProjectionFilters> {
        Ok(Self {
            operation,
            store_identity: store_identity.into(),
            projection_schema_version,
            filters: EffectiveItemFilters::from_request(request)?,
            ordering: request.ordering,
            projection: request.projection,
        })
    }

    /// Versioned, unambiguous bytes for hashing by a later cursor adapter.
    ///
    /// Strings use u64 big-endian byte lengths and raw UTF-8; optional values
    /// have presence tags. Enum tags are explicit, never Debug output or casts.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = b"BIF:item-query:1\0".to_vec();
        bytes.push(match self.operation {
            ItemReadOperation::List => 0,
            ItemReadOperation::Next => 1,
        });
        append_string(&mut bytes, &self.store_identity);
        bytes.extend_from_slice(&self.projection_schema_version.to_be_bytes());
        bytes.push(match self.projection {
            ItemProjectionKind::Summary => 0,
            ItemProjectionKind::Work => 1,
            ItemProjectionKind::Audit => 2,
        });
        bytes.push(match self.ordering {
            ItemListOrdering::NewestFirst => 0,
            ItemListOrdering::Next => 1,
        });
        bytes.push(self.filters.statuses.len() as u8);
        for status in &self.filters.statuses {
            bytes.push(match status {
                Status::Proposed => 0,
                Status::Ready => 1,
                Status::InProgress => 2,
                Status::Blocked => 3,
                Status::Done => 4,
                Status::Rejected => 5,
            });
        }
        append_optional_string(&mut bytes, self.filters.project().map(ProjectId::as_str));
        append_optional_string(
            &mut bytes,
            self.filters.requester().map(RequesterId::as_str),
        );
        match self.filters.assignee() {
            AssigneeFilter::Any => bytes.push(0),
            AssigneeFilter::Assigned(assignee) => {
                bytes.push(1);
                append_string(&mut bytes, assignee.as_str());
            }
            AssigneeFilter::Unassigned => bytes.push(2),
        }
        match self.filters.priority() {
            None => bytes.push(0),
            Some(priority) => {
                bytes.push(1);
                bytes.push(priority_rank(Some(priority)));
            }
        }
        append_optional_string(&mut bytes, self.filters.text());
        bytes
    }
}

fn append_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn append_optional_string(bytes: &mut Vec<u8>, value: Option<&str>) {
    match value {
        None => bytes.push(0),
        Some(value) => {
            bytes.push(1);
            append_string(bytes, value);
        }
    }
}
