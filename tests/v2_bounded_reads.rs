use std::{
    cell::{Cell, RefCell},
    io,
};

use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, Command, EventActor, EventExecution, Execution,
        HistoryOrdering, HistoryPageRequest, HistoryReadKey, ItemAudit, ItemHistoryError,
        ItemHistoryEvent, ItemHistoryPageStore, ItemHistoryStoreError, ItemListFilters,
        ItemListOrdering, ItemProjection, ItemProjectionKind, ItemProjectionPageRequest,
        ItemProjectionStore, ItemReadKey, ItemSummary, ItemWork, ObservedExecution,
        ProjectedItemRow, ProjectionGetRequest, ProjectionPageError, ReadItemError, ReadPage,
        ReadPageRequest, read_item_history_page, read_item_projection, read_item_projection_page,
    },
    domain::{
        AssigneeId, EventType, ItemId, NamedView, Priority, ProjectId, Provenance, RequesterId,
        Revision, Status, Timestamp,
    },
};

fn id() -> ItemId {
    ItemId::new(
        RequesterId::new("DAVIS").unwrap(),
        ProjectId::new("bif").unwrap(),
        9_007_199_254_740_993,
    )
    .unwrap()
}

fn authorization() -> AuthorizationRequest<'static> {
    AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: "DAVIS",
            surface: "test",
            host: "local",
        },
        execution: Execution::Direct {
            surface: "test",
            host: "local",
        },
        observed_execution: ObservedExecution::Direct,
        command: Command::Read,
        human_authorization: None,
    }
}

fn projection(kind: ItemProjectionKind) -> ItemProjection {
    // A direct storage row: this fake never constructs or loads a domain Item.
    match kind {
        ItemProjectionKind::Summary => ItemProjection::Summary(ItemSummary::new(
            id(),
            "Row",
            Status::Ready,
            Some(Priority::P4),
            None,
            Revision::new(1).unwrap(),
        )),
        ItemProjectionKind::Work => ItemProjection::Work(ItemWork::new(
            id(),
            "Row",
            Status::Ready,
            Some(Priority::P4),
            None,
            Revision::new(1).unwrap(),
            None,
            vec![],
            None,
        )),
        ItemProjectionKind::Audit => ItemProjection::Audit(ItemAudit::new(
            id(),
            "Row",
            Status::Ready,
            Some(Priority::P4),
            None,
            Revision::new(1).unwrap(),
            None,
            vec![],
            None,
            Timestamp::new("2025-01-02T03:04:05Z"),
            Timestamp::new("2025-01-02T03:04:05Z"),
            Provenance::default(),
        )),
    }
}

fn key() -> ItemReadKey {
    ItemReadKey {
        id: id(),
        captured_at: Timestamp::new("2025-01-02T03:04:05Z"),
        priority: Some(Priority::P4),
    }
}

fn item_request(kind: ItemProjectionKind, ordering: ItemListOrdering) -> ItemProjectionPageRequest {
    ItemProjectionPageRequest {
        view: NamedView::Mine,
        configured_requester: RequesterId::new("DAVIS").unwrap(),
        filters: ItemListFilters {
            project: Some(ProjectId::new("bif").unwrap()),
            priority: Some(Priority::P4),
            ..ItemListFilters::default()
        },
        projection: kind,
        ordering,
        page: ReadPageRequest::new(1, Some(key())).unwrap(),
    }
}

#[derive(Debug, PartialEq)]
enum Call {
    Get(ProjectionGetRequest),
    Items(ItemProjectionPageRequest),
    History(HistoryPageRequest),
}

// Implementing only required bounded ports is enough; no v1/full-loader port.
#[derive(Default)]
struct FakeStore {
    calls: RefCell<Vec<Call>>,
    available_rows: usize,
    selected_rows: Cell<usize>,
    hydrated_rows: Cell<usize>,
    missing: bool,
    fail: bool,
}

impl FakeStore {
    // Select primary rows first; the sentinel is never hydrated into a record.
    fn select_primary_rows<Key>(&self, request: &ReadPageRequest<Key>) -> (Vec<usize>, bool) {
        let mut rows: Vec<_> = (0..self.available_rows.min(request.row_limit())).collect();
        self.selected_rows.set(rows.len());
        let has_more = rows.len() > request.limit.get();
        if has_more {
            rows.pop();
        }
        (rows, has_more)
    }
}

impl ItemProjectionStore for FakeStore {
    type Error = io::Error;

    fn read_projection(
        &self,
        request: &ProjectionGetRequest,
    ) -> Result<Option<ItemProjection>, Self::Error> {
        self.calls.borrow_mut().push(Call::Get(request.clone()));
        if self.fail {
            return Err(io::Error::other("get failed"));
        }
        Ok((!self.missing).then(|| projection(request.projection)))
    }

