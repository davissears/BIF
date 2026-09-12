//! Core item types and business rules.
//!
//! This innermost module does not depend on any other crate module.

use std::{error::Error, fmt, str::FromStr};

/// Returned when an identifier contains no ASCII letters or digits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidIdentifier;

impl fmt::Display for InvalidIdentifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("identifier must contain an ASCII letter or digit")
    }
}

impl Error for InvalidIdentifier {}

#[derive(Clone, Copy)]
enum IdentifierCase {
    Upper,
    Lower,
}

fn normalize_identifier(
    input: &str,
    identifier_case: IdentifierCase,
) -> Result<String, InvalidIdentifier> {
    let mut normalized = String::with_capacity(input.len());
    let mut separator_pending = false;

    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            if separator_pending && !normalized.is_empty() {
                normalized.push('-');
            }
            separator_pending = false;
            normalized.push(match identifier_case {
                IdentifierCase::Upper => character.to_ascii_uppercase(),
                IdentifierCase::Lower => character.to_ascii_lowercase(),
            });
        } else if !normalized.is_empty() {
            separator_pending = true;
        }
    }

    if normalized.is_empty() {
        Err(InvalidIdentifier)
    } else {
        Ok(normalized)
    }
}

macro_rules! identifier {
    ($name:ident, $case:expr, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Normalizes `input` and constructs the identifier.
            pub fn new(input: impl AsRef<str>) -> Result<Self, InvalidIdentifier> {
                normalize_identifier(input.as_ref(), $case).map(Self)
            }

            /// Returns the canonical identifier.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = InvalidIdentifier;

            fn from_str(input: &str) -> Result<Self, Self::Err> {
                Self::new(input)
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidIdentifier;

            fn try_from(input: String) -> Result<Self, Self::Error> {
                Self::new(input)
            }
        }
    };
}

identifier!(
    RequesterId,
    IdentifierCase::Upper,
    "A requester identifier normalized to uppercase ASCII tokens separated by hyphens."
);
identifier!(
    ProjectId,
    IdentifierCase::Lower,
    "A project identifier normalized to lowercase ASCII tokens separated by hyphens."
);
identifier!(
    AssigneeId,
    IdentifierCase::Lower,
    "An assignee identifier normalized to lowercase ASCII tokens separated by hyphens."
);

/// Returned when an item sequence is not a positive integer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidItemSequence;

impl fmt::Display for InvalidItemSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("item sequence must be positive")
    }
}

impl Error for InvalidItemSequence {}

/// The requester- and project-scoped identity of an item.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ItemId {
    requester: RequesterId,
    project: ProjectId,
    sequence: u64,
}

impl ItemId {
    /// Constructs an item identity, rejecting sequence zero.
    pub fn new(
        requester: RequesterId,
        project: ProjectId,
        sequence: u64,
    ) -> Result<Self, InvalidItemSequence> {
        if sequence == 0 {
            return Err(InvalidItemSequence);
        }

        Ok(Self {
            requester,
            project,
            sequence,
        })
    }

    pub fn requester(&self) -> &RequesterId {
        &self.requester
    }

    pub fn project(&self) -> &ProjectId {
        &self.project
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}:{:03}",
            self.requester, self.project, self.sequence
        )
    }
}

/// Returned when immutable item content has no title.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyTitle;

impl fmt::Display for EmptyTitle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("item title must not be empty")
    }
}

impl Error for EmptyTitle {}

/// Content fixed for the lifetime of an item in BIF v1.
///
/// Acceptance criteria retain the order supplied by the requester.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemContent {
    title: String,
    description: Option<String>,
    acceptance_criteria: Vec<String>,
}

impl ItemContent {
    /// Constructs immutable content, rejecting an empty title.
    pub fn new(
        title: impl Into<String>,
        description: Option<String>,
        acceptance_criteria: Vec<String>,
    ) -> Result<Self, EmptyTitle> {
        let title = title.into();
        if title.is_empty() {
            return Err(EmptyTitle);
        }

        Ok(Self {
            title,
            description,
            acceptance_criteria,
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn acceptance_criteria(&self) -> &[String] {
        &self.acceptance_criteria
    }
}

/// The current lifecycle state of an item.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Status {
    Proposed,
    Ready,
    InProgress,
    Blocked,
    Done,
    Rejected,
}

/// Why a requested lifecycle operation could not be applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    /// The operation is not available from the item's current status.
    InvalidTransition,
    /// A required reason was absent or contained only whitespace.
    InvalidInput,
}

impl LifecycleError {
    /// Returns the stable BIF error code for this failure.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidTransition => "invalid_transition",
            Self::InvalidInput => "invalid_input",
        }
    }
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition => formatter.write_str("invalid lifecycle transition"),
            Self::InvalidInput => formatter.write_str("a non-empty reason is required"),
        }
    }
}

impl Error for LifecycleError {}

/// An item's optional triage priority.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Priority {
    P0,
    P1,
    P2,
    P3,
    P4,
}

/// A nullable triage field supplied by a caller.
///
/// Unlike `Option<T>`, this representation preserves the distinction between
/// an omitted field and an explicit request to clear it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum TriageField<T> {
    #[default]
    Omitted,
    Clear,
    Set(T),
}

/// The priority, assignment, and note components of one triage request.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Triage {
    pub priority: TriageField<Priority>,
    pub assignee: TriageField<AssigneeId>,
    pub note: Option<String>,
}

