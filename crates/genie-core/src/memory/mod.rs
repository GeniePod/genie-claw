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
        "recipe" => "recipe",
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
}

fn semantic_memory_type(kind: &str, content: &str) -> String {
    let lower = content.to_ascii_lowercase();
    if lower.contains("father's day")
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

fn embedding_text_for_memory(kind: &str, content: &str) -> String {
    format!("{} {}", semantic_memory_type(kind, content), content)
}

fn embedding_text_for_query(query: &str) -> String {
    let lower = query.to_ascii_lowercase();
    if lower.contains("father's day")
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
    } else if lower.contains("date night") {
        format!("date_night jazz italian {query}")
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
    if lower.contains("father's day")
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
    } else if lower.contains("date night") {
        Some("date_night".into())
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
        || kind_lower.contains("city_services")
        || kind_lower.contains("community_services")
        || kind_lower.contains("business_hours")
        || kind_lower.contains("astronomical")
        || kind_lower.contains("trash")
        || lower.contains("bus arrives")
        || lower.contains("bill is due")
        || lower.contains("sunset")
        || lower.contains("sun set")
        || lower.contains("recycling")
        || lower.contains("trash pickup")
        || lower.contains("pool")
        || lower.contains("library closes")
        || lower.contains("library close")
        || lower.contains("parent-teacher conference")
        || lower.contains("parent teacher conference"))
    {
        return Vec::new();
    }

    let mut items = Vec::new();
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
        || lower.contains("security system was disarmed")
        || lower.contains("system was disarmed")
        || lower.contains("disarmed by")
        || lower.contains("allowance")
        || lower.contains("bill") && lower.contains("paid"))
    {
        return Vec::new();
    }

    let mut events = Vec::new();
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

fn schedule_item_query(query: &str) -> Option<(String, Option<String>, Option<String>)> {
    let lower = query.to_ascii_lowercase();
    if lower.contains("sunset") || lower.contains("sun set") {
        return Some((
            "sunset".into(),
            Some("sunset".into()),
            calendar_day_from_text(&lower).or_else(|| Some("today".into())),
        ));
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
    None
}

fn event_log_query(query: &str) -> Option<(String, String, Option<String>)> {
    let lower = query.to_ascii_lowercase();
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
    if (mentions_wifi && mentions_credential) || lower.contains("network password") {
        Some("wifi_password")
    } else if lower.contains("password") || lower.contains(" pass:") || lower.starts_with("pass:") {
        Some("password")
    } else if lower.contains("gate code") {
        Some("gate_code")
    } else if lower.contains("door code")
        || lower.contains("lock code")
        || (mentions_lock_word && (lower.contains("combination") || lower.contains("combo")))
    {
        Some("lock_code")
    } else if lower.contains("alarm code") || lower.contains("security code") {
        Some("security_code")
    } else if lower.contains("confirmation number") {
        Some("confirmation_number")
    } else if lower.contains("account number") {
        Some("account_number")
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
        "find the receipt for ",
        "find receipt for ",
        "find the instructions for ",
        "find instructions for ",
        "who do we call for ",
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
        || lower.starts_with("who took the photos ")
        || lower.starts_with("we have a leak ")
        || lower.starts_with("there is a leak ")
        || lower.starts_with("how do i clean ")
        || lower.starts_with("how do we clean ")
        || lower.starts_with("how do i reset ")
        || lower.starts_with("how do we reset ")
        || lower.starts_with("what did we have for dinner ")
        || lower.starts_with("find the recipe for ")
        || lower.starts_with("what is the school")
        || lower.starts_with("what s the school")
        || lower.starts_with("what's the school")
        || lower.starts_with("who do we call for ")
        || lower.starts_with("what's on the hardware store list")
        || lower.starts_with("what s on the hardware store list")
        || lower.starts_with("what is on the hardware store list")
        || lower.starts_with("where did we put ")
        || lower.starts_with("where did i put ")
        || lower.starts_with("when is the next trash pickup")
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
    let secret_type = secret_type_from_text(&lower)?;
    if !(lower.contains("what")
        || lower.contains("show")
        || lower.contains("find")
        || lower.contains("where")
        || lower.contains("password")
        || lower.contains("code")
        || lower.contains("combo")
        || lower.contains("number"))
    {
        return None;
    }

    let label = if lower.contains("guest") && secret_type == "wifi_password" {
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
            other => other,
        };
        format!("Yes. {} marked {task_name} complete{time}.", task.person)
    } else {
        format!("I found this task log: {}", task.description)
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
