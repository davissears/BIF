//! Allocation windows exclude input construction and independent JSON oracles.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use bif::{
    application::{
        CursorContext, CursorEncodeError, ItemListFilters, ItemListOrdering, ItemProjection,
        ItemProjectionKind, ItemProjectionPageRequest, ItemReadKey, ItemSummary, MAX_CURSOR_BYTES,
        ProjectedItemRow, ReadPage, ReadPageRequest,
    },
    domain::{ItemId, NamedView, ProjectId, RequesterId, Revision, Status, Timestamp},
    v2_response::{
        CursorCandidate, EncodeError, ReadError, ResponseBudget, write_item_page,
        write_item_page_candidates,
    },
};
use serde_json::{Value, json};

#[derive(Clone, Copy, Default)]
struct Allocations {
    bytes: usize,
    largest: usize,
}
thread_local! {
    static MEASURE: Cell<Option<Allocations>> = const { Cell::new(None) };
}
struct MeasuredSystem;
fn record(size: usize) {
    let _ = MEASURE.try_with(|measure| {
        if let Some(mut allocations) = measure.get() {
            allocations.bytes += size;
            allocations.largest = allocations.largest.max(size);
            measure.set(Some(allocations));
        }
    });
}
// SAFETY: System receives unchanged layouts/pointers; measuring uses only Cells.
unsafe impl GlobalAlloc for MeasuredSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: MeasuredSystem = MeasuredSystem;
fn measured<T>(operation: impl FnOnce() -> T) -> (T, Allocations) {
    MEASURE.with(|measure| measure.set(Some(Allocations::default())));
    let result = operation();
    let allocations = MEASURE.with(|measure| measure.replace(None).unwrap());
    (result, allocations)
}
fn request() -> ItemProjectionPageRequest {
    ItemProjectionPageRequest {
        view: NamedView::All,
        configured_requester: RequesterId::new("R").unwrap(),
        filters: ItemListFilters::default(),
        projection: ItemProjectionKind::Summary,
        ordering: ItemListOrdering::NewestFirst,
        page: ReadPageRequest::new(100, None).unwrap(),
    }
}
fn key() -> ItemReadKey {
    ItemReadKey {
        id: ItemId::new(
            RequesterId::new("R").unwrap(),
            ProjectId::new("p").unwrap(),
            1,
        )
        .unwrap(),
        captured_at: Timestamp::new(""),
        priority: None,
    }
}
fn envelope(token: &str) -> Value {
    let bytes: Vec<_> = token.as_bytes()[6..]
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn generated_overflow_borrows_large_key_and_context_fields_and_counts_to_the_end() {
    let context = CursorContext::item_page("store", &request()).unwrap();
    let baseline = key();
    let original = envelope(&context.encode_item_key(&baseline).unwrap());
    for field in ["requester", "project", "captured_at", "store_id"] {
        let mut key = key();
        let mut oracle = original.clone();
        let mut context = context.clone();
        let large = if field == "captured_at" || field == "store_id" {
            "雪\n\"\\".repeat(MAX_CURSOR_BYTES / 2)
        } else {
            "R".repeat(MAX_CURSOR_BYTES * 2)
        };
        match field {
            "requester" => {
                key.id = ItemId::new(
                    RequesterId::new(&large).unwrap(),
                    ProjectId::new("p").unwrap(),
                    1,
                )
                .unwrap();
            }
            "project" => {
                key.id = ItemId::new(
                    RequesterId::new("R").unwrap(),
                    ProjectId::new(&large).unwrap(),
                    1,
                )
                .unwrap();
            }
            "captured_at" => key.captured_at = Timestamp::new(&large),
            "store_id" => context = CursorContext::item_page(&large, &request()).unwrap(),
            _ => unreachable!(),
        }
        if field == "store_id" {
            // Fingerprints always occupy 64 ASCII bytes, even when scope changes.
            oracle["store_id"] = json!(large);
        } else {
            oracle["boundary"]["item"][field] = json!(large);
        }
        let expected = 6 + serde_json::to_vec(&oracle).unwrap().len() * 2;
        let (result, allocations) = measured(|| context.encode_item_key(&key));
        assert!(matches!(result.unwrap_err(), CursorEncodeError::TooLarge {
            encoded_token_bytes
        } if encoded_token_bytes == expected));
        // At most the bounded JSON staging buffer, not an input clone or token.
        assert!(allocations.largest <= MAX_CURSOR_BYTES, "{field}");
        assert!(allocations.bytes <= MAX_CURSOR_BYTES * 2, "{field}");
    }
}

#[test]
fn real_codec_overflow_reports_exact_page_minimum_without_output_allocation() {
    let context = CursorContext::item_page("store", &request()).unwrap();
    let mut key = key();
    let mut cursor_oracle = envelope(&context.encode_item_key(&key).unwrap());
    let timestamp = "雪\n\"\\".repeat(MAX_CURSOR_BYTES);
    key.captured_at = Timestamp::new(&timestamp);
    cursor_oracle["boundary"]["item"]["captured_at"] = json!(timestamp);
    let encoded_token_bytes = 6 + serde_json::to_vec(&cursor_oracle).unwrap().len() * 2;
    let page = ReadPage {
        records: vec![ProjectedItemRow {
            item: ItemProjection::Summary(ItemSummary::new(
                key.id.clone(),
                "title",
                Status::Proposed,
                None,
                None,
                Revision::new(1).unwrap(),
            )),
            key,
        }],
        has_more: false,
    };
    let mut terminal = Vec::new();
    write_item_page(
        &mut terminal,
        &page,
        ResponseBudget::default(),
        |_| panic!(),
    )
    .unwrap();
    let mut oracle: Value = serde_json::from_slice(&terminal).unwrap();
    oracle["result"]["next_cursor"] = json!("a".repeat(encoded_token_bytes));
    let expected = serde_json::to_vec(&oracle).unwrap().len();
    let page = ReadPage {
        has_more: true,
        ..page
    };
    let mut output = Vec::new();
    let (result, allocations) = measured(|| {
        write_item_page_candidates(&mut output, &page, ResponseBudget::default(), |row| {
            match context.encode_item_key(&row.key) {
                Err(CursorEncodeError::TooLarge {
                    encoded_token_bytes,
                }) => Ok(CursorCandidate::Oversized {
                    encoded_token_bytes,
                }),
                result => panic!("expected oversized generation, got {result:?}"),
            }
        })
    });
    let EncodeError::Read(ReadError::PayloadTooLarge(details)) = result.unwrap_err() else {
        panic!("expected payload_too_large");
    };
    assert_eq!(details.minimum_required_bytes, expected);
    assert_eq!(details.record_id, "R:p:001");
    assert!(output.is_empty());
    assert!(allocations.largest <= MAX_CURSOR_BYTES);
    assert!(allocations.bytes <= MAX_CURSOR_BYTES * 2);
}
