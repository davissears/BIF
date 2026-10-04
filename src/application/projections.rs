//! Complete typed replacements; nullable fields and empty criteria stay present.
//! No serialization or transport representation belongs in these read models.

use crate::domain::{
    AssigneeId, Item, ItemId, Priority, ProjectId, Provenance, RequesterId, Revision, Status,
    Timestamp,
};

/// Queue-selection fields for an item.
///
/// This is a complete summary projection, not a partial [`Item`] that callers
/// may hydrate implicitly. In particular, it cannot carry descriptions,
/// acceptance criteria, status reasons, or provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemSummary {
    pub id: ItemId,
    pub title: String,
    pub status: Status,
    pub priority: Option<Priority>,
    pub assignee: Option<AssigneeId>,
    pub revision: Revision,
}

impl ItemSummary {
    pub fn new(
        id: ItemId,
        title: impl Into<String>,
        status: Status,
        priority: Option<Priority>,
        assignee: Option<AssigneeId>,
        revision: Revision,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            status,
            priority,
            assignee,
            revision,
        }
    }
}

impl From<&Item> for ItemSummary {
    fn from(item: &Item) -> Self {
        Self::new(
            item.id().clone(),
            item.content().title(),
            item.status(),
            item.priority(),
            item.assignee().cloned(),
            item.revision(),
        )
    }
}

/// Current item content needed to execute work, without audit metadata.
///
/// A work projection replaces a summary projection; it is not a patch to be
/// merged into one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemWork {
    pub id: ItemId,
    pub title: String,
    pub status: Status,
    pub priority: Option<Priority>,
    pub assignee: Option<AssigneeId>,
    pub revision: Revision,
    pub description: Option<String>,
    pub acceptance_criteria: Vec<String>,
    pub status_reason: Option<String>,
}

impl ItemWork {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ItemId,
        title: impl Into<String>,
        status: Status,
        priority: Option<Priority>,
        assignee: Option<AssigneeId>,
        revision: Revision,
        description: Option<String>,
        acceptance_criteria: Vec<String>,
        status_reason: Option<String>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            status,
            priority,
            assignee,
            revision,
            description,
            acceptance_criteria,
            status_reason,
        }
    }
}

impl From<&Item> for ItemWork {
    fn from(item: &Item) -> Self {
        Self::new(
            item.id().clone(),
            item.content().title(),
            item.status(),
            item.priority(),
            item.assignee().cloned(),
            item.revision(),
            item.content().description().map(str::to_owned),
            item.content().acceptance_criteria().to_vec(),
            item.status_reason().map(str::to_owned),
        )
    }
}

/// Complete current item state and provenance at one read snapshot.
///
/// History is deliberately absent and remains a separately paginated result.
/// Requester, project, and sequence are always derived from the canonical `id`,
/// so replacing it cannot leave projection identity internally inconsistent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemAudit {
    pub id: ItemId,
    pub title: String,
    pub status: Status,
    pub priority: Option<Priority>,
    pub assignee: Option<AssigneeId>,
    pub revision: Revision,
    pub description: Option<String>,
    pub acceptance_criteria: Vec<String>,
    pub status_reason: Option<String>,
    pub captured_at: Timestamp,
    pub updated_at: Timestamp,
    pub provenance: Provenance,
}

impl ItemAudit {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ItemId,
        title: impl Into<String>,
        status: Status,
        priority: Option<Priority>,
        assignee: Option<AssigneeId>,
        revision: Revision,
        description: Option<String>,
        acceptance_criteria: Vec<String>,
        status_reason: Option<String>,
        captured_at: Timestamp,
        updated_at: Timestamp,
        provenance: Provenance,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            status,
            priority,
            assignee,
            revision,
            description,
            acceptance_criteria,
            status_reason,
            captured_at,
            updated_at,
            provenance,
        }
    }

    /// The requester encoded in the current canonical item ID.
    pub fn requester(&self) -> &RequesterId {
        self.id.requester()
    }

    /// The project encoded in the current canonical item ID.
    pub fn project(&self) -> &ProjectId {
        self.id.project()
    }

    /// The sequence encoded in the current canonical item ID.
    pub fn sequence(&self) -> u64 {
        self.id.sequence()
    }
}

impl From<&Item> for ItemAudit {
    fn from(item: &Item) -> Self {
        Self::new(
            item.id().clone(),
            item.content().title(),
            item.status(),
            item.priority(),
            item.assignee().cloned(),
            item.revision(),
            item.content().description().map(str::to_owned),
            item.content().acceptance_criteria().to_vec(),
            item.status_reason().map(str::to_owned),
            item.captured_at().clone(),
            item.updated_at().clone(),
            item.provenance().clone(),
        )
    }
}
