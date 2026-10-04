//! Thread-local allocation measurements isolate adapter work from test setup.
//! Bounds are correctness assertions; v1 comparisons are evidence, not ratios.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    io,
};

use bif::{
    application::{ItemAudit, ItemProjection, ItemReadKey, ProjectedItemRow, ReadPage},
    domain::{
        Item, ItemContent, ItemId, ProjectId, Provenance, RequesterId, Revision, Status, Timestamp,
    },
    v2_response::{EncodeError, ReadError, ResponseBudget, write_get, write_item_page},
};

#[derive(Clone, Copy, Debug, Default)]
struct Allocations {
    calls: usize,
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
            allocations.calls += 1;
            allocations.bytes += size;
            allocations.largest = allocations.largest.max(size);
            measure.set(Some(allocations));
        }
    });
}

// SAFETY: allocation, reallocation, and deallocation delegate unchanged layouts
// and pointers to System. Measurement uses only nonallocating thread-local Cells.
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

fn item(description: String) -> Item {
    Item::new(
        ItemId::new(
            RequesterId::new("DAVIS").unwrap(),
            ProjectId::new("bif").unwrap(),
            1,
        )
        .unwrap(),
        ItemContent::new(
            "Allocation evidence",
            Some(description),
            (0..32).map(|_| "🦀".repeat(256)).collect(),
        )
        .unwrap(),
        Status::Ready,
        None,
        None,
        None,
        Revision::new(1).unwrap(),
        Timestamp::new("2025-01-02T03:04:05Z"),
        Timestamp::new("2025-01-02T03:04:05Z"),
        Provenance::default(),
    )
}

#[test]
fn bounded_buffer_and_allocation_evidence_against_v1_dynamic_serialization() {
    let item = item("x".repeat(65_536));
    let projection = ItemProjection::Audit(ItemAudit::from(&item));
    let (result, typed) =
        measured(|| write_get(&mut io::sink(), &projection, ResponseBudget::default()));
    let stats = result.unwrap();
    // No encoded-record copy or dynamic JSON tree may accompany the final
    // buffer. The small allowance avoids pinning serde's incidental internals.
    assert!(typed.largest <= stats.json_bytes + 1);
    assert!(typed.bytes <= stats.json_bytes + 1 + 4096);
    let (_, dynamic) = measured(|| {
        let result = bif::rpc_read::get_result_json(&item);
        serde_json::to_writer(io::sink(), &result).unwrap();
    });
    eprintln!(
        "audit allocation evidence ({} JSON bytes): typed={typed:?}; v1 dynamic={dynamic:?}",
        stats.json_bytes
    );
}

#[test]
fn page_omission_counts_large_next_row_without_allocating_its_encoded_copy() {
    let small = item("small".into());
    let large = item("🦀\n\"\\".repeat(262_144));
    let row = |item: &Item| ProjectedItemRow {
        item: ItemProjection::Audit(ItemAudit::from(item)),
        key: ItemReadKey {
            id: item.id().clone(),
            captured_at: item.captured_at().clone(),
            priority: item.priority(),
        },
    };
    let page = ReadPage {
        records: vec![row(&small), row(&large)],
        has_more: false,
    };
    let budget = ResponseBudget::new(40_000).unwrap();
    let (result, allocations) =
        measured(|| write_item_page(&mut io::sink(), &page, budget, |_| Ok("cursor".into())));
    let stats = result.unwrap();
    assert_eq!(stats.emitted_records, 1);
    assert!(allocations.largest <= stats.json_bytes + 1);
    assert!(allocations.bytes <= stats.json_bytes + 4096);
    assert!(allocations.largest <= budget.maximum_bytes() + 1);
}

#[test]
fn oversized_counting_does_not_allocate_an_encoded_record_or_response_buffer() {
    let item = item("🦀\n\"\\".repeat(262_144));
    let projection = ItemProjection::Audit(ItemAudit::from(&item));
    let (result, allocations) =
        measured(|| write_get(&mut io::sink(), &projection, ResponseBudget::default()));
    let EncodeError::Read(ReadError::PayloadTooLarge(details)) = result.unwrap_err() else {
        panic!("expected payload_too_large");
    };
    assert!(details.minimum_required_bytes > 1_048_576);
    assert!(allocations.largest < 4096);
    assert!(allocations.bytes < 4096);
    // Independently measure the oversized UTF-8/escaped output after the
    // allocation window, using the equivalent v1 item plus the v2 envelope.
    let oracle = serde_json::json!({
        "api_version": 2, "schema_version": 1, "ok": true,
        "result": bif::rpc_read::get_result_json(&item),
    });
    assert_eq!(
        details.minimum_required_bytes,
        serde_json::to_vec(&oracle).unwrap().len()
    );
    eprintln!(
        "oversized counting evidence ({} JSON bytes): {allocations:?}",
        details.minimum_required_bytes
    );
}