    fn select_projection_page(
        &self,
        request: &ItemProjectionPageRequest,
    ) -> Result<ReadPage<ProjectedItemRow>, Self::Error> {
        self.calls.borrow_mut().push(Call::Items(request.clone()));
        if self.fail {
            return Err(io::Error::other("page failed"));
        }
        let (rows, has_more) = self.select_primary_rows(&request.page);
        Ok(ReadPage {
            records: rows
                .into_iter()
                .map(|_| {
                    self.hydrated_rows.set(self.hydrated_rows.get() + 1);
                    ProjectedItemRow {
                        item: projection(request.projection),
                        key: key(),
                    }
                })
                .collect(),
            has_more,
        })
    }
}

impl ItemHistoryPageStore for FakeStore {
    type Error = io::Error;

    fn select_history_page(
        &self,
        request: &HistoryPageRequest,
    ) -> Result<ReadPage<ItemHistoryEvent>, ItemHistoryStoreError<Self::Error>> {
        self.calls.borrow_mut().push(Call::History(request.clone()));
        if self.missing {
            return Err(ItemHistoryStoreError::NotFound);
        }
        if self.fail {
            return Err(ItemHistoryStoreError::InvalidPersistedData(
                io::Error::other("bad event"),
            ));
        }
        let (rows, has_more) = self.select_primary_rows(&request.page);
        Ok(ReadPage {
            records: rows
                .into_iter()
                .map(|index| {
                    self.hydrated_rows.set(self.hydrated_rows.get() + 1);
                    let mut event = history_event();
                    event.event_index = index as u64;
                    event
                })
                .collect(),
            has_more,
        })
    }
}

#[test]
fn item_pages_pass_projection_filters_order_limit_and_key_without_full_loading() {
    for kind in [
        ItemProjectionKind::Summary,
        ItemProjectionKind::Work,
        ItemProjectionKind::Audit,
    ] {
        for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
            for limit in [1, 100] {
                for available_rows in [0, limit, limit + 1, limit + 2] {
                    let store = FakeStore {
                        available_rows,
                        ..FakeStore::default()
                    };
                    let mut request = item_request(kind, ordering);
                    request.page = ReadPageRequest::new(limit, Some(key())).unwrap();
                    let page =
                        read_item_projection_page(&store, &authorization(), &request).unwrap();
                    assert_eq!(*store.calls.borrow(), [Call::Items(request)]);
                    assert_eq!(page.records.len(), available_rows.min(limit));
                    assert_eq!(page.has_more, available_rows > limit);
                    assert_eq!(store.selected_rows.get(), available_rows.min(limit + 1));
                    assert_eq!(store.hydrated_rows.get(), page.records.len());
                    for row in page.records {
                        assert_eq!(row.item, projection(kind));
                        assert_eq!(row.key, key());
                        assert_eq!(row.key.id.sequence(), 9_007_199_254_740_993);
                    }
                }
            }
        }
    }
}

#[test]
fn invalid_requests_are_rejected_before_storage() {
    let store = FakeStore::default();
    for limit in [0, 101, usize::MAX] {
        assert!(ReadPageRequest::<ItemReadKey>::new(limit, None).is_err());
        assert!(ReadPageRequest::<HistoryReadKey>::new(limit, None).is_err());
    }
    for limit in [1, 100] {
        assert_eq!(
            ReadPageRequest::<ItemReadKey>::new(limit, None)
                .unwrap()
                .row_limit(),
            limit + 1
        );
    }
    let mut request = item_request(ItemProjectionKind::Summary, ItemListOrdering::NewestFirst);
    request.filters.assignee = Some(AssigneeId::new("davis").unwrap());
    request.filters.unassigned = true;
    assert!(matches!(
        read_item_projection_page(&store, &authorization(), &request),
        Err(ProjectionPageError::InvalidFilters(_))
    ));
    assert!(store.calls.borrow().is_empty());
}

#[test]
fn all_reads_reauthorize_including_continuations_before_storage() {
    let store = FakeStore::default();
    let get = ProjectionGetRequest {
        item_id: id(),
        projection: ItemProjectionKind::Summary,
    };
    let items = item_request(ItemProjectionKind::Summary, ItemListOrdering::Next);
    let history = HistoryPageRequest {
        item_id: id(),
        ordering: HistoryOrdering::RevisionThenEventIndex,
        page: ReadPageRequest::new(
            1,
            Some(HistoryReadKey {
                item_revision: Revision::new(u64::MAX).unwrap(),
                event_index: u64::MAX,
            }),
        )
        .unwrap(),
    };
    let mut wrong_command = authorization();
    wrong_command.command = Command::Capture;
    let mut wrong_attribution = authorization();
    wrong_attribution.observed_execution = ObservedExecution::Agent {
        agent_id: "untrusted",
    };
    for denied in [wrong_command, wrong_attribution] {
        assert!(matches!(
            read_item_projection(&store, &denied, &get),
            Err(ReadItemError::Unauthorized(_))
        ));
        assert!(matches!(
            read_item_projection_page(&store, &denied, &items),
            Err(ProjectionPageError::Unauthorized(_))
        ));
        assert!(matches!(
            read_item_history_page(&store, &denied, &history),
            Err(ItemHistoryError::Unauthorized(_))
        ));
    }
    assert!(store.calls.borrow().is_empty());
}

