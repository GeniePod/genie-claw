use genie_core::Memory;

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn bench_search_like_fallback() {
    let dir = std::env::temp_dir().join(format!("genie-fallback-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("mem.db");

    let mem = Memory::open(&path).expect("open");

    let n = 300usize;
    for i in 0..n {
        mem.store(
            "fact",
            &format!(
                "Household qwxzytoken note {i}: the family prefers warm rooms in the evening and \
                 keeps grocery and lunchbox snacks stocked for school day {}.",
                i % 30
            ),
        )
        .expect("store");
    }

    // "wxzyto" is an infix of "qwxzytoken": no FTS token/prefix match, so search()
    // routes to the LIKE fallback; LIKE %wxzyto% matches every row.
    let query = "wxzyto";
    let limit = 50usize;

    for _ in 0..3 {
        let _ = mem.search(query, limit).expect("warm");
    }

    let iterations = 200usize;
    let start = std::time::Instant::now();
    let mut total_hits = 0usize;
    for _ in 0..iterations {
        total_hits += mem.search(query, limit).expect("search").len();
    }
    let elapsed = start.elapsed();

    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "BENCH search_like_fallback: {n} memories, {iterations} searches (limit {limit}), total \
         {elapsed:?}, per-search {:?} ({total_hits} total hits)",
        elapsed / iterations as u32,
    );
    assert!(total_hits > 0, "fallback path must return hits");
}