/// The effective changes made by a triage request.
///
/// This is domain data for the later event-generation step; applying triage
/// itself does not create events, change revisions, or retain notes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriageChanges {
    pub priority: Option<(Option<Priority>, Option<Priority>)>,
    pub assignee: Option<(Option<AssigneeId>, Option<AssigneeId>)>,
    pub note: Option<String>,
}

/// A lifecycle action that may form part of one compound mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleMutation {
    Approve,
    Reject { reason: String },
    Start,
    Block { reason: String },
    Resume,
    Finish,
}

/// One intended, atomically validated mutation of an item.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ItemMutation {
    pub lifecycle: Option<LifecycleMutation>,
    pub triage: Option<Triage>,
}

/// The canonical kind of a domain event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EventType {
    Captured,
    Approved,
    Rejected,
    Started,
    Blocked,
    Resumed,
    Finished,
    PriorityChanged,
    AssigneeChanged,
    NoteAdded,
}

/// A typed value on one side of an event change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventValue {
    Status(Status),
    Priority(Option<Priority>),
    Assignee(Option<AssigneeId>),
    Note(String),
}

/// A persistence-neutral event produced by a successful item mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainEvent {
    pub event_type: EventType,
    pub item_revision: Revision,
    pub before: Option<EventValue>,
    pub after: Option<EventValue>,
}

/// Why a compound item mutation could not be planned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationError {
    InvalidInput,
    InvalidTransition,
    RevisionOverflow,
}

impl MutationError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidInput | Self::RevisionOverflow => "invalid_input",
            Self::InvalidTransition => "invalid_transition",
        }
    }
}

impl fmt::Display for MutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput => formatter.write_str("mutation input is invalid"),
            Self::InvalidTransition => formatter.write_str("mutation transition is invalid"),
            Self::RevisionOverflow => formatter.write_str("item revision cannot be incremented"),
        }
    }
}

impl Error for MutationError {}

impl From<LifecycleError> for MutationError {
    fn from(error: LifecycleError) -> Self {
        match error {
            LifecycleError::InvalidInput => Self::InvalidInput,
            LifecycleError::InvalidTransition => Self::InvalidTransition,
        }
    }
}

impl From<TriageError> for MutationError {
    fn from(error: TriageError) -> Self {
        match error {
            TriageError::InvalidInput => Self::InvalidInput,
            TriageError::InvalidTransition => Self::InvalidTransition,
        }
    }
}

/// Why a triage request could not be applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriageError {
    /// No priority, assignee, or note was supplied.
    InvalidInput,
    /// An effective priority or assignee change targeted a terminal item.
    InvalidTransition,
}

impl TriageError {
    /// Returns the stable BIF error code for this failure.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::InvalidTransition => "invalid_transition",
        }
    }
}

impl fmt::Display for TriageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput => formatter.write_str("triage request must not be empty"),
            Self::InvalidTransition => {
                formatter.write_str("terminal items only permit note-only triage")
            }
        }
    }
}

impl Error for TriageError {}

/// The system in which the source context originated.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceHost {
    Delta,
    Codex,
    Local,
}

macro_rules! opaque_reference {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, PartialEq)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
    };
}

opaque_reference!(ThreadId, "An opaque source conversation identifier.");
opaque_reference!(MessageId, "An opaque source message identifier.");
opaque_reference!(SourceUrl, "An opaque source URL.");
opaque_reference!(
    RepositoryReference,
    "An opaque reference to the source repository."
);
opaque_reference!(
    RevisionReference,
    "An opaque reference to a source repository revision."
);
opaque_reference!(Timestamp, "An opaque server-generated timestamp.");

/// Returned when an item revision is not a positive integer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidRevision;

impl fmt::Display for InvalidRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("item revision must be positive")
    }
}

impl Error for InvalidRevision {}

/// A positive item revision used for optimistic concurrency.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Revision(u64);

impl Revision {
    pub fn new(value: u64) -> Result<Self, InvalidRevision> {
        if value == 0 {
            Err(InvalidRevision)
        } else {
            Ok(Self(value))
        }
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// Optional source information retained with an item.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Provenance {
    source_host: Option<SourceHost>,
    thread_id: Option<ThreadId>,
    message_id: Option<MessageId>,
    url: Option<SourceUrl>,
    repository_reference: Option<RepositoryReference>,
    revision_reference: Option<RevisionReference>,
    context_excerpt: Option<String>,
}

impl Provenance {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_host: Option<SourceHost>,
        thread_id: Option<ThreadId>,
        message_id: Option<MessageId>,
        url: Option<SourceUrl>,
        repository_reference: Option<RepositoryReference>,
        revision_reference: Option<RevisionReference>,
        context_excerpt: Option<String>,
    ) -> Self {
        Self {
            source_host,
            thread_id,
            message_id,
            url,
            repository_reference,
            revision_reference,
            context_excerpt,
        }
    }

    pub fn source_host(&self) -> Option<SourceHost> {
        self.source_host
    }

    pub fn thread_id(&self) -> Option<&ThreadId> {
        self.thread_id.as_ref()
    }

    pub fn message_id(&self) -> Option<&MessageId> {
        self.message_id.as_ref()
    }

    pub fn url(&self) -> Option<&SourceUrl> {
        self.url.as_ref()
    }

