use bif::{
    application::{
        CursorContext, CursorEncodeError, DecodedCursor, HistoryOrdering, HistoryPageRequest,
        HistoryReadKey, InvalidCursor, ItemListFilters, ItemListOrdering, ItemProjectionKind,
        ItemProjectionPageRequest, ItemReadKey, ItemTextFilter, MAX_CURSOR_BYTES, ReadPageRequest,
    },
    domain::{ItemId, NamedView, Priority, ProjectId, RequesterId, Revision, Status, Timestamp},
};

fn item_request(ordering: ItemListOrdering) -> ItemProjectionPageRequest {
    ItemProjectionPageRequest {
        view: NamedView::All,
        configured_requester: RequesterId::new("Alice").unwrap(),
        filters: ItemListFilters::default(),
        projection: ItemProjectionKind::Summary,
        ordering,
        page: ReadPageRequest::new(20, None).unwrap(),
    }
}

fn item_key(sequence: u64, priority: Option<Priority>) -> ItemReadKey {
    ItemReadKey {
        id: ItemId::new(
            RequesterId::new("Alice").unwrap(),
            ProjectId::new("bif").unwrap(),
            sequence,
        )
        .unwrap(),
        captured_at: Timestamp::new("opaque time\n雪"),
        priority,
    }
}

fn history_request(sequence: u64) -> HistoryPageRequest {
    HistoryPageRequest {
        item_id: item_key(sequence, None).id,
        ordering: HistoryOrdering::RevisionThenEventIndex,
        page: ReadPageRequest::new(20, None).unwrap(),
    }
}

fn assert_invalid(error: InvalidCursor, reason: &str) {
    assert_eq!(error.code(), "invalid_cursor");
    assert_eq!(error.reason(), reason);
    assert!(error.restart_required());
    assert!(error.to_string().contains("restart"));
}

// Tests inspect the local codec representation.
fn token_json(token: &str) -> String {
    let hex = token.strip_prefix("bifc1.").unwrap();
    let bytes: Vec<_> = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    String::from_utf8(bytes).unwrap()
}

