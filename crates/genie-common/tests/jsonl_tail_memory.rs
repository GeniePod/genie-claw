//! `tail_lines` must bound its peak buffering by `max_line_bytes`, not by the
//! length of the file's longest line.
//!
//! The behavioural tests in `jsonl.rs` only observe *which* lines come back —
//! they pass just as well against an implementation that reads a 50 MB line
//! into memory and then discards it. This file measures the allocation itself,
//! which is the actual contract `max_line_bytes` documents ("skip individual
//! lines larger than this instead of allocating them whole") and the reason it
//! exists on a memory-constrained device.
//!
//! It lives in its own integration-test binary because it installs a
//! `#[global_allocator]`; keeping it isolated means the instrumentation cannot
//! perturb, or be perturbed by, any other test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

struct PeakTrackingAllocator;

fn record_growth(bytes: usize) {
    let live = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for PeakTrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record_growth(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            if new_size >= layout.size() {
                record_growth(new_size - layout.size());
            } else {
                LIVE_BYTES.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOCATOR: PeakTrackingAllocator = PeakTrackingAllocator;

const LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 4096;
/// Generous ceiling: ~256x `MAX_LINE_BYTES`, still ~8x below the line itself.
/// Wide enough that incidental test-harness allocation cannot trip it, tight
/// enough that buffering the line whole cannot pass.
const PEAK_BUDGET_BYTES: usize = 1024 * 1024;

/// Write the fixture in small chunks so building it does not itself allocate
/// anything close to the budget — the measurement window opens afterwards.
fn write_fixture(path: &std::path::Path) {
    let mut file = std::fs::File::create(path).unwrap();
    writeln!(file, r#"{{"n":1}}"#).unwrap();

    file.write_all(br#"{"blob":""#).unwrap();
    let filler = vec![b'x'; 64 * 1024];
    let mut written = 0;
    while written < LINE_BYTES {
        let take = filler.len().min(LINE_BYTES - written);
        file.write_all(&filler[..take]).unwrap();
        written += take;
    }
    file.write_all(b"\"}\n").unwrap();

    writeln!(file, r#"{{"n":3}}"#).unwrap();
    file.flush().unwrap();
}

#[test]
fn tail_lines_peak_allocation_stays_bounded_by_max_line_bytes() {
    let path = std::env::temp_dir().join(format!(
        "geniepod-jsonl-tail-mem-{}-{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&path);
    write_fixture(&path);

    // Open the measurement window only now, so the fixture's own allocations
    // are excluded and PEAK reflects the tail read alone.
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(baseline, Ordering::Relaxed);

    let lines = genie_common::jsonl::tail_lines(&path, 5, MAX_LINE_BYTES).unwrap();

    let peak_growth = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    let _ = std::fs::remove_file(&path);

    // Correctness first: the oversize line is skipped, its neighbours survive.
    assert_eq!(
        lines,
        vec![r#"{"n":1}"#.to_string(), r#"{"n":3}"#.to_string()],
        "oversize line must be skipped and its neighbours returned"
    );

    // Then the actual contract. Buffering the line whole costs >= 8 MiB here,
    // so this fails loudly against the pre-fix implementation.
    assert!(
        peak_growth < PEAK_BUDGET_BYTES,
        "tail_lines allocated {peak_growth} bytes for an {LINE_BYTES}-byte line \
         with max_line_bytes={MAX_LINE_BYTES}; budget is {PEAK_BUDGET_BYTES} bytes. \
         An over-long line must be abandoned as it streams, not buffered and \
         then discarded."
    );
}
