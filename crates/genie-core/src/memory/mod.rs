pub mod decay;
pub mod extract;
pub mod inject;
pub mod policy;
pub mod recall;

use anyhow::Result;
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

        let entries = stmt
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

        let entries = stmt
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

        let entries = stmt
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
        let entries = stmt
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

        let entries = stmt
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
    let (note_type, note_content) = if matches!(
        kind_lower.as_str(),
        "note" | "notes" | "reminder" | "manual" | "document" | "context"
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
        if clean.eq_ignore_ascii_case("for") || clean.eq_ignore_ascii_case("because") {
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

fn format_profile_attribute_answer(attr: &HouseholdProfileAttribute) -> String {
    match attr.attribute.as_str() {
        "age" => format!("{} is {} years old.", attr.name, attr.value),
        "likes" => format!("{} likes {}.", attr.name, attr.value),
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

fn format_household_note_answer(note: &HouseholdNote) -> String {
    match note.note_type.as_str() {
        "reminder" => format!("I found this reminder: {}", note.content),
        "manual" => format!("I found these instructions: {}", note.content),
        "media" => format!("I found this watch note: {}", note.content),
        _ => format!("I found this note: {}", note.content),
    }
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