#[test]
fn get_passes_each_projection_and_distinguishes_missing_and_storage_failure() {
    let store = FakeStore::default();
    for kind in [
        ItemProjectionKind::Summary,
        ItemProjectionKind::Work,
        ItemProjectionKind::Audit,
    ] {
        let request = ProjectionGetRequest {
            item_id: id(),
            projection: kind,
        };
        assert_eq!(
            read_item_projection(&store, &authorization(), &request).unwrap(),
            projection(kind)
        );
        assert_eq!(store.calls.borrow().last(), Some(&Call::Get(request)));
    }
    let request = ProjectionGetRequest {
        item_id: id(),
        projection: ItemProjectionKind::Summary,
    };
    let missing = FakeStore {
        missing: true,
        ..FakeStore::default()
    };
    assert!(matches!(
        read_item_projection(&missing, &authorization(), &request),
        Err(ReadItemError::NotFound)
    ));
    let failing = FakeStore {
        fail: true,
        ..FakeStore::default()
    };
    assert!(matches!(
        read_item_projection(&failing, &authorization(), &request),
        Err(ReadItemError::Storage(_))
    ));
    assert!(matches!(
        read_item_projection_page(
            &failing,
            &authorization(),
            &item_request(ItemProjectionKind::Summary, ItemListOrdering::NewestFirst)
        ),
        Err(ProjectionPageError::Storage(_))
    ));
}

#[test]
fn history_passes_bounds_and_order_and_distinguishes_empty_missing_and_invalid_data() {
    let request = HistoryPageRequest {
        item_id: id(),
        ordering: HistoryOrdering::RevisionThenEventIndex,
        page: ReadPageRequest::new(
            100,
            Some(HistoryReadKey {
                item_revision: Revision::new(u64::MAX).unwrap(),
                event_index: u64::MAX,
            }),
        )
        .unwrap(),
    };
    let store = FakeStore::default();
    let page = read_item_history_page(&store, &authorization(), &request).unwrap();
    assert!(page.records.is_empty());
    assert!(!page.has_more);
    assert_eq!(store.hydrated_rows.get(), 0);
    assert_eq!(*store.calls.borrow(), [Call::History(request.clone())]);
    assert_eq!(request.page.row_limit(), 101);
    let missing = FakeStore {
        missing: true,
        ..FakeStore::default()
    };
    assert!(matches!(
        read_item_history_page(&missing, &authorization(), &request),
        Err(ItemHistoryError::NotFound)
    ));
    let failing = FakeStore {
        fail: true,
        ..FakeStore::default()
    };
    assert!(matches!(
        read_item_history_page(&failing, &authorization(), &request),
        Err(ItemHistoryError::InvalidPersistedData(_))
    ));
}

#[test]
fn history_pages_strip_lookahead_before_hydration_and_forward_has_more() {
    for limit in [1, 100] {
        for available_rows in [0, limit, limit + 1, limit + 2] {
            let store = FakeStore {
                available_rows,
                ..FakeStore::default()
            };
            let request = HistoryPageRequest {
                item_id: id(),
                ordering: HistoryOrdering::RevisionThenEventIndex,
                page: ReadPageRequest::new(limit, None).unwrap(),
            };
            let page = read_item_history_page(&store, &authorization(), &request).unwrap();
            assert_eq!(*store.calls.borrow(), [Call::History(request)]);
            assert_eq!(page.records.len(), available_rows.min(limit));
            assert_eq!(page.has_more, available_rows > limit);
            assert_eq!(store.selected_rows.get(), available_rows.min(limit + 1));
            assert_eq!(store.hydrated_rows.get(), page.records.len());
            for (index, event) in page.records.iter().enumerate() {
                assert_eq!(event.event_index, index as u64);
                assert_eq!(event.item_revision, Revision::new(u64::MAX).unwrap());
            }
        }
    }
}

fn history_event() -> ItemHistoryEvent {
    ItemHistoryEvent {
        operation_id: "op".to_owned(),
        event_id: "event".to_owned(),
        item_revision: Revision::new(u64::MAX).unwrap(),
        event_index: u64::MAX,
        event_type: EventType::NoteAdded,
        before: None,
        after: None,
        actor: EventActor {
            kind: ActorKind::Human,
            id: "DAVIS".to_owned(),
            surface: "test".to_owned(),
            host: "local".to_owned(),
        },
        execution: EventExecution::Direct {
            surface: "test".to_owned(),
            host: "local".to_owned(),
        },
        reason: None,
        note: Some("note".to_owned()),
        occurred_at: Timestamp::new("2025-01-02T03:04:05Z"),
        schema_version: 1,
    }
}

#[test]
fn history_keys_preserve_full_revision_and_event_index_without_wire_values() {
    let event = history_event();
    assert_eq!(
        HistoryReadKey::from(&event),
        HistoryReadKey {
            item_revision: Revision::new(u64::MAX).unwrap(),
            event_index: u64::MAX,
        }
    );
}
