use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::llm::Message;

/// Persistent conversation store.
///
/// Stores full conversation history in SQLite so chat survives
/// restarts. Supports multiple named sessions.
///
/// genie-core's HTTP server and Telegram adapter share one OS thread
/// (`tokio::main(flavor = "current_thread")`, see `lib.rs`). Every I/O method
/// therefore runs its `rusqlite` call on `tokio::task::spawn_blocking`'s
/// blocking pool rather than synchronously on the caller's task — a
/// synchronous disk write here would otherwise stall every other in-flight
/// HTTP connection and every other Telegram chat for its duration, the same
/// bug class as #77/#78 one layer down the stack. See #571.
pub struct ConversationStore {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMeta {
    pub id: String,
    pub title: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub message_count: usize,
}

/// A persisted message, including the resolved speaker (if any) that was
/// tagged when the message was appended.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

impl ConversationStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;

            CREATE TABLE IF NOT EXISTS conversations (
                id          TEXT PRIMARY KEY,
                title       TEXT NOT NULL DEFAULT 'New conversation',
                created_ms  INTEGER NOT NULL,
                updated_ms  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id          INTEGER PRIMARY KEY,
                conv_id     TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                tool_name   TEXT,
                ts_ms       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_messages_conv ON messages(conv_id, ts_ms);
            ",
        )?;

