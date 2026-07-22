//! Benchmark: quick-command conversation persistence.
//!
//! Before this change, a deterministic quick turn paid five SQLite /
//! `spawn_blocking` round trips (`ensure` + user `append` + three result
//! `append`s). After, it pays two transactional batches. This harness times
//! both sequences against on-disk SQLite files and asserts identical persisted
//! rows so a single run is a before→after proof.
//!
//! Run with (prefer real disk on Jetson, not tmpfs):
//!   GENIE_BENCH_DIR=/opt/geniepod/bench \
//!     cargo test -p genie-core --release --test conversation_turn_bench \
//!     -- --ignored --nocapture
//!
//! Repeat the command at least 10 times on Jetson Orin Nano 8 GB and report
//! median / IQR of the printed per-turn timings. Pin power mode and clocks
//! (`nvpmodel -m 1`, `jetson_clocks`) and label warm runs separately from
//! cold (post `genie-drop-caches` / hard restart).

use genie_core::conversation::{BatchMessage, ConversationStore};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn bench_root() -> PathBuf {
    if let Ok(dir) = std::env::var("GENIE_BENCH_DIR") {
        let path = PathBuf::from(dir);
        std::fs::create_dir_all(&path).expect("create GENIE_BENCH_DIR");
        path
    } else {
        let path = std::env::temp_dir().join(format!(
            "genie-conversation-turn-bench-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp bench dir");
        path
    }
}

fn open_store(path: &Path) -> ConversationStore {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    ConversationStore::open(path).expect("open conversation store")
}

/// Faithful old path: ensure + user append + three result appends.
async fn old_quick_turn(store: &ConversationStore, conv_id: &str, user_text: &str, reply: &str) {
    store.ensure(conv_id, "New conversation").await.unwrap();
    store
        .append(conv_id, "user", user_text, None, None)
        .await
        .unwrap();

    let tool_json = r#"{"tool":"get_time","arguments":{}}"#;
    store
        .append(conv_id, "assistant", tool_json, Some("get_time"), None)
        .await
        .unwrap();
    store
        .append(
            conv_id,
            "system",
            &format!("Tool result: {reply}"),
            None,
            None,
        )
        .await
        .unwrap();
    store
        .append(conv_id, "assistant", reply, None, None)
        .await
        .unwrap();
}

/// New path: two transactional batches (user prelude, then tool results).
async fn new_quick_turn(store: &ConversationStore, conv_id: &str, user_text: &str, reply: &str) {
    store
        .ensure_and_append_batch(
            conv_id,
            "New conversation",
            vec![BatchMessage::new("user", user_text, None, None)],
        )
        .await
        .unwrap();

    let tool_json = r#"{"tool":"get_time","arguments":{}}"#;
    store
        .ensure_and_append_batch(
            conv_id,
            "New conversation",
            vec![
                BatchMessage::new("assistant", tool_json, Some("get_time"), None),
                BatchMessage::new("system", format!("Tool result: {reply}"), None, None),
                BatchMessage::new("assistant", reply, None, None),
            ],
        )
        .await
        .unwrap();
}

async fn message_fingerprint(store: &ConversationStore, conv_id: &str) -> Vec<(String, String)> {
    store
        .get_messages(conv_id)
        .await
        .unwrap()
        .into_iter()
        .map(|m| (m.role, m.content))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
async fn bench_quick_turn_conversation_writes() {
    let root = bench_root();
    let old_path = root.join("old-path.db");
    let new_path = root.join("new-path.db");

    let old_store = open_store(&old_path);
    let new_store = open_store(&new_path);

    let warmup = 10usize;
    let iterations = 500usize;
    let user_text = "what time is it?";
    let reply = "3:32 PM";

    for i in 0..warmup {
        old_quick_turn(&old_store, &format!("warm-old-{i}"), user_text, reply).await;
        new_quick_turn(&new_store, &format!("warm-new-{i}"), user_text, reply).await;
    }

    // Correctness: one paired turn must persist identical role/content order.
    let proof_old = "proof-old";
    let proof_new = "proof-new";
    old_quick_turn(&old_store, proof_old, user_text, reply).await;
    new_quick_turn(&new_store, proof_new, user_text, reply).await;
    let old_msgs = message_fingerprint(&old_store, proof_old).await;
    let new_msgs = message_fingerprint(&new_store, proof_new).await;
    assert_eq!(
        old_msgs, new_msgs,
        "batched path must persist the same messages as the old five-op path"
    );
    assert_eq!(old_msgs.len(), 4);
    assert_eq!(old_msgs[0].0, "user");
    assert_eq!(old_msgs[1].0, "assistant");
    assert_eq!(old_msgs[2].0, "system");
    assert_eq!(old_msgs[3].0, "assistant");

    let old_title = old_store
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.id == proof_old)
        .unwrap()
        .title;
    let new_title = new_store
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.id == proof_new)
        .unwrap()
        .title;
    assert_eq!(old_title, new_title);
    assert_eq!(old_title, user_text);

    let start = Instant::now();
    for i in 0..iterations {
        old_quick_turn(
            &old_store,
            &format!("bench-old-{i}"),
            black_box(user_text),
            black_box(reply),
        )
        .await;
    }
    let old_elapsed = start.elapsed();

    let start = Instant::now();
    for i in 0..iterations {
        new_quick_turn(
            &new_store,
            &format!("bench-new-{i}"),
            black_box(user_text),
            black_box(reply),
        )
        .await;
    }
    let new_elapsed = start.elapsed();

    let speedup = old_elapsed.as_secs_f64() / new_elapsed.as_secs_f64().max(f64::EPSILON);
    eprintln!(
        "BENCH conversation_turn: dir={}  warmup={warmup} iterations={iterations}\n\
         BENCH conversation_turn: old(5 ops)={old_elapsed:?} ({:?}/turn)\n\
         BENCH conversation_turn: new(2 batches)={new_elapsed:?} ({:?}/turn)\n\
         BENCH conversation_turn: speedup={speedup:.2}x",
        root.display(),
        old_elapsed / iterations as u32,
        new_elapsed / iterations as u32,
    );

    // Local/desktop SSDs still show a clear win; keep a soft floor so CI
    // accidental `--ignored` runs fail loudly if the batch path regresses.
    assert!(
        new_elapsed <= old_elapsed,
        "batched path should not be slower than the five-op path \
         (old={old_elapsed:?}, new={new_elapsed:?})"
    );

    if std::env::var("GENIE_BENCH_DIR").is_err() {
        let _ = std::fs::remove_dir_all(&root);
    }
}