fn token_from_json(json: &str) -> String {
    let mut token = String::from("bifc1.");
    for byte in json.as_bytes() {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

#[test]
fn legacy_bifc1_envelope_still_decodes_and_encodes_byte_for_byte() {
    // Fixed pre-budget-change envelope, including the unchanged query binding.
    let legacy = token_from_json(concat!(
        r#"{"cursor_version":1,"kind":"list","store_id":"store","generation":null,"query_fingerprint":"#,
        r#""ccef68ac438d83bc7206c51d844917b946d4a19ae066cc079e2e6a7fdcd1503d","order_version":1,"projection_schema_version":1,"boundary":{"item":{"requester":"ALICE","project":"bif","sequence":1,"captured_at":"opaque time\n雪","priority":null}}}"#,
    ));
    assert!(legacy.len() < 16_384);
    let context =
        CursorContext::item_page("store", &item_request(ItemListOrdering::NewestFirst)).unwrap();
    let key = item_key(1, None);
    assert_eq!(context.decode_item_key(&legacy).unwrap(), key);
    assert_eq!(context.encode_item_key(&key).unwrap(), legacy);
}

#[test]
fn round_trips_all_operations_and_full_width_keys() {
    for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
        let context = CursorContext::item_page("store", &item_request(ordering)).unwrap();
        for sequence in [999, 1000, (1_u64 << 53) + 1, u64::MAX] {
            for priority in [
                None,
                Some(Priority::P0),
                Some(Priority::P1),
                Some(Priority::P2),
                Some(Priority::P3),
                Some(Priority::P4),
            ] {
                let key = item_key(sequence, priority);
                let token = context.encode_item_key(&key).unwrap();
                let decoded = DecodedCursor::parse(&token).unwrap();
                decoded.require_item_kind(ordering).unwrap();
                assert_invalid(decoded.require_history_kind().unwrap_err(), "wrong_kind");
                assert_eq!(context.bind_item_key(&decoded).unwrap(), key);
                assert_eq!(context.decode_item_key(&token).unwrap(), key);
                assert_eq!(context.encode_item_key(&key).unwrap(), token);
            }
        }
    }
    let context = CursorContext::history("store", &history_request(1));
    for event_index in [0, (1_u64 << 53) + 1, u64::MAX] {
        let key = HistoryReadKey {
            item_revision: Revision::new(u64::MAX).unwrap(),
            event_index,
        };
        let decoded = DecodedCursor::parse(&context.encode_history_key(&key).unwrap()).unwrap();
        decoded.require_history_kind().unwrap();
        assert_invalid(
            decoded
                .require_item_kind(ItemListOrdering::NewestFirst)
                .unwrap_err(),
            "wrong_kind",
        );
        assert_eq!(context.bind_history_key(&decoded).unwrap(), key);
        assert_eq!(
            context
                .decode_history_key(&context.encode_history_key(&key).unwrap())
                .unwrap(),
            key
        );
    }
}

#[test]
fn binds_store_operation_projection_and_effective_query_not_page_size_or_boundary() {
    let request = item_request(ItemListOrdering::NewestFirst);
    let context = CursorContext::item_page("store", &request).unwrap();
    let token = context.encode_item_key(&item_key(1, None)).unwrap();
    assert_invalid(
        CursorContext::item_page("other", &request)
            .unwrap()
            .decode_item_key(&token)
            .unwrap_err(),
        "wrong_store",
    );
    for (changed, reason) in [
        (
            ItemProjectionPageRequest {
                ordering: ItemListOrdering::Next,
                ..request.clone()
            },
            "wrong_kind",
        ),
        (
            ItemProjectionPageRequest {
                projection: ItemProjectionKind::Work,
                ..request.clone()
            },
            "wrong_query",
        ),
        (
            ItemProjectionPageRequest {
                filters: ItemListFilters {
                    status: Some(Status::Ready),
                    ..Default::default()
                },
                ..request.clone()
            },
            "wrong_query",
        ),
    ] {
        assert_invalid(
            CursorContext::item_page("store", &changed)
                .unwrap()
                .decode_item_key(&token)
                .unwrap_err(),
            reason,
        );
    }
    let changed = ItemProjectionPageRequest {
        configured_requester: RequesterId::new("Bob").unwrap(),
        page: ReadPageRequest::new(100, Some(item_key(5, None))).unwrap(),
        ..request
    };
    assert!(
        CursorContext::item_page("store", &changed)
            .unwrap()
            .decode_item_key(&token)
            .is_ok()
    );
}

#[test]
fn equivalent_normalized_filters_share_tokens() {
    let mut first = item_request(ItemListOrdering::NewestFirst);
    first.filters.status = Some(Status::Ready);
    first.filters.text = Some(ItemTextFilter::new("WoRK %_雪").unwrap());
    let mut second = first.clone();
    second.view = NamedView::Ready;
    second.filters.status = None;
    second.filters.text = Some(ItemTextFilter::new("work %_雪").unwrap());
    let token = CursorContext::item_page("store", &first)
        .unwrap()
        .encode_item_key(&item_key(1, None))
        .unwrap();
    assert!(
        CursorContext::item_page("store", &second)
            .unwrap()
            .decode_item_key(&token)
            .is_ok()
    );

    // A nonempty mine query and its explicit effective ownership restriction
    // have identical semantics; raw view spelling is not part of the binding.
    first.view = NamedView::Mine;
    second.filters.assignee = Some(bif::domain::AssigneeId::new("Alice").unwrap());
    let token = CursorContext::item_page("store", &first)
        .unwrap()
        .encode_item_key(&item_key(1, None))
        .unwrap();
    assert!(
        CursorContext::item_page("store", &second)
            .unwrap()
            .decode_item_key(&token)
            .is_ok()
    );
}

#[test]
fn resolved_mine_identity_is_bound_even_when_query_is_contradictory() {
    for empty in [false, true] {
        let mut alice = item_request(ItemListOrdering::NewestFirst);
        alice.view = NamedView::Mine;
        alice.filters.unassigned = empty;
        let mut bob = alice.clone();
        bob.configured_requester = RequesterId::new("Bob").unwrap();
        let token = CursorContext::item_page("store", &alice)
            .unwrap()
            .encode_item_key(&item_key(1, None))
            .unwrap();
        assert_invalid(
            CursorContext::item_page("store", &bob)
                .unwrap()
                .decode_item_key(&token)
                .unwrap_err(),
            "wrong_query",
        );
    }
}

#[test]
fn rejects_invalid_filter_contexts() {
    let mut request = item_request(ItemListOrdering::NewestFirst);
    request.filters.assignee = Some(bif::domain::AssigneeId::new("Alice").unwrap());
    request.filters.unassigned = true;
    assert!(CursorContext::item_page("store", &request).is_err());
}

#[test]
fn history_binds_item_and_rejects_item_cursor_swaps_and_wrong_encoding_api() {
    let history = CursorContext::history("store", &history_request(1));
    let history_key = HistoryReadKey {
        item_revision: Revision::new(1).unwrap(),
        event_index: 0,
    };
    let token = history.encode_history_key(&history_key).unwrap();
    assert_invalid(
        CursorContext::history("store", &history_request(2))
            .decode_history_key(&token)
            .unwrap_err(),
        "wrong_query",
    );
    let items =
        CursorContext::item_page("store", &item_request(ItemListOrdering::NewestFirst)).unwrap();
    assert_invalid(items.decode_item_key(&token).unwrap_err(), "wrong_kind");
    assert_invalid(
        history
            .decode_history_key(&items.encode_item_key(&item_key(1, None)).unwrap())
            .unwrap_err(),
        "wrong_kind",
    );
    assert_invalid(
        match history.encode_item_key(&item_key(1, None)).unwrap_err() {
            CursorEncodeError::InvalidContext(error) => error,
            error => panic!("{error}"),
        },
        "wrong_kind",
    );
    assert_invalid(
        match items.encode_history_key(&history_key).unwrap_err() {
            CursorEncodeError::InvalidContext(error) => error,
            error => panic!("{error}"),
        },
        "wrong_kind",
    );
}

#[test]
fn rejects_unsupported_envelope_order_projection_versions_and_generation() {
    let context =
        CursorContext::item_page("store", &item_request(ItemListOrdering::NewestFirst)).unwrap();
    let json = token_json(&context.encode_item_key(&item_key(1, None)).unwrap());
    for (from, to) in [
        ("\"cursor_version\":1", "\"cursor_version\":2"),
        ("\"order_version\":1", "\"order_version\":2"),
        (
            "\"projection_schema_version\":1",
            "\"projection_schema_version\":2",
        ),
        ("\"generation\":null", "\"generation\":\"future\""),
    ] {
        assert!(json.contains(from));
        assert_invalid(
            DecodedCursor::parse(&token_from_json(&json.replace(from, to))).unwrap_err(),
            "unsupported_version",
        );
        assert_invalid(
            context
                .decode_item_key(&token_from_json(&json.replace(from, to)))
                .unwrap_err(),
            "unsupported_version",
        );
    }
    assert!(
        context
            .decode_item_key(&token_from_json(&json.replace("\"generation\":null,", "")))
            .is_ok()
    );
}

#[test]
fn rejects_unknown_duplicate_missing_fields_wrong_types_and_overflow() {
    let context = CursorContext::item_page("store", &item_request(ItemListOrdering::Next)).unwrap();
    let json = token_json(&context.encode_item_key(&item_key(1, None)).unwrap());
    for changed in [
        json.replacen('{', "{\"unknown\":0,", 1),
        json.replace("\"sequence\":1", "\"sequence\":1,\"sequence\":2"),
        json.replace("\"priority\":null", "\"priority\":null,\"priority\":null"),
        json.replace(
            "\"generation\":null",
            "\"generation\":null,\"generation\":null",
        ),
        json.replace("\"sequence\":1", "\"sequence\":18446744073709551616"),
        json.replace("\"sequence\":1", "\"sequence\":-1"),
        json.replace("\"sequence\":1", "\"sequence\":1.0"),
        json.replace("\"sequence\":1", "\"sequence\":\"1\""),
        json.replace("\"sequence\":1", "\"sequence\":0"),
        json.replace("\"priority\":null", "\"priority\":\"P5\""),
        json.replace(",\"priority\":null", ""),
        json.replace("\"captured_at\":", "\"captured_at\":null,\"removed\":"),
        json.replace("\"requester\":\"ALICE\"", "\"requester\":\"alice\""),
        json.replace("\"priority\":null", "\"removed\":null"),
        json.replace(
            "\"store_id\":\"store\"",
            "\"store_id\":\"store\",\"store_id\":\"store\"",
        ),
        json.replace("\"generation\":null", "\"generation\":1"),
        json.replace("\"cursor_version\":1", "\"cursor_version\":4294967296"),
        json.replace("\"kind\":\"next\"", "\"kind\":\"NEXT\""),
        json.replace("\"item\":{", "\"item\":{\"unknown\":0,"),
        String::from("null"),
        format!("{json} trailing"),
    ] {
        assert_invalid(
            DecodedCursor::parse(&token_from_json(&changed)).unwrap_err(),
            "malformed",
        );
        assert_invalid(
            context
                .decode_item_key(&token_from_json(&changed))
                .unwrap_err(),
            "malformed",
        );
    }
    let history = CursorContext::history("store", &history_request(1));
    let json = token_json(
        &history
            .encode_history_key(&HistoryReadKey {
                item_revision: Revision::new(1).unwrap(),
                event_index: 0,
            })
            .unwrap(),
    );
    for changed in [
        json.replace("\"item_revision\":1", "\"item_revision\":0"),
        json.replace("\"event_index\":0", "\"event_index\":18446744073709551616"),
        json.replace("\"event_index\":0", "\"event_index\":null"),
    ] {
        assert_invalid(
            DecodedCursor::parse(&token_from_json(&changed)).unwrap_err(),
            "malformed",
        );
        assert_invalid(
            history
                .decode_history_key(&token_from_json(&changed))
                .unwrap_err(),
            "malformed",
        );
    }
}

#[test]
fn rejects_positional_objects_and_object_form_unit_enums_for_item_cursors() {
    for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
        let context = CursorContext::item_page("store", &item_request(ordering)).unwrap();
        let key = item_key(1, None);
        let token = context.encode_item_key(&key).unwrap();
        assert_eq!(context.decode_item_key(&token).unwrap(), key);
        let envelope: serde_json::Value = serde_json::from_str(&token_json(&token)).unwrap();
        let item = &envelope["boundary"]["item"];
        // Match the struct field order: these arrays previously decoded successfully.
        let positional_envelope = serde_json::json!([
            envelope["cursor_version"],
            envelope["kind"],
            envelope["store_id"],
            envelope["generation"],
            envelope["query_fingerprint"],
            envelope["order_version"],
            envelope["projection_schema_version"],
            envelope["boundary"],
        ]);
        let mut positional_item = envelope.clone();
        positional_item["boundary"]["item"] = serde_json::json!([
            item["requester"],
            item["project"],
            item["sequence"],
            item["captured_at"],
            item["priority"],
        ]);
        let mut object_kind = envelope.clone();
        object_kind["kind"] = serde_json::json!({ (envelope["kind"].as_str().unwrap()): null });
        let mut object_priority = envelope.clone();
        object_priority["boundary"]["item"]["priority"] = serde_json::json!({"P0": null});
        for changed in [
            positional_envelope,
            positional_item,
            object_kind,
            object_priority,
        ] {
            assert_invalid(
                DecodedCursor::parse(&token_from_json(&changed.to_string())).unwrap_err(),
                "malformed",
            );
            assert_invalid(
                context
                    .decode_item_key(&token_from_json(&changed.to_string()))
                    .unwrap_err(),
                "malformed",
            );
        }
    }
}

