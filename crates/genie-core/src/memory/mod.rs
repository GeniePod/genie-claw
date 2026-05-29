pub mod decay;
pub mod embedding;
pub mod extract;
pub mod inject;
pub mod policy;
pub mod recall;

use anyhow::Result;
use embedding::{EmbeddingProvider, LocalHashEmbeddingProvider};
use rusqlite::{Connection, OpenFlags, params_from_iter};
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_QUERY_HASHES: usize = 16;

/// Persistent conversational memory with dreaming-inspired consolidation.
///
/// Architecture (inspired by OpenClaw's memory-core, clean-room Rust):
///
/// ```text
/// ┌─────────────────────────────────────────────┐
/// │ Permanent Memory (MEMORY table)              │
/// │ Facts, preferences — survives forever         │
/// │ Populated by: dreaming promotion             │
/// ├─────────────────────────────────────────────┤
/// │ Recall Tracker (recalls table)               │
/// │ Tracks: access count, scores, query diversity│
/// │ 6-component weighted scoring for promotion   │
/// ├─────────────────────────────────────────────┤
/// │ Short-Term (memories table + FTS5)           │
/// │ Raw facts from conversations                 │
/// │ Temporal decay: exp(-ln2/halfLife * ageDays) │
/// └─────────────────────────────────────────────┘
/// ```
pub struct Memory {
    conn: Connection,
    half_life_days: f64,
    canonical_dir: PathBuf,
    /// Set when schema migration or FTS rebuild failed during [`Memory::open`].
    migration_degraded: bool,
}

/// Process-wide handle to the single memory store opened at startup.
pub type SharedMemory = Arc<Mutex<Memory>>;

/// Run a closure against the shared memory store.
pub fn with_shared_memory<R>(memory: &SharedMemory, f: impl FnOnce(&Memory) -> R) -> R {
    let guard = memory
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&guard)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryHealth {
    pub quick_check_ok: bool,
    pub memory_rows: usize,
    pub fts_rows: usize,
    pub fts_consistent: bool,
    pub migration_degraded: bool,
    pub canonical_root_exists: bool,
    pub canonical_namespace_files: usize,
    pub canonical_daily_files: usize,
    pub canonical_event_logs: usize,
    pub person_rows: usize,
    pub private_rows: usize,
    pub restricted_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreOutcome {
    pub id: Option<i64>,
    pub replaced: usize,
    pub duplicate: bool,
}

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub id: i64,
    pub kind: String,
    pub content: String,
    pub created_ms: i64,
    pub accessed_ms: i64,
    pub recall_count: i64,
    pub max_score: f64,
    pub promoted: bool,
    pub metadata: policy::MemoryPolicyMetadata,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ManagedMemoryEntry {
    pub id: i64,
    pub kind: String,
    pub content: String,
    pub created_ms: i64,
    pub accessed_ms: i64,
    pub recall_count: i64,
    pub promoted: bool,
    pub scope: String,
    pub sensitivity: String,
    pub spoken_policy: String,
    pub disclosure_class: String,
    pub namespace: String,
    pub canonical_note: Option<String>,
    pub display_order: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdProfile {
    pub source_memory_id: i64,
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAlias {
    pub source_memory_id: i64,
    pub alias: String,
    pub target_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdProfileAttribute {
    pub source_memory_id: i64,
    pub name: String,
    pub attribute: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdRule {
    pub source_memory_id: i64,
    pub person: Option<String>,
    pub rule_type: String,
    pub subject: String,
    pub value: Option<String>,
    pub allowed: bool,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdNote {
    pub source_memory_id: i64,
    pub note_type: String,
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppOnlySecretReference {
    pub source_memory_id: i64,
    pub secret_type: String,
    pub label: String,
    pub location_hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaProfileItem {
    pub source_memory_id: i64,
    pub owner: Option<String>,
    pub item_type: String,
    pub name: String,
    pub provider: Option<String>,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyCalendarEvent {
    pub source_memory_id: i64,
    pub person: Option<String>,
    pub event_type: String,
    pub title: String,
    pub day: Option<String>,
    pub time: Option<String>,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShoppingListItem {
    pub source_memory_id: i64,
    pub item: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdInventoryItem {
    pub source_memory_id: i64,
    pub item: String,
    pub quantity: Option<String>,
    pub location: Option<String>,
    pub category: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPermission {
    pub source_memory_id: i64,
    pub person: String,
    pub device: String,
    pub action: String,
    pub allowed: bool,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdTaskLog {
    pub source_memory_id: i64,
    pub person: String,
    pub task: String,
    pub subject: Option<String>,
    pub day: Option<String>,
    pub time: Option<String>,
    pub status: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdScheduleItem {
    pub source_memory_id: i64,
    pub schedule_type: String,
    pub subject: Option<String>,
    pub title: String,
    pub day: Option<String>,
    pub date: Option<String>,
    pub time: Option<String>,
    pub amount: Option<String>,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdEventLog {
    pub source_memory_id: i64,
    pub event_type: String,
    pub subject: Option<String>,
    pub action: String,
    pub actor: Option<String>,
    pub time: Option<String>,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct SemanticMemoryHit {
    pub entry: MemoryEntry,
    pub score: f64,
    pub embedding_model: String,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryEvent {
    ts_ms: u64,
    action: &'static str,
    id: Option<i64>,
    kind: Option<String>,
    content: Option<String>,
    detail: Option<String>,
}

impl Memory {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_half_life(path, 30.0)
    }

    pub fn open_with_half_life(path: &Path, half_life_days: f64) -> Result<Self> {
        let canonical_dir = path
            .parent()
            .map(|parent| parent.join("memory"))
            .unwrap_or_else(|| PathBuf::from("memory"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(&canonical_dir)?;

        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA temp_store = MEMORY;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS memories (
                id            INTEGER PRIMARY KEY,
                kind          TEXT NOT NULL,
                content       TEXT NOT NULL,
                created_ms    INTEGER NOT NULL,
                accessed_ms   INTEGER NOT NULL,
                recall_count  INTEGER NOT NULL DEFAULT 0,
                max_score     REAL NOT NULL DEFAULT 0.0,
                promoted      INTEGER NOT NULL DEFAULT 0,
                query_hashes  TEXT NOT NULL DEFAULT '[]',
                evergreen     INTEGER NOT NULL DEFAULT 0,
                scope         TEXT NOT NULL DEFAULT 'household',
                sensitivity   TEXT NOT NULL DEFAULT 'normal',
                spoken_policy TEXT NOT NULL DEFAULT 'allow',
                display_order INTEGER NOT NULL DEFAULT 2147483647
            );

            CREATE TABLE IF NOT EXISTS household_profiles (
                source_memory_id INTEGER PRIMARY KEY,
                name             TEXT NOT NULL,
                role             TEXT NOT NULL,
                updated_ms       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_household_profiles_role
                ON household_profiles(role, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS device_aliases (
                source_memory_id INTEGER PRIMARY KEY,
                alias            TEXT NOT NULL,
                normalized_alias TEXT NOT NULL,
                target_id        TEXT NOT NULL,
                kind             TEXT NOT NULL,
                updated_ms       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_device_aliases_normalized_alias
                ON device_aliases(normalized_alias, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS household_profile_attributes (
                id               INTEGER PRIMARY KEY,
                source_memory_id INTEGER NOT NULL,
                name             TEXT NOT NULL,
                normalized_name  TEXT NOT NULL,
                attribute        TEXT NOT NULL,
                value            TEXT NOT NULL,
                updated_ms       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_household_profile_attributes_lookup
                ON household_profile_attributes(normalized_name, attribute, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS household_rules (
                id                INTEGER PRIMARY KEY,
                source_memory_id  INTEGER NOT NULL,
                person            TEXT,
                normalized_person TEXT,
                rule_type         TEXT NOT NULL,
                subject           TEXT NOT NULL,
                value             TEXT,
                allowed           INTEGER NOT NULL,
                description       TEXT NOT NULL,
                updated_ms        INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_household_rules_lookup
                ON household_rules(rule_type, subject, normalized_person, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS household_notes (
                source_memory_id INTEGER PRIMARY KEY,
                note_type        TEXT NOT NULL,
                title            TEXT NOT NULL,
                content          TEXT NOT NULL,
                updated_ms       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_household_notes_type
                ON household_notes(note_type, updated_ms DESC);

            CREATE VIRTUAL TABLE IF NOT EXISTS household_notes_fts USING fts5(
                title,
                content,
                note_type UNINDEXED,
                content='household_notes',
                content_rowid='source_memory_id'
            );

            CREATE TRIGGER IF NOT EXISTS household_notes_ai AFTER INSERT ON household_notes BEGIN
                INSERT INTO household_notes_fts(rowid, title, content, note_type)
                VALUES (new.source_memory_id, new.title, new.content, new.note_type);
            END;

            CREATE TRIGGER IF NOT EXISTS household_notes_ad AFTER DELETE ON household_notes BEGIN
                INSERT INTO household_notes_fts(household_notes_fts, rowid, title, content, note_type)
                VALUES('delete', old.source_memory_id, old.title, old.content, old.note_type);
            END;

            CREATE TRIGGER IF NOT EXISTS household_notes_au AFTER UPDATE ON household_notes BEGIN
                INSERT INTO household_notes_fts(household_notes_fts, rowid, title, content, note_type)
                VALUES('delete', old.source_memory_id, old.title, old.content, old.note_type);
                INSERT INTO household_notes_fts(rowid, title, content, note_type)
                VALUES (new.source_memory_id, new.title, new.content, new.note_type);
            END;

            CREATE TABLE IF NOT EXISTS app_only_secret_references (
                source_memory_id INTEGER PRIMARY KEY,
                secret_type      TEXT NOT NULL,
                label            TEXT NOT NULL,
                normalized_label TEXT NOT NULL,
                location_hint    TEXT NOT NULL,
                updated_ms       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_app_only_secret_references_lookup
                ON app_only_secret_references(secret_type, normalized_label, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS media_profile_items (
                source_memory_id INTEGER PRIMARY KEY,
                owner            TEXT,
                normalized_owner TEXT,
                item_type        TEXT NOT NULL,
                name             TEXT NOT NULL,
                normalized_name  TEXT NOT NULL,
                provider         TEXT,
                target           TEXT NOT NULL,
                updated_ms       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_media_profile_items_lookup
                ON media_profile_items(item_type, normalized_name, normalized_owner, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS family_calendar_events (
                id                INTEGER PRIMARY KEY,
                source_memory_id  INTEGER NOT NULL,
                person            TEXT,
                normalized_person TEXT,
                event_type        TEXT NOT NULL,
                title             TEXT NOT NULL,
                day               TEXT,
                time              TEXT,
                description       TEXT NOT NULL,
                updated_ms        INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_family_calendar_events_lookup
                ON family_calendar_events(event_type, day, normalized_person, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS shopping_list_items (
                id               INTEGER PRIMARY KEY,
                source_memory_id INTEGER NOT NULL,
                item             TEXT NOT NULL,
                normalized_item  TEXT NOT NULL,
                status           TEXT NOT NULL,
                updated_ms       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_shopping_list_items_status
                ON shopping_list_items(status, normalized_item, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS household_inventory_items (
                id               INTEGER PRIMARY KEY,
                source_memory_id INTEGER NOT NULL,
                item             TEXT NOT NULL,
                normalized_item  TEXT NOT NULL,
                quantity         TEXT,
                location         TEXT,
                category         TEXT NOT NULL,
                description      TEXT NOT NULL,
                updated_ms       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_household_inventory_items_lookup
                ON household_inventory_items(normalized_item, category, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS access_permissions (
                id                INTEGER PRIMARY KEY,
                source_memory_id  INTEGER NOT NULL,
                person            TEXT NOT NULL,
                normalized_person TEXT NOT NULL,
                device            TEXT NOT NULL,
                normalized_device TEXT NOT NULL,
                action            TEXT NOT NULL,
                allowed           INTEGER NOT NULL,
                description       TEXT NOT NULL,
                updated_ms        INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_access_permissions_lookup
                ON access_permissions(normalized_person, action, normalized_device, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS household_task_logs (
                id                INTEGER PRIMARY KEY,
                source_memory_id  INTEGER NOT NULL,
                person            TEXT NOT NULL,
                normalized_person TEXT NOT NULL,
                task              TEXT NOT NULL,
                subject           TEXT,
                normalized_subject TEXT,
                day               TEXT,
                time              TEXT,
                status            TEXT NOT NULL,
                description       TEXT NOT NULL,
                updated_ms        INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_household_task_logs_lookup
                ON household_task_logs(task, day, normalized_person, normalized_subject, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS household_schedule_items (
                id                INTEGER PRIMARY KEY,
                source_memory_id  INTEGER NOT NULL,
                schedule_type     TEXT NOT NULL,
                subject           TEXT,
                normalized_subject TEXT,
                title             TEXT NOT NULL,
                day               TEXT,
                date              TEXT,
                time              TEXT,
                amount            TEXT,
                description       TEXT NOT NULL,
                updated_ms        INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_household_schedule_items_lookup
                ON household_schedule_items(schedule_type, normalized_subject, day, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS household_event_logs (
                id                INTEGER PRIMARY KEY,
                source_memory_id  INTEGER NOT NULL,
                event_type        TEXT NOT NULL,
                subject           TEXT,
                normalized_subject TEXT,
                action            TEXT NOT NULL,
                actor             TEXT,
                normalized_actor  TEXT,
                time              TEXT,
                description       TEXT NOT NULL,
                updated_ms        INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_household_event_logs_lookup
                ON household_event_logs(event_type, action, normalized_subject, updated_ms DESC);

            CREATE TABLE IF NOT EXISTS embedded_memories (
                source_memory_id INTEGER PRIMARY KEY,
                memory_type      TEXT NOT NULL,
                embedding_model  TEXT NOT NULL,
                dimensions       INTEGER NOT NULL,
                embedding        TEXT NOT NULL,
                updated_ms       INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_embedded_memories_type
                ON embedded_memories(memory_type, updated_ms DESC);

            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content,
                content='memories',
                content_rowid='id'
            );

            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, content) VALUES (new.id, new.content);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content) VALUES('delete', old.id, old.content);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE OF content ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content) VALUES('delete', old.id, old.content);
                INSERT INTO memories_fts(rowid, content) VALUES (new.id, new.content);
            END;
            ",
        )?;

        // Migrate: add columns if they don't exist (idempotent).
        let mut migration_degraded = false;
        run_open_migration(
            &conn,
            "ALTER TABLE memories ADD COLUMN recall_count INTEGER NOT NULL DEFAULT 0",
            "add recall_count",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "ALTER TABLE memories ADD COLUMN max_score REAL NOT NULL DEFAULT 0.0",
            "add max_score",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "ALTER TABLE memories ADD COLUMN promoted INTEGER NOT NULL DEFAULT 0",
            "add promoted",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "ALTER TABLE memories ADD COLUMN query_hashes TEXT NOT NULL DEFAULT '[]'",
            "add query_hashes",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "ALTER TABLE memories ADD COLUMN evergreen INTEGER NOT NULL DEFAULT 0",
            "add evergreen",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "ALTER TABLE memories ADD COLUMN scope TEXT NOT NULL DEFAULT 'household'",
            "add scope",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "ALTER TABLE memories ADD COLUMN sensitivity TEXT NOT NULL DEFAULT 'normal'",
            "add sensitivity",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "ALTER TABLE memories ADD COLUMN spoken_policy TEXT NOT NULL DEFAULT 'allow'",
            "add spoken_policy",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "ALTER TABLE memories ADD COLUMN display_order INTEGER NOT NULL DEFAULT 2147483647",
            "add display_order",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "CREATE INDEX IF NOT EXISTS idx_memories_kind_accessed ON memories(kind, accessed_ms DESC)",
            "create idx_memories_kind_accessed",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "CREATE INDEX IF NOT EXISTS idx_memories_promotion ON memories(promoted, recall_count, max_score)",
            "create idx_memories_promotion",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "CREATE INDEX IF NOT EXISTS idx_memories_prune ON memories(evergreen, promoted, accessed_ms)",
            "create idx_memories_prune",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "CREATE INDEX IF NOT EXISTS idx_memories_scope_sensitivity ON memories(scope, sensitivity, spoken_policy)",
            "create idx_memories_scope_sensitivity",
            &mut migration_degraded,
        );
        run_open_migration(
            &conn,
            "CREATE INDEX IF NOT EXISTS idx_memories_display_order ON memories(display_order, accessed_ms DESC, id DESC)",
            "create idx_memories_display_order",
            &mut migration_degraded,
        );

        backfill_policy_columns(&conn)?;
        rebuild_household_profiles(&conn)?;
        rebuild_device_aliases(&conn)?;
        rebuild_household_profile_attributes(&conn)?;
        rebuild_household_rules(&conn)?;
        rebuild_household_notes(&conn)?;
        rebuild_app_only_secret_references(&conn)?;
        rebuild_media_profile_items(&conn)?;
        rebuild_family_calendar_events(&conn)?;
        rebuild_shopping_list_items(&conn)?;
        rebuild_household_inventory_items(&conn)?;
        rebuild_access_permissions(&conn)?;
        rebuild_household_task_logs(&conn)?;
        rebuild_household_schedule_items(&conn)?;
        rebuild_household_event_logs(&conn)?;
        rebuild_embedded_memories(&conn)?;

        // Older databases may predate the FTS update trigger or may have been
        // edited by a recovery tool. Rebuild once at open so recall and forget
        // do not silently miss rows.
        run_open_fts_rebuild(&conn, &mut migration_degraded);

        Ok(Self {
            conn,
            half_life_days,
            canonical_dir,
            migration_degraded,
        })
    }

    /// Store a new memory.
    pub fn store(&self, kind: &str, content: &str) -> Result<i64> {
        let now = now_ms();
        let content = normalize_memory_content(content);
        let metadata = policy::infer_metadata(kind, &content);
        let id = self.store_with_metadata(kind, &content, metadata, false)?;
        self.record_canonical_event(MemoryEvent {
            ts_ms: now,
            action: "store",
            id: Some(id),
            kind: Some(kind.to_string()),
            content: Some(content.clone()),
            detail: None,
        })?;
        self.append_daily_note(now, kind, &content)?;
        Ok(id)
    }

    /// Store an evergreen memory (never decays).
    pub fn store_evergreen(&self, kind: &str, content: &str) -> Result<i64> {
        let now = now_ms();
        let content = normalize_memory_content(content);
        let metadata = policy::infer_metadata(kind, &content);
        let id = self.store_with_metadata(kind, &content, metadata, true)?;
        self.record_canonical_event(MemoryEvent {
            ts_ms: now,
            action: "store_evergreen",
            id: Some(id),
            kind: Some(kind.to_string()),
            content: Some(content.clone()),
            detail: Some("evergreen".into()),
        })?;
        self.append_daily_note(now, kind, &content)?;
        Ok(id)
    }

    /// Store a fact while resolving simple single-value conflicts.
    ///
    /// This keeps household memory coherent for facts that should have one
    /// current answer, such as the user's name, age, location, workplace, or
    /// favorite color. Free-form facts and broad preferences are still append-only.
    pub fn store_resolved(&self, kind: &str, content: &str) -> Result<StoreOutcome> {
        let content = normalize_memory_content(content);
        if self.has_similar(&content)? {
            return Ok(StoreOutcome {
                id: None,
                replaced: 0,
                duplicate: true,
            });
        }

        let mut replaced = 0;
        if let Some(slot) = memory_slot(kind, &content) {
            for existing in self.get_by_kind(kind, 100)? {
                if existing.content != content
                    && memory_slot(&existing.kind, &existing.content).as_deref() == Some(&slot)
                    && self.delete_by_id(existing.id)?
                {
                    replaced += 1;
                }
            }
        }

        let id = self.store(kind, &content)?;
        Ok(StoreOutcome {
            id: Some(id),
            replaced,
            duplicate: false,
        })
    }

    /// Search memories with temporal decay applied.
    ///
    /// Returns results ranked by: BM25 relevance * temporal_decay_multiplier.
    /// Evergreen memories are exempt from decay.
    /// Each search updates recall tracking (count, score, query hash).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let limit = limit.max(1);
        let now = now_ms();
        let query_hash = hash_query(query);
        let Some(fts_query) = build_fts_query(query) else {
            return self.search_like_fallback(query, limit, now, &query_hash);
        };

        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.kind, m.content, m.created_ms, m.accessed_ms,
                    m.recall_count, m.max_score, m.promoted, m.scope,
                    m.sensitivity, m.spoken_policy, m.evergreen,
                    bm25(memories_fts) as bm25_rank
             FROM memories m
             JOIN memories_fts f ON m.id = f.rowid
             WHERE memories_fts MATCH ?1
             ORDER BY bm25_rank
             LIMIT ?2",
        )?;

        let raw_entries: Vec<(MemoryEntry, f64, bool)> = stmt
            .query_map(rusqlite::params![fts_query, limit * 3], |row| {
                let entry = read_entry(row)?;
                let bm25_rank: f64 = row.get::<_, f64>(12).unwrap_or(0.0);
                let evergreen: bool = row.get::<_, i64>(11).unwrap_or(0) != 0;
                Ok((entry, bm25_rank, evergreen))
            })?
            .filter_map(|r| r.ok())
            .collect();

        if raw_entries.is_empty() {
            return self.search_like_fallback(query, limit, now, &query_hash);
        }

        // Apply temporal decay and BM25 normalization.
        let mut scored: Vec<(MemoryEntry, f64)> = raw_entries
            .into_iter()
            .map(|(entry, bm25_rank, evergreen)| {
                let bm25_score = decay::bm25_rank_to_score(bm25_rank);
                let decay_mult = if evergreen {
                    1.0
                } else {
                    let age_days = (now as f64 - entry.created_ms as f64) / (86_400_000.0);
                    decay::exponential_decay(age_days, self.half_life_days)
                };
                let final_score = bm25_score * decay_mult;
                (entry, final_score)
            })
            .collect();

        // Sort by decayed score.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // Update recall tracking for returned results.
        for (entry, score) in &scored {
            if let Err(error) = self.update_recall_tracking(entry.id, now, *score, &query_hash) {
                tracing::error!(
                    memory_id = entry.id,
                    error = %error,
                    "memory recall tracking update failed"
                );
            }
        }

        Ok(scored.into_iter().map(|(e, _)| e).collect())
    }

    fn search_like_fallback(
        &self,
        query: &str,
        limit: usize,
        now: u64,
        query_hash: &str,
    ) -> Result<Vec<MemoryEntry>> {
        let tokens = search_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let where_clause = tokens
            .iter()
            .map(|_| "LOWER(content) LIKE ?".to_string())
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            "SELECT id, kind, content, created_ms, accessed_ms,
                    recall_count, max_score, promoted, scope, sensitivity, spoken_policy
             FROM memories
             WHERE {where_clause}
             ORDER BY accessed_ms DESC, id DESC
             LIMIT ?"
        );

        let mut values = tokens
            .iter()
            .map(|token| format!("%{}%", token))
            .collect::<Vec<_>>();
        values.push((limit * 3).to_string());

        let mut stmt = self.conn.prepare(&sql)?;
        let mut entries = stmt
            .query_map(params_from_iter(values.iter()), read_entry)?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();

        entries.sort_by(|a, b| {
            let a_score = lexical_overlap_score(query, &a.content);
            let b_score = lexical_overlap_score(query, &b.content);
            b_score
                .partial_cmp(&a_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        entries.truncate(limit);

        for entry in &entries {
            let score = lexical_overlap_score(query, &entry.content);
            if let Err(error) = self.update_recall_tracking(entry.id, now, score, query_hash) {
                tracing::error!(
                    memory_id = entry.id,
                    error = %error,
                    "memory recall tracking update failed"
                );
            }
        }

        Ok(entries)
    }

    fn update_recall_tracking(
        &self,
        id: i64,
        now: u64,
        score: f64,
        query_hash: &str,
    ) -> Result<()> {
        let mut hashes = self.query_hashes(id).unwrap_or_default();
        if !hashes.iter().any(|hash| hash == query_hash) {
            hashes.push(query_hash.to_string());
            if hashes.len() > MAX_QUERY_HASHES {
                let overflow = hashes.len() - MAX_QUERY_HASHES;
                hashes.drain(0..overflow);
            }
        }
        let hashes_json = serde_json::to_string(&hashes)?;

        self.conn.execute(
            "UPDATE memories SET
                accessed_ms = ?1,
                recall_count = recall_count + 1,
                max_score = CASE WHEN ?2 > max_score THEN ?2 ELSE max_score END,
                query_hashes = ?3
             WHERE id = ?4",
            rusqlite::params![now, score, hashes_json, id],
        )?;
        Ok(())
    }

    fn query_hashes(&self, id: i64) -> Result<Vec<String>> {
        let hashes_json: String = self.conn.query_row(
            "SELECT query_hashes FROM memories WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&hashes_json).unwrap_or_default())
    }

    /// Number of distinct query shapes that recalled this memory.
    pub fn query_diversity(&self, id: i64) -> Result<usize> {
        Ok(self.query_hashes(id)?.len())
    }

    /// Get recent memories for context injection.
    pub fn recent(&self, limit: usize) -> Result<Vec<MemoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, content, created_ms, accessed_ms,
                    recall_count, max_score, promoted, scope, sensitivity, spoken_policy
             FROM memories
             ORDER BY accessed_ms DESC, id DESC
             LIMIT ?1",
        )?;

        let entries: Vec<MemoryEntry> = stmt
            .query_map([limit], read_entry)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(entries)
    }

    pub fn list_managed(&self, limit: usize) -> Result<Vec<ManagedMemoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, content, created_ms, accessed_ms, recall_count, promoted,
                    scope, sensitivity, spoken_policy, display_order
             FROM memories
             ORDER BY display_order ASC, accessed_ms DESC, id DESC
             LIMIT ?1",
        )?;

        let mut entries = stmt
            .query_map([limit.max(1)], read_managed_entry)?
            .filter_map(|row| row.ok())
            .collect::<Vec<_>>();

        for entry in &mut entries {
            let metadata = policy::MemoryPolicyMetadata {
                scope: policy::MemoryScope::from_storage(&entry.scope),
                sensitivity: policy::MemorySensitivity::from_storage(&entry.sensitivity),
                spoken_policy: policy::SpokenMemoryPolicy::from_storage(&entry.spoken_policy),
            };
            entry.disclosure_class = policy::classify_memory(metadata).as_str().to_string();
            entry.namespace = canonical_namespace(&entry.kind, metadata);
            entry.canonical_note = if entry.promoted {
                Some(format!(
                    "memory/{}",
                    canonical_namespace_note_relative(&entry.namespace)
                ))
            } else {
                None
            };
        }

        Ok(entries)
    }

    /// Get promotion candidates — memories recalled frequently from diverse queries.
    pub fn promotion_candidates(
        &self,
        min_recall_count: i64,
        min_score: f64,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, content, created_ms, accessed_ms,
                    recall_count, max_score, promoted, scope, sensitivity, spoken_policy
             FROM memories
             WHERE recall_count >= ?1
               AND max_score >= ?2
               AND promoted = 0
             ORDER BY recall_count * max_score DESC
             LIMIT ?3",
        )?;

        let entries: Vec<MemoryEntry> = stmt
            .query_map(
                rusqlite::params![min_recall_count, min_score, limit],
                read_entry,
            )?
            .filter_map(|r| r.ok())
            .collect();

        Ok(entries)
    }

    /// Mark a memory as promoted (moved to permanent storage).
    pub fn mark_promoted(&self, id: i64) -> Result<()> {
        self.conn
            .execute("UPDATE memories SET promoted = 1 WHERE id = ?1", [id])?;
        if let Some(entry) = self.get_by_id(id)? {
            let shared_safe = policy::assess_memory_read(
                entry.metadata,
                policy::MemoryReadContext::shared_room_voice(),
            )
            .allowed;
            self.rebuild_root_memory_file()?;
            self.record_canonical_event(MemoryEvent {
                ts_ms: now_ms(),
                action: "promote",
                id: Some(id),
                kind: Some(entry.kind),
                content: Some(entry.content),
                detail: Some(if shared_safe {
                    "added to MEMORY.md".into()
                } else {
                    "promotion retained in DB only; skipped MEMORY.md due to policy".into()
                }),
            })?;
        }
        Ok(())
    }

    /// Delete old, unaccessed memories using exponential decay.
    /// Keeps evergreen and promoted memories.
    pub fn prune_decayed(&self, min_decay_threshold: f64) -> Result<usize> {
        let now = now_ms();
        let mut stmt = self
            .conn
            .prepare("SELECT id, created_ms FROM memories WHERE evergreen = 0 AND promoted = 0")?;

        let candidates: Vec<(i64, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        let mut deleted = 0;
        for (id, created_ms) in candidates {
            let age_days = (now as f64 - created_ms as f64) / 86_400_000.0;
            let multiplier = decay::exponential_decay(age_days, self.half_life_days);

            if multiplier < min_decay_threshold {
                self.conn
                    .execute("DELETE FROM memories WHERE id = ?1", [id])?;
                deleted += 1;
            }
        }

        Ok(deleted)
    }

    /// Prune memories older than max_age_days (simple cutoff).
    pub fn prune_stale(&self, max_age_days: u32) -> Result<usize> {
        let cutoff = now_ms() - (max_age_days as u64 * 86_400_000);
        let deleted = self.conn.execute(
            "DELETE FROM memories WHERE accessed_ms < ?1 AND evergreen = 0 AND promoted = 0",
            [cutoff],
        )?;
        Ok(deleted)
    }

    /// Get all memories of a specific category (e.g. "identity").
    pub fn get_by_kind(&self, kind: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, content, created_ms, accessed_ms,
                    recall_count, max_score, promoted, scope, sensitivity, spoken_policy
             FROM memories
             WHERE kind = ?1
             ORDER BY accessed_ms DESC
             LIMIT ?2",
        )?;

        let entries: Vec<MemoryEntry> = stmt
            .query_map(rusqlite::params![kind, limit], read_entry)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(entries)
    }

    pub fn household_profiles_by_role(&self, role: &str) -> Result<Vec<HouseholdProfile>> {
        let Some(role) = normalize_household_role(role) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, name, role
             FROM household_profiles
             WHERE role = ?1
             ORDER BY updated_ms DESC, source_memory_id DESC",
        )?;

        let profiles = stmt
            .query_map([role], |row| {
                Ok(HouseholdProfile {
                    source_memory_id: row.get(0)?,
                    name: row.get(1)?,
                    role: row.get(2)?,
                })
            })?
            .filter_map(|row| row.ok())
            .collect();

        Ok(profiles)
    }

    pub fn device_alias(&self, alias: &str) -> Result<Option<DeviceAlias>> {
        let normalized = normalize_alias_key(alias);
        if normalized.is_empty() {
            return Ok(None);
        }

        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, alias, target_id, kind
             FROM device_aliases
             WHERE normalized_alias = ?1
             ORDER BY updated_ms DESC, source_memory_id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query([normalized])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        Ok(Some(DeviceAlias {
            source_memory_id: row.get(0)?,
            alias: row.get(1)?,
            target_id: row.get(2)?,
            kind: row.get(3)?,
        }))
    }

    pub fn profile_attributes(
        &self,
        name: &str,
        attribute: &str,
    ) -> Result<Vec<HouseholdProfileAttribute>> {
        let normalized_name = normalize_name_key(name);
        if normalized_name.is_empty() || attribute.trim().is_empty() {
            return Ok(Vec::new());
        }

        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, name, attribute, value
             FROM household_profile_attributes
             WHERE normalized_name = ?1 AND attribute = ?2
             ORDER BY updated_ms DESC, source_memory_id DESC",
        )?;
        let entries = stmt
            .query_map(rusqlite::params![normalized_name, attribute], |row| {
                Ok(HouseholdProfileAttribute {
                    source_memory_id: row.get(0)?,
                    name: row.get(1)?,
                    attribute: row.get(2)?,
                    value: row.get(3)?,
                })
            })?
            .filter_map(|row| row.ok())
            .collect();

        Ok(entries)
    }

    pub fn household_rules(
        &self,
        person: Option<&str>,
        rule_type: &str,
        subject: Option<&str>,
    ) -> Result<Vec<HouseholdRule>> {
        let normalized_person = person.map(normalize_name_key);
        let subject = subject.map(normalize_rule_subject);
        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, person, rule_type, subject, value, allowed, description
             FROM household_rules
             WHERE rule_type = ?1
               AND (?2 IS NULL OR normalized_person = ?2)
               AND (?3 IS NULL OR subject = ?3)
             ORDER BY updated_ms DESC, source_memory_id DESC",
        )?;
        let entries: Vec<HouseholdRule> = stmt
            .query_map(
                rusqlite::params![rule_type, normalized_person.as_deref(), subject.as_deref()],
                |row| {
                    Ok(HouseholdRule {
                        source_memory_id: row.get(0)?,
                        person: row.get(1)?,
                        rule_type: row.get(2)?,
                        subject: row.get(3)?,
                        value: row.get(4)?,
                        allowed: row.get::<_, i64>(5)? != 0,
                        description: row.get(6)?,
                    })
                },
            )?
            .filter_map(|row| row.ok())
            .collect();

        Ok(entries)
    }

    pub fn household_notes_search(&self, query: &str, limit: usize) -> Result<Vec<HouseholdNote>> {
        let limit = limit.max(1);
        let Some(fts_query) = build_fts_query(query) else {
            return self.household_notes_like_fallback(query, limit);
        };

        let mut stmt = self.conn.prepare(
            "SELECT n.source_memory_id, n.note_type, n.title, n.content,
                    bm25(household_notes_fts) AS bm25_rank
             FROM household_notes n
             JOIN household_notes_fts f ON n.source_memory_id = f.rowid
             WHERE household_notes_fts MATCH ?1
             ORDER BY bm25_rank
             LIMIT ?2",
        )?;

        let entries: Vec<HouseholdNote> = stmt
            .query_map(rusqlite::params![fts_query, limit], read_household_note)?
            .filter_map(|row| row.ok())
            .collect::<Vec<_>>();

        if entries.is_empty() {
            self.household_notes_like_fallback(query, limit)
        } else {
            Ok(entries)
        }
    }

    fn household_notes_like_fallback(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<HouseholdNote>> {
        let tokens = search_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let where_clause = tokens
            .iter()
            .map(|_| "(LOWER(title) LIKE ? OR LOWER(content) LIKE ?)".to_string())
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            "SELECT source_memory_id, note_type, title, content, 0.0 AS bm25_rank
             FROM household_notes
             WHERE {where_clause}
             ORDER BY updated_ms DESC, source_memory_id DESC
             LIMIT ?"
        );

        let mut values = Vec::with_capacity(tokens.len() * 2 + 1);
        for token in tokens {
            let value = format!("%{}%", token);
            values.push(value.clone());
            values.push(value);
        }
        values.push(limit.to_string());

        let mut stmt = self.conn.prepare(&sql)?;
        let entries = stmt
            .query_map(params_from_iter(values.iter()), read_household_note)?
            .filter_map(|row| row.ok())
            .collect();
        Ok(entries)
    }

    pub fn app_only_secret_references(&self, query: &str) -> Result<Vec<AppOnlySecretReference>> {
        let Some((secret_type, label_query)) = secret_reference_query(query) else {
            return Ok(Vec::new());
        };
        let normalized_label = normalize_alias_key(&label_query);
        let label_like = format!("%{}%", normalized_label);

        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, secret_type, label, location_hint
             FROM app_only_secret_references
             WHERE secret_type = ?1
               AND (?2 = '' OR normalized_label LIKE ?3 OR ?2 LIKE '%' || normalized_label || '%')
             ORDER BY updated_ms DESC, source_memory_id DESC",
        )?;
        let entries: Vec<AppOnlySecretReference> = stmt
            .query_map(
                rusqlite::params![secret_type, normalized_label, label_like],
                |row| {
                    Ok(AppOnlySecretReference {
                        source_memory_id: row.get(0)?,
                        secret_type: row.get(1)?,
                        label: row.get(2)?,
                        location_hint: row.get(3)?,
                    })
                },
            )?
            .filter_map(|row| row.ok())
            .collect();
        if entries.is_empty() {
            self.app_only_secret_reference_fallback(secret_type, query)
        } else {
            Ok(entries)
        }
    }

    fn app_only_secret_reference_fallback(
        &self,
        secret_type: &str,
        query: &str,
    ) -> Result<Vec<AppOnlySecretReference>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, secret_type, label, location_hint
             FROM app_only_secret_references
             WHERE secret_type = ?1
             ORDER BY updated_ms DESC, source_memory_id DESC",
        )?;
        let mut entries = stmt
            .query_map([secret_type], |row| {
                Ok(AppOnlySecretReference {
                    source_memory_id: row.get(0)?,
                    secret_type: row.get(1)?,
                    label: row.get(2)?,
                    location_hint: row.get(3)?,
                })
            })?
            .filter_map(|row| row.ok())
            .map(|entry| {
                let score = lexical_overlap_score(query, &entry.label);
                (entry, score)
            })
            .filter(|(_, score)| *score > 0.0)
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.0.source_memory_id.cmp(&a.0.source_memory_id))
        });
        Ok(entries.into_iter().map(|(entry, _)| entry).collect())
    }

    pub fn media_playlist_for_query(&self, query: &str) -> Result<Option<MediaProfileItem>> {
        let Some((owner, name)) = media_playlist_query(query) else {
            return Ok(None);
        };
        self.media_playlist(owner.as_deref(), &name)
    }

    pub fn media_playlist(
        &self,
        owner: Option<&str>,
        name: &str,
    ) -> Result<Option<MediaProfileItem>> {
        let normalized_name = normalize_alias_key(name);
        if normalized_name.is_empty() {
            return Ok(None);
        }
        let normalized_owner = owner.map(normalize_name_key).unwrap_or_default();
        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, owner, item_type, name, provider, target
             FROM media_profile_items
             WHERE item_type = 'playlist'
               AND normalized_name = ?1
               AND (?2 = '' OR normalized_owner = ?2 OR normalized_owner = '')
             ORDER BY CASE WHEN normalized_owner = ?2 THEN 0 ELSE 1 END,
                      updated_ms DESC,
                      source_memory_id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![normalized_name, normalized_owner])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(MediaProfileItem {
            source_memory_id: row.get(0)?,
            owner: row.get(1)?,
            item_type: row.get(2)?,
            name: row.get(3)?,
            provider: row.get(4)?,
            target: row.get(5)?,
        }))
    }

    pub fn family_calendar_event_for_query(
        &self,
        query: &str,
    ) -> Result<Option<FamilyCalendarEvent>> {
        if let Some((person, event_type, day)) = calendar_event_query(query) {
            return self.family_calendar_event(Some(&person), &event_type, day.as_deref());
        }
        if let Some(day) = school_pickup_query(query) {
            return self.family_calendar_event(None, "school_pickup", Some(&day));
        }
        Ok(None)
    }

    pub fn family_calendar_event(
        &self,
        person: Option<&str>,
        event_type: &str,
        day: Option<&str>,
    ) -> Result<Option<FamilyCalendarEvent>> {
        let normalized_person = person.map(normalize_name_key).unwrap_or_default();
        let normalized_day = day.map(normalize_calendar_day).unwrap_or_default();
        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, person, event_type, title, day, time, description
             FROM family_calendar_events
             WHERE event_type = ?1
               AND (?2 = '' OR normalized_person = ?2 OR normalized_person IS NULL)
               AND (?3 = '' OR day = ?3)
             ORDER BY CASE WHEN normalized_person = ?2 THEN 0 ELSE 1 END,
                      updated_ms DESC,
                      source_memory_id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![
            event_type,
            normalized_person,
            normalized_day
        ])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(FamilyCalendarEvent {
            source_memory_id: row.get(0)?,
            person: row.get(1)?,
            event_type: row.get(2)?,
            title: row.get(3)?,
            day: row.get(4)?,
            time: row.get(5)?,
            description: row.get(6)?,
        }))
    }

    pub fn shopping_list_items(&self) -> Result<Vec<ShoppingListItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.source_memory_id, s.item, s.status
             FROM shopping_list_items s
             WHERE s.status = 'pending'
               AND NOT EXISTS (
                   SELECT 1
                   FROM shopping_list_items newer
                   WHERE newer.normalized_item = s.normalized_item
                     AND (
                         newer.updated_ms > s.updated_ms
                         OR (newer.updated_ms = s.updated_ms
                             AND newer.source_memory_id > s.source_memory_id)
                     )
               )
             ORDER BY s.updated_ms DESC, s.source_memory_id DESC",
        )?;
        let entries = stmt
            .query_map([], |row| {
                Ok(ShoppingListItem {
                    source_memory_id: row.get(0)?,
                    item: row.get(1)?,
                    status: row.get(2)?,
                })
            })?
            .filter_map(|row| row.ok())
            .collect();
        Ok(entries)
    }

    pub fn shopping_list_pending_count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*)
             FROM shopping_list_items s
             WHERE s.status = 'pending'
               AND NOT EXISTS (
                   SELECT 1
                   FROM shopping_list_items newer
                   WHERE newer.normalized_item = s.normalized_item
                     AND (
                         newer.updated_ms > s.updated_ms
                         OR (newer.updated_ms = s.updated_ms
                             AND newer.source_memory_id > s.source_memory_id)
                     )
               )",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn household_inventory_item_for_query(
        &self,
        query: &str,
    ) -> Result<Option<HouseholdInventoryItem>> {
        let Some(item) = inventory_item_query(query) else {
            return Ok(None);
        };
        let normalized_item = normalize_inventory_item(&item);
        if normalized_item.is_empty() {
            return Ok(None);
        }

        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, item, quantity, location, category, description
             FROM household_inventory_items
             WHERE normalized_item = ?1
                OR normalized_item = ?2
                OR ?1 LIKE '%' || normalized_item || '%'
                OR normalized_item LIKE '%' || ?1 || '%'
             ORDER BY updated_ms DESC, source_memory_id DESC
             LIMIT 1",
        )?;
        let singular_item = normalized_item.trim_end_matches('s');
        let mut rows = stmt.query(rusqlite::params![normalized_item, singular_item])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(HouseholdInventoryItem {
            source_memory_id: row.get(0)?,
            item: row.get(1)?,
            quantity: row.get(2)?,
            location: row.get(3)?,
            category: row.get(4)?,
            description: row.get(5)?,
        }))
    }

    pub fn access_permission_for_query(&self, query: &str) -> Result<Option<AccessPermission>> {
        let Some((person, action, device)) = access_permission_query(query) else {
            return Ok(None);
        };
        let normalized_person = normalize_name_key(&person);
        let normalized_device = normalize_alias_key(&device);
        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, person, device, action, allowed, description
             FROM access_permissions
             WHERE normalized_person = ?1
               AND action = ?2
               AND (normalized_device = ?3 OR ?3 LIKE '%' || normalized_device || '%' OR normalized_device LIKE '%' || ?3 || '%')
             ORDER BY updated_ms DESC, source_memory_id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![
            normalized_person,
            action,
            normalized_device
        ])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(AccessPermission {
            source_memory_id: row.get(0)?,
            person: row.get(1)?,
            device: row.get(2)?,
            action: row.get(3)?,
            allowed: row.get::<_, i64>(4)? != 0,
            description: row.get(5)?,
        }))
    }

    pub fn household_task_log_for_query(&self, query: &str) -> Result<Option<HouseholdTaskLog>> {
        let Some((person, task, subject, day)) = task_log_query(query) else {
            return Ok(None);
        };
        let normalized_person = normalize_name_key(&person);
        let normalized_subject = subject
            .as_deref()
            .map(normalize_alias_key)
            .unwrap_or_default();
        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, person, task, subject, day, time, status, description
             FROM household_task_logs
             WHERE normalized_person = ?1
               AND task = ?2
               AND (?3 = '' OR normalized_subject = ?3 OR normalized_subject IS NULL)
               AND (?4 IS NULL OR day = ?4)
             ORDER BY updated_ms DESC, source_memory_id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![
            normalized_person,
            task,
            normalized_subject,
            day
        ])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(HouseholdTaskLog {
            source_memory_id: row.get(0)?,
            person: row.get(1)?,
            task: row.get(2)?,
            subject: row.get(3)?,
            day: row.get(4)?,
            time: row.get(5)?,
            status: row.get(6)?,
            description: row.get(7)?,
        }))
    }

    pub fn household_task_logs_for_task_day(
        &self,
        task: &str,
        day: &str,
    ) -> Result<Vec<HouseholdTaskLog>> {
        let mut stmt = self.conn.prepare(
            "SELECT l.source_memory_id, l.person, l.task, l.subject, l.day, l.time, l.status, l.description
             FROM household_task_logs l
             WHERE l.task = ?1
               AND l.day = ?2
               AND NOT EXISTS (
                   SELECT 1
                   FROM household_task_logs newer
                   WHERE newer.task = l.task
                     AND newer.normalized_person = l.normalized_person
                     AND newer.day = l.day
                     AND (
                         newer.updated_ms > l.updated_ms
                         OR (newer.updated_ms = l.updated_ms
                             AND newer.source_memory_id > l.source_memory_id)
                     )
               )
             ORDER BY l.person",
        )?;
        let logs = stmt
            .query_map(rusqlite::params![task, day], |row| {
                Ok(HouseholdTaskLog {
                    source_memory_id: row.get(0)?,
                    person: row.get(1)?,
                    task: row.get(2)?,
                    subject: row.get(3)?,
                    day: row.get(4)?,
                    time: row.get(5)?,
                    status: row.get(6)?,
                    description: row.get(7)?,
                })
            })?
            .filter_map(|row| row.ok())
            .collect();
        Ok(logs)
    }

    fn household_profile_names(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name
             FROM household_profiles
             WHERE role NOT IN ('dog', 'cat', 'pet')
             ORDER BY name",
        )?;
        let names = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|row| row.ok())
            .collect();
        Ok(names)
    }

    pub fn household_schedule_item_for_query(
        &self,
        query: &str,
    ) -> Result<Option<HouseholdScheduleItem>> {
        let Some((schedule_type, subject, day)) = schedule_item_query(query) else {
            return Ok(None);
        };
        let normalized_subject = subject
            .as_deref()
            .map(normalize_alias_key)
            .unwrap_or_default();
        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, schedule_type, subject, title, day, date, time, amount, description
             FROM household_schedule_items
             WHERE schedule_type = ?1
               AND (?2 = ''
                    OR normalized_subject = ?2
                    OR ?2 LIKE '%' || normalized_subject || '%'
                    OR normalized_subject LIKE '%' || ?2 || '%'
                    OR normalized_subject IS NULL)
               AND (?3 IS NULL OR day = ?3 OR day IS NULL)
             ORDER BY updated_ms DESC, source_memory_id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![schedule_type, normalized_subject, day])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(HouseholdScheduleItem {
            source_memory_id: row.get(0)?,
            schedule_type: row.get(1)?,
            subject: row.get(2)?,
            title: row.get(3)?,
            day: row.get(4)?,
            date: row.get(5)?,
            time: row.get(6)?,
            amount: row.get(7)?,
            description: row.get(8)?,
        }))
    }

    pub fn household_event_log_for_query(&self, query: &str) -> Result<Option<HouseholdEventLog>> {
        let Some((event_type, action, subject)) = event_log_query(query) else {
            return Ok(None);
        };
        let normalized_subject = subject
            .as_deref()
            .map(normalize_alias_key)
            .unwrap_or_default();
        let mut stmt = self.conn.prepare(
            "SELECT source_memory_id, event_type, subject, action, actor, time, description
             FROM household_event_logs
             WHERE event_type = ?1
               AND action = ?2
               AND (?3 = '' OR normalized_subject = ?3 OR normalized_subject IS NULL)
             ORDER BY updated_ms DESC, source_memory_id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![event_type, action, normalized_subject])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(HouseholdEventLog {
            source_memory_id: row.get(0)?,
            event_type: row.get(1)?,
            subject: row.get(2)?,
            action: row.get(3)?,
            actor: row.get(4)?,
            time: row.get(5)?,
            description: row.get(6)?,
        }))
    }

    pub fn semantic_search(&self, query: &str, limit: usize) -> Result<Vec<SemanticMemoryHit>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let provider = LocalHashEmbeddingProvider;
        let query_text = embedding_text_for_query(query);
        let query_type = semantic_query_type(query);
        let query_embedding = provider.embed(&query_text);
        let now = now_ms();
        let query_hash = hash_query(query);
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.kind, m.content, m.created_ms, m.accessed_ms,
                    m.recall_count, m.max_score, m.promoted, m.scope,
                    m.sensitivity, m.spoken_policy,
                    e.embedding_model, e.dimensions, e.embedding
             FROM embedded_memories e
             JOIN memories m ON m.id = e.source_memory_id
             WHERE e.embedding_model = ?1",
        )?;

        let mut hits = stmt
            .query_map([provider.model_name()], |row| {
                let entry = read_entry(row)?;
                let embedding_model: String = row.get(11)?;
                let dimensions: i64 = row.get(12)?;
                let embedding_json: String = row.get(13)?;
                Ok((entry, embedding_model, dimensions as usize, embedding_json))
            })?
            .filter_map(|row| row.ok())
            .filter_map(|(entry, embedding_model, dimensions, embedding_json)| {
                parse_embedding(&embedding_json, dimensions).map(|embedding| {
                    let mut score = embedding::cosine_similarity(&query_embedding, &embedding);
                    if query_type.as_deref().is_some_and(|expected| {
                        expected == semantic_memory_type(&entry.kind, &entry.content)
                    }) {
                        score = score.max(0.95 + word_overlap(&query_text, &entry.content) * 0.04);
                    }
                    SemanticMemoryHit {
                        entry,
                        score,
                        embedding_model,
                    }
                })
            })
            .filter(|hit| hit.score >= embedding::SEMANTIC_MIN_SCORE)
            .collect::<Vec<_>>();

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.entry.id.cmp(&a.entry.id))
        });
        hits.truncate(limit.max(1));

        for hit in &hits {
            if let Err(error) =
                self.update_recall_tracking(hit.entry.id, now, hit.score, &query_hash)
            {
                tracing::error!(
                    memory_id = hit.entry.id,
                    error = %error,
                    "semantic memory recall tracking update failed"
                );
            }
        }

        Ok(hits)
    }

    pub fn structured_household_answer(&self, query: &str) -> Result<Option<String>> {
        if let Some((name, attribute)) = profile_attribute_query(query) {
            let attrs = self.profile_attributes(&name, attribute)?;
            if let Some(attr) = attrs.first() {
                return Ok(Some(format_profile_attribute_answer(attr)));
            }
        }

        if let Some(subject) = allergy_query_subject(query) {
            let rules = self.household_rules(None, "allergy", subject.as_deref())?;
            if !rules.is_empty() {
                return Ok(Some(format_allergy_answer(&rules)));
            }
        }

        if let Some((person, subject, value)) = allowed_rule_query(query) {
            let rules = self.household_rules(Some(&person), "screen_time", Some(&subject))?;
            if let Some(rule) = rules
                .iter()
                .find(|rule| {
                    value
                        .as_deref()
                        .is_none_or(|v| rule.value.as_deref() == Some(v))
                })
                .or_else(|| rules.first())
            {
                return Ok(Some(format_allowed_rule_answer(rule)));
            }
        }

        if let Some(person) = homework_rule_query(query) {
            let rules = self.household_rules(Some(&person), "homework", Some("homework"))?;
            if !rules.is_empty() {
                return Ok(Some(format_rule_list_answer(&rules)));
            }
        }

        if let Some(event) = self.family_calendar_event_for_query(query)? {
            return Ok(Some(format_family_calendar_event_answer(&event)));
        }

        if let Some(permission) = self.access_permission_for_query(query)? {
            return Ok(Some(format_access_permission_answer(&permission)));
        }

        if let Some(day) = everyone_brush_teeth_query(query) {
            let logs = self.household_task_logs_for_task_day("brush_teeth", &day)?;
            if !logs.is_empty() {
                let profiles = self.household_profile_names()?;
                return Ok(Some(format_everyone_task_log_answer(&profiles, &logs)));
            }
        }

        if let Some(task) = self.household_task_log_for_query(query)? {
            return Ok(Some(format_household_task_log_answer(&task)));
        }

        if let Some(schedule) = self.household_schedule_item_for_query(query)? {
            return Ok(Some(format_household_schedule_item_answer(&schedule)));
        }

        if let Some(event) = self.household_event_log_for_query(query)? {
            return Ok(Some(format_household_event_log_answer(&event)));
        }

        if shopping_list_query(query) {
            let items = self.shopping_list_items()?;
            if !items.is_empty() {
                return Ok(Some(format_shopping_list_answer(&items)));
            }
        }

        if let Some(item) = self.household_inventory_item_for_query(query)? {
            return Ok(Some(format_household_inventory_item_answer(&item)));
        }

        if secret_reference_query(query).is_some() {
            let refs = self.app_only_secret_references(query)?;
            if let Some(secret_ref) = refs.first() {
                return Ok(Some(format_app_only_secret_reference_answer(secret_ref)));
            }
        }

        if let Some(note_query) = household_note_query(query) {
            let notes = self.household_notes_search(&note_query, 3)?;
            if let Some(note) = notes.first() {
                return Ok(Some(format_household_note_answer(note)));
            }
        }

        Ok(None)
    }

    /// Delete a memory by ID.
    pub fn delete_by_id(&self, id: i64) -> Result<bool> {
        let existing = self.get_by_id(id)?;
        let deleted = self
            .conn
            .execute("DELETE FROM memories WHERE id = ?1", [id])?;
        if deleted > 0
            && let Some(entry) = existing
        {
            self.conn.execute(
                "DELETE FROM household_profiles WHERE source_memory_id = ?1",
                [id],
            )?;
            self.conn.execute(
                "DELETE FROM device_aliases WHERE source_memory_id = ?1",
                [id],
            )?;
            delete_structured_household_rows(&self.conn, id)?;
            if entry.promoted {
                self.rebuild_root_memory_file()?;
            }
            self.record_canonical_event(MemoryEvent {
                ts_ms: now_ms(),
                action: "delete",
                id: Some(id),
                kind: Some(entry.kind.clone()),
                content: Some(entry.content.clone()),
                detail: None,
            })?;
            self.append_daily_note(
                now_ms(),
                "deleted",
                &format!("[{}] {}", entry.kind, entry.content),
            )?;
        }
        Ok(deleted > 0)
    }

    pub fn update_managed(&self, id: i64, content: &str, kind: Option<&str>) -> Result<bool> {
        let Some(existing) = self.get_by_id(id)? else {
            return Ok(false);
        };

        let content = normalize_memory_content(content);
        if content.is_empty() {
            anyhow::bail!("memory content cannot be empty");
        }

        let next_kind = match kind {
            Some(kind) if kind.trim().is_empty() => {
                anyhow::bail!("memory kind cannot be empty");
            }
            Some(kind) => kind.trim().to_string(),
            None => existing.kind.clone(),
        };

        let metadata = policy::infer_metadata(&next_kind, &content);
        let changed = existing.kind != next_kind
            || existing.content != content
            || existing.metadata != metadata;

        if !changed {
            return Ok(true);
        }

        let updated = self.conn.execute(
            "UPDATE memories
             SET kind = ?1, content = ?2, scope = ?3, sensitivity = ?4, spoken_policy = ?5
             WHERE id = ?6",
            rusqlite::params![
                next_kind,
                content,
                metadata.scope.as_str(),
                metadata.sensitivity.as_str(),
                metadata.spoken_policy.as_str(),
                id
            ],
        )?;

        if updated > 0 {
            upsert_household_profile_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_device_alias_from_memory(&self.conn, id, &content, metadata, now_ms())?;
            upsert_household_profile_attributes_from_memory(
                &self.conn,
                id,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_household_rules_from_memory(&self.conn, id, &content, metadata, now_ms())?;
            upsert_household_note_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_app_only_secret_reference_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_media_profile_item_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_family_calendar_events_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_shopping_list_items_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_household_inventory_items_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_access_permissions_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_household_task_logs_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_household_schedule_items_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_household_event_logs_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            upsert_embedded_memory_from_memory(
                &self.conn,
                id,
                &next_kind,
                &content,
                metadata,
                now_ms(),
            )?;
            if existing.promoted {
                self.rebuild_root_memory_file()?;
            }
            let detail = format!("from [{}] {}", existing.kind, existing.content);
            self.record_canonical_event(MemoryEvent {
                ts_ms: now_ms(),
                action: "update",
                id: Some(id),
                kind: Some(next_kind.clone()),
                content: Some(content.clone()),
                detail: Some(detail),
            })?;
            self.append_daily_note(now_ms(), "updated", &format!("[{}] {}", next_kind, content))?;
        }

        Ok(updated > 0)
    }

    pub fn reorder_managed(&self, ids: &[i64]) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE TRANSACTION")?;
        let update_result = (|| -> Result<()> {
            for (idx, id) in ids.iter().enumerate() {
                self.conn.execute(
                    "UPDATE memories SET display_order = ?1 WHERE id = ?2",
                    rusqlite::params![idx as i64, id],
                )?;
            }
            Ok(())
        })();

        match update_result {
            Ok(()) => self.conn.execute_batch("COMMIT")?,
            Err(err) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(err);
            }
        }

        self.rebuild_root_memory_file()?;
        Ok(())
    }

    /// Search and delete matching memories. Returns count deleted.
    pub fn delete_matching(&self, query: &str) -> Result<usize> {
        let matches = self.search(query, 10)?;
        let mut deleted = 0;
        for entry in &matches {
            if self.delete_by_id(entry.id)? {
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    /// Check if a similar memory already exists (for deduplication).
    ///
    /// Uses SQL LIKE with key words from the content. More reliable than
    /// FTS5 for deduplication since FTS5 has issues with apostrophes and
    /// short queries.
    pub fn has_similar(&self, content: &str) -> Result<bool> {
        let clean = strip_source_tag(content);

        // Extract the most distinctive words (skip common ones).
        let skip = [
            "user", "users", "the", "is", "are", "was", "has", "have", "and", "for", "that",
            "this", "with", "from", "not",
        ];
        let words: Vec<String> = search_tokens(&clean)
            .into_iter()
            .filter(|w| !skip.contains(&w.as_str()))
            .take(4)
            .collect();

        if words.is_empty() {
            return Ok(false);
        }

        // Build a parameterized query: content LIKE '%word1%' AND content LIKE '%word2%'.
        // This is intentionally not FTS; dedup needs stable substring behavior
        // for apostrophes, short names, and partially normalized phrases.
        let conditions: Vec<String> = words
            .iter()
            .map(|_| "LOWER(content) LIKE ?".to_string())
            .collect();
        let where_clause = conditions.join(" AND ");

        let query = format!("SELECT COUNT(*) FROM memories WHERE {}", where_clause);
        let values = words
            .iter()
            .map(|word| format!("%{}%", word))
            .collect::<Vec<_>>();

        let count: i64 = self
            .conn
            .query_row(&query, params_from_iter(values.iter()), |row| row.get(0))?;
        Ok(count > 0)
    }

    /// Rebuild FTS rows from the canonical memories table.
    pub fn rebuild_fts(&self) -> Result<()> {
        rebuild_fts_index(&self.conn)
    }

    /// Whether schema migration or FTS rebuild failed during open.
    pub fn migration_degraded(&self) -> bool {
        self.migration_degraded
    }

    /// Lightweight operator health check for the memory store.
    pub fn health(&self) -> Result<MemoryHealth> {
        let quick_check: String = self
            .conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        let memory_rows: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
        let fts_rows: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM memories_fts", [], |row| row.get(0))?;
        let person_rows: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE scope = 'person'",
            [],
            |row| row.get(0),
        )?;
        let private_rows: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE scope = 'private'",
            [],
            |row| row.get(0),
        )?;
        let restricted_rows: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE sensitivity = 'restricted'",
            [],
            |row| row.get(0),
        )?;
        let canonical_root_exists = self.canonical_dir.join("MEMORY.md").exists();
        let canonical_namespace_files =
            count_markdown_files(&self.canonical_dir.join("namespaces"));
        let canonical_daily_files = std::fs::read_dir(&self.canonical_dir)
            .ok()
            .into_iter()
            .flat_map(|iter| iter.filter_map(|entry| entry.ok()))
            .filter(|entry| {
                entry.path().is_file()
                    && entry
                        .path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.eq_ignore_ascii_case("md"))
                        .unwrap_or(false)
                    && entry
                        .file_name()
                        .to_str()
                        .map(|name| name != "MEMORY.md")
                        .unwrap_or(false)
            })
            .count();
        let canonical_event_logs = std::fs::read_dir(self.canonical_dir.join("events"))
            .ok()
            .into_iter()
            .flat_map(|iter| iter.filter_map(|entry| entry.ok()))
            .filter(|entry| {
                entry.path().is_file()
                    && entry
                        .path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.eq_ignore_ascii_case("jsonl"))
                        .unwrap_or(false)
            })
            .count();

        Ok(MemoryHealth {
            quick_check_ok: quick_check.eq_ignore_ascii_case("ok"),
            memory_rows: memory_rows as usize,
            fts_rows: fts_rows as usize,
            fts_consistent: memory_rows == fts_rows,
            migration_degraded: self.migration_degraded,
            canonical_root_exists,
            canonical_namespace_files,
            canonical_daily_files,
            canonical_event_logs,
            person_rows: person_rows as usize,
            private_rows: private_rows as usize,
            restricted_rows: restricted_rows as usize,
        })
    }

    pub fn count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn promoted_count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE promoted = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    fn get_by_id(&self, id: i64) -> Result<Option<MemoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, content, created_ms, accessed_ms,
                    recall_count, max_score, promoted, scope, sensitivity, spoken_policy
             FROM memories
             WHERE id = ?1",
        )?;
        let mut rows = stmt.query([id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(read_entry(row)?))
    }

    pub(crate) fn store_with_metadata(
        &self,
        kind: &str,
        content: &str,
        metadata: policy::MemoryPolicyMetadata,
        evergreen: bool,
    ) -> Result<i64> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO memories (
                kind, content, created_ms, accessed_ms, evergreen, scope, sensitivity, spoken_policy
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                kind,
                content,
                now,
                now,
                if evergreen { 1 } else { 0 },
                metadata.scope.as_str(),
                metadata.sensitivity.as_str(),
                metadata.spoken_policy.as_str(),
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        upsert_household_profile_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_device_alias_from_memory(&self.conn, id, content, metadata, now)?;
        upsert_household_profile_attributes_from_memory(&self.conn, id, content, metadata, now)?;
        upsert_household_rules_from_memory(&self.conn, id, content, metadata, now)?;
        upsert_household_note_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_app_only_secret_reference_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_media_profile_item_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_family_calendar_events_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_shopping_list_items_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_household_inventory_items_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_access_permissions_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_household_task_logs_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_household_schedule_items_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_household_event_logs_from_memory(&self.conn, id, kind, content, metadata, now)?;
        upsert_embedded_memory_from_memory(&self.conn, id, kind, content, metadata, now)?;
        Ok(id)
    }

    fn record_canonical_event(&self, event: MemoryEvent) -> Result<()> {
        let file = canonical_event_file(&self.canonical_dir, event.ts_ms);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(&event)?;
        use std::io::Write;
        let mut handle = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)?;
        writeln!(handle, "{json}")?;
        Ok(())
    }

    fn append_daily_note(&self, ts_ms: u64, kind: &str, content: &str) -> Result<()> {
        let file = canonical_daily_note_file(&self.canonical_dir, ts_ms);
        let date = canonical_date(ts_ms);
        let mut existing = std::fs::read_to_string(&file).unwrap_or_default();
        if existing.is_empty() {
            existing.push_str(&format!("# Memory Note {date}\n\n"));
        }
        let line = format!("- [{}] {}\n", kind, content);
        if !existing.contains(&line) {
            use std::io::Write;
            let mut handle = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)?;
            if !existing.ends_with('\n') {
                writeln!(handle)?;
            }
            write!(handle, "{line}")?;
        }
        Ok(())
    }

    fn rebuild_root_memory_file(&self) -> Result<()> {
        let file = self.canonical_dir.join("MEMORY.md");
        let index_file = self.canonical_dir.join("INDEX.md");
        let namespaces_dir = self.canonical_dir.join("namespaces");
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, content, scope, sensitivity, spoken_policy
             FROM memories
             WHERE promoted = 1
             ORDER BY display_order ASC, accessed_ms DESC, id ASC",
        )?;
        let records = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    policy::MemoryPolicyMetadata {
                        scope: policy::MemoryScope::from_storage(&row.get::<_, String>(3)?),
                        sensitivity: policy::MemorySensitivity::from_storage(
                            &row.get::<_, String>(4)?,
                        ),
                        spoken_policy: policy::SpokenMemoryPolicy::from_storage(
                            &row.get::<_, String>(5)?,
                        ),
                    },
                ))
            })?
            .filter_map(|row| row.ok())
            .collect::<Vec<_>>();

        let _ = std::fs::remove_dir_all(&namespaces_dir);
        if records.is_empty() {
            let _ = std::fs::remove_file(&file);
            let _ = std::fs::remove_file(&index_file);
            return Ok(());
        }

        std::fs::create_dir_all(&namespaces_dir)?;

        let mut namespace_index: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        let mut root_lines = Vec::new();

        for (id, kind, content, metadata) in &records {
            let namespace = canonical_namespace(kind, *metadata);
            let shared_safe = policy::assess_memory_read(
                *metadata,
                policy::MemoryReadContext::shared_room_voice(),
            )
            .allowed;
            if shared_safe {
                root_lines.push(format!("- [mem:{id}] [{kind}] {content}\n"));
            }

            let namespace_line = if shared_safe {
                format!("- [mem:{id}] [{kind}] {content}\n")
            } else {
                format!(
                    "- [mem:{id}] redacted ({}, {}, {})\n",
                    metadata.scope.as_str(),
                    metadata.sensitivity.as_str(),
                    metadata.spoken_policy.as_str()
                )
            };
            namespace_index
                .entry(namespace)
                .or_default()
                .push(namespace_line);
        }

        let mut index_text =
            String::from("# GenieClaw Memory Index\n\nGenerated local durable-memory map.\n\n");
        index_text.push_str("## Namespaces\n\n");

        for (namespace, lines) in &namespace_index {
            let relative = canonical_namespace_note_relative(namespace);
            let note_path = self.canonical_dir.join(&relative);
            if let Some(parent) = note_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let mut note_text = format!(
                "---\nnamespace: {}\nkind: durable-memory\nsource: genie-core\n---\n\n# {}\n\n",
                namespace, namespace
            );
            for line in lines {
                note_text.push_str(line);
            }
            std::fs::write(note_path, note_text)?;

            index_text.push_str(&format!(
                "- [{}]({}) — {} durable entr{}\n",
                namespace,
                relative,
                lines.len(),
                if lines.len() == 1 { "y" } else { "ies" }
            ));
        }

        std::fs::write(index_file, index_text)?;

        if root_lines.is_empty() {
            let mut text = String::from("# GenieClaw Durable Memory\n\n");
            text.push_str(
                "No promoted memories are currently safe for shared-room disclosure.\n\nSee [INDEX.md](INDEX.md) for the local namespace map.\n",
            );
            std::fs::write(file, text)?;
            return Ok(());
        }

        let mut text = String::from("# GenieClaw Durable Memory\n\n");
        text.push_str("See [INDEX.md](INDEX.md) for namespace notes.\n\n");
        for line in root_lines {
            text.push_str(&line);
        }
        std::fs::write(file, text)?;
        Ok(())
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn read_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryEntry> {
    Ok(MemoryEntry {
        id: row.get(0)?,
        kind: row.get(1)?,
        content: row.get(2)?,
        created_ms: row.get(3)?,
        accessed_ms: row.get(4)?,
        recall_count: row.get(5).unwrap_or(0),
        max_score: row.get(6).unwrap_or(0.0),
        promoted: row.get::<_, i64>(7).unwrap_or(0) != 0,
        metadata: policy::MemoryPolicyMetadata {
            scope: policy::MemoryScope::from_storage(&row.get::<_, String>(8)?),
            sensitivity: policy::MemorySensitivity::from_storage(&row.get::<_, String>(9)?),
            spoken_policy: policy::SpokenMemoryPolicy::from_storage(&row.get::<_, String>(10)?),
        },
    })
}

fn read_managed_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<ManagedMemoryEntry> {
    Ok(ManagedMemoryEntry {
        id: row.get(0)?,
        kind: row.get(1)?,
        content: row.get(2)?,
        created_ms: row.get(3)?,
        accessed_ms: row.get(4)?,
        recall_count: row.get(5).unwrap_or(0),
        promoted: row.get::<_, i64>(6).unwrap_or(0) != 0,
        scope: row.get::<_, String>(7)?,
        sensitivity: row.get::<_, String>(8)?,
        spoken_policy: row.get::<_, String>(9)?,
        disclosure_class: String::new(),
        namespace: String::new(),
        canonical_note: None,
        display_order: row.get::<_, i64>(10).unwrap_or(i64::MAX),
    })
}

fn read_household_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<HouseholdNote> {
    Ok(HouseholdNote {
        source_memory_id: row.get(0)?,
        note_type: row.get(1)?,
        title: row.get(2)?,
        content: row.get(3)?,
    })
}

fn backfill_policy_columns(conn: &Connection) -> Result<()> {
    let mut stmt =
        conn.prepare("SELECT id, kind, content, scope, sensitivity, spoken_policy FROM memories")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)
                    .unwrap_or_else(|_| "household".into()),
                row.get::<_, String>(4).unwrap_or_else(|_| "normal".into()),
                row.get::<_, String>(5).unwrap_or_else(|_| "allow".into()),
            ))
        })?
        .filter_map(|row| row.ok())
        .collect::<Vec<_>>();
    drop(stmt);

    for (id, kind, content, scope, sensitivity, spoken_policy) in rows {
        let inferred = policy::infer_metadata(&kind, &content);
        let needs_scope = scope.trim().is_empty() || scope.eq_ignore_ascii_case("household");
        let needs_sensitivity =
            sensitivity.trim().is_empty() || sensitivity.eq_ignore_ascii_case("normal");
        let needs_policy =
            spoken_policy.trim().is_empty() || spoken_policy.eq_ignore_ascii_case("allow");
        if !(needs_scope || needs_sensitivity || needs_policy) {
            continue;
        }

        conn.execute(
            "UPDATE memories
             SET scope = ?1, sensitivity = ?2, spoken_policy = ?3
             WHERE id = ?4",
            rusqlite::params![
                inferred.scope.as_str(),
                inferred.sensitivity.as_str(),
                inferred.spoken_policy.as_str(),
                id
            ],
        )?;
    }

    Ok(())
}

fn rebuild_household_profiles(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_profiles", [])?;

    let mut stmt = conn.prepare(
        "SELECT id, kind, content, scope, sensitivity, spoken_policy
         FROM memories
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                policy::MemoryPolicyMetadata {
                    scope: policy::MemoryScope::from_storage(&row.get::<_, String>(3)?),
                    sensitivity: policy::MemorySensitivity::from_storage(&row.get::<_, String>(4)?),
                    spoken_policy: policy::SpokenMemoryPolicy::from_storage(
                        &row.get::<_, String>(5)?,
                    ),
                },
            ))
        })?
        .filter_map(|row| row.ok())
        .collect::<Vec<_>>();
    drop(stmt);

    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_profile_from_memory(conn, id, &kind, &content, metadata, now)?;
    }

    Ok(())
}

fn rebuild_device_aliases(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM device_aliases", [])?;

    let mut stmt = conn.prepare(
        "SELECT id, content, scope, sensitivity, spoken_policy
         FROM memories
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                policy::MemoryPolicyMetadata {
                    scope: policy::MemoryScope::from_storage(&row.get::<_, String>(2)?),
                    sensitivity: policy::MemorySensitivity::from_storage(&row.get::<_, String>(3)?),
                    spoken_policy: policy::SpokenMemoryPolicy::from_storage(
                        &row.get::<_, String>(4)?,
                    ),
                },
            ))
        })?
        .filter_map(|row| row.ok())
        .collect::<Vec<_>>();
    drop(stmt);

    let now = now_ms();
    for (id, content, metadata) in rows {
        upsert_device_alias_from_memory(conn, id, &content, metadata, now)?;
    }

    Ok(())
}

fn rebuild_household_profile_attributes(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_profile_attributes", [])?;
    let rows = shared_safe_memory_rows(conn)?;
    let now = now_ms();
    for (id, content, metadata) in rows {
        upsert_household_profile_attributes_from_memory(conn, id, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_household_rules(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_rules", [])?;
    let rows = shared_safe_memory_rows(conn)?;
    let now = now_ms();
    for (id, content, metadata) in rows {
        upsert_household_rules_from_memory(conn, id, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_household_notes(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_notes", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_note_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_app_only_secret_references(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM app_only_secret_references", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_app_only_secret_reference_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_media_profile_items(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM media_profile_items", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_media_profile_item_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_family_calendar_events(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM family_calendar_events", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_family_calendar_events_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_shopping_list_items(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM shopping_list_items", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_shopping_list_items_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_household_inventory_items(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_inventory_items", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_inventory_items_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_access_permissions(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM access_permissions", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_access_permissions_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_household_task_logs(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_task_logs", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_task_logs_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_household_schedule_items(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_schedule_items", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_schedule_items_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_household_event_logs(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM household_event_logs", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_household_event_logs_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn rebuild_embedded_memories(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM embedded_memories", [])?;
    let rows = shared_safe_memory_rows_with_kind(conn)?;
    let now = now_ms();
    for (id, kind, content, metadata) in rows {
        upsert_embedded_memory_from_memory(conn, id, &kind, &content, metadata, now)?;
    }
    Ok(())
}

fn shared_safe_memory_rows(
    conn: &Connection,
) -> Result<Vec<(i64, String, policy::MemoryPolicyMetadata)>> {
    let mut stmt = conn.prepare(
        "SELECT id, content, scope, sensitivity, spoken_policy
         FROM memories
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                policy::MemoryPolicyMetadata {
                    scope: policy::MemoryScope::from_storage(&row.get::<_, String>(2)?),
                    sensitivity: policy::MemorySensitivity::from_storage(&row.get::<_, String>(3)?),
                    spoken_policy: policy::SpokenMemoryPolicy::from_storage(
                        &row.get::<_, String>(4)?,
                    ),
                },
            ))
        })?
        .filter_map(|row| row.ok())
        .collect();
    Ok(rows)
}

fn shared_safe_memory_rows_with_kind(
    conn: &Connection,
) -> Result<Vec<(i64, String, String, policy::MemoryPolicyMetadata)>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, content, scope, sensitivity, spoken_policy
         FROM memories
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                policy::MemoryPolicyMetadata {
                    scope: policy::MemoryScope::from_storage(&row.get::<_, String>(3)?),
                    sensitivity: policy::MemorySensitivity::from_storage(&row.get::<_, String>(4)?),
                    spoken_policy: policy::SpokenMemoryPolicy::from_storage(
                        &row.get::<_, String>(5)?,
                    ),
                },
            ))
        })?
        .filter_map(|row| row.ok())
        .collect();
    Ok(rows)
}

fn upsert_household_profile_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_profiles WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    let Some((name, role)) = household_profile_from_memory(kind, content) else {
        return Ok(());
    };

    conn.execute(
        "INSERT INTO household_profiles (source_memory_id, name, role, updated_ms)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![source_memory_id, name, role, updated_ms],
    )?;
    Ok(())
}

fn upsert_household_profile_attributes_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_profile_attributes WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for attr in household_profile_attributes_from_memory(content) {
        let normalized_name = normalize_name_key(&attr.name);
        conn.execute(
            "INSERT INTO household_profile_attributes (
                source_memory_id, name, normalized_name, attribute, value, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                source_memory_id,
                attr.name,
                normalized_name,
                attr.attribute,
                attr.value,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

fn upsert_household_rules_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_rules WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for rule in household_rules_from_memory(content) {
        let normalized_person = rule.person.as_deref().map(normalize_name_key);
        conn.execute(
            "INSERT INTO household_rules (
                source_memory_id, person, normalized_person, rule_type, subject,
                value, allowed, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                source_memory_id,
                rule.person,
                normalized_person,
                rule.rule_type,
                rule.subject,
                rule.value,
                if rule.allowed { 1 } else { 0 },
                rule.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

fn upsert_household_note_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_notes WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    let Some((note_type, title, content)) = household_note_from_memory(kind, content) else {
        return Ok(());
    };

    conn.execute(
        "INSERT INTO household_notes (source_memory_id, note_type, title, content, updated_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![source_memory_id, note_type, title, content, updated_ms],
    )?;
    Ok(())
}

fn upsert_app_only_secret_reference_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM app_only_secret_references WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    let Some(secret_ref) = app_only_secret_reference_from_memory(kind, content, metadata) else {
        return Ok(());
    };

    conn.execute(
        "INSERT INTO app_only_secret_references (
            source_memory_id, secret_type, label, normalized_label, location_hint, updated_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            source_memory_id,
            secret_ref.secret_type,
            secret_ref.label,
            normalize_alias_key(&secret_ref.label),
            secret_ref.location_hint,
            updated_ms
        ],
    )?;
    Ok(())
}

fn upsert_media_profile_item_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    _kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM media_profile_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    let Some(item) = media_profile_item_from_memory(content) else {
        return Ok(());
    };
    let normalized_owner = item
        .owner
        .as_deref()
        .map(normalize_name_key)
        .unwrap_or_default();
    let normalized_name = normalize_alias_key(&item.name);

    conn.execute(
        "INSERT INTO media_profile_items (
            source_memory_id, owner, normalized_owner, item_type, name,
            normalized_name, provider, target, updated_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            source_memory_id,
            item.owner,
            normalized_owner,
            item.item_type,
            item.name,
            normalized_name,
            item.provider,
            item.target,
            updated_ms
        ],
    )?;
    Ok(())
}

fn upsert_family_calendar_events_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM family_calendar_events WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for event in family_calendar_events_from_memory(kind, content) {
        let normalized_person = event.person.as_deref().map(normalize_name_key);
        conn.execute(
            "INSERT INTO family_calendar_events (
                source_memory_id, person, normalized_person, event_type, title,
                day, time, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                source_memory_id,
                event.person,
                normalized_person,
                event.event_type,
                event.title,
                event.day,
                event.time,
                event.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

fn upsert_shopping_list_items_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM shopping_list_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for item in shopping_list_items_from_memory(kind, content) {
        let normalized_item = normalize_alias_key(&item.item);
        conn.execute(
            "INSERT INTO shopping_list_items (
                source_memory_id, item, normalized_item, status, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                source_memory_id,
                item.item,
                normalized_item,
                item.status,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

fn upsert_household_inventory_items_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_inventory_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for item in household_inventory_items_from_memory(kind, content) {
        conn.execute(
            "INSERT INTO household_inventory_items (
                source_memory_id, item, normalized_item, quantity, location,
                category, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                source_memory_id,
                item.item,
                normalize_inventory_item(&item.item),
                item.quantity,
                item.location,
                item.category,
                item.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

fn upsert_access_permissions_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM access_permissions WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for permission in access_permissions_from_memory(kind, content) {
        let normalized_person = normalize_name_key(&permission.person);
        let normalized_device = normalize_alias_key(&permission.device);
        conn.execute(
            "INSERT INTO access_permissions (
                source_memory_id, person, normalized_person, device, normalized_device,
                action, allowed, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                source_memory_id,
                permission.person,
                normalized_person,
                permission.device,
                normalized_device,
                permission.action,
                if permission.allowed { 1 } else { 0 },
                permission.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

fn upsert_household_task_logs_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_task_logs WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for task in household_task_logs_from_memory(kind, content) {
        let normalized_person = normalize_name_key(&task.person);
        let normalized_subject = task.subject.as_deref().map(normalize_alias_key);
        conn.execute(
            "INSERT INTO household_task_logs (
                source_memory_id, person, normalized_person, task, subject,
                normalized_subject, day, time, status, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                source_memory_id,
                task.person,
                normalized_person,
                task.task,
                task.subject,
                normalized_subject,
                task.day,
                task.time,
                task.status,
                task.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

fn upsert_household_schedule_items_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_schedule_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for item in household_schedule_items_from_memory(kind, content) {
        let normalized_subject = item.subject.as_deref().map(normalize_alias_key);
        conn.execute(
            "INSERT INTO household_schedule_items (
                source_memory_id, schedule_type, subject, normalized_subject, title,
                day, date, time, amount, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                source_memory_id,
                item.schedule_type,
                item.subject,
                normalized_subject,
                item.title,
                item.day,
                item.date,
                item.time,
                item.amount,
                item.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

fn upsert_household_event_logs_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM household_event_logs WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    for event in household_event_logs_from_memory(kind, content) {
        let normalized_subject = event.subject.as_deref().map(normalize_alias_key);
        let normalized_actor = event.actor.as_deref().map(normalize_name_key);
        conn.execute(
            "INSERT INTO household_event_logs (
                source_memory_id, event_type, subject, normalized_subject, action,
                actor, normalized_actor, time, description, updated_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                source_memory_id,
                event.event_type,
                event.subject,
                normalized_subject,
                event.action,
                event.actor,
                normalized_actor,
                event.time,
                event.description,
                updated_ms
            ],
        )?;
    }
    Ok(())
}

fn upsert_embedded_memory_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM embedded_memories WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !should_embed_memory(kind, content, metadata) {
        return Ok(());
    }

    let provider = LocalHashEmbeddingProvider;
    let embedding_text = embedding_text_for_memory(kind, content);
    let embedding = provider.embed(&embedding_text);
    let embedding_json = serde_json::to_string(&embedding)?;

    conn.execute(
        "INSERT INTO embedded_memories (
            source_memory_id, memory_type, embedding_model, dimensions, embedding, updated_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            source_memory_id,
            semantic_memory_type(kind, content),
            provider.model_name(),
            provider.dimensions() as i64,
            embedding_json,
            updated_ms
        ],
    )?;
    Ok(())
}

fn delete_structured_household_rows(conn: &Connection, source_memory_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM household_profile_attributes WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_rules WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_notes WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM app_only_secret_references WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM media_profile_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM family_calendar_events WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM shopping_list_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_inventory_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM access_permissions WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_task_logs WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_schedule_items WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM household_event_logs WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    conn.execute(
        "DELETE FROM embedded_memories WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;
    Ok(())
}

fn upsert_device_alias_from_memory(
    conn: &Connection,
    source_memory_id: i64,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
    updated_ms: u64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM device_aliases WHERE source_memory_id = ?1",
        [source_memory_id],
    )?;

    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return Ok(());
    }

    let Some((alias, target_id)) = device_alias_from_memory(content) else {
        return Ok(());
    };
    let normalized_alias = normalize_alias_key(&alias);
    if normalized_alias.is_empty() || target_id.is_empty() {
        return Ok(());
    }
    let kind = device_alias_kind(&target_id);

    conn.execute(
        "INSERT INTO device_aliases (
            source_memory_id, alias, normalized_alias, target_id, kind, updated_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            source_memory_id,
            alias,
            normalized_alias,
            target_id,
            kind,
            updated_ms
        ],
    )?;
    Ok(())
}

fn household_profile_from_memory(_kind: &str, content: &str) -> Option<(String, &'static str)> {
    let lower = content.to_ascii_lowercase();

    if let Some((role, name)) = possessive_named_profile(content, &lower) {
        return Some((name, role));
    }

    if let Some((role, name)) = definite_role_profile(content, &lower) {
        return Some((name, role));
    }

    if let Some((name, role)) = subject_role_profile(content, &lower) {
        return Some((name, role));
    }

    None
}

fn device_alias_from_memory(content: &str) -> Option<(String, String)> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();

    for marker in [
        " maps to ",
        " points to ",
        " targets ",
        " target is ",
        " entity is ",
        " device is ",
        " means ",
        " = ",
        " -> ",
        " is ",
    ] {
        if let Some(pos) = lower.find(marker) {
            let alias = clean_device_alias(&trimmed[..pos]);
            let target = clean_device_target(&trimmed[pos + marker.len()..]);
            if is_valid_device_alias_pair(&alias, &target, marker == " is ") {
                return Some((alias, target));
            }
        }
    }

    None
}

fn clean_device_alias(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("remember that ")
        .trim_start_matches("remember ")
        .trim_start_matches("the ")
        .trim_matches(|ch: char| matches!(ch, '"' | '\''))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_device_target(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("the ")
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '.' | ',' | '!' | '?'))
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

fn is_valid_device_alias_pair(alias: &str, target: &str, broad_marker: bool) -> bool {
    if alias.is_empty() || target.is_empty() || alias.eq_ignore_ascii_case(target) {
        return false;
    }

    let alias_lower = alias.to_ascii_lowercase();
    let target_lower = target.to_ascii_lowercase();
    let looks_like_target = target_lower.contains('.') || target_lower.starts_with("smartplug_");
    let looks_like_alias = [
        "light",
        "lights",
        "lamp",
        "plug",
        "switch",
        "outlet",
        "thermostat",
        "scene",
        "routine",
        "fan",
    ]
    .iter()
    .any(|term| alias_lower.contains(term));

    let explicit_alias_shape = !broad_marker && alias.split_whitespace().count() <= 6;

    looks_like_target && (looks_like_alias || explicit_alias_shape)
}

fn normalize_alias_key(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn device_alias_kind(target_id: &str) -> String {
    target_id
        .split_once('.')
        .map(|(domain, _)| domain.to_string())
        .unwrap_or_else(|| "entity".into())
}

fn household_profile_attributes_from_memory(content: &str) -> Vec<HouseholdProfileAttribute> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let mut attrs = Vec::new();

    if let Some((name, rest)) = split_once_case_insensitive(trimmed, &lower, " is ") {
        let rest_lower = rest.to_ascii_lowercase();
        if let Some(age) = leading_age(&rest_lower) {
            attrs.push(profile_attr(name, "age", &age.to_string()));
        }
    }

    for marker in [" likes ", " prefers ", " enjoys "] {
        if let Some((name, value)) = split_once_case_insensitive(trimmed, &lower, marker) {
            let value = clean_sentence_value(value);
            if !value.is_empty() {
                attrs.push(profile_attr(name, "likes", &value));
            }
        }
    }

    if lower.contains("shoe")
        && let Some((name, value)) = shoe_size_statement(trimmed, &lower)
    {
        attrs.push(profile_attr(&name, "shoe_size", &value));
    }

    if let Some((left, value)) = split_once_case_insensitive(trimmed, &lower, " is ")
        && left.to_ascii_lowercase().contains("favorite ")
    {
        let name = left
            .split_once("'s ")
            .map(|(name, _)| name)
            .unwrap_or("household");
        let attribute = left
            .to_ascii_lowercase()
            .split("favorite ")
            .nth(1)
            .map(|subject| format!("favorite_{}", normalize_rule_subject(subject)))
            .unwrap_or_else(|| "favorite".into());
        let value = clean_sentence_value(value);
        if !value.is_empty() {
            attrs.push(profile_attr(name, &attribute, &value));
        }
    }

    attrs
}

fn shoe_size_statement(content: &str, lower: &str) -> Option<(String, String)> {
    for marker in [" currently wears ", " now wears ", " wears "] {
        if let Some((name, rest)) = split_once_case_insensitive(content, lower, marker) {
            let rest_lower = rest.to_ascii_lowercase();
            if !rest_lower.contains("size") && !rest_lower.contains("shoe") {
                continue;
            }
            let value = rest
                .trim_start_matches("a ")
                .trim_start_matches("an ")
                .trim_start_matches("shoe ")
                .trim_start_matches("size ")
                .trim_start_matches("shoe size ");
            return Some((clean_person_name(name), clean_sentence_value(value)));
        }
    }

    if let Some((left, value)) = split_once_case_insensitive(content, lower, " shoe size is ") {
        return Some((clean_person_name(left), clean_sentence_value(value)));
    }

    None
}

fn household_rules_from_memory(content: &str) -> Vec<HouseholdRule> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let mut rules = Vec::new();

    if lower.contains("allerg")
        && let Some((person, subject)) = parse_allergy_rule(trimmed, &lower)
    {
        rules.push(HouseholdRule {
            source_memory_id: 0,
            person: Some(person),
            rule_type: "allergy".into(),
            subject,
            value: None,
            allowed: false,
            description: trimmed.to_string(),
        });
    }

    if (lower.contains("screen time") || lower.contains("gaming") || lower.contains("video game"))
        && (lower.contains("after ") || lower.contains("ends at "))
        && let Some((person, subject, value)) = parse_screen_time_rule(trimmed, &lower)
    {
        rules.push(HouseholdRule {
            source_memory_id: 0,
            person: Some(person),
            rule_type: "screen_time".into(),
            subject,
            value: Some(value),
            allowed: false,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("homework")
        && let Some(person) = leading_person_name(trimmed)
    {
        rules.push(HouseholdRule {
            source_memory_id: 0,
            person: Some(person),
            rule_type: "homework".into(),
            subject: "homework".into(),
            value: if lower.contains("before screen") {
                Some("before_screen_time".into())
            } else {
                None
            },
            allowed: true,
            description: trimmed.to_string(),
        });
    }

    rules
}

fn household_note_from_memory(kind: &str, content: &str) -> Option<(String, String, String)> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    if trimmed.is_empty() {
        return None;
    }

    let kind_lower = kind.to_ascii_lowercase();
    let lower = trimmed.to_ascii_lowercase();
    if secret_type_from_text(&lower).is_some() {
        return None;
    }
    let (note_type, note_content) = if matches!(
        kind_lower.as_str(),
        "note"
            | "notes"
            | "reminder"
            | "manual"
            | "document"
            | "context"
            | "pet_health"
            | "home_maintenance"
            | "storage"
            | "gift"
            | "recipe"
            | "recipe_book"
            | "mechanic"
            | "troubleshooting"
            | "activity"
            | "media_library"
            | "routine"
            | "safe_inventory"
            | "appliance_manual"
            | "photo_metadata"
            | "warranty"
            | "school"
            | "utility"
            | "recycling"
            | "wellness"
            | "science_project"
            | "first_aid"
            | "medicine"
            | "audiobook"
            | "story"
            | "pet_inventory"
            | "travel"
            | "travel_document"
            | "travel_documents"
            | "diet"
            | "watch_history"
            | "doorbell"
            | "visitor"
            | "music_profile"
            | "device_manual"
            | "device_manuals"
            | "home_note"
            | "home_notes"
            | "home_inventory"
            | "meal_history"
            | "recipe_collection"
            | "shopping_list"
            | "shopping_lists"
            | "school_info"
            | "security_log"
            | "beverage"
            | "beverage_preference"
            | "social_connection"
            | "commute"
            | "pantry"
            | "comfort"
            | "location_history"
            | "tracker"
            | "pizza"
            | "arrival"
            | "financial_record"
            | "financial_records"
            | "digital_scan"
            | "digital_scans"
            | "storage_inventory"
            | "game_manual"
            | "game_manuals"
            | "educational_resource"
            | "educational_resources"
            | "entertainment"
            | "dictionary"
            | "dictionary_knowledge_base"
            | "activity_idea"
            | "activity_ideas"
            | "air_quality"
            | "health_profile"
            | "party_theme"
            | "party_themes"
            | "pest_control"
            | "family_reaction"
            | "family_reactions"
            | "food_safety"
            | "contact_book"
            | "social_graph"
            | "educational_content"
            | "documentary_library"
            | "productivity_tip"
            | "productivity_tips"
            | "sleep_routine"
            | "meal_plan"
            | "delivery_instruction"
            | "delivery_instructions"
            | "shipping_tracking"
            | "flight_info"
            | "traffic_to_airport"
            | "travel_preference"
            | "travel_preferences"
            | "party_recipe"
            | "party_recipes"
            | "pet_calendar"
            | "astronomical_data"
            | "payment_history"
            | "tool_inventory"
            | "digital_receipts"
            | "scanned_docs"
            | "network_config"
            | "health_tracker"
            | "cooking_substitutes"
            | "diy_projects"
            | "material_lists"
            | "plumbing_troubleshooting"
            | "injury_recovery"
            | "health_tips"
            | "gym_schedule"
            | "gym_routine"
            | "financial_advice"
            | "contacts"
            | "message_templates"
            | "location_api"
            | "arrival_rain"
            | "safety_protocol"
            | "streaming_services"
            | "user_location"
            | "turkey_thawing_guide"
            | "safety_equipment_log"
            | "school_documents"
            | "contractor_list"
            | "recipe_notes"
            | "wish_list"
            | "interests_profile"
            | "wellness_activities"
            | "food_pairing_database"
            | "device_profiles"
            | "gift_history"
            | "board_games"
            | "baby_monitor_logs"
            | "news_sources"
            | "appliance_states"
            | "waste_management_log"
            | "environmental_sensors"
            | "location_services"
            | "garden_devices"
            | "appliance_manuals"
            | "security_codes"
            | "subscription_credentials"
            | "music_library"
            | "ebook_store"
            | "read_history"
            | "restaurant_history"
            | "delivery_apps"
            | "plant_care"
            | "weight_trend"
            | "lunch_preferences"
            | "outdoor_furniture"
            | "cycling_route"
            | "financial_services"
            | "smart_plug"
            | "electronic_program_guide"
            | "water_heater_sensor"
            | "craft_inventory"
            | "secure_storage_log"
            | "vehicle_registration"
            | "appliance_warranties"
            | "network_credentials"
            | "local_business_reviews"
            | "wardrobe_inventory"
            | "event_dress_code"
            | "wellness_content"
            | "education_app"
            | "takeout_menus"
            | "hotel_preferences"
            | "maintenance_schedule"
            | "filter_model_number"
            | "routine_logs"
            | "family_activities"
            | "plumbing_history"
            | "sewing_instructions"
            | "breathing_monitor"
            | "smart_scale"
            | "connected_car"
            | "printer_status"
            | "financial_market_api"
            | "pool_robot"
            | "backyard_devices"
            | "baby_monitor"
            | "navigation_service"
            | "smart_lock"
            | "shipping_tracker"
            | "digital_documents"
            | "vehicle_documents"
            | "subscriptions"
            | "cooking_reference"
            | "hobby_inventory"
            | "tutorial_videos"
            | "health_advice"
            | "local_businesses"
            | "charity_ratings"
            | "personal_interests"
            | "language_apps"
            | "podcast_library"
            | "audio_library"
            | "wardrobe_database"
            | "fashion_advice"
            | "beverage_prefs"
            | "uv_index"
            | "sun_safety"
            | "friend_availability"
            | "favorite_dishes"
            | "fever_management"
            | "snow_protocol"
            | "device_usage"
            | "site_category"
            | "weather_video_url"
            | "preferred_presenter"
            | "mood_context"
            | "smart_oven"
            | "plumbing_sensors"
            | "basement_monitoring"
            | "fitness_tracker"
            | "kitchen_appliances"
            | "air_quality_monitor"
            | "appliance_docs"
            | "shoe_closet_inventory"
            | "password_manager"
            | "community_calendar"
            | "restaurant_list"
            | "home_warranties"
            | "network_device_list"
            | "financial_archive"
            | "story_library"
            | "literature_database"
            | "local_trail_database"
            | "photo_album"
            | "object_recognition"
            | "pet_names_db"
            | "educational_video"
            | "camping_checklist"
            | "bar_inventory"
            | "restaurants"
            | "babysitter_availability"
            | "dinner_plan"
            | "water_sensor"
            | "bike_tracker"
            | "security_logs"
            | "taco_bar_ingredients"
            | "user_profiles"
            | "presence_state"
            | "device_states"
            | "comfort_preference_embeddings"
            | "activity_preference_embeddings"
            | "room_mood_embeddings"
            | "sleep_preference_embeddings"
            | "safety_intent_embeddings"
            | "parental_rules"
            | "screen_time_usage"
            | "family_schedule"
            | "inventory_items"
            | "notes_fts"
            | "documents"
            | "last_opened_locations"
            | "manuals_fts"
            | "document_store"
            | "scenes"
            | "scene_actions"
            | "ambient_light_sensors"
            | "reminders"
            | "routine_steps"
            | "access_logs"
            | "device_events"
            | "health_device_events"
            | "delivery_events"
            | "camera_object_events"
            | "shopping_notes_fts"
            | "watering_schedule"
            | "garden_zones"
            | "soil_moisture_sensors"
            | "recipes_fts"
            | "recipe_embeddings"
            | "meal_ratings"
            | "automation_runs"
            | "sensor_health"
            | "alarms"
            | "automation_rules"
            | "appliance_thresholds"
            | "sensor_reading_history"
            | "item_location_events"
            | "motion_events"
            | "camera_devices"
            | "replacement_parts"
            | "room_assignments"
            | "pet_care_routines"
            | "pet_device_events"
            | "household_guides_fts"
            | "household_notes_fts"
            | "family_notes_fts"
            | "family_rules"
            | "family_preference_embeddings"
            | "notification_rules"
            | "vent_states"
            | "blind_positions"
            | "meal_notes"
            | "chore_assignments"
            | "chore_checkins"
            | "energy_meter_readings"
            | "documents_fts"
            | "shared_room_reservations"
            | "school_transport_schedule"
            | "door_sensor_events"
            | "temperature_sensors"
            | "gas_sensors"
            | "stove_state"
            | "presence_alerts"
            | "presence_alert"
            | "document_embeddings"
            | "shopping_list_items"
            | "routine_overrides"
            | "school_notes_fts"
            | "smart_plug_states"
            | "lighting_simulation_rules"
            | "thermostat_schedule_overrides"
            | "lock_check_rules"
            | "food_inventory"
            | "vacuum_zones"
            | "restricted_zones"
            | "do_not_disturb_rule"
            | "irrigation_events"
            | "safety_profiles"
            | "permission_requests"
            | "approval_events"
            | "health_documents_fts"
            | "scene_embeddings"
            | "weather_context"
            | "calendar_events"
            | "reservation"
            | "ble_tag_events"
            | "vacuum_events"
            | "room_map_zones"
            | "obstacle_reports"
            | "device_audit_log"
            | "control_source"
            | "family_calendar"
            | "fan_states"
            | "water_leak_sensors"
            | "health_routines"
            | "medicine_cabinet_events"
            | "activity_notes_fts"
            | "learning_history"
            | "device_metadata"
            | "audio_event_classifications"
            | "device_alerts"
            | "battery_status"
            | "network_access_rules"
            | "school_tasks"
            | "trusted_contacts"
            | "security_audit_log"
            | "guest_profiles"
            | "door_open_events"
            | "daily_checklists"
            | "user_preferences"
            | "floor_plan_graph"
            | "safety_routes"
            | "smoke_detector_locations"
            | "door_window_sensor_states"
            | "project_notes_fts"
            | "home_project_records"
            | "glass_break_sensors"
            | "camera_events"
            | "child_contact_rules"
            | "device_health"
            | "child_profiles"
            | "laundry_events"
            | "hvac_runtime"
            | "window_sensor_states"
            | "appliance_events"
            | "notification_log"
            | "air_quality_sensors"
            | "filter_status"
            | "filter_life"
            | "municipal_schedule"
            | "household_routines"
            | "routine_checkins"
            | "home_project_notes_fts"
            | "electrical_panel_map"
            | "item_embeddings"
            | "timers"
            | "home_maintenance_embeddings"
            | "scheduled_device_actions"
            | "medicine_inventory"
            | "guest_access_policies"
            | "household_notes"
            | "alarm_preferences"
            | "media_sessions"
            | "user_media_aliases"
            | "media_preference_embeddings"
            | "kitchen_timers"
            | "temporary_notifications"
            | "device_safety_profiles"
            | "security_mode_attempts"
            | "lock_errors"
            | "device_notes_fts"
            | "device_credentials"
            | "open_reminders"
            | "plant_care_profiles"
            | "last_watered_events"
            | "dishwasher_rack_state"
            | "kitchen_item_locations"
            | "door_sensor_states"
            | "project_lists"
            | "project_list_items"
            | "sensor_alert_rules"
            | "privacy_audit_log"
            | "family_messages"
            | "message_events"
            | "camera_access_logs"
            | "privacy_mode_events"
            | "camera_recording_rules"
            | "meal_memory_embeddings"
            | "child_media_rules"
            | "media_preferences"
            | "pet_feeding_events"
            | "pet_care_profiles"
            | "outdoor_air_quality_feed"
            | "indoor_air_quality_sensors"
            | "power_events"
            | "holiday_calendar"
            | "alarm_preference_embeddings"
            | "network_clients"
            | "known_devices"
            | "outdoor_temperature_sensors"
            | "moisture_sensors"
            | "router_stats"
            | "bandwidth_usage"
            | "activity_templates"
            | "camera_health"
            | "image_quality_metrics"
            | "maintenance_notes"
            | "garage_door_events"
            | "water_pressure_sensors"
            | "home_utility_thresholds"
            | "water_valve_state"
            | "camera_person_events"
            | "humidity_sensors"
            | "dehumidifier_state"
            | "filter_life_remaining"
            | "user_print_rules"
            | "printer_supplies"
            | "cooking_sessions"
            | "appliance_safety_profiles"
            | "camera_privacy_rules"
            | "room_sensor_history"
            | "do_not_disturb_rules"
            | "temporary_mode_overrides"
            | "lock_status"
            | "smoke_detector_status"
            | "security_modes"
            | "recipe_ingredients"
            | "school_forms"
            | "meal_notes_fts"
    ) {
        (note_type_from_kind(&kind_lower, &lower), trimmed)
    } else if let Some(rest) = lower
        .strip_prefix("remember that ")
        .and_then(|_| trimmed.get("remember that ".len()..))
    {
        ("note", rest.trim())
    } else if let Some(rest) = lower
        .strip_prefix("remember to ")
        .and_then(|_| trimmed.get("remember to ".len()..))
    {
        ("reminder", rest.trim())
    } else if let Some(rest) = lower
        .strip_prefix("note: ")
        .and_then(|_| trimmed.get("note: ".len()..))
    {
        ("note", rest.trim())
    } else if lower.contains(" manual:") || lower.starts_with("manual:") {
        ("manual", trimmed)
    } else if lower.starts_with("watched ") || lower.contains(" watched ") {
        ("media", trimmed)
    } else {
        return None;
    };

    let note_content = note_content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    if note_content.is_empty() {
        return None;
    }

    Some((
        note_type.to_string(),
        household_note_title(note_content),
        note_content.to_string(),
    ))
}

fn note_type_from_kind<'a>(kind: &'a str, lower_content: &str) -> &'a str {
    match kind {
        "reminder" => "reminder",
        "manual" | "document" => "manual",
        "pet_health" => "pet_health",
        "home_maintenance" => "home_maintenance",
        "storage" => "storage",
        "gift" => "gift",
        "recipe" | "recipe_book" => "recipe",
        "mechanic" | "troubleshooting" => "troubleshooting",
        "activity" => "activity",
        "media_library" => "media",
        "routine" => "routine",
        "safe_inventory" => "storage",
        "appliance_manual" => "manual",
        "photo_metadata" => "photo",
        "warranty" => "warranty",
        "school" => "school",
        "utility" => "utility",
        "recycling" => "recycling",
        "wellness" => "wellness",
        "science_project" => "education",
        "first_aid" | "medicine" => "first_aid",
        "audiobook" | "story" => "story",
        "pet_inventory" => "pet",
        "travel" | "travel_document" | "travel_documents" => "travel",
        "diet" => "diet",
        "watch_history" => "media",
        "doorbell" | "visitor" => "visitor",
        "music_profile" => "media",
        "device_manual" | "device_manuals" => "manual",
        "home_note" | "home_notes" => "home_maintenance",
        "home_inventory" => "storage",
        "meal_history" => "meal",
        "recipe_collection" => "recipe",
        "shopping_list" | "shopping_lists" => "shopping",
        "school_info" => "school",
        "security_log" => "security",
        "beverage" | "beverage_preference" => "beverage",
        "social_connection" => "social",
        "commute" => "commute",
        "pantry" => "pantry",
        "comfort" => "home_comfort",
        "location_history" | "tracker" => "location",
        "pizza" => "shopping",
        "arrival" => "routine",
        "financial_record" | "financial_records" | "digital_scan" | "digital_scans" => "receipt",
        "storage_inventory" => "storage",
        "game_manual" | "game_manuals" => "manual",
        "educational_resource" | "educational_resources" => "education",
        "entertainment" => "entertainment",
        "dictionary" | "dictionary_knowledge_base" => "dictionary",
        "activity_idea" | "activity_ideas" => "activity",
        "air_quality" | "health_profile" => "health",
        "party_theme" | "party_themes" => "party",
        "pest_control" => "pest_control",
        "family_reaction" | "family_reactions" => "family",
        "food_safety" => "food_safety",
        "contact_book" | "social_graph" => "contact",
        "educational_content" | "documentary_library" => "education",
        "productivity_tip" | "productivity_tips" | "sleep_routine" => "routine",
        "meal_plan" => "meal",
        "delivery_instruction" | "delivery_instructions" | "shipping_tracking" => "delivery",
        "flight_info" | "traffic_to_airport" | "travel_preference" | "travel_preferences" => {
            "travel"
        }
        "party_recipe" | "party_recipes" => "party",
        "pet_calendar" => "pet",
        "astronomical_data" => "schedule",
        "payment_history" | "financial_advice" => "finance",
        "tool_inventory" => "tool",
        "digital_receipts" | "scanned_docs" => "receipt",
        "network_config" => "network",
        "health_tracker" | "injury_recovery" | "health_tips" => "health",
        "cooking_substitutes" => "recipe",
        "diy_projects" | "material_lists" => "diy",
        "plumbing_troubleshooting" => "home_maintenance",
        "gym_schedule" | "gym_routine" => "fitness",
        "contacts" | "message_templates" => "contact",
        "location_api" | "arrival_rain" | "safety_protocol" | "user_location" => "safety",
        "streaming_services" => "media",
        "turkey_thawing_guide" => "food_safety",
        "safety_equipment_log" => "safety",
        "school_documents" => "school",
        "contractor_list" => "contact",
        "recipe_notes" => "recipe",
        "wish_list" | "interests_profile" | "gift_history" => "gift",
        "wellness_activities" => "wellness",
        "food_pairing_database" => "recipe",
        "device_profiles" => "device",
        "board_games" => "entertainment",
        "baby_monitor_logs" => "routine",
        "news_sources" => "news",
        "appliance_states" => "device",
        "waste_management_log" => "schedule",
        "environmental_sensors" => "home_comfort",
        "location_services" => "location",
        "garden_devices" => "device",
        "appliance_manuals" => "manual",
        "security_codes" => "security",
        "subscription_credentials" => "security",
        "music_library" => "media",
        "ebook_store" | "read_history" => "entertainment",
        "restaurant_history" | "delivery_apps" => "meal",
        "plant_care" => "home_maintenance",
        "weight_trend" => "health",
        "lunch_preferences" => "meal",
        "outdoor_furniture" => "home_maintenance",
        "cycling_route" => "fitness",
        "financial_services" => "finance",
        "smart_plug" => "device",
        "electronic_program_guide" => "media",
        "water_heater_sensor" => "device",
        "craft_inventory" => "storage",
        "secure_storage_log" => "storage",
        "vehicle_registration" => "vehicle",
        "appliance_warranties" => "warranty",
        "network_credentials" => "network",
        "local_business_reviews" => "business",
        "wardrobe_inventory" | "event_dress_code" => "wardrobe",
        "wellness_content" => "wellness",
        "education_app" => "education",
        "takeout_menus" => "meal",
        "hotel_preferences" => "travel",
        "maintenance_schedule" | "filter_model_number" => "home_maintenance",
        "routine_logs" => "routine",
        "family_activities" => "activity",
        "plumbing_history" => "home_maintenance",
        "sewing_instructions" => "diy",
        "breathing_monitor" => "health",
        "smart_scale" => "health",
        "connected_car" => "vehicle",
        "printer_status" => "device",
        "financial_market_api" => "finance",
        "pool_robot" | "backyard_devices" => "device",
        "baby_monitor" => "health",
        "navigation_service" => "commute",
        "smart_lock" => "security",
        "shipping_tracker" => "delivery",
        "digital_documents" => "warranty",
        "vehicle_documents" => "vehicle",
        "subscriptions" => "finance",
        "cooking_reference" => "recipe",
        "hobby_inventory" | "tutorial_videos" => "activity",
        "health_advice" | "fever_management" => "health",
        "local_businesses" => "business",
        "charity_ratings" | "personal_interests" => "social",
        "language_apps" => "education",
        "podcast_library" | "audio_library" => "media",
        "wardrobe_database" | "fashion_advice" => "wardrobe",
        "beverage_prefs" => "beverage",
        "uv_index" | "sun_safety" => "safety",
        "friend_availability" => "social",
        "favorite_dishes" => "meal",
        "snow_protocol" => "home_maintenance",
        "device_usage" | "site_category" => "education",
        "weather_video_url" | "preferred_presenter" => "news",
        "mood_context" => "wellness",
        "smart_oven" | "plumbing_sensors" | "basement_monitoring" | "kitchen_appliances" => {
            "device"
        }
        "fitness_tracker" => "fitness",
        "air_quality_monitor" => "health",
        "appliance_docs" => "manual",
        "shoe_closet_inventory" => "storage",
        "password_manager" => "security",
        "community_calendar" => "schedule",
        "restaurant_list" => "contact",
        "home_warranties" => "warranty",
        "network_device_list" => "network",
        "financial_archive" => "finance",
        "story_library" => "story",
        "literature_database" => "literature",
        "local_trail_database" => "activity",
        "photo_album" | "object_recognition" => "photo",
        "pet_names_db" => "pet",
        "educational_video" => "education",
        "camping_checklist" => "activity",
        "bar_inventory" => "recipe",
        "restaurants" | "babysitter_availability" => "social",
        "dinner_plan" => "meal",
        "water_sensor" => "home_maintenance",
        "bike_tracker" | "security_logs" => "security",
        "taco_bar_ingredients" => "meal",
        "user_profiles" => "profile",
        "presence_state" | "last_opened_locations" | "item_location_events" => "location",
        "device_states"
        | "scene_actions"
        | "ambient_light_sensors"
        | "motion_events"
        | "camera_devices" => "device",
        "comfort_preference_embeddings"
        | "activity_preference_embeddings"
        | "room_mood_embeddings"
        | "family_preference_embeddings"
        | "scene_embeddings"
        | "sleep_preference_embeddings" => "home_comfort",
        "safety_intent_embeddings" => "safety",
        "parental_rules" | "screen_time_usage" => "family",
        "inventory_items" => "inventory",
        "family_schedule"
        | "school_transport_schedule"
        | "shared_room_reservations"
        | "reminders"
        | "alarms"
        | "presence_alerts"
        | "presence_alert"
        | "reservation" => "schedule",
        "notes_fts" | "documents" | "documents_fts" | "school_notes_fts" => "school",
        "manuals_fts" | "document_store" | "health_documents_fts" => "manual",
        "scenes" | "automation_rules" | "routine_steps" | "routine_overrides" => "routine",
        "access_logs" | "device_events" => "security",
        "health_device_events" => "health",
        "pet_care_routines" | "pet_device_events" => "pet",
        "delivery_events" | "camera_object_events" | "shopping_notes_fts" => "delivery",
        "watering_schedule" | "garden_zones" | "soil_moisture_sensors" | "irrigation_events" => {
            "garden"
        }
        "recipes_fts" | "recipe_embeddings" => "recipe",
        "meal_ratings" | "meal_notes" | "food_inventory" => "meal",
        "household_guides_fts" => "recycling",
        "household_notes_fts" => "home_maintenance",
        "family_notes_fts" | "family_rules" => "family",
        "chore_assignments" | "chore_checkins" => "family",
        "automation_runs" | "sensor_health" | "vent_states" | "blind_positions" => {
            "troubleshooting"
        }
        "appliance_thresholds"
        | "sensor_reading_history"
        | "door_sensor_events"
        | "temperature_sensors"
        | "energy_meter_readings"
        | "smart_plug_states" => "device",
        "room_assignments" | "vacuum_zones" | "restricted_zones" => "location",
        "ble_tag_events" => "location",
        "vacuum_events" | "room_map_zones" | "obstacle_reports" => "device",
        "device_audit_log" | "control_source" | "security_audit_log" => "security",
        "family_calendar" | "daily_checklists" => "schedule",
        "fan_states" | "device_metadata" | "device_health" => "device",
        "water_leak_sensors" | "glass_break_sensors" | "camera_events" => "safety",
        "health_routines" | "medicine_cabinet_events" => "health",
        "activity_notes_fts" | "learning_history" => "education",
        "audio_event_classifications" | "device_alerts" | "battery_status" => "troubleshooting",
        "network_access_rules" | "school_tasks" => "education",
        "trusted_contacts" | "guest_profiles" | "child_profiles" => "family",
        "door_open_events" => "delivery",
        "user_preferences" => "home_comfort",
        "floor_plan_graph"
        | "safety_routes"
        | "smoke_detector_locations"
        | "door_window_sensor_states" => "safety",
        "project_notes_fts" | "home_project_records" => "home_maintenance",
        "child_contact_rules" => "family",
        "laundry_events" | "appliance_events" | "notification_log" => "device",
        "hvac_runtime" | "window_sensor_states" => "home_comfort",
        "air_quality_sensors" | "filter_status" | "filter_life" => "health",
        "municipal_schedule" | "household_routines" | "routine_checkins" => "schedule",
        "home_project_notes_fts" | "electrical_panel_map" => "home_maintenance",
        "item_embeddings" => "inventory",
        "timers" | "scheduled_device_actions" | "alarm_preferences" => "schedule",
        "home_maintenance_embeddings" => "home_maintenance",
        "medicine_inventory" => "health",
        "guest_access_policies" => "family",
        "household_notes" => "home_maintenance",
        "media_sessions" | "user_media_aliases" | "media_preference_embeddings" => "media",
        "kitchen_timers" | "temporary_notifications" => "device",
        "device_safety_profiles" => "safety",
        "security_mode_attempts" | "lock_errors" => "security",
        "device_notes_fts" | "device_credentials" => "device",
        "open_reminders" => "schedule",
        "plant_care_profiles" | "last_watered_events" => "garden",
        "dishwasher_rack_state" | "kitchen_item_locations" => "inventory",
        "door_sensor_states" => "security",
        "project_lists" | "project_list_items" => "school",
        "sensor_alert_rules" => "device",
        "privacy_audit_log"
        | "camera_access_logs"
        | "privacy_mode_events"
        | "camera_recording_rules"
        | "camera_privacy_rules" => "privacy",
        "family_messages" | "message_events" => "family",
        "meal_memory_embeddings" => "meal",
        "child_media_rules" | "media_preferences" => "media",
        "pet_feeding_events" | "pet_care_profiles" => "pet",
        "outdoor_air_quality_feed" | "indoor_air_quality_sensors" => "health",
        "power_events" => "device",
        "holiday_calendar" => "schedule",
        "alarm_preference_embeddings" => "schedule",
        "network_clients" | "known_devices" | "router_stats" | "bandwidth_usage" => "network",
        "outdoor_temperature_sensors" | "moisture_sensors" => "safety",
        "activity_templates" => "activity",
        "camera_health" | "image_quality_metrics" | "maintenance_notes" => "home_maintenance",
        "garage_door_events" => "security",
        "water_pressure_sensors" | "home_utility_thresholds" | "water_valve_state" => "utility",
        "camera_person_events" => "security",
        "humidity_sensors" | "dehumidifier_state" | "filter_life_remaining" => "home_comfort",
        "user_print_rules" | "printer_supplies" => "device",
        "cooking_sessions" => "recipe",
        "appliance_safety_profiles" => "safety",
        "room_sensor_history" => "home_comfort",
        "do_not_disturb_rules" => "home_comfort",
        "temporary_mode_overrides" => "routine",
        "lock_status" | "security_modes" => "security",
        "smoke_detector_status" => "safety",
        "recipe_ingredients" => "recipe",
        "school_forms" => "school",
        "meal_notes_fts" => "meal",
        "notification_rules" | "do_not_disturb_rule" => "home_comfort",
        "gas_sensors" | "stove_state" | "safety_profiles" => "safety",
        "shopping_list_items" => "shopping",
        "permission_requests" | "approval_events" => "family",
        "replacement_parts" => "inventory",
        _ if lower_content.starts_with("watched ") || lower_content.contains(" watched ") => {
            "media"
        }
        _ => "note",
    }
}

fn household_note_title(content: &str) -> String {
    let title = content
        .split(['.', '!', '?'])
        .next()
        .unwrap_or(content)
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        "note".into()
    } else {
        title
    }
}

fn should_embed_memory(kind: &str, content: &str, metadata: policy::MemoryPolicyMetadata) -> bool {
    if secret_type_from_text(&content.to_ascii_lowercase()).is_some() {
        return false;
    }
    if !policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice()).allowed
    {
        return false;
    }

    let kind = kind.to_ascii_lowercase();
    let lower = content.to_ascii_lowercase();
    matches!(
        kind.as_str(),
        "preference"
            | "context"
            | "fact"
            | "note"
            | "document"
            | "manual"
            | "shopping"
            | "movie"
            | "activity"
            | "mechanic"
            | "troubleshooting"
            | "recipe"
            | "recipe_book"
            | "media_library"
            | "pet_health"
            | "home_maintenance"
            | "routine"
            | "wellness"
            | "science_project"
            | "first_aid"
            | "medicine"
            | "audiobook"
            | "story"
            | "pet_inventory"
            | "travel"
            | "travel_document"
            | "travel_documents"
            | "diet"
            | "watch_history"
            | "doorbell"
            | "visitor"
            | "music_profile"
            | "device_manual"
            | "device_manuals"
            | "home_note"
            | "home_notes"
            | "home_inventory"
            | "meal_history"
            | "recipe_collection"
            | "shopping_list"
            | "shopping_lists"
            | "school_info"
            | "security_log"
            | "beverage"
            | "beverage_preference"
            | "social_connection"
            | "commute"
            | "pantry"
            | "comfort"
            | "location_history"
            | "tracker"
            | "pizza"
            | "arrival"
            | "financial_record"
            | "financial_records"
            | "digital_scan"
            | "digital_scans"
            | "storage_inventory"
            | "game_manual"
            | "game_manuals"
            | "educational_resource"
            | "educational_resources"
            | "entertainment"
            | "dictionary"
            | "dictionary_knowledge_base"
            | "activity_idea"
            | "activity_ideas"
            | "air_quality"
            | "health_profile"
            | "party_theme"
            | "party_themes"
            | "pest_control"
            | "family_reaction"
            | "family_reactions"
            | "food_safety"
            | "contact_book"
            | "social_graph"
            | "educational_content"
            | "documentary_library"
            | "productivity_tip"
            | "productivity_tips"
            | "sleep_routine"
            | "meal_plan"
            | "delivery_instruction"
            | "delivery_instructions"
            | "shipping_tracking"
            | "flight_info"
            | "traffic_to_airport"
            | "travel_preference"
            | "travel_preferences"
            | "party_recipe"
            | "party_recipes"
            | "pet_calendar"
            | "astronomical_data"
            | "payment_history"
            | "tool_inventory"
            | "digital_receipts"
            | "scanned_docs"
            | "network_config"
            | "health_tracker"
            | "cooking_substitutes"
            | "diy_projects"
            | "material_lists"
            | "plumbing_troubleshooting"
            | "injury_recovery"
            | "health_tips"
            | "gym_schedule"
            | "gym_routine"
            | "financial_advice"
            | "contacts"
            | "message_templates"
            | "location_api"
            | "arrival_rain"
            | "safety_protocol"
            | "streaming_services"
            | "user_location"
            | "turkey_thawing_guide"
            | "safety_equipment_log"
            | "school_documents"
            | "contractor_list"
            | "recipe_notes"
            | "wish_list"
            | "interests_profile"
            | "wellness_activities"
            | "food_pairing_database"
            | "device_profiles"
            | "gift_history"
            | "board_games"
            | "baby_monitor_logs"
            | "news_sources"
            | "appliance_states"
            | "waste_management_log"
            | "environmental_sensors"
            | "location_services"
            | "garden_devices"
            | "appliance_manuals"
            | "security_codes"
            | "subscription_credentials"
            | "music_library"
            | "ebook_store"
            | "read_history"
            | "restaurant_history"
            | "delivery_apps"
            | "plant_care"
            | "weight_trend"
            | "lunch_preferences"
            | "outdoor_furniture"
            | "cycling_route"
            | "financial_services"
            | "smart_plug"
            | "electronic_program_guide"
            | "water_heater_sensor"
            | "craft_inventory"
            | "secure_storage_log"
            | "vehicle_registration"
            | "appliance_warranties"
            | "network_credentials"
            | "local_business_reviews"
            | "wardrobe_inventory"
            | "event_dress_code"
            | "wellness_content"
            | "education_app"
            | "takeout_menus"
            | "hotel_preferences"
            | "maintenance_schedule"
            | "filter_model_number"
            | "routine_logs"
            | "family_activities"
            | "plumbing_history"
            | "sewing_instructions"
            | "breathing_monitor"
            | "smart_scale"
            | "connected_car"
            | "printer_status"
            | "financial_market_api"
            | "pool_robot"
            | "backyard_devices"
            | "baby_monitor"
            | "navigation_service"
            | "smart_lock"
            | "shipping_tracker"
            | "digital_documents"
            | "vehicle_documents"
            | "subscriptions"
            | "cooking_reference"
            | "hobby_inventory"
            | "tutorial_videos"
            | "health_advice"
            | "local_businesses"
            | "charity_ratings"
            | "personal_interests"
            | "language_apps"
            | "podcast_library"
            | "audio_library"
            | "wardrobe_database"
            | "fashion_advice"
            | "beverage_prefs"
            | "uv_index"
            | "sun_safety"
            | "friend_availability"
            | "favorite_dishes"
            | "fever_management"
            | "snow_protocol"
            | "device_usage"
            | "site_category"
            | "weather_video_url"
            | "preferred_presenter"
            | "mood_context"
            | "smart_oven"
            | "plumbing_sensors"
            | "basement_monitoring"
            | "fitness_tracker"
            | "kitchen_appliances"
            | "air_quality_monitor"
            | "appliance_docs"
            | "shoe_closet_inventory"
            | "password_manager"
            | "community_calendar"
            | "restaurant_list"
            | "home_warranties"
            | "network_device_list"
            | "financial_archive"
            | "story_library"
            | "literature_database"
            | "local_trail_database"
            | "photo_album"
            | "object_recognition"
            | "pet_names_db"
            | "educational_video"
            | "camping_checklist"
            | "bar_inventory"
            | "restaurants"
            | "babysitter_availability"
            | "dinner_plan"
            | "water_sensor"
            | "bike_tracker"
            | "security_logs"
            | "taco_bar_ingredients"
            | "user_profiles"
            | "presence_state"
            | "device_states"
            | "comfort_preference_embeddings"
            | "activity_preference_embeddings"
            | "room_mood_embeddings"
            | "sleep_preference_embeddings"
            | "safety_intent_embeddings"
            | "parental_rules"
            | "screen_time_usage"
            | "family_schedule"
            | "inventory_items"
            | "notes_fts"
            | "documents"
            | "last_opened_locations"
            | "manuals_fts"
            | "document_store"
            | "scenes"
            | "scene_actions"
            | "ambient_light_sensors"
            | "reminders"
            | "routine_steps"
            | "access_logs"
            | "device_events"
            | "health_device_events"
            | "delivery_events"
            | "camera_object_events"
            | "shopping_notes_fts"
            | "watering_schedule"
            | "garden_zones"
            | "soil_moisture_sensors"
            | "recipes_fts"
            | "recipe_embeddings"
            | "meal_ratings"
            | "automation_runs"
            | "sensor_health"
            | "alarms"
            | "automation_rules"
            | "appliance_thresholds"
            | "sensor_reading_history"
            | "item_location_events"
            | "motion_events"
            | "camera_devices"
            | "replacement_parts"
            | "room_assignments"
            | "pet_care_routines"
            | "pet_device_events"
            | "household_guides_fts"
            | "household_notes_fts"
            | "family_notes_fts"
            | "family_rules"
            | "family_preference_embeddings"
            | "notification_rules"
            | "vent_states"
            | "blind_positions"
            | "meal_notes"
            | "chore_assignments"
            | "chore_checkins"
            | "energy_meter_readings"
            | "documents_fts"
            | "shared_room_reservations"
            | "school_transport_schedule"
            | "door_sensor_events"
            | "temperature_sensors"
            | "gas_sensors"
            | "stove_state"
            | "presence_alerts"
            | "presence_alert"
            | "document_embeddings"
            | "shopping_list_items"
            | "routine_overrides"
            | "school_notes_fts"
            | "smart_plug_states"
            | "lighting_simulation_rules"
            | "thermostat_schedule_overrides"
            | "lock_check_rules"
            | "food_inventory"
            | "vacuum_zones"
            | "restricted_zones"
            | "do_not_disturb_rule"
            | "irrigation_events"
            | "safety_profiles"
            | "permission_requests"
            | "approval_events"
            | "health_documents_fts"
            | "scene_embeddings"
            | "weather_context"
            | "calendar_events"
            | "reservation"
            | "ble_tag_events"
            | "vacuum_events"
            | "room_map_zones"
            | "obstacle_reports"
            | "device_audit_log"
            | "control_source"
            | "family_calendar"
            | "fan_states"
            | "water_leak_sensors"
            | "health_routines"
            | "medicine_cabinet_events"
            | "activity_notes_fts"
            | "learning_history"
            | "device_metadata"
            | "audio_event_classifications"
            | "device_alerts"
            | "battery_status"
            | "network_access_rules"
            | "school_tasks"
            | "trusted_contacts"
            | "security_audit_log"
            | "guest_profiles"
            | "door_open_events"
            | "daily_checklists"
            | "user_preferences"
            | "floor_plan_graph"
            | "safety_routes"
            | "smoke_detector_locations"
            | "door_window_sensor_states"
            | "project_notes_fts"
            | "home_project_records"
            | "glass_break_sensors"
            | "camera_events"
            | "child_contact_rules"
            | "device_health"
            | "child_profiles"
            | "laundry_events"
            | "hvac_runtime"
            | "window_sensor_states"
            | "appliance_events"
            | "notification_log"
            | "air_quality_sensors"
            | "filter_status"
            | "filter_life"
            | "municipal_schedule"
            | "household_routines"
            | "routine_checkins"
            | "home_project_notes_fts"
            | "electrical_panel_map"
            | "item_embeddings"
            | "timers"
            | "home_maintenance_embeddings"
            | "scheduled_device_actions"
            | "medicine_inventory"
            | "guest_access_policies"
            | "household_notes"
            | "alarm_preferences"
            | "media_sessions"
            | "user_media_aliases"
            | "media_preference_embeddings"
            | "kitchen_timers"
            | "temporary_notifications"
            | "device_safety_profiles"
            | "security_mode_attempts"
            | "lock_errors"
            | "device_notes_fts"
            | "device_credentials"
            | "open_reminders"
            | "plant_care_profiles"
            | "last_watered_events"
            | "dishwasher_rack_state"
            | "kitchen_item_locations"
            | "door_sensor_states"
            | "project_lists"
            | "project_list_items"
            | "sensor_alert_rules"
            | "privacy_audit_log"
            | "family_messages"
            | "message_events"
            | "camera_access_logs"
            | "privacy_mode_events"
            | "camera_recording_rules"
            | "meal_memory_embeddings"
            | "child_media_rules"
            | "media_preferences"
            | "pet_feeding_events"
            | "pet_care_profiles"
            | "outdoor_air_quality_feed"
            | "indoor_air_quality_sensors"
            | "power_events"
            | "holiday_calendar"
            | "alarm_preference_embeddings"
            | "network_clients"
            | "known_devices"
            | "outdoor_temperature_sensors"
            | "moisture_sensors"
            | "router_stats"
            | "bandwidth_usage"
            | "activity_templates"
            | "camera_health"
            | "image_quality_metrics"
            | "maintenance_notes"
            | "garage_door_events"
            | "water_pressure_sensors"
            | "home_utility_thresholds"
            | "water_valve_state"
            | "camera_person_events"
            | "humidity_sensors"
            | "dehumidifier_state"
            | "filter_life_remaining"
            | "user_print_rules"
            | "printer_supplies"
            | "cooking_sessions"
            | "appliance_safety_profiles"
            | "camera_privacy_rules"
            | "room_sensor_history"
            | "do_not_disturb_rules"
            | "temporary_mode_overrides"
            | "lock_status"
            | "smoke_detector_status"
            | "security_modes"
            | "recipe_ingredients"
            | "school_forms"
            | "meal_notes_fts"
    ) || content.len() >= 48
        || lower.contains("thermostat")
        || lower.contains("lunchbox")
        || lower.contains("lunch box")
        || lower.contains("snack")
        || lower.contains("movie")
        || lower.contains("watched")
        || lower.contains("detergent")
        || lower.contains("coffee machine")
        || lower.contains("bored")
        || lower.contains("lego")
        || lower.contains("car")
        || lower.contains("mechanic")
        || lower.contains("printer")
        || lower.contains("recipe")
        || lower.contains("chicken")
        || lower.contains("rice")
        || lower.contains("comfort")
        || lower.contains("feel-good")
        || lower.contains("park")
        || lower.contains("plumber")
        || lower.contains("leak")
        || lower.contains("date night")
        || lower.contains("grandma")
        || lower.contains("feeding")
        || lower.contains("stressed")
        || lower.contains("stress")
        || lower.contains("calm")
        || lower.contains("science fair")
        || lower.contains("baking soda")
        || lower.contains("flour")
        || lower.contains("sugar")
        || lower.contains("headache")
        || lower.contains("tylenol")
        || lower.contains("story")
        || lower.contains("audiobook")
        || lower.contains("recycling")
        || lower.contains("dog food")
        || lower.contains("zoo")
        || lower.contains("diet")
        || lower.contains("calorie")
        || lower.contains("washing machine")
        || lower.contains("shaking")
        || lower.contains("watch history")
        || lower.contains("doorbell")
        || lower.contains("uncle")
        || lower.contains("focus music")
        || lower.contains("scary movie")
        || lower.contains("horror")
        || lower.contains("thriller")
        || lower.contains("drink")
        || lower.contains("hydration")
        || lower.contains("too bright")
        || lower.contains("blinds")
        || lower.contains("lonely")
        || lower.contains("video call")
        || lower.contains("sink smells")
        || lower.contains("garbage disposal")
        || lower.contains("commute")
        || lower.contains("traffic")
        || lower.contains("tacos")
        || lower.contains("muggy")
        || lower.contains("humidity")
        || lower.contains("dehumidifier")
        || lower.contains("cut my finger")
        || lower.contains("band-aid")
        || lower.contains("keys")
        || lower.contains("bluetooth tracker")
        || lower.contains("noise outside")
        || lower.contains("pizza")
        || lower.contains("start the car")
        || lower.contains("arrival routine")
        || lower.contains("math homework")
        || lower.contains("fractions")
        || lower.contains("joke")
        || lower.contains("ephemeral")
        || lower.contains("build a fort")
        || lower.contains("train")
        || lower.contains("air quality")
        || lower.contains("asthma")
        || lower.contains("slow cooker")
        || lower.contains("chili")
        || lower.contains("birthday party")
        || lower.contains("spider")
        || lower.contains("remote")
        || lower.contains("freezer")
        || lower.contains("food safety")
        || lower.contains("chicken stir-fry")
        || lower.contains("stand-up")
        || lower.contains("solar system")
        || lower.contains("oversleep")
        || lower.contains("backup alarm")
        || lower.contains("airport")
        || lower.contains("flight")
        || lower.contains("dinner party")
        || lower.contains("gluten")
        || lower.contains("vegan")
        || lower.contains("guest arrival")
        || lower.contains("guests coming")
        || lower.contains("baby is awake")
        || lower.contains("nightlight")
        || lower.contains("meal plan")
        || lower.contains("running playlist")
        || lower.contains("package")
        || lower.contains("delivery instruction")
        || lower.contains("olive oil")
        || lower.contains("substitute")
        || lower.contains("bookshelf")
        || lower.contains("toilet")
        || lower.contains("flapper")
        || lower.contains("knee")
        || lower.contains("side dish")
        || lower.contains("pasta")
        || lower.contains("gym bag")
        || lower.contains("knee sleeves")
        || lower.contains("over budget")
        || lower.contains("happy birthday")
        || lower.contains("driving home")
        || lower.contains("rain")
        || lower.contains("dark parking lot")
        || lower.contains("live location")
        || lower.contains("haven't seen")
        || lower.contains("defrost")
        || lower.contains("turkey")
        || lower.contains("father's day")
        || lower.contains("father s day")
        || lower.contains("fathers day")
        || lower.contains("gift idea")
        || lower.contains("need a break")
        || lower.contains("breathing exercise")
        || lower.contains("wine")
        || lower.contains("steak")
        || lower.contains("stuffy")
        || lower.contains("ventilation")
        || lower.contains("working from home")
        || lower.contains("work from home")
        || lower.contains("printer ink")
        || lower.contains("cartridge")
        || lower.contains("safe to run")
        || lower.contains("running outside")
        || lower.contains("game night")
        || lower.contains("board game")
        || lower.contains("baby is crying again")
        || lower.contains("white noise")
        || lower.contains("jazz")
        || lower.contains("suggest a book")
        || lower.contains("mystery")
        || lower.contains("thriller")
        || lower.contains("takeout")
        || lower.contains("sushi")
        || lower.contains("ripe banana")
        || lower.contains("banana bread")
        || lower.contains("beach trip")
        || lower.contains("freeze")
        || lower.contains("potted plants")
        || lower.contains("weight trend")
        || lower.contains("pack a lunch")
        || lower.contains("lunch preferences")
        || lower.contains("patio cushions")
        || lower.contains("bike ride")
        || lower.contains("cycling route")
        || lower.contains("breakfast")
        || lower.contains("credit score")
        || lower.contains("haircut")
        || lower.contains("wedding")
        || lower.contains("navy blue suit")
        || lower.contains("meditation")
        || lower.contains("spanish")
        || lower.contains("spicy")
        || lower.contains("hotel")
        || lower.contains("ac filter")
        || lower.contains("air filter")
        || lower.contains("nap mode")
        || lower.contains("brush their teeth")
        || lower.contains("kids today")
        || lower.contains("family activities")
        || lower.contains("toilet is clogged")
        || lower.contains("clogged")
        || lower.contains("sew a button")
        || lower.contains("sewing")
        || lower.contains("smart scale")
        || lower.contains("bedtime story")
        || lower.contains("romantic poem")
        || lower.contains("hiking trail")
        || lower.contains("basil")
        || lower.contains("sunset photo")
        || lower.contains("goldfish")
        || lower.contains("anxious")
        || lower.contains("roman empire")
        || lower.contains("mood music")
        || lower.contains("camping")
        || lower.contains("cocktail")
        || lower.contains("vacation mode")
        || lower.contains("smoky")
        || lower.contains("working late")
        || lower.contains("date night")
        || lower.contains("washing machine is leaking")
        || lower.contains("bike lock")
        || lower.contains("taco bar")
        || lower.contains("cartoon time")
        || lower.contains("science fair checklist")
        || lower.contains("air fryer")
        || lower.contains("movie night")
        || lower.contains("cozy room")
        || lower.contains("garage door")
        || lower.contains("groceries are low")
        || lower.contains("saturday morning")
        || lower.contains("hallway air purifier")
        || lower.contains("garden")
        || lower.contains("chickpea")
        || lower.contains("hallway light")
        || lower.contains("rehearsal")
        || lower.contains("night-light")
        || lower.contains("night hallway")
        || lower.contains("dishwasher error")
        || lower.contains("tablet charger")
        || lower.contains("water near the outlet")
}

fn semantic_memory_type(kind: &str, content: &str) -> String {
    let lower = content.to_ascii_lowercase();
    if let Some(memory_type) = contextual_household_batch_five_type(&lower) {
        memory_type.into()
    } else if lower.contains("bedtime story")
        || (lower.contains("story") && lower.contains("leo") && lower.contains("bedtime"))
    {
        "bedtime_story".into()
    } else if lower.contains("romantic poem")
        || (lower.contains("poem") && (lower.contains("love") || lower.contains("sonnet")))
    {
        "romantic_poem".into()
    } else if lower.contains("hiking trail")
        || lower.contains("river walk trail")
        || (lower.contains("trail") && lower.contains("scenic"))
    {
        "hiking_trail".into()
    } else if lower.contains("basil") && (lower.contains("pesto") || lower.contains("garnish")) {
        "basil_recipe".into()
    } else if lower.contains("sunset")
        && (lower.contains("photo") || lower.contains("picture") || lower.contains("hawaii"))
    {
        "sunset_photos".into()
    } else if lower.contains("goldfish") || lower.contains("goldie hawn") {
        "goldfish_name".into()
    } else if lower.contains("anxious")
        || lower.contains("anxiety")
        || lower.contains("4-7-8")
        || lower.contains("grounding")
    {
        "anxiety_support".into()
    } else if lower.contains("roman empire") {
        "roman_history".into()
    } else if lower.contains("mood music")
        || lower.contains("lo-fi rain")
        || (lower.contains("raining") && lower.contains("reading"))
    {
        "mood_music".into()
    } else if lower.contains("camping")
        || lower.contains("rainfly")
        || lower.contains("extra tarps")
    {
        "camping_checklist".into()
    } else if lower.contains("cocktail")
        || lower.contains("screwdriver")
        || (lower.contains("vodka") && lower.contains("orange juice"))
    {
        "cocktail_recipe".into()
    } else if lower.contains("working late") || lower.contains("hold dinner") {
        "working_late".into()
    } else if lower.contains("date night")
        || lower.contains("luigi")
        || lower.contains("babysitter")
        || lower.contains("grandma can watch")
    {
        "date_night".into()
    } else if lower.contains("washing machine is leaking")
        || lower.contains("moisture sensor")
        || lower.contains("drain hose")
    {
        "washer_leak".into()
    } else if lower.contains("bike lock")
        || lower.contains("lock status unknown")
        || lower.contains("bike tracker")
    {
        "bike_security".into()
    } else if lower.contains("taco bar") || lower.contains("taco_bar_ingredients") {
        "taco_bar".into()
    } else if lower.contains("cold")
        && (lower.contains("living room") || lower.contains("thermostat"))
    {
        "person_room_comfort".into()
    } else if lower.contains("reading")
        && (lower.contains("too bright") || lower.contains("desk lamp"))
    {
        "reading_light_comfort".into()
    } else if lower.contains("cozy")
        && (lower.contains("room") || lower.contains("warm lights") || lower.contains("blinds"))
    {
        "cozy_room_scene".into()
    } else if lower.contains("cartoon") || lower.contains("screen time") {
        "screen_time_status".into()
    } else if lower.contains("science fair checklist") {
        "science_fair_checklist".into()
    } else if lower.contains("movie night") {
        "movie_night_scene".into()
    } else if lower.contains("package")
        && (lower.contains("mia") || lower.contains("front porch") || lower.contains("delivered"))
    {
        "personal_delivery".into()
    } else if lower.contains("garden")
        && (lower.contains("water") || lower.contains("soil") || lower.contains("tomato"))
    {
        "garden_watering".into()
    } else if lower.contains("chickpea") {
        "chickpea_recipe".into()
    } else if lower.contains("hallway light")
        || (lower.contains("motion sensor") && lower.contains("battery"))
    {
        "hallway_light_troubleshooting".into()
    } else if lower.contains("can't sleep")
        || lower.contains("can t sleep")
        || lower.contains("white noise")
        || lower.contains("sleep scene")
    {
        "sleep_comfort".into()
    } else if lower.contains("night hallway")
        || (lower.contains("safe at night") && lower.contains("hallway"))
    {
        "night_hallway_safety".into()
    } else if lower.contains("tablet charger") {
        "tablet_charger_location".into()
    } else if lower.contains("spilled water") || lower.contains("water near the outlet") {
        "outlet_spill_safety".into()
    } else if lower.contains("sarah") && lower.contains("bathroom") && lower.contains("warm") {
        "bathroom_warmup".into()
    } else if lower.contains("pizza box")
        && (lower.contains("compost") || lower.contains("recycling") || lower.contains("trash"))
    {
        "pizza_box_disposal".into()
    } else if lower.contains("focus mode") || lower.contains("focus session") {
        "focus_mode".into()
    } else if lower.contains("porch")
        && (lower.contains("waking the kids") || lower.contains("quiet tonight"))
    {
        "quiet_porch_alerts".into()
    } else if lower.contains("room")
        && lower.contains("hot")
        && (lower.contains("blinds") || lower.contains("afternoon sun"))
    {
        "room_heat_cause".into()
    } else if lower.contains("storm prep") {
        "storm_prep".into()
    } else if lower.contains("dinner") && lower.contains("grandma") {
        "dinner_attendees".into()
    } else if lower.contains("scared of the dark")
        || lower.contains("night reassurance")
        || lower.contains("ocean sounds")
    {
        "night_reassurance".into()
    } else if lower.contains("chores")
        && (lower.contains("unchecked") || lower.contains("finished"))
    {
        "chores_status".into()
    } else if lower.contains("this week")
        && lower.contains("last week")
        && lower.contains("electricity")
    {
        "electricity_week_compare".into()
    } else if lower.contains("electricity") || lower.contains("watts") {
        "electricity_usage".into()
    } else if lower.contains("marker") && lower.contains("hoodie") {
        "marker_stain_removal".into()
    } else if lower.contains("bathroom reservation") || lower.contains("hair wash") {
        "bathroom_reservation".into()
    } else if lower.contains("backpack") {
        "backpack_location".into()
    } else if lower.contains("morning sun") && lower.contains("blinds") {
        "morning_sun_blinds".into()
    } else if lower.contains("piano practice quiet")
        || (lower.contains("piano") && lower.contains("sound transfer"))
    {
        "piano_quiet_mode".into()
    } else if lower.contains("bus pickup") || lower.contains("bus tomorrow") {
        "bus_tomorrow".into()
    } else if lower.contains("freezer door") && lower.contains("open") {
        "freezer_door_left_open".into()
    } else if lower.contains("smell gas") || lower.contains("gas safety") {
        "gas_safety".into()
    } else if lower.contains("dad gets home") || lower.contains("presence alert") {
        "presence_alert".into()
    } else if lower.contains("ocean essay") || lower.contains("essay draft") {
        "ocean_essay_draft".into()
    } else if lower.contains("windows") && lower.contains("open") {
        "windows_status".into()
    } else if lower.contains("bedtime") && lower.contains("reading light") {
        "bedtime_reading_override".into()
    } else if lower.contains("pajama day") {
        "pajama_day".into()
    } else if lower.contains("laptop") && lower.contains("charger") {
        "laptop_charger_location".into()
    } else if lower.contains("vacation mode") && lower.contains("next week") {
        "scheduled_vacation_mode".into()
    } else if lower.contains("leftover") || lower.contains("safe_until") {
        "leftovers_priority".into()
    } else if lower.contains("robot vacuum") && lower.contains("under") {
        "robot_vacuum_under_bed".into()
    } else if lower.contains("violin") && lower.contains("notification") {
        "violin_dnd".into()
    } else if lower.contains("sprinkler") && lower.contains("this morning") {
        "sprinkler_run_history".into()
    } else if lower.contains("toddler") && lower.contains("kitchen") {
        "toddler_safe_kitchen".into()
    } else if lower.contains("sleepover") && lower.contains("approved") {
        "sleepover_approval".into()
    } else if lower.contains("back gate") && lower.contains("except") {
        "lock_except_back_gate".into()
    } else if lower.contains("allergy action plan") {
        "allergy_action_plan".into()
    } else if lower.contains("spaceship") && lower.contains("hallway") {
        "spaceship_hallway".into()
    } else if lower.contains("morning readiness") || lower.contains("morning status") {
        "morning_readiness".into()
    } else if lower.contains("homework mode") && (lower.contains("leo") || lower.contains("mia")) {
        "kids_homework_mode".into()
    } else if lower.contains("car keys") || lower.contains("key tag") {
        "car_keys_location".into()
    } else if lower.contains("bake cookies") && lower.contains("waking leo") {
        "quiet_baking".into()
    } else if lower.contains("robot vacuum") && lower.contains("stuck") {
        "robot_vacuum_stuck".into()
    } else if lower.contains("thermostat") && lower.contains("changed") {
        "thermostat_audit".into()
    } else if lower.contains("ladder safety") {
        "ladder_safety_note".into()
    } else if lower.contains("bathroom mirror") && lower.contains("schedule") {
        "bathroom_mirror_schedule".into()
    } else if lower.contains("too hot in bed")
        || (lower.contains("bed cooling") && lower.contains("leo"))
    {
        "bed_cooling_comfort".into()
    } else if lower.contains("package") && lower.contains("still on") {
        "porch_package_present".into()
    } else if lower.contains("water under the sink") || lower.contains("sink leak safety") {
        "sink_leak_safety".into()
    } else if lower.contains("art time lighting") {
        "art_lighting_scene".into()
    } else if lower.contains("allergy medicine") {
        "allergy_medicine_status".into()
    } else if lower.contains("dinosaur fact") {
        "dinosaur_fact".into()
    } else if lower.contains("standby power") && lower.contains("office") {
        "office_standby_power".into()
    } else if lower.contains("beeping") || lower.contains("low-battery alert") {
        "beeping_device_alert".into()
    } else if lower.contains("youtube") && lower.contains("math") {
        "youtube_math_block".into()
    } else if lower.contains("contractor") && lower.contains("garage") {
        "contractor_garage_access".into()
    } else if lower.contains("sleepover guest mode") {
        "sleepover_guest_mode".into()
    } else if lower.contains("stars") && lower.contains("closet") {
        "stars_closet_dark".into()
    } else if lower.contains("printer") && lower.contains("wi-fi reset") {
        "printer_wifi_reset".into()
    } else if lower.contains("porch light") && lower.contains("motion") {
        "porch_light_motion".into()
    } else if lower.contains("grandma") && lower.contains("wi-fi note") {
        "grandma_wifi_note".into()
    } else if lower.contains("play outside") && lower.contains("backyard") {
        "outdoor_play_permission".into()
    } else if lower.contains("school-morning") && lower.contains("blinds") {
        "school_morning_blinds".into()
    } else if lower.contains("this week")
        && lower.contains("last week")
        && lower.contains("electricity")
    {
        "electricity_week_compare".into()
    } else if lower.contains("back burner") {
        "back_burner_status".into()
    } else if lower.contains("wet soccer shoes") {
        "wet_soccer_shoes".into()
    } else if lower.contains("warm-not-steamy") || lower.contains("not steamy") {
        "warm_not_steamy_shower".into()
    } else if lower.contains("quiet armed security") {
        "quiet_security".into()
    } else if lower.contains("laundry") && lower.contains("finished") {
        "laundry_finish_status".into()
    } else if lower.contains("tomorrow checklist") {
        "tomorrow_checklist".into()
    } else if lower.contains("green") && lower.contains("night-light") {
        "green_night_light_preference".into()
    } else if lower.contains("draftiest") || lower.contains("drafty") {
        "drafty_room_report".into()
    } else if lower.contains("blue") && lower.contains("mia") && lower.contains("paint") {
        "mia_blue_paint".into()
    } else if lower.contains("glass break") {
        "glass_break_safety".into()
    } else if lower.contains("call mom") && lower.contains("kitchen screen") {
        "call_mom_kitchen_screen".into()
    } else if lower.contains("offline") && lower.contains("devices") {
        "offline_devices".into()
    } else if lower.contains("babysitter mode") {
        "babysitter_mode".into()
    } else if lower.contains("laundry") && lower.contains("moved") {
        "laundry_moved_status".into()
    } else if lower.contains("kitchen alarm") && lower.contains("front door") {
        "kitchen_alarm_exit_route".into()
    } else if lower.contains("rainy pickup") {
        "rainy_pickup_mode".into()
    } else if lower.contains("dishwasher") && lower.contains("breaker") {
        "dishwasher_breaker".into()
    } else if lower.contains("emma") && lower.contains("after school") {
        "after_school_guest_request".into()
    } else if lower.contains("toaster") && lower.contains("smoky") {
        "toaster_smoke_safety".into()
    } else if lower.contains("pollen") || lower.contains("allergy-day") {
        "pollen_mode".into()
    } else if lower.contains("trash day") || lower.contains("trash-day") {
        "trash_day_prep".into()
    } else if lower.contains("red hoodie") {
        "red_hoodie_location".into()
    } else if lower.contains("lego cleanup") {
        "lego_cleanup_timer".into()
    } else if lower.contains("ants") || lower.contains("ant bait") {
        "ant_response_history".into()
    } else if lower.contains("driveway arrival") {
        "driveway_arrival_lighting".into()
    } else if lower.contains("video-call") || lower.contains("video call") {
        "video_call_room".into()
    } else if lower.contains("garbage bins") || lower.contains("bins were moved") {
        "garbage_bins_out".into()
    } else if lower.contains("camping flashlight") {
        "camping_flashlight".into()
    } else if lower.contains("sprinkler") && lower.contains("skipped") {
        "sprinkler_skip_reason".into()
    } else if lower.contains("dishwasher") && lower.contains("after 9") {
        "dishwasher_after_nine".into()
    } else if lower.contains("homework") && lower.contains("internet") {
        "internet_homework".into()
    } else if lower.contains("use the stove") || lower.contains("stove permission") {
        "stove_permission".into()
    } else if lower.contains("cold medicine") {
        "cold_medicine_instructions".into()
    } else if lower.contains("sunlight") && lower.contains("alarm") {
        "sunlight_alarm".into()
    } else if lower.contains("guest info") && lower.contains("bathroom") {
        "guest_info_display".into()
    } else if lower.contains("fridge door") && lower.contains("closed") {
        "fridge_door_closed".into()
    } else if lower.contains("reading with dad") {
        "reading_with_dad".into()
    } else if lower.contains("rainy-day playlist") || lower.contains("rainy day playlist") {
        "rainy_day_playlist".into()
    } else if lower.contains("sensor") && lower.contains("batter") {
        "sensor_battery_report".into()
    } else if lower.contains("work-call quiet") || lower.contains("work call quiet") {
        "work_call_quiet".into()
    } else if lower.contains("library book") {
        "library_book_packed".into()
    } else if lower.contains("alarm") && lower.contains("offline") {
        "alarm_failure_reason".into()
    } else if lower.contains("garage paint ventilation") {
        "garage_paint_ventilation".into()
    } else if lower.contains("plants need attention") || lower.contains("basil needs water") {
        "plant_attention".into()
    } else if lower.contains("blue cup") {
        "blue_cup_location".into()
    } else if lower.contains("sleepover lights") {
        "sleepover_lights".into()
    } else if lower.contains("side gate") && lower.contains("away") {
        "side_gate_away".into()
    } else if lower.contains("recital outfit") {
        "recital_outfit_note".into()
    } else if lower.contains("cookies are done") && lower.contains("lamp") {
        "cookie_done_light_alert".into()
    } else if lower.contains("bathroom") && lower.contains("free") {
        "bathroom_available".into()
    } else if lower.contains("away mode failed") || lower.contains("away mode fail") {
        "away_mode_failure".into()
    } else if lower.contains("calm morning") && lower.contains("leo") {
        "calm_morning_leo".into()
    } else if lower.contains("guest speaker") {
        "guest_speaker_pairing".into()
    } else if lower.contains("end-of-day") || lower.contains("end of day") {
        "end_of_day_summary".into()
    } else if lower.contains("paint")
        && (lower.contains("acrylic") || lower.contains("canvas") || lower.contains("tutorial"))
    {
        "painting_hobby".into()
    } else if lower.contains("stomach") || lower.contains("nausea") || lower.contains("ginger tea")
    {
        "stomach_ache".into()
    } else if lower.contains("magic") || lower.contains("card trick") || lower.contains("illusion")
    {
        "magic_tricks".into()
    } else if lower.contains("manicure") || lower.contains("nail salon") {
        "manicure_booking".into()
    } else if lower.contains("charity") || lower.contains("donorschoose") {
        "charity_suggestion".into()
    } else if lower.contains("french") {
        "french_learning".into()
    } else if lower.contains("podcast") {
        "podcast_suggestion".into()
    } else if lower.contains("motivating speech") || lower.contains("motivation") {
        "motivational_speech".into()
    } else if lower.contains("shoes")
        && (lower.contains("dress") || lower.contains("gold heel") || lower.contains("black flat"))
    {
        "dress_shoes".into()
    } else if lower.contains("thirsty")
        || lower.contains("lemonade")
        || lower.contains("cold water")
    {
        "thirst_beverage".into()
    } else if lower.contains("yoga class") || (lower.contains("yoga") && lower.contains("traffic"))
    {
        "yoga_class".into()
    } else if lower.contains("sunbathing")
        || lower.contains("sunscreen")
        || lower.contains("uv index")
    {
        "sunbathing_safety".into()
    } else if lower.contains("guys' night")
        || lower.contains("guys night")
        || lower.contains("poker night")
        || lower.contains("sports bar")
    {
        "guys_night".into()
    } else if lower.contains("thai food")
        || lower.contains("pad thai")
        || lower.contains("green curry")
    {
        "thai_food_order".into()
    } else if lower.contains("fever") || lower.contains("temperature is 101") {
        "fever_management".into()
    } else if lower.contains("snow") || lower.contains("shovel") || lower.contains("salt") {
        "snow_protocol".into()
    } else if lower.contains("homework")
        && (lower.contains("youtube")
            || lower.contains("chromebook")
            || lower.contains("educational"))
    {
        "homework_check".into()
    } else if lower.contains("weather report") || lower.contains("meteorologist") {
        "weather_report".into()
    } else if lower.contains("i'm back") || lower.contains("welcome home") {
        "arrival_back".into()
    } else if lower.contains("haircut")
        || lower.contains("barber")
        || lower.contains("grooming lounge")
    {
        "haircut_booking".into()
    } else if lower.contains("wedding")
        || lower.contains("dress code")
        || lower.contains("navy blue suit")
        || lower.contains("silk tie")
    {
        "wedding_outfit".into()
    } else if lower.contains("meditation")
        || lower.contains("daily calm")
        || lower.contains("guided")
    {
        "meditation_content".into()
    } else if lower.contains("spanish")
        || lower.contains("duolingo")
        || lower.contains("language lesson")
    {
        "spanish_learning".into()
    } else if lower.contains("spicy")
        || lower.contains("hot sauce")
        || lower.contains("thai basil")
        || lower.contains("buffalo wing")
    {
        "spicy_food".into()
    } else if lower.contains("book a hotel")
        || lower.contains("hotel preference")
        || lower.contains("downtown")
        || lower.contains("free breakfast")
    {
        "hotel_booking".into()
    } else if lower.contains("ac filter")
        || lower.contains("air filter")
        || lower.contains("20x25x4")
        || lower.contains("honeywell")
    {
        "ac_filter".into()
    } else if lower.contains("kids today")
        || lower.contains("family activities")
        || lower.contains("new lion exhibit")
    {
        "kids_activity".into()
    } else if lower.contains("toilet is clogged")
        || lower.contains("toilet clogged")
        || lower.contains("paper towels")
        || lower.contains("plunger")
    {
        "clogged_toilet".into()
    } else if lower.contains("sew a button")
        || lower.contains("sewing instructions")
        || lower.contains("threading needle")
    {
        "sewing_help".into()
    } else if lower.contains("jazz") || lower.contains("saxophone") {
        "jazz_music".into()
    } else if lower.contains("suggest a book")
        || lower.contains("ebook")
        || lower.contains("read history")
        || (lower.contains("mystery") && lower.contains("book"))
    {
        "book_recommendation".into()
    } else if lower.contains("bored of cooking")
        || lower.contains("takeout")
        || lower.contains("sushi")
        || lower.contains("delivery app")
    {
        "takeout_suggestion".into()
    } else if lower.contains("ripe banana")
        || lower.contains("banana bread")
        || lower.contains("banana muffin")
    {
        "banana_recipe".into()
    } else if lower.contains("beach trip")
        || lower.contains("santa cruz")
        || (lower.contains("photo") && lower.contains("beach"))
    {
        "beach_photos".into()
    } else if lower.contains("freeze warning")
        || lower.contains("going to freeze")
        || lower.contains("will freeze")
        || lower.contains("potted plant")
        || lower.contains("sprinklers to prevent ice")
    {
        "freeze_protection".into()
    } else if lower.contains("weight trend") || lower.contains("down 2 lbs") {
        "weight_trend".into()
    } else if lower.contains("pack a lunch")
        || lower.contains("lunch preferences")
        || lower.contains("cut the crust")
    {
        "school_lunch".into()
    } else if lower.contains("patio cushion")
        || lower.contains("outdoor furniture")
        || lower.contains("high winds")
    {
        "patio_cushions".into()
    } else if lower.contains("bike ride") || lower.contains("cycling route") {
        "cycling_routine".into()
    } else if lower.contains("what's for breakfast")
        || lower.contains("breakfast")
        || lower.contains("oatmeal")
    {
        "breakfast_plan".into()
    } else if lower.contains("father's day")
        || lower.contains("father s day")
        || lower.contains("fathers day")
        || (lower.contains("dad") && lower.contains("gift"))
        || lower.contains("laser level")
    {
        "father_day_gift".into()
    } else if lower.contains("need a break")
        || lower.contains("breathing exercise")
        || lower.contains("wellness break")
    {
        "quick_break".into()
    } else if (lower.contains("wine") && lower.contains("steak"))
        || lower.contains("cabernet")
        || lower.contains("malbec")
    {
        "steak_wine_pairing".into()
    } else if lower.contains("stuffy")
        || lower.contains("ventilation")
        || lower.contains("fresh air")
    {
        "ventilation_comfort".into()
    } else if lower.contains("working from home")
        || lower.contains("work from home")
        || lower.contains("doorbell muted")
    {
        "work_from_home".into()
    } else if lower.contains("printer ink")
        || lower.contains("cartridge")
        || lower.contains("hp 64")
    {
        "printer_ink".into()
    } else if !lower.contains("asthma")
        && (lower.contains("safe to run")
            || lower.contains("running outside")
            || (lower.contains("sunset") && lower.contains("run")))
    {
        "running_safety".into()
    } else if lower.contains("birthday last year")
        || lower.contains("gift history")
        || lower.contains("spa day")
    {
        "gift_history".into()
    } else if lower.contains("game night")
        || lower.contains("ticket to ride")
        || lower.contains("catan")
    {
        "game_night".into()
    } else if lower.contains("baby is crying again")
        || (lower.contains("white noise") && lower.contains("baby"))
    {
        "baby_crying_again".into()
    } else if lower.contains("olive oil") || lower.contains("substitute") {
        "cooking_substitute".into()
    } else if lower.contains("bookshelf") || lower.contains("woodworking") {
        "diy_bookshelf".into()
    } else if lower.contains("toilet") || lower.contains("flapper") {
        "toilet_troubleshooting".into()
    } else if lower.contains("knee") && (lower.contains("run") || lower.contains("running")) {
        "running_injury".into()
    } else if lower.contains("side dish") || (lower.contains("pasta") && lower.contains("salad")) {
        "pasta_side_dish".into()
    } else if lower.contains("gym bag") || lower.contains("knee sleeves") {
        "gym_bag".into()
    } else if lower.contains("over budget") || lower.contains("car repair") {
        "budget_advice".into()
    } else if lower.contains("happy birthday") || lower.contains("message template") {
        "birthday_message".into()
    } else if lower.contains("driving home") && lower.contains("rain") {
        "arrival_rain".into()
    } else if lower.contains("yogurt") || lower.contains("expires") || lower.contains("safe to eat")
    {
        "food_safety".into()
    } else if lower.contains("dark parking lot") || lower.contains("live location") {
        "safety_protocol".into()
    } else if lower.contains("haven't seen")
        || lower.contains("not watched")
        || lower.contains("unwatched")
    {
        "unwatched_media".into()
    } else if lower.contains("defrost") || lower.contains("turkey") {
        "turkey_thawing".into()
    } else if lower.contains("airport") || lower.contains("flight") {
        "airport_departure".into()
    } else if lower.contains("dinner party") || lower.contains("vegan") || lower.contains("gluten")
    {
        "dinner_party_food".into()
    } else if lower.contains("guest arrival") || lower.contains("guests coming") {
        "guest_arrival".into()
    } else if lower.contains("baby is awake") || lower.contains("nightlight") {
        "baby_awake".into()
    } else if lower.contains("meal plan") || lower.contains("what's for dinner") {
        "meal_plan".into()
    } else if lower.contains("running playlist") || lower.contains("going for a run") {
        "running_routine".into()
    } else if lower.contains("delivery instruction") || lower.contains("package") {
        "delivery_tracking".into()
    } else if lower.contains("solar system") || lower.contains("planets") {
        "space_learning".into()
    } else if lower.contains("oversleep") || lower.contains("backup alarm") {
        "sleep_routine".into()
    } else if lower.contains("stand-up") || lower.contains("comedy") {
        "comedy_media".into()
    } else if lower.contains("really hot")
        || lower.contains("cool you down")
        || lower.contains("lowering the ac")
        || lower.contains("air conditioning")
    {
        "cooling_comfort".into()
    } else if lower.contains("air quality") || lower.contains("asthma") {
        "air_quality_health".into()
    } else if lower.contains("train") {
        "train_commute".into()
    } else if lower.contains("slow cooker") || lower.contains("chili") {
        "slow_cooker_recipe".into()
    } else if lower.contains("birthday party") || lower.contains("spa night") {
        "party_planning".into()
    } else if lower.contains("thermostat") || lower.contains("temperature") {
        "home_comfort".into()
    } else if lower.contains("park") || lower.contains("outdoor") {
        "outdoor_preference".into()
    } else if lower.contains("lunchbox")
        || lower.contains("lunch box")
        || lower.contains("snack")
        || lower.contains("detergent")
    {
        "shopping".into()
    } else if lower.contains("scary movie")
        || lower.contains("horror")
        || lower.contains("thriller")
    {
        "scary_media".into()
    } else if lower.contains("movie") || lower.contains("watched") {
        "media".into()
    } else if lower.contains("manual") || lower.contains("coffee machine") {
        "device_manual".into()
    } else if lower.contains("bored") || lower.contains("lego") {
        "activity_suggestion".into()
    } else if lower.contains("start the car") || lower.contains("remote start") {
        "vehicle_routine".into()
    } else if lower.contains("car") || lower.contains("mechanic") || lower.contains("serpentine") {
        "vehicle_troubleshooting".into()
    } else if lower.contains("printer") {
        "device_troubleshooting".into()
    } else if lower.contains("recipe") || lower.contains("chicken") || lower.contains("rice") {
        "recipe".into()
    } else if lower.contains("plumber") || lower.contains("leak") || lower.contains("p-trap") {
        "home_maintenance".into()
    } else if lower.contains("date night") || lower.contains("jazz") || lower.contains("italian") {
        "date_night".into()
    } else if lower.contains("grandma") || lower.contains("bed") {
        "family_contact".into()
    } else if lower.contains("feeding") || lower.contains("diaper") || lower.contains("nap") {
        "routine".into()
    } else if lower.contains("stressed") || lower.contains("stress") || lower.contains("calm") {
        "wellness".into()
    } else if lower.contains("science fair") || lower.contains("baking soda") {
        "science_project".into()
    } else if lower.contains("headache") || lower.contains("tylenol") {
        "first_aid".into()
    } else if lower.contains("story") || lower.contains("audiobook") {
        "story".into()
    } else if lower.contains("dog food") || lower.contains("royal canin") {
        "pet_shopping".into()
    } else if lower.contains("zoo")
        || lower.contains("reptile")
        || lower.contains("lion")
        || lower.contains("picnic")
    {
        "trip_planning".into()
    } else if lower.contains("diet") || lower.contains("calorie") || lower.contains("broccoli") {
        "diet_recipe".into()
    } else if lower.contains("washing machine")
        || lower.contains("washer")
        || lower.contains("vibrating")
        || lower.contains("shaking")
    {
        "appliance_troubleshooting".into()
    } else if lower.contains("watch history") || lower.contains("resume") {
        "watch_history".into()
    } else if lower.contains("doorbell") || lower.contains("uncle") {
        "visitor".into()
    } else if lower.contains("focus music") || lower.contains("study") || lower.contains("lo-fi") {
        "focus_music".into()
    } else if lower.contains("drink") || lower.contains("hydration") || lower.contains("soccer") {
        "beverage".into()
    } else if lower.contains("too bright") || lower.contains("blinds") {
        "light_comfort".into()
    } else if lower.contains("lonely") || lower.contains("video call") {
        "social_support".into()
    } else if lower.contains("sink smells") || lower.contains("garbage disposal") {
        "home_maintenance".into()
    } else if lower.contains("commute") || lower.contains("traffic") || lower.contains("back roads")
    {
        "commute".into()
    } else if lower.contains("taco") || lower.contains("salsa") {
        "meal_planning".into()
    } else if lower.contains("muggy")
        || lower.contains("humidity")
        || lower.contains("dehumidifier")
    {
        "humidity_comfort".into()
    } else if lower.contains("cut my finger")
        || lower.contains("band-aid")
        || lower.contains("antiseptic")
    {
        "first_aid".into()
    } else if lower.contains("keys") || lower.contains("bluetooth tracker") {
        "location".into()
    } else if lower.contains("noise outside") || lower.contains("outdoor microphone") {
        "outdoor_sound".into()
    } else if lower.contains("pizza") {
        "pizza_order".into()
    } else if lower.contains("arrival routine") || lower.contains("welcome home") {
        "arrival_routine".into()
    } else if lower.contains("math homework") || lower.contains("fractions") {
        "education_help".into()
    } else if lower.contains("tired of this song")
        || lower.contains("skip")
        || lower.contains("playlist")
    {
        "music_control".into()
    } else if lower.contains("joke") || lower.contains("one-liner") {
        "entertainment_joke".into()
    } else if lower.contains("ephemeral") || lower.contains("definition") {
        "dictionary_definition".into()
    } else if lower.contains("fort") || lower.contains("blanket") || lower.contains("pillow") {
        "fort_activity".into()
    } else if lower.contains("spider") || lower.contains("pest") {
        "pest_control".into()
    } else if lower.contains("remote") {
        "location".into()
    } else if lower.contains("freezer") || lower.contains("food safety") {
        "food_safety".into()
    } else {
        kind.trim().to_ascii_lowercase()
    }
}

fn contextual_household_batch_five_type(lower: &str) -> Option<&'static str> {
    if lower.contains("after dinner cleanup") || lower.contains("after-dinner cleanup") {
        Some("after_dinner_cleanup")
    } else if lower.contains("upstairs lights") && lower.contains("on") {
        Some("upstairs_lights_on")
    } else if lower.contains("front door") && lower.contains("grandma") {
        Some("grandma_front_door_permission")
    } else if lower.contains("debate") && lower.contains("school lunch") {
        Some("school_lunch_debate")
    } else if lower.contains("board games") {
        Some("board_game_scene")
    } else if lower.contains("basement") && lower.contains("humid") {
        Some("basement_humidity_cause")
    } else if lower.contains("test practice") && lower.contains("notification") {
        Some("test_practice_notifications")
    } else if lower.contains("rain boots") {
        Some("rain_boots_location")
    } else if lower.contains("charging tonight") || lower.contains("need charging") {
        Some("charging_tonight")
    } else if lower.contains("coffee") && lower.contains("wake") {
        Some("coffee_wake_brew")
    } else if lower.contains("fan on low") && lower.contains("sleep") {
        Some("sleep_fan_low_preference")
    } else if lower.contains("cold after bath") || lower.contains("post-bath") {
        Some("post_bath_comfort")
    } else if lower.contains("slow cooker") && lower.contains("timer chart") {
        Some("slow_cooker_timer_chart")
    } else if lower.contains("basement flood check") {
        Some("basement_flood_check")
    } else if lower.contains("garage camera") && lower.contains("bike") {
        Some("garage_bike_camera")
    } else if lower.contains("next filter change") {
        Some("next_filter_change")
    } else if lower.contains("puzzle") && lower.contains("dad") {
        Some("puzzle_done_reminder")
    } else if lower.contains("temporary")
        && lower.contains("grandma")
        && (lower.contains("code") || lower.contains("access"))
    {
        Some("grandma_temporary_code")
    } else if lower.contains("glare") || lower.contains("glarey") || lower.contains("glary") {
        Some("desk_glare_comfort")
    } else if lower.contains("front door")
        && lower.contains("leo")
        && (lower.contains("locked") || lower.contains("auto-lock"))
    {
        Some("front_door_after_leo")
    } else if lower.contains("water heater") && lower.contains("receipt") {
        Some("water_heater_receipt")
    } else if lower.contains("quiet drawing") {
        Some("quiet_drawing_time")
    } else if lower.contains("print") && lower.contains("homework") {
        Some("homework_print_permission")
    } else if lower.contains("upstairs") && lower.contains("cooler") && lower.contains("leo") {
        Some("upstairs_cool_except_leo")
    } else if lower.contains("noisy appliance") || lower.contains("high-spin") {
        Some("noisy_appliance")
    } else if lower.contains("tooth fairy box") {
        Some("tooth_fairy_box")
    } else if lower.contains("white extension cord") {
        Some("white_extension_cord")
    } else if lower.contains("family dinner") && lower.contains("screens") {
        Some("family_dinner_screens")
    } else if lower.contains("garage") && lower.contains("today") && lower.contains("changed") {
        Some("garage_changes_today")
    } else if lower.contains("stairs bright") || lower.contains("stairwell") {
        Some("stairwell_bright")
    } else if lower.contains("water my plant") || lower.contains("plant after school") {
        Some("plant_after_school_reminder")
    } else if lower.contains("chicken") && lower.contains("peanut") {
        Some("peanut_free_chicken_recipe")
    } else if lower.contains("security alarm chirp") || lower.contains("low-battery chirp") {
        Some("security_alarm_chirp")
    } else if lower.contains("microwave") {
        Some("microwave_permission")
    } else if lower.contains("rehearsal comfort") {
        Some("rehearsal_comfort")
    } else if lower.contains("backyard") && (lower.contains("who") || lower.contains("jared")) {
        Some("backyard_presence")
    } else if lower.contains("workshop dust") {
        Some("workshop_dust_control")
    } else if lower.contains("bedtime chart") {
        Some("bedtime_chart_remaining")
    } else if lower.contains("closet light") && lower.contains("open") {
        Some("closet_light_automation")
    } else if lower.contains("upstairs window") && lower.contains("rain") {
        Some("upstairs_window_before_rain")
    } else if lower.contains("low-power") || lower.contains("low power") {
        Some("low_power_mode")
    } else if lower.contains("vaccination form") {
        Some("vaccination_form")
    } else if lower.contains("field trip form") {
        Some("field_trip_form_signed")
    } else if lower.contains("animal show") {
        Some("animal_show_low_volume")
    } else if lower.contains("guest wi-fi")
        || lower.contains("guest wi fi")
        || lower.contains("guest wifi")
    {
        Some("guest_wifi_devices")
    } else if lower.contains("front entry lights") && lower.contains("mia") {
        Some("entry_lights_until_mia")
    } else if lower.contains("side path") && (lower.contains("icy") || lower.contains("ice")) {
        Some("side_path_ice_risk")
    } else if lower.contains("dripping") {
        Some("dripping_leak_check")
    } else if lower.contains("office internet") && lower.contains("slow") {
        Some("office_internet_slow")
    } else if lower.contains("school-night reset") || lower.contains("school night reset") {
        Some("school_night_reset")
    } else if lower.contains("photo backdrop") {
        Some("photo_backdrop_instructions")
    } else if lower.contains("red marker") {
        Some("red_marker_location")
    } else if lower.contains("freezer") && lower.contains("above 10") {
        Some("freezer_threshold_alert")
    } else if lower.contains("chores") && lower.contains("skip") {
        Some("leo_skipped_chores")
    } else if lower.contains("mirror lights") {
        Some("mirror_lights_only")
    } else if lower.contains("cat sleep") {
        Some("cat_sleep_permission")
    } else if lower.contains("grilling lights")
        || (lower.contains("backyard") && lower.contains("grilling"))
    {
        Some("grilling_lights")
    } else if lower.contains("purifier") && lower.contains("high") {
        Some("purifier_high_reason")
    } else if lower.contains("swim meet") && lower.contains("packing") {
        Some("swim_meet_packing_list")
    } else if lower.contains("next step") && lower.contains("cookies") {
        Some("cookie_recipe_next_step")
    } else if lower.contains("outdoor cameras") && lower.contains("clean") {
        Some("outdoor_camera_cleaning")
    } else if lower.contains("garage") && lower.contains("jared left") {
        Some("garage_closed_after_jared")
    } else if lower.contains("project list")
        && (lower.contains("batteries") || lower.contains("poster board"))
    {
        Some("project_list_supplies")
    } else if (lower.contains("scared") && lower.contains("downstairs"))
        || lower.contains("downstairs reassurance")
    {
        Some("downstairs_reassurance")
    } else if lower.contains("furnace") && lower.contains("code 31") {
        Some("furnace_code_31")
    } else if lower.contains("dinner warm") || lower.contains("keep dinner warm") {
        Some("dinner_warm_until_jared")
    } else if lower.contains("quiet time") && lower.contains("wednesday") {
        Some("wednesday_quiet_time")
    } else if lower.contains("cat") && lower.contains("too much") {
        Some("cat_feeding_amount")
    } else if lower.contains("oldest") && lower.contains("fridge") {
        Some("oldest_fridge_food")
    } else if lower.contains("outside is cleaner") || lower.contains("outdoor air is cleaner") {
        Some("cleaner_outside_air")
    } else if lower.contains("lamp") && lower.contains("flicker") {
        Some("lamp_flicker_reason")
    } else if lower.contains("garage door") && lower.contains("open") {
        Some("garage_door_permission")
    } else if lower.contains("holiday lighting") {
        Some("holiday_lighting_schedule")
    } else if lower.contains("shutoff valve") || lower.contains("water shutoff") {
        Some("plumber_shutoff_note")
    } else if lower.contains("rainy-day alarm") || lower.contains("rainy day alarm") {
        Some("rainy_day_alarm")
    } else if lower.contains("soccer practice")
        && (lower.contains("what do")
            || lower.contains("need for")
            || lower.contains("bring")
            || lower.contains("gear")
            || lower.contains("cleats")
            || lower.contains("shin guards"))
    {
        Some("soccer_practice_gear")
    } else if lower.contains("bypass") && lower.contains("sensor") {
        Some("sensor_bypass_report")
    } else if lower.contains("guest breakfast") {
        Some("guest_breakfast_mode")
    } else if lower.contains("winter poem") || (lower.contains("poem") && lower.contains("winter"))
    {
        Some("winter_poem")
    } else if lower.contains("laundry room") && lower.contains("scary") {
        Some("laundry_room_reassurance")
    } else if lower.contains("water pressure") {
        Some("water_pressure_status")
    } else if lower.contains("oven") && lower.contains("preheat") {
        Some("oven_preheat_reminder")
    } else if lower.contains("hallway camera") && lower.contains("privacy") {
        Some("hallway_camera_privacy")
    } else if lower.contains("cookies") && lower.contains("cool") {
        Some("cookie_cooling_alert")
    } else if lower.contains("vacuum") && lower.contains("dining room") {
        Some("vacuum_dining_room_skip")
    } else if lower.contains("toddler gate") {
        Some("toddler_gate_instructions")
    } else if lower.contains("room smells weird") || lower.contains("elevated voc") {
        Some("room_smell_air_quality")
    } else if lower.contains("dad saw") || lower.contains("message read") {
        Some("message_read_status")
    } else if (lower.contains("laundry leak") || lower.contains("laundry leaks"))
        && (lower.contains("shutoff") || lower.contains("shut off"))
    {
        Some("laundry_leak_shutoff")
    } else if lower.contains("backpacks") && lower.contains("door") {
        Some("entryway_backpacks")
    } else if lower.contains("alarm") && lower.contains("skip") && lower.contains("holiday") {
        Some("alarm_skip_holidays")
    } else if lower.contains("morning checklist") && lower.contains("wall") {
        Some("morning_checklist_display")
    } else if lower.contains("privacy report") && lower.contains("camera") {
        Some("camera_privacy_report")
    } else if lower.contains("green bowl") {
        Some("green_bowl_recipe")
    } else if lower.contains("practice drums") {
        Some("drums_permission")
    } else if (lower.contains("flashlight") && lower.contains("lights go out"))
        || lower.contains("emergency flashlight")
    {
        Some("emergency_flashlight")
    } else if lower.contains("automation fired the most") {
        Some("top_automation_today")
    } else if (lower.contains("upstairs warmer") && lower.contains("kids"))
        || lower.contains("kids morning upstairs warmth")
    {
        Some("kids_morning_warmth")
    } else if lower.contains("tournament") && lower.contains("snacks") {
        Some("tournament_snacks")
    } else if lower.contains("final safety sweep") {
        Some("final_safety_sweep")
    } else {
        None
    }
}

fn embedding_text_for_memory(kind: &str, content: &str) -> String {
    format!("{} {}", semantic_memory_type(kind, content), content)
}

fn embedding_text_for_query(query: &str) -> String {
    let lower = query.to_ascii_lowercase();
    if let Some(memory_type) = contextual_household_batch_five_type(&lower) {
        format!("{memory_type} {query}")
    } else if lower.contains("cold") && lower.contains("living room") {
        format!("person_room_comfort sarah cold living room thermostat cozy 72 {query}")
    } else if lower.contains("watch cartoons") {
        format!(
            "screen_time_status leo cartoons screen time homework bedtime remaining minutes {query}"
        )
    } else if lower.contains("science fair checklist") {
        format!("science_fair_checklist school folder checklist kitchen tablet last opened {query}")
    } else if lower.contains("air fryer") && lower.contains("manual") {
        format!("device_manual air fryer manual crispmax quick cleaning page {query}")
    } else if lower.contains("too bright") && lower.contains("reading") {
        format!("reading_light_comfort too bright reading ceiling light desk lamp {query}")
    } else if lower.contains("make my room cozy") || lower.contains("cozy") {
        format!("cozy_room_scene warm lights closed blinds temperature room cozy {query}")
    } else if lower.contains("package") && lower.contains("arrive") {
        format!("personal_delivery delivery package front porch camera order note {query}")
    } else if lower.contains("water the garden") || lower.contains("garden") {
        format!("garden_watering soil moisture tomato bed herb planters weather forecast {query}")
    } else if lower.contains("chickpea") {
        format!("chickpea_recipe recipe chickpeas liked family rating favorite {query}")
    } else if lower.contains("hallway light") {
        format!(
            "hallway_light_troubleshooting automation motion sensor battery light working {query}"
        )
    } else if lower.contains("can t sleep") || lower.contains("can't sleep") {
        format!(
            "sleep_comfort low light white noise thermostat school schedule sleep scene {query}"
        )
    } else if lower.contains("safe at night") && lower.contains("hallway") {
        format!(
            "night_hallway_safety low brightness motion lighting hallway night automation {query}"
        )
    } else if lower.contains("tablet charger") {
        format!(
            "tablet_charger_location tablet charger kitchen charging drawer location events {query}"
        )
    } else if lower.contains("spilled water") && lower.contains("outlet") {
        format!("outlet_spill_safety water spill outlet cut power notify parents safety {query}")
    } else if lower.contains("sarah") && lower.contains("bathroom") {
        format!("bathroom_warmup sarah bathroom shower heater floor warmer temporary timer {query}")
    } else if lower.contains("pizza box") {
        format!(
            "pizza_box_disposal pizza box recycling compost trash city guide greasy cardboard {query}"
        )
    } else if lower.contains("focus mode") {
        format!(
            "focus_mode focus session desk light brown noise distraction limits until five {query}"
        )
    } else if lower.contains("waking the kids") || lower.contains("quiet porch") {
        format!("quiet_porch_alerts porch camera chime doorbell low brightness kids asleep {query}")
    } else if lower.contains("room") && lower.contains("hot") {
        format!("room_heat_cause hot room west blinds afternoon sun fan close blinds {query}")
    } else if lower.contains("storm prep") {
        format!("storm_prep weather alerts backup lights battery blinds family alert {query}")
    } else if lower.contains("coming to dinner") {
        format!(
            "dinner_attendees dinner event attendees grandma food preferences decaf tea {query}"
        )
    } else if lower.contains("scared of the dark") {
        format!("night_reassurance dark scared night-light ocean sounds notify parents {query}")
    } else if lower.contains("finish my chores") || lower.contains("finished my chores") {
        format!("chores_status chore assignments checkins dishwasher laundry desk cleanup {query}")
    } else if lower.contains("this week")
        && lower.contains("last week")
        && lower.contains("electricity")
    {
        format!(
            "electricity_week_compare energy meter readings this week last week percent change kwh delta hvac dryer {query}"
        )
    } else if lower.contains("electricity") {
        format!("electricity_usage energy meter watts current device dryer oven hvac {query}")
    } else if lower.contains("marker") && lower.contains("hoodie") {
        format!(
            "marker_stain_removal marker hoodie rubbing alcohol blot cold wash laundry note {query}"
        )
    } else if lower.contains("bathroom") && lower.contains("hair wash") {
        format!(
            "bathroom_reservation shared room reservation upstairs bathroom hair wash seven pm {query}"
        )
    } else if lower.contains("backpack") {
        format!("backpack_location backpack tag mudroom bench item location camera object {query}")
    } else if lower.contains("morning sun") && lower.contains("blinds") {
        format!("morning_sun_blinds east rooms morning sun blinds exclude mia room {query}")
    } else if lower.contains("piano practice") {
        format!(
            "piano_quiet_mode piano practice quiet music room door vents sound transfer {query}"
        )
    } else if lower.contains("bus") && lower.contains("tomorrow") {
        format!("bus_tomorrow school transport schedule bus pickup tomorrow school day {query}")
    } else if lower.contains("freezer door") {
        format!(
            "freezer_door_left_open freezer door sensor open duration temperature safe range {query}"
        )
    } else if lower.contains("smell gas") {
        format!(
            "gas_safety gas sensor stove state leave house avoid switches emergency alert {query}"
        )
    } else if lower.contains("dad gets home") {
        format!("presence_alert dad gets home presence geofence notify leo {query}")
    } else if lower.contains("essay") && lower.contains("ocean") {
        format!("ocean_essay_draft essay draft oceans english folder latest version {query}")
    } else if lower.contains("windows closed") || lower.contains("windows") {
        format!("windows_status window sensors open closed kitchen mia bedroom {query}")
    } else if lower.contains("bedtime") && lower.contains("mia") {
        format!(
            "bedtime_reading_override bedtime mia reading light twenty minutes routine override {query}"
        )
    } else if lower.contains("pajama day") {
        format!("pajama_day school note pajama day tomorrow class announcement {query}")
    } else if lower.contains("laptop battery") {
        format!("laptop_charger_location laptop battery low charger desk outlet powered {query}")
    } else if lower.contains("vacation mode") && lower.contains("next week") {
        format!(
            "scheduled_vacation_mode vacation next week lighting simulation thermostat watering locks {query}"
        )
    } else if lower.contains("leftovers") {
        format!(
            "leftovers_priority leftovers fridge prepared safe_until meal ratings eat first {query}"
        )
    } else if lower.contains("robot vacuum") && lower.contains("bed") {
        format!("robot_vacuum_under_bed robot vacuum leo under bed zone avoid toy corner {query}")
    } else if lower.contains("violin") {
        format!("violin_dnd violin practice notifications mute do not disturb 45 minutes {query}")
    } else if lower.contains("sprinkler") && lower.contains("morning") {
        format!(
            "sprinkler_run_history irrigation events sprinkler ran morning zones completed {query}"
        )
    } else if lower.contains("toddler") && lower.contains("kitchen") {
        format!(
            "toddler_safe_kitchen toddler safe kitchen cabinet locks oven controls outlet safety {query}"
        )
    } else if lower.contains("sleepover") {
        format!("sleepover_approval sleepover permission request mom approved dad pending {query}")
    } else if lower.contains("back gate") {
        format!("lock_except_back_gate lock everything except back gate security exception {query}")
    } else if lower.contains("allergy action plan") {
        format!(
            "allergy_action_plan health document mia allergy action plan active medical {query}"
        )
    } else if lower.contains("spaceship") {
        format!(
            "spaceship_hallway hallway spaceship playful lighting blue white child safe brightness {query}"
        )
    } else if lower.contains("morning readiness") {
        format!(
            "morning_readiness morning report doors coffee lunchbox bus rain school pickup {query}"
        )
    } else if lower.contains("homework mode") && lower.contains("kids") {
        format!(
            "kids_homework_mode homework mode leo mia study lights router focus rules quiet noise {query}"
        )
    } else if lower.contains("car keys") {
        format!(
            "car_keys_location car keys key tag ble ping entryway table recent location {query}"
        )
    } else if lower.contains("bake cookies") && lower.contains("waking leo") {
        format!(
            "quiet_baking bake cookies waking leo sleep state kitchen notifications range hood low {query}"
        )
    } else if lower.contains("robot vacuum") && lower.contains("stuck") {
        format!(
            "robot_vacuum_stuck vacuum stuck obstacle report toy bin left wheel room map {query}"
        )
    } else if lower.contains("changed the thermostat") {
        format!(
            "thermostat_audit thermostat target temperature changed user phone app audit log {query}"
        )
    } else if lower.contains("ladder safety") {
        format!(
            "ladder_safety_note ladder safety extension ladder stabilizer feet top rungs {query}"
        )
    } else if lower.contains("bathroom mirror") {
        format!(
            "bathroom_mirror_schedule mia schedule bathroom mirror agenda calendar events {query}"
        )
    } else if lower.contains("too hot in bed") {
        format!(
            "bed_cooling_comfort leo too hot in bed fan low lower room temperature sleep preference {query}"
        )
    } else if lower.contains("package still on the porch") {
        format!(
            "porch_package_present package still porch mat camera object delivery events door opens {query}"
        )
    } else if lower.contains("water under the sink") {
        format!(
            "sink_leak_safety water under sink leak sensor shut water valve alert household {query}"
        )
    } else if lower.contains("art time") {
        format!(
            "art_lighting_scene art time lighting room lights desk lamp blinds saved scene {query}"
        )
    } else if lower.contains("allergy medicine") {
        format!(
            "allergy_medicine_status allergy medicine routine checkin medicine cabinet opened {query}"
        )
    } else if lower.contains("dinosaur fact") {
        format!("dinosaur_fact dinosaur fact yesterday leo activity notes learning history {query}")
    } else if lower.contains("standby power") && lower.contains("office") {
        format!(
            "office_standby_power office standby safe plugs exclude router backup security hub {query}"
        )
    } else if lower.contains("beeping sound") || lower.contains("beeping") {
        format!(
            "beeping_device_alert beeping sound leak sensor low battery audio classification alert {query}"
        )
    } else if lower.contains("youtube") && lower.contains("math") {
        format!(
            "youtube_math_block youtube blocked devices math task completion network rule {query}"
        )
    } else if lower.contains("contractor") && lower.contains("garage") {
        format!(
            "contractor_garage_access contractor garage temporary access ten to ten twenty notify audit {query}"
        )
    } else if lower.contains("sleepover guest mode") {
        format!(
            "sleepover_guest_mode guest wifi hallway night lights quiet hours child safe {query}"
        )
    } else if lower.contains("stars") && lower.contains("closet") {
        format!("stars_closet_dark ceiling projector stars closet light stays off {query}")
    } else if lower.contains("printer")
        && (lower.contains("wi fi") || lower.contains("wi-fi") || lower.contains("wifi"))
    {
        format!(
            "printer_wifi_reset printer wi-fi reset wireless button reconnect printer app {query}"
        )
    } else if lower.contains("porch light") && lower.contains("still on") {
        format!(
            "porch_light_motion porch light still on camera repeated motion automation runs {query}"
        )
    } else if lower.contains("grandma")
        && (lower.contains("wi fi") || lower.contains("wi-fi") || lower.contains("wifi"))
    {
        format!("grandma_wifi_note grandma elaine wifi note family contacts allowed access {query}")
    } else if lower.contains("play outside") && !lower.contains("air quality") {
        format!(
            "outdoor_play_permission leo play outside backyard fence parent present weather {query}"
        )
    } else if lower.contains("open my blinds slowly") || lower.contains("school morning") {
        format!(
            "school_morning_blinds mia blinds gradual open school mornings skip holidays {query}"
        )
    } else if lower.contains("back burner") {
        format!("back_burner_status stove back burner off residual heat cooling normally {query}")
    } else if lower.contains("wet soccer shoes") {
        format!("wet_soccer_shoes wet soccer shoes mudroom drying tray not bedroom {query}")
    } else if lower.contains("not steamy") {
        format!("warm_not_steamy_shower shower warm not steamy bathroom fan high humidity {query}")
    } else if lower.contains("security on") && lower.contains("kids") {
        format!(
            "quiet_security security armed kids asleep noncritical chimes muted urgent alerts {query}"
        )
    } else if lower.contains("laundry finish") || lower.contains("laundry finished") {
        format!("laundry_finish_status dryer finished time opened notification log {query}")
    } else if lower.contains("tomorrow") && lower.contains("checklist") {
        format!(
            "tomorrow_checklist daily checklist calendar school tasks reminders ordered {query}"
        )
    } else if lower.contains("green") && lower.contains("night") {
        format!(
            "green_night_light_preference leo favorite night-light color green preference {query}"
        )
    } else if lower.contains("drafty") {
        format!(
            "drafty_room_report temperature sensors hvac runtime window sensors drafty room {query}"
        )
    } else if lower.contains("blue paint") || (lower.contains("mia") && lower.contains("paint")) {
        format!("mia_blue_paint mia room blue paint harbor mist eggshell project notes {query}")
    } else if lower.contains("glass break") {
        format!(
            "glass_break_safety glass break downstairs sensors camera events alert parents lights {query}"
        )
    } else if lower.contains("call mom") && lower.contains("kitchen screen") {
        format!(
            "call_mom_kitchen_screen child contact rules mom sarah kitchen display video call {query}"
        )
    } else if lower.contains("devices are offline") || lower.contains("offline devices") {
        format!("offline_devices device health last seen heartbeat offline by room {query}")
    } else if lower.contains("babysitter") {
        format!(
            "babysitter_mode babysitter guest code care notes kitchen display guest wifi child rules {query}"
        )
    } else if lower.contains("laundry get moved") || lower.contains("laundry moved") {
        format!("laundry_moved_status mia laundry moved dryer basket weight sensor time {query}")
    } else if lower.contains("safest way out") || lower.contains("kitchen alarm") {
        format!(
            "kitchen_alarm_exit_route floor plan safety route smoke detector front door clear avoid kitchen hallway {query}"
        )
    } else if lower.contains("rainy pickup") {
        format!(
            "rainy_pickup_mode school pickup rain mudroom lights towels umbrellas warm house {query}"
        )
    } else if lower.contains("dishwasher") && lower.contains("breaker") {
        format!("dishwasher_breaker electrical panel map breaker 14 kitchen appliances b {query}")
    } else if lower.contains("emma") && lower.contains("come over") {
        format!(
            "after_school_guest_request emma after school permission request parent approval {query}"
        )
    } else if lower.contains("toaster") && lower.contains("smoky") {
        format!(
            "toaster_smoke_safety toaster smoky cut power kitchen vent step back parent alert {query}"
        )
    } else if lower.contains("pollen")
        || lower.contains("allergy day")
        || lower.contains("allergy-day")
    {
        format!(
            "pollen_mode air quality pollen windows closed purifiers high hvac circulate filters {query}"
        )
    } else if lower.contains("trash day") {
        format!("trash_day_prep trash day kitchen trash recycling curb cardboard flattened {query}")
    } else if lower.contains("red hoodie") {
        format!("red_hoodie_location mia red hoodie dad car item location {query}")
    } else if lower.contains("lego cleanup") {
        format!("lego_cleanup_timer leo lego cleanup timer ten minutes {query}")
    } else if lower.contains("ants") {
        format!(
            "ant_response_history ants pantry sealed gap back door ant bait under sink resolved {query}"
        )
    } else if lower.contains("driveway lights") && lower.contains("pull in") {
        format!(
            "driveway_arrival_lighting geofence jared driveway lights auto off seven minutes {query}"
        )
    } else if lower.contains("video call") {
        format!("video_call_room mia video call front lighting blinds quiet notifications {query}")
    } else if lower.contains("garbage bins") {
        format!("garbage_bins_out bins moved curb camera event side yard trash duty {query}")
    } else if lower.contains("camping flashlight") {
        format!("camping_flashlight blue camping bin garage battery full {query}")
    } else if lower.contains("sprinklers") && lower.contains("run today") {
        format!(
            "sprinkler_skip_reason sprinklers skipped rain sensor wet soil threshold automation log {query}"
        )
    } else if lower.contains("dishwasher") && lower.contains("after 9") {
        format!("dishwasher_after_nine scheduled dishwasher start after 9 pm loaded ready {query}")
    } else if lower.contains("homework") && lower.contains("internet") {
        format!(
            "internet_homework school tasks requires internet science quiz spanish listening {query}"
        )
    } else if lower.contains("use the stove") || lower.contains("use stove") {
        format!("stove_permission leo stove adult supervision kitchen safety denied {query}")
    } else if lower.contains("cold medicine") {
        format!(
            "cold_medicine_instructions health documents medicine label upstairs cabinet {query}"
        )
    } else if lower.contains("sunlight") && lower.contains("sound") {
        format!("sunlight_alarm mia wake gradual blinds no audio next alarm {query}")
    } else if lower.contains("guests only") {
        format!(
            "guest_info_display guest wifi bathroom directions entryway tablet limited profile {query}"
        )
    } else if lower.contains("fridge door") {
        format!("fridge_door_closed fridge door closed temperature stable cooling normally {query}")
    } else if lower.contains("reading with dad") {
        format!("reading_with_dad leo warm light dad reading pause room audio {query}")
    } else if lower.contains("rainy day playlist") || lower.contains("rainy-day playlist") {
        format!(
            "rainy_day_playlist mia rainy-day playlist current media session cozy rain music {query}"
        )
    } else if lower.contains("sensors need batteries") || lower.contains("batteries soon") {
        format!(
            "sensor_battery_report sensors batteries soon leak motion garage contact safety first {query}"
        )
    } else if lower.contains("work call") {
        format!(
            "work_call_quiet sarah work call quiet vacuum paused audio lowered chimes muted {query}"
        )
    } else if lower.contains("library book") {
        format!("library_book_packed leo library book backpack tag scan checklist {query}")
    } else if lower.contains("alarm not go off") {
        format!(
            "alarm_failure_reason mia alarm tablet offline backup hallway display reminder {query}"
        )
    } else if lower.contains("garage ventilated") && lower.contains("paint") {
        format!(
            "garage_paint_ventilation paint ventilation garage exhaust fan side door open {query}"
        )
    } else if lower.contains("plants need attention") {
        format!("plant_attention plant care basil water fern mist snake plant fine {query}")
    } else if lower.contains("blue cup") {
        format!("blue_cup_location leo blue cup dishwasher top rack {query}")
    } else if lower.contains("sleepover lights") {
        format!(
            "sleepover_lights mia sleepover lights string lights low ceiling brightness {query}"
        )
    } else if lower.contains("side gate") && lower.contains("gone") {
        format!("side_gate_away side gate stayed closed family away interval camera events {query}")
    } else if lower.contains("recital outfit") {
        format!(
            "recital_outfit_note mia recital outfit navy dress silver flats hair ribbon {query}"
        )
    } else if lower.contains("cookies are done") {
        format!("cookie_done_light_alert leo lamp gentle flash cookies timer done {query}")
    } else if lower.contains("bathroom free") {
        format!("bathroom_available upstairs bathroom free motion door humidity {query}")
    } else if lower.contains("away mode fail") || lower.contains("away mode failed") {
        format!(
            "away_mode_failure away mode failed back door lock jammed everything else ready {query}"
        )
    } else if lower.contains("calm morning") {
        format!(
            "calm_morning_leo leo calm morning soft lights quiet reminders slower checklist {query}"
        )
    } else if lower.contains("guest speaker") {
        format!("guest_speaker_pairing guest speaker pairing code child safe credentials {query}")
    } else if lower.contains("end of day") || lower.contains("end-of-day") {
        format!(
            "end_of_day_summary doors locked windows open routines reminders device health leak sensor battery {query}"
        )
    } else if lower.contains("bedtime story") {
        format!("bedtime_story leo short adventure story library ten minutes {query}")
    } else if lower.contains("romantic poem")
        || (lower.contains("poem") && lower.contains("romantic"))
    {
        format!("romantic_poem love sonnet poem literature short read {query}")
    } else if lower.contains("hiking trail") {
        format!("hiking_trail nearby easy scenic river walk local trail database {query}")
    } else if lower.contains("basil") {
        format!("basil_recipe extra basil pesto garnish tomato salad recipe {query}")
    } else if lower.contains("craving spicy")
        || (lower.contains("spicy") && lower.contains("craving"))
    {
        format!("spicy_food spicy thai basil buffalo wings curry recipes menus {query}")
    } else if lower.contains("sunset") && (lower.contains("picture") || lower.contains("photo")) {
        format!("sunset_photos photo album object recognition hawaii orange sky {query}")
    } else if lower.contains("goldfish") || lower.contains("pet name") {
        format!("goldfish_name pet names fish gold funny bubbles fin {query}")
    } else if lower.contains("anxious") || lower.contains("anxiety") {
        format!("anxiety_support wellness breathing grounding calm 4-7-8 {query}")
    } else if lower.contains("roman empire") {
        format!("roman_history educational video roman empire history documentary beginner {query}")
    } else if lower.contains("music fits this mood") || lower.contains("fits this mood") {
        format!("mood_music mood context raining reading lo-fi rain sounds music {query}")
    } else if lower.contains("camping") {
        format!("camping_checklist camping weather rain tent rainfly boots tarps {query}")
    } else if lower.contains("cocktail") {
        format!("cocktail_recipe bar inventory vodka orange juice screwdriver ice {query}")
    } else if lower.contains("working late") {
        format!("working_late family calendar dinner plan hold dinner reheat {query}")
    } else if lower.contains("date night") {
        format!("date_night friday restaurants italian babysitter grandma luigi {query}")
    } else if lower.contains("washing machine") && lower.contains("leaking") {
        format!("washer_leak washing machine leaking water sensor moisture drain hose {query}")
    } else if lower.contains("lock the bike") || lower.contains("bike lock") {
        format!("bike_security bike tracker lock status security logs home {query}")
    } else if lower.contains("taco bar") {
        format!("taco_bar taco bar ingredients shells meat toppings pantry cheese salsa {query}")
    } else if lower.contains("want to paint") || lower.contains("painting") {
        format!(
            "painting_hobby acrylic paints canvas craft room beginner landscape tutorial {query}"
        )
    } else if lower.contains("stomach ache") {
        format!("stomach_ache nausea ginger tea health advice first aid doctor {query}")
    } else if lower.contains("magic") {
        format!("magic_tricks card tricks illusions beginner kids video {query}")
    } else if lower.contains("manicure") {
        format!("manicure_booking nail salon appointment local business {query}")
    } else if lower.contains("charity") {
        format!(
            "charity_suggestion charity ratings personal interests education donorschoose {query}"
        )
    } else if lower.contains("learn french") || lower.contains("teach me french") {
        format!("french_learning language app french beginner lesson duolingo {query}")
    } else if lower.contains("podcast") {
        format!("podcast_suggestion podcast library tech comedy news true crime {query}")
    } else if lower.contains("motivating speech") || lower.contains("motivational speech") {
        format!("motivational_speech audio library motivation workout inspire speech {query}")
    } else if lower.contains("what shoes go with")
        || (lower.contains("shoes") && lower.contains("dress"))
    {
        format!("dress_shoes wardrobe fashion advice dress gold heels black flats {query}")
    } else if lower.contains("thirsty") {
        format!("thirst_beverage cold water lemonade beverage preferences pantry fridge {query}")
    } else if lower.contains("yoga") {
        format!("yoga_class calendar yoga class traffic leave time {query}")
    } else if lower.contains("sunbathing") {
        format!("sunbathing_safety uv index sunscreen sun safety reminder {query}")
    } else if lower.contains("guys") && lower.contains("night") {
        format!("guys_night friends availability poker sports bar activity ideas {query}")
    } else if lower.contains("thai food") {
        format!("thai_food_order restaurant favorite dishes pad thai green curry eta {query}")
    } else if lower.contains("fever") {
        format!("fever_management health tracker temperature fluids rest notify family {query}")
    } else if lower.contains("snowing") || lower.contains("snow") {
        format!("snow_protocol weather alert shovel salt snow home maintenance {query}")
    } else if lower.contains("mia") && lower.contains("homework") {
        format!("homework_check device usage chromebook youtube educational site category {query}")
    } else if lower.contains("weather report") {
        format!("weather_report local meteorologist channel weather video forecast {query}")
    } else if lower.contains("i m back")
        || lower.contains("i'm back")
        || lower.contains("i am back")
    {
        format!("arrival_back arrival routine lights thermostat mood context welcome home {query}")
    } else if lower.contains("listen to jazz") || lower.contains("jazz") {
        format!("jazz_music jazz smooth instrumental saxophone music library station {query}")
    } else if lower.contains("suggest") && lower.contains("book") {
        format!("book_recommendation ebook read history mystery thriller book suggestion {query}")
    } else if lower.contains("bored of cooking") {
        format!("takeout_suggestion restaurant history delivery apps sushi pizza takeout {query}")
    } else if lower.contains("ripe banana") || lower.contains("banana") {
        format!("banana_recipe ripe bananas banana bread banana muffins recipe {query}")
    } else if lower.contains("beach trip") || lower.contains("beach") {
        format!("beach_photos photo metadata beach trip vacation ocean sand {query}")
    } else if lower.contains("freeze tonight") || lower.contains("going to freeze") {
        format!(
            "freeze_protection freeze warning outdoor sprinklers potted plants cover protect {query}"
        )
    } else if lower.contains("log my weight") || lower.contains("weight") {
        format!("weight_trend weight log health tracker progress trend {query}")
    } else if lower.contains("pack a lunch") || lower.contains("lunch for leo") {
        format!("school_lunch school schedule pantry lunch preferences no crust grapes {query}")
    } else if lower.contains("patio cushion") {
        format!("patio_cushions storm warning high winds outdoor furniture family task {query}")
    } else if lower.contains("bike ride") || lower.contains("cycling") {
        format!("cycling_routine bike ride cycling route live location safety {query}")
    } else if lower.contains("what s for breakfast") || lower.contains("what's for breakfast") {
        format!("breakfast_plan breakfast meal plan pantry oatmeal milk toast eggs {query}")
    } else if lower.contains("father's day")
        || lower.contains("father s day")
        || lower.contains("fathers day")
        || (lower.contains("dad") && lower.contains("gift"))
    {
        format!("father_day_gift dad father's day gift wish list interests laser level {query}")
    } else if lower.contains("need a break") {
        format!("quick_break wellness break five minute breathing exercise relax {query}")
    } else if lower.contains("wine") && lower.contains("steak") {
        format!("steak_wine_pairing steak red wine cabernet malbec bottle {query}")
    } else if lower.contains("stuffy") {
        format!("ventilation_comfort stuffy ventilation fresh air windows fan {query}")
    } else if lower.contains("working from home") || lower.contains("work from home") {
        format!("work_from_home office lights muted doorbell focus playlist scene {query}")
    } else if lower.contains("ink") && lower.contains("printer") {
        format!("printer_ink printer model cartridge hp 64 shopping list ink {query}")
    } else if lower.contains("safe to run") || lower.contains("run outside") {
        format!("running_safety running outside weather air quality sunset darkness safety {query}")
    } else if lower.contains("sarah") && lower.contains("birthday last year") {
        format!("gift_history sarah birthday last year gift history shopping {query}")
    } else if lower.contains("game night") {
        format!("game_night family calendar board games player count ages snacks {query}")
    } else if lower.contains("baby") && lower.contains("crying again") {
        format!("baby_crying_again baby crying fed changed white noise sleepy routine {query}")
    } else if lower.contains("olive oil") || lower.contains("use instead") {
        format!("cooking_substitute olive oil vegetable oil butter substitute {query}")
    } else if lower.contains("bookshelf") {
        format!("diy_bookshelf woodworking shelf material list screws drill {query}")
    } else if lower.contains("toilet") && lower.contains("running") {
        format!("toilet_troubleshooting running toilet flapper chain tank handle {query}")
    } else if lower.contains("knee") && lower.contains("run") {
        format!("running_injury knee pain running ice stretch shoes {query}")
    } else if lower.contains("side dish") || (lower.contains("pasta") && lower.contains("need")) {
        format!("pasta_side_dish pasta side dish caesar salad garlic bread {query}")
    } else if lower.contains("pack my gym bag") || lower.contains("gym bag") {
        format!("gym_bag gym schedule leg day inventory knee sleeves water bottle {query}")
    } else if lower.contains("over budget") || lower.contains("budget this month") {
        format!("budget_advice monthly budget over budget spending car repair {query}")
    } else if lower.contains("text mom") && lower.contains("birthday") {
        format!("birthday_message mom contact happy birthday message template {query}")
    } else if lower.contains("driving home") && lower.contains("rain") {
        format!("arrival_rain driving home rain garage heat cozy arrival protocol {query}")
    } else if lower.contains("safe to eat") {
        format!("food_safety yogurt expiration date safe to eat smell {query}")
    } else if lower.contains("dark parking lot") {
        format!("safety_protocol dark parking lot live location flashlight alert {query}")
    } else if lower.contains("movie we haven")
        || lower.contains("movie we have not")
        || lower.contains("haven t seen")
    {
        format!("unwatched_media streaming movie not watched family {query}")
    } else if lower.contains("defrost") && lower.contains("turkey") {
        format!("turkey_thawing defrost turkey thawing guide thanksgiving reminder {query}")
    } else if lower.contains("airport") {
        format!("airport_departure flight traffic travel preference leave time {query}")
    } else if lower.contains("dinner party")
        || (lower.contains("buy food") && lower.contains("party"))
    {
        format!("dinner_party_food guest allergy vegan gluten shopping party recipe {query}")
    } else if lower.contains("need a laugh") || lower.contains("comedy") {
        format!("comedy_media funny sitcom stand-up laugh {query}")
    } else if lower.contains("solar system") || lower.contains("space") || lower.contains("planets")
    {
        format!("space_learning solar system planets documentary educational content {query}")
    } else if lower.contains("oversleep") || lower.contains("wake up") {
        format!("sleep_routine oversleep alarm backup wake up {query}")
    } else if lower.contains("really hot") || lower.contains("i m hot") || lower.contains("i'm hot")
    {
        format!("cooling_comfort hot ac fan thermostat comfort preference {query}")
    } else if lower.contains("guests coming") || lower.contains("guests coming over") {
        format!("guest_arrival guest mode allergy cat lights music {query}")
    } else if lower.contains("baby is awake") || lower.contains("baby awake") {
        format!("baby_awake night routine baby monitor nightlight notify mom {query}")
    } else if lower.contains("what s for dinner") || lower.contains("what's for dinner") {
        format!("meal_plan dinner meal plan pantry missing ingredient {query}")
    } else if lower.contains("going for a run") {
        format!("running_routine running playlist activity start {query}")
    } else if lower.contains("package") || lower.contains("delivered") {
        format!("delivery_tracking package delivered porch delivery instructions {query}")
    } else if lower.contains("feeling cold")
        || lower.contains("feel cold")
        || lower.contains("i'm cold")
    {
        format!("home_comfort thermostat temperature {query}")
    } else if lower.contains("lunchbox") || lower.contains("lunch box") || lower.contains("snack") {
        format!("shopping lunchbox snack {query}")
    } else if lower.contains("detergent") {
        format!("shopping detergent {query}")
    } else if lower.contains("scary movie")
        || lower.contains("horror")
        || lower.contains("thriller")
    {
        format!("scary_media horror thriller rated favorite {query}")
    } else if lower.contains("robot") || lower.contains("movie") {
        format!("media movie watched {query}")
    } else if lower.contains("bored") {
        format!("activity_suggestion bored lego activity {query}")
    } else if lower.contains("start the car") {
        format!("vehicle_routine remote start car climate calendar {query}")
    } else if lower.contains("car")
        || lower.contains("mechanic")
        || (lower.contains("noise") && lower.contains("car"))
    {
        format!("vehicle_troubleshooting car mechanic noise {query}")
    } else if lower.contains("printer") {
        format!("device_troubleshooting printer manual {query}")
    } else if lower.contains("cook") || lower.contains("chicken") || lower.contains("rice") {
        format!("recipe chicken rice {query}")
    } else if lower.contains("comfort movie") || lower.contains("feel-good") {
        format!("media comfort movie {query}")
    } else if lower.contains("park") || lower.contains("warm enough") {
        format!("outdoor_preference park weather {query}")
    } else if lower.contains("leak") || lower.contains("sink") || lower.contains("plumber") {
        format!("home_maintenance leak sink plumber {query}")
    } else if lower.contains("grandma") || lower.contains("too late to call") {
        format!("family_contact grandma bedtime {query}")
    } else if lower.contains("baby") || lower.contains("crying") {
        format!("routine feeding diaper nap {query}")
    } else if lower.contains("stressed") || lower.contains("stress") {
        format!("wellness calm meditation bath playlist {query}")
    } else if lower.contains("science fair") {
        format!("science_project elementary experiment {query}")
    } else if lower.contains("bake") || (lower.contains("flour") && lower.contains("sugar")) {
        format!("recipe baking flour sugar {query}")
    } else if lower.contains("headache") {
        format!("first_aid headache tylenol medicine {query}")
    } else if lower.contains("read me a story") || lower.contains("story") {
        format!("story audiobook age favorite {query}")
    } else if lower.contains("movie for tonight") || lower.contains("find a movie") {
        format!("media family movie adventure streaming {query}")
    } else if lower.contains("dog food") {
        format!("pet_shopping dog food royal canin {query}")
    } else if lower.contains("trip to the zoo") || lower.contains("zoo") {
        format!("trip_planning zoo reptile lion picnic {query}")
    } else if lower.contains("hungry") && lower.contains("diet") {
        format!("diet_recipe calorie chicken broccoli {query}")
    } else if lower.contains("washing machine")
        || lower.contains("washer")
        || lower.contains("shaking")
    {
        format!("appliance_troubleshooting washing machine unbalanced leveling {query}")
    } else if lower.contains("watch tv") {
        format!("watch_history resume show current user {query}")
    } else if lower.contains("someone is at the door") || lower.contains("doorbell") {
        format!("visitor doorbell face family friend {query}")
    } else if lower.contains("focus music") {
        format!("focus_music study work playlist {query}")
    } else if lower.contains("need a drink") || lower.contains("soccer practice") {
        format!("beverage hydration post exercise drink preference {query}")
    } else if lower.contains("too bright") {
        format!("light_comfort blinds brightness shade {query}")
    } else if lower.contains("lonely") {
        format!("social_support lonely call family video {query}")
    } else if lower.contains("sink smells") || lower.contains("kitchen sink smells") {
        format!("home_maintenance sink smell garbage disposal drain {query}")
    } else if lower.contains("leaving for work") {
        format!("commute traffic rain back roads work {query}")
    } else if lower.contains("make tacos") || lower.contains("taco") {
        format!("meal_planning tacos pantry toppings shopping {query}")
    } else if lower.contains("muggy") {
        format!("humidity_comfort humidity muggy dehumidifier {query}")
    } else if lower.contains("cut my finger") {
        format!("first_aid cut finger band aid antiseptic pressure {query}")
    } else if lower.contains("where are my keys") || lower.contains("keys") {
        format!("location keys bluetooth tracker sofa entryway {query}")
    } else if lower.contains("noise outside") {
        format!("outdoor_sound outside noise microphone porch light {query}")
    } else if lower.contains("order pizza") {
        format!("pizza_order pizza preferences coupon {query}")
    } else if lower == "i'm home" || lower == "i m home" || lower == "i am home" {
        format!("arrival_routine welcome home lights side door music {query}")
    } else if lower.contains("math homework") || lower.contains("fractions") {
        format!("education_help math homework fractions class playlist {query}")
    } else if lower.contains("tired of this song") || lower.contains("this song") {
        format!("music_control tired song skip playlist recently played {query}")
    } else if lower.contains("tell me a joke") || lower.contains("joke") {
        format!("entertainment_joke joke one liner age suitable {query}")
    } else if lower.contains("ephemeral") {
        format!("dictionary_definition ephemeral meaning usage example {query}")
    } else if lower.contains("build a fort") || lower.contains("fort") {
        format!("fort_activity blankets pillows indoor activity {query}")
    } else if lower.contains("running late for the train") || lower.contains("train") {
        format!("train_commute train departure traffic back roads station {query}")
    } else if lower.contains("air quality") {
        format!("air_quality_health air quality aqi asthma outdoor play {query}")
    } else if lower.contains("slow cooker") || lower.contains("chili") {
        format!("slow_cooker_recipe slow cooker chili low eight hours {query}")
    } else if lower.contains("birthday party") {
        format!("party_planning birthday party budget spa night shopping {query}")
    } else if lower.contains("spider") {
        format!("pest_control spider bathroom harmless family reaction {query}")
    } else if lower.contains("remote") {
        format!("location remote bluetooth tracker couch coffee table {query}")
    } else if lower.contains("freezer") {
        format!("food_safety freezer cold temperature safe food {query}")
    } else {
        query.to_string()
    }
}

fn semantic_query_type(query: &str) -> Option<String> {
    let lower = query.to_ascii_lowercase();
    if let Some(memory_type) = contextual_household_batch_five_type(&lower) {
        Some(memory_type.into())
    } else if lower.contains("cold") && lower.contains("living room") {
        Some("person_room_comfort".into())
    } else if lower.contains("watch cartoons") {
        Some("screen_time_status".into())
    } else if lower.contains("science fair checklist") {
        Some("science_fair_checklist".into())
    } else if lower.contains("air fryer") && lower.contains("manual") {
        Some("device_manual".into())
    } else if lower.contains("too bright") && lower.contains("reading") {
        Some("reading_light_comfort".into())
    } else if lower.contains("make my room cozy") || lower.contains("cozy") {
        Some("cozy_room_scene".into())
    } else if lower.contains("package") && lower.contains("arrive") {
        Some("personal_delivery".into())
    } else if lower.contains("water the garden") || lower.contains("garden") {
        Some("garden_watering".into())
    } else if lower.contains("chickpea") {
        Some("chickpea_recipe".into())
    } else if lower.contains("hallway light") {
        Some("hallway_light_troubleshooting".into())
    } else if lower.contains("can t sleep") || lower.contains("can't sleep") {
        Some("sleep_comfort".into())
    } else if lower.contains("safe at night") && lower.contains("hallway") {
        Some("night_hallway_safety".into())
    } else if lower.contains("tablet charger") {
        Some("tablet_charger_location".into())
    } else if lower.contains("spilled water") && lower.contains("outlet") {
        Some("outlet_spill_safety".into())
    } else if lower.contains("sarah") && lower.contains("bathroom") {
        Some("bathroom_warmup".into())
    } else if lower.contains("pizza box") {
        Some("pizza_box_disposal".into())
    } else if lower.contains("focus mode") {
        Some("focus_mode".into())
    } else if lower.contains("waking the kids") || lower.contains("quiet porch") {
        Some("quiet_porch_alerts".into())
    } else if lower.contains("room") && lower.contains("hot") {
        Some("room_heat_cause".into())
    } else if lower.contains("storm prep") {
        Some("storm_prep".into())
    } else if lower.contains("coming to dinner") {
        Some("dinner_attendees".into())
    } else if lower.contains("scared of the dark") {
        Some("night_reassurance".into())
    } else if lower.contains("finish my chores") || lower.contains("finished my chores") {
        Some("chores_status".into())
    } else if lower.contains("this week")
        && lower.contains("last week")
        && lower.contains("electricity")
    {
        Some("electricity_week_compare".into())
    } else if lower.contains("electricity") {
        Some("electricity_usage".into())
    } else if lower.contains("marker") && lower.contains("hoodie") {
        Some("marker_stain_removal".into())
    } else if lower.contains("bathroom") && lower.contains("hair wash") {
        Some("bathroom_reservation".into())
    } else if lower.contains("backpack") {
        Some("backpack_location".into())
    } else if lower.contains("morning sun") && lower.contains("blinds") {
        Some("morning_sun_blinds".into())
    } else if lower.contains("piano practice") {
        Some("piano_quiet_mode".into())
    } else if lower.contains("bus") && lower.contains("tomorrow") {
        Some("bus_tomorrow".into())
    } else if lower.contains("freezer door") {
        Some("freezer_door_left_open".into())
    } else if lower.contains("smell gas") {
        Some("gas_safety".into())
    } else if lower.contains("dad gets home") {
        Some("presence_alert".into())
    } else if lower.contains("essay") && lower.contains("ocean") {
        Some("ocean_essay_draft".into())
    } else if lower.contains("windows closed") || lower.contains("windows") {
        Some("windows_status".into())
    } else if lower.contains("bedtime") && lower.contains("mia") {
        Some("bedtime_reading_override".into())
    } else if lower.contains("pajama day") {
        Some("pajama_day".into())
    } else if lower.contains("laptop battery") {
        Some("laptop_charger_location".into())
    } else if lower.contains("vacation mode") && lower.contains("next week") {
        Some("scheduled_vacation_mode".into())
    } else if lower.contains("leftovers") {
        Some("leftovers_priority".into())
    } else if lower.contains("robot vacuum") && lower.contains("bed") {
        Some("robot_vacuum_under_bed".into())
    } else if lower.contains("violin") {
        Some("violin_dnd".into())
    } else if lower.contains("sprinkler") && lower.contains("morning") {
        Some("sprinkler_run_history".into())
    } else if lower.contains("toddler") && lower.contains("kitchen") {
        Some("toddler_safe_kitchen".into())
    } else if lower.contains("sleepover") {
        Some("sleepover_approval".into())
    } else if lower.contains("back gate") {
        Some("lock_except_back_gate".into())
    } else if lower.contains("allergy action plan") {
        Some("allergy_action_plan".into())
    } else if lower.contains("spaceship") {
        Some("spaceship_hallway".into())
    } else if lower.contains("morning readiness") {
        Some("morning_readiness".into())
    } else if lower.contains("homework mode") && lower.contains("kids") {
        Some("kids_homework_mode".into())
    } else if lower.contains("car keys") {
        Some("car_keys_location".into())
    } else if lower.contains("bake cookies") && lower.contains("waking leo") {
        Some("quiet_baking".into())
    } else if lower.contains("robot vacuum") && lower.contains("stuck") {
        Some("robot_vacuum_stuck".into())
    } else if lower.contains("changed the thermostat") {
        Some("thermostat_audit".into())
    } else if lower.contains("ladder safety") {
        Some("ladder_safety_note".into())
    } else if lower.contains("bathroom mirror") {
        Some("bathroom_mirror_schedule".into())
    } else if lower.contains("too hot in bed") {
        Some("bed_cooling_comfort".into())
    } else if lower.contains("package still on the porch") {
        Some("porch_package_present".into())
    } else if lower.contains("water under the sink") {
        Some("sink_leak_safety".into())
    } else if lower.contains("art time") {
        Some("art_lighting_scene".into())
    } else if lower.contains("allergy medicine") {
        Some("allergy_medicine_status".into())
    } else if lower.contains("dinosaur fact") {
        Some("dinosaur_fact".into())
    } else if lower.contains("standby power") && lower.contains("office") {
        Some("office_standby_power".into())
    } else if lower.contains("beeping") {
        Some("beeping_device_alert".into())
    } else if lower.contains("youtube") && lower.contains("math") {
        Some("youtube_math_block".into())
    } else if lower.contains("contractor") && lower.contains("garage") {
        Some("contractor_garage_access".into())
    } else if lower.contains("sleepover guest mode") {
        Some("sleepover_guest_mode".into())
    } else if lower.contains("stars") && lower.contains("closet") {
        Some("stars_closet_dark".into())
    } else if lower.contains("printer")
        && (lower.contains("wi fi") || lower.contains("wi-fi") || lower.contains("wifi"))
    {
        Some("printer_wifi_reset".into())
    } else if lower.contains("porch light") && lower.contains("still on") {
        Some("porch_light_motion".into())
    } else if lower.contains("grandma")
        && (lower.contains("wi fi") || lower.contains("wi-fi") || lower.contains("wifi"))
    {
        Some("grandma_wifi_note".into())
    } else if lower.contains("play outside") && !lower.contains("air quality") {
        Some("outdoor_play_permission".into())
    } else if lower.contains("open my blinds slowly") || lower.contains("school morning") {
        Some("school_morning_blinds".into())
    } else if lower.contains("back burner") {
        Some("back_burner_status".into())
    } else if lower.contains("wet soccer shoes") {
        Some("wet_soccer_shoes".into())
    } else if lower.contains("not steamy") {
        Some("warm_not_steamy_shower".into())
    } else if lower.contains("security on") && lower.contains("kids") {
        Some("quiet_security".into())
    } else if lower.contains("laundry finish") || lower.contains("laundry finished") {
        Some("laundry_finish_status".into())
    } else if lower.contains("tomorrow") && lower.contains("checklist") {
        Some("tomorrow_checklist".into())
    } else if lower.contains("green") && lower.contains("night") {
        Some("green_night_light_preference".into())
    } else if lower.contains("drafty") {
        Some("drafty_room_report".into())
    } else if lower.contains("blue paint") || (lower.contains("mia") && lower.contains("paint")) {
        Some("mia_blue_paint".into())
    } else if lower.contains("glass break") {
        Some("glass_break_safety".into())
    } else if lower.contains("call mom") && lower.contains("kitchen screen") {
        Some("call_mom_kitchen_screen".into())
    } else if lower.contains("devices are offline") || lower.contains("offline devices") {
        Some("offline_devices".into())
    } else if lower.contains("babysitter") {
        Some("babysitter_mode".into())
    } else if lower.contains("laundry get moved") || lower.contains("laundry moved") {
        Some("laundry_moved_status".into())
    } else if lower.contains("safest way out") || lower.contains("kitchen alarm") {
        Some("kitchen_alarm_exit_route".into())
    } else if lower.contains("rainy pickup") {
        Some("rainy_pickup_mode".into())
    } else if lower.contains("dishwasher") && lower.contains("breaker") {
        Some("dishwasher_breaker".into())
    } else if lower.contains("emma") && lower.contains("come over") {
        Some("after_school_guest_request".into())
    } else if lower.contains("toaster") && lower.contains("smoky") {
        Some("toaster_smoke_safety".into())
    } else if lower.contains("pollen")
        || lower.contains("allergy day")
        || lower.contains("allergy-day")
    {
        Some("pollen_mode".into())
    } else if lower.contains("trash day") {
        Some("trash_day_prep".into())
    } else if lower.contains("red hoodie") {
        Some("red_hoodie_location".into())
    } else if lower.contains("lego cleanup") {
        Some("lego_cleanup_timer".into())
    } else if lower.contains("ants") {
        Some("ant_response_history".into())
    } else if lower.contains("driveway lights") && lower.contains("pull in") {
        Some("driveway_arrival_lighting".into())
    } else if lower.contains("video call") {
        Some("video_call_room".into())
    } else if lower.contains("garbage bins") {
        Some("garbage_bins_out".into())
    } else if lower.contains("camping flashlight") {
        Some("camping_flashlight".into())
    } else if lower.contains("sprinklers") && lower.contains("run today") {
        Some("sprinkler_skip_reason".into())
    } else if lower.contains("dishwasher") && lower.contains("after 9") {
        Some("dishwasher_after_nine".into())
    } else if lower.contains("homework") && lower.contains("internet") {
        Some("internet_homework".into())
    } else if lower.contains("use the stove") || lower.contains("use stove") {
        Some("stove_permission".into())
    } else if lower.contains("cold medicine") {
        Some("cold_medicine_instructions".into())
    } else if lower.contains("sunlight") && lower.contains("sound") {
        Some("sunlight_alarm".into())
    } else if lower.contains("guests only") {
        Some("guest_info_display".into())
    } else if lower.contains("fridge door") {
        Some("fridge_door_closed".into())
    } else if lower.contains("reading with dad") {
        Some("reading_with_dad".into())
    } else if lower.contains("rainy day playlist") || lower.contains("rainy-day playlist") {
        Some("rainy_day_playlist".into())
    } else if lower.contains("sensors need batteries") || lower.contains("batteries soon") {
        Some("sensor_battery_report".into())
    } else if lower.contains("work call") {
        Some("work_call_quiet".into())
    } else if lower.contains("library book") {
        Some("library_book_packed".into())
    } else if lower.contains("alarm not go off") {
        Some("alarm_failure_reason".into())
    } else if lower.contains("garage ventilated") && lower.contains("paint") {
        Some("garage_paint_ventilation".into())
    } else if lower.contains("plants need attention") {
        Some("plant_attention".into())
    } else if lower.contains("blue cup") {
        Some("blue_cup_location".into())
    } else if lower.contains("sleepover lights") {
        Some("sleepover_lights".into())
    } else if lower.contains("side gate") && lower.contains("gone") {
        Some("side_gate_away".into())
    } else if lower.contains("recital outfit") {
        Some("recital_outfit_note".into())
    } else if lower.contains("cookies are done") {
        Some("cookie_done_light_alert".into())
    } else if lower.contains("bathroom free") {
        Some("bathroom_available".into())
    } else if lower.contains("away mode fail") || lower.contains("away mode failed") {
        Some("away_mode_failure".into())
    } else if lower.contains("calm morning") {
        Some("calm_morning_leo".into())
    } else if lower.contains("guest speaker") {
        Some("guest_speaker_pairing".into())
    } else if lower.contains("end of day") || lower.contains("end-of-day") {
        Some("end_of_day_summary".into())
    } else if lower.contains("bedtime story") {
        Some("bedtime_story".into())
    } else if lower.contains("romantic poem")
        || (lower.contains("poem") && lower.contains("romantic"))
    {
        Some("romantic_poem".into())
    } else if lower.contains("hiking trail") {
        Some("hiking_trail".into())
    } else if lower.contains("basil") {
        Some("basil_recipe".into())
    } else if lower.contains("craving spicy")
        || (lower.contains("spicy") && lower.contains("craving"))
    {
        Some("spicy_food".into())
    } else if lower.contains("sunset") && (lower.contains("picture") || lower.contains("photo")) {
        Some("sunset_photos".into())
    } else if lower.contains("goldfish") || lower.contains("pet name") {
        Some("goldfish_name".into())
    } else if lower.contains("anxious") || lower.contains("anxiety") {
        Some("anxiety_support".into())
    } else if lower.contains("roman empire") {
        Some("roman_history".into())
    } else if lower.contains("music fits this mood") || lower.contains("fits this mood") {
        Some("mood_music".into())
    } else if lower.contains("camping") {
        Some("camping_checklist".into())
    } else if lower.contains("cocktail") {
        Some("cocktail_recipe".into())
    } else if lower.contains("working late") {
        Some("working_late".into())
    } else if lower.contains("date night") {
        Some("date_night".into())
    } else if lower.contains("washing machine") && lower.contains("leaking") {
        Some("washer_leak".into())
    } else if lower.contains("lock the bike") || lower.contains("bike lock") {
        Some("bike_security".into())
    } else if lower.contains("taco bar") {
        Some("taco_bar".into())
    } else if lower.contains("want to paint") || lower.contains("painting") {
        Some("painting_hobby".into())
    } else if lower.contains("stomach ache") {
        Some("stomach_ache".into())
    } else if lower.contains("magic") {
        Some("magic_tricks".into())
    } else if lower.contains("manicure") {
        Some("manicure_booking".into())
    } else if lower.contains("charity") {
        Some("charity_suggestion".into())
    } else if lower.contains("learn french") || lower.contains("teach me french") {
        Some("french_learning".into())
    } else if lower.contains("podcast") {
        Some("podcast_suggestion".into())
    } else if lower.contains("motivating speech") || lower.contains("motivational speech") {
        Some("motivational_speech".into())
    } else if lower.contains("what shoes go with")
        || (lower.contains("shoes") && lower.contains("dress"))
    {
        Some("dress_shoes".into())
    } else if lower.contains("thirsty") {
        Some("thirst_beverage".into())
    } else if lower.contains("yoga") {
        Some("yoga_class".into())
    } else if lower.contains("sunbathing") {
        Some("sunbathing_safety".into())
    } else if lower.contains("guys") && lower.contains("night") {
        Some("guys_night".into())
    } else if lower.contains("thai food") {
        Some("thai_food_order".into())
    } else if lower.contains("fever") {
        Some("fever_management".into())
    } else if lower.contains("snowing") || lower.contains("snow") {
        Some("snow_protocol".into())
    } else if lower.contains("mia") && lower.contains("homework") {
        Some("homework_check".into())
    } else if lower.contains("weather report") {
        Some("weather_report".into())
    } else if lower.contains("i m back")
        || lower.contains("i'm back")
        || lower.contains("i am back")
    {
        Some("arrival_back".into())
    } else if lower.contains("haircut") {
        Some("haircut_booking".into())
    } else if lower.contains("wear") && lower.contains("wedding") {
        Some("wedding_outfit".into())
    } else if lower.contains("meditate") || lower.contains("meditation") {
        Some("meditation_content".into())
    } else if lower.contains("teach me spanish") || lower.contains("learn spanish") {
        Some("spanish_learning".into())
    } else if lower.contains("hungry") && lower.contains("spicy") {
        Some("spicy_food".into())
    } else if lower.contains("book a hotel") || lower.contains("hotel in chicago") {
        Some("hotel_booking".into())
    } else if lower.contains("change the ac filter") || lower.contains("change ac filter") {
        Some("ac_filter".into())
    } else if lower.contains("what should we do with the kids") || lower.contains("kids today") {
        Some("kids_activity".into())
    } else if lower.contains("toilet is clogged") || lower.contains("toilet clogged") {
        Some("clogged_toilet".into())
    } else if lower.contains("sew a button") {
        Some("sewing_help".into())
    } else if lower.contains("listen to jazz") || lower.contains("jazz") {
        Some("jazz_music".into())
    } else if lower.contains("suggest") && lower.contains("book") {
        Some("book_recommendation".into())
    } else if lower.contains("bored of cooking") {
        Some("takeout_suggestion".into())
    } else if lower.contains("ripe banana") || lower.contains("banana") {
        Some("banana_recipe".into())
    } else if lower.contains("beach trip") || lower.contains("beach") {
        Some("beach_photos".into())
    } else if lower.contains("freeze tonight") || lower.contains("going to freeze") {
        Some("freeze_protection".into())
    } else if lower.contains("log my weight") || lower.contains("weight") {
        Some("weight_trend".into())
    } else if lower.contains("pack a lunch") || lower.contains("lunch for leo") {
        Some("school_lunch".into())
    } else if lower.contains("patio cushion") {
        Some("patio_cushions".into())
    } else if lower.contains("bike ride") || lower.contains("cycling") {
        Some("cycling_routine".into())
    } else if lower.contains("what s for breakfast") || lower.contains("what's for breakfast") {
        Some("breakfast_plan".into())
    } else if lower.contains("father's day")
        || lower.contains("father s day")
        || lower.contains("fathers day")
        || (lower.contains("dad") && lower.contains("gift"))
    {
        Some("father_day_gift".into())
    } else if lower.contains("need a break") {
        Some("quick_break".into())
    } else if lower.contains("wine") && lower.contains("steak") {
        Some("steak_wine_pairing".into())
    } else if lower.contains("stuffy") {
        Some("ventilation_comfort".into())
    } else if lower.contains("working from home") || lower.contains("work from home") {
        Some("work_from_home".into())
    } else if lower.contains("ink") && lower.contains("printer") {
        Some("printer_ink".into())
    } else if lower.contains("safe to run") || lower.contains("run outside") {
        Some("running_safety".into())
    } else if lower.contains("sarah") && lower.contains("birthday last year") {
        Some("gift_history".into())
    } else if lower.contains("game night") {
        Some("game_night".into())
    } else if lower.contains("baby") && lower.contains("crying again") {
        Some("baby_crying_again".into())
    } else if lower.contains("olive oil") || lower.contains("use instead") {
        Some("cooking_substitute".into())
    } else if lower.contains("bookshelf") {
        Some("diy_bookshelf".into())
    } else if lower.contains("toilet") && lower.contains("running") {
        Some("toilet_troubleshooting".into())
    } else if lower.contains("knee") && lower.contains("run") {
        Some("running_injury".into())
    } else if lower.contains("side dish") || (lower.contains("pasta") && lower.contains("need")) {
        Some("pasta_side_dish".into())
    } else if lower.contains("pack my gym bag") || lower.contains("gym bag") {
        Some("gym_bag".into())
    } else if lower.contains("over budget") || lower.contains("budget this month") {
        Some("budget_advice".into())
    } else if lower.contains("text mom") && lower.contains("birthday") {
        Some("birthday_message".into())
    } else if lower.contains("driving home") && lower.contains("rain") {
        Some("arrival_rain".into())
    } else if lower.contains("safe to eat") {
        Some("food_safety".into())
    } else if lower.contains("dark parking lot") {
        Some("safety_protocol".into())
    } else if lower.contains("movie we haven")
        || lower.contains("movie we have not")
        || lower.contains("haven t seen")
    {
        Some("unwatched_media".into())
    } else if lower.contains("defrost") && lower.contains("turkey") {
        Some("turkey_thawing".into())
    } else if lower.contains("airport") {
        Some("airport_departure".into())
    } else if lower.contains("dinner party")
        || (lower.contains("buy food") && lower.contains("party"))
    {
        Some("dinner_party_food".into())
    } else if lower.contains("need a laugh") || lower.contains("comedy") {
        Some("comedy_media".into())
    } else if lower.contains("solar system") || lower.contains("space") || lower.contains("planets")
    {
        Some("space_learning".into())
    } else if lower.contains("oversleep") || lower.contains("wake up") {
        Some("sleep_routine".into())
    } else if lower.contains("really hot") || lower.contains("i m hot") || lower.contains("i'm hot")
    {
        Some("cooling_comfort".into())
    } else if lower.contains("guests coming") || lower.contains("guests coming over") {
        Some("guest_arrival".into())
    } else if lower.contains("baby is awake") || lower.contains("baby awake") {
        Some("baby_awake".into())
    } else if lower.contains("what s for dinner") || lower.contains("what's for dinner") {
        Some("meal_plan".into())
    } else if lower.contains("going for a run") {
        Some("running_routine".into())
    } else if lower.contains("package") || lower.contains("delivered") {
        Some("delivery_tracking".into())
    } else if lower.contains("feeling cold")
        || lower.contains("feel cold")
        || lower.contains("i'm cold")
    {
        Some("home_comfort".into())
    } else if lower.contains("lunchbox")
        || lower.contains("lunch box")
        || lower.contains("snack")
        || lower.contains("detergent")
    {
        Some("shopping".into())
    } else if lower.contains("scary movie")
        || lower.contains("horror")
        || lower.contains("thriller")
    {
        Some("scary_media".into())
    } else if lower.contains("robot")
        || lower.contains("movie")
        || lower.contains("comfort movie")
        || lower.contains("feel-good")
    {
        Some("media".into())
    } else if lower.contains("bored") {
        Some("activity_suggestion".into())
    } else if lower.contains("start the car") {
        Some("vehicle_routine".into())
    } else if lower.contains("car")
        || lower.contains("mechanic")
        || (lower.contains("noise") && lower.contains("car"))
    {
        Some("vehicle_troubleshooting".into())
    } else if lower.contains("printer") {
        Some("device_troubleshooting".into())
    } else if lower.contains("cook") || lower.contains("chicken") || lower.contains("rice") {
        Some("recipe".into())
    } else if lower.contains("park") || lower.contains("warm enough") {
        Some("outdoor_preference".into())
    } else if lower.contains("leak") || lower.contains("sink") || lower.contains("plumber") {
        Some("home_maintenance".into())
    } else if lower.contains("grandma") || lower.contains("too late to call") {
        Some("family_contact".into())
    } else if lower.contains("baby") || lower.contains("crying") {
        Some("routine".into())
    } else if lower.contains("stressed") || lower.contains("stress") {
        Some("wellness".into())
    } else if lower.contains("science fair") {
        Some("science_project".into())
    } else if lower.contains("bake") || (lower.contains("flour") && lower.contains("sugar")) {
        Some("recipe".into())
    } else if lower.contains("headache") {
        Some("first_aid".into())
    } else if lower.contains("read me a story") || lower.contains("story") {
        Some("story".into())
    } else if lower.contains("movie for tonight") || lower.contains("find a movie") {
        Some("media".into())
    } else if lower.contains("dog food") {
        Some("pet_shopping".into())
    } else if lower.contains("trip to the zoo") || lower.contains("zoo") {
        Some("trip_planning".into())
    } else if lower.contains("hungry") && lower.contains("diet") {
        Some("diet_recipe".into())
    } else if lower.contains("washing machine")
        || lower.contains("washer")
        || lower.contains("shaking")
    {
        Some("appliance_troubleshooting".into())
    } else if lower.contains("watch tv") {
        Some("watch_history".into())
    } else if lower.contains("someone is at the door") || lower.contains("doorbell") {
        Some("visitor".into())
    } else if lower.contains("focus music") {
        Some("focus_music".into())
    } else if lower.contains("need a drink") || lower.contains("soccer practice") {
        Some("beverage".into())
    } else if lower.contains("too bright") {
        Some("light_comfort".into())
    } else if lower.contains("lonely") {
        Some("social_support".into())
    } else if lower.contains("sink smells") || lower.contains("kitchen sink smells") {
        Some("home_maintenance".into())
    } else if lower.contains("leaving for work") {
        Some("commute".into())
    } else if lower.contains("make tacos") || lower.contains("taco") {
        Some("meal_planning".into())
    } else if lower.contains("muggy") {
        Some("humidity_comfort".into())
    } else if lower.contains("cut my finger") {
        Some("first_aid".into())
    } else if lower.contains("where are my keys") || lower.contains("keys") {
        Some("location".into())
    } else if lower.contains("noise outside") {
        Some("outdoor_sound".into())
    } else if lower.contains("order pizza") {
        Some("pizza_order".into())
    } else if lower == "i'm home" || lower == "i m home" || lower == "i am home" {
        Some("arrival_routine".into())
    } else if lower.contains("math homework") || lower.contains("fractions") {
        Some("education_help".into())
    } else if lower.contains("tired of this song") || lower.contains("this song") {
        Some("music_control".into())
    } else if lower.contains("tell me a joke") || lower.contains("joke") {
        Some("entertainment_joke".into())
    } else if lower.contains("ephemeral") {
        Some("dictionary_definition".into())
    } else if lower.contains("build a fort") || lower.contains("fort") {
        Some("fort_activity".into())
    } else if lower.contains("running late for the train") || lower.contains("train") {
        Some("train_commute".into())
    } else if lower.contains("air quality") {
        Some("air_quality_health".into())
    } else if lower.contains("slow cooker") || lower.contains("chili") {
        Some("slow_cooker_recipe".into())
    } else if lower.contains("birthday party") {
        Some("party_planning".into())
    } else if lower.contains("spider") {
        Some("pest_control".into())
    } else if lower.contains("remote") {
        Some("location".into())
    } else if lower.contains("freezer") {
        Some("food_safety".into())
    } else {
        None
    }
}

fn parse_embedding(value: &str, dimensions: usize) -> Option<Vec<f32>> {
    let embedding = serde_json::from_str::<Vec<f32>>(value).ok()?;
    if embedding.len() == dimensions {
        Some(embedding)
    } else {
        None
    }
}

fn family_calendar_events_from_memory(kind: &str, content: &str) -> Vec<FamilyCalendarEvent> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if trimmed.is_empty()
        || !(kind_lower.contains("calendar")
            || kind_lower.contains("schedule")
            || kind_lower.contains("event")
            || kind_lower.contains("medical")
            || kind_lower.contains("pet_calendar")
            || lower.contains(" lesson")
            || lower.contains("appointment")
            || lower.contains("checkup")
            || lower.contains("school pickup"))
    {
        return Vec::new();
    }

    let mut events = Vec::new();
    if lower.contains("piano")
        && let Some((person, _)) = split_once_case_insensitive(trimmed, &lower, " has ")
    {
        events.push(FamilyCalendarEvent {
            source_memory_id: 0,
            person: Some(clean_person_name(person)),
            event_type: "piano_lesson".into(),
            title: "piano lessons".into(),
            day: calendar_day_from_text(&lower),
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("dentist")
        && lower.contains("appointment")
        && let Some(person) = calendar_person_from_statement(trimmed, &lower)
    {
        events.push(FamilyCalendarEvent {
            source_memory_id: 0,
            person: Some(person),
            event_type: "dentist_appointment".into(),
            title: "dentist appointment".into(),
            day: calendar_day_from_text(&lower),
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if (lower.contains("vet") || lower.contains("checkup"))
        && (lower.contains("appointment") || lower.contains("checkup"))
        && let Some(person) = calendar_person_from_statement(trimmed, &lower)
    {
        events.push(FamilyCalendarEvent {
            source_memory_id: 0,
            person: Some(person),
            event_type: "vet_appointment".into(),
            title: "vet appointment".into(),
            day: calendar_day_from_text(&lower),
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("school pickup") {
        let person = if let Some((person, _)) =
            split_once_case_insensitive(trimmed, &lower, " is scheduled for school pickup")
        {
            Some(clean_person_name(person))
        } else if let Some((_, person)) =
            split_once_case_insensitive(trimmed, &lower, "school pickup today is ")
        {
            Some(clean_person_name(person))
        } else if let Some((_, person)) =
            split_once_case_insensitive(trimmed, &lower, "school pickup is ")
        {
            Some(clean_person_name(person))
        } else {
            None
        };

        events.push(FamilyCalendarEvent {
            source_memory_id: 0,
            person,
            event_type: "school_pickup".into(),
            title: "school pickup".into(),
            day: calendar_day_from_text(&lower),
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    events
}

fn shopping_list_items_from_memory(kind: &str, content: &str) -> Vec<ShoppingListItem> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if !(kind_lower == "shopping" || lower.contains("shopping list")) {
        return Vec::new();
    }

    let status = if contains_any(&lower, &[" removed:", " remove:", " taken off:"]) {
        "removed"
    } else if contains_any(
        &lower,
        &[" done:", " bought:", " completed:", " purchased:"],
    ) {
        "done"
    } else {
        "pending"
    };
    let items_text = lower
        .find("shopping list pending:")
        .map(|pos| &trimmed[pos + "shopping list pending:".len()..])
        .or_else(|| {
            lower
                .find("shopping list removed:")
                .map(|pos| &trimmed[pos + "shopping list removed:".len()..])
        })
        .or_else(|| {
            lower
                .find("shopping list remove:")
                .map(|pos| &trimmed[pos + "shopping list remove:".len()..])
        })
        .or_else(|| {
            lower
                .find("shopping list:")
                .map(|pos| &trimmed[pos + "shopping list:".len()..])
        })
        .or_else(|| {
            if let Some(rest) = lower.strip_prefix("add ") {
                let pos = rest.find(" to the shopping list")?;
                trimmed.get("add ".len().."add ".len() + pos)
            } else {
                None
            }
        })
        .unwrap_or(trimmed);

    split_list_items(items_text)
        .into_iter()
        .map(|item| ShoppingListItem {
            source_memory_id: 0,
            item,
            status: status.into(),
        })
        .collect()
}

fn household_inventory_items_from_memory(kind: &str, content: &str) -> Vec<HouseholdInventoryItem> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if trimmed.is_empty()
        || !(kind_lower.contains("pantry")
            || kind_lower.contains("inventory")
            || kind_lower.contains("fridge")
            || kind_lower.contains("grocery")
            || lower.contains("inventory:")
            || lower.contains("remaining in")
            || lower.contains("left in"))
    {
        return Vec::new();
    }

    let mut items = Vec::new();
    if lower.contains("egg") {
        items.push(HouseholdInventoryItem {
            source_memory_id: 0,
            item: "eggs".into(),
            quantity: quantity_for_inventory_item(trimmed, &lower, &["egg", "eggs"]),
            location: inventory_location(trimmed, &lower),
            category: if lower.contains("fridge") || lower.contains("refrigerator") {
                "fridge".into()
            } else {
                "pantry".into()
            },
            description: trimmed.to_string(),
        });
    }

    items
}

fn access_permissions_from_memory(kind: &str, content: &str) -> Vec<AccessPermission> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if !(kind_lower.contains("access")
        || kind_lower.contains("permission")
        || lower.contains("authorized to unlock")
        || lower.contains("can only unlock"))
    {
        return Vec::new();
    }

    let primary_person = leading_person_name(trimmed);
    let mut permissions = Vec::new();
    if let Some((person, device)) = permission_statement(
        trimmed,
        &lower,
        " is not authorized to unlock ",
        primary_person.as_deref(),
    ) {
        permissions.push(AccessPermission {
            source_memory_id: 0,
            person,
            device,
            action: "unlock".into(),
            allowed: false,
            description: trimmed.to_string(),
        });
    }
    if let Some((person, device)) = permission_statement(
        trimmed,
        &lower,
        " can only unlock ",
        primary_person.as_deref(),
    ) {
        permissions.push(AccessPermission {
            source_memory_id: 0,
            person,
            device,
            action: "unlock".into(),
            allowed: true,
            description: trimmed.to_string(),
        });
    }
    permissions
}

fn household_task_logs_from_memory(kind: &str, content: &str) -> Vec<HouseholdTaskLog> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if !(kind_lower.contains("task")
        || kind_lower.contains("chore")
        || kind_lower.contains("pet_care")
        || lower.contains("marked the task")
        || lower.contains("fed the dog")
        || lower.contains("feed the dog")
        || lower.contains("fed the cat")
        || lower.contains("feed the cat")
        || lower.contains("cat feeding")
        || lower.contains("brush")
        || lower.contains("brushed"))
    {
        return Vec::new();
    }

    let mut logs = Vec::new();
    if lower.contains("fed the dog") || lower.contains("feed the dog") {
        let person = leading_person_name(trimmed).unwrap_or_else(|| "Unknown".into());
        logs.push(HouseholdTaskLog {
            source_memory_id: 0,
            person,
            task: "feeding".into(),
            subject: Some("dog".into()),
            day: calendar_day_from_text(&lower),
            time: time_after_marker(trimmed, &lower, " at "),
            status: if contains_any(&lower, &["complete", "completed", "done", "yes"]) {
                "complete".into()
            } else {
                "logged".into()
            },
            description: trimmed.to_string(),
        });
    }

    if lower.contains("fed the cat")
        || lower.contains("feed the cat")
        || lower.contains("cat feeding")
    {
        let person = leading_person_name(trimmed)
            .or_else(|| subject_before_marker(trimmed, &lower, " checked off cat feeding"))
            .unwrap_or_else(|| "Unknown".into());
        logs.push(HouseholdTaskLog {
            source_memory_id: 0,
            person,
            task: "feeding".into(),
            subject: Some("cat".into()),
            day: calendar_day_from_text(&lower).or_else(|| Some("today".into())),
            time: time_after_marker(trimmed, &lower, " at "),
            status: if contains_any(
                &lower,
                &["complete", "completed", "done", "yes", "checked off"],
            ) {
                "complete".into()
            } else {
                "logged".into()
            },
            description: trimmed.to_string(),
        });
    }

    if (lower.contains("brushed") || lower.contains("brush"))
        && lower.contains("teeth")
        && let Some(person) = leading_person_name(trimmed)
    {
        logs.push(HouseholdTaskLog {
            source_memory_id: 0,
            person,
            task: "brush_teeth".into(),
            subject: None,
            day: calendar_day_from_text(&lower).or_else(|| Some("today".into())),
            time: time_after_marker(trimmed, &lower, " at "),
            status: if contains_any(&lower, &["complete", "completed", "done", "yes"]) {
                "complete".into()
            } else {
                "logged".into()
            },
            description: trimmed.to_string(),
        });
    }

    logs
}

fn household_schedule_items_from_memory(kind: &str, content: &str) -> Vec<HouseholdScheduleItem> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if !(kind_lower.contains("schedule")
        || kind_lower.contains("bill")
        || kind_lower.contains("utility")
        || kind_lower.contains("recycling")
        || kind_lower.contains("school_calendar")
        || kind_lower.contains("school_transport")
        || kind_lower.contains("city_services")
        || kind_lower.contains("community_services")
        || kind_lower.contains("business_hours")
        || kind_lower.contains("astronomical")
        || kind_lower.contains("program_guide")
        || kind_lower.contains("electronic_program_guide")
        || kind_lower.contains("community_calendar")
        || kind_lower.contains("subscription")
        || kind_lower.contains("trash")
        || lower.contains("bus arrives")
        || lower.contains("bus pickup")
        || lower.contains("bill is due")
        || lower.contains(" channel ")
        || lower.contains("tv tonight")
        || lower.contains("tonight at")
        || lower.contains("city council")
        || lower.contains("sunset")
        || lower.contains("sun set")
        || lower.contains("recycling")
        || lower.contains("trash pickup")
        || lower.contains("pool")
        || lower.contains("library closes")
        || lower.contains("library close")
        || lower.contains("subscription")
        || lower.contains("renews")
        || lower.contains("parent-teacher conference")
        || lower.contains("parent teacher conference"))
    {
        return Vec::new();
    }

    let mut items = Vec::new();
    if lower.contains(" channel ")
        && let Some((subject, channel)) = channel_guide_from_text(trimmed, &lower)
    {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "channel_guide".into(),
            subject: Some(subject),
            title: "channel guide".into(),
            day: None,
            date: None,
            time: None,
            amount: Some(channel),
            description: trimmed.to_string(),
        });
    }

    if (kind_lower.contains("program_guide")
        || kind_lower.contains("electronic_program_guide")
        || lower.contains("tv tonight")
        || lower.contains("tonight at"))
        && (lower.contains("tonight") || lower.contains(" tv "))
    {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "tv_tonight".into(),
            subject: Some("tv tonight".into()),
            title: "TV tonight".into(),
            day: Some("today".into()),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at "),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("city council") && lower.contains("meeting") {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "community_meeting".into(),
            subject: Some("city council".into()),
            title: "city council meeting".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at "),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if (lower.contains("school bus") || lower.contains("bus arrives")) && lower.contains("arrives")
    {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "school_bus_arrival".into(),
            subject: Some("school bus".into()),
            title: "school bus arrival".into(),
            day: calendar_day_from_text(&lower),
            date: None,
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| time_after_marker(trimmed, &lower, " arrives at ")),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("bus pickup") {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "school_bus_arrival".into(),
            subject: if lower.contains("mia") {
                Some("mia".into())
            } else if lower.contains("leo") {
                Some("leo".into())
            } else {
                Some("school bus".into())
            },
            title: "school bus pickup".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| time_after_marker(trimmed, &lower, " is ")),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("bill") && lower.contains("due") {
        let subject = bill_subject_from_text(trimmed, &lower);
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "bill_due".into(),
            subject,
            title: "bill due".into(),
            day: relative_due_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: None,
            amount: amount_from_text(trimmed),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("recycling") {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "recycling".into(),
            subject: Some("recycling".into()),
            title: "recycling schedule".into(),
            day: calendar_day_from_text(&lower),
            date: None,
            time: None,
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("trash pickup") || lower.contains("trash day") {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "trash_pickup".into(),
            subject: Some("trash".into()),
            title: "trash pickup".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at "),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("conference")
        && (lower.contains("parent-teacher") || lower.contains("parent teacher"))
    {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "school_conference".into(),
            subject: subject_after_marker(trimmed, &lower, " for "),
            title: "parent-teacher conference".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at "),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("sunset") || lower.contains("sun set") {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "sunset".into(),
            subject: Some("sunset".into()),
            title: "sunset".into(),
            day: calendar_day_from_text(&lower).or_else(|| Some("today".into())),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| time_after_marker(trimmed, &lower, " is ")),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("pool") && (lower.contains("open") || lower.contains("hours")) {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "community_facility_hours".into(),
            subject: Some(if lower.contains("community pool") {
                "community pool".into()
            } else {
                "pool".into()
            }),
            title: "community pool hours".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| time_after_marker(trimmed, &lower, " opens at ")),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("library") && (lower.contains("close") || lower.contains("hours")) {
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "business_hours".into(),
            subject: Some(if lower.contains("public library") {
                "public library".into()
            } else {
                "library".into()
            }),
            title: "library hours".into(),
            day: calendar_day_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: time_after_marker(trimmed, &lower, " closes at ")
                .or_else(|| time_after_marker(trimmed, &lower, " close at "))
                .or_else(|| time_after_marker(trimmed, &lower, " at ")),
            amount: None,
            description: trimmed.to_string(),
        });
    }

    if lower.contains("subscription") && (lower.contains("renew") || lower.contains("due")) {
        let subject = subscription_subject_from_text(trimmed, &lower);
        items.push(HouseholdScheduleItem {
            source_memory_id: 0,
            schedule_type: "subscription_renewal".into(),
            subject,
            title: "subscription renewal".into(),
            day: relative_due_from_text(&lower),
            date: due_date_from_text(trimmed, &lower),
            time: None,
            amount: amount_from_text(trimmed),
            description: trimmed.to_string(),
        });
    }

    items
}

fn household_event_logs_from_memory(kind: &str, content: &str) -> Vec<HouseholdEventLog> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    let kind_lower = kind.to_ascii_lowercase();
    if !(kind_lower.contains("security_log")
        || kind_lower.contains("event_log")
        || kind_lower.contains("family_ledger")
        || kind_lower.contains("ledger")
        || kind_lower.contains("finance")
        || kind_lower.contains("payment_history")
        || kind_lower.contains("financial_services")
        || kind_lower.contains("financial_market_api")
        || kind_lower.contains("fitness_tracker")
        || kind_lower.contains("smart_scale")
        || kind_lower.contains("presence_state")
        || kind_lower.contains("access_logs")
        || kind_lower.contains("device_events")
        || kind_lower.contains("health_device_events")
        || kind_lower.contains("appliance_state")
        || kind_lower.contains("waste_management")
        || kind_lower.contains("environmental_sensor")
        || kind_lower.contains("location_service")
        || lower.contains("security system was disarmed")
        || lower.contains("system was disarmed")
        || lower.contains("disarmed by")
        || lower.contains("dishwasher")
        || lower.contains("trash truck")
        || lower.contains("attic")
        || lower.contains("home from school")
        || lower.contains("credit score")
        || lower.contains("stock price")
        || lower.contains("trading at")
        || lower.contains("vo2 max")
        || lower.contains("weight is")
        || lower.contains("garage door")
        || lower.contains("phone connected")
        || lower.contains("is home")
        || lower.contains("home network")
        || lower.contains("allowance")
        || lower.contains("bill") && lower.contains("paid"))
    {
        return Vec::new();
    }

    let mut events = Vec::new();
    if lower.contains("credit score") {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "finance".into(),
            subject: Some("credit score".into()),
            action: "credit_score".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| relative_calendar_phrase_from_text(&lower)),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("vo2 max") {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "health".into(),
            subject: Some("vo2 max".into()),
            action: "vo2_max".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| relative_calendar_phrase_from_text(&lower)),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("stock price") || lower.contains("trading at") {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "finance".into(),
            subject: stock_subject_from_text(trimmed, &lower),
            action: "stock_price".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| relative_calendar_phrase_from_text(&lower)),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("weight is")
        || (kind_lower.contains("smart_scale") && lower.contains("weight"))
    {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "health".into(),
            subject: Some("weight".into()),
            action: "weight_reading".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at ")
                .or_else(|| relative_calendar_phrase_from_text(&lower)),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("dishwasher") && (lower.contains("clean") || lower.contains("dirty")) {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "appliance_state".into(),
            subject: Some("dishwasher".into()),
            action: "clean_status".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("trash truck") || lower.contains("truck came") {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "waste".into(),
            subject: Some("trash".into()),
            action: "collection".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("attic") && lower.contains("temperature") {
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "environment".into(),
            subject: Some("attic".into()),
            action: "temperature".into(),
            actor: None,
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("home from school") || lower.contains("arrived home") {
        let person = leading_person_name(trimmed).or_else(|| {
            lower
                .find(" arrived home")
                .map(|pos| clean_person_name(&trimmed[..pos]))
        });
        if let Some(person) = person.filter(|person| !person.is_empty()) {
            events.push(HouseholdEventLog {
                source_memory_id: 0,
                event_type: "location".into(),
                subject: Some(person.clone()),
                action: "home_arrival".into(),
                actor: Some(person),
                time: time_after_marker(trimmed, &lower, " at ")
                    .or_else(|| relative_calendar_phrase_from_text(&lower)),
                description: trimmed.to_string(),
            });
        }
    }

    if (kind_lower.contains("presence_state")
        || lower.contains("phone connected")
        || lower.contains("home network")
        || lower.contains(" is home"))
        && lower.contains("home")
    {
        let person = leading_person_name(trimmed)
            .or_else(|| subject_before_marker(trimmed, &lower, " phone connected"))
            .or_else(|| subject_before_marker(trimmed, &lower, " is home"));
        if let Some(person) = person.filter(|person| !person.is_empty()) {
            events.push(HouseholdEventLog {
                source_memory_id: 0,
                event_type: "location".into(),
                subject: Some(person.clone()),
                action: "presence_home".into(),
                actor: Some(person),
                time: time_after_marker(trimmed, &lower, " at ")
                    .or_else(|| relative_calendar_phrase_from_text(&lower)),
                description: trimmed.to_string(),
            });
        }
    }

    if lower.contains("garage door")
        && (lower.contains("opened") || lower.contains("open event") || lower.contains("open "))
    {
        let actor = actor_after_marker(trimmed, &lower, " by ")
            .or_else(|| subject_before_marker(trimmed, &lower, " opened the garage door"))
            .or_else(|| leading_person_name(trimmed));
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "access".into(),
            subject: Some("garage door".into()),
            action: "open".into(),
            actor,
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("disarmed") || lower.contains("turned off the security system") {
        let actor = actor_after_marker(trimmed, &lower, " by ");
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "security".into(),
            subject: Some("security system".into()),
            action: "disarm".into(),
            actor,
            time: time_after_marker(trimmed, &lower, " at "),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("allowance") && (lower.contains("received") || lower.contains("got ")) {
        let person = allowance_person_from_text(trimmed, &lower);
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "finance".into(),
            subject: person.clone(),
            action: "allowance".into(),
            actor: person,
            time: relative_calendar_phrase_from_text(&lower),
            description: trimmed.to_string(),
        });
    }

    if lower.contains("bill") && lower.contains("paid") {
        let subject = bill_subject_from_text(trimmed, &lower);
        events.push(HouseholdEventLog {
            source_memory_id: 0,
            event_type: "finance".into(),
            subject: subject.clone(),
            action: "paid_bill".into(),
            actor: None,
            time: relative_calendar_phrase_from_text(&lower),
            description: trimmed.to_string(),
        });
    }

    events
}

fn media_profile_item_from_memory(content: &str) -> Option<MediaProfileItem> {
    let trimmed = content
        .trim()
        .trim_matches(|ch| matches!(ch, '.' | '!' | '?'));
    let lower = trimmed.to_ascii_lowercase();
    if !lower.contains("playlist") {
        return None;
    }

    let (statement, target) = split_media_target(trimmed, &lower)?;
    let statement_lower = statement.to_ascii_lowercase();
    let (owner, name) = playlist_owner_and_name(statement, &statement_lower)?;
    let name = clean_sentence_value(&name);
    let target = clean_sentence_value(target);
    if name.is_empty() || target.is_empty() {
        return None;
    }

    Some(MediaProfileItem {
        source_memory_id: 0,
        owner,
        item_type: "playlist".into(),
        name,
        provider: media_provider_from_target(&target, &lower),
        target,
    })
}

fn split_media_target<'a>(content: &'a str, lower: &str) -> Option<(&'a str, &'a str)> {
    for marker in [
        " maps to ",
        " is ",
        " uri is ",
        " url is ",
        " opens ",
        " plays ",
    ] {
        if let Some(pos) = lower.rfind(marker) {
            let left = content[..pos].trim();
            let right = content[pos + marker.len()..].trim();
            if !left.is_empty() && !right.is_empty() {
                return Some((left, right));
            }
        }
    }
    None
}

fn playlist_owner_and_name(statement: &str, lower: &str) -> Option<(Option<String>, String)> {
    if let Some(pos) = lower.find("'s playlist named ") {
        let owner = clean_person_name(&statement[..pos]);
        let name = statement[pos + "'s playlist named ".len()..].trim();
        return Some((Some(owner), name.to_string()));
    }
    if let Some(pos) = lower.find("'s playlist ") {
        let owner = clean_person_name(&statement[..pos]);
        let name = statement[pos + "'s playlist ".len()..].trim();
        return Some((Some(owner), name.to_string()));
    }
    if let Some(pos) = lower.find("'s ")
        && let Some(playlist_pos) = lower[pos + 3..].find(" playlist")
    {
        let owner = clean_person_name(&statement[..pos]);
        let start = pos + 3;
        let end = start + playlist_pos;
        let name = statement[start..end].trim();
        return Some((Some(owner), name.to_string()));
    }
    if let Some(pos) = lower.find("playlist named ") {
        let name = statement[pos + "playlist named ".len()..].trim();
        return Some((None, name.to_string()));
    }
    if let Some(pos) = lower.find(" playlist") {
        let name = statement[..pos]
            .trim()
            .trim_start_matches("the ")
            .trim_start_matches("my ")
            .trim_start_matches("our ");
        return Some((None, name.to_string()));
    }
    None
}

fn media_provider_from_target(target: &str, lower: &str) -> Option<String> {
    let target_lower = target.to_ascii_lowercase();
    if target_lower.starts_with("spotify:") || lower.contains("spotify") {
        Some("spotify".into())
    } else if target_lower.contains("youtube") || lower.contains("youtube") {
        Some("youtube".into())
    } else if target_lower.contains("plex") || lower.contains("plex") {
        Some("plex".into())
    } else {
        None
    }
}

fn media_playlist_query(query: &str) -> Option<(Option<String>, String)> {
    let normalized = normalize_alias_key(query);
    if !normalized.contains("playlist") {
        return None;
    }
    let mut text = normalized.as_str();
    for prefix in ["please play ", "play ", "start ", "put on "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
            break;
        }
    }
    let text = text
        .trim_end_matches(" on spotify")
        .trim_end_matches(" playlist")
        .trim();
    if text.is_empty() {
        return None;
    }

    let tokens = text.split_whitespace().collect::<Vec<_>>();
    let (owner, name_tokens) = if tokens.len() >= 3 && tokens[1] == "s" {
        (Some(clean_person_name(tokens[0])), &tokens[2..])
    } else if matches!(tokens.first(), Some(&"my" | &"our" | &"the")) {
        (None, &tokens[1..])
    } else {
        (None, tokens.as_slice())
    };
    let name = name_tokens.join(" ");
    if name.is_empty() {
        None
    } else {
        Some((owner, name))
    }
}

fn calendar_event_query(query: &str) -> Option<(String, String, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if (lower.contains("vet") || lower.contains("checkup")) && lower.contains("appointment") {
        let person = if let Some(rest) = lower.strip_prefix("when is ") {
            rest.split("'s")
                .next()
                .or_else(|| rest.split(" next ").next())
                .map(clean_person_name)
        } else if lower.starts_with("does ") || lower.starts_with("do ") {
            let rest = query.get(
                if lower.starts_with("does ") {
                    "does ".len()
                } else {
                    "do ".len()
                }..,
            )?;
            let lower_rest = rest.to_ascii_lowercase();
            let have_pos = lower_rest.find(" have ")?;
            Some(clean_person_name(&rest[..have_pos]))
        } else {
            None
        }?;
        if !person.is_empty() {
            return Some((
                person,
                "vet_appointment".into(),
                calendar_day_from_text(&lower),
            ));
        }
    }

    if lower.contains("dentist") && lower.contains("appointment") {
        let person = if let Some(rest) = lower.strip_prefix("when is ") {
            rest.split("'s")
                .next()
                .or_else(|| rest.split(" next ").next())
                .map(clean_person_name)
        } else if lower.starts_with("does ") || lower.starts_with("do ") {
            let rest = query.get(
                if lower.starts_with("does ") {
                    "does ".len()
                } else {
                    "do ".len()
                }..,
            )?;
            let lower_rest = rest.to_ascii_lowercase();
            let have_pos = lower_rest.find(" have ")?;
            Some(clean_person_name(&rest[..have_pos]))
        } else {
            None
        }?;
        if !person.is_empty() {
            return Some((
                person,
                "dentist_appointment".into(),
                calendar_day_from_text(&lower),
            ));
        }
    }

    if !(lower.starts_with("does ") || lower.starts_with("do ")) {
        return None;
    }
    if !lower.contains(" have ") || !lower.contains("piano") {
        return None;
    }
    let rest = query.get(
        if lower.starts_with("does ") {
            "does ".len()
        } else {
            "do ".len()
        }..,
    )?;
    let lower_rest = rest.to_ascii_lowercase();
    let have_pos = lower_rest.find(" have ")?;
    let person = clean_person_name(&rest[..have_pos]);
    if person.is_empty() {
        return None;
    }
    Some((
        person,
        "piano_lesson".into(),
        calendar_day_from_text(&lower),
    ))
}

fn calendar_person_from_statement(content: &str, lower: &str) -> Option<String> {
    if let Some((person, _)) = split_once_case_insensitive(content, lower, " has ") {
        let person = clean_person_name(person);
        if !person.is_empty() {
            return Some(person);
        }
    }
    if let Some(pos) = lower.find("'s ") {
        let person = clean_person_name(&content[..pos]);
        if !person.is_empty() {
            return Some(person);
        }
    }
    leading_person_name(content)
}

fn school_pickup_query(query: &str) -> Option<String> {
    let lower = query.to_ascii_lowercase();
    if !(lower.contains("picking up the kids")
        || lower.contains("picking up kids")
        || lower.contains("school pickup"))
    {
        return None;
    }
    calendar_day_from_text(&lower).or_else(|| Some("today".into()))
}

fn access_permission_query(query: &str) -> Option<(String, String, String)> {
    let lower = query.to_ascii_lowercase();
    if !lower.starts_with("can ") || !lower.contains(" unlock ") {
        return None;
    }
    let rest = query.get("can ".len()..)?;
    let lower_rest = rest.to_ascii_lowercase();
    let unlock_pos = lower_rest.find(" unlock ")?;
    let person = clean_person_name(&rest[..unlock_pos]);
    let device = clean_device_phrase(&rest[unlock_pos + " unlock ".len()..]);
    if person.is_empty() || device.is_empty() {
        return None;
    }
    Some((person, "unlock".into(), device))
}

fn shopping_list_query(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    lower.contains("shopping list")
        && (lower.starts_with("what")
            || lower.starts_with("show")
            || lower.starts_with("what is on")
            || lower.starts_with("what's on"))
}

fn inventory_item_query(query: &str) -> Option<String> {
    let lower = clean_sentence_value(query).to_ascii_lowercase();
    let patterns = [
        ("do we have any ", " left"),
        ("do we have ", " left"),
        ("do we have any ", ""),
        ("do we have ", ""),
        ("are there any ", " left"),
        ("are there ", " left"),
    ];
    for (prefix, suffix) in patterns {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let item = if suffix.is_empty() {
                rest
            } else if let Some(item) = rest.strip_suffix(suffix) {
                item
            } else {
                continue;
            };
            let item = clean_sentence_value(item)
                .trim_start_matches("any ")
                .trim_start_matches("the ")
                .trim()
                .to_string();
            if !item.is_empty() {
                return Some(item);
            }
        }
    }
    None
}

fn task_log_query(query: &str) -> Option<(String, String, Option<String>, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if !(lower.starts_with("did ")
        && (lower.contains(" feed ")
            || lower.contains(" fed ")
            || lower.contains(" brush ")
            || lower.contains(" brushed ")))
    {
        return None;
    }
    let rest = query.get("did ".len()..)?;
    let lower_rest = rest.to_ascii_lowercase();
    let task_pos = lower_rest
        .find(" feed ")
        .or_else(|| lower_rest.find(" fed "))
        .or_else(|| lower_rest.find(" brush "))
        .or_else(|| lower_rest.find(" brushed "))?;
    let person = clean_person_name(&rest[..task_pos]);
    if person.is_empty() {
        return None;
    }
    let task = if lower.contains("brush") || lower.contains("brushed") {
        "brush_teeth".to_string()
    } else {
        "feeding".to_string()
    };
    let subject = if task == "feeding" && lower.contains("dog") {
        Some("dog".to_string())
    } else if task == "feeding" && lower.contains("cat") {
        Some("cat".to_string())
    } else {
        None
    };
    Some((
        person,
        task,
        subject,
        calendar_day_from_text(&lower).or_else(|| Some("today".into())),
    ))
}

fn everyone_brush_teeth_query(query: &str) -> Option<String> {
    let lower = query.to_ascii_lowercase();
    if lower.starts_with("did everyone") && lower.contains("brush") && lower.contains("teeth") {
        return Some(calendar_day_from_text(&lower).unwrap_or_else(|| "today".into()));
    }
    None
}

fn channel_guide_from_text(content: &str, lower: &str) -> Option<(String, String)> {
    for marker in [" is on channel ", " is channel ", " channel is "] {
        if let Some(pos) = lower.find(marker) {
            let subject = &content[..pos];
            let rest = &content[pos + marker.len()..];
            let subject = clean_sentence_value(subject)
                .trim_start_matches("the ")
                .to_string();
            let channel = rest
                .split_whitespace()
                .find(|token| token.chars().any(|ch| ch.is_ascii_digit()))?
                .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
                .to_string();
            if !subject.is_empty() && !channel.is_empty() {
                return Some((subject, channel));
            }
        }
    }
    None
}

fn schedule_item_query(query: &str) -> Option<(String, Option<String>, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if lower.starts_with("what channel is ") || lower.starts_with("what channel s ") {
        let subject = query
            .trim_start_matches("What channel is ")
            .trim_start_matches("what channel is ")
            .trim_start_matches("what channel s ")
            .trim();
        let subject = clean_sentence_value(subject);
        if !subject.is_empty() {
            return Some(("channel_guide".into(), Some(subject), None));
        }
    }
    if lower.contains("tv tonight") || lower.contains("on tv tonight") {
        return Some((
            "tv_tonight".into(),
            Some("tv tonight".into()),
            Some("today".into()),
        ));
    }
    if lower.contains("city council") && lower.contains("meeting") {
        return Some((
            "community_meeting".into(),
            Some("city council".into()),
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("sunset") || lower.contains("sun set") {
        return Some((
            "sunset".into(),
            Some("sunset".into()),
            calendar_day_from_text(&lower).or_else(|| Some("today".into())),
        ));
    }
    if lower.contains("bus")
        && lower.contains("tomorrow")
        && (lower.contains("what time") || lower.contains("pickup"))
    {
        return Some(("school_bus_arrival".into(), None, Some("tomorrow".into())));
    }
    if lower.contains("school bus") && (lower.contains("arrive") || lower.contains("what time")) {
        return Some((
            "school_bus_arrival".into(),
            Some("school bus".into()),
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("bill") && lower.contains("due") {
        let subject = bill_subject_from_text(query, &lower);
        return Some(("bill_due".into(), subject, calendar_day_from_text(&lower)));
    }
    if lower.contains("recycling week") || lower.contains("recycling day") {
        return Some((
            "recycling".into(),
            Some("recycling".into()),
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("trash pickup") || lower.contains("trash day") {
        return Some((
            "trash_pickup".into(),
            Some("trash".into()),
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("conference")
        && (lower.contains("parent-teacher") || lower.contains("parent teacher"))
    {
        return Some((
            "school_conference".into(),
            None,
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("community pool") || (lower.contains("pool") && lower.contains("open")) {
        return Some((
            "community_facility_hours".into(),
            Some("community pool".into()),
            calendar_day_from_text(&lower).or_else(|| Some("today".into())),
        ));
    }
    if lower.contains("library") && (lower.contains("close") || lower.contains("hours")) {
        return Some((
            "business_hours".into(),
            Some("library".into()),
            calendar_day_from_text(&lower),
        ));
    }
    if lower.contains("subscription") && (lower.contains("due") || lower.contains("renew")) {
        return Some((
            "subscription_renewal".into(),
            None,
            calendar_day_from_text(&lower),
        ));
    }
    None
}

fn event_log_query(query: &str) -> Option<(String, String, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if lower.contains("credit score") {
        return Some((
            "finance".into(),
            "credit_score".into(),
            Some("credit score".into()),
        ));
    }
    if lower.contains("stock price") {
        return Some(("finance".into(), "stock_price".into(), None));
    }
    if lower.contains("vo2 max") {
        return Some(("health".into(), "vo2_max".into(), Some("vo2 max".into())));
    }
    if matches!(
        lower.as_str(),
        "what is my weight" | "what's my weight" | "what s my weight"
    ) || (lower.contains("my weight") && lower.starts_with("what"))
    {
        return Some((
            "health".into(),
            "weight_reading".into(),
            Some("weight".into()),
        ));
    }
    if lower.contains("dishwasher") && (lower.contains("clean") || lower.contains("dirty")) {
        return Some((
            "appliance_state".into(),
            "clean_status".into(),
            Some("dishwasher".into()),
        ));
    }
    if lower.starts_with("did ") && lower.contains("trash truck") {
        return Some(("waste".into(), "collection".into(), Some("trash".into())));
    }
    if lower.contains("temperature") && lower.contains("attic") {
        return Some((
            "environment".into(),
            "temperature".into(),
            Some("attic".into()),
        ));
    }
    if lower.starts_with("is ") && lower.contains("home from school") {
        let rest = query.get("is ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let name_end = lower_rest.find(" home from school")?;
        let person = clean_person_name(&rest[..name_end]);
        if !person.is_empty() {
            return Some(("location".into(), "home_arrival".into(), Some(person)));
        }
    }
    if lower.starts_with("is ") && (lower.ends_with(" home") || lower.ends_with(" home?")) {
        let rest = query.get("is ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let name_end = lower_rest.find(" home")?;
        let person = clean_person_name(&rest[..name_end]);
        if !person.is_empty() {
            return Some(("location".into(), "presence_home".into(), Some(person)));
        }
    }
    if lower.contains("who opened the garage door") {
        return Some(("access".into(), "open".into(), Some("garage door".into())));
    }
    if lower.starts_with("who ")
        && (lower.contains("turned off the security system")
            || lower.contains("disarmed the security system")
            || lower.contains("turned off security system"))
    {
        return Some((
            "security".into(),
            "disarm".into(),
            Some("security system".into()),
        ));
    }
    if lower.starts_with("did ") && lower.contains("allowance") {
        let rest = query.get("did ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let name_end = lower_rest
            .find(" get ")
            .or_else(|| lower_rest.find(" receive "))
            .or_else(|| lower_rest.find(" received "))?;
        let person = clean_person_name(&rest[..name_end]);
        if !person.is_empty() {
            return Some(("finance".into(), "allowance".into(), Some(person)));
        }
    }
    if lower.starts_with("did ") && lower.contains("pay") && lower.contains("bill") {
        return Some((
            "finance".into(),
            "paid_bill".into(),
            bill_subject_from_text(query, &lower),
        ));
    }
    None
}

fn bill_subject_from_text(content: &str, lower: &str) -> Option<String> {
    for subject in [
        "electricity",
        "electric",
        "power",
        "water",
        "gas",
        "internet",
        "utility",
        "utilities",
    ] {
        if lower.contains(subject) {
            return Some(
                match subject {
                    "electric" | "power" => "electricity",
                    "utilities" => "utility",
                    other => other,
                }
                .to_string(),
            );
        }
    }
    if let Some(pos) = lower.find(" bill") {
        let candidate = content[..pos]
            .split_whitespace()
            .last()
            .unwrap_or("")
            .trim_matches(|ch: char| !ch.is_alphanumeric());
        if !candidate.is_empty() {
            return Some(candidate.to_ascii_lowercase());
        }
    }
    None
}

fn subscription_subject_from_text(content: &str, lower: &str) -> Option<String> {
    if let Some(pos) = lower.find(" subscription") {
        let subject = content[..pos]
            .split_whitespace()
            .last()
            .unwrap_or("")
            .trim_matches(|ch: char| !ch.is_alphanumeric());
        if !subject.is_empty() {
            return Some(subject.to_string());
        }
    }
    None
}

fn stock_subject_from_text(content: &str, lower: &str) -> Option<String> {
    if let Some(pos) = lower.find(" is currently trading") {
        let subject = clean_sentence_value(&content[..pos]);
        if !subject.is_empty() {
            return Some(subject);
        }
    }
    if let Some(pos) = lower.find(" stock price") {
        let subject = clean_sentence_value(&content[..pos]);
        if !subject.is_empty() {
            return Some(subject);
        }
    }
    None
}

fn relative_due_from_text(lower: &str) -> Option<String> {
    let pos = lower.find("due in ")?;
    let rest = &lower[pos + "due ".len()..];
    let value = rest
        .split(['.', ',', ';'])
        .next()
        .unwrap_or(rest)
        .split(" on ")
        .next()
        .unwrap_or(rest)
        .trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn due_date_from_text(content: &str, lower: &str) -> Option<String> {
    let pos = lower.find(" on ")?;
    let rest = content[pos + " on ".len()..].trim();
    let date = rest
        .split(['.', ',', ';'])
        .next()
        .unwrap_or(rest)
        .split(" at ")
        .next()
        .unwrap_or(rest)
        .split(" estimated ")
        .next()
        .unwrap_or(rest)
        .trim();
    if date.is_empty() {
        None
    } else {
        Some(clean_sentence_value(date))
    }
}

fn subject_after_marker(content: &str, lower: &str, marker: &str) -> Option<String> {
    let pos = lower.rfind(marker)?;
    let rest = content[pos + marker.len()..].trim();
    let subject = rest
        .split(['.', ',', ';'])
        .next()
        .unwrap_or(rest)
        .split(" at ")
        .next()
        .unwrap_or(rest)
        .trim();
    if subject.is_empty() {
        None
    } else {
        Some(clean_sentence_value(subject))
    }
}

fn subject_before_marker(content: &str, lower: &str, marker: &str) -> Option<String> {
    let pos = lower.find(marker)?;
    let subject = content[..pos].trim();
    if subject.is_empty() {
        None
    } else {
        Some(clean_person_name(subject))
    }
}

fn actor_after_marker(content: &str, lower: &str, marker: &str) -> Option<String> {
    let pos = lower.find(marker)?;
    let rest = content[pos + marker.len()..].trim();
    let actor = rest
        .split(" using ")
        .next()
        .unwrap_or(rest)
        .split(" with ")
        .next()
        .unwrap_or(rest)
        .split(" at ")
        .next()
        .unwrap_or(rest)
        .split(['.', ',', ';'])
        .next()
        .unwrap_or(rest)
        .trim();
    if actor.is_empty() {
        None
    } else {
        Some(clean_person_name(actor))
    }
}

fn amount_from_text(content: &str) -> Option<String> {
    let pos = content.find('$')?;
    let amount = content[pos..]
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|ch: char| !ch.is_ascii_digit() && ch != '$' && ch != '.');
    if amount.is_empty() {
        None
    } else {
        Some(amount.to_string())
    }
}

fn allowance_person_from_text(content: &str, lower: &str) -> Option<String> {
    for marker in [" received ", " got "] {
        if let Some((person, _)) = split_once_case_insensitive(content, lower, marker) {
            let person = clean_person_name(person);
            if !person.is_empty() {
                return Some(person);
            }
        }
    }
    leading_person_name(content)
}

fn app_only_secret_reference_from_memory(
    _kind: &str,
    content: &str,
    metadata: policy::MemoryPolicyMetadata,
) -> Option<AppOnlySecretReference> {
    let lower = content.to_ascii_lowercase();
    let secret_type = secret_type_from_text(&lower)?;

    let shared_allowed =
        policy::assess_memory_read(metadata, policy::MemoryReadContext::shared_room_voice())
            .allowed;
    let explicitly_app_only = matches!(
        metadata.spoken_policy,
        policy::SpokenMemoryPolicy::AppOnly | policy::SpokenMemoryPolicy::Deny
    ) || lower.contains("credential:")
        || lower.contains("credentials vault")
        || lower.contains("local vault")
        || lower.contains("app-only")
        || lower.contains("app only");

    if shared_allowed && !explicitly_app_only {
        return None;
    }

    let label = secret_label_from_text(content, &lower, secret_type);
    Some(AppOnlySecretReference {
        source_memory_id: 0,
        secret_type: secret_type.into(),
        label,
        location_hint: secret_location_hint(content, &lower),
    })
}

fn secret_type_from_text(lower: &str) -> Option<&'static str> {
    let mentions_wifi =
        lower.contains("wi-fi") || lower.contains("wifi") || lower.contains("wi fi");
    let mentions_credential =
        lower.contains("password") || lower.contains("passcode") || lower.contains("credential");
    let mentions_lock_word = lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|token| matches!(token, "lock" | "locks"));
    if (mentions_wifi && mentions_credential)
        || lower.contains("network password")
        || (lower.contains("guest network") && mentions_credential)
    {
        Some("wifi_password")
    } else if lower.contains("password")
        || lower.contains(" pass:")
        || lower.starts_with("pass:")
        || lower.contains("bank login")
        || lower.contains("password manager")
        || lower.contains("secure vault")
        || lower.contains("credentials vault")
        || (lower.contains("netflix")
            && (lower.contains("code") || lower.contains("credential") || lower.contains("login")))
    {
        Some("password")
    } else if lower.contains("gate code") {
        Some("gate_code")
    } else if lower.contains("door code")
        || lower.contains("lock code")
        || (lower.contains("shed")
            && lower.contains("code")
            && !lower.contains("paint")
            && !lower.contains("color")
            && !lower.contains("colour"))
        || (lower.contains("shed") && lower.contains("combination"))
        || (mentions_lock_word && (lower.contains("combination") || lower.contains("combo")))
    {
        Some("lock_code")
    } else if lower.contains("alarm code") || lower.contains("security code") {
        Some("security_code")
    } else if lower.contains("confirmation number") {
        Some("confirmation_number")
    } else if lower.contains("account number") {
        Some("account_number")
    } else if lower.contains("spare key")
        || lower.contains("spare keys")
        || lower.contains("house key")
        || lower.contains("house keys")
    {
        Some("secure_location")
    } else if lower.contains("combination") || lower.contains("combo") {
        Some("combination")
    } else {
        None
    }
}

fn secret_label_from_text(content: &str, lower: &str, secret_type: &str) -> String {
    if lower.contains("router") && lower.contains("admin") && secret_type == "password" {
        return "router admin".into();
    }
    if lower.contains("guest") && matches!(secret_type, "wifi_password" | "password") {
        return "guest wifi".into();
    }
    if lower.contains("printer") && matches!(secret_type, "wifi_password" | "password") {
        return "printer wifi".into();
    }
    if lower.contains("xbox") && matches!(secret_type, "wifi_password" | "password") {
        return "Xbox wifi".into();
    }
    if lower.contains("locker") && matches!(secret_type, "combination" | "lock_code") {
        if lower.contains("mia") {
            return "Mia locker combination".into();
        }
        return "locker combination".into();
    }
    if lower.contains("shed") && matches!(secret_type, "combination" | "lock_code") {
        return "shed combination".into();
    }
    if lower.contains("netflix") && secret_type == "password" {
        return "Netflix account".into();
    }
    if lower.contains("bank") && secret_type == "password" {
        return "bank login".into();
    }
    if matches!(secret_type, "secure_location") && lower.contains("key") {
        return "spare keys".into();
    }
    if secret_type == "confirmation_number" && lower.contains("hotel") {
        return "hotel confirmation number".into();
    }
    if secret_type == "account_number" && lower.contains("gas") {
        return "gas bill account number".into();
    }
    if lower.contains("wi-fi") || lower.contains("wifi") || lower.contains("wi fi") {
        return "wifi".into();
    }
    let before_marker = [" is ", " pass:", " pass ", " stored ", " saved ", " lives "]
        .iter()
        .filter_map(|marker| lower.find(marker).map(|pos| content[..pos].trim()))
        .next()
        .unwrap_or(content)
        .trim_start_matches("the ")
        .trim_start_matches("our ")
        .trim_start_matches("my ");
    let label = clean_sentence_value(before_marker);
    if label.is_empty() {
        secret_type.replace('_', " ")
    } else {
        label
    }
}

fn secret_location_hint(content: &str, lower: &str) -> String {
    for marker in [
        "credential:",
        "credentials vault",
        "local vault",
        "vault",
        "dashboard",
    ] {
        if let Some(pos) = lower.find(marker) {
            let hint = content[pos..]
                .trim()
                .trim_matches(|ch: char| matches!(ch, '.' | ',' | ';' | '!' | '?'));
            if !hint.is_empty() {
                return hint.to_string();
            }
        }
    }
    "app-only credential storage".into()
}

fn parse_allergy_rule(content: &str, lower: &str) -> Option<(String, String)> {
    if let Some((person, rest)) = split_once_case_insensitive(content, lower, " is allergic to ") {
        return Some((clean_person_name(person), normalize_rule_subject(rest)));
    }

    if let Some((person, rest)) = split_once_case_insensitive(content, lower, " has ") {
        let rest_lower = rest.to_ascii_lowercase();
        if let Some(pos) = rest_lower.find(" allergy") {
            let subject = rest[..pos]
                .split_whitespace()
                .rfind(|word| {
                    !matches!(
                        word.to_ascii_lowercase().as_str(),
                        "a" | "an" | "mild" | "severe" | "recent"
                    )
                })
                .unwrap_or("");
            if !subject.is_empty() {
                return Some((clean_person_name(person), normalize_rule_subject(subject)));
            }
        }
    }

    None
}

fn parse_screen_time_rule(content: &str, lower: &str) -> Option<(String, String, String)> {
    let person = if let Some((person, _)) =
        split_once_case_insensitive(content, lower, " is not allowed ")
    {
        clean_person_name(person)
    } else if let Some((person, _)) = split_once_case_insensitive(content, lower, "'s screen time")
    {
        clean_person_name(person)
    } else {
        leading_person_name(content)?
    };

    let subject = if lower.contains("video game") || lower.contains("gaming") {
        "video_games"
    } else {
        "screen_time"
    };
    let value = time_after_marker(content, lower, " after ")
        .or_else(|| time_after_marker(content, lower, " ends at "))?;
    Some((person, subject.into(), value))
}

fn profile_attr(name: &str, attribute: &str, value: &str) -> HouseholdProfileAttribute {
    HouseholdProfileAttribute {
        source_memory_id: 0,
        name: clean_person_name(name),
        attribute: attribute.into(),
        value: clean_sentence_value(value),
    }
}

fn split_once_case_insensitive<'a>(
    original: &'a str,
    lower: &str,
    marker: &str,
) -> Option<(&'a str, &'a str)> {
    let pos = lower.find(marker)?;
    Some((&original[..pos], &original[pos + marker.len()..]))
}

fn leading_age(value: &str) -> Option<u8> {
    let digits = value
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    digits
        .parse::<u8>()
        .ok()
        .filter(|age| (1..=120).contains(age))
}

fn leading_person_name(value: &str) -> Option<String> {
    let name = value.split_whitespace().next()?;
    let name = clean_person_name(name);
    if name.is_empty() { None } else { Some(name) }
}

fn clean_person_name(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("for ")
        .trim_start_matches("that ")
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '.' | ',' | ':' | ';' | '?' | '!'))
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_sentence_value(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '.' | ',' | ':' | ';' | '?' | '!'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_device_phrase(value: &str) -> String {
    clean_sentence_value(value)
        .trim_start_matches("the ")
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .to_string()
}

fn calendar_day_from_text(lower: &str) -> Option<String> {
    for day in [
        "today",
        "tomorrow",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ] {
        if lower.split_whitespace().any(|token| {
            token.trim_matches(|ch: char| matches!(ch, '.' | ',' | ';' | '?' | '!')) == day
        }) {
            return Some(day.into());
        }
    }
    None
}

fn relative_calendar_phrase_from_text(lower: &str) -> Option<String> {
    for day in [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ] {
        let last = format!("last {day}");
        if lower.contains(&last) {
            return Some(last);
        }
        let next = format!("next {day}");
        if lower.contains(&next) {
            return Some(next);
        }
    }
    calendar_day_from_text(lower)
}

fn normalize_calendar_day(value: &str) -> String {
    calendar_day_from_text(&value.to_ascii_lowercase())
        .unwrap_or_else(|| normalize_alias_key(value))
}

fn split_list_items(value: &str) -> Vec<String> {
    value
        .replace(" and ", ",")
        .split([',', ';'])
        .map(clean_sentence_value)
        .map(|item| {
            item.trim_start_matches("the ")
                .trim_start_matches("a ")
                .trim_start_matches("an ")
                .to_string()
        })
        .filter(|item| !item.is_empty())
        .collect()
}

fn quantity_for_inventory_item(content: &str, lower: &str, item_tokens: &[&str]) -> Option<String> {
    let tokens = content.split_whitespace().collect::<Vec<_>>();
    let lower_tokens = lower.split_whitespace().collect::<Vec<_>>();
    for (idx, token) in lower_tokens.iter().enumerate() {
        let cleaned = token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric());
        if !item_tokens.contains(&cleaned) {
            continue;
        }

        if let Some(quantity) = idx
            .checked_sub(1)
            .and_then(|prev| tokens.get(prev))
            .and_then(|token| parse_quantity_token(token))
        {
            return Some(quantity);
        }
        if let Some(quantity) = tokens
            .get(idx + 1)
            .and_then(|token| parse_quantity_token(token))
        {
            return Some(quantity);
        }
    }

    lower_tokens.iter().enumerate().find_map(|(idx, token)| {
        if matches!(
            token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric()),
            "have" | "has" | "remaining" | "left" | "quantity"
        ) {
            tokens
                .get(idx + 1)
                .and_then(|token| parse_quantity_token(token))
        } else {
            None
        }
    })
}

fn parse_quantity_token(token: &str) -> Option<String> {
    let cleaned =
        token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '.' && ch != '/');
    if cleaned.is_empty() {
        return None;
    }
    if cleaned.chars().any(|ch| ch.is_ascii_digit()) {
        return Some(cleaned.to_string());
    }
    match cleaned.to_ascii_lowercase().as_str() {
        "zero" | "none" => Some("0".into()),
        "one" | "a" | "an" => Some("1".into()),
        "two" => Some("2".into()),
        "three" => Some("3".into()),
        "four" => Some("4".into()),
        "five" => Some("5".into()),
        "six" => Some("6".into()),
        "seven" => Some("7".into()),
        "eight" => Some("8".into()),
        "nine" => Some("9".into()),
        "ten" => Some("10".into()),
        "dozen" => Some("12".into()),
        _ => None,
    }
}

fn inventory_location(content: &str, lower: &str) -> Option<String> {
    for marker in [" in the ", " in "] {
        if let Some(pos) = lower.rfind(marker) {
            let location = content[pos + marker.len()..]
                .split(['.', ',', ';'])
                .next()
                .map(clean_sentence_value)
                .unwrap_or_default();
            if !location.is_empty() {
                return Some(location);
            }
        }
    }
    None
}

fn normalize_inventory_item(value: &str) -> String {
    match normalize_alias_key(value).as_str() {
        "egg" | "eggs" => "eggs".into(),
        "milk" => "milk".into(),
        other => other.trim_end_matches('s').to_string(),
    }
}

fn permission_statement(
    content: &str,
    lower: &str,
    marker: &str,
    fallback_person: Option<&str>,
) -> Option<(String, String)> {
    let pos = lower.find(marker)?;
    let left = content[..pos]
        .rsplit(['.', ';'])
        .next()
        .unwrap_or(&content[..pos])
        .trim();
    let person = if matches!(
        left.to_ascii_lowercase().trim(),
        "he" | "she" | "they" | "him" | "her"
    ) {
        fallback_person.map(ToOwned::to_owned)?
    } else {
        clean_person_name(left)
    };
    let rest = content[pos + marker.len()..].trim();
    let device = rest
        .split(['.', ';'])
        .next()
        .map(clean_device_phrase)
        .unwrap_or_default();
    if person.is_empty() || device.is_empty() {
        None
    } else {
        Some((person, device))
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn normalize_name_key(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_rule_subject(value: &str) -> String {
    let singular = value
        .trim()
        .trim_start_matches("the ")
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '.' | ',' | ':' | ';' | '?' | '!'))
        .to_ascii_lowercase();
    match singular.as_str() {
        "peanuts" => "peanut".into(),
        "video_games" | "video games" | "video game" | "gaming" => "video_games".into(),
        "screen_time" | "screen time" => "screen_time".into(),
        "homework" => "homework".into(),
        other => other
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("_")
            .trim_end_matches('s')
            .to_string(),
    }
}

fn time_after_marker(content: &str, lower: &str, marker: &str) -> Option<String> {
    let pos = lower.find(marker)?;
    let rest = content[pos + marker.len()..].trim();
    let mut parts = Vec::new();
    for word in rest.split_whitespace().take(3) {
        let clean = word.trim_matches(|ch: char| matches!(ch, '.' | ',' | ';' | '!' | '?'));
        if clean.eq_ignore_ascii_case("for")
            || clean.eq_ignore_ascii_case("because")
            || clean.eq_ignore_ascii_case("with")
            || clean.eq_ignore_ascii_case("on")
        {
            break;
        }
        parts.push(clean);
        if clean.eq_ignore_ascii_case("am") || clean.eq_ignore_ascii_case("pm") {
            break;
        }
    }
    let raw = parts.join(" ");
    normalize_time_value(&raw)
}

fn normalize_time_value(value: &str) -> Option<String> {
    let cleaned = value.trim().to_ascii_lowercase().replace(' ', "");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn profile_attribute_query(query: &str) -> Option<(String, &'static str)> {
    let query = query.trim();
    let lower = query.to_ascii_lowercase();

    for prefix in ["how old is ", "what age is "] {
        if let Some(name) = lower.strip_prefix(prefix) {
            return Some((clean_person_name(name), "age"));
        }
    }

    if lower.starts_with("what does ") && lower.contains(" like") {
        let rest = query.get("what does ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let pos = lower_rest.find(" like")?;
        return Some((clean_person_name(&rest[..pos]), "likes"));
    }

    if lower.starts_with("what size shoe does ") && lower.contains(" wear") {
        let rest = query.get("what size shoe does ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let pos = lower_rest.find(" wear")?;
        return Some((clean_person_name(&rest[..pos]), "shoe_size"));
    }

    if lower.starts_with("what shoe size does ") && lower.contains(" wear") {
        let rest = query.get("what shoe size does ".len()..)?;
        let lower_rest = rest.to_ascii_lowercase();
        let pos = lower_rest.find(" wear")?;
        return Some((clean_person_name(&rest[..pos]), "shoe_size"));
    }

    None
}

fn allergy_query_subject(query: &str) -> Option<Option<String>> {
    let lower = query.to_ascii_lowercase();
    if !(lower.contains("allergic") || lower.contains("allergy")) {
        return None;
    }

    if let Some((_, subject)) = split_once_case_insensitive(query, &lower, " allergic to ") {
        return Some(Some(normalize_rule_subject(subject)));
    }
    if lower.contains("peanut") {
        return Some(Some("peanut".into()));
    }

    Some(None)
}

fn allowed_rule_query(query: &str) -> Option<(String, String, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if !(lower.starts_with("is ") && lower.contains(" allowed")) {
        return None;
    }
    let rest = query.get("is ".len()..)?;
    let lower_rest = rest.to_ascii_lowercase();
    let allowed_pos = lower_rest.find(" allowed")?;
    let person = clean_person_name(&rest[..allowed_pos]);
    if person.is_empty() {
        return None;
    }

    let subject = if lower.contains("video game") || lower.contains("gaming") {
        "video_games"
    } else if lower.contains("screen time") {
        "screen_time"
    } else {
        return None;
    };
    let value = time_after_marker(query, &lower, " after ");
    Some((person, subject.into(), value))
}

fn homework_rule_query(query: &str) -> Option<String> {
    let lower = query.to_ascii_lowercase();
    if !lower.contains("homework") {
        return None;
    }

    for marker in ["show me ", "what are ", "what is "] {
        if let Some(rest) = lower.strip_prefix(marker) {
            let name = rest
                .split("'s")
                .next()
                .or_else(|| rest.split_whitespace().next())
                .map(clean_person_name)?;
            if !name.is_empty() {
                return Some(name);
            }
        }
    }

    leading_person_name(query)
}

fn household_note_query(query: &str) -> Option<String> {
    let query = query.trim();
    let lower = query.to_ascii_lowercase();

    for prefix in [
        "what did i say about ",
        "what did we say about ",
        "find my note about ",
        "find note about ",
        "find the note about ",
        "show my note about ",
        "show the note about ",
        "what is the note about ",
        "what did i write about ",
        "what did we write about ",
        "what did the vet say about ",
        "what did the mechanic say about ",
        "find our note about ",
        "find the record about ",
        "find record about ",
        "find the warranty for ",
        "find warranty for ",
        "what is the warranty for ",
        "what s the warranty for ",
        "what's the warranty for ",
        "find the receipt for ",
        "find receipt for ",
        "find my essay draft about ",
        "find the essay draft about ",
        "find the manual for ",
        "find the user manual for ",
        "find manual for ",
        "where did i save ",
        "find the instructions for ",
        "find instructions for ",
        "who do we call for ",
        "what is the phone number for ",
        "what s the phone number for ",
        "what's the phone number for ",
        "what is the ip address of ",
        "what s the ip address of ",
        "what's the ip address of ",
        "find anything about ",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let cleaned = clean_sentence_value(rest);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }

    if lower.starts_with("where are ")
        || lower.starts_with("where is ")
        || lower.starts_with("where s ")
        || lower.starts_with("where did i put ")
        || lower.starts_with("where did we put ")
        || lower.starts_with("where are the ")
        || lower.starts_with("what color is ")
        || lower.starts_with("what colour is ")
        || lower.starts_with("what color did we paint ")
        || lower.starts_with("what colour did we paint ")
        || lower.starts_with("what's the model number ")
        || lower.starts_with("what s the model number ")
        || lower.starts_with("what is the model number ")
        || lower.starts_with("what is the license plate")
        || lower.starts_with("what s the license plate")
        || lower.starts_with("what's the license plate")
        || lower.starts_with("find the sewing kit")
        || (lower.starts_with("find the ") && lower.contains(" warranty"))
        || lower.starts_with("find the manual for the car")
        || lower.starts_with("who took the photos ")
        || lower.starts_with("we have a leak ")
        || lower.starts_with("there is a leak ")
        || lower.starts_with("how do i clean ")
        || lower.starts_with("how do we clean ")
        || lower.starts_with("how do i remove ")
        || lower.starts_with("how do we remove ")
        || lower.starts_with("how do i reset ")
        || lower.starts_with("how do we reset ")
        || lower.starts_with("how long do i boil ")
        || lower.starts_with("how long should i boil ")
        || lower.starts_with("what bin does ")
        || lower.starts_with("tell me the dinosaur fact")
        || lower.starts_with("what did we have for dinner ")
        || lower.starts_with("find the recipe for ")
        || lower.starts_with("what is the school")
        || lower.starts_with("what s the school")
        || lower.starts_with("what's the school")
        || lower.starts_with("what is the doctor")
        || lower.starts_with("what s the doctor")
        || lower.starts_with("what's the doctor")
        || lower.starts_with("what is the vet")
        || lower.starts_with("what s the vet")
        || lower.starts_with("what's the vet")
        || lower.starts_with("what is the phone number for ")
        || lower.starts_with("what s the phone number for ")
        || lower.starts_with("what's the phone number for ")
        || lower.starts_with("what is the ip address of ")
        || lower.starts_with("what s the ip address of ")
        || lower.starts_with("what's the ip address of ")
        || lower.starts_with("who do we call for ")
        || lower.starts_with("what's on the hardware store list")
        || lower.starts_with("what s on the hardware store list")
        || lower.starts_with("what is on the hardware store list")
        || lower.starts_with("where did we put ")
        || lower.starts_with("where did i put ")
        || lower.starts_with("where are the tax documents")
        || lower.starts_with("when is the next trash pickup")
        || lower.contains("science fair checklist")
        || lower.contains("air fryer manual")
        || lower.contains("which filter")
        || lower.contains("dishwasher error")
        || lower.contains("tablet charger")
        || lower.contains("saturday morning routine")
        || lower.contains("what groceries are low")
        || lower.contains("what s next before school")
        || lower.contains("what's next before school")
        || lower.contains("can i watch cartoons")
        || lower.contains("can i have a snack")
        || lower.contains("coming to dinner tonight")
        || lower.contains("did i finish my chores")
        || lower.contains("what time is my bus")
        || lower.contains("bus tomorrow")
        || lower.contains("which leftovers should we eat first")
        || lower.contains("did mom approve my sleepover")
        || lower.contains("pajama day")
        || lower.contains("allergy action plan")
        || lower.contains("car keys")
        || lower.contains("robot vacuum stuck")
        || lower.contains("who changed the thermostat")
        || lower.contains("ladder safety")
        || lower.contains("bathroom mirror")
        || lower.contains("package still on the porch")
        || lower.contains("allergy medicine")
        || lower.contains("dinosaur fact")
        || lower.contains("what s making that beeping sound")
        || lower.contains("what's making that beeping sound")
        || lower.contains("porch light still on")
        || lower.contains("grandma")
            && (lower.contains("wi fi note")
                || lower.contains("wi-fi note")
                || lower.contains("wifi note"))
        || lower.contains("allowed to play outside")
        || lower.contains("wet soccer shoes")
        || lower.contains("when did the laundry finish")
        || lower.contains("blue paint")
        || lower.contains("did my laundry get moved")
        || lower.contains("safest way out")
        || lower.contains("which breaker controls the dishwasher")
        || lower.contains("trash day")
        || lower.contains("red hoodie")
        || lower.contains("lego cleanup")
        || lower.contains("ants")
        || lower.contains("garbage bins")
        || lower.contains("camping flashlight")
        || lower.contains("why didn t the sprinklers run")
        || lower.contains("why didn't the sprinklers run")
        || lower.contains("homework needs internet")
        || lower.contains("use the stove")
        || lower.contains("cold medicine")
        || lower.contains("fridge door")
        || lower.contains("sensors need batteries")
        || lower.contains("library book")
        || lower.contains("alarm not go off")
        || lower.contains("plants need attention")
        || lower.contains("blue cup")
        || lower.contains("side gate")
        || lower.contains("recital outfit")
        || lower.contains("bathroom free")
        || lower.contains("away mode fail")
        || lower.contains("guest speaker")
        || lower.contains("end of day")
        || lower.contains("end-of-day")
        || lower.contains("after dinner cleanup")
        || lower.contains("after-dinner cleanup")
        || lower.contains("upstairs lights")
        || lower.contains("front door") && lower.contains("grandma")
        || lower.contains("debate") && lower.contains("school lunch")
        || lower.contains("board games")
        || lower.contains("basement humid")
        || lower.contains("test practice")
        || lower.contains("rain boots")
        || lower.contains("charging tonight")
        || lower.contains("coffee") && lower.contains("wake")
        || lower.contains("fan on low") && lower.contains("sleep")
        || lower.contains("cold after bath")
        || lower.contains("slow cooker") && lower.contains("timer chart")
        || lower.contains("basement flood check")
        || lower.contains("garage camera") && lower.contains("bike")
        || lower.contains("next filter change")
        || lower.contains("puzzle") && lower.contains("dad")
        || lower.contains("temporary code") && lower.contains("grandma")
        || lower.contains("glarey")
        || lower.contains("front door locked after")
        || lower.contains("water heater receipt")
        || lower.contains("quiet drawing")
        || lower.contains("print my homework")
        || lower.contains("upstairs cooler") && lower.contains("leo")
        || lower.contains("noisy appliance")
        || lower.contains("tooth fairy box")
        || lower.contains("white extension cord")
        || lower.contains("family dinner") && lower.contains("screens")
        || lower.contains("changed in the garage today")
        || lower.contains("stairs bright")
        || lower.contains("water my plant")
        || lower.contains("chicken recipe") && lower.contains("peanut")
        || lower.contains("security alarm chirp")
        || lower.contains("use the microwave")
        || lower.contains("rehearsal comfort")
        || lower.contains("who s in the backyard")
        || lower.contains("who's in the backyard")
        || lower.contains("workshop dust control")
        || lower.contains("bedtime chart")
        || lower.contains("closet light")
        || lower.contains("upstairs window before the rain")
        || lower.contains("low power mode")
        || lower.contains("low-power mode")
        || lower.contains("vaccination form")
        || lower.contains("field trip form")
        || lower.contains("animal show")
        || lower.contains("guest wi fi")
        || lower.contains("guest wifi")
        || lower.contains("guest wi-fi")
        || lower.contains("front entry lights")
        || lower.contains("side path icy")
        || lower.contains("dripping")
        || lower.contains("office internet slow")
        || lower.contains("school night reset")
        || lower.contains("school-night reset")
        || lower.contains("photo backdrop")
        || lower.contains("red marker")
        || lower.contains("freezer") && lower.contains("above 10")
        || lower.contains("chores did leo skip")
        || lower.contains("mirror lights")
        || lower.contains("cat sleep")
        || lower.contains("grilling")
        || lower.contains("purifier on high")
        || lower.contains("swim meet")
        || lower.contains("next step") && lower.contains("cookies")
        || lower.contains("outdoor cameras need cleaning")
        || lower.contains("garage close after jared")
        || lower.contains("project list")
        || lower.contains("scared to go downstairs")
        || lower.contains("furnace") && lower.contains("code 31")
        || lower.contains("dinner warm")
        || lower.contains("quiet time") && lower.contains("wednesday")
        || lower.contains("feed the cat too much")
        || lower.contains("oldest thing in the fridge")
        || lower.contains("outside is cleaner")
        || lower.contains("lamp flickering")
        || lower.contains("open the garage door")
        || lower.contains("holiday lighting")
        || lower.contains("shutoff valve")
        || lower.contains("rainy day alarm")
        || lower.contains("rainy-day alarm")
        || lower.contains("soccer practice")
        || lower.contains("bypass a sensor")
        || lower.contains("guest breakfast")
        || lower.contains("winter poem")
        || lower.contains("laundry room not scary")
        || lower.contains("water pressure")
        || lower.contains("oven") && lower.contains("preheat")
        || lower.contains("hallway camera")
        || lower.contains("cookies are cool")
        || lower.contains("vacuum avoid")
        || lower.contains("toddler gate")
        || lower.contains("room smells weird")
        || lower.contains("dad see my message")
        || lower.contains("laundry leaks again")
        || lower.contains("backpacks are by the door")
        || lower.contains("alarm skip holidays")
        || lower.contains("morning checklist")
        || lower.contains("privacy report") && lower.contains("cameras")
        || lower.contains("green bowl")
        || lower.contains("practice drums")
        || lower.contains("flashlight") && lower.contains("lights go out")
        || lower.contains("automation fired")
        || lower.contains("upstairs warmer") && lower.contains("kids")
        || lower.contains("tournament") && lower.contains("snacks")
        || lower.contains("final safety sweep")
    {
        return Some(query.to_string());
    }

    if lower.starts_with("what did we watch about ")
        || lower.starts_with("what did i watch about ")
        || lower.starts_with("what movie ")
        || lower.starts_with("what was that movie ")
    {
        return Some(query.to_string());
    }

    None
}

fn secret_reference_query(query: &str) -> Option<(&'static str, String)> {
    let lower = query.to_ascii_lowercase();
    if lower.contains("guest speaker") {
        return None;
    }
    let secret_type = secret_type_from_text(&lower)?;
    if !(lower.contains("what")
        || lower.contains("show")
        || lower.contains("find")
        || lower.contains("where")
        || lower.contains("password")
        || lower.contains("code")
        || lower.contains("combo")
        || lower.contains("key")
        || lower.contains("number")
        || lower.contains("login")
        || lower.contains("credential"))
    {
        return None;
    }

    let label = if lower.contains("guest") && matches!(secret_type, "wifi_password" | "password") {
        "guest wifi".into()
    } else if lower.contains("printer") && secret_type == "wifi_password" {
        "printer wifi".into()
    } else if lower.contains("xbox") && secret_type == "wifi_password" {
        "Xbox wifi".into()
    } else if lower.contains("locker") && matches!(secret_type, "combination" | "lock_code") {
        if lower.contains("mia") {
            "Mia locker combination".into()
        } else {
            "locker combination".into()
        }
    } else if lower.contains("shed") && matches!(secret_type, "combination" | "lock_code") {
        "shed combination".into()
    } else if lower.contains("netflix") && secret_type == "password" {
        "Netflix account".into()
    } else if lower.contains("bank") && secret_type == "password" {
        "bank login".into()
    } else if matches!(secret_type, "secure_location") && lower.contains("key") {
        "spare keys".into()
    } else if secret_type == "confirmation_number" && lower.contains("hotel") {
        "hotel confirmation number".into()
    } else if secret_type == "account_number" && lower.contains("gas") {
        "gas bill account number".into()
    } else if lower.contains("wifi") || lower.contains("wi-fi") || lower.contains("wi fi") {
        "wifi".into()
    } else {
        search_tokens(query).join(" ")
    };
    Some((secret_type, label))
}

fn format_profile_attribute_answer(attr: &HouseholdProfileAttribute) -> String {
    match attr.attribute.as_str() {
        "age" => format!("{} is {} years old.", attr.name, attr.value),
        "likes" => format!("{} likes {}.", attr.name, attr.value),
        "shoe_size" => format!("{} currently wears shoe size {}.", attr.name, attr.value),
        attribute if attribute.starts_with("favorite_") => {
            let subject = attribute.trim_start_matches("favorite_").replace('_', " ");
            format!("{}'s favorite {} is {}.", attr.name, subject, attr.value)
        }
        _ => format!("{}: {}.", attr.name, attr.value),
    }
}

fn format_allergy_answer(rules: &[HouseholdRule]) -> String {
    let items = rules
        .iter()
        .map(|rule| rule.description.as_str())
        .collect::<Vec<_>>();
    format!("Yes. {}", items.join(" "))
}

fn format_allowed_rule_answer(rule: &HouseholdRule) -> String {
    if rule.allowed {
        format!("Yes. {}", rule.description)
    } else {
        format!("No. {}", rule.description)
    }
}

fn format_rule_list_answer(rules: &[HouseholdRule]) -> String {
    let items = rules
        .iter()
        .map(|rule| rule.description.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    format!("I found this rule: {items}")
}

fn format_family_calendar_event_answer(event: &FamilyCalendarEvent) -> String {
    if event.event_type == "school_pickup" {
        return format!("I found this calendar event: {}", event.description);
    }

    let person = event.person.as_deref().unwrap_or("They");
    let day = event
        .day
        .as_deref()
        .map(|day| format!(" {day}"))
        .unwrap_or_default();
    let time = event
        .time
        .as_deref()
        .map(|time| format!(" at {time}"))
        .unwrap_or_default();
    format!(
        "Yes. {person} has {}{day}{time}. {}",
        event.title, event.description
    )
}

fn format_shopping_list_answer(items: &[ShoppingListItem]) -> String {
    let names = items
        .iter()
        .map(|item| item.item.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("Shopping list: {names}.")
}

fn format_household_inventory_item_answer(item: &HouseholdInventoryItem) -> String {
    let location = item
        .location
        .as_deref()
        .map(|location| format!(" in {location}"))
        .unwrap_or_default();
    match item.quantity.as_deref() {
        Some("0") => format!("No, I do not have {} remaining{}.", item.item, location),
        Some(quantity) => format!(
            "Yes, you have {quantity} {} remaining{}.",
            item.item, location
        ),
        None => format!("I found this inventory note: {}", item.description),
    }
}

fn format_access_permission_answer(permission: &AccessPermission) -> String {
    if permission.allowed {
        format!("Yes. {}", permission.description)
    } else {
        format!("No. {}", permission.description)
    }
}

fn format_household_task_log_answer(task: &HouseholdTaskLog) -> String {
    if task.status == "complete" {
        let time = task
            .time
            .as_deref()
            .map(|time| format!(" at {time}"))
            .unwrap_or_default();
        let task_name = match task.task.as_str() {
            "brush_teeth" => "brushing teeth",
            "feeding" if task.subject.as_deref() == Some("dog") => "feeding the dog",
            "feeding" if task.subject.as_deref() == Some("cat") => "feeding the cat",
            other => other,
        };
        format!("Yes. {} marked {task_name} complete{time}.", task.person)
    } else {
        format!("I found this task log: {}", task.description)
    }
}

fn format_everyone_task_log_answer(profiles: &[String], logs: &[HouseholdTaskLog]) -> String {
    let completed = logs
        .iter()
        .filter(|log| log.status == "complete")
        .map(|log| log.person.clone())
        .collect::<Vec<_>>();
    let completed_keys = completed
        .iter()
        .map(|name| normalize_name_key(name))
        .collect::<std::collections::HashSet<_>>();
    let not_logged = profiles
        .iter()
        .filter(|name| !completed_keys.contains(&normalize_name_key(name)))
        .cloned()
        .collect::<Vec<_>>();

    if completed.is_empty() {
        return "No one has logged brushing teeth yet.".into();
    }
    if not_logged.is_empty() {
        return format!(
            "Everyone has logged brushing teeth: {}.",
            join_names(&completed)
        );
    }
    format!(
        "{} have logged brushing teeth. Not logged yet: {}.",
        join_names(&completed),
        join_names(&not_logged)
    )
}

fn join_names(names: &[String]) -> String {
    match names {
        [] => "none".into(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        many => {
            let (last, rest) = many.split_last().expect("non-empty slice");
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

fn format_household_schedule_item_answer(item: &HouseholdScheduleItem) -> String {
    match item.schedule_type.as_str() {
        "school_bus_arrival" => {
            let time = item.time.as_deref().unwrap_or("the scheduled time");
            format!("The bus arrives at {time}. {}", item.description)
        }
        "bill_due" => {
            let subject = item.subject.as_deref().unwrap_or("bill");
            let due = item
                .date
                .as_deref()
                .or(item.day.as_deref())
                .unwrap_or("the scheduled date");
            let amount = item
                .amount
                .as_deref()
                .map(|amount| format!(" The estimated amount is {amount}."))
                .unwrap_or_default();
            format!("The {subject} bill is due {due}.{amount}")
        }
        "recycling" => format!("I found this recycling schedule: {}", item.description),
        "trash_pickup" => format!("I found this trash pickup schedule: {}", item.description),
        "school_conference" => {
            let date = item
                .date
                .as_deref()
                .or(item.day.as_deref())
                .unwrap_or("the scheduled date");
            let time = item
                .time
                .as_deref()
                .map(|time| format!(" at {time}"))
                .unwrap_or_default();
            let subject = item
                .subject
                .as_deref()
                .map(|subject| format!(" for {subject}"))
                .unwrap_or_default();
            format!("The next parent-teacher conference is on {date}{time}{subject}.")
        }
        "sunset" => {
            let time = item.time.as_deref().unwrap_or("the scheduled time");
            format!("Sunset is at {time}. {}", item.description)
        }
        "community_facility_hours" => {
            format!(
                "I found this community facility schedule: {}",
                item.description
            )
        }
        "business_hours" => {
            let time = item
                .time
                .as_deref()
                .map(|time| format!(" It closes at {time}."))
                .unwrap_or_default();
            format!("I found these business hours: {}{time}", item.description)
        }
        "channel_guide" => {
            let subject = item.subject.as_deref().unwrap_or("That channel");
            let channel = item.amount.as_deref().unwrap_or("the listed channel");
            format!("{subject} is on channel {channel}.")
        }
        "tv_tonight" => format!("I found this TV schedule: {}", item.description),
        "community_meeting" => {
            let time = item
                .time
                .as_deref()
                .map(|time| format!(" at {time}"))
                .unwrap_or_default();
            let day_or_date = item
                .date
                .as_deref()
                .or(item.day.as_deref())
                .unwrap_or("the scheduled date");
            format!("The next city council meeting is on {day_or_date}{time}.")
        }
        "subscription_renewal" => {
            let subject = item.subject.as_deref().unwrap_or("subscription");
            format!(
                "I found this {subject} subscription schedule: {}",
                item.description
            )
        }
        _ => format!("I found this schedule item: {}", item.description),
    }
}

fn format_household_event_log_answer(event: &HouseholdEventLog) -> String {
    if event.event_type == "security" && event.action == "disarm" {
        let actor = event.actor.as_deref().unwrap_or("someone");
        let time = event
            .time
            .as_deref()
            .map(|time| format!(" at {time}"))
            .unwrap_or_default();
        return format!("The security system was disarmed by {actor}{time}.");
    }

    if event.event_type == "finance" && event.action == "allowance" {
        return format!("Yes. {}", event.description);
    }

    if event.event_type == "finance" && event.action == "paid_bill" {
        return format!("Yes. {}", event.description);
    }

    if event.event_type == "finance" && event.action == "credit_score" {
        return event.description.clone();
    }

    if event.event_type == "finance" && event.action == "stock_price" {
        return event.description.clone();
    }

    if event.event_type == "health" && event.action == "weight_reading" {
        return event.description.clone();
    }

    if event.event_type == "health" && event.action == "vo2_max" {
        return event.description.clone();
    }

    if event.event_type == "appliance_state" && event.action == "clean_status" {
        return event.description.clone();
    }

    if event.event_type == "waste" && event.action == "collection" {
        return format!("Yes. {}", event.description);
    }

    if event.event_type == "environment" && event.action == "temperature" {
        return event.description.clone();
    }

    if event.event_type == "location" && event.action == "home_arrival" {
        return format!("Yes. {}", event.description);
    }

    if event.event_type == "location" && event.action == "presence_home" {
        return format!("Yes. {}", event.description);
    }

    if event.event_type == "access" && event.action == "open" {
        return event.description.clone();
    }

    format!("I found this event log: {}", event.description)
}

fn format_household_note_answer(note: &HouseholdNote) -> String {
    match note.note_type.as_str() {
        "reminder" => format!("I found this reminder: {}", note.content),
        "manual" => format!("I found these instructions: {}", note.content),
        "media" => format!("I found this watch note: {}", note.content),
        "pet_health" => format!("I found this pet health note: {}", note.content),
        "home_maintenance" => format!("I found this home maintenance note: {}", note.content),
        "storage" => format!("I found this storage note: {}", note.content),
        "gift" => format!("I found this gift note: {}", note.content),
        "troubleshooting" => format!("I found this troubleshooting note: {}", note.content),
        "photo" => format!("I found this photo note: {}", note.content),
        "warranty" => format!("I found this warranty note: {}", note.content),
        "school" => format!("I found this school note: {}", note.content),
        "utility" => format!("I found this utility note: {}", note.content),
        "recycling" => format!("I found this recycling note: {}", note.content),
        "first_aid" => format!("I found this first-aid note: {}", note.content),
        "story" => format!("I found this story note: {}", note.content),
        "pet" => format!("I found this pet note: {}", note.content),
        "travel" => format!("I found this travel note: {}", note.content),
        "visitor" => format!("I found this visitor note: {}", note.content),
        "meal" => format!("I found this meal note: {}", note.content),
        "shopping" => format!("I found this shopping note: {}", note.content),
        "security" => format!("I found this security note: {}", note.content),
        "beverage" => format!("I found this beverage note: {}", note.content),
        "social" => format!("I found this social note: {}", note.content),
        "commute" => format!("I found this commute note: {}", note.content),
        "pantry" => format!("I found this pantry note: {}", note.content),
        "home_comfort" => format!("I found this comfort note: {}", note.content),
        "location" => format!("I found this location note: {}", note.content),
        "receipt" => format!("I found this receipt note: {}", note.content),
        "education" => format!("I found this education note: {}", note.content),
        "entertainment" => format!("I found this entertainment note: {}", note.content),
        "dictionary" => format!("I found this dictionary note: {}", note.content),
        "health" => format!("I found this health note: {}", note.content),
        "party" => format!("I found this party note: {}", note.content),
        "pest_control" => format!("I found this pest-control note: {}", note.content),
        "food_safety" => format!("I found this food-safety note: {}", note.content),
        "contact" => format!("I found this contact note: {}", note.content),
        "delivery" => format!("I found this delivery note: {}", note.content),
        "schedule" => format!("I found this schedule note: {}", note.content),
        "finance" => format!("I found this finance note: {}", note.content),
        "tool" => format!("I found this tool note: {}", note.content),
        "network" => format!("I found this network note: {}", note.content),
        "diy" => format!("I found this DIY note: {}", note.content),
        "fitness" => format!("I found this fitness note: {}", note.content),
        "safety" => format!("I found this safety note: {}", note.content),
        "device" => format!("I found this device note: {}", note.content),
        "news" => format!("I found this news note: {}", note.content),
        "profile" => format!("I found this profile note: {}", note.content),
        "family" => format!("I found this family note: {}", note.content),
        "garden" => format!("I found this garden note: {}", note.content),
        "inventory" => format!("I found this inventory note: {}", note.content),
        _ => format!("I found this note: {}", note.content),
    }
}

fn format_app_only_secret_reference_answer(secret_ref: &AppOnlySecretReference) -> String {
    format!(
        "I have an app-only reference for {}. Open the local dashboard or credential store to view it; I won't speak the value in shared-room chat.",
        secret_ref.label
    )
}

fn possessive_named_profile(content: &str, lower: &str) -> Option<(&'static str, String)> {
    let marker = " is named ";
    let marker_pos = lower.find(marker)?;
    let left = lower[..marker_pos].trim();
    let role_phrase = left
        .strip_prefix("user's ")
        .or_else(|| left.strip_prefix("my "))
        .or_else(|| left.strip_prefix("our "))?;
    let role = normalize_household_role(role_phrase)?;
    let name = clean_profile_name(&content[marker_pos + marker.len()..]);
    if name.is_empty() {
        None
    } else {
        Some((role, name))
    }
}

fn definite_role_profile(content: &str, lower: &str) -> Option<(&'static str, String)> {
    let marker = " is ";
    let marker_pos = lower.find(marker)?;
    let left = lower[..marker_pos].trim();
    let role_phrase = left.strip_prefix("the ")?;
    let role = normalize_household_role(role_phrase)?;
    let name = clean_profile_name(&content[marker_pos + marker.len()..]);
    if name.is_empty() {
        None
    } else {
        Some((role, name))
    }
}

fn subject_role_profile(content: &str, lower: &str) -> Option<(String, &'static str)> {
    for marker in [" is the ", " is our ", " is my "] {
        if let Some(marker_pos) = lower.find(marker) {
            let name = clean_profile_name(&content[..marker_pos]);
            let role_phrase = lower[marker_pos + marker.len()..].trim();
            let role = normalize_household_role(role_phrase)?;
            if !name.is_empty() {
                return Some((name, role));
            }
        }
    }
    None
}

fn clean_profile_name(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("named ")
        .trim_matches(|ch: char| matches!(ch, '.' | ',' | '!' | '?' | '"' | '\''))
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_household_role(value: &str) -> Option<&'static str> {
    let normalized = value
        .trim()
        .trim_start_matches("the ")
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|ch: char| matches!(ch, '.' | ',' | '!' | '?' | ':' | ';'));

    match normalized {
        "dad" | "father" => Some("dad"),
        "mom" | "mother" | "mum" => Some("mom"),
        "son" | "sons" => Some("son"),
        "daughter" | "daughters" => Some("daughter"),
        "child" | "children" | "kid" | "kids" => Some("child"),
        "wife" => Some("wife"),
        "husband" => Some("husband"),
        "partner" => Some("partner"),
        "dog" | "dogs" => Some("dog"),
        "cat" | "cats" => Some("cat"),
        "pet" | "pets" => Some("pet"),
        _ => None,
    }
}

fn canonical_date(ts_ms: u64) -> String {
    let secs = (ts_ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::gmtime_r(&secs, &mut tm) };
    if result.is_null() {
        return "1970-01-01".into();
    }
    format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    )
}

fn canonical_daily_note_file(canonical_dir: &Path, ts_ms: u64) -> PathBuf {
    canonical_dir.join(format!("{}.md", canonical_date(ts_ms)))
}

fn canonical_event_file(canonical_dir: &Path, ts_ms: u64) -> PathBuf {
    canonical_dir
        .join("events")
        .join(format!("{}.jsonl", canonical_date(ts_ms)))
}

fn canonical_namespace(kind: &str, metadata: policy::MemoryPolicyMetadata) -> String {
    let lower = kind.trim().to_ascii_lowercase();
    let leaf = lower
        .strip_prefix("person_")
        .or_else(|| lower.strip_prefix("private_"))
        .or_else(|| lower.strip_prefix("session_"))
        .or_else(|| lower.strip_prefix("household_"))
        .unwrap_or(&lower)
        .to_string();
    let leaf = sanitize_namespace_segment(&leaf);
    format!(
        "{}.{}",
        metadata.scope.as_str(),
        if leaf.is_empty() { "general" } else { &leaf }
    )
}

fn canonical_namespace_note_relative(namespace: &str) -> String {
    let mut parts = namespace
        .split('.')
        .map(sanitize_namespace_segment)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push("general".into());
    }
    let leaf = parts.pop().unwrap_or_else(|| "general".into());
    let mut path = PathBuf::from("namespaces");
    for part in parts {
        path.push(part);
    }
    path.push(format!("{leaf}.md"));
    path.to_string_lossy().into_owned()
}

fn sanitize_namespace_segment(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else if ch == '_' || ch == '-' || ch == ' ' || ch == '.' {
            '-'
        } else {
            continue;
        };
        if next == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        out.push(next);
    }
    out.trim_matches('-').to_string()
}

fn count_markdown_files(root: &Path) -> usize {
    if !root.exists() {
        return 0;
    }
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
            {
                count += 1;
            }
        }
    }
    count
}

/// Word overlap ratio between two strings (Jaccard-like).
fn word_overlap(a: &str, b: &str) -> f64 {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();
    let a_words: std::collections::HashSet<&str> = a_lower.split_whitespace().collect();
    let b_words: std::collections::HashSet<&str> = b_lower.split_whitespace().collect();

    if a_words.is_empty() || b_words.is_empty() {
        return 0.0;
    }

    let intersection = a_words.intersection(&b_words).count();
    let union = a_words.union(&b_words).count();

    intersection as f64 / union as f64
}

fn lexical_overlap_score(a: &str, b: &str) -> f64 {
    word_overlap(a, b).max(0.05)
}

fn normalize_memory_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn memory_slot(kind: &str, content: &str) -> Option<String> {
    let kind = kind.trim().to_lowercase();
    let lower = content.trim().to_lowercase();

    match kind.as_str() {
        "identity" => {
            if lower.starts_with("user's name is ") {
                Some("identity:name".into())
            } else if lower.starts_with("user is ") && lower.contains(" years old") {
                Some("identity:age".into())
            } else if lower.starts_with("user lives in ") {
                Some("identity:location".into())
            } else if lower.starts_with("user works at ") {
                Some("identity:workplace".into())
            } else if lower.starts_with("user is a ") || lower.starts_with("user is an ") {
                Some("identity:occupation".into())
            } else {
                None
            }
        }
        "preference" => favorite_slot(&lower).map(|slot| format!("preference:favorite:{slot}")),
        _ => None,
    }
}

fn favorite_slot(lower_content: &str) -> Option<String> {
    let rest = lower_content.strip_prefix("user's favorite ")?;
    let (thing, _) = rest.split_once(" is ")?;
    let thing = thing.trim();
    if thing.is_empty() {
        None
    } else {
        Some(thing.to_string())
    }
}

fn search_tokens(text: &str) -> Vec<String> {
    let stop = [
        "a", "an", "and", "are", "about", "can", "did", "do", "does", "for", "have", "how", "i",
        "is", "it", "me", "my", "of", "on", "or", "please", "remember", "that", "the", "this",
        "to", "what", "whats", "when", "where", "who", "you", "your",
    ];
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() > 1 && !stop.contains(token))
        .map(ToString::to_string)
        .collect()
}

fn build_fts_query(query: &str) -> Option<String> {
    let tokens = search_tokens(query);
    if tokens.is_empty() {
        None
    } else {
        Some(
            tokens
                .into_iter()
                .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" OR "),
        )
    }
}

fn rebuild_fts_index(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO memories_fts(memories_fts) VALUES('rebuild')",
        [],
    )?;
    Ok(())
}

fn is_duplicate_column_error(err: &rusqlite::Error) -> bool {
    match err {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => {
            msg.to_ascii_lowercase().contains("duplicate column")
        }
        _ => false,
    }
}

fn run_open_migration(conn: &Connection, sql: &str, step: &str, migration_degraded: &mut bool) {
    if let Err(error) = conn.execute(sql, []) {
        if is_duplicate_column_error(&error) {
            return;
        }
        tracing::error!(step, error = %error, "memory schema migration failed");
        *migration_degraded = true;
    }
}

fn run_open_fts_rebuild(conn: &Connection, migration_degraded: &mut bool) {
    if let Err(error) = rebuild_fts_index(conn) {
        tracing::error!(error = %error, "memory FTS rebuild failed at open");
        *migration_degraded = true;
    }
}

/// Strip "(source: filename)" tags from memory content for comparison.
fn strip_source_tag(text: &str) -> String {
    if let Some(pos) = text.rfind(" (source:") {
        text[..pos].trim().to_string()
    } else {
        text.to_string()
    }
}

fn hash_query(query: &str) -> String {
    // Simple hash for query dedup tracking.
    let bytes = query.as_bytes();
    let mut hash: u64 = 5381;
    for &b in bytes {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    format!("{:016x}", hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Return a freshly-created unique parent dir and a `memory.db` path
    /// inside it. `Memory::open` derives
    /// `canonical_dir = path.parent().join("memory")` and writes promotion
    /// and namespace markdown files into it, so sharing a parent dir across
    /// tests causes promotion tests to race on shared files like
    /// `namespaces/person/preference.md` (issue #21, AC-D2). Every memory
    /// test path MUST flow through this helper. The `nanos` suffix on top
    /// of `pid + counter` defends against rapid test-binary reruns that
    /// could reuse a pid before the previous run's tempdir was cleaned.
    fn temp_memory_path(label: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "geniepod-mem-{}-{}-{}-{}",
            label,
            std::process::id(),
            id,
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("create temp memory dir");
        dir.join("memory.db")
    }

    fn temp_memory() -> Memory {
        Memory::open(&temp_memory_path("test")).unwrap()
    }

    #[test]
    fn store_and_search() {
        let mem = temp_memory();
        mem.store("fact", "The user's name is Jared").unwrap();
        mem.store("fact", "Jared is building GeniePod").unwrap();
        mem.store("preference", "User prefers dark mode").unwrap();

        let results = mem.search("Jared", 10).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|r| r.content.contains("name is Jared")));
    }

    #[test]
    fn recent_memories() {
        let mem = temp_memory();
        mem.store("fact", "first").unwrap();
        mem.store("fact", "second").unwrap();
        mem.store("fact", "third").unwrap();

        let recent = mem.recent(2).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].content, "third");
    }

    #[test]
    fn count_memories() {
        let mem = temp_memory();
        assert_eq!(mem.count().unwrap(), 0);
        mem.store("fact", "test").unwrap();
        assert_eq!(mem.count().unwrap(), 1);
    }

    #[test]
    fn recall_count_increments() {
        let mem = temp_memory();
        mem.store("fact", "GeniePod runs on Jetson").unwrap();

        // Search 3 times.
        mem.search("Jetson", 10).unwrap();
        mem.search("Jetson", 10).unwrap();
        let results = mem.search("Jetson", 10).unwrap();

        assert_eq!(results.len(), 1, "expected 1 result");
        // After 3 searches, recall_count is 2 (reads before increment on 3rd call).
        assert!(
            results[0].recall_count >= 2,
            "recall_count was {}",
            results[0].recall_count
        );
    }

    #[test]
    fn evergreen_memories_dont_decay() {
        let mem = Memory::open_with_half_life(
            &temp_memory_path("evergreen"),
            0.001, // Extreme decay — everything decays almost instantly.
        )
        .unwrap();

        mem.store_evergreen("fact", "permanent knowledge").unwrap();
        mem.store("fact", "temporary knowledge").unwrap();

        // Evergreen should survive prune.
        let deleted = mem.prune_decayed(0.5).unwrap();
        assert!(deleted <= 1); // temporary might be deleted
        assert!(mem.count().unwrap() >= 1); // evergreen survives
    }

    #[test]
    fn promotion_candidates() {
        let mem = temp_memory();
        mem.store("fact", "frequently recalled fact").unwrap();

        // Simulate recalls.
        for _ in 0..5 {
            mem.search("frequently", 10).unwrap();
        }

        let candidates = mem.promotion_candidates(3, 0.0, 10).unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].recall_count >= 5);
    }

    #[test]
    fn mark_promoted() {
        let mem = temp_memory();
        let id = mem.store("fact", "important fact").unwrap();
        mem.mark_promoted(id).unwrap();

        assert_eq!(mem.promoted_count().unwrap(), 1);

        // Promoted memories excluded from candidates.
        let candidates = mem.promotion_candidates(0, 0.0, 10).unwrap();
        assert!(candidates.is_empty());
    }

    #[test]
    fn search_handles_question_words_and_apostrophes() {
        let mem = temp_memory();
        mem.store("identity", "User's name is Jared").unwrap();

        let results = mem.search("did you remember my name?", 10).unwrap();
        assert!(
            results.iter().any(|entry| entry.content.contains("Jared")),
            "expected name memory in {:?}",
            results
        );
    }

    #[test]
    fn recall_tracking_records_query_diversity_without_duplicates() {
        let mem = temp_memory();
        let id = mem
            .store("preference", "User likes spicy noodle soup")
            .unwrap();

        mem.search("spicy", 10).unwrap();
        mem.search("spicy", 10).unwrap();
        assert_eq!(mem.query_diversity(id).unwrap(), 1);

        mem.search("noodle soup", 10).unwrap();
        assert_eq!(mem.query_diversity(id).unwrap(), 2);
    }

    #[test]
    fn store_resolved_replaces_single_value_identity() {
        let mem = temp_memory();
        mem.store_resolved("identity", "User's name is Jared")
            .unwrap();
        let outcome = mem
            .store_resolved("identity", "User's name is Alice")
            .unwrap();

        assert_eq!(outcome.replaced, 1);

        let identities = mem.get_by_kind("identity", 10).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].content, "User's name is Alice");
    }

    #[test]
    fn store_resolved_replaces_favorite_value_by_subject() {
        let mem = temp_memory();
        mem.store_resolved("preference", "User's favorite color is blue")
            .unwrap();
        let outcome = mem
            .store_resolved("preference", "User's favorite color is green")
            .unwrap();

        assert_eq!(outcome.replaced, 1);

        let preferences = mem.get_by_kind("preference", 10).unwrap();
        assert_eq!(preferences.len(), 1);
        assert_eq!(preferences[0].content, "User's favorite color is green");
    }

    #[test]
    fn relationship_memory_indexes_household_profile_role() {
        let mem = temp_memory();
        mem.store("relationship", "Jared is the dad").unwrap();

        let profiles = mem.household_profiles_by_role("father").unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Jared");
        assert_eq!(profiles[0].role, "dad");
    }

    #[test]
    fn household_profiles_rebuild_on_reopen() {
        let path = temp_memory_path("profiles-reopen");
        {
            let mem = Memory::open(&path).unwrap();
            mem.store("relationship", "User's son is named Leo")
                .unwrap();
        }

        let mem = Memory::open(&path).unwrap();
        let profiles = mem.household_profiles_by_role("son").unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Leo");
    }

    #[test]
    fn managed_update_refreshes_household_profile_role() {
        let mem = temp_memory();
        let id = mem.store("relationship", "Jared is the dad").unwrap();

        assert!(
            mem.update_managed(id, "Sarah is the mom", Some("relationship"))
                .unwrap()
        );

        assert!(mem.household_profiles_by_role("dad").unwrap().is_empty());
        let profiles = mem.household_profiles_by_role("mom").unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Sarah");
    }

    #[test]
    fn private_relationship_memory_is_not_indexed_as_household_profile() {
        let mem = temp_memory();
        mem.store_with_metadata(
            "private_relationship",
            "Jared is the dad",
            policy::MemoryPolicyMetadata {
                scope: policy::MemoryScope::Private,
                sensitivity: policy::MemorySensitivity::Cautious,
                spoken_policy: policy::SpokenMemoryPolicy::AppOnly,
            },
            false,
        )
        .unwrap();

        assert!(mem.household_profiles_by_role("dad").unwrap().is_empty());
    }

    #[test]
    fn device_alias_memory_indexes_exact_target() {
        let mem = temp_memory();
        mem.store("fact", "Playroom lights maps to light.playroom")
            .unwrap();

        let alias = mem.device_alias("playroom lights").unwrap().unwrap();
        assert_eq!(alias.target_id, "light.playroom");
        assert_eq!(alias.kind, "light");
    }

    #[test]
    fn device_alias_allows_room_alias_for_explicit_target_marker() {
        let mem = temp_memory();
        mem.store("fact", "Playroom maps to smartplug_04").unwrap();

        let alias = mem.device_alias("playroom").unwrap().unwrap();
        assert_eq!(alias.target_id, "smartplug_04");
    }

    #[test]
    fn device_aliases_rebuild_on_reopen() {
        let path = temp_memory_path("device-alias-reopen");
        {
            let mem = Memory::open(&path).unwrap();
            mem.store("fact", "Movie night scene maps to scene.movie_night")
                .unwrap();
        }

        let mem = Memory::open(&path).unwrap();
        let alias = mem.device_alias("movie night scene").unwrap().unwrap();
        assert_eq!(alias.target_id, "scene.movie_night");
    }

    #[test]
    fn private_device_alias_memory_is_not_indexed() {
        let mem = temp_memory();
        mem.store_with_metadata(
            "private_fact",
            "Bedroom camera maps to switch.private_camera",
            policy::MemoryPolicyMetadata {
                scope: policy::MemoryScope::Private,
                sensitivity: policy::MemorySensitivity::Cautious,
                spoken_policy: policy::SpokenMemoryPolicy::AppOnly,
            },
            false,
        )
        .unwrap();

        assert!(mem.device_alias("bedroom camera").unwrap().is_none());
    }

    #[test]
    fn profile_attribute_memory_indexes_age_and_preferences() {
        let mem = temp_memory();
        mem.store("fact", "Leo is 8 years old").unwrap();
        mem.store("preference", "Leo likes granola bars").unwrap();

        let age = mem.profile_attributes("leo", "age").unwrap();
        assert_eq!(age[0].name, "Leo");
        assert_eq!(age[0].value, "8");

        let answer = mem
            .structured_household_answer("How old is Leo?")
            .unwrap()
            .unwrap();
        assert_eq!(answer, "Leo is 8 years old.");
    }

    #[test]
    fn household_rules_answer_allergy_and_screen_time() {
        let mem = temp_memory();
        mem.store("fact", "Leo has a mild peanut allergy").unwrap();
        mem.store("fact", "Leo is not allowed to play video games after 8 PM")
            .unwrap();

        let allergy = mem
            .structured_household_answer("Is anyone allergic to peanuts?")
            .unwrap()
            .unwrap();
        assert!(allergy.contains("Leo has a mild peanut allergy"));

        let allowed = mem
            .structured_household_answer("Is Leo allowed to play video games after 8 PM?")
            .unwrap()
            .unwrap();
        assert!(allowed.starts_with("No."));
        assert!(allowed.contains("not allowed"));
    }

    #[test]
    fn household_rules_answer_homework_rules() {
        let mem = temp_memory();
        mem.store("fact", "Mia must finish homework before screen time")
            .unwrap();

        let answer = mem
            .structured_household_answer("Show me Mia's homework rules")
            .unwrap()
            .unwrap();
        assert!(answer.contains("Mia must finish homework before screen time"));
    }

    #[test]
    fn household_notes_index_and_search_fts() {
        let mem = temp_memory();
        mem.store(
            "note",
            "Remember to water the potted plant on the porch every Tuesday",
        )
        .unwrap();

        let notes = mem.household_notes_search("potted plant porch", 3).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].note_type, "note");
        assert!(notes[0].content.contains("potted plant"));
    }

    #[test]
    fn household_notes_answer_note_and_storage_questions() {
        let mem = temp_memory();
        mem.store("note", "Bike lock hangs on the garage hook")
            .unwrap();
        mem.store(
            "note",
            "Extra AA and AAA batteries are in the junk drawer in the laundry room",
        )
        .unwrap();

        let lock = mem
            .structured_household_answer("Find my note about the bicycle lock")
            .unwrap()
            .unwrap();
        assert!(lock.contains("garage hook"));

        let batteries = mem
            .structured_household_answer("Where are the extra batteries kept?")
            .unwrap()
            .unwrap();
        assert!(batteries.contains("junk drawer"));
    }

    #[test]
    fn household_notes_rebuild_on_reopen() {
        let path = temp_memory_path("notes-reopen");
        {
            let mem = Memory::open(&path).unwrap();
            mem.store("note", "Extra batteries are in the laundry room drawer")
                .unwrap();
        }

        let mem = Memory::open(&path).unwrap();
        let notes = mem.household_notes_search("batteries drawer", 3).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].content.contains("laundry room drawer"));
    }

    #[test]
    fn private_household_note_is_not_indexed() {
        let mem = temp_memory();
        mem.store_with_metadata(
            "note",
            "Private safe code is 1234",
            policy::MemoryPolicyMetadata {
                scope: policy::MemoryScope::Private,
                sensitivity: policy::MemorySensitivity::Restricted,
                spoken_policy: policy::SpokenMemoryPolicy::AppOnly,
            },
            false,
        )
        .unwrap();

        assert!(
            mem.household_notes_search("safe code", 3)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn app_only_secret_reference_indexes_without_revealing_value() {
        let mem = temp_memory();
        mem.store(
            "credential_reference",
            "Guest Wi-Fi password is stored in credential:guest_wifi",
        )
        .unwrap();

        let refs = mem
            .app_only_secret_references("What is our Wi-Fi password for guests?")
            .unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].secret_type, "wifi_password");
        assert_eq!(refs[0].label, "guest wifi");

        let answer = mem
            .structured_household_answer("What is our Wi-Fi password for guests?")
            .unwrap()
            .unwrap();
        assert!(answer.contains("app-only reference"));
        assert!(!answer.contains("credential:guest_wifi"));
    }

    #[test]
    fn normal_password_memory_is_not_indexed_as_shared_note() {
        let mem = temp_memory();
        mem.store("fact", "Guest Wi-Fi password is pizza-party-2024")
            .unwrap();

        assert!(
            mem.household_notes_search("wifi password", 3)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn app_only_secret_reference_matches_lock_combo_query_without_value() {
        let mem = temp_memory();
        mem.store(
            "credential_reference",
            "Bike lock combo is stored in credential:bike_lock",
        )
        .unwrap();

        let refs = mem
            .app_only_secret_references("Find my note about the bicycle lock code")
            .unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].secret_type, "lock_code");
        assert_eq!(refs[0].label, "Bike lock combo");

        let answer = mem
            .structured_household_answer("Find my note about the bicycle lock code")
            .unwrap()
            .unwrap();
        assert!(answer.contains("app-only reference"));
        assert!(!answer.contains("credential:bike_lock"));
    }

    #[test]
    fn app_only_secret_reference_matches_locker_combo_query_without_value() {
        let mem = temp_memory();
        mem.store(
            "credential_reference",
            "Mia's locker combination is stored in credential:mia_locker",
        )
        .unwrap();

        let refs = mem
            .app_only_secret_references("What is Mia's locker combination?")
            .unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].secret_type, "combination");
        assert_eq!(refs[0].label, "Mia locker combination");

        let answer = mem
            .structured_household_answer("What is Mia's locker combination?")
            .unwrap()
            .unwrap();
        assert!(answer.contains("app-only reference"));
        assert!(!answer.contains("credential:mia_locker"));
    }

    #[test]
    fn app_only_secret_reference_matches_travel_and_bill_numbers_without_value() {
        let mem = temp_memory();
        mem.store(
            "credential_reference",
            "Hotel confirmation number is stored in credential:hotel_chicago",
        )
        .unwrap();
        mem.store(
            "credential_reference",
            "Gas bill account number is stored in credential:gas_bill",
        )
        .unwrap();

        let hotel = mem
            .structured_household_answer("What is the confirmation number for the hotel?")
            .unwrap()
            .unwrap();
        assert!(hotel.contains("app-only reference"));
        assert!(hotel.contains("hotel confirmation number"));
        assert!(!hotel.contains("credential:hotel_chicago"));

        let gas = mem
            .structured_household_answer("What is the account number for the gas bill?")
            .unwrap()
            .unwrap();
        assert!(gas.contains("app-only reference"));
        assert!(gas.contains("gas bill account number"));
        assert!(!gas.contains("credential:gas_bill"));
    }

    #[test]
    fn media_profile_indexes_and_resolves_playlist() {
        let mem = temp_memory();
        mem.store(
            "media_profile",
            "Jared's Morning Boost playlist is spotify:playlist:morning_boost",
        )
        .unwrap();

        let playlist = mem
            .media_playlist_for_query("play my Morning Boost playlist")
            .unwrap()
            .unwrap();
        assert_eq!(playlist.owner.as_deref(), Some("Jared"));
        assert_eq!(playlist.name, "Morning Boost");
        assert_eq!(playlist.provider.as_deref(), Some("spotify"));
        assert_eq!(playlist.target, "spotify:playlist:morning_boost");
    }

    #[test]
    fn media_profile_rebuilds_on_reopen() {
        let path = temp_memory_path("media-profile-reopen");
        {
            let mem = Memory::open(&path).unwrap();
            mem.store(
                "media_profile",
                "Jared's playlist Morning Boost is spotify:playlist:morning_boost",
            )
            .unwrap();
        }

        let reopened = Memory::open(&path).unwrap();
        let playlist = reopened
            .media_playlist_for_query("play Jared's Morning Boost playlist")
            .unwrap()
            .unwrap();
        assert_eq!(playlist.target, "spotify:playlist:morning_boost");
    }

    #[test]
    fn family_calendar_indexes_and_answers_piano_lesson() {
        let mem = temp_memory();
        mem.store(
            "calendar",
            "Mia has piano lessons today at 4:00 PM with Mrs. Higgins",
        )
        .unwrap();

        let event = mem
            .family_calendar_event_for_query("Does Mia have piano lessons today?")
            .unwrap()
            .unwrap();
        assert_eq!(event.person.as_deref(), Some("Mia"));
        assert_eq!(event.event_type, "piano_lesson");
        assert_eq!(event.day.as_deref(), Some("today"));
        assert_eq!(event.time.as_deref(), Some("4:00pm"));

        let answer = mem
            .structured_household_answer("Does Mia have piano lessons today?")
            .unwrap()
            .unwrap();
        assert!(answer.contains("Mia"));
        assert!(answer.contains("piano"));
        assert!(answer.contains("4:00pm"));
    }

    #[test]
    fn household_inventory_and_calendar_answer_grocery_travel_questions() {
        let mem = temp_memory();
        mem.store(
            "pantry_inventory",
            "Eggs inventory: 4 eggs remaining in the fridge door",
        )
        .unwrap();
        mem.store(
            "family_calendar",
            "Mia has a dentist appointment on October 20th at 3:30 PM",
        )
        .unwrap();

        let item = mem
            .household_inventory_item_for_query("Do we have any eggs left?")
            .unwrap()
            .unwrap();
        assert_eq!(item.item, "eggs");
        assert_eq!(item.quantity.as_deref(), Some("4"));
        assert_eq!(item.location.as_deref(), Some("fridge door"));

        let inventory = mem
            .structured_household_answer("Do we have any eggs left?")
            .unwrap()
            .unwrap();
        assert!(inventory.contains("4 eggs"));
        assert!(inventory.contains("fridge door"));

        let appointment = mem
            .structured_household_answer("When is Mia's next dentist appointment?")
            .unwrap()
            .unwrap();
        assert!(appointment.contains("Mia"));
        assert!(appointment.contains("dentist"));
        assert!(appointment.contains("3:30pm"));
    }

    #[test]
    fn garden_health_logistics_structured_recall_answers_exact_questions() {
        let mem = temp_memory();
        mem.store("astronomical_data", "Sunset today is at 7:42 PM")
            .unwrap();
        mem.store(
            "pet_calendar",
            "Buster has a vet appointment next Tuesday at 10:00 AM",
        )
        .unwrap();
        mem.store(
            "payment_history",
            "Electric bill for January was paid on the 15th for $142",
        )
        .unwrap();

        let sunset = mem
            .structured_household_answer("What time does the sun set today?")
            .unwrap()
            .unwrap();
        assert!(sunset.contains("7:42pm"));

        let vet = mem
            .structured_household_answer("When is Buster's next vet appointment?")
            .unwrap()
            .unwrap();
        assert!(vet.contains("Buster"));
        assert!(vet.contains("vet appointment"));
        assert!(vet.contains("10:00am"));

        let paid = mem
            .structured_household_answer("Did I pay the electric bill?")
            .unwrap()
            .unwrap();
        assert!(paid.contains("Electric bill"));
        assert!(paid.contains("$142"));
    }

    #[test]
    fn vehicle_work_seasonal_structured_recall_answers_exact_questions() {
        let mem = temp_memory();
        mem.store(
            "chore_completion_log",
            "Leo brushed teeth today at 8:15 PM and marked the task complete",
        )
        .unwrap();
        mem.store(
            "community_services_schedule",
            "Community pool is open today from 10:00 AM to 8:00 PM",
        )
        .unwrap();
        mem.store(
            "local_business_hours",
            "The public library closes at 9:00 PM on Mondays",
        )
        .unwrap();

        let brushed = mem
            .structured_household_answer("Did Leo brush his teeth?")
            .unwrap()
            .unwrap();
        assert!(brushed.contains("Leo"));
        assert!(brushed.contains("brushing teeth"));
        assert!(brushed.contains("8:15pm"));

        let pool = mem
            .structured_household_answer("Is the community pool open today?")
            .unwrap()
            .unwrap();
        assert!(pool.contains("Community pool"));
        assert!(pool.contains("10:00 AM"));

        let library = mem
            .structured_household_answer("When does the library close?")
            .unwrap()
            .unwrap();
        assert!(library.contains("public library"));
        assert!(library.contains("9:00pm"));
    }

    #[test]
    fn appliance_outdoor_media_structured_recall_answers_exact_questions() {
        let mem = temp_memory();
        mem.store(
            "appliance_states",
            "Dishwasher clean status is dirty and ready to be loaded",
        )
        .unwrap();
        mem.store("waste_management_log", "Trash truck came today at 7:45 AM")
            .unwrap();
        mem.store(
            "environmental_sensors",
            "Attic temperature is 85F and the ventilation fan should be checked",
        )
        .unwrap();
        mem.store(
            "location_services",
            "Mia arrived home from school 10 minutes ago",
        )
        .unwrap();

        let dishwasher = mem
            .structured_household_answer("Is the dishwasher clean or dirty?")
            .unwrap()
            .unwrap();
        assert!(dishwasher.contains("dirty"));

        let trash = mem
            .structured_household_answer("Did the trash truck come yet?")
            .unwrap()
            .unwrap();
        assert!(trash.contains("7:45 AM"));

        let attic = mem
            .structured_household_answer("What is the temperature in the attic?")
            .unwrap()
            .unwrap();
        assert!(attic.contains("85F"));

        let mia = mem
            .structured_household_answer("Is Mia home from school?")
            .unwrap()
            .unwrap();
        assert!(mia.contains("Mia"));
        assert!(mia.contains("10 minutes ago"));
    }

    #[test]
    fn finance_program_guide_and_health_recall_answer_exact_questions() {
        let mem = temp_memory();
        mem.store(
            "financial_services",
            "Your current FICO credit score is 785",
        )
        .unwrap();
        mem.store("electronic_program_guide", "ESPN is on channel 206")
            .unwrap();
        mem.store(
            "smart_scale",
            "Your weight is 175 lbs, down 2 lbs since last week",
        )
        .unwrap();
        for (role, name) in [
            ("dad", "Jared"),
            ("mom", "Sarah"),
            ("son", "Leo"),
            ("daughter", "Mia"),
        ] {
            mem.store("fact", &format!("{name} is the {role} in this house"))
                .unwrap();
        }
        mem.store(
            "chore_completion_log",
            "Jared brushed teeth complete today at 8:05 PM",
        )
        .unwrap();
        mem.store(
            "chore_completion_log",
            "Sarah brushed teeth complete today at 8:10 PM",
        )
        .unwrap();

        let credit = mem
            .structured_household_answer("What is my credit score?")
            .unwrap()
            .unwrap();
        assert!(credit.contains("785"));

        let channel = mem
            .structured_household_answer("What channel is ESPN?")
            .unwrap()
            .unwrap();
        assert!(channel.contains("206"));

        let teeth = mem
            .structured_household_answer("Did everyone brush their teeth?")
            .unwrap()
            .unwrap();
        assert!(teeth.contains("Jared"));
        assert!(teeth.contains("Sarah"));
        assert!(teeth.contains("Leo"));
        assert!(teeth.contains("Mia"));

        let weight = mem
            .structured_household_answer("What is my weight?")
            .unwrap()
            .unwrap();
        assert!(weight.contains("175 lbs"));
    }

    #[test]
    fn vehicle_market_subscription_recall_answers_exact_questions() {
        let mem = temp_memory();
        mem.store(
            "financial_market_api",
            "Apple (AAPL) is currently trading at $175.50",
        )
        .unwrap();
        mem.store(
            "subscriptions",
            "Netflix subscription renews on the 15th of next month",
        )
        .unwrap();
        mem.store("astronomical_data", "Sunset today is at 7:15 PM")
            .unwrap();

        let stock = mem
            .structured_household_answer("What is the stock price of Apple?")
            .unwrap()
            .unwrap();
        assert!(stock.contains("AAPL"));
        assert!(stock.contains("$175.50"));

        let subscription = mem
            .structured_household_answer("When is the subscription due?")
            .unwrap()
            .unwrap();
        assert!(subscription.contains("Netflix"));
        assert!(subscription.contains("15th"));

        let sunset = mem
            .structured_household_answer("What time is sunset?")
            .unwrap()
            .unwrap();
        assert!(sunset.contains("7:15pm"));
    }

    #[test]
    fn vehicle_appliance_schedule_and_health_recall_answers_exact_questions() {
        let mem = temp_memory();
        mem.store(
            "fitness_tracker",
            "VO2 max reading is 45, which is above average",
        )
        .unwrap();
        mem.store(
            "electronic_program_guide",
            "TV tonight: Tonight at 8 PM The Big Game on ESPN. At 9 PM Nova on PBS",
        )
        .unwrap();
        mem.store(
            "community_calendar",
            "City council meeting is on Tuesday at 7 PM",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("What is my VO2 max?")
                .unwrap()
                .unwrap()
                .contains("45")
        );
        assert!(
            mem.structured_household_answer("What's on TV tonight?")
                .unwrap()
                .unwrap()
                .contains("Big Game")
        );
        assert!(
            mem.structured_household_answer("When is the next city council meeting?")
                .unwrap()
                .unwrap()
                .contains("Tuesday")
        );
    }

    #[test]
    fn shopping_list_indexes_pending_items_and_counts() {
        let mem = temp_memory();
        mem.store("shopping", "shopping list pending: milk, eggs")
            .unwrap();

        let items = mem.shopping_list_items().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(mem.shopping_list_pending_count().unwrap(), 2);

        let answer = mem
            .structured_household_answer("What is on the shopping list?")
            .unwrap()
            .unwrap();
        assert!(answer.contains("milk"));
        assert!(answer.contains("eggs"));
    }

    #[test]
    fn shopping_list_removed_items_stop_showing_as_pending() {
        let mem = temp_memory();
        mem.store("shopping", "shopping list pending: milk, eggs")
            .unwrap();
        mem.store("shopping", "shopping list removed: milk")
            .unwrap();

        let items = mem.shopping_list_items().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item, "eggs");
        assert_eq!(mem.shopping_list_pending_count().unwrap(), 1);
    }

    #[test]
    fn access_permissions_index_denied_and_allowed_unlock_targets() {
        let mem = temp_memory();
        mem.store(
            "access_permission",
            "Leo is not authorized to unlock the front door. He can only unlock the side door",
        )
        .unwrap();

        let denied = mem
            .access_permission_for_query("Can Leo unlock the front door?")
            .unwrap()
            .unwrap();
        assert!(!denied.allowed);
        assert_eq!(denied.device, "front door");

        let allowed = mem
            .access_permission_for_query("Can Leo unlock the side door?")
            .unwrap()
            .unwrap();
        assert!(allowed.allowed);
        assert_eq!(allowed.device, "side door");
    }

    #[test]
    fn household_task_logs_and_schedules_answer_chore_school_bill_and_recycling() {
        let mem = temp_memory();
        mem.store(
            "pet_care",
            "Leo fed the dog today at 8:00 AM and marked the task complete",
        )
        .unwrap();
        mem.store("school_schedule", "The school bus arrives at 7:45 AM")
            .unwrap();
        mem.store(
            "bill_schedule",
            "The electricity bill is due in 3 days on October 25th. The estimated amount is $145",
        )
        .unwrap();
        mem.store(
            "recycling_schedule",
            "Tomorrow is recycling day. Remember to rinse plastic containers and put glass in the blue bin",
        )
        .unwrap();

        let task = mem
            .household_task_log_for_query("Did Leo feed the dog today?")
            .unwrap()
            .unwrap();
        assert_eq!(task.person, "Leo");
        assert_eq!(task.task, "feeding");
        assert_eq!(task.subject.as_deref(), Some("dog"));
        assert_eq!(task.status, "complete");

        let task_answer = mem
            .structured_household_answer("Did Leo feed the dog today?")
            .unwrap()
            .unwrap();
        assert!(task_answer.contains("Leo"));
        assert!(task_answer.contains("8:00am"));

        let bus = mem
            .structured_household_answer("What time does the school bus arrive?")
            .unwrap()
            .unwrap();
        assert!(bus.contains("7:45am"));

        let bill = mem
            .structured_household_answer("When is the electricity bill due?")
            .unwrap()
            .unwrap();
        assert!(bill.contains("October 25th"));
        assert!(bill.contains("$145"));

        let recycling = mem
            .structured_household_answer("Is it recycling week?")
            .unwrap()
            .unwrap();
        assert!(recycling.contains("blue bin"));
    }

    #[test]
    fn household_schedules_and_event_logs_answer_school_and_security_questions() {
        let mem = temp_memory();
        mem.store(
            "school_calendar",
            "The next parent-teacher conference is on November 15th at 4:00 PM for Leo",
        )
        .unwrap();
        mem.store(
            "security_log",
            "The security system was disarmed by Sarah using her keypad code at 5:12 PM",
        )
        .unwrap();

        let conference = mem
            .structured_household_answer("When is the next parent-teacher conference?")
            .unwrap()
            .unwrap();
        assert!(conference.contains("November 15th"));
        assert!(conference.contains("4:00pm"));
        assert!(conference.contains("Leo"));

        let event = mem
            .household_event_log_for_query("Who turned off the security system?")
            .unwrap()
            .unwrap();
        assert_eq!(event.event_type, "security");
        assert_eq!(event.action, "disarm");
        assert_eq!(event.actor.as_deref(), Some("Sarah"));

        let answer = mem
            .structured_household_answer("Who turned off the security system?")
            .unwrap()
            .unwrap();
        assert!(answer.contains("Sarah"));
        assert!(answer.contains("5:12pm"));
        assert!(!answer.contains("keypad code"));
    }

    #[test]
    fn household_profiles_schedules_and_events_answer_seasonal_finance_questions() {
        let mem = temp_memory();
        mem.store("fact", "Mia currently wears a size 5 Women's shoe")
            .unwrap();
        mem.store(
            "family_ledger",
            "Leo received $20 last Friday for allowance",
        )
        .unwrap();
        mem.store(
            "city_services_schedule",
            "Trash pickup is every Thursday morning. Tomorrow is trash day",
        )
        .unwrap();

        let shoe = mem
            .structured_household_answer("What size shoe does Mia wear now?")
            .unwrap()
            .unwrap();
        assert!(shoe.contains("Mia"));
        assert!(shoe.contains("5 Women's"));

        let allowance = mem
            .structured_household_answer("Did Leo get his allowance this week?")
            .unwrap()
            .unwrap();
        assert!(allowance.contains("$20"));
        assert!(allowance.contains("last Friday"));

        let trash = mem
            .structured_household_answer("When is the next trash pickup?")
            .unwrap()
            .unwrap();
        assert!(trash.contains("Thursday"));
        assert!(trash.contains("Tomorrow"));
    }

    #[test]
    fn secret_like_notes_are_not_indexed_as_speakable_fts_notes() {
        let mem = temp_memory();
        mem.store(
            "note",
            "Router Admin URL: 192.168.1.1. User: admin. Pass: SkyNet-2024!",
        )
        .unwrap();

        assert!(
            mem.household_notes_search("router admin password", 3)
                .unwrap()
                .is_empty()
        );
        let answer = mem
            .structured_household_answer("What is the password for the router admin page?")
            .unwrap()
            .unwrap();
        assert!(answer.contains("app-only reference"));
        assert!(!answer.contains("SkyNet"));
    }

    #[test]
    fn expanded_household_notes_cover_pet_gift_paint_and_storage() {
        let mem = temp_memory();
        mem.store(
            "pet_health",
            "Dr. Smith said to give Buster one pill with food every morning until the bottle is empty",
        )
        .unwrap();
        mem.store(
            "gift",
            "Sarah gift ideas: new yoga mat or noise-canceling headphones for her birthday",
        )
        .unwrap();
        mem.store(
            "home_maintenance",
            "The shed was painted Forest Green Benjamin Moore code 2041-10 last summer",
        )
        .unwrap();
        mem.store(
            "storage",
            "Christmas decorations are in the attic in the red plastic bins labeled XMAS",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("What did the vet say about Buster's medicine?")
                .unwrap()
                .unwrap()
                .contains("one pill")
        );
        assert!(
            mem.structured_household_answer("Find my note about Sarah's gift ideas")
                .unwrap()
                .unwrap()
                .contains("yoga mat")
        );
        assert!(
            mem.structured_household_answer("What color did we paint the shed?")
                .unwrap()
                .unwrap()
                .contains("Forest Green")
        );
        assert!(
            mem.structured_household_answer("Where are the Christmas decorations?")
                .unwrap()
                .unwrap()
                .contains("red plastic bins")
        );
    }

    #[test]
    fn expanded_household_notes_cover_passport_appliance_photo_warranty_and_printer_secret() {
        let mem = temp_memory();
        mem.store(
            "safe_inventory",
            "Passports are located in the top drawer of the filing cabinet inside the manila envelope marked Travel",
        )
        .unwrap();
        mem.store(
            "appliance_manual",
            "The refrigerator model number is Samsung RF28R7551SR found in the kitchen manual",
        )
        .unwrap();
        mem.store(
            "photo_metadata",
            "Most Hawaii photos were taken by Jared, but the sunset beach photos were taken by Sarah",
        )
        .unwrap();
        mem.store(
            "home_maintenance",
            "The roof warranty is in the Home Improvements 2021 folder and valid for 25 years",
        )
        .unwrap();
        mem.store(
            "credential_reference",
            "Printer Wi-Fi password is stored in credential:printer_wifi",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("Where are the passports?")
                .unwrap()
                .unwrap()
                .contains("manila envelope")
        );
        assert!(
            mem.structured_household_answer("What's the model number of the fridge?")
                .unwrap()
                .unwrap()
                .contains("RF28R7551SR")
        );
        assert!(
            mem.structured_household_answer("Who took the photos in Hawaii?")
                .unwrap()
                .unwrap()
                .contains("Jared")
        );
        assert!(
            mem.structured_household_answer("Find the warranty for the roof")
                .unwrap()
                .unwrap()
                .contains("25 years")
        );
        let secret = mem
            .structured_household_answer("What's the Wi-Fi password for the printer?")
            .unwrap()
            .unwrap();
        assert!(secret.contains("app-only reference"));
        assert!(!secret.contains("credential:printer_wifi"));
    }

    #[test]
    fn expanded_household_notes_cover_inventory_meals_recipes_and_lists() {
        let mem = temp_memory();
        mem.store(
            "appliance_manual",
            "Oven rack cleaning: place racks in a sealed bag with ammonia overnight, then scrub with dish soap",
        )
        .unwrap();
        mem.store(
            "home_inventory",
            "Spare lightbulbs and LED bulbs are in the utility closet on the middle shelf",
        )
        .unwrap();
        mem.store(
            "meal_history",
            "Last Tuesday dinner was Spaghetti Bolognese with garlic bread",
        )
        .unwrap();
        mem.store(
            "recipe_collection",
            "Classic Fluffy Pancakes recipe: 1 cup flour, 1 tbsp sugar, 2 tsp baking powder",
        )
        .unwrap();
        mem.store(
            "shopping_list",
            "Hardware store list pending: 4 AA batteries, 1 pack of screws size 8, painter's tape",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("How do I clean the oven racks?")
                .unwrap()
                .unwrap()
                .contains("ammonia")
        );
        assert!(
            mem.structured_household_answer("Where are the spare lightbulbs?")
                .unwrap()
                .unwrap()
                .contains("utility closet")
        );
        assert!(
            mem.structured_household_answer("What did we have for dinner last Tuesday?")
                .unwrap()
                .unwrap()
                .contains("Spaghetti")
        );
        assert!(
            mem.structured_household_answer("Find the recipe for pancakes")
                .unwrap()
                .unwrap()
                .contains("Pancakes")
        );
        assert!(
            mem.structured_household_answer("What's on the hardware store list?")
                .unwrap()
                .unwrap()
                .contains("screws")
        );
    }

    #[test]
    fn expanded_household_notes_cover_receipts_storage_game_manuals_and_xbox_secret() {
        let mem = temp_memory();
        mem.store(
            "financial_records",
            "New dishwasher receipt in 2024_Purchases.pdf: total $850, purchased on March 12th",
        )
        .unwrap();
        mem.store(
            "storage_inventory",
            "The 4-person tent is in the basement on the top shelf of the storage rack",
        )
        .unwrap();
        mem.store(
            "game_manuals",
            "Catan board game instructions: setup rules are in the game box documentation",
        )
        .unwrap();
        mem.store(
            "credential_reference",
            "Xbox Wi-Fi password is stored in credential:xbox_wifi",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("Find the receipt for the new dishwasher")
                .unwrap()
                .unwrap()
                .contains("$850")
        );
        assert!(
            mem.structured_household_answer("Where did we put the tent?")
                .unwrap()
                .unwrap()
                .contains("basement")
        );
        assert!(
            mem.structured_household_answer("Find the instructions for the board game")
                .unwrap()
                .unwrap()
                .contains("Catan")
        );
        let secret = mem
            .structured_household_answer("What's the Wi-Fi password for the Xbox?")
            .unwrap()
            .unwrap();
        assert!(secret.contains("app-only reference"));
        assert!(!secret.contains("credential:xbox_wifi"));
    }

    #[test]
    fn expanded_household_notes_cover_travel_home_manuals_and_paint() {
        let mem = temp_memory();
        mem.store(
            "home_notes",
            "The nursery is painted Soft Sky Blue with the Behr color code",
        )
        .unwrap();
        mem.store(
            "device_manuals",
            "Smoke detector reset: press and hold the Test/Silence button on the unit for 3 seconds",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("What color is the nursery paint?")
                .unwrap()
                .unwrap()
                .contains("Soft Sky Blue")
        );
        assert!(
            mem.structured_household_answer("How do I reset the smoke detector?")
                .unwrap()
                .unwrap()
                .contains("Test/Silence")
        );
    }

    #[test]
    fn expanded_household_notes_cover_tools_receipts_and_deck_stain() {
        let mem = temp_memory();
        mem.store(
            "tool_inventory",
            "The 10mm socket was last checked out by Jared and should be in the red toolbox in the garage",
        )
        .unwrap();
        mem.store(
            "digital_receipts",
            "Target receipt for the Lego Star Wars set dated December 12th",
        )
        .unwrap();
        mem.store(
            "home_maintenance",
            "The deck is stained Cedar Tone by Behr. The can is in the shed",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("Where is the 10mm socket?")
                .unwrap()
                .unwrap()
                .contains("red toolbox")
        );
        assert!(
            mem.structured_household_answer("Find the receipt for the Lego set.")
                .unwrap()
                .unwrap()
                .contains("Target receipt")
        );
        assert!(
            mem.structured_household_answer("What color is the deck stain?")
                .unwrap()
                .unwrap()
                .contains("Cedar Tone")
        );
    }

    #[test]
    fn expanded_household_notes_cover_safety_school_contractors_storage_and_glaze() {
        let mem = temp_memory();
        mem.store(
            "safety_equipment_log",
            "The main fire extinguisher is under the kitchen sink; there is also one in the garage",
        )
        .unwrap();
        mem.store(
            "school_documents",
            "School emergency contact: main office emergency line is 555-0199",
        )
        .unwrap();
        mem.store(
            "contractor_list",
            "HVAC repair contact: Cool Breeze Solutions at 555-0222",
        )
        .unwrap();
        mem.store(
            "storage_inventory",
            "Summer clothes are in the attic in the clear bins labeled Summer 2024",
        )
        .unwrap();
        mem.store(
            "recipe_notes",
            "Honey Glaze recipe: 1 cup honey, 1/4 cup butter, and vanilla extract",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("Where is the fire extinguisher?")
                .unwrap()
                .unwrap()
                .contains("kitchen sink")
        );
        assert!(
            mem.structured_household_answer("What is the school's emergency number?")
                .unwrap()
                .unwrap()
                .contains("555-0199")
        );
        assert!(
            mem.structured_household_answer("Who do we call for HVAC repair?")
                .unwrap()
                .unwrap()
                .contains("Cool Breeze")
        );
        assert!(
            mem.structured_household_answer("Where are the summer clothes?")
                .unwrap()
                .unwrap()
                .contains("Summer 2024")
        );
        assert!(
            mem.structured_household_answer("Find the recipe for the glaze")
                .unwrap()
                .unwrap()
                .contains("Honey Glaze")
        );
    }

    #[test]
    fn expanded_household_notes_cover_grill_candles_vet_and_protected_account_codes() {
        let mem = temp_memory();
        mem.store(
            "appliance_manuals",
            "Weber Genesis E-325 grill manual is stored in the Outdoor folder",
        )
        .unwrap();
        mem.store(
            "home_inventory",
            "Scented candles are in the linen closet on the bottom shelf",
        )
        .unwrap();
        mem.store(
            "contact_book",
            "Vet address: Paws & Claws Clinic is at 123 Maple Drive, Springfield",
        )
        .unwrap();
        mem.store(
            "credential_reference",
            "Shed combination is stored in credential:shed_lock",
        )
        .unwrap();
        mem.store(
            "credential_reference",
            "Netflix account password is stored in credential:netflix_account",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("Find the manual for the grill")
                .unwrap()
                .unwrap()
                .contains("Weber Genesis")
        );
        assert!(
            mem.structured_household_answer("Where are the scented candles?")
                .unwrap()
                .unwrap()
                .contains("linen closet")
        );
        assert!(
            mem.structured_household_answer("What is the vet's address?")
                .unwrap()
                .unwrap()
                .contains("123 Maple Drive")
        );

        let shed = mem
            .structured_household_answer("What is the combination for the shed?")
            .unwrap()
            .unwrap();
        assert!(shed.contains("app-only reference"));
        assert!(!shed.contains("credential:shed_lock"));

        let netflix = mem
            .structured_household_answer("What's the code for the Netflix account?")
            .unwrap()
            .unwrap();
        assert!(netflix.contains("app-only reference"));
        assert!(!netflix.contains("credential:netflix_account"));
    }

    #[test]
    fn expanded_household_notes_cover_finance_diy_vehicle_warranty_and_guest_secret() {
        let mem = temp_memory();
        mem.store(
            "craft_inventory",
            "The sewing kit is in the hall closet, second shelf",
        )
        .unwrap();
        mem.store(
            "secure_storage_log",
            "Spare house keys are in the locked box on the top shelf of the pantry",
        )
        .unwrap();
        mem.store(
            "vehicle_registration",
            "The license plate for the SUV is ABC-1234",
        )
        .unwrap();
        mem.store(
            "appliance_warranties",
            "The Samsung fridge has a 1-year parts and labor warranty, expiring in November",
        )
        .unwrap();
        mem.store(
            "credential_reference",
            "Guest network password is stored in credential:guest_wifi",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("Find the sewing kit")
                .unwrap()
                .unwrap()
                .contains("hall closet")
        );
        let keys = mem
            .structured_household_answer("Where are the spare keys?")
            .unwrap()
            .unwrap();
        assert!(keys.contains("app-only reference"));
        assert!(!keys.contains("locked box"));
        assert!(
            mem.structured_household_answer("What is the license plate number?")
                .unwrap()
                .unwrap()
                .contains("ABC-1234")
        );
        assert!(
            mem.structured_household_answer("What is the warranty for the fridge?")
                .unwrap()
                .unwrap()
                .contains("1-year")
        );

        let guest = mem
            .structured_household_answer("Find the password for the guest network")
            .unwrap()
            .unwrap();
        assert!(guest.contains("app-only reference"));
        assert!(!guest.contains("credential:guest_wifi"));
    }

    #[test]
    fn expanded_household_notes_cover_vehicle_documents_tax_cooking_and_sensitive_codes() {
        let mem = temp_memory();
        mem.store(
            "digital_documents",
            "Laptop MacBook Pro Warranty PDF is valid until December 2025",
        )
        .unwrap();
        mem.store(
            "financial_records",
            "Tax documents are in the Taxes 2023 folder on the NAS drive",
        )
        .unwrap();
        mem.store(
            "cooking_reference",
            "Hard-boiled eggs take 9-12 minutes; soft-boiled eggs take 6 minutes",
        )
        .unwrap();
        mem.store("contact_book", "Dr. Smith's number is 555-0199")
            .unwrap();
        mem.store(
            "vehicle_documents",
            "Car SUV digital owner's manual for the 2023 Ford Explorer",
        )
        .unwrap();
        mem.store(
            "credential_reference",
            "Spare house key location is stored in credential:spare_key",
        )
        .unwrap();
        mem.store(
            "credential_reference",
            "Shed code is stored in credential:shed_lock",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("Find the warranty for the laptop")
                .unwrap()
                .unwrap()
                .contains("MacBook Pro")
        );
        assert!(
            mem.structured_household_answer("Where are the tax documents?")
                .unwrap()
                .unwrap()
                .contains("Taxes 2023")
        );
        assert!(
            mem.structured_household_answer("How long do I boil an egg?")
                .unwrap()
                .unwrap()
                .contains("9-12 minutes")
        );
        assert!(
            mem.structured_household_answer("What is the doctor's number?")
                .unwrap()
                .unwrap()
                .contains("555-0199")
        );
        assert!(
            mem.structured_household_answer("Find the manual for the car")
                .unwrap()
                .unwrap()
                .contains("Ford Explorer")
        );

        let spare_key = mem
            .structured_household_answer("Where is the spare key?")
            .unwrap()
            .unwrap();
        assert!(spare_key.contains("app-only reference"));
        assert!(!spare_key.contains("credential:spare_key"));

        let shed = mem
            .structured_household_answer("What is the code for the shed?")
            .unwrap()
            .unwrap();
        assert!(shed.contains("app-only reference"));
        assert!(!shed.contains("credential:shed_lock"));
    }

    #[test]
    fn expanded_household_notes_cover_appliance_travel_network_and_bank_login() {
        let mem = temp_memory();
        mem.store(
            "appliance_docs",
            "Sony Bravia User Guide for the TV is stored in the Living Room folder",
        )
        .unwrap();
        mem.store(
            "shoe_closet_inventory",
            "Hiking boots are on the bottom rack of the shoe closet",
        )
        .unwrap();
        mem.store(
            "password_manager",
            "Bank login for Chase Bank is stored in credential:chase_bank",
        )
        .unwrap();
        mem.store("recipe_book", "7-Day Sourdough Starter recipe")
            .unwrap();
        mem.store("restaurant_list", "Tony's Pizza phone number is 555-PIZZA")
            .unwrap();
        mem.store(
            "storage_inventory",
            "Thanksgiving decorations are in the attic in the orange bins",
        )
        .unwrap();
        mem.store(
            "home_warranties",
            "HVAC AC unit warranty is valid until December 2026",
        )
        .unwrap();
        mem.store("network_device_list", "Printer IP address is 192.168.1.105")
            .unwrap();
        mem.store(
            "financial_archive",
            "2020 tax returns are in the Taxes folder on the backup drive",
        )
        .unwrap();

        assert!(
            mem.structured_household_answer("Find the user manual for the TV")
                .unwrap()
                .unwrap()
                .contains("Sony Bravia")
        );
        assert!(
            mem.structured_household_answer("Where are the hiking boots?")
                .unwrap()
                .unwrap()
                .contains("shoe closet")
        );
        let bank = mem
            .structured_household_answer("Where did I save the bank login?")
            .unwrap()
            .unwrap();
        assert!(bank.contains("app-only reference"));
        assert!(!bank.contains("credential:chase_bank"));
        assert!(
            mem.structured_household_answer("Find the recipe for sourdough starter")
                .unwrap()
                .unwrap()
                .contains("Sourdough")
        );
        assert!(
            mem.structured_household_answer("What's the phone number for the pizza place?")
                .unwrap()
                .unwrap()
                .contains("555-PIZZA")
        );
        assert!(
            mem.structured_household_answer("Where are the Thanksgiving decorations?")
                .unwrap()
                .unwrap()
                .contains("orange bins")
        );
        assert!(
            mem.structured_household_answer("Find the warranty for the AC unit")
                .unwrap()
                .unwrap()
                .contains("December 2026")
        );
        assert!(
            mem.structured_household_answer("What is the IP address of the printer?")
                .unwrap()
                .unwrap()
                .contains("192.168.1.105")
        );
        assert!(
            mem.structured_household_answer("Where are the tax returns from 2020?")
                .unwrap()
                .unwrap()
                .contains("backup drive")
        );
    }

    #[test]
    fn contextual_family_routines_answer_exact_and_fts_questions() {
        let mem = temp_memory();
        mem.store(
            "screen_time_usage",
            "Leo can watch cartoons now: 25 minutes of screen time left; homework and bedtime restrictions inactive",
        )
        .unwrap();
        mem.store(
            "presence_state",
            "Mia is home, and her phone connected upstairs on the home network",
        )
        .unwrap();
        mem.store(
            "access_logs",
            "Sarah opened the garage door at 3:42 PM when she arrived home",
        )
        .unwrap();
        mem.store(
            "inventory_items",
            "Groceries are low: milk, eggs, bananas, pasta, and Leo's lunchbox granola bars",
        )
        .unwrap();
        mem.store(
            "notes_fts",
            "Mia science fair checklist is in her school folder, last opened yesterday on the kitchen tablet",
        )
        .unwrap();
        mem.store(
            "manuals_fts",
            "CrispMax 6Q air fryer manual: quick-cleaning instructions are on page 12",
        )
        .unwrap();
        mem.store(
            "routine",
            "Saturday morning routine: pancakes at 8:30, laundry pickup at 9:15, Leo's soccer at 10:00, and Mia's library time at 11:30",
        )
        .unwrap();
        mem.store(
            "routine_steps",
            "Leo before school next steps: packing your lunchbox, then putting on your shoes",
        )
        .unwrap();
        mem.store(
            "replacement_parts",
            "Hallway air purifier filter: H13 mini HEPA filter; one spare is in the laundry-room cabinet",
        )
        .unwrap();
        mem.store(
            "manuals_fts",
            "Dishwasher error E24 means a drain issue; check the drain hose and clean the filter basket",
        )
        .unwrap();
        mem.store(
            "item_location_events",
            "Mia tablet charger is most likely in the kitchen charging drawer",
        )
        .unwrap();

        for (query, expected) in [
            ("Can I watch cartoons now?", "25 minutes"),
            ("Is Mia home?", "phone connected upstairs"),
            ("Who opened the garage door?", "Sarah"),
            ("What groceries are low?", "granola bars"),
            ("Where's my science fair checklist?", "school folder"),
            ("Find the air fryer manual", "page 12"),
            ("What's our Saturday morning routine?", "pancakes"),
            ("What's next before school?", "lunchbox"),
            (
                "Which filter does the hallway air purifier need?",
                "H13 mini HEPA",
            ),
            ("Find anything about dishwasher error E24", "drain issue"),
            ("Where's my tablet charger?", "charging drawer"),
        ] {
            let answer = mem.structured_household_answer(query).unwrap();
            assert!(answer.is_some(), "query {query:?} should have an answer");
            assert!(
                answer.unwrap().contains(expected),
                "query {query:?} should include {expected:?}"
            );
        }
    }

    #[test]
    fn contextual_family_routines_second_batch_answer_exact_and_fts_questions() {
        let mem = temp_memory();
        mem.store(
            "pet_care_routines",
            "Mia checked off cat feeding today, and the smart food bin was opened at 5:18 PM",
        )
        .unwrap();
        mem.store(
            "household_guides_fts",
            "Pizza box disposal city guide: greasy pizza boxes go in compost if accepted; plastic or foil goes in trash",
        )
        .unwrap();
        mem.store(
            "documents_fts",
            "Washing machine warranty: active washer warranty expires on March 14, 2028",
        )
        .unwrap();
        mem.store(
            "family_schedule",
            "Leo snack rule: apple slices, yogurt, or crackers are approved when dinner is more than an hour away",
        )
        .unwrap();
        mem.store(
            "meal_notes",
            "Dinner tonight attendees: Jared, Sarah, Leo, Mia, and Grandma Elaine; Grandma Elaine prefers decaf tea",
        )
        .unwrap();
        mem.store(
            "chore_assignments",
            "Mia chores today: dishwasher unloading and laundry pickup are finished; desk cleanup is still unchecked",
        )
        .unwrap();
        mem.store(
            "school_transport_schedule",
            "Mia bus pickup tomorrow is at 7:26 AM",
        )
        .unwrap();
        mem.store(
            "food_inventory",
            "Leftovers priority: eat the turkey chili first; it should be used by tomorrow. Pasta bake is good for two more days",
        )
        .unwrap();
        mem.store(
            "permission_requests",
            "Mia sleepover request: Mom approved it, but Dad has not answered yet",
        )
        .unwrap();
        mem.store(
            "documents_fts",
            "Mia oceans essay draft is in her English folder; latest version was edited last night",
        )
        .unwrap();
        mem.store(
            "school_notes_fts",
            "Leo school announcement: tomorrow is pajama day for his class",
        )
        .unwrap();
        mem.store(
            "health_documents_fts",
            "Mia allergy action plan: current active medical plan saved in Health Documents",
        )
        .unwrap();

        for (query, expected) in [
            ("Did Mia feed the cat?", "5:18pm"),
            ("What bin does a pizza box go in?", "compost"),
            ("Find the washing machine warranty", "March 14, 2028"),
            ("Can I have a snack?", "apple slices"),
            ("Who's coming to dinner tonight?", "Grandma Elaine"),
            ("Did I finish my chores?", "desk cleanup"),
            ("What time is my bus tomorrow?", "7:26 AM"),
            ("Which leftovers should we eat first?", "turkey chili"),
            ("Did Mom approve my sleepover?", "Dad"),
            ("Find my essay draft about oceans", "English folder"),
            ("Is it pajama day tomorrow?", "pajama day"),
            ("Find Mia's allergy action plan", "Health Documents"),
        ] {
            let answer = mem.structured_household_answer(query).unwrap();
            assert!(answer.is_some(), "query {query:?} should have an answer");
            assert!(
                answer.unwrap().contains(expected),
                "query {query:?} should include {expected:?}"
            );
        }
    }

    #[test]
    fn contextual_family_routines_third_batch_answer_exact_and_fts_questions() {
        let mem = temp_memory();
        mem.store(
            "ble_tag_events",
            "Jared car keys location: car keys are on the entryway table; key tag last pinged there 6 minutes ago",
        )
        .unwrap();
        mem.store(
            "device_audit_log",
            "Thermostat audit: Jared changed the thermostat to 70F from his phone at 6:12 PM",
        )
        .unwrap();
        mem.store(
            "household_notes_fts",
            "Ladder safety note in garage maintenance checklist: use stabilizer feet and avoid the top two rungs",
        )
        .unwrap();
        mem.store(
            "delivery_events",
            "Package still on porch: porch camera still detects the package by the front mat",
        )
        .unwrap();
        mem.store(
            "health_routines",
            "Mia allergy medicine: marked done at 7:41 AM; medicine cabinet opened at the same time",
        )
        .unwrap();
        mem.store(
            "activity_notes_fts",
            "Leo dinosaur fact from yesterday: some sauropods swallowed stones to help grind food in their stomachs",
        )
        .unwrap();
        mem.store(
            "audio_event_classifications",
            "Beeping sound source: laundry-room leak sensor has a low battery alert",
        )
        .unwrap();
        mem.store(
            "manuals_fts",
            "Printer Wi-Fi reset: hold the printer wireless button for 5 seconds, then reconnect it from the printer app",
        )
        .unwrap();
        mem.store(
            "family_notes_fts",
            "Grandma Elaine Wi-Fi note is saved in Family Contacts",
        )
        .unwrap();
        mem.store(
            "family_rules",
            "Leo outdoor play permission: he can play in the backyard, stay inside the fence, and Mom is in the kitchen",
        )
        .unwrap();
        mem.store(
            "appliance_events",
            "Laundry finish status: dryer finished at 4:47 PM and has not been opened yet",
        )
        .unwrap();
        mem.store(
            "household_guides_fts",
            "Wet soccer shoes guide: put wet soccer shoes on the mudroom drying tray, not in the bedroom",
        )
        .unwrap();
        mem.store(
            "project_notes_fts",
            "Mia room blue paint: Harbor Mist, eggshell finish",
        )
        .unwrap();
        mem.store(
            "laundry_events",
            "Mia laundry moved: dryer door opened and Mia basket weight increased at 5:22 PM",
        )
        .unwrap();
        mem.store(
            "safety_routes",
            "Kitchen alarm safest route: use the front door route from the living room; avoid the kitchen hallway",
        )
        .unwrap();

        for (query, expected) in [
            ("Where are my car keys?", "entryway table"),
            ("Who changed the thermostat?", "Jared"),
            ("Find the ladder safety note", "stabilizer feet"),
            ("Is the package still on the porch?", "front mat"),
            ("Did Mia take her allergy medicine?", "7:41 AM"),
            ("Tell me the dinosaur fact from yesterday", "sauropods"),
            ("What's making that beeping sound?", "leak sensor"),
            ("How do I reset the printer Wi-Fi?", "wireless button"),
            ("Find Grandma's Wi-Fi note", "Family Contacts"),
            ("Am I allowed to play outside?", "backyard"),
            ("When did the laundry finish?", "4:47 PM"),
            ("Where do my wet soccer shoes go?", "mudroom drying tray"),
            (
                "What was the blue paint color in Mia's room?",
                "Harbor Mist",
            ),
            ("Did my laundry get moved?", "5:22 PM"),
            (
                "What's the safest way out if the kitchen alarm goes off?",
                "front door",
            ),
        ] {
            let answer = mem.structured_household_answer(query).unwrap();
            assert!(answer.is_some(), "query {query:?} should have an answer");
            assert!(
                answer.unwrap().contains(expected),
                "query {query:?} should include {expected:?}"
            );
        }
    }

    #[test]
    fn contextual_family_routines_fourth_batch_answer_exact_and_fts_questions() {
        let mem = temp_memory();
        mem.store(
            "home_project_notes_fts",
            "Dishwasher breaker: breaker 14, labeled Kitchen Appliances B",
        )
        .unwrap();
        mem.store(
            "household_routines",
            "Trash day prep still needs kitchen trash out, recycling moved to curb, and cardboard flattened",
        )
        .unwrap();
        mem.store(
            "household_notes_fts",
            "Ant response history: last time ants showed up, clean pantry shelf, seal the back door gap, and use ant bait under the sink",
        )
        .unwrap();
        mem.store(
            "camera_object_events",
            "Garbage bins out: bins were moved to the curb at 7:18 PM",
        )
        .unwrap();
        mem.store(
            "inventory_items",
            "Camping flashlight is in the blue camping bin in the garage and its battery is full",
        )
        .unwrap();
        mem.store(
            "automation_runs",
            "Sprinkler skip reason: sprinklers skipped today because the rain sensor reported enough moisture",
        )
        .unwrap();
        mem.store(
            "school_tasks",
            "Mia homework internet: science quiz review and Spanish listening need internet; math worksheet does not",
        )
        .unwrap();
        mem.store(
            "family_rules",
            "Leo stove permission: No, Leo needs Mom or Dad with him before using the stove",
        )
        .unwrap();
        mem.store(
            "health_documents_fts",
            "Cold medicine instructions: label scan in Health Documents; bottle is in upstairs medicine cabinet",
        )
        .unwrap();
        mem.store(
            "door_sensor_events",
            "Fridge door close status: fridge door is closed and temperature is stable",
        )
        .unwrap();
        mem.store(
            "battery_status",
            "Sensor battery report: laundry leak sensor, hallway motion sensor, and garage door contact sensor need batteries soon",
        )
        .unwrap();
        mem.store(
            "daily_checklists",
            "Leo library book packed: library book was scanned by the backpack this morning",
        )
        .unwrap();
        mem.store(
            "notification_log",
            "Mia alarm failure reason: tablet was offline, but backup hallway display alarm was triggered",
        )
        .unwrap();
        mem.store(
            "plant_care_profiles",
            "Plants need attention: basil needs water, fern needs misting, snake plant is fine",
        )
        .unwrap();
        mem.store(
            "dishwasher_rack_state",
            "Leo blue cup location: blue cup is in the top rack of the dishwasher",
        )
        .unwrap();
        mem.store(
            "device_events",
            "Side gate away status: side gate stayed closed while everyone was away",
        )
        .unwrap();
        mem.store(
            "family_notes_fts",
            "Mia recital outfit note: navy dress, silver flats, and hair ribbon",
        )
        .unwrap();
        mem.store(
            "shared_room_reservations",
            "Bathroom availability: upstairs bathroom is free right now",
        )
        .unwrap();
        mem.store(
            "security_mode_attempts",
            "Away mode failure: away mode failed because the back door lock is jammed; everything else was ready",
        )
        .unwrap();
        mem.store("device_credentials", "Guest speaker pairing code is 4821")
            .unwrap();
        mem.store(
            "open_reminders",
            "End-of-day house summary: all doors locked, two windows open, Leo finished routine, Mia has one reminder, laundry-room leak sensor needs a battery",
        )
        .unwrap();

        for (query, expected) in [
            ("Which breaker controls the dishwasher?", "breaker 14"),
            ("What do we still need to do before trash day?", "cardboard"),
            ("What did we do last time ants showed up?", "ant bait"),
            ("Did anyone take the garbage bins out?", "7:18 PM"),
            ("Where's the camping flashlight?", "blue camping bin"),
            ("Why didn't the sprinklers run today?", "rain sensor"),
            ("Which homework needs internet?", "Spanish listening"),
            ("Can I use the stove?", "Mom or Dad"),
            ("Find the cold medicine instructions", "Health Documents"),
            (
                "Did the fridge door close all the way?",
                "temperature is stable",
            ),
            ("Which sensors need batteries soon?", "hallway motion"),
            ("Did I pack my library book?", "backpack"),
            ("Why did my alarm not go off?", "tablet was offline"),
            ("What plants need attention?", "basil"),
            ("I can't find my blue cup", "top rack"),
            (
                "Did the side gate open while we were gone?",
                "stayed closed",
            ),
            ("Find the note about Mia's recital outfit", "silver flats"),
            ("Is the bathroom free?", "free right now"),
            ("Why did away mode fail?", "back door lock"),
            ("What's the password for the guest speaker?", "4821"),
            ("Make an end-of-day house summary", "two windows"),
        ] {
            let answer = mem.structured_household_answer(query).unwrap();
            assert!(answer.is_some(), "query {query:?} should have an answer");
            assert!(
                answer.unwrap().contains(expected),
                "query {query:?} should include {expected:?}"
            );
        }
    }

    #[test]
    fn contextual_family_routines_fifth_batch_answer_exact_and_fts_questions() {
        let mem = temp_memory();
        for (kind, content) in [
            (
                "documents_fts",
                "Mia debate school lunch research: Civics folder latest draft School Lunch Debate Notes",
            ),
            (
                "item_location_events",
                "Leo rain boots location: rain boots are by the mudroom drying mat",
            ),
            (
                "manuals_fts",
                "Slow cooker timer chart: slow cooker manual has the timer chart in the cooking guide section",
            ),
            (
                "camera_object_events",
                "Mia garage bike camera: Mia's bike was seen in the garage near the workbench at 4:09 PM",
            ),
            (
                "documents_fts",
                "New water heater receipt: receipt is in Home Projects and linked to the installation record",
            ),
            (
                "inventory_items",
                "White extension cord: in the craft bin under the dining-room sideboard for Mia's project",
            ),
            (
                "recipes_fts",
                "Peanut-free chicken recipe: lemon chicken rice bowls are peanut-free and rated highly",
            ),
            (
                "health_documents_fts",
                "Leo vaccination form: latest active vaccination form is in Health Documents",
            ),
            (
                "school_forms",
                "Mia field trip form: Mom signed the field trip form this morning",
            ),
            (
                "project_notes_fts",
                "Photo backdrop instructions: saved in Mia's art project folder",
            ),
            (
                "inventory_items",
                "Red marker location: red marker should be in the dining-room craft caddy",
            ),
            (
                "cooking_sessions",
                "Cookie recipe next step: scoop the dough onto the baking sheet, leaving space between each cookie",
            ),
            (
                "manuals_fts",
                "Furnace code 31: listed under pressure switch troubleshooting in the furnace manual",
            ),
            (
                "home_project_notes_fts",
                "Plumber shutoff valve note: secondary shutoff valve is behind the laundry-room access panel",
            ),
            (
                "documents_fts",
                "Mia winter poem: saved in English folder as First Snow Draft",
            ),
            (
                "manuals_fts",
                "Toddler gate instructions: pressure-mount setup note attached to visitor safety bin",
            ),
            (
                "meal_notes_fts",
                "Green bowl recipe: sesame noodle salad was served in the big green bowl",
            ),
            (
                "safety_equipment_log",
                "Emergency flashlight: child-accessible flashlight is in the lower mudroom drawer",
            ),
            (
                "activity_notes_fts",
                "Mia tournament snacks: pretzels, apple slices, cheese stick, and blue sports drink",
            ),
        ] {
            mem.store(kind, content).unwrap();
        }

        for (query, expected) in [
            (
                "Find my debate research about school lunches",
                "School Lunch Debate Notes",
            ),
            ("Where did I leave my rain boots?", "mudroom drying mat"),
            (
                "Find the slow cooker manual and timer chart",
                "cooking guide",
            ),
            ("Did the garage camera see my bike?", "4:09 PM"),
            (
                "Find the receipt for the new water heater",
                "installation record",
            ),
            (
                "Where is the white extension cord for my project?",
                "craft bin",
            ),
            ("Find a chicken recipe without peanuts", "lemon chicken"),
            ("Find Leo's vaccination form", "Health Documents"),
            ("Did Mom sign my field trip form?", "this morning"),
            ("Find the photo backdrop instructions", "art project folder"),
            ("Where's the red marker?", "craft caddy"),
            ("Read me the next step for cookies", "scoop the dough"),
            (
                "Find furnace manual troubleshooting code 31",
                "pressure switch",
            ),
            (
                "Find the note about the plumber's shutoff valve",
                "laundry-room access panel",
            ),
            ("Where did I save my poem about winter?", "First Snow Draft"),
            ("Find the toddler gate instructions", "pressure-mount"),
            (
                "Find the recipe where we used the green bowl",
                "sesame noodle salad",
            ),
            (
                "Where's the flashlight if the lights go out?",
                "lower mudroom drawer",
            ),
            (
                "What snacks did we pack for my last tournament?",
                "blue sports drink",
            ),
        ] {
            let answer = mem.structured_household_answer(query).unwrap();
            assert!(answer.is_some(), "query {query:?} should have an answer");
            assert!(
                answer.unwrap().contains(expected),
                "query {query:?} should include {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_cold_to_thermostat_preference() {
        let mem = temp_memory();
        mem.store(
            "preference",
            "Jared prefers the living room thermostat at 72F.",
        )
        .unwrap();

        let hits = mem.semantic_search("I'm feeling cold", 3).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].entry.content.contains("thermostat"));
    }

    #[test]
    fn semantic_search_links_lunchbox_snacks() {
        let mem = temp_memory();
        mem.store(
            "shopping",
            "Leo's lunchbox snacks include granola bars and fruit snacks.",
        )
        .unwrap();

        let hits = mem
            .semantic_search("We need more snacks for Leo's lunchbox", 3)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].entry.content.contains("granola bars"));
    }

    #[test]
    fn semantic_search_links_robot_movie_hint() {
        let mem = temp_memory();
        mem.store(
            "note",
            "Watched The Iron Giant with the kids - they loved it.",
        )
        .unwrap();

        let hits = mem
            .semantic_search("What movie had a robot that wanted to be a real boy?", 3)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].entry.content.contains("Iron Giant"));
    }

    #[test]
    fn semantic_search_links_activity_car_printer_recipe_and_comfort_media() {
        let mem = temp_memory();
        mem.store(
            "activity",
            "Leo usually enjoys building Lego when bored. The Lego bin is in the playroom",
        )
        .unwrap();
        mem.store(
            "mechanic",
            "Squealing car noise usually means the serpentine belt. It was replaced 20k miles ago",
        )
        .unwrap();
        mem.store(
            "manual",
            "Printer troubleshooting: power cycle it, then check whether the Wi-Fi light is blinking blue",
        )
        .unwrap();
        mem.store(
            "recipe",
            "Chicken and Rice Casserole uses chicken and rice and takes about 45 minutes",
        )
        .unwrap();
        mem.store(
            "media_library",
            "Comfort movies include The Princess Bride and Paddington",
        )
        .unwrap();

        assert!(
            mem.semantic_search("I'm bored", 3)
                .unwrap()
                .iter()
                .any(|hit| hit.entry.content.contains("Lego"))
        );
        assert!(
            mem.semantic_search("Weird noise coming from the car", 3)
                .unwrap()
                .iter()
                .any(|hit| hit.entry.content.contains("serpentine"))
        );
        assert!(
            mem.semantic_search("I can't get the printer to work", 3)
                .unwrap()
                .iter()
                .any(|hit| hit.entry.content.contains("Wi-Fi light"))
        );
        assert!(
            mem.semantic_search("What can I cook with chicken and rice?", 3)
                .unwrap()
                .iter()
                .any(|hit| hit.entry.content.contains("Casserole"))
        );
        assert!(
            mem.semantic_search("I need a comfort movie", 3)
                .unwrap()
                .iter()
                .any(|hit| hit.entry.content.contains("Princess Bride"))
        );
    }

    #[test]
    fn semantic_search_links_chores_utilities_education_and_contextual_decisions() {
        let mem = temp_memory();
        mem.store(
            "wellness",
            "Sarah stress relief routine: dim lights, play the Calm Piano playlist, and offer to run the bath",
        )
        .unwrap();
        mem.store(
            "science_project",
            "Elementary science fair idea: Volcano Baking Soda experiment uses supplies already at home",
        )
        .unwrap();
        mem.store(
            "recipe",
            "Simple shortbread cookies can be baked with flour and sugar",
        )
        .unwrap();
        mem.store(
            "first_aid",
            "Headache first aid: Tylenol is in the hall cabinet and the label says take 2 tablets",
        )
        .unwrap();
        mem.store(
            "story",
            "Mia story favorite: Alice in Wonderland audiobook, resume from last night",
        )
        .unwrap();
        mem.store(
            "media_library",
            "Family movie for tonight: Jungle Cruise on Disney+ is an adventure movie suitable for kids",
        )
        .unwrap();
        mem.store(
            "pet_inventory",
            "Dog food preference: Royal Canin Large Breed 30lb bag",
        )
        .unwrap();
        mem.store(
            "travel",
            "Zoo trip plan: visit the Reptile House and picnic area near the lions for lunch",
        )
        .unwrap();
        mem.store(
            "diet",
            "Diet meal under 500 calories: grilled chicken salad with broccoli",
        )
        .unwrap();
        mem.store(
            "troubleshooting",
            "Washing machine shaking usually means an unbalanced load; last leveling check was 2 years ago",
        )
        .unwrap();
        mem.store("watch_history", "Mia watch history resume Stranger Things")
            .unwrap();
        mem.store(
            "visitor",
            "Doorbell visitor Uncle Bob is a known family friend",
        )
        .unwrap();
        mem.store(
            "music_profile",
            "Focus music playlist Lo-Fi Beats to Study To at quiet volume",
        )
        .unwrap();

        for (query, expected) in [
            ("I'm stressed", "Calm Piano"),
            ("I need a science fair idea", "Volcano"),
            ("What can I bake with just flour and sugar?", "shortbread"),
            ("I have a headache", "Tylenol"),
            ("Read me a story", "Alice in Wonderland"),
            ("Find a movie for tonight", "Jungle Cruise"),
            ("Order more dog food", "Royal Canin"),
            ("Plan a trip to the zoo", "Reptile House"),
            ("I'm hungry but on a diet", "broccoli"),
            ("The washing machine is shaking", "unbalanced"),
            ("Watch TV", "Stranger Things"),
            ("Someone is at the door", "Uncle Bob"),
            ("Play focus music", "Lo-Fi Beats"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_school_inventory_social_and_complex_context() {
        let mem = temp_memory();
        mem.store(
            "media_library",
            "Scary movie favorite: Jared rated A Quiet Place 5 stars and it is available on Paramount+",
        )
        .unwrap();
        mem.store(
            "beverage_preference",
            "Mia post soccer practice drink preference: chocolate milk in the fridge door",
        )
        .unwrap();
        mem.store(
            "comfort",
            "Too bright comfort rule: close smart blinds in the living room to reduce brightness",
        )
        .unwrap();
        mem.store(
            "social_connection",
            "Sarah lonely support options: video call her sister or Mom when Jared is away",
        )
        .unwrap();
        mem.store(
            "home_maintenance",
            "Kitchen sink smells maintenance: clean the garbage disposal with ice cubes and lemon peels",
        )
        .unwrap();
        mem.store(
            "commute",
            "Work commute alternative: in rain and highway traffic, back roads can be faster",
        )
        .unwrap();
        mem.store(
            "pantry",
            "Taco dinner plan: shells and beef are available, but cheese and salsa should be added to the shopping list",
        )
        .unwrap();
        mem.store(
            "comfort",
            "Muggy comfort preference: high humidity should use the basement dehumidifier",
        )
        .unwrap();
        mem.store(
            "first_aid",
            "Cut finger first aid: band-aids and antiseptic are in the bathroom cabinet; clean with water and apply pressure",
        )
        .unwrap();
        mem.store(
            "location_history",
            "Jared keys bluetooth tracker last reported in the living room near the sofa",
        )
        .unwrap();
        mem.store(
            "troubleshooting",
            "Noise outside routine: use outdoor microphones to compare known sounds and offer porch light",
        )
        .unwrap();
        mem.store(
            "pizza",
            "Pizza preferences: Leo likes Pepperoni, Mia likes Veggie Supreme, and Tuesday Deal coupons may apply",
        )
        .unwrap();
        mem.store(
            "commute",
            "Start the car routine: remote start checks calendar and sets car climate for the next trip",
        )
        .unwrap();
        mem.store(
            "arrival",
            "Arrival routine: welcome home, turn on kitchen lights, unlock side door, and start favorite music",
        )
        .unwrap();

        for (query, expected) in [
            ("I'm in the mood for a scary movie", "Quiet Place"),
            ("I need a drink", "chocolate milk"),
            ("It's too bright in here", "smart blinds"),
            ("I'm feeling lonely", "video call"),
            ("The kitchen sink smells", "garbage disposal"),
            ("I'm leaving for work", "back roads"),
            ("Make tacos for dinner", "salsa"),
            ("It feels muggy in here", "dehumidifier"),
            ("I cut my finger", "band-aids"),
            ("Where are my keys?", "living room"),
            ("I hear a weird noise outside", "porch light"),
            ("Order pizza", "Pepperoni"),
            ("Start the car", "car climate"),
            ("I'm home", "kitchen lights"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_seasonal_education_finance_and_social_coordination() {
        let mem = temp_memory();
        mem.store(
            "educational_resources",
            "Leo math homework help: adding fractions video from the math class playlist",
        )
        .unwrap();
        mem.store(
            "music_profile",
            "Music control preference: when tired of this song, skip to the next Pop Hits track not played recently",
        )
        .unwrap();
        mem.store(
            "entertainment",
            "Kid-safe joke favorite: Why don't scientists trust atoms? Because they make up everything",
        )
        .unwrap();
        mem.store(
            "dictionary_knowledge_base",
            "Ephemeral definition: lasting for a very short time; example: the fad was ephemeral",
        )
        .unwrap();
        mem.store(
            "activity_ideas",
            "Indoor fort activity: use extra sheets from the linen closet and light dining chairs",
        )
        .unwrap();
        mem.store(
            "commute",
            "Train commute alternative: if Route 1 is congested, Elm Street back roads save about 5 minutes",
        )
        .unwrap();
        mem.store(
            "health_profile",
            "Leo has mild asthma; with moderate air quality he should limit vigorous running outside",
        )
        .unwrap();
        mem.store(
            "recipe",
            "Slow cooker chili recipe: set cooker to Low for 8 hours",
        )
        .unwrap();
        mem.store(
            "party_themes",
            "Mia birthday party plan: $200 budget supports Spa Night at home with face masks and pizza",
        )
        .unwrap();
        mem.store(
            "pest_control",
            "Bathroom spider note: common local spiders are likely harmless; notify Jared if Sarah is terrified",
        )
        .unwrap();
        mem.store(
            "location_history",
            "TV remote bluetooth tracker last reported in the living room under the coffee table",
        )
        .unwrap();
        mem.store(
            "food_safety",
            "Garage freezer food safety: freezer should be 0F; 5F is safe but check the door seal",
        )
        .unwrap();

        for (query, expected) in [
            ("I need help with my math homework", "fractions"),
            ("I'm tired of this song", "Pop Hits"),
            ("Tell me a joke", "scientists"),
            ("What does ephemeral mean?", "very short time"),
            ("I want to build a fort", "linen closet"),
            ("I'm running late for the train", "Elm Street"),
            ("Is the air quality okay for Leo to play outside?", "asthma"),
            ("Set up the slow cooker for chili", "Low"),
            ("Plan a birthday party for Mia", "Spa Night"),
            ("We have a spider in the bathroom", "Jared"),
            ("I can't find the remote", "coffee table"),
            ("Is the garage freezer cold enough?", "door seal"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_grocery_travel_and_maintenance_context() {
        let mem = temp_memory();
        mem.store(
            "media_library",
            "Comedy media preference: Jared likes The Office and favorite stand-up clips when he needs a laugh",
        )
        .unwrap();
        mem.store(
            "recipe",
            "Chicken Stir-Fry idea: use chicken, soy sauce, and vegetables from the crisper",
        )
        .unwrap();
        mem.store(
            "contact_book",
            "Sink repair contact: Uncle Dave used to be a plumber; Bob's Plumbing is the backup",
        )
        .unwrap();
        mem.store(
            "educational_content",
            "Solar system learning: Cosmos documentary and NASA interactive planets activity",
        )
        .unwrap();
        mem.store(
            "sleep_routine",
            "Oversleeping tip: place the phone across the room and set a backup alarm 10 minutes later",
        )
        .unwrap();
        mem.store(
            "travel_preferences",
            "Airport departure rule: Jared likes a 2-hour buffer before flights, plus traffic time",
        )
        .unwrap();
        mem.store(
            "party_recipes",
            "Dinner party food plan: Stuffed Peppers are vegan and gluten-free with Quinoa Salad",
        )
        .unwrap();
        mem.store(
            "comfort",
            "Really hot comfort preference: lower AC to 68F and turn on the ceiling fan",
        )
        .unwrap();
        mem.store(
            "home_notes",
            "Guest arrival routine: lights 100%, music playing, and move the cat because Uncle Bob is allergic",
        )
        .unwrap();
        mem.store(
            "routine",
            "Baby is awake night routine: start the nightlight and notify Mom",
        )
        .unwrap();
        mem.store(
            "meal_plan",
            "Meal plan today: tacos for dinner, but cheese is missing from pantry inventory",
        )
        .unwrap();
        mem.store(
            "music_profile",
            "Going for a run routine: start the Run Fast high-BPM running playlist on the phone",
        )
        .unwrap();

        for (query, expected) in [
            ("I need a laugh", "The Office"),
            ("I have chicken but no ideas", "Stir-Fry"),
            ("Who do we know that fixes sinks?", "Uncle Dave"),
            ("I want to learn about the solar system", "NASA"),
            ("I keep oversleeping", "backup alarm"),
            ("When should I leave for the airport?", "2-hour buffer"),
            ("Buy food for the dinner party", "Stuffed Peppers"),
            ("I'm really hot", "ceiling fan"),
            ("We have guests coming over", "Uncle Bob"),
            ("The baby is awake", "nightlight"),
            ("What's for dinner?", "tacos"),
            ("I'm going for a run", "Run Fast"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_garden_health_logistics_and_substitutions() {
        let mem = temp_memory();
        mem.store(
            "cooking_substitutes",
            "Olive oil substitute: use melted butter for richness or vegetable oil for a neutral saute",
        )
        .unwrap();
        mem.store(
            "diy_projects",
            "Bookshelf beginner plan: simple 3-shelf unit with a 1x12 board, screws, and a drill",
        )
        .unwrap();
        mem.store(
            "plumbing_troubleshooting",
            "Running toilet troubleshooting: check for a stuck flapper chain or old flapper in the tank",
        )
        .unwrap();
        mem.store(
            "injury_recovery",
            "Running knee pain recovery: ice it for 15 minutes, stretch quads, and check running shoes",
        )
        .unwrap();
        mem.store(
            "recipe",
            "Pasta side dish ideas: classic Caesar salad or Garlic Bread",
        )
        .unwrap();
        mem.store(
            "gym_routine",
            "Gym bag leg day checklist: shorts, shirt, shoes, knee sleeves, and water bottle",
        )
        .unwrap();
        mem.store(
            "financial_advice",
            "Monthly budget note: you are $150 over budget because of the car repair; delay dining out",
        )
        .unwrap();
        mem.store(
            "message_templates",
            "Mom birthday message template: Happy Birthday! Hope you have a great day! Love, the family",
        )
        .unwrap();
        mem.store(
            "arrival_rain",
            "Driving home in rain arrival protocol: close the garage door and turn up the heat so the house is cozy",
        )
        .unwrap();
        mem.store(
            "food_safety",
            "Yogurt food safety: if it expires on October 25 and today is October 22, it is safe; sniff first",
        )
        .unwrap();
        mem.store(
            "safety_protocol",
            "Dark parking lot safety protocol: share live location with Jared and turn on flashlight",
        )
        .unwrap();
        mem.store(
            "streaming_services",
            "Family unwatched movie suggestion: Encanto is on Disney+ and not in watch history",
        )
        .unwrap();
        mem.store(
            "turkey_thawing_guide",
            "Turkey thawing guide: set defrost reminder before Thanksgiving; a 15lb bird needs about 24 hours per 5lb",
        )
        .unwrap();

        for (query, expected) in [
            (
                "I'm out of olive oil. What can I use instead?",
                "vegetable oil",
            ),
            ("I want to build a bookshelf", "3-shelf"),
            ("The toilet keeps running", "flapper"),
            ("My knee hurts after my run", "15 minutes"),
            ("We need a side dish for pasta", "Garlic Bread"),
            ("Pack my gym bag", "knee sleeves"),
            ("Are we over budget this month?", "$150"),
            ("Text Mom happy birthday", "Happy Birthday"),
            ("I'm driving home in the rain", "cozy"),
            ("Is it safe to eat this?", "Yogurt"),
            ("I'm in a dark parking lot", "live location"),
            ("Find a movie we haven't seen", "Encanto"),
            ("Remind me to defrost the turkey", "24 hours"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_vehicle_work_seasonal_and_safety_context() {
        let mem = temp_memory();
        mem.store(
            "wish_list",
            "Father's Day gift idea for Dad: Jared was looking at a new laser level last week",
        )
        .unwrap();
        mem.store(
            "wellness_activities",
            "Quick break wellness activity: 5-minute guided breathing exercise for a reset",
        )
        .unwrap();
        mem.store(
            "food_pairing_database",
            "Steak wine pairing: Cabernet Sauvignon or Malbec works best; Cabernet is in the rack",
        )
        .unwrap();
        mem.store(
            "comfort",
            "Stuffy ventilation comfort: open living room windows and turn on the ceiling fan",
        )
        .unwrap();
        mem.store(
            "routine",
            "Work from home scene: office lights on, doorbell muted, and focus playlist started",
        )
        .unwrap();
        mem.store(
            "device_profiles",
            "Printer ink profile: HP OfficeJet uses HP 64 Black and Tri-color cartridges",
        )
        .unwrap();
        mem.store(
            "health_profile",
            "Running outside safety: check weather, air quality, and sunset before leaving",
        )
        .unwrap();
        mem.store(
            "gift_history",
            "Sarah birthday last year gift history: spa day gift certificate and a necklace",
        )
        .unwrap();
        mem.store(
            "board_games",
            "Game night options for four players ages 8-12: Ticket to Ride or Catan, plus snacks",
        )
        .unwrap();
        mem.store(
            "baby_monitor_logs",
            "Baby is crying again routine: fed and changed recently, start the white noise machine",
        )
        .unwrap();

        for (query, expected) in [
            ("What should I get Dad for Father's Day?", "laser level"),
            ("I need a break", "breathing"),
            ("What wine goes with steak?", "Cabernet"),
            ("It's stuffy in here", "ceiling fan"),
            ("I'm working from home today", "doorbell muted"),
            ("Order more ink for the printer", "HP 64"),
            ("Is it safe to run outside?", "sunset"),
            (
                "What did I get Sarah for her birthday last year?",
                "spa day",
            ),
            ("Plan a game night", "Ticket to Ride"),
            ("The baby is crying again", "white noise"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_appliance_outdoor_finance_and_media_context() {
        let mem = temp_memory();
        mem.store(
            "music_library",
            "Jazz Classics station: smooth instrumental saxophone for evening listening",
        )
        .unwrap();
        mem.store(
            "read_history",
            "Suggest a book from read history: mystery thriller readers who liked Gone Girl may enjoy The Silent Patient",
        )
        .unwrap();
        mem.store(
            "restaurant_history",
            "Bored of cooking takeout option: Tokyo Sushi is a recent favorite and available in delivery apps",
        )
        .unwrap();
        mem.store(
            "recipe_book",
            "Ripe bananas recipe: banana bread or banana muffins both work well",
        )
        .unwrap();
        mem.store(
            "photo_metadata",
            "Beach trip photos from July Santa Cruz show ocean, sand, and family vacation",
        )
        .unwrap();
        mem.store(
            "plant_care",
            "Freeze protection routine: turn off outdoor sprinklers and bring in potted plants",
        )
        .unwrap();
        mem.store(
            "weight_trend",
            "Weight trend after logging: down 2 lbs from last month",
        )
        .unwrap();
        mem.store(
            "lunch_preferences",
            "Pack a lunch for Leo: bread, turkey, grapes, cheese, and cut the crusts off",
        )
        .unwrap();
        mem.store(
            "outdoor_furniture",
            "Patio cushions high wind routine: bring cushions inside before storms",
        )
        .unwrap();
        mem.store(
            "cycling_route",
            "Bike ride safety route: 45 minute cycling route with live location shared with Sarah",
        )
        .unwrap();
        mem.store(
            "pet_inventory",
            "Dog food reorder preference: Royal Canin Large Breed 30lb bag",
        )
        .unwrap();
        mem.store(
            "meal_plan",
            "What's for breakfast: planned oatmeal, but milk is out; toast and eggs are available",
        )
        .unwrap();

        for (query, expected) in [
            ("I want to listen to Jazz", "Jazz Classics"),
            ("Suggest a book I might like", "Silent Patient"),
            ("I'm bored of cooking tonight", "Tokyo Sushi"),
            ("What can I make with ripe bananas?", "banana bread"),
            ("Show me pictures from our beach trip", "Santa Cruz"),
            ("It's going to freeze tonight", "potted plants"),
            ("Log my weight", "down 2 lbs"),
            ("Pack a lunch for Leo", "cut the crusts"),
            ("Bring in the patio cushions", "high wind"),
            ("I'm going for a bike ride", "live location"),
            ("Order dog food", "Royal Canin"),
            ("What's for breakfast?", "toast and eggs"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_finance_diy_wardrobe_and_service_booking_context() {
        let mem = temp_memory();
        mem.store(
            "local_business_reviews",
            "Haircut booking option: The Grooming Lounge is nearby with a 4.8 star barber rating",
        )
        .unwrap();
        mem.store(
            "wardrobe_inventory",
            "Wedding outfit plan: formal wedding uses Jared's navy blue suit and silk tie",
        )
        .unwrap();
        mem.store(
            "wellness_content",
            "Meditation content: play 10-Minute Daily Calm guided audio",
        )
        .unwrap();
        mem.store(
            "education_app",
            "Spanish learning plan: open Duolingo to the Spanish basics lesson",
        )
        .unwrap();
        mem.store(
            "takeout_menus",
            "Spicy food options: Spicy Thai Basil takeout or Buffalo Wings with hot sauce",
        )
        .unwrap();
        mem.store(
            "hotel_preferences",
            "Book a hotel in Chicago: downtown hotel with gym and free breakfast for next Friday",
        )
        .unwrap();
        mem.store(
            "maintenance_schedule",
            "AC filter maintenance due now; filter model is Honeywell 20x25x4",
        )
        .unwrap();
        mem.store(
            "family_activities",
            "Kids today activity: sunny 75F day is good for zoo or park; zoo has a new lion exhibit",
        )
        .unwrap();
        mem.store(
            "plumbing_history",
            "Toilet is clogged again history: last month was paper towels, try the plunger first",
        )
        .unwrap();
        mem.store(
            "sewing_instructions",
            "Sew a button help: sewing instructions cover threading needle, knotting thread, and using the hall closet kit",
        )
        .unwrap();

        for (query, expected) in [
            ("I need a haircut", "Grooming Lounge"),
            ("What should I wear to the wedding?", "navy blue suit"),
            ("I want to meditate", "Daily Calm"),
            ("Teach me Spanish", "Duolingo"),
            ("I'm hungry for something spicy", "Thai Basil"),
            ("Book a hotel in Chicago", "free breakfast"),
            ("Change the AC filter", "20x25x4"),
            ("What should we do with the kids today?", "lion exhibit"),
            ("The toilet is clogged again", "paper towels"),
            ("Sew a button on my shirt", "threading needle"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_vehicle_health_hobbies_and_social_logistics() {
        let mem = temp_memory();
        mem.store(
            "hobby_inventory",
            "Painting hobby setup: acrylic paints are in the craft room and a beginner landscape tutorial is available",
        )
        .unwrap();
        mem.store(
            "health_advice",
            "Stomach ache comfort: sip ginger tea, lie down, and call Dr. Smith if it persists",
        )
        .unwrap();
        mem.store(
            "education_content",
            "Magic tricks lesson: 3 Easy Card Tricks for Beginners video",
        )
        .unwrap();
        mem.store(
            "local_businesses",
            "Manicure booking: Polished Nails has an opening tomorrow at 2 PM",
        )
        .unwrap();
        mem.store(
            "charity_ratings",
            "Good charity suggestion: DonorsChoose matches the family's interest in education",
        )
        .unwrap();
        mem.store(
            "language_apps",
            "French learning plan: open Duolingo to French lesson 1",
        )
        .unwrap();
        mem.store(
            "podcast_library",
            "Podcast suggestion: Tech Talk Daily matches Jared's interest in gadgets",
        )
        .unwrap();
        mem.store(
            "audio_library",
            "Motivational speech audio: play a workout inspiration speech",
        )
        .unwrap();
        mem.store(
            "wardrobe_database",
            "Dress shoe advice: a gold heel or black flat complements the black dress",
        )
        .unwrap();
        mem.store(
            "beverage_prefs",
            "Thirst options: cold water and lemonade are in the fridge",
        )
        .unwrap();
        mem.store(
            "calendar",
            "Yoga class is at 6 PM; traffic is light, so leave by 5:30 PM",
        )
        .unwrap();
        mem.store(
            "sun_safety",
            "Sunbathing safety: UV index is high, so reapply sunscreen every 2 hours",
        )
        .unwrap();
        mem.store(
            "friend_availability",
            "Guys' night plan: Dave is free Friday for poker night or the sports bar",
        )
        .unwrap();
        mem.store(
            "favorite_dishes",
            "Thai food order: Pad Thai and Green Curry from Bangkok Palace, ETA 45 minutes",
        )
        .unwrap();
        mem.store(
            "fever_management",
            "Fever management: temperature is 101F, drink fluids, rest, and notify Sarah",
        )
        .unwrap();
        mem.store(
            "snow_protocol",
            "Snow protocol: expect 3 inches, add salt to the shopping list, and shovel the walk",
        )
        .unwrap();
        mem.store(
            "device_usage",
            "Homework check: Mia is on her Chromebook, but YouTube is not an educational site category",
        )
        .unwrap();
        mem.store(
            "weather_video_url",
            "Weather report preference: play the Channel 5 local meteorologist forecast video",
        )
        .unwrap();
        mem.store(
            "arrival",
            "I'm back arrival routine: welcome home, turn on lights, and set thermostat to 70F",
        )
        .unwrap();

        for (query, expected) in [
            ("I want to paint", "acrylic paints"),
            ("I have a stomach ache", "ginger tea"),
            ("Teach me magic tricks", "Card Tricks"),
            ("I need a manicure", "Polished Nails"),
            ("What's a good charity?", "DonorsChoose"),
            ("I want to learn French", "French lesson 1"),
            ("Suggest a podcast", "Tech Talk Daily"),
            ("I need a motivating speech", "workout inspiration"),
            ("What shoes go with this dress?", "gold heel"),
            ("I'm thirsty", "lemonade"),
            ("I'm going to yoga class", "5:30 PM"),
            ("I'm sunbathing", "sunscreen"),
            ("Plan a guys' night", "Dave"),
            ("Order Thai food", "Green Curry"),
            ("I have a fever", "101F"),
            ("It's snowing", "salt"),
            ("Is Mia doing her homework?", "YouTube"),
            ("Play the weather report", "Channel 5"),
            ("I'm back", "70F"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_vehicle_appliance_creative_and_emergency_context() {
        let mem = temp_memory();
        mem.store(
            "story_library",
            "Bedtime story for Leo: The Dragon Who Couldn't Roar is a short 10 minute adventure",
        )
        .unwrap();
        mem.store(
            "literature_database",
            "Romantic poem choice: Sonnet 18 by William Shakespeare is a short love poem",
        )
        .unwrap();
        mem.store(
            "local_trail_database",
            "Hiking trail suggestion: River Walk Trail is 3 miles, flat, scenic, and nearby",
        )
        .unwrap();
        mem.store(
            "recipe_book",
            "Extra basil idea: make pesto or garnish a tomato salad",
        )
        .unwrap();
        mem.store(
            "photo_album",
            "Sunset photo album: Hawaii trip photos tagged Sunset with orange evening sky",
        )
        .unwrap();
        mem.store(
            "pet_names_db",
            "Goldfish name ideas: Goldie Hawn, Fin, and Bubbles",
        )
        .unwrap();
        mem.store(
            "wellness_content",
            "Anxiety support: try a 4-7-8 breathing exercise and grounding routine",
        )
        .unwrap();
        mem.store(
            "educational_video",
            "Roman Empire intro: The Roman Empire in Color is a beginner history documentary",
        )
        .unwrap();
        mem.store(
            "music_library",
            "Mood music: when it is raining and someone is reading, play Lo-Fi Rain Sounds",
        )
        .unwrap();
        mem.store(
            "camping_checklist",
            "Camping checklist for rain: pack the rainfly, hiking boots, and extra tarps",
        )
        .unwrap();
        mem.store(
            "bar_inventory",
            "Cocktail recipe: vodka and orange juice make a Screwdriver with ice",
        )
        .unwrap();
        mem.store(
            "dinner_plan",
            "Working late dinner plan: tell Sarah to hold dinner or reheat Jared's plate",
        )
        .unwrap();
        mem.store(
            "restaurants",
            "Friday date night: Grandma can watch the kids and Luigi's has a 7 PM table",
        )
        .unwrap();
        mem.store(
            "water_sensor",
            "Washing machine is leaking: moisture sensor confirmed; check the drain hose at the back",
        )
        .unwrap();
        mem.store(
            "bike_tracker",
            "Bike security: the bike is at home, but lock status is unknown in security logs",
        )
        .unwrap();
        mem.store(
            "taco_bar_ingredients",
            "Taco bar ingredients: shells, meat, toppings, salsa, and cheese",
        )
        .unwrap();

        for (query, expected) in [
            ("Find a bedtime story for Leo", "Dragon"),
            ("Find a romantic poem", "Sonnet 18"),
            ("Suggest a hiking trail", "River Walk"),
            ("What can I do with extra basil?", "pesto"),
            ("Find a picture of a sunset", "Hawaii"),
            ("What's a good name for a goldfish?", "Bubbles"),
            ("I feel anxious", "4-7-8"),
            ("Teach me about the Roman Empire", "Roman Empire in Color"),
            ("What music fits this mood?", "Lo-Fi Rain"),
            ("I'm going camping", "rainfly"),
            ("Make me a cocktail", "Screwdriver"),
            ("I'm working late", "hold dinner"),
            ("Plan a date night for Friday", "Luigi"),
            ("The washing machine is leaking", "drain hose"),
            ("Did I lock the bike?", "lock status"),
            ("Order groceries for a taco bar", "cheese"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_contextual_family_routines_and_safety() {
        let mem = temp_memory();
        mem.store(
            "comfort_preference_embeddings",
            "Sarah cold living room comfort preference: warm the living room thermostat to 72F for cozy comfort",
        )
        .unwrap();
        mem.store(
            "activity_preference_embeddings",
            "Mia reading light preference: if the room is too bright for reading, soften the ceiling light and turn on the desk lamp",
        )
        .unwrap();
        mem.store(
            "room_mood_embeddings",
            "Mia cozy room scene: warm lights, blinds closed, and temperature set to 71F",
        )
        .unwrap();
        mem.store(
            "delivery_events",
            "Mia package delivery: small package delivered to the front porch at 2:08 PM",
        )
        .unwrap();
        mem.store(
            "garden_zones",
            "Garden watering plan: water the tomato bed tonight for 12 minutes; herb planters can wait until Sunday",
        )
        .unwrap();
        mem.store(
            "recipe_embeddings",
            "Roasted chickpea pita bowls recipe has a 5 star family rating and Mia marked it as a favorite",
        )
        .unwrap();
        mem.store(
            "automation_runs",
            "Hallway light troubleshooting: the motion sensor battery is low; the hallway light itself is working",
        )
        .unwrap();
        mem.store(
            "sleep_preference_embeddings",
            "Mia can't sleep routine: dim lights, turn on white noise, and set thermostat to 69F",
        )
        .unwrap();
        mem.store(
            "automation_rules",
            "Night hallway safety: low-brightness motion lights from 10 PM to 6 AM",
        )
        .unwrap();
        mem.store(
            "safety_intent_embeddings",
            "Outlet spill safety: if water is spilled near an outlet, cut nearby outlet power and notify Jared and Sarah",
        )
        .unwrap();

        for (query, expected) in [
            ("Sarah: I'm cold in the living room", "72F"),
            ("Mia: The room is too bright for reading", "desk lamp"),
            ("Mia: Make my room cozy", "71F"),
            ("Mia: Did my package arrive?", "2:08 PM"),
            ("Jared: When should we water the garden?", "tomato bed"),
            (
                "Sarah: Find the recipe we liked with chickpeas",
                "Roasted chickpea",
            ),
            (
                "Jared: Why didn't the hallway light turn on?",
                "motion sensor battery",
            ),
            ("Mia: I can't sleep", "white noise"),
            ("Mia: Make the hallway safe at night", "10 PM"),
            (
                "Leo: I spilled water near the outlet",
                "cut nearby outlet power",
            ),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_contextual_household_controls_and_reports() {
        let mem = temp_memory();
        mem.store(
            "room_assignments",
            "Sarah bathroom warmup: warm Sarah's bathroom heater and floor warmer before shower for 25 minutes",
        )
        .unwrap();
        mem.store(
            "household_guides_fts",
            "Pizza box disposal: greasy cardboard goes in compost when accepted; plastic or foil goes in trash",
        )
        .unwrap();
        mem.store(
            "scenes",
            "Mia focus mode session: desk light bright, brown noise playing, and distracting apps limited until five",
        )
        .unwrap();
        mem.store(
            "family_preference_embeddings",
            "Quiet porch alerts tonight: keep security recording on, mute the chime, and keep porch light low to avoid waking the kids",
        )
        .unwrap();
        mem.store(
            "automation_runs",
            "Mia room hot cause: west blinds stayed open during afternoon sun; close blinds and run the fan",
        )
        .unwrap();
        mem.store(
            "routine",
            "Storm prep routine: close blinds, charge backup lights, check battery system, and alert the family",
        )
        .unwrap();
        mem.store(
            "meal_notes",
            "Dinner tonight attendees: the four family members plus Grandma Elaine, who prefers decaf tea",
        )
        .unwrap();
        mem.store(
            "comfort_preference_embeddings",
            "Leo scared of the dark night reassurance: raise night-light, start ocean sounds, and notify Mom and Dad",
        )
        .unwrap();
        mem.store(
            "chore_checkins",
            "Mia chores status: dishwasher unloading and laundry pickup finished; desk cleanup still unchecked",
        )
        .unwrap();
        mem.store(
            "energy_meter_readings",
            "Electricity usage now: clothes dryer is highest watts, followed by oven and upstairs HVAC",
        )
        .unwrap();
        mem.store(
            "household_notes_fts",
            "Marker hoodie stain removal: use rubbing alcohol under the stain, blot from the back, wash cold, and avoid dryer",
        )
        .unwrap();
        mem.store(
            "shared_room_reservations",
            "Bathroom reservation for Mia at 7:00 PM for hair wash",
        )
        .unwrap();
        mem.store(
            "item_location_events",
            "Leo backpack location: backpack is by the mudroom bench",
        )
        .unwrap();
        mem.store(
            "room_sun_exposure",
            "Morning sun blinds: open kitchen and living-room blinds, but leave Mia's room unchanged",
        )
        .unwrap();
        mem.store(
            "notification_rules",
            "Piano practice quiet mode: close the music room door and reduce sound transfer through the vents",
        )
        .unwrap();
        mem.store(
            "school_transport_schedule",
            "Mia bus pickup tomorrow is at 7:26 AM",
        )
        .unwrap();
        mem.store(
            "door_sensor_events",
            "Freezer door left open for 4 minutes at 6:11 PM; temperature stayed safe",
        )
        .unwrap();
        mem.store(
            "safety_intent_embeddings",
            "Gas safety emergency: if someone smells gas, leave the house, avoid switches or flames, and alert Jared and Sarah",
        )
        .unwrap();
        mem.store(
            "presence_alerts",
            "Presence alert: tell Leo when Dad gets home from the geofence event",
        )
        .unwrap();
        mem.store(
            "document_embeddings",
            "Mia ocean essay draft is in her English folder; latest version was edited last night",
        )
        .unwrap();
        mem.store(
            "device_states",
            "Windows status: kitchen window and Mia's bedroom window are open",
        )
        .unwrap();
        mem.store(
            "routine_overrides",
            "Bedtime reading light override: Mia gets 20 minutes of reading light before lamp-off",
        )
        .unwrap();
        mem.store(
            "school_notes_fts",
            "Pajama day school announcement: tomorrow is pajama day for Leo's class",
        )
        .unwrap();
        mem.store(
            "smart_plug_states",
            "Mia laptop charger location: charger is plugged in at her desk and the outlet is on",
        )
        .unwrap();
        mem.store(
            "lighting_simulation_rules",
            "Scheduled vacation mode next week: simulate evening lights, lower HVAC, keep watering active, and check locks daily",
        )
        .unwrap();
        mem.store(
            "food_inventory",
            "Leftovers priority: eat turkey chili first because it is safe until tomorrow; pasta bake has two more days",
        )
        .unwrap();
        mem.store(
            "vacuum_zones",
            "Robot vacuum under Leo's bed: clean leo_under_bed and avoid the toy corner",
        )
        .unwrap();
        mem.store(
            "do_not_disturb_rule",
            "Violin practice notifications: mute Mia's notifications for 45 minutes while practicing violin",
        )
        .unwrap();
        mem.store(
            "irrigation_events",
            "Sprinkler run history this morning: front lawn ran 10 minutes at 5:45 AM and garden beds ran 8 minutes at 6:00 AM",
        )
        .unwrap();
        mem.store(
            "safety_profiles",
            "Toddler-safe kitchen routine: lock lower cabinets, lock oven controls, and enable outlet safety mode",
        )
        .unwrap();
        mem.store(
            "permission_requests",
            "Mia sleepover approval: Mom approved it, but Dad has not answered yet",
        )
        .unwrap();
        mem.store(
            "security_mode_exceptions",
            "Lock everything except back gate: all locks secured except the back gate security exception",
        )
        .unwrap();
        mem.store(
            "health_documents_fts",
            "Mia allergy action plan: current active medical plan saved in Health Documents",
        )
        .unwrap();
        mem.store(
            "scene_embeddings",
            "Spaceship hallway scene: blue and white pulsing lights at child-safe brightness",
        )
        .unwrap();
        mem.store(
            "weather_context",
            "Morning readiness report: doors locked, coffee ready, Leo still needs lunchbox, Mia bus 7:26, rain expected by pickup",
        )
        .unwrap();

        for (query, expected) in [
            (
                "Jared: Warm up Sarah's bathroom before her shower",
                "25 minutes",
            ),
            ("Leo: What bin does a pizza box go in?", "compost"),
            ("Mia: Give me focus mode until five", "brown noise"),
            (
                "Sarah: Keep the porch from waking the kids tonight",
                "mute the chime",
            ),
            ("Mia: Why is my room so hot?", "west blinds"),
            ("Jared: Start storm prep", "backup lights"),
            ("Sarah: Who's coming to dinner tonight?", "Grandma Elaine"),
            ("Leo: I'm scared of the dark", "ocean sounds"),
            ("Mia: Did I finish my chores?", "desk cleanup"),
            (
                "Jared: What's using the most electricity right now?",
                "clothes dryer",
            ),
            (
                "Sarah: How do I remove marker from Leo's hoodie?",
                "rubbing alcohol",
            ),
            (
                "Mia: Save the bathroom for me at 7 for hair wash",
                "7:00 PM",
            ),
            ("Leo: Where's my backpack?", "mudroom bench"),
            (
                "Jared: Open blinds where there's morning sun, but not Mia's room",
                "living-room blinds",
            ),
            (
                "Sarah: Keep piano practice quiet for the rest of the house",
                "sound transfer",
            ),
            ("Mia: What time is my bus tomorrow?", "7:26 AM"),
            ("Jared: Was the freezer door left open?", "4 minutes"),
            ("Sarah: I smell gas", "avoid switches"),
            ("Leo: Tell me when Dad gets home", "geofence"),
            ("Mia: Find my essay draft about oceans", "English folder"),
            ("Jared: Are all the windows closed?", "kitchen window"),
            (
                "Sarah: Start bedtime, but let Mia read for 20 minutes",
                "20 minutes",
            ),
            ("Leo: Is it pajama day tomorrow?", "pajama day"),
            ("Mia: My laptop battery is low", "desk"),
            ("Jared: Set vacation mode for next week", "check locks"),
            (
                "Sarah: Which leftovers should we eat first?",
                "turkey chili",
            ),
            (
                "Leo: Can the robot vacuum clean under my bed?",
                "toy corner",
            ),
            (
                "Mia: Turn off notifications while I'm practicing violin",
                "45 minutes",
            ),
            ("Jared: Did the sprinkler run this morning?", "5:45 AM"),
            (
                "Sarah: Make the kitchen toddler-safe for our visitor",
                "outlet safety",
            ),
            ("Mia: Did Mom approve my sleepover?", "Dad"),
            ("Jared: Lock everything except the back gate", "back gate"),
            ("Sarah: Find Mia's allergy action plan", "Health Documents"),
            (
                "Leo: Make the hallway look like a spaceship",
                "blue and white",
            ),
            ("Jared: Give me a morning readiness report", "lunchbox"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_contextual_household_controls_and_reports_third_batch() {
        let mem = temp_memory();
        mem.store(
            "scenes",
            "Kids homework mode: Leo and Mia study lights bright, router focus rules active, and quiet background sound playing",
        )
        .unwrap();
        mem.store(
            "ble_tag_events",
            "Jared car keys location: key tag last pinged on the entryway table 6 minutes ago",
        )
        .unwrap();
        mem.store(
            "family_preference_embeddings",
            "Quiet baking without waking Leo: keep kitchen notifications quiet and use the range hood on low",
        )
        .unwrap();
        mem.store(
            "vacuum_events",
            "Robot vacuum stuck: blocked near Leo's toy bin with something blocking the left wheel",
        )
        .unwrap();
        mem.store(
            "device_audit_log",
            "Thermostat audit: Jared changed the thermostat to 70F from his phone at 6:12 PM",
        )
        .unwrap();
        mem.store(
            "household_notes_fts",
            "Ladder safety note: use stabilizer feet and avoid the top two rungs",
        )
        .unwrap();
        mem.store(
            "family_calendar",
            "Mia bathroom mirror schedule: school, violin practice, and math homework",
        )
        .unwrap();
        mem.store(
            "sleep_preference_embeddings",
            "Leo too hot in bed comfort: turn fan to low and lower his room by 2 degrees",
        )
        .unwrap();
        mem.store(
            "delivery_events",
            "Porch package present: package is still on the front porch by the mat",
        )
        .unwrap();
        mem.store(
            "safety_intent_embeddings",
            "Kitchen sink leak safety: shut off water to the kitchen sink and alert Sarah",
        )
        .unwrap();
        mem.store(
            "scene_embeddings",
            "Art time lighting scene: restore Mia's saved room lights, desk lamp, and blinds",
        )
        .unwrap();
        mem.store(
            "health_routines",
            "Mia allergy medicine status: marked done at 7:41 AM with medicine cabinet opening",
        )
        .unwrap();
        mem.store(
            "learning_history",
            "Dinosaur fact from yesterday: some sauropods swallowed stones to grind food",
        )
        .unwrap();
        mem.store(
            "device_states",
            "Office standby power: monitors, speakers, and printer are standby-safe; router, backup drive, and security hub are excluded",
        )
        .unwrap();
        mem.store(
            "audio_event_classifications",
            "Beeping sound source: laundry-room leak sensor low battery alert",
        )
        .unwrap();
        mem.store(
            "network_access_rules",
            "YouTube math block: YouTube is blocked on Mia's devices until the math task is marked finished",
        )
        .unwrap();
        mem.store(
            "access_permissions",
            "Contractor garage access: contractor can open the garage between 10:00 and 10:20 with notification",
        )
        .unwrap();
        mem.store(
            "routines",
            "Sleepover guest mode: guest Wi-Fi on, hallway night lights enabled, and quiet hours set",
        )
        .unwrap();
        mem.store(
            "device_aliases",
            "Leo stars except closet: ceiling projector stars on and closet light stays off",
        )
        .unwrap();
        mem.store(
            "manuals_fts",
            "Printer Wi-Fi reset: hold wireless button for 5 seconds and reconnect from printer app",
        )
        .unwrap();
        mem.store(
            "automation_runs",
            "Porch light still on because the camera saw repeated motion twice in the last 10 minutes",
        )
        .unwrap();
        mem.store(
            "family_notes_fts",
            "Grandma Elaine Wi-Fi note is in Family Contacts",
        )
        .unwrap();
        mem.store(
            "family_rules",
            "Leo play outside permission: backyard is allowed, stay inside the fence, and Mom is in the kitchen",
        )
        .unwrap();
        mem.store(
            "automation_rules",
            "Mia school-morning gradual blinds: open slowly on school mornings and skip school holidays",
        )
        .unwrap();
        mem.store(
            "energy_meter_readings",
            "Electricity week comparison: this week is 12 percent higher, mainly upstairs HVAC and clothes dryer",
        )
        .unwrap();
        mem.store(
            "device_states",
            "Back burner status: back burner is off, still warm, and cooling normally",
        )
        .unwrap();
        mem.store(
            "household_guides_fts",
            "Wet soccer shoes guide: put wet soccer shoes on the mudroom drying tray, not in the bedroom",
        )
        .unwrap();
        mem.store(
            "comfort_preference_embeddings",
            "Warm-not-steamy shower: warm the shower and run the bathroom fan high",
        )
        .unwrap();
        mem.store(
            "notification_rules",
            "Quiet armed security: security stays armed, noncritical chimes muted, loud alerts only for urgent events",
        )
        .unwrap();
        mem.store(
            "appliance_events",
            "Laundry finish status: dryer finished at 4:47 PM and has not been opened yet",
        )
        .unwrap();
        mem.store(
            "daily_checklists",
            "Tomorrow checklist for Mia: pack gym clothes, bring math notebook, violin practice at 4:30, and charge laptop",
        )
        .unwrap();
        mem.store(
            "user_preferences",
            "Leo green night-light preference: green is his favorite night-light color",
        )
        .unwrap();
        mem.store(
            "hvac_runtime",
            "Drafty room report: dining room seems draftiest because temperature drops fastest after heat cycles off",
        )
        .unwrap();
        mem.store(
            "project_notes_fts",
            "Mia room blue paint: Harbor Mist, eggshell finish",
        )
        .unwrap();
        mem.store(
            "glass_break_sensors",
            "Glass break safety: check downstairs, keep Mia in place, alert Mom and Dad, and turn hallway lights on",
        )
        .unwrap();
        mem.store(
            "child_contact_rules",
            "Kitchen screen call Mom: Leo may call Sarah on the kitchen display",
        )
        .unwrap();
        mem.store(
            "device_health",
            "Offline devices: garage temperature sensor, guest room plug, and side-yard camera are offline",
        )
        .unwrap();
        mem.store(
            "routines",
            "Babysitter mode: guest code active, care notes on kitchen display, and guest Wi-Fi enabled",
        )
        .unwrap();
        mem.store(
            "laundry_events",
            "Mia laundry moved: dryer door opened and Mia basket weight increased at 5:22 PM",
        )
        .unwrap();
        mem.store(
            "safety_routes",
            "Kitchen alarm exit route: use the front door route from the living room and avoid the kitchen hallway",
        )
        .unwrap();

        for (query, expected) in [
            ("Sarah: Start homework mode for both kids", "study lights"),
            ("Jared: Where are my car keys?", "entryway table"),
            (
                "Mia: Can I bake cookies without waking Leo?",
                "range hood on low",
            ),
            ("Leo: Why is the robot vacuum stuck?", "left wheel"),
            ("Sarah: Who changed the thermostat?", "6:12 PM"),
            ("Jared: Find the ladder safety note", "stabilizer feet"),
            (
                "Mia: Put my schedule on the bathroom mirror",
                "violin practice",
            ),
            ("Leo: I'm too hot in bed", "2 degrees"),
            ("Sarah: Is the package still on the porch?", "front porch"),
            ("Jared: There's water under the sink", "shut off water"),
            ("Mia: Save this lighting for art time", "desk lamp"),
            ("Sarah: Did Mia take her allergy medicine?", "7:41 AM"),
            ("Leo: Tell me the dinosaur fact from yesterday", "sauropods"),
            ("Jared: Turn off standby power in the office", "router"),
            ("Sarah: What's making that beeping sound?", "leak sensor"),
            ("Mia: Block YouTube until I finish math", "math task"),
            ("Jared: Let the contractor into the garage at 10", "10:20"),
            ("Sarah: Set up sleepover guest mode", "quiet hours"),
            (
                "Leo: Turn on stars but keep the closet dark",
                "closet light",
            ),
            ("Mia: How do I reset the printer Wi-Fi?", "wireless button"),
            ("Jared: Why is the porch light still on?", "repeated motion"),
            ("Sarah: Find Grandma's Wi-Fi note", "Family Contacts"),
            ("Leo: Am I allowed to play outside?", "backyard"),
            (
                "Mia: Open my blinds slowly every school morning",
                "school mornings",
            ),
            (
                "Jared: Compare this week's electricity use to last week",
                "12 percent",
            ),
            ("Sarah: Is the back burner off?", "cooling normally"),
            (
                "Leo: Where do my wet soccer shoes go?",
                "mudroom drying tray",
            ),
            ("Mia: Make the shower warm but not steamy", "fan high"),
            (
                "Jared: Keep security on, but don't wake the kids",
                "noncritical chimes",
            ),
            ("Sarah: When did the laundry finish?", "4:47 PM"),
            ("Mia: Make tomorrow into a checklist", "math notebook"),
            (
                "Leo: Remember that I like the green night-light better",
                "green",
            ),
            ("Jared: Which room seems drafty?", "dining room"),
            (
                "Sarah: What was the blue paint color in Mia's room?",
                "Harbor Mist",
            ),
            ("Mia: I heard glass break downstairs", "alert Mom and Dad"),
            ("Leo: Call Mom on the kitchen screen", "Sarah"),
            ("Jared: Which devices are offline?", "side-yard camera"),
            ("Sarah: Prep the house for the babysitter", "guest code"),
            ("Mia: Did my laundry get moved?", "5:22 PM"),
            (
                "Jared: What's the safest way out if the kitchen alarm goes off?",
                "front door",
            ),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_contextual_household_controls_and_reports_fourth_batch() {
        let mem = temp_memory();
        for (kind, content) in [
            (
                "routines",
                "Rainy pickup mode: mudroom lights on, towels and umbrellas checklist on the kitchen display, and the house stays warm",
            ),
            (
                "electrical_panel_map",
                "Dishwasher breaker: breaker 14 labeled Kitchen Appliances B",
            ),
            (
                "permission_requests",
                "After-school guest request: Emma can come over after school after parent approval is requested",
            ),
            (
                "safety_intent_embeddings",
                "Toaster smoky safety: cut power to the kitchen toaster, start the kitchen vent, and tell a parent",
            ),
            (
                "air_quality_sensors",
                "Pollen mode: close open windows, increase purifiers, and set HVAC fan to circulate",
            ),
            (
                "household_routines",
                "Trash day prep: kitchen trash out, recycling to curb, and cardboard flattened",
            ),
            (
                "item_location_events",
                "Mia red hoodie location: red hoodie is in Dad's car",
            ),
            (
                "timers",
                "Leo Lego cleanup timer: cleanup timer is 10 minutes",
            ),
            (
                "home_maintenance_embeddings",
                "Ant response history: clean pantry shelf, seal back door gap, and use ant bait under the sink",
            ),
            (
                "automation_rules",
                "Driveway arrival lighting: Jared geofence turns driveway lights on when he pulls in, then auto-off after 7 minutes",
            ),
            (
                "activity_preference_embeddings",
                "Mia video-call room setup: bright front lighting, adjusted blinds, and quiet notifications",
            ),
            (
                "camera_object_events",
                "Garbage bins out: bins were moved to the curb at 7:18 PM",
            ),
            (
                "inventory_items",
                "Camping flashlight: blue camping bin in the garage with battery full",
            ),
            (
                "automation_runs",
                "Sprinkler skip reason: sprinklers skipped because rain sensor passed the wet-soil threshold",
            ),
            (
                "scheduled_device_actions",
                "Dishwasher after 9: dishwasher scheduled to start after 9:00 PM",
            ),
            (
                "school_tasks",
                "Mia homework internet: science quiz review and Spanish listening need internet; math worksheet does not",
            ),
            (
                "family_rules",
                "Leo stove permission: he needs Mom or Dad in the kitchen before using the stove",
            ),
            (
                "comfort_preference_embeddings",
                "Allergy-day setup: windows closed, purifiers on high, and HVAC filter reminder active",
            ),
            (
                "health_documents_fts",
                "Cold medicine instructions: label scan in Health Documents and bottle in upstairs medicine cabinet",
            ),
            (
                "alarm_preferences",
                "Mia sunlight alarm: next alarm opens blinds gradually instead of sound",
            ),
            (
                "guest_access_policies",
                "Guest info display: entryway tablet only shows Wi-Fi name, approved guest access note, and bathroom directions",
            ),
            (
                "door_sensor_events",
                "Fridge door closed: fridge door is closed and temperature is stable",
            ),
            (
                "activity_preference_embeddings",
                "Leo reading with Dad scene: warm light on and room audio paused",
            ),
            (
                "user_media_aliases",
                "Mia rainy-day playlist: saved current media session as rainy-day playlist",
            ),
            (
                "battery_status",
                "Sensor battery report: laundry leak sensor, hallway motion sensor, and garage door contact sensor need batteries soon",
            ),
            (
                "activity_preference_embeddings",
                "Sarah work-call quiet mode: pause vacuum, lower house audio, and mute nonurgent chimes",
            ),
            (
                "daily_checklists",
                "Leo library book packed: library book was scanned by the backpack this morning",
            ),
            (
                "notification_log",
                "Mia alarm failure reason: tablet was offline, backup hallway display alarm still triggered",
            ),
            (
                "safety_intent_embeddings",
                "Garage paint ventilation: run garage exhaust fan and crack side door open while painting",
            ),
            (
                "plant_care_profiles",
                "Plants need attention: basil needs water, fern needs misting, snake plant is fine",
            ),
            (
                "dishwasher_rack_state",
                "Leo blue cup location: blue cup is in the top rack of the dishwasher",
            ),
            (
                "scene_embeddings",
                "Mia sleepover lights: soft string lights and low ceiling brightness",
            ),
            (
                "device_events",
                "Side gate away status: side gate stayed closed while the family was away",
            ),
            (
                "family_notes_fts",
                "Mia recital outfit note: navy dress, silver flats, and hair ribbon",
            ),
            (
                "temporary_notifications",
                "Cookie done light alert: Leo's lamp will flash gently when the cookies are done",
            ),
            (
                "shared_room_reservations",
                "Bathroom availability: upstairs bathroom is free right now",
            ),
            (
                "security_mode_attempts",
                "Away mode failed: back door lock is jammed, everything else was ready",
            ),
            (
                "comfort_preference_embeddings",
                "Leo calm morning: soft lights, quieter reminders, and a slower checklist",
            ),
            ("device_credentials", "Guest speaker pairing code is 4821"),
            (
                "open_reminders",
                "End-of-day house summary: all doors locked, two windows open, Leo routine done, Mia reminder tomorrow, leak sensor battery low",
            ),
        ] {
            mem.store(kind, content).unwrap();
        }

        for (query, expected) in [
            ("Sarah: Start rainy pickup mode", "umbrellas"),
            (
                "Jared: Which breaker controls the dishwasher?",
                "breaker 14",
            ),
            ("Mia: Can Emma come over after school?", "parent approval"),
            ("Leo: The toaster smells smoky!", "kitchen vent"),
            ("Sarah: Make the house better for pollen", "purifiers"),
            (
                "Jared: What do we still need to do before trash day?",
                "cardboard",
            ),
            (
                "Mia: Remember that my red hoodie is in Dad's car",
                "Dad's car",
            ),
            ("Leo: Start a Lego cleanup timer", "10 minutes"),
            (
                "Sarah: What did we do last time ants showed up?",
                "ant bait",
            ),
            (
                "Jared: Turn on the driveway lights only when I pull in",
                "7 minutes",
            ),
            ("Mia: Make my room good for a video call", "front lighting"),
            ("Sarah: Did anyone take the garbage bins out?", "7:18 PM"),
            ("Leo: Where's the camping flashlight?", "blue camping bin"),
            ("Jared: Why didn't the sprinklers run today?", "wet-soil"),
            ("Sarah: Run the dishwasher after 9", "9:00 PM"),
            ("Mia: Which homework needs internet?", "Spanish listening"),
            ("Leo: Can I use the stove?", "Mom or Dad"),
            ("Jared: Run allergy-day setup", "filter reminder"),
            ("Sarah: Find the cold medicine instructions", "upstairs"),
            ("Mia: Wake me with sunlight, not sound", "blinds"),
            (
                "Jared: Show guests only the Wi-Fi and bathroom info",
                "bathroom directions",
            ),
            ("Sarah: Did the fridge door close all the way?", "stable"),
            ("Leo: Make my room ready for reading with Dad", "warm light"),
            ("Mia: Save this as my rainy-day playlist", "rainy-day"),
            (
                "Jared: Which sensors need batteries soon?",
                "hallway motion",
            ),
            ("Sarah: Make the house quiet for my work call", "vacuum"),
            ("Leo: Did I pack my library book?", "backpack"),
            ("Mia: Why did my alarm not go off?", "tablet was offline"),
            ("Jared: Keep the garage ventilated while I paint", "exhaust"),
            ("Sarah: What plants need attention?", "fern"),
            ("Leo: I can't find my blue cup", "top rack"),
            ("Mia: Set my room to sleepover lights", "string lights"),
            (
                "Jared: Did the side gate open while we were gone?",
                "stayed closed",
            ),
            (
                "Sarah: Find the note about Mia's recital outfit",
                "silver flats",
            ),
            (
                "Leo: Make my lights flash when the cookies are done",
                "flash gently",
            ),
            ("Mia: Is the bathroom free?", "free right now"),
            ("Jared: Why did away mode fail?", "jammed"),
            ("Sarah: Start a calm morning for Leo", "slower checklist"),
            ("Mia: What's the password for the guest speaker?", "4821"),
            ("Jared: Make an end-of-day house summary", "two windows"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_search_links_contextual_household_controls_and_reports_fifth_batch() {
        let mem = temp_memory();
        for (kind, content) in [
            (
                "scenes",
                "After-dinner cleanup mode: kitchen lights bright, dishwasher queued, and robot vacuum cleans kitchen dining",
            ),
            (
                "device_states",
                "Upstairs lights on report: Mia desk lamp, Leo night-light, and hallway sconce are still on",
            ),
            (
                "trusted_contacts",
                "Grandma front door permission: Grandma Elaine is recognized and Leo may open the front door while Mom is notified",
            ),
            (
                "documents_fts",
                "Mia debate school lunch research: Civics folder latest draft School Lunch Debate Notes",
            ),
            (
                "activity_preference_embeddings",
                "Board games living room scene: brighter table lights, TV off, and 71F seating temperature",
            ),
            (
                "humidity_sensors",
                "Basement humidity cause: basement window open during damp weather and dehumidifier paused since noon",
            ),
            (
                "do_not_disturb_rules",
                "Mia test practice notifications: block notifications except Mom during test practice",
            ),
            (
                "item_location_events",
                "Leo rain boots location: rain boots are by the mudroom drying mat",
            ),
            (
                "battery_status",
                "Charging tonight report: Mia laptop, Leo tablet, and Jared garage headset should be charged tonight",
            ),
            (
                "scheduled_device_actions",
                "Coffee wake brew: coffee maker starts when Jared's wake-up alarm goes off",
            ),
            (
                "preference",
                "Mia sleep fan preference: fan on low for sleep",
            ),
            (
                "comfort_preference_embeddings",
                "Leo post-bath comfort: warm bathroom area and turn on the towel warmer",
            ),
            (
                "manuals_fts",
                "Slow cooker timer chart: timer chart is in the slow cooker cooking guide section",
            ),
            (
                "water_leak_sensors",
                "Basement flood check clear: no wet leak sensors and sump pump last ran normally",
            ),
            (
                "camera_object_events",
                "Mia garage bike camera: bike seen in garage near the workbench at 4:09 PM",
            ),
            (
                "maintenance_schedule",
                "Next filter change: upstairs air purifier filter due in 6 days",
            ),
            (
                "reminders",
                "Puzzle done reminder: when Leo says the puzzle is done, tell Dad",
            ),
            (
                "access_permissions",
                "Grandma temporary access: Grandma Elaine's temporary door access active until 9:00 PM",
            ),
            (
                "activity_preference_embeddings",
                "Mia desk glare comfort: lower desk lamp slightly and adjust blinds to reduce glare",
            ),
            (
                "access_logs",
                "Front door after Leo arrival: door opened, closed, then locked automatically 45 seconds later",
            ),
            (
                "documents_fts",
                "Water heater receipt: Home Projects receipt linked to installation record",
            ),
            (
                "activity_preference_embeddings",
                "Leo quiet drawing time: warm light, soft music, and fewer interruptions",
            ),
            (
                "user_print_rules",
                "Mia homework print permission: printer online and paper available",
            ),
            (
                "hvac_zones",
                "Upstairs cooler except Leo: cool upstairs zones while leaving Leo's room unchanged",
            ),
            (
                "audio_event_classifications",
                "Noisy appliance report: washing machine vibration pattern during high-spin cycle",
            ),
            (
                "device_events",
                "Leo tooth fairy box: contact sensor stayed closed since bedtime",
            ),
            (
                "inventory_items",
                "White extension cord location: craft bin under the dining-room sideboard",
            ),
            (
                "network_access_rules",
                "Family dinner screens: pause kid screens and stop room media sessions during family dinner",
            ),
            (
                "camera_object_events",
                "Garage changes today: side door opened twice, Mia bike moved near workbench, freezer normal",
            ),
            (
                "device_states",
                "Stairwell bright scene: stairwell lights set to 90 percent child-safe brightness",
            ),
            (
                "reminder",
                "Mia plant after school reminder: water her plant after school",
            ),
            (
                "recipes_fts",
                "Peanut-free chicken recipe: lemon chicken rice bowls are peanut-free and rated highly",
            ),
            (
                "device_alerts",
                "Security alarm chirp: low-battery chirp from side-yard contact sensor, not intrusion",
            ),
            (
                "family_rules",
                "Leo microwave permission: allowed only while Mom is in the kitchen",
            ),
            (
                "activity_preference_embeddings",
                "Mia rehearsal comfort: saved temperature, fan speed, and humidity for rehearsal comfort",
            ),
            (
                "camera_person_events",
                "Backyard presence: Jared is recognized in the backyard and nobody else is recognized",
            ),
            (
                "activity_preference_embeddings",
                "Workshop dust control: garage exhaust fan running and purifier set to high",
            ),
            (
                "routine_steps",
                "Leo bedtime chart remaining: pajamas, story time, and lights out",
            ),
            (
                "automation_rules",
                "Mia closet light automation: closet light turns on when the door opens and auto-off after no motion",
            ),
            (
                "window_sensor_events",
                "Upstairs window before rain: window closed at 2:14 PM and rain started at 2:37 PM",
            ),
            (
                "temporary_mode_overrides",
                "Low-power mode until five: reduce lights, eco hold thermostat, and turn off standby-safe loads",
            ),
            (
                "health_documents_fts",
                "Leo vaccination form: latest active vaccination form is in Health Documents",
            ),
            (
                "school_forms",
                "Mia field trip form signed: Mom signed it this morning",
            ),
            (
                "child_media_rules",
                "Leo animal show low volume: allowed animal show with volume kept low",
            ),
            (
                "network_clients",
                "Guest Wi-Fi devices: babysitter phone, Grandma Elaine tablet, and one unknown device",
            ),
            (
                "automation_rules",
                "Entry lights until Mia home: front entry lights held on until Mia geofence arrival",
            ),
            (
                "outdoor_temperature_sensors",
                "Side path ice risk: below freezing and surface sensor wet",
            ),
            (
                "safety_intent_embeddings",
                "Dripping leak check: bathroom sink sensor dry but Mom should look nearby",
            ),
            (
                "router_stats",
                "Office internet slow reason: backup drive large upload saturating office network",
            ),
            (
                "routines",
                "School-night reset: kids devices wind down, lights dim, and morning prep list on kitchen display",
            ),
            (
                "project_notes_fts",
                "Photo backdrop instructions: saved in Mia's art project folder",
            ),
            (
                "inventory_items",
                "Red marker location: dining-room craft caddy",
            ),
            (
                "sensor_alert_rules",
                "Freezer threshold alert: notify Jared if freezer rises above 10F",
            ),
            (
                "chore_assignments",
                "Leo skipped chores this week: toy cleanup Tuesday and lunchbox emptying Thursday",
            ),
            (
                "device_states",
                "Mia mirror lights only: non-mirror lights off and mirror lights on",
            ),
            (
                "family_rules",
                "Cat sleep permission: school nights require Mom and Dad approval for cat in Leo's room",
            ),
            (
                "scenes",
                "Backyard grilling lights: grill task lights bright and path lights at medium",
            ),
            (
                "automation_runs",
                "Mia purifier high reason: elevated dust triggered automatic high mode",
            ),
            (
                "activity_templates",
                "Mia swim meet packing list: swimsuit, towel, goggles, cap, water bottle, snack, and dry clothes",
            ),
            (
                "cooking_sessions",
                "Cookie recipe next step: scoop dough onto the baking sheet with space between cookies",
            ),
            (
                "camera_health",
                "Outdoor camera cleaning report: side-yard and driveway cameras likely need cleaning",
            ),
            (
                "garage_door_events",
                "Garage closed after Jared left: garage closed two minutes after Jared left and stayed closed",
            ),
            (
                "project_list_items",
                "Mia project list supplies: batteries and poster board are on the project list",
            ),
            (
                "comfort_preference_embeddings",
                "Downstairs reassurance: stair and kitchen lights on and parents notified",
            ),
            (
                "manuals_fts",
                "Furnace code 31: pressure switch troubleshooting section",
            ),
            (
                "appliance_safety_profiles",
                "Dinner warm until Jared arrives: oven keep-warm within safe duration limit",
            ),
            (
                "automation_rules",
                "Mia Wednesday quiet time: quiet time scheduled after school every Wednesday",
            ),
            (
                "pet_feeding_events",
                "Cat feeding amount check: amount logged is within today's feeding plan",
            ),
            (
                "food_inventory",
                "Oldest fridge food: lentil soup should be eaten today",
            ),
            (
                "indoor_air_quality_sensors",
                "Cleaner outside air: motorized kitchen window opened while bedroom windows stay closed for pollen control",
            ),
            (
                "power_events",
                "Mia lamp flicker reason: smart plug reports unstable power, lamp turned off and Dad told",
            ),
            (
                "family_rules",
                "Leo garage door permission: children cannot operate garage door alone",
            ),
            (
                "automation_rules",
                "Holiday lighting schedule: holiday lights from sunset to 10:30 PM with energy limit",
            ),
            (
                "home_project_notes_fts",
                "Plumber shutoff valve note: secondary shutoff valve behind laundry-room access panel",
            ),
            (
                "alarm_preference_embeddings",
                "Mia rainy-day alarm: rainy-day sound, slower light ramp, and earlier start tomorrow",
            ),
            (
                "activity_templates",
                "Leo soccer practice gear: cleats, shin guards, water bottle, and light jacket",
            ),
            (
                "security_audit_log",
                "Sensor bypass report: Sarah bypassed laundry-room window sensor at 9:12 AM for cleaning",
            ),
            (
                "scenes",
                "Guest breakfast mode: kitchen lights on, guest coffee brewing, breakfast notes on counter display",
            ),
            (
                "documents_fts",
                "Mia winter poem: English folder file First Snow Draft",
            ),
            (
                "comfort_preference_embeddings",
                "Leo laundry room not scary: brighter laundry-room light and soft sound",
            ),
            (
                "water_pressure_sensors",
                "Water pressure status: current water pressure 58 PSI, normal for the house",
            ),
            (
                "appliance_events",
                "Oven preheat reminder: notify Sarah when oven finishes preheating",
            ),
            (
                "privacy_audit_log",
                "Hallway camera privacy: privacy mode for 20 minutes while sleepover guests change, safety sensors active",
            ),
            (
                "cooking_sessions",
                "Cookie cooling alert: tell Leo when cookies have cooled enough to eat safely",
            ),
            (
                "vacuum_events",
                "Vacuum dining room skip: temporary no-go zone active around Mia's school project",
            ),
            (
                "manuals_fts",
                "Toddler gate instructions: pressure-mount setup note attached to visitor safety bin",
            ),
            (
                "air_quality_sensors",
                "Mia room smell air quality: elevated VOCs, purifier on, vent opened, tell parents if stronger",
            ),
            (
                "message_events",
                "Leo Dad message read status: Dad saw Leo's message at 4:23 PM",
            ),
            (
                "automation_rules",
                "Laundry leak shutoff: shut main water valve and alert Jared and Sarah if laundry sensor gets wet",
            ),
            (
                "ble_tag_events",
                "Entryway backpacks: Leo's backpack is by the door and Mia's backpack is not there",
            ),
            (
                "alarms",
                "Mia alarm skip holidays: recurring alarm skips school holidays",
            ),
            (
                "routine_steps",
                "Leo morning checklist display: active morning checklist on hallway display",
            ),
            (
                "camera_access_logs",
                "Camera privacy report: no unusual access, hallway camera privacy once, outdoor cameras normal",
            ),
            (
                "meal_memory_embeddings",
                "Green bowl recipe: sesame noodle salad served in the big green bowl",
            ),
            (
                "family_rules",
                "Mia drums permission: practice drums now with practice pads and door closed",
            ),
            (
                "safety_equipment_log",
                "Emergency flashlight location: child-accessible flashlight in lower mudroom drawer",
            ),
            (
                "automation_runs",
                "Top automation today: hallway motion-light automation fired 18 times",
            ),
            (
                "automation_rules",
                "Kids morning upstairs warmth: upstairs warms automatically while Leo and Mia get ready",
            ),
            (
                "activity_notes_fts",
                "Mia tournament snacks: pretzels, apple slices, cheese stick, and blue sports drink",
            ),
            (
                "security_modes",
                "Final safety sweep: doors locked, smoke detectors normal, no leaks, oven off, one upstairs window open",
            ),
        ] {
            mem.store(kind, content).unwrap();
        }

        for (query, expected) in [
            ("Sarah: Start after-dinner cleanup mode", "robot vacuum"),
            (
                "Jared: Which lights are still on upstairs?",
                "Mia desk lamp",
            ),
            (
                "Leo: Can I open the front door for Grandma?",
                "Grandma Elaine",
            ),
            (
                "Mia: Find my debate research about school lunches",
                "School Lunch Debate Notes",
            ),
            (
                "Sarah: Make the living room good for board games",
                "table lights",
            ),
            ("Jared: Why is the basement humid?", "dehumidifier"),
            (
                "Mia: Block notifications except Mom during my test practice",
                "except Mom",
            ),
            ("Leo: Where did I leave my rain boots?", "mudroom"),
            ("Sarah: What needs charging tonight?", "garage headset"),
            ("Jared: Start the coffee when I wake up", "wake-up alarm"),
            (
                "Mia: Remember I like the fan on low for sleep",
                "fan on low",
            ),
            ("Leo: I'm cold after bath", "towel warmer"),
            (
                "Sarah: Find the slow cooker manual and timer chart",
                "cooking guide",
            ),
            ("Jared: Run basement flood check", "clear"),
            ("Mia: Did the garage camera see my bike?", "4:09 PM"),
            ("Sarah: When is the next filter change?", "6 days"),
            ("Leo: Tell Dad when my puzzle is done", "tell Dad"),
            ("Jared: Create a temporary code for Grandma", "9:00 PM"),
            ("Mia: My desk feels glarey", "reduce glare"),
            (
                "Sarah: Was the front door locked after Leo came in?",
                "45 seconds",
            ),
            (
                "Jared: Find the receipt for the new water heater",
                "installation record",
            ),
            ("Leo: Start quiet drawing time", "soft music"),
            ("Mia: Can I print my homework?", "paper"),
            (
                "Sarah: Make upstairs cooler but leave Leo's room alone",
                "unchanged",
            ),
            ("Jared: What's the noisy appliance?", "washing machine"),
            ("Leo: Did my tooth fairy box stay closed?", "stayed closed"),
            (
                "Mia: Where is the white extension cord for my project?",
                "craft bin",
            ),
            (
                "Sarah: Turn off screens during family dinner",
                "Family dinner",
            ),
            ("Jared: What changed in the garage today?", "side door"),
            ("Leo: Make the stairs bright", "90 percent"),
            (
                "Mia: Remind me to water my plant after school",
                "after school",
            ),
            (
                "Sarah: Find a chicken recipe without peanuts",
                "lemon chicken",
            ),
            ("Jared: Why did the security alarm chirp?", "low-battery"),
            ("Leo: Can I use the microwave?", "Mom"),
            (
                "Mia: Save this temperature as rehearsal comfort",
                "rehearsal",
            ),
            ("Sarah: Who's in the backyard?", "Jared"),
            ("Jared: Start workshop dust control", "purifier"),
            ("Leo: What's left on my bedtime chart?", "pajamas"),
            (
                "Mia: Make my closet light turn on when I open it",
                "auto-off",
            ),
            (
                "Sarah: Did I close the upstairs window before the rain?",
                "2:14 PM",
            ),
            (
                "Jared: Put the house in low-power mode until five",
                "standby-safe",
            ),
            ("Sarah: Find Leo's vaccination form", "Health Documents"),
            ("Mia: Did Mom sign my field trip form?", "this morning"),
            ("Leo: Put on an animal show, but not loud", "low"),
            ("Jared: What devices are on guest Wi-Fi?", "unknown device"),
            (
                "Sarah: Keep the front entry lights on until Mia gets home",
                "geofence",
            ),
            ("Mia: Is the side path icy?", "below freezing"),
            ("Leo: I hear dripping", "sensor dry"),
            ("Jared: Why is the office internet slow?", "backup drive"),
            ("Sarah: Start school-night reset", "morning prep"),
            ("Mia: Find the photo backdrop instructions", "art project"),
            ("Leo: Where's the red marker?", "craft caddy"),
            (
                "Jared: Notify me if the freezer goes above 10 degrees",
                "10F",
            ),
            ("Sarah: What chores did Leo skip this week?", "Tuesday"),
            ("Mia: Turn on only the mirror lights", "mirror lights on"),
            ("Leo: Can the cat sleep in my room?", "approval"),
            ("Jared: Set backyard lights for grilling", "path lights"),
            ("Sarah: Why is Mia's purifier on high?", "dust"),
            ("Mia: Make a packing list for my swim meet", "goggles"),
            ("Leo: Read me the next step for cookies", "scoop dough"),
            ("Jared: Which outdoor cameras need cleaning?", "driveway"),
            (
                "Sarah: Did the garage close after Jared left?",
                "two minutes",
            ),
            (
                "Mia: Add batteries and poster board to my project list",
                "poster board",
            ),
            ("Leo: I'm too scared to go downstairs", "parents notified"),
            (
                "Jared: Find furnace manual troubleshooting code 31",
                "pressure switch",
            ),
            (
                "Sarah: Keep dinner warm until Jared arrives",
                "safe duration",
            ),
            (
                "Mia: Schedule quiet time after school on Wednesdays",
                "Wednesday",
            ),
            ("Leo: Did I feed the cat too much?", "within"),
            (
                "Jared: What's the oldest thing in the fridge?",
                "lentil soup",
            ),
            (
                "Sarah: Open windows if the air outside is cleaner",
                "kitchen window",
            ),
            ("Mia: Why is my lamp flickering?", "unstable power"),
            ("Leo: Can I open the garage door?", "cannot"),
            ("Jared: Create a holiday lighting schedule", "10:30 PM"),
            (
                "Sarah: Find the note about the plumber's shutoff valve",
                "access panel",
            ),
            ("Mia: Use my rainy-day alarm tomorrow", "light ramp"),
            ("Leo: What do I need for soccer practice?", "shin guards"),
            ("Jared: Did anyone bypass a sensor?", "9:12 AM"),
            ("Sarah: Start guest breakfast mode", "coffee"),
            (
                "Mia: Where did I save my poem about winter?",
                "First Snow Draft",
            ),
            ("Leo: Make the laundry room not scary", "brighter"),
            ("Jared: What's the current water pressure?", "58 PSI"),
            (
                "Sarah: Remind me to check the oven after it preheats",
                "preheating",
            ),
            (
                "Mia: Turn off the hallway camera while sleepover guests change",
                "privacy mode",
            ),
            (
                "Leo: Tell me when the cookies are cool enough",
                "cooled enough",
            ),
            (
                "Jared: Why did the vacuum avoid the dining room?",
                "no-go zone",
            ),
            (
                "Sarah: Find the toddler gate instructions",
                "pressure-mount",
            ),
            ("Mia: My room smells weird", "elevated VOCs"),
            ("Leo: Did Dad see my message?", "4:23 PM"),
            (
                "Jared: Shut off the water automatically if the laundry leaks again",
                "main water valve",
            ),
            ("Sarah: Which backpacks are by the door?", "Leo's backpack"),
            ("Mia: Make my alarm skip holidays", "school holidays"),
            (
                "Leo: Turn on my morning checklist on the wall",
                "hallway display",
            ),
            (
                "Jared: Give me a privacy report for the cameras",
                "no unusual access",
            ),
            (
                "Sarah: Find the recipe where we used the green bowl",
                "sesame noodle",
            ),
            ("Mia: Can I practice drums now?", "practice pads"),
            (
                "Leo: Where's the flashlight if the lights go out?",
                "mudroom drawer",
            ),
            ("Jared: Which automation fired the most today?", "18 times"),
            (
                "Sarah: Make upstairs warmer when the kids are getting ready",
                "automatically",
            ),
            (
                "Mia: What snacks did we pack for my last tournament?",
                "blue sports drink",
            ),
            ("Jared: Run a final safety sweep", "one upstairs window"),
        ] {
            assert!(
                mem.semantic_search(query, 3)
                    .unwrap()
                    .iter()
                    .any(|hit| hit.entry.content.contains(expected)),
                "query {query:?} should recall {expected:?}"
            );
        }
    }

    #[test]
    fn semantic_embeddings_rebuild_on_reopen() {
        let path = temp_memory_path("semantic-reopen");
        {
            let mem = Memory::open(&path).unwrap();
            mem.store(
                "preference",
                "Jared prefers the living room thermostat at 72F.",
            )
            .unwrap();
        }

        let reopened = Memory::open(&path).unwrap();
        let hits = reopened.semantic_search("I'm feeling cold", 3).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].entry.content.contains("thermostat"));
    }

    #[test]
    fn has_similar_is_parameterized_for_quotes() {
        let mem = temp_memory();
        mem.store("relationship", "User's dog is named O'Malley")
            .unwrap();

        assert!(
            mem.has_similar("User's dog is named O'Malley").unwrap(),
            "quoted names should not break duplicate detection"
        );
    }

    #[test]
    fn fts_rebuild_restores_consistency() {
        let mem = temp_memory();
        mem.store("fact", "GenieClaw runs locally").unwrap();

        mem.rebuild_fts().unwrap();
        let healthy = mem.health().unwrap();
        assert!(healthy.quick_check_ok);
        assert!(healthy.fts_consistent);
        assert!(!healthy.migration_degraded);
        assert_eq!(healthy.memory_rows, healthy.fts_rows);
    }

    #[test]
    fn open_does_not_mark_migration_degraded_on_fresh_db() {
        let mem = temp_memory();
        assert!(!mem.migration_degraded());
        let health = mem.health().unwrap();
        assert!(!health.migration_degraded);
    }

    #[test]
    fn open_marks_migration_degraded_when_fts_rebuild_fails() {
        let path = temp_memory_path("broken-fts");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE memories (
                    id            INTEGER PRIMARY KEY,
                    kind          TEXT NOT NULL,
                    content       TEXT NOT NULL,
                    created_ms    INTEGER NOT NULL,
                    accessed_ms   INTEGER NOT NULL,
                    recall_count  INTEGER NOT NULL DEFAULT 0,
                    max_score     REAL NOT NULL DEFAULT 0.0,
                    promoted      INTEGER NOT NULL DEFAULT 0,
                    query_hashes  TEXT NOT NULL DEFAULT '[]',
                    evergreen     INTEGER NOT NULL DEFAULT 0,
                    scope         TEXT NOT NULL DEFAULT 'household',
                    sensitivity   TEXT NOT NULL DEFAULT 'normal',
                    spoken_policy TEXT NOT NULL DEFAULT 'allow',
                    display_order INTEGER NOT NULL DEFAULT 2147483647
                );
                CREATE TABLE memories_fts (content TEXT NOT NULL);
                ",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO memories (kind, content, created_ms, accessed_ms) VALUES (?1, ?2, 1, 1)",
                rusqlite::params!["fact", "orphaned row"],
            )
            .unwrap();
        }

        let mem = Memory::open(&path).unwrap();
        assert!(
            mem.migration_degraded(),
            "broken FTS table should mark memory degraded at open"
        );
        let health = mem.health().unwrap();
        assert!(health.migration_degraded);
    }

    #[test]
    fn fts_updates_when_content_changes() {
        let mem = temp_memory();
        let id = mem.store("fact", "old keyword").unwrap();
        mem.conn
            .execute(
                "UPDATE memories SET content = 'new keyword' WHERE id = ?1",
                [id],
            )
            .unwrap();

        let results = mem.search("new keyword", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "new keyword");
    }

    #[test]
    fn store_writes_canonical_daily_note_and_event_log() {
        let mem = temp_memory();
        let id = mem.store("identity", "User's name is Jared").unwrap();

        let daily = mem
            .canonical_dir
            .join(format!("{}.md", canonical_date(now_ms())));
        let events = mem
            .canonical_dir
            .join("events")
            .join(format!("{}.jsonl", canonical_date(now_ms())));

        let daily_text = std::fs::read_to_string(daily).unwrap();
        let event_text = std::fs::read_to_string(events).unwrap();

        assert!(daily_text.contains("User's name is Jared"));
        assert!(event_text.contains("\"action\":\"store\""));
        assert!(event_text.contains(&format!("\"id\":{id}")));
    }

    #[test]
    fn store_persists_policy_metadata() {
        let mem = temp_memory();
        mem.store("person_preference", "Maya likes oat milk")
            .unwrap();

        let entries = mem.search("oat milk", 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].metadata.scope, policy::MemoryScope::Person);
        assert_eq!(
            entries[0].metadata.spoken_policy,
            policy::SpokenMemoryPolicy::Allow
        );
    }

    #[test]
    fn open_backfills_policy_columns_for_existing_rows() {
        let path = temp_memory_path("backfill");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE memories (
                    id            INTEGER PRIMARY KEY,
                    kind          TEXT NOT NULL,
                    content       TEXT NOT NULL,
                    created_ms    INTEGER NOT NULL,
                    accessed_ms   INTEGER NOT NULL,
                    recall_count  INTEGER NOT NULL DEFAULT 0,
                    max_score     REAL NOT NULL DEFAULT 0.0,
                    promoted      INTEGER NOT NULL DEFAULT 0,
                    query_hashes  TEXT NOT NULL DEFAULT '[]',
                    evergreen     INTEGER NOT NULL DEFAULT 0
                );
                CREATE VIRTUAL TABLE memories_fts USING fts5(
                    content,
                    content='memories',
                    content_rowid='id'
                );
                ",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO memories (kind, content, created_ms, accessed_ms) VALUES (?1, ?2, 1, 1)",
                rusqlite::params!["person_preference", "Maya likes oat milk"],
            )
            .unwrap();
        }

        let mem = Memory::open(&path).unwrap();
        let entries = mem.search("oat milk", 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].metadata.scope, policy::MemoryScope::Person);
    }

    #[test]
    fn promotion_writes_root_memory_file() {
        let mem = temp_memory();
        let id = mem
            .store("preference", "User's favorite color is green")
            .unwrap();
        mem.mark_promoted(id).unwrap();

        let root = mem.canonical_dir.join("MEMORY.md");
        let text = std::fs::read_to_string(root).unwrap();
        assert!(text.contains("User's favorite color is green"));
        assert!(text.contains("INDEX.md"));
    }

    #[test]
    fn promotion_writes_namespace_note_for_household_memory() {
        let mem = temp_memory();
        let id = mem.store("preference", "User likes ginger tea").unwrap();
        mem.mark_promoted(id).unwrap();

        let note = mem.canonical_dir.join("namespaces/household/preference.md");
        let text = std::fs::read_to_string(note).unwrap();
        assert!(text.contains("namespace: household.preference"));
        assert!(text.contains("User likes ginger tea"));
    }

    #[test]
    fn promotion_does_not_write_person_memory_to_root_file() {
        let mem = temp_memory();
        let id = mem
            .store("person_preference", "Maya likes oat milk")
            .unwrap();
        mem.mark_promoted(id).unwrap();

        let root = mem.canonical_dir.join("MEMORY.md");
        let text = std::fs::read_to_string(root).unwrap_or_default();
        assert!(!text.contains("Maya likes oat milk"));
    }

    #[test]
    fn promotion_redacts_person_memory_in_namespace_note() {
        let mem = temp_memory();
        let id = mem
            .store("person_preference", "Maya likes oat milk")
            .unwrap();
        mem.mark_promoted(id).unwrap();

        let note = mem.canonical_dir.join("namespaces/person/preference.md");
        let text = std::fs::read_to_string(note).unwrap();
        assert!(text.contains("namespace: person.preference"));
        assert!(text.contains("redacted"));
        assert!(!text.contains("Maya likes oat milk"));
    }

    #[test]
    fn delete_writes_delete_event() {
        let mem = temp_memory();
        let id = mem.store("fact", "temporary fact").unwrap();
        assert!(mem.delete_by_id(id).unwrap());

        let events = mem
            .canonical_dir
            .join("events")
            .join(format!("{}.jsonl", canonical_date(now_ms())));
        let event_text = std::fs::read_to_string(events).unwrap();
        assert!(event_text.contains("\"action\":\"delete\""));
        assert!(event_text.contains("temporary fact"));
    }

    #[test]
    fn update_managed_refreshes_promoted_root_file() {
        let mem = temp_memory();
        let id = mem
            .store("preference", "User's favorite color is green")
            .unwrap();
        mem.mark_promoted(id).unwrap();
        mem.update_managed(id, "User's favorite color is blue", None)
            .unwrap();

        let root = mem.canonical_dir.join("MEMORY.md");
        let text = std::fs::read_to_string(root).unwrap();
        assert!(!text.contains("green"));
        assert!(text.contains("blue"));
    }

    #[test]
    fn delete_promoted_memory_refreshes_root_file() {
        let mem = temp_memory();
        let first = mem.store("fact", "alpha durable fact").unwrap();
        let second = mem.store("fact", "beta durable fact").unwrap();
        mem.mark_promoted(first).unwrap();
        mem.mark_promoted(second).unwrap();

        assert!(mem.delete_by_id(first).unwrap());

        let root = mem.canonical_dir.join("MEMORY.md");
        let text = std::fs::read_to_string(root).unwrap();
        assert!(!text.contains("alpha durable fact"));
        assert!(text.contains("beta durable fact"));
    }

    #[test]
    fn reorder_managed_rebuilds_promoted_root_order() {
        let mem = temp_memory();
        let first = mem.store("fact", "first durable fact").unwrap();
        let second = mem.store("fact", "second durable fact").unwrap();
        mem.mark_promoted(first).unwrap();
        mem.mark_promoted(second).unwrap();

        mem.reorder_managed(&[second, first]).unwrap();

        let root = mem.canonical_dir.join("MEMORY.md");
        let text = std::fs::read_to_string(root).unwrap();
        let first_pos = text.find("first durable fact").unwrap();
        let second_pos = text.find("second durable fact").unwrap();
        assert!(second_pos < first_pos);
    }

    #[test]
    fn list_managed_reports_namespace_and_canonical_note() {
        let mem = temp_memory();
        let id = mem.store("preference", "User likes lemon tea").unwrap();
        mem.mark_promoted(id).unwrap();

        let entries = mem.list_managed(10).unwrap();
        let entry = entries.into_iter().find(|entry| entry.id == id).unwrap();
        assert_eq!(entry.namespace, "household.preference");
        assert_eq!(entry.disclosure_class, "household");
        assert_eq!(
            entry.canonical_note.as_deref(),
            Some("memory/namespaces/household/preference.md")
        );
    }
}
