//! Benchmark: the tool-call parser's single-key normalizer only needs to know
//! whether a name is a known tool. It used to call `ToolDispatcher::tool_defs()`
//! — which builds a `ToolDef` per built-in tool (each with a `serde_json::json!`
//! parameter schema) and parses every loaded skill's `parameters_json` — and
//! then read only `.name`. `is_known_tool()` answers the same membership
//! question without building any of that.
//!
//! This times both against the same dispatcher, so a single run shows the
//! before (`tool_defs().iter().any(...)`) and after (`is_known_tool(...)`) cost
//! of the exact line the change replaces.
//!
//! Run with:
//!   cargo test -p genie-core --release --test tool_known_bench -- --ignored --nocapture

use genie_core::tools::ToolDispatcher;
use std::hint::black_box;

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_is_known_tool_vs_tool_defs() {
    let dispatcher = ToolDispatcher::new(None);
    let iters = 200_000u32;

    // A hit ("calculate") and a miss ("not_a_real_tool") — both must scan the
    // whole name set, which is the work the old path paid a full table build for.
    for name in ["calculate", "not_a_real_tool"] {
        // Warm both paths.
        for _ in 0..2000 {
            black_box(dispatcher.tool_defs().iter().any(|t| t.name == name));
            black_box(dispatcher.is_known_tool(black_box(name)));
        }

        let start = std::time::Instant::now();
        let mut acc = 0u64;
        for _ in 0..iters {
            if dispatcher
                .tool_defs()
                .iter()
                .any(|t| t.name == black_box(name))
            {
                acc += 1;
            }
        }
        let old = start.elapsed();

        let start = std::time::Instant::now();
        let mut acc2 = 0u64;
        for _ in 0..iters {
            if dispatcher.is_known_tool(black_box(name)) {
                acc2 += 1;
            }
        }
        let new = start.elapsed();

        black_box((acc, acc2));
        eprintln!(
            "BENCH known-tool [{name}]: {iters} calls  \
             tool_defs().any={old:?} ({:?}/call)  is_known_tool={new:?} ({:?}/call)",
            old / iters,
            new / iters,
        );
    }
}