#[test]
fn rejects_positional_objects_and_object_form_kind_for_history_cursors() {
    let context = CursorContext::history("store", &history_request(1));
    let key = HistoryReadKey {
        item_revision: Revision::new(1).unwrap(),
        event_index: 0,
    };
    let token = context.encode_history_key(&key).unwrap();
    assert_eq!(context.decode_history_key(&token).unwrap(), key);
    let envelope: serde_json::Value = serde_json::from_str(&token_json(&token)).unwrap();
    let mut positional_history = envelope.clone();
    positional_history["boundary"]["history"] = serde_json::json!([1, 0]);
    let mut object_kind = envelope.clone();
    object_kind["kind"] = serde_json::json!({"history": null});
    for changed in [positional_history, object_kind] {
        assert_invalid(
            DecodedCursor::parse(&token_from_json(&changed.to_string())).unwrap_err(),
            "malformed",
        );
        assert_invalid(
            context
                .decode_history_key(&token_from_json(&changed.to_string()))
                .unwrap_err(),
            "malformed",
        );
    }
}

#[test]
fn rejects_malformed_and_oversized_tokens_and_bounds_encoding() {
    let context =
        CursorContext::item_page("store", &item_request(ItemListOrdering::NewestFirst)).unwrap();
    for token in [
        "",
        "opaque",
        "bifc1.0",
        "bifc1.gg",
        "bifc1.ff",
        "bifc1.7b7d",
        "bifc1.5b5d",
    ] {
        assert_invalid(context.decode_item_key(token).unwrap_err(), "malformed");
    }
    assert_invalid(
        context
            .decode_item_key(&"x".repeat(MAX_CURSOR_BYTES + 1))
            .unwrap_err(),
        "oversized",
    );
    let mut key = item_key(1, None);
    key.captured_at = Timestamp::new("x".repeat(MAX_CURSOR_BYTES));
    assert!(matches!(
        context.encode_item_key(&key).unwrap_err(),
        CursorEncodeError::TooLarge { .. }
    ));
}