    pub fn repository_reference(&self) -> Option<&RepositoryReference> {
        self.repository_reference.as_ref()
    }

    pub fn revision_reference(&self) -> Option<&RevisionReference> {
        self.revision_reference.as_ref()
    }

    pub fn context_excerpt(&self) -> Option<&str> {
        self.context_excerpt.as_deref()
    }
}

/// The complete domain representation of a canonical BIF item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Item {
    id: ItemId,
    content: ItemContent,
    status: Status,
    priority: Option<Priority>,
    assignee: Option<AssigneeId>,
    status_reason: Option<String>,
    revision: Revision,
    captured_at: Timestamp,
    updated_at: Timestamp,
    provenance: Provenance,
}

impl Item {
    /// Captures immutable content and provenance as a newly proposed item.
    ///
    /// Identity and server-generated timestamps are supplied by the caller so
    /// this domain operation remains deterministic and independent of storage
    /// and clock concerns.
    pub fn capture(
        id: ItemId,
        content: ItemContent,
        provenance: Provenance,
        captured_at: Timestamp,
        updated_at: Timestamp,
    ) -> Self {
        Self {
            id,
            content,
            status: Status::Proposed,
            priority: None,
            assignee: None,
            status_reason: None,
            revision: Revision(1),
            captured_at,
            updated_at,
            provenance,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ItemId,
        content: ItemContent,
        status: Status,
        priority: Option<Priority>,
        assignee: Option<AssigneeId>,
        status_reason: Option<String>,
        revision: Revision,
        captured_at: Timestamp,
        updated_at: Timestamp,
        provenance: Provenance,
    ) -> Self {
        Self {
            id,
            content,
            status,
            priority,
            assignee,
            status_reason,
            revision,
            captured_at,
            updated_at,
            provenance,
        }
    }

    pub fn id(&self) -> &ItemId {
        &self.id
    }

    pub fn content(&self) -> &ItemContent {
        &self.content
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn priority(&self) -> Option<Priority> {
        self.priority
    }

    pub fn assignee(&self) -> Option<&AssigneeId> {
        self.assignee.as_ref()
    }

    pub fn status_reason(&self) -> Option<&str> {
        self.status_reason.as_deref()
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn captured_at(&self) -> &Timestamp {
        &self.captured_at
    }

    pub fn updated_at(&self) -> &Timestamp {
        &self.updated_at
    }

    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Moves a proposed item to ready.
    pub fn approve(&mut self) -> Result<(), LifecycleError> {
        self.transition(Status::Proposed, Status::Ready, None)
    }

    /// Rejects a proposed item with a non-empty reason.
    pub fn reject(&mut self, reason: Option<&str>) -> Result<(), LifecycleError> {
        self.transition_with_required_reason(Status::Proposed, Status::Rejected, reason)
    }

    /// Starts a ready item.
    pub fn start(&mut self) -> Result<(), LifecycleError> {
        self.transition(Status::Ready, Status::InProgress, None)
    }

    /// Blocks an in-progress item with a non-empty reason.
    pub fn block(&mut self, reason: Option<&str>) -> Result<(), LifecycleError> {
        self.transition_with_required_reason(Status::InProgress, Status::Blocked, reason)
    }

    /// Resumes a blocked item.
    pub fn resume(&mut self) -> Result<(), LifecycleError> {
        self.transition(Status::Blocked, Status::InProgress, None)
    }

    /// Finishes an in-progress item.
    pub fn finish(&mut self) -> Result<(), LifecycleError> {
        self.transition(Status::InProgress, Status::Done, None)
    }

    /// Applies priority and assignee changes and validates an optional note.
    ///
    /// The returned value describes only effective changes. Notes are returned
    /// for later event generation rather than stored on the item.
    pub fn triage(&mut self, triage: Triage) -> Result<TriageChanges, TriageError> {
        let Triage {
            priority,
            assignee,
            note,
        } = triage;
        if matches!(priority, TriageField::Omitted)
            && matches!(assignee, TriageField::Omitted)
            && note.is_none()
        {
            return Err(TriageError::InvalidInput);
        }

        let next_priority = match priority {
            TriageField::Omitted => self.priority,
            TriageField::Clear => None,
            TriageField::Set(priority) => Some(priority),
        };
        let next_assignee = match assignee {
            TriageField::Omitted => self.assignee.clone(),
            TriageField::Clear => None,
            TriageField::Set(assignee) => Some(assignee),
        };
        let priority_change =
            (next_priority != self.priority).then_some((self.priority, next_priority));
        let assignee_change = (next_assignee != self.assignee)
            .then(|| (self.assignee.clone(), next_assignee.clone()));

        if matches!(self.status, Status::Done | Status::Rejected)
            && (priority_change.is_some() || assignee_change.is_some())
        {
            return Err(TriageError::InvalidTransition);
        }

        self.priority = next_priority;
        self.assignee = next_assignee;
        Ok(TriageChanges {
            priority: priority_change,
            assignee: assignee_change,
            note,
        })
    }

    /// Validates and applies one compound mutation, returning its ordered events.
    ///
    /// Validation is performed on a copy. The item is changed only after every
    /// component succeeds. A mutation with no effective changes is a successful
    /// no-op: it returns no events and does not increment the revision.
    pub fn apply_mutation(
        &mut self,
        mutation: ItemMutation,
    ) -> Result<Vec<DomainEvent>, MutationError> {
        if mutation.lifecycle.is_none() && mutation.triage.is_none() {
            return Err(MutationError::InvalidInput);
        }

        let mut planned = self.clone();
        let mut changes = Vec::new();

        if let Some(lifecycle) = mutation.lifecycle {
            let before = planned.status;
            let event_type = match lifecycle {
                LifecycleMutation::Approve => {
                    planned.approve()?;
                    EventType::Approved
                }
                LifecycleMutation::Reject { reason } => {
                    planned.reject(Some(&reason))?;
                    EventType::Rejected
                }
                LifecycleMutation::Start => {
                    planned.start()?;
                    EventType::Started
                }
                LifecycleMutation::Block { reason } => {
                    planned.block(Some(&reason))?;
                    EventType::Blocked
                }
                LifecycleMutation::Resume => {
                    planned.resume()?;
                    EventType::Resumed
                }
                LifecycleMutation::Finish => {
                    planned.finish()?;
                    EventType::Finished
                }
            };
            changes.push((
                event_type,
                Some(EventValue::Status(before)),
                Some(EventValue::Status(planned.status)),
            ));
        }

        if let Some(triage) = mutation.triage {
            let triage_changes = planned.triage(triage)?;
            if let Some((before, after)) = triage_changes.priority {
                changes.push((
                    EventType::PriorityChanged,
                    Some(EventValue::Priority(before)),
                    Some(EventValue::Priority(after)),
                ));
            }
            if let Some((before, after)) = triage_changes.assignee {
                changes.push((
                    EventType::AssigneeChanged,
                    Some(EventValue::Assignee(before)),
                    Some(EventValue::Assignee(after)),
                ));
            }
            if let Some(note) = triage_changes.note {
                changes.push((EventType::NoteAdded, None, Some(EventValue::Note(note))));
            }
        }

        if changes.is_empty() {
            return Ok(Vec::new());
        }

        let revision = Revision(
            self.revision
                .get()
                .checked_add(1)
                .ok_or(MutationError::RevisionOverflow)?,
        );
        planned.revision = revision;
        let events = changes
            .into_iter()
            .map(|(event_type, before, after)| DomainEvent {
                event_type,
                item_revision: revision,
                before,
                after,
            })
            .collect();
        *self = planned;
        Ok(events)
    }

    /// Records the application-supplied time of a successfully persisted change.
    pub(crate) fn set_updated_at(&mut self, updated_at: Timestamp) {
        self.updated_at = updated_at;
    }

    fn transition_with_required_reason(
        &mut self,
        from: Status,
        to: Status,
        reason: Option<&str>,
    ) -> Result<(), LifecycleError> {
        if self.status != from {
            return Err(LifecycleError::InvalidTransition);
        }
        let reason = reason
            .filter(|reason| reason.chars().any(|character| !character.is_whitespace()))
            .ok_or(LifecycleError::InvalidInput)?;

        self.status = to;
        self.status_reason = Some(reason.to_owned());
        Ok(())
    }

    fn transition(
        &mut self,
        from: Status,
        to: Status,
        reason: Option<String>,
    ) -> Result<(), LifecycleError> {
        if self.status != from {
            return Err(LifecycleError::InvalidTransition);
        }

        self.status = to;
        self.status_reason = reason;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AssigneeId, DomainEvent, EventType, EventValue, Item, ItemContent, ItemId, ItemMutation,
        LifecycleError, LifecycleMutation, MessageId, MutationError, Priority, ProjectId,
        Provenance, RepositoryReference, RequesterId, Revision, RevisionReference, SourceHost,
        SourceUrl, Status, ThreadId, Timestamp, Triage, TriageError, TriageField,
    };

    #[test]
    fn item_content_rejects_only_an_empty_title() {
        assert!(ItemContent::new("", None, Vec::new()).is_err());

        let content = ItemContent::new("Title", None, Vec::new()).expect("valid content");
        assert_eq!(content.title(), "Title");
        assert_eq!(content.description(), None);
        assert!(content.acceptance_criteria().is_empty());
    }

    #[test]
    fn revision_must_be_positive() {
        assert!(Revision::new(0).is_err());
        assert_eq!(Revision::new(1).unwrap().get(), 1);
    }

    #[test]
    fn item_preserves_ordered_content_and_nullable_state_and_provenance() {
        let criteria = vec!["first".to_owned(), "second".to_owned()];
        let content =
            ItemContent::new("Title", None, criteria.clone()).expect("non-empty title is valid");
        let id = ItemId::new(
            RequesterId::new("DAVIS").unwrap(),
            ProjectId::new("bif").unwrap(),
            1,
        )
        .unwrap();
        let item = Item::new(
            id,
            content,
            Status::Proposed,
            None,
            None,
            None,
            Revision::new(1).unwrap(),
            Timestamp::new("2025-01-02T03:04:05Z"),
            Timestamp::new("2025-01-02T03:04:05Z"),
            Provenance::default(),
        );

        assert_eq!(item.content().acceptance_criteria(), criteria);
        assert_eq!(item.priority(), None);
        assert_eq!(item.assignee(), None);
        assert_eq!(item.status_reason(), None);
        assert_eq!(item.provenance().source_host(), None);
        assert_eq!(item.provenance().context_excerpt(), None);
        assert_eq!(item.provenance().repository_reference(), None);
        assert_eq!(item.provenance().revision_reference(), None);
    }

    #[test]
    fn capture_sets_every_initial_item_field() {
        let id = ItemId::new(
            RequesterId::new("DAVIS").unwrap(),
            ProjectId::new("bif").unwrap(),
            42,
        )
        .unwrap();
        let content = ItemContent::new(
            "Capture normalization",
            Some("Construct the canonical initial item".to_owned()),
            vec!["Every field is deterministic".to_owned()],
        )
        .unwrap();
        let provenance = Provenance::new(
            Some(SourceHost::Delta),
            Some(ThreadId::new("thread-123")),
            Some(MessageId::new("message-456")),
            Some(SourceUrl::new("https://example.test/thread/123")),
            Some(RepositoryReference::new("example/bif")),
            Some(RevisionReference::new("abc123")),
            Some("The source context".to_owned()),
        );
        let captured_at = Timestamp::new("2025-02-03T04:05:06Z");
        let updated_at = Timestamp::new("2025-02-03T04:05:07Z");

        let item = Item::capture(
            id.clone(),
            content.clone(),
            provenance.clone(),
            captured_at.clone(),
            updated_at.clone(),
        );

        assert_eq!(item.id(), &id);
        assert_eq!(item.content(), &content);
        assert_eq!(item.status(), Status::Proposed);
        assert_eq!(item.priority(), None);
        assert_eq!(item.assignee(), None);
        assert_eq!(item.status_reason(), None);
        assert_eq!(item.revision().get(), 1);
        assert_eq!(item.captured_at(), &captured_at);
        assert_eq!(item.updated_at(), &updated_at);
        assert_eq!(item.provenance(), &provenance);
    }

    #[test]
    fn lifecycle_operation_status_cases_follow_fixture_without_partial_mutation() {
        let mut count = 0;
        for case in fixture_cases("operation_status_cases") {
            let operation = fixture_string(case, "operation");
            let mut item = item_with_status(fixture_status(fixture_string(case, "from")));
            let before = item.clone();
            let result = apply_fixture_operation(&mut item, operation, default_reason(operation));
            let allowed = fixture_bool(case, "allowed");

            if allowed {
                assert_eq!(
                    result,
                    Ok(()),
                    "{operation} should be allowed from {:?}",
                    before.status()
                );
                assert_eq!(
                    item.status(),
                    fixture_status(fixture_string(case, "to")),
                    "{operation} result"
                );
            } else {
                assert_eq!(
                    result.map_err(LifecycleError::code),
                    Err(fixture_string(case, "error")),
                    "{operation} from {:?}",
                    before.status()
                );
                assert_eq!(item, before, "failed {operation} mutated the item");
            }
            count += 1;
        }
        assert_eq!(count, 36, "all operation/status fixture cases must run");
    }

    #[test]
    fn lifecycle_reason_cases_follow_fixture_without_partial_mutation() {
        let mut count = 0;
        for case in fixture_cases("reason_cases") {
            let operation = fixture_string(case, "operation");
            let reason = fixture_optional_string(case, "reason");
            let mut item = item_with_status(fixture_status(fixture_string(case, "from")));
            let before = item.clone();
            let result = apply_fixture_operation(&mut item, operation, reason.as_deref());
            let allowed = fixture_bool(case, "allowed");

            if allowed {
                assert_eq!(result, Ok(()), "{operation} with reason {reason:?}");
                assert_eq!(item.status(), fixture_status(fixture_string(case, "to")));
                assert_eq!(item.status_reason(), reason.as_deref());
            } else {
                assert_eq!(
                    result.map_err(LifecycleError::code),
                    Err(fixture_string(case, "error"))
                );
                assert_eq!(item, before, "failed {operation} mutated the item");
            }
            count += 1;
        }
        assert_eq!(count, 8, "all reason fixture cases must run");
    }

    #[test]
    fn terminal_state_cases_follow_fixture_without_partial_mutation() {
        let mut count = 0;
        for case in fixture_cases("terminal_state_cases") {
            let operation = fixture_string(case, "operation");
            let mut item = item_with_status(fixture_status(fixture_string(case, "from")));
            let before = item.clone();
            let result = apply_fixture_operation(&mut item, operation, None);

            assert!(!fixture_bool(case, "allowed"));
            assert_eq!(
                result.map_err(LifecycleError::code),
                Err(fixture_string(case, "error"))
            );
            assert_eq!(item, before, "terminal {operation} mutated the item");
            count += 1;
        }
        assert_eq!(count, 12, "all terminal fixture cases must run");
    }

    #[test]
    fn lifecycle_fixture_status_cross_product_is_complete_and_consistent() {
        let allowed: Vec<_> = fixture_cases("operations")
            .map(|case| (fixture_string(case, "from"), fixture_string(case, "to")))
            .collect();
        assert_eq!(allowed.len(), 6, "all operation definitions must run");
        let mut count = 0;
        for case in fixture_cases("status_cross_product") {
            let pair = (fixture_string(case, "from"), fixture_string(case, "to"));
            assert_eq!(
                fixture_bool(case, "allowed"),
                allowed.contains(&pair),
                "fixture transition {pair:?}"
            );
            count += 1;
        }
        assert_eq!(count, 36, "complete six-by-six status fixture must run");
    }

    #[test]
    fn triage_distinguishes_omitted_fields_from_explicit_clears() {
        let mut item = item_with_status(Status::Ready);
        item.triage(Triage {
            priority: TriageField::Set(Priority::P1),
            assignee: TriageField::Set(AssigneeId::new("Taylor").unwrap()),
            note: None,
        })
        .unwrap();

        let changes = item
            .triage(Triage {
                priority: TriageField::Clear,
                assignee: TriageField::Omitted,
                note: None,
            })
            .unwrap();

        assert_eq!(item.priority(), None);
        assert_eq!(item.assignee().map(AssigneeId::as_str), Some("taylor"));
        assert_eq!(
            changes.priority,
            Some((Some(Priority::P1), None)),
            "explicit null clears priority"
        );
        assert_eq!(changes.assignee, None, "omitted assignee is unchanged");

        item.triage(Triage {
            priority: TriageField::Omitted,
            assignee: TriageField::Clear,
            note: None,
        })
        .unwrap();
        assert_eq!(item.assignee(), None, "explicit null clears assignee");
    }

    #[test]
    fn same_value_triage_is_a_valid_no_op() {
        let mut item = item_with_status(Status::Ready);
        let assignee = AssigneeId::new("taylor").unwrap();
        item.triage(Triage {
            priority: TriageField::Set(Priority::P2),
            assignee: TriageField::Set(assignee.clone()),
            note: None,
        })
        .unwrap();
        let before = item.clone();

        let changes = item
            .triage(Triage {
                priority: TriageField::Set(Priority::P2),
                assignee: TriageField::Set(assignee),
                note: None,
            })
            .unwrap();

        assert_eq!(changes.priority, None);
        assert_eq!(changes.assignee, None);
        assert_eq!(item, before);
    }

    #[test]
    fn empty_triage_is_invalid_input_without_mutation() {
        let mut item = item_with_status(Status::Ready);
        let before = item.clone();

        assert_eq!(
            item.triage(Triage::default()).map_err(TriageError::code),
            Err("invalid_input")
        );
        assert_eq!(item, before);
    }

    #[test]
    fn note_only_triage_is_allowed_in_every_status_without_state_changes() {
        for status in [
            Status::Proposed,
            Status::Ready,
            Status::InProgress,
            Status::Blocked,
            Status::Done,
            Status::Rejected,
        ] {
            let mut item = item_with_status(status);
            let before = item.clone();
            let changes = item
                .triage(Triage {
                    note: Some("clarification".to_owned()),
                    ..Triage::default()
                })
                .unwrap();

            assert_eq!(changes.note.as_deref(), Some("clarification"));
            assert_eq!(changes.priority, None);
            assert_eq!(changes.assignee, None);
            assert_eq!(item, before, "note-only triage changed {status:?}");
        }
    }

    #[test]
    fn terminal_items_reject_effective_priority_and_assignee_changes_atomically() {
        for status in [Status::Done, Status::Rejected] {
            let mut item = item_with_status(status);
            let before = item.clone();
            let result = item.triage(Triage {
                priority: TriageField::Set(Priority::P0),
                assignee: TriageField::Set(AssigneeId::new("taylor").unwrap()),
                note: Some("must not be accepted partially".to_owned()),
            });

            assert_eq!(result.map_err(TriageError::code), Err("invalid_transition"));
            assert_eq!(item, before, "failed terminal triage mutated {status:?}");
        }
    }

    #[test]
    fn rejection_cannot_include_unrelated_priority_or_assignment_changes() {
        for forbidden in [
            Triage {
                priority: TriageField::Set(Priority::P1),
                ..Triage::default()
            },
            Triage {
                assignee: TriageField::Set(AssigneeId::new("taylor").unwrap()),
                ..Triage::default()
            },
        ] {
            let mut item = item_with_status(Status::Proposed);
            item.reject(Some("duplicate")).unwrap();
            let before = item.clone();

            assert_eq!(
                item.triage(forbidden).map_err(TriageError::code),
                Err("invalid_transition")
            );
            assert_eq!(item, before);
        }
    }

    #[test]
    fn triage_does_not_change_revision_or_lifecycle_state() {
        let mut item = item_with_status(Status::Blocked);
        let revision = item.revision();
        let reason = item.status_reason().map(str::to_owned);

        item.triage(Triage {
            priority: TriageField::Set(Priority::P3),
            assignee: TriageField::Omitted,
            note: Some("keep lifecycle intact".to_owned()),
        })
        .unwrap();

        assert_eq!(item.status(), Status::Blocked);
        assert_eq!(item.status_reason(), reason.as_deref());
        assert_eq!(item.revision(), revision);
    }

    #[test]
    fn compound_approve_triage_produces_ordered_events_at_one_revision() {
        let mut item = item_with_status(Status::Proposed);
        let assignee = AssigneeId::new("Taylor").unwrap();

        let events = item
            .apply_mutation(ItemMutation {
                lifecycle: Some(LifecycleMutation::Approve),
                triage: Some(Triage {
                    priority: TriageField::Set(Priority::P1),
                    assignee: TriageField::Set(assignee.clone()),
                    note: Some("ready to build".to_owned()),
                }),
            })
            .unwrap();

        assert_eq!(item.revision(), Revision::new(8).unwrap());
        assert_eq!(item.status(), Status::Ready);
        assert_eq!(item.priority(), Some(Priority::P1));
        assert_eq!(item.assignee(), Some(&assignee));
        assert_eq!(
            events,
            vec![
                DomainEvent {
                    event_type: EventType::Approved,
                    item_revision: Revision::new(8).unwrap(),
                    before: Some(EventValue::Status(Status::Proposed)),
                    after: Some(EventValue::Status(Status::Ready)),
                },
                DomainEvent {
                    event_type: EventType::PriorityChanged,
                    item_revision: Revision::new(8).unwrap(),
                    before: Some(EventValue::Priority(None)),
                    after: Some(EventValue::Priority(Some(Priority::P1))),
                },
                DomainEvent {
                    event_type: EventType::AssigneeChanged,
                    item_revision: Revision::new(8).unwrap(),
                    before: Some(EventValue::Assignee(None)),
                    after: Some(EventValue::Assignee(Some(assignee))),
                },
                DomainEvent {
                    event_type: EventType::NoteAdded,
                    item_revision: Revision::new(8).unwrap(),
                    before: None,
                    after: Some(EventValue::Note("ready to build".to_owned())),
                },
            ]
        );
    }

    #[test]
    fn failed_later_component_leaves_compound_mutation_atomic() {
        let mut item = item_with_status(Status::Proposed);
        let before = item.clone();

        let result = item.apply_mutation(ItemMutation {
            lifecycle: Some(LifecycleMutation::Reject {
                reason: "duplicate".to_owned(),
            }),
            triage: Some(Triage {
                priority: TriageField::Set(Priority::P1),
                ..Triage::default()
            }),
        });

        assert_eq!(result, Err(MutationError::InvalidTransition));
        assert_eq!(item, before);
    }

    #[test]
    fn effective_no_op_has_no_events_and_does_not_increment_revision() {
        let mut item = item_with_status(Status::Ready);
        item.triage(Triage {
            priority: TriageField::Set(Priority::P2),
            ..Triage::default()
        })
        .unwrap();
        let before = item.clone();

        let events = item
            .apply_mutation(ItemMutation {
                triage: Some(Triage {
                    priority: TriageField::Set(Priority::P2),
                    ..Triage::default()
                }),
                ..ItemMutation::default()
            })
            .unwrap();

        assert!(events.is_empty());
        assert_eq!(item, before);
    }

    #[test]
    fn empty_mutation_is_rejected_without_mutation() {
        let mut item = item_with_status(Status::Ready);
        let before = item.clone();

        assert_eq!(
            item.apply_mutation(ItemMutation::default()),
            Err(MutationError::InvalidInput)
        );
        assert_eq!(item, before);
    }

    fn item_with_status(status: Status) -> Item {
        Item::new(
            ItemId::new(
                RequesterId::new("DAVIS").unwrap(),
                ProjectId::new("bif").unwrap(),
                1,
            )
            .unwrap(),
            ItemContent::new("Lifecycle fixture", None, Vec::new()).unwrap(),
            status,
            None,
            None,
            None,
            Revision::new(7).unwrap(),
            Timestamp::new("captured"),
            Timestamp::new("updated"),
            Provenance::default(),
        )
    }

    fn apply_fixture_operation(
        item: &mut Item,
        operation: &str,
        reason: Option<&str>,
    ) -> Result<(), LifecycleError> {
        match operation {
            "approve" => item.approve(),
            "reject" => item.reject(reason),
            "start" => item.start(),
            "block" => item.block(reason),
            "resume" => item.resume(),
            "finish" => item.finish(),
            unexpected => panic!("unexpected lifecycle operation: {unexpected}"),
        }
    }

    fn default_reason(operation: &str) -> Option<&'static str> {
        matches!(operation, "reject" | "block").then_some("fixture reason")
    }

    fn fixture_status(status: &str) -> Status {
        match status {
            "proposed" => Status::Proposed,
            "ready" => Status::Ready,
            "in_progress" => Status::InProgress,
            "blocked" => Status::Blocked,
            "done" => Status::Done,
            "rejected" => Status::Rejected,
            unexpected => panic!("unexpected fixture status: {unexpected}"),
        }
    }

    fn fixture_cases(section: &str) -> impl Iterator<Item = &'static str> {
        let fixture = include_str!("../docs/fixtures/bif-v1-lifecycle.json");
        let marker = format!("\"{section}\": [");
        fixture
            .split_once(&marker)
            .unwrap_or_else(|| panic!("fixture must contain {section}"))
            .1
            .split_once("\n  ]")
            .unwrap_or_else(|| panic!("fixture section {section} must end"))
            .0
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('{'))
    }

