use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::llm::Message;

/// Persistent conversation store.
///
/// Stores full conversation history in SQLite so chat survives
/// restarts. Supports multiple named sessions.
pub struct ConversationStore {
    conn: Connection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMeta {
    pub id: String,
    pub title: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub message_count: usize,
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

        Ok(Self { conn })
    }

    /// Create a new conversation. Returns the conversation ID.
    pub fn create(&self) -> Result<String> {
        let id = generate_id();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO conversations (id, title, created_ms, updated_ms) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, "New conversation", now, now],
        )?;
        Ok(id)
    }

    /// Ensure a conversation with a stable ID exists.
    pub fn ensure(&self, id: &str, title: &str) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "INSERT OR IGNORE INTO conversations (id, title, created_ms, updated_ms) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, title, now, now],
        )?;
        Ok(())
    }

    /// Append a message to a conversation.
    pub fn append(
        &self,
        conv_id: &str,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
    ) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO messages (conv_id, role, content, tool_name, ts_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![conv_id, role, content, tool_name, now],
        )?;

        // Update conversation title from first user message.
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE conv_id = ?1 AND role = 'user'",
            [conv_id],
            |row| row.get(0),
        )?;
        if count == 1 && role == "user" {
            let title = if content.len() > 60 {
                format!("{}...", &content[..57])
            } else {
                content.to_string()
            };
            self.conn.execute(
                "UPDATE conversations SET title = ?1, updated_ms = ?2 WHERE id = ?3",
                rusqlite::params![title, now, conv_id],
            )?;
        } else {
            self.conn.execute(
                "UPDATE conversations SET updated_ms = ?1 WHERE id = ?2",
                rusqlite::params![now, conv_id],
            )?;
        }

        Ok(())
    }

    /// Append a message, logging SQLite/IO failures instead of silently dropping them.
    pub fn append_or_log(&self, conv_id: &str, role: &str, content: &str, tool_name: Option<&str>) {
        if let Err(error) = self.append(conv_id, role, content, tool_name) {
            tracing::error!(
                conv_id,
                role,
                tool_name,
                error = %error,
                "conversation append failed"
            );
        }
    }

    /// Get all messages in a conversation.
    pub fn get_messages(&self, conv_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self
            .conn
            .prepare("SELECT role, content FROM messages WHERE conv_id = ?1 ORDER BY ts_ms ASC")?;

        let messages = stmt
            .query_map([conv_id], |row| {
                Ok(Message {
                    role: row.get(0)?,
                    content: row.get(1)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(messages)
    }

    /// Get recent N messages (for context window).
    pub fn get_recent(&self, conv_id: &str, limit: usize) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
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
    }

    /// List all conversations (most recent first).
    pub fn list(&self) -> Result<Vec<ConversationMeta>> {
        let mut stmt = self.conn.prepare(
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
    }

    /// Delete a conversation and all its messages.
    pub fn delete(&self, conv_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM messages WHERE conv_id = ?1", [conv_id])?;
        self.conn
            .execute("DELETE FROM conversations WHERE id = ?1", [conv_id])?;
        Ok(())
    }

    /// Delete all conversations.
    pub fn clear_all(&self) -> Result<()> {
        self.conn.execute("DELETE FROM messages", [])?;
        self.conn.execute("DELETE FROM conversations", [])?;
        Ok(())
    }

    /// Export a conversation as JSON.
    pub fn export_json(&self, conv_id: &str) -> Result<String> {
        let meta_opt: Option<ConversationMeta> = self
            .conn
            .query_row(
                "SELECT id, title, created_ms, updated_ms FROM conversations WHERE id = ?1",
                [conv_id],
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

        let messages = self.get_messages(conv_id)?;

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

    #[test]
    fn create_and_list() {
        let store = temp_store();
        let id = store.create().unwrap();
        assert!(id.starts_with("conv-"));

        let convos = store.list().unwrap();
        assert_eq!(convos.len(), 1);
        assert_eq!(convos[0].title, "New conversation");
    }

    #[test]
    #[cfg(unix)]
    fn append_returns_error_on_readonly_db() {
        use std::os::unix::fs::PermissionsExt;

        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geniepod-conv-readonly-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        let store = ConversationStore::open(&path).unwrap();
        let conv_id = store.create().unwrap();
        drop(store);

        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&path, perms).unwrap();

        let store = ConversationStore {
            conn: Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap(),
        };
        let error = store
            .append(&conv_id, "assistant", "hello", None)
            .unwrap_err();
        assert!(
            !error.to_string().is_empty(),
            "append should fail on readonly db: {error}"
        );

        store.append_or_log(&conv_id, "assistant", "hello", None);

        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_and_get() {
        let store = temp_store();
        let id = store.create().unwrap();

        store.append(&id, "user", "hello", None).unwrap();
        store.append(&id, "assistant", "hi there!", None).unwrap();

        let messages = store.get_messages(&id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn auto_title_from_first_message() {
        let store = temp_store();
        let id = store.create().unwrap();

        store
            .append(&id, "user", "what's the weather in Tokyo?", None)
            .unwrap();

        let convos = store.list().unwrap();
        assert_eq!(convos[0].title, "what's the weather in Tokyo?");
    }

    #[test]
    fn get_recent_limits() {
        let store = temp_store();
        let id = store.create().unwrap();

        for i in 0..10 {
            store
                .append(&id, "user", &format!("msg {}", i), None)
                .unwrap();
        }

        let recent = store.get_recent(&id, 3).unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].content, "msg 7");
        assert_eq!(recent[2].content, "msg 9");
    }

    #[test]
    fn delete_conversation() {
        let store = temp_store();
        let id = store.create().unwrap();
        store.append(&id, "user", "test", None).unwrap();

        store.delete(&id).unwrap();
        assert_eq!(store.list().unwrap().len(), 0);
    }

    #[test]
    fn export_json() {
        let store = temp_store();
        let id = store.create().unwrap();
        store.append(&id, "user", "hello", None).unwrap();
        store.append(&id, "assistant", "world", None).unwrap();

        let json = store.export_json(&id).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn ensure_stable_conversation_id_is_idempotent() {
        let store = temp_store();
        store.ensure("telegram-123", "Telegram 123").unwrap();
        store
            .ensure("telegram-123", "Second title ignored")
            .unwrap();

        let convos = store.list().unwrap();
        assert_eq!(convos.len(), 1);
        assert_eq!(convos[0].id, "telegram-123");
        assert_eq!(convos[0].title, "Telegram 123");
    }
}