#[test]
fn exact_size_limit_is_supported_and_one_more_byte_is_rejected() {
    let context =
        CursorContext::item_page("store", &item_request(ItemListOrdering::NewestFirst)).unwrap();
    let mut key = item_key(1, None);
    key.captured_at = Timestamp::new("");
    let overhead = context.encode_item_key(&key).unwrap().len();
    key.captured_at = Timestamp::new("x".repeat((MAX_CURSOR_BYTES - overhead) / 2));
    let token = context.encode_item_key(&key).unwrap();
    assert_eq!(token.len(), MAX_CURSOR_BYTES);
    assert_eq!(context.decode_item_key(&token).unwrap(), key);
    assert_invalid(
        context.decode_item_key(&(token + "0")).unwrap_err(),
        "oversized",
    );
    key.captured_at = Timestamp::new(format!("{}x", key.captured_at.as_str()));
    assert!(matches!(
        context.encode_item_key(&key).unwrap_err(),
        CursorEncodeError::TooLarge { encoded_token_bytes } if encoded_token_bytes == MAX_CURSOR_BYTES + 2
    ));
}

#[test]
fn generated_overflow_counts_the_complete_borrowed_envelope_exactly() {
    assert_eq!(MAX_CURSOR_BYTES, 1_048_576);
    let context =
        CursorContext::item_page("store\n雪\"", &item_request(ItemListOrdering::NewestFirst))
            .unwrap();
    let mut key = item_key(u64::MAX, Some(Priority::P4));
    let token = context.encode_item_key(&key).unwrap();
    let mut oracle: serde_json::Value = serde_json::from_str(&token_json(&token)).unwrap();
    // An early string-length shortcut would miss escaping, UTF-8 and the fields
    // that follow the timestamp. The test-only oracle may allocate freely.
    let timestamp = "雪\n\"\\".repeat(MAX_CURSOR_BYTES / 3);
    key.captured_at = Timestamp::new(&timestamp);
    oracle["boundary"]["item"]["captured_at"] = serde_json::json!(timestamp);
    let expected = 6 + serde_json::to_vec(&oracle).unwrap().len() * 2;
    assert!(expected > MAX_CURSOR_BYTES);
    assert!(matches!(
        context.encode_item_key(&key).unwrap_err(),
        CursorEncodeError::TooLarge { encoded_token_bytes } if encoded_token_bytes == expected
    ));
}