    fn fixture_string<'a>(case: &'a str, key: &str) -> &'a str {
        let value = fixture_value(case, key);
        value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or_else(|| panic!("{key} must be a JSON string in {case}"))
    }

    fn fixture_optional_string(case: &str, key: &str) -> Option<String> {
        let value = fixture_value(case, key);
        if value == "null" {
            return None;
        }
        let escaped = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or_else(|| panic!("{key} must be a string or null in {case}"));
        let mut decoded = String::new();
        let mut characters = escaped.chars();
        while let Some(character) = characters.next() {
            if character == '\\' {
                decoded.push(match characters.next().expect("complete JSON escape") {
                    't' => '\t',
                    'n' => '\n',
                    '"' => '"',
                    '\\' => '\\',
                    unexpected => panic!("unsupported JSON escape: {unexpected}"),
                });
            } else {
                decoded.push(character);
            }
        }
        Some(decoded)
    }

    fn fixture_bool(case: &str, key: &str) -> bool {
        match fixture_value(case, key) {
            "true" => true,
            "false" => false,
            unexpected => panic!("{key} must be a JSON boolean, got {unexpected}"),
        }
    }

    fn fixture_value<'a>(case: &'a str, key: &str) -> &'a str {
        let marker = format!("\"{key}\": ");
        case.split_once(&marker)
            .unwrap_or_else(|| panic!("fixture case must contain {key}: {case}"))
            .1
            .split([',', '}'])
            .next()
            .expect("fixture value")
    }

    #[test]
    fn canonical_identifier_cases() {
        let fixture = include_str!("../docs/fixtures/bif-v1-canonical.json");
        let identifiers = fixture
            .split_once("\"identifiers\": {")
            .expect("fixture must contain identifiers")
            .1
            .split_once("\"item_display_id\": {")
            .expect("identifiers must precede item_display_id")
            .0;
        let mut kind = None;
        let mut input = None;
        let mut valid = None;
        let mut canonical = None;
        let mut case_count = 0;

        for line in identifiers.lines().map(str::trim) {
            match line {
                "\"requester\": {" => kind = Some("requester"),
                "\"project\": {" => kind = Some("project"),
                "\"assignee\": {" => kind = Some("assignee"),
                _ if line.starts_with("\"input\": ") => input = json_string_value(line),
                "\"valid\": true," | "\"valid\": true" => valid = Some(true),
                "\"valid\": false," | "\"valid\": false" => valid = Some(false),
                _ if line.starts_with("\"canonical\": ") => canonical = json_string_value(line),
                "}," | "}" if input.is_some() && valid.is_some() => {
                    let kind = kind.expect("identifier case must have a kind");
                    let input = input.take().expect("case must have input");
                    let actual = match kind {
                        "requester" => RequesterId::new(input).map(|id| id.to_string()),
                        "project" => ProjectId::new(input).map(|id| id.to_string()),
                        "assignee" => AssigneeId::new(input).map(|id| id.to_string()),
                        unexpected => panic!("unexpected identifier kind: {unexpected}"),
                    };

                    if valid.take().expect("case must have validity") {
                        assert_eq!(
                            actual.as_deref(),
                            Ok(canonical
                                .take()
                                .as_deref()
                                .expect("valid case must have a canonical value")),
                            "{kind} input {input:?}"
                        );
                    } else {
                        assert!(actual.is_err(), "{kind} input {input:?}");
                        canonical = None;
                    }
                    case_count += 1;
                }
                _ => {}
            }
        }

        assert_eq!(case_count, 19, "all canonical identifier cases must run");
    }

    #[test]
    fn canonical_item_id_cases() {
        let fixture = include_str!("../docs/fixtures/bif-v1-canonical.json");
        let cases = fixture
            .split_once("\"item_display_id\": {")
            .expect("fixture must contain item_display_id")
            .1
            .split_once("\"cases\": [")
            .expect("item_display_id must contain cases")
            .1
            .split_once("\"invalid_display_strings\": [")
            .expect("cases must precede invalid_display_strings")
            .0;
        let mut requester = None;
        let mut project = None;
        let mut sequence = None;
        let mut valid = None;
        let mut display = None;
        let mut case_count = 0;

        for line in cases.lines().map(str::trim) {
            if line.starts_with("\"requester\": ") {
                requester = json_string_value(line);
            } else if line.starts_with("\"project\": ") {
                project = json_string_value(line);
            } else if line.starts_with("\"sequence\": ") {
                sequence = json_u64_value(line);
            } else if matches!(line, "\"valid\": true," | "\"valid\": true") {
                valid = Some(true);
            } else if matches!(line, "\"valid\": false," | "\"valid\": false") {
                valid = Some(false);
            } else if line.starts_with("\"display\": ") {
                display = json_string_value(line);
            } else if matches!(line, "}," | "}") && requester.is_some() && valid.is_some() {
                let requester_text = requester.take().expect("case must have requester");
                let project_text = project.take().expect("case must have project");
                let sequence = sequence.take().expect("case must have sequence");

                // Item-ID fixture components are canonical, unlike identifier inputs.
                let canonical_requester = RequesterId::new(requester_text)
                    .ok()
                    .filter(|id| id.as_str() == requester_text);
                let canonical_project = ProjectId::new(project_text)
                    .ok()
                    .filter(|id| id.as_str() == project_text);
                let actual = canonical_requester
                    .and_then(|requester| canonical_project.map(|project| (requester, project)))
                    .and_then(|(requester, project)| {
                        ItemId::new(requester, project, sequence).ok()
                    });

                if valid.take().expect("case must have validity") {
                    assert_eq!(
                        actual.as_ref().map(ToString::to_string).as_deref(),
                        display.take(),
                        "item ID case for {requester_text}:{project_text}:{sequence}"
                    );
                } else {
                    assert!(
                        actual.is_none(),
                        "invalid item ID case was accepted: {requester_text}:{project_text}:{sequence}"
                    );
                    display = None;
                }
                case_count += 1;
            }
        }

        assert_eq!(case_count, 7, "all canonical item-ID cases must run");
    }

    fn json_string_value(line: &str) -> Option<&str> {
        let value = line.split_once(':')?.1.trim().trim_end_matches(',');
        value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
    }

    fn json_u64_value(line: &str) -> Option<u64> {
        line.split_once(':')?
            .1
            .trim()
            .trim_end_matches(',')
            .parse()
            .ok()
    }
}