        // Migrate existing databases: add speaker column if not present (#559).
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN speaker TEXT", []);

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run `f` against the connection on the blocking thread pool.
    ///
    /// Every public I/O method below is a thin wrapper around this — it's
    /// the one place that actually touches SQLite, so it's the one place
    /// that has to leave the async executor thread to do it.
    async fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            f(&conn)
        })
        .await
        .map_err(|e| anyhow::anyhow!("conversation store blocking task failed: {e}"))?
    }

    /// Create a new conversation. Returns the conversation ID.
    pub async fn create(&self) -> Result<String> {
        self.with_conn(|conn| {
            let id = generate_id();
            let now = now_ms();
            conn.execute(
                "INSERT INTO conversations (id, title, created_ms, updated_ms) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, "New conversation", now, now],
            )?;
            Ok(id)
        })
        .await
    }

    /// Ensure a conversation with a stable ID exists.
    pub async fn ensure(&self, id: &str, title: &str) -> Result<()> {
        let id = id.to_string();
        let title = title.to_string();
        self.with_conn(move |conn| {
            let now = now_ms();
            conn.execute(
                "INSERT OR IGNORE INTO conversations (id, title, created_ms, updated_ms) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, title, now, now],
            )?;
            Ok(())
        })
        .await
    }

    /// Append a message to a conversation.
    pub async fn append(
        &self,
        conv_id: &str,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        speaker: Option<&str>,
    ) -> Result<()> {
        let conv_id = conv_id.to_string();
        let role = role.to_string();
        let content = content.to_string();
        let tool_name = tool_name.map(|s| s.to_string());
        let speaker = speaker.map(|s| s.to_string());
        self.with_conn(move |conn| {
            let now = now_ms();
            conn.execute(
                "INSERT INTO messages (conv_id, role, content, tool_name, ts_ms, speaker) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![conv_id, role, content, tool_name, now, speaker],
            )?;

            // Update conversation title from first user message.
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE conv_id = ?1 AND role = 'user'",
                [&conv_id],
                |row| row.get(0),
            )?;
            if count == 1 && role == "user" {
                // `&content[..57]` is a byte slice and panics if byte 57 falls
                // inside a multi-byte UTF-8 codepoint — e.g. an emoji 4-byte char
                // or a Cyrillic / Greek / Hebrew / Arabic 2-byte char at an odd
                // alignment. With `panic = "abort"` in the release profile
                // (Cargo.toml), the daemon would die on the user's first emoji
                // message. Same bug class as #147 / PR #150 (UTF-8 slice in
                // `llm::openai_compat::truncate_body`); fix is the same shape:
                // walk back to the nearest char boundary before slicing.
                let title = if content.len() > 60 {
                    format!("{}...", truncate_at_char_boundary(&content, 57))
                } else {
                    content.clone()
                };
                conn.execute(
                    "UPDATE conversations SET title = ?1, updated_ms = ?2 WHERE id = ?3",
                    rusqlite::params![title, now, conv_id],
                )?;
            } else {
                conn.execute(
                    "UPDATE conversations SET updated_ms = ?1 WHERE id = ?2",
                    rusqlite::params![now, conv_id],
                )?;
            }

            Ok(())
        })
        .await
    }

    /// Append a message, logging SQLite/IO failures instead of silently dropping them.
    pub async fn append_or_log(
        &self,
        conv_id: &str,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        speaker: Option<&str>,
    ) {
        if let Err(error) = self
            .append(conv_id, role, content, tool_name, speaker)
            .await
        {
            tracing::error!(
                conv_id,
                role,
                tool_name,
                error = %error,
                "conversation append failed"
            );
        }
    }

    /// Get all messages in a conversation, including the tagged speaker (if any).
    pub async fn get_messages(&self, conv_id: &str) -> Result<Vec<StoredMessage>> {
        let conv_id = conv_id.to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT role, content, speaker FROM messages WHERE conv_id = ?1 ORDER BY ts_ms ASC",
            )?;

            let messages = stmt
                .query_map([conv_id], |row| {
                    Ok(StoredMessage {
                        role: row.get(0)?,
                        content: row.get(1)?,
                        speaker: row.get(2)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            Ok(messages)
        })
        .await
    }

    /// Get recent N messages (for context window).
    pub async fn get_recent(&self, conv_id: &str, limit: usize) -> Result<Vec<Message>> {
        let conv_id = conv_id.to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT role, content FROM (
                    SELECT role, content, ts_ms, id FROM messages
                    WHERE conv_id = ?1
                    ORDER BY ts_ms DESC, id DESC LIMIT ?2
                 ) ORDER BY ts_ms ASC, id ASC",
            )?;

            let messages = stmt
                .query_map(rusqlite::params![conv_id, limit], |row| {
                    Ok(Message {
                        role: row.get(0)?,
                        content: row.get(1)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            Ok(messages)
        })
        .await
    }

    /// List all conversations (most recent first).
    pub async fn list(&self) -> Result<Vec<ConversationMeta>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT c.id, c.title, c.created_ms, c.updated_ms,
                        (SELECT COUNT(*) FROM messages WHERE conv_id = c.id)
                 FROM conversations c
                 ORDER BY c.updated_ms DESC",
            )?;

            let convos = stmt
                .query_map([], |row| {
                    Ok(ConversationMeta {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        created_ms: row.get(2)?,
                        updated_ms: row.get(3)?,
                        message_count: row.get::<_, i64>(4)? as usize,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            Ok(convos)
        })
        .await
    }

    /// Delete a conversation and all its messages.
    pub async fn delete(&self, conv_id: &str) -> Result<()> {
        let conv_id = conv_id.to_string();
        self.with_conn(move |conn| {
            conn.execute("DELETE FROM messages WHERE conv_id = ?1", [&conv_id])?;
            conn.execute("DELETE FROM conversations WHERE id = ?1", [&conv_id])?;
            Ok(())
        })
        .await
    }

    /// Remove old conversation messages and trim long conversations.
    ///
    /// Two passes are run:
    /// 1. Messages older than `retention_days` are deleted across all conversations
    ///    (pass 0 to skip age-based pruning).
    /// 2. Each conversation that exceeds `max_messages_per_conversation` has its
    ///    oldest messages deleted until only the most recent N remain
    ///    (pass 0 to skip per-conversation capping).
    ///
    /// After both passes, `PRAGMA wal_checkpoint(TRUNCATE)` is issued so the WAL
    /// file is folded back into the main database and freed pages are returned
    /// to the OS.
    ///
    /// Returns the total number of messages deleted.
    pub async fn prune_old_turns(
        &self,
        retention_days: u32,
        max_messages_per_conversation: usize,
    ) -> Result<usize> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut deleted = 0usize;

            if retention_days > 0 {
                let cutoff_ms = now_ms() - (retention_days as i64 * 86_400_000);
                deleted += tx.execute("DELETE FROM messages WHERE ts_ms < ?1", [cutoff_ms])?;
                // Remove conversations that have no remaining messages.
                tx.execute(
                    "DELETE FROM conversations \
                     WHERE id NOT IN (SELECT DISTINCT conv_id FROM messages)",
                    [],
                )?;
            }

            if max_messages_per_conversation > 0 {
                let over_limit: Vec<String> = {
                    let mut stmt = tx.prepare(
                        "SELECT id FROM conversations \
                         WHERE (SELECT COUNT(*) FROM messages WHERE conv_id = conversations.id) > ?1",
                    )?;
                    stmt.query_map([max_messages_per_conversation as i64], |row| row.get(0))?
                        .filter_map(|r| r.ok())
                        .collect()
                };
                for conv_id in &over_limit {
                    deleted += tx.execute(
                        "DELETE FROM messages \
                         WHERE conv_id = ?1 \
                           AND id NOT IN ( \
                               SELECT id FROM messages \
                               WHERE conv_id = ?1 \
                               ORDER BY ts_ms DESC, id DESC \
                               LIMIT ?2 \
                           )",
                        rusqlite::params![conv_id, max_messages_per_conversation as i64],
                    )?;
                }
            }

            tx.commit()?;
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;

            Ok(deleted)
        })
        .await
    }

    /// Return the combined on-disk size of the conversation database and its
    /// WAL file in bytes. The WAL can be sizeable between checkpoints, so
    /// omitting it would under-report actual disk use in WAL mode.
    pub async fn db_size_bytes(&self) -> Option<u64> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            let path = conn.path()?.to_string();
            let main_size = std::fs::metadata(&path).ok()?.len();
            let wal_size = std::fs::metadata(format!("{}-wal", path))
                .map(|m| m.len())
                .unwrap_or(0);
            Some(main_size + wal_size)
        })
        .await
        .unwrap_or(None)
    }

    /// Delete all conversations.
    pub async fn clear_all(&self) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM messages", [])?;
            conn.execute("DELETE FROM conversations", [])?;
            Ok(())
        })
        .await
    }

    /// Export a conversation as JSON.
    pub async fn export_json(&self, conv_id: &str) -> Result<String> {
        let meta = {
            let conv_id = conv_id.to_string();
            self.with_conn(move |conn| {
                let meta_opt: Option<ConversationMeta> = conn
                    .query_row(
                        "SELECT id, title, created_ms, updated_ms FROM conversations WHERE id = ?1",
                        [&conv_id],
                        |row| {
                            Ok(ConversationMeta {
                                id: row.get(0)?,
                                title: row.get(1)?,
                                created_ms: row.get(2)?,
                                updated_ms: row.get(3)?,
                                message_count: 0,
                            })
                        },
                    )
                    .ok();

                let Some(meta) = meta_opt else {
                    anyhow::bail!("conversation not found: {}", conv_id);
                };
                Ok(meta)
            })
            .await?
        };

        let messages = self.get_messages(conv_id).await?;

        let export = serde_json::json!({
            "id": meta.id,
            "title": meta.title,
            "created": meta.created_ms,
            "updated": meta.updated_ms,
            "messages": messages,
        });

        Ok(serde_json::to_string_pretty(&export)?)
    }
}

fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("conv-{:x}", ts)
}

/// Truncate `text` to at most `max_bytes` bytes, walking back to the nearest
/// UTF-8 char boundary so a slice on a multi-byte codepoint never panics.
fn truncate_at_char_boundary(text: &str, mut max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    while max_bytes > 0 && !text.is_char_boundary(max_bytes) {
        max_bytes -= 1;
    }
    &text[..max_bytes]
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_ID: AtomicU32 = AtomicU32::new(0);

    fn temp_store() -> ConversationStore {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geniepod-conv-test-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        ConversationStore::open(&path).unwrap()
    }

    #[tokio::test]
    async fn create_and_list() {
        let store = temp_store();
        let id = store.create().await.unwrap();
        assert!(id.starts_with("conv-"));

        let convos = store.list().await.unwrap();
        assert_eq!(convos.len(), 1);
        assert_eq!(convos[0].title, "New conversation");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn append_returns_error_on_readonly_db() {
        use std::os::unix::fs::PermissionsExt;

        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geniepod-conv-readonly-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        let store = ConversationStore::open(&path).unwrap();
        let conv_id = store.create().await.unwrap();
        drop(store);

        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&path, perms).unwrap();

        let store = ConversationStore {
            conn: Arc::new(Mutex::new(
                Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap(),
            )),
        };
        let error = store
            .append(&conv_id, "assistant", "hello", None, None)
            .await
            .unwrap_err();
        assert!(
            !error.to_string().is_empty(),
            "append should fail on readonly db: {error}"
        );

        store
            .append_or_log(&conv_id, "assistant", "hello", None, None)
            .await;

        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn append_and_get() {
        let store = temp_store();
        let id = store.create().await.unwrap();

        store
            .append(&id, "user", "hello", None, None)
            .await
            .unwrap();
        store
            .append(&id, "assistant", "hi there!", None, None)
            .await
            .unwrap();

        let messages = store.get_messages(&id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, "assistant");
    }

    #[tokio::test]
    async fn append_persists_speaker_round_trip() {
        let store = temp_store();
        let id = store.create().await.unwrap();

        store
            .append(&id, "user", "hello", None, Some("dana"))
            .await
            .unwrap();
        store
            .append(&id, "assistant", "hi there!", None, None)
            .await
            .unwrap();

        let messages = store.get_messages(&id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].speaker.as_deref(),
            Some("dana"),
            "speaker tagged on the user turn should round-trip through storage"
        );
        assert_eq!(
            messages[1].speaker, None,
            "turns appended without a speaker should read back as None"
        );
    }

    #[tokio::test]
    async fn auto_title_from_first_message() {
        let store = temp_store();
        let id = store.create().await.unwrap();

        store
            .append(&id, "user", "what's the weather in Tokyo?", None, None)
            .await
            .unwrap();

        let convos = store.list().await.unwrap();
        assert_eq!(convos[0].title, "what's the weather in Tokyo?");
    }

    #[tokio::test]
    async fn get_recent_limits() {
        let store = temp_store();
        let id = store.create().await.unwrap();

        for i in 0..10 {
            store
                .append(&id, "user", &format!("msg {}", i), None, None)
                .await
                .unwrap();
        }

        let recent = store.get_recent(&id, 3).await.unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].content, "msg 7");
        assert_eq!(recent[2].content, "msg 9");
    }

    #[tokio::test]
    async fn delete_conversation() {
        let store = temp_store();
        let id = store.create().await.unwrap();
        store.append(&id, "user", "test", None, None).await.unwrap();

        store.delete(&id).await.unwrap();
        assert_eq!(store.list().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn export_json() {
        let store = temp_store();
        let id = store.create().await.unwrap();
        store
            .append(&id, "user", "hello", None, None)
            .await
            .unwrap();
        store
            .append(&id, "assistant", "world", None, None)
            .await
            .unwrap();

        let json = store.export_json(&id).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["messages"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn ensure_stable_conversation_id_is_idempotent() {
        let store = temp_store();
        store.ensure("telegram-123", "Telegram 123").await.unwrap();
        store
            .ensure("telegram-123", "Second title ignored")
            .await
            .unwrap();

        let convos = store.list().await.unwrap();
        assert_eq!(convos.len(), 1);
        assert_eq!(convos[0].id, "telegram-123");
        assert_eq!(convos[0].title, "Telegram 123");
    }

    /// Direct helper coverage: the truncation must always land on a char
    /// boundary, never inside a multi-byte codepoint, regardless of input.
    #[test]
    fn truncate_at_char_boundary_walks_back_to_a_char_edge() {
        // ASCII fits exactly — no truncation.
        assert_eq!(truncate_at_char_boundary("hello", 5), "hello");
        // ASCII over-budget — cut at the byte budget.
        assert_eq!(truncate_at_char_boundary("hello world", 5), "hello");
        // 16 × U+1F382 BIRTHDAY CAKE (4 bytes each, 64 bytes total). Asked
        // for 57 bytes; must walk back to 56 (last char boundary <= 57).
        let cakes = "🎂".repeat(16);
        let out = truncate_at_char_boundary(&cakes, 57);
        assert!(out.is_char_boundary(out.len()));
        assert_eq!(out.len(), 56);
        assert_eq!(out.chars().count(), 14);
        // 31 × U+0439 CYRILLIC SMALL LETTER SHORT I (2 bytes each, 62 bytes).
        // Asked for 57 bytes; byte 57 is odd, must walk back to 56.
        let cyr = "й".repeat(31);
        let out = truncate_at_char_boundary(&cyr, 57);
        assert!(out.is_char_boundary(out.len()));
        assert_eq!(out.len(), 56);
        assert_eq!(out.chars().count(), 28);
        // Empty string — no panic, returns empty.
        assert_eq!(truncate_at_char_boundary("", 57), "");
    }

    /// Regression for the bug fixed here: a >60-byte emoji first message on
    /// a new conversation used to panic at `&content[..57]` because byte 57
    /// is inside a 4-byte emoji codepoint. With `panic = "abort"` in the
    /// release profile this aborted the whole `genie-core` daemon. After the
    /// fix, `append` must succeed and produce a valid UTF-8 title.
    #[tokio::test]
    async fn append_title_truncates_emoji_first_message_without_panic() {
        let store = temp_store();
        let id = store.create().await.unwrap();

        let message = format!("I love coding! {}", "🎉".repeat(13));
        assert!(message.len() > 60, "test fixture must trigger the >60 path");
        store
            .append(&id, "user", &message, None, None)
            .await
            .unwrap();

        let convos = store.list().await.unwrap();
        let title = &convos[0].title;
        assert!(title.ends_with("..."), "title must end with the ... suffix");
        // Title must be valid UTF-8 (it always is in Rust, but we also want
        // to assert no broken-emoji is rendered: the last codepoint before
        // the trailing "..." must be a whole '🎉'.
        let body = title.trim_end_matches("...");
        assert!(body.is_char_boundary(body.len()));
        assert!(
            body.chars()
                .last()
                .map(|c| c == '🎉' || c == ' ' || c.is_ascii())
                .unwrap_or(false)
        );
    }

    /// Regression for the Cyrillic odd-byte-alignment case. With 2-byte
    /// codepoints, byte 57 is inside the 29th char and the old code panicked.
    /// The new code must succeed and clip on a char boundary.
    #[tokio::test]
    async fn append_title_handles_cyrillic_first_message_at_odd_byte_boundary() {
        let store = temp_store();
        let id = store.create().await.unwrap();

        // 31 × "й" = 62 bytes. Byte 57 is inside char 29 (0-indexed 28).
        let message = "й".repeat(31);
        assert!(message.len() > 60);
        store
            .append(&id, "user", &message, None, None)
            .await
            .unwrap();

        let convos = store.list().await.unwrap();
        let title = &convos[0].title;
        assert!(title.ends_with("..."));
        let body = title.trim_end_matches("...");
        assert!(body.is_char_boundary(body.len()));
        assert!(body.chars().all(|c| c == 'й'));
    }

    /// Short messages must be used verbatim — confirms the `else` branch
    /// (no truncation) isn't accidentally regressed by the helper plumbing.
    #[tokio::test]
    async fn append_title_short_message_used_verbatim() {
        let store = temp_store();
        let id = store.create().await.unwrap();
        store
            .append(&id, "user", "set a timer", None, None)
            .await
            .unwrap();
        let convos = store.list().await.unwrap();
        assert_eq!(convos[0].title, "set a timer");
    }

    /// Long ASCII path (the case that already worked) must still produce
    /// the same 57-byte-prefix + "..." title.
    #[tokio::test]
    async fn append_title_long_ascii_truncates_with_ellipsis() {
        let store = temp_store();
        let id = store.create().await.unwrap();
        let message = "a".repeat(70);
        store
            .append(&id, "user", &message, None, None)
            .await
            .unwrap();
        let convos = store.list().await.unwrap();
        assert_eq!(convos[0].title, format!("{}...", "a".repeat(57)));
    }

    /// Regression for #571: conversation writes must not monopolize the
    /// single-threaded executor. `append` runs its `rusqlite` work on
    /// `spawn_blocking`'s pool, so a concurrently spawned task with no
    /// blocking work of its own should be able to interleave and finish
    /// promptly instead of waiting for every write to complete first. Before
    /// the fix, `append`'s synchronous body ran to completion inside a
    /// single poll of the writer task, starving everything else on the
    /// current-thread runtime until it was done.
    #[tokio::test]
    async fn appends_do_not_starve_a_concurrent_task() {
        let store = Arc::new(temp_store());
        let id = store.create().await.unwrap();

        let writer = {
            let store = Arc::clone(&store);
            let id = id.clone();
            tokio::spawn(async move {
                for i in 0..200 {
                    store
                        .append(&id, "user", &format!("message number {i}"), None, None)
                        .await
                        .unwrap();
                }
            })
        };

        let interleaved = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for _ in 0..2000 {
                tokio::task::yield_now().await;
            }
        })
        .await;

        writer.await.unwrap();
        assert!(
            interleaved.is_ok(),
            "a concurrent task was starved while conversation writes were in flight — \
             ConversationStore regressed to blocking the executor thread"
        );
    }

    fn insert_at(store: &ConversationStore, conv_id: &str, content: &str, ts_ms: i64) {
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO messages (conv_id, role, content, tool_name, ts_ms) \
                 VALUES (?1, 'user', ?2, NULL, ?3)",
                rusqlite::params![conv_id, content, ts_ms],
            )
            .unwrap();
    }

    fn msg_count(store: &ConversationStore, conv_id: &str) -> usize {
        store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE conv_id = ?1",
                [conv_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap() as usize
    }

    fn conv_exists(store: &ConversationStore, conv_id: &str) -> bool {
        store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM conversations WHERE id = ?1",
                [conv_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    }

    fn msg_contents(store: &ConversationStore, conv_id: &str) -> Vec<String> {
        let conn = store.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT content FROM messages WHERE conv_id = ?1 ORDER BY ts_ms ASC, id ASC")
            .unwrap();
        stmt.query_map([conv_id], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    fn make_conv(store: &ConversationStore, id: &str) {
        let now = now_ms();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO conversations (id, title, created_ms, updated_ms) \
                 VALUES (?1, ?1, ?2, ?2)",
                rusqlite::params![id, now],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn prune_old_turns_deletes_stale_keeps_recent_removes_orphan_trims_cap() {
        let store = temp_store();
        let now = now_ms();
        let day_ms = 86_400_000i64;

        // conv_keep: 1 old message (will be deleted) + 1 recent (kept).
        let keep = "conv-keep";
        make_conv(&store, keep);
        insert_at(&store, keep, "old", now - 3 * day_ms);
        insert_at(&store, keep, "recent", now);

        // conv_gone: only old messages → conversation itself must be removed.
        let gone = "conv-gone";
        make_conv(&store, gone);
        insert_at(&store, gone, "old-a", now - 3 * day_ms);
        insert_at(&store, gone, "old-b", now - 3 * day_ms);

        // conv_cap: 5 messages, cap at 3 → oldest 2 dropped, newest 3 kept.
        let cap = "conv-cap";
        make_conv(&store, cap);
        for i in 0..5i64 {
            insert_at(&store, cap, &format!("msg-{i}"), now - (4 - i) * 1000);
        }

        let deleted = store.prune_old_turns(2, 3).await.unwrap();

        // Age pass: 3 old rows deleted (1 from conv_keep + 2 from conv_gone).
        // Cap pass: 2 rows deleted from conv_cap.
        assert_eq!(deleted, 5);

        // conv_keep: only the recent message remains.
        assert!(conv_exists(&store, keep));
        assert_eq!(msg_count(&store, keep), 1);
        assert_eq!(msg_contents(&store, keep), vec!["recent"]);

        // conv_gone: removed entirely (no surviving messages).
        assert!(!conv_exists(&store, gone));

        // conv_cap: exactly the 3 newest messages (msg-2, msg-3, msg-4).
        assert!(conv_exists(&store, cap));
        let remaining = msg_contents(&store, cap);
        assert_eq!(remaining, vec!["msg-2", "msg-3", "msg-4"]);
    }
}