#[test]
fn decoded_and_modified_cursors_never_authorize_item_or_history_reads() {
    use bif::application::{
        Actor, ActorKind, AuthorizationRequest, Command, Execution, ItemHistoryError,
        ItemHistoryEvent, ItemHistoryPageStore, ItemHistoryStoreError, ItemProjection,
        ItemProjectionStore, ObservedExecution, ProjectedItemRow, ProjectionGetRequest,
        ProjectionPageError, ReadPage, read_item_history_page, read_item_projection_page,
    };
    use std::cell::Cell;

    struct Store(Cell<usize>);
    impl ItemProjectionStore for Store {
        type Error = std::io::Error;

        fn read_projection(
            &self,
            _: &ProjectionGetRequest,
        ) -> Result<Option<ItemProjection>, Self::Error> {
            panic!("pagination must not call the get loader");
        }

        fn select_projection_page(
            &self,
            _: &ItemProjectionPageRequest,
        ) -> Result<ReadPage<ProjectedItemRow>, Self::Error> {
            self.0.set(self.0.get() + 1);
            Ok(ReadPage {
                records: vec![],
                has_more: false,
            })
        }
    }
    impl ItemHistoryPageStore for Store {
        type Error = std::io::Error;

        fn select_history_page(
            &self,
            _: &HistoryPageRequest,
        ) -> Result<ReadPage<ItemHistoryEvent>, ItemHistoryStoreError<Self::Error>> {
            self.0.set(self.0.get() + 1);
            Ok(ReadPage {
                records: vec![],
                has_more: false,
            })
        }
    }
    let store = Store(Cell::new(0));
    let mut authorization = AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: "ALICE",
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
    };
    let mut items = item_request(ItemListOrdering::NewestFirst);
    let mut history = history_request(1);
    read_item_projection_page(&store, &authorization, &items).unwrap();
    read_item_history_page(&store, &authorization, &history).unwrap();
    let context = CursorContext::item_page("store", &items).unwrap();
    let token = context.encode_item_key(&item_key(1, None)).unwrap();
    // No MAC is promised: a valid edited boundary is accepted as parameter data,
    // not as permission, and still cannot bypass the read authorization policy.
    let edited = token_from_json(&token_json(&token).replace("\"sequence\":1", "\"sequence\":2"));
    let decoded = DecodedCursor::parse(&edited).unwrap();
    items.page.after = Some(context.bind_item_key(&decoded).unwrap());
    assert_eq!(items.page.after.as_ref().unwrap().id.sequence(), 2);
    let context = CursorContext::history("store", &history);
    let key = HistoryReadKey {
        item_revision: Revision::new(1).unwrap(),
        event_index: 0,
    };
    let decoded = DecodedCursor::parse(&context.encode_history_key(&key).unwrap()).unwrap();
    history.page.after = Some(context.bind_history_key(&decoded).unwrap());
    authorization.observed_execution = ObservedExecution::Agent {
        agent_id: "untrusted",
    };
    assert!(matches!(
        read_item_projection_page(&store, &authorization, &items),
        Err(ProjectionPageError::Unauthorized(_))
    ));
    assert!(matches!(
        read_item_history_page(&store, &authorization, &history),
        Err(ItemHistoryError::Unauthorized(_))
    ));
    assert_eq!(
        store.0.get(),
        2,
        "unauthorized continuations must not access storage"
    );
}
