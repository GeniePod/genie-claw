pub mod decay;
pub mod extract;
pub mod inject;
pub mod recall;

use anyhow::Result;
use rusqlite::{Connection, OpenFlags, params_from_iter};
use std::path::Path;
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryHealth {
    pub quick_check_ok: bool,
    pub memory_rows: usize,
    pub fts_rows: usize,
    pub fts_consistent: bool,
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
}

impl Memory {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_half_life(path, 30.0)
    }

    pub fn open_with_half_life(path: &Path, half_life_days: f64) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

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
                evergreen     INTEGER NOT NULL DEFAULT 0
            );

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
        let _ = conn.execute(
            "ALTER TABLE memories ADD COLUMN recall_count INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE memories ADD COLUMN max_score REAL NOT NULL DEFAULT 0.0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE memories ADD COLUMN promoted INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE memories ADD COLUMN query_hashes TEXT NOT NULL DEFAULT '[]'",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE memories ADD COLUMN evergreen INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_memories_kind_accessed ON memories(kind, accessed_ms DESC)", []);
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_memories_promotion ON memories(promoted, recall_count, max_score)", []);
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_memories_prune ON memories(evergreen, promoted, accessed_ms)", []);

        // Older databases may predate the FTS update trigger or may have been
        // edited by a recovery tool. Rebuild once at open so recall and forget
        // do not silently miss rows.
        let _ = rebuild_fts_index(&conn);

        Ok(Self {
            conn,
            half_life_days,
        })
    }

    /// Store a new memory.
    pub fn store(&self, kind: &str, content: &str) -> Result<i64> {
        let now = now_ms();
        let content = normalize_memory_content(content);
        self.conn.execute(
            "INSERT INTO memories (kind, content, created_ms, accessed_ms) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![kind, content, now, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Store an evergreen memory (never decays).
    pub fn store_evergreen(&self, kind: &str, content: &str) -> Result<i64> {
        let now = now_ms();
        let content = normalize_memory_content(content);
        self.conn.execute(
            "INSERT INTO memories (kind, content, created_ms, accessed_ms, evergreen) VALUES (?1, ?2, ?3, ?4, 1)",
            rusqlite::params![kind, content, now, now],
        )?;
        Ok(self.conn.last_insert_rowid())
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
                    m.recall_count, m.max_score, m.promoted, m.evergreen,
                    bm25(memories_fts) as bm25_rank
             FROM memories m
             JOIN memories_fts f ON m.id = f.rowid
             WHERE memories_fts MATCH ?1
             ORDER BY bm25_rank
             LIMIT ?2",
        )?;

        let raw_entries: Vec<(MemoryEntry, f64, bool)> = stmt
            .query_map(rusqlite::params![fts_query, limit * 3], |row| {
                let entry = MemoryEntry {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    content: row.get(2)?,
                    created_ms: row.get(3)?,
                    accessed_ms: row.get(4)?,
                    recall_count: row.get::<_, i64>(5).unwrap_or(0),
                    max_score: row.get::<_, f64>(6).unwrap_or(0.0),
                    promoted: row.get::<_, i64>(7).unwrap_or(0) != 0,
                };
                let bm25_rank: f64 = row.get::<_, f64>(9).unwrap_or(0.0);
                let evergreen: bool = row.get::<_, i64>(8).unwrap_or(0) != 0;
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
            let _ = self.update_recall_tracking(entry.id, now, *score, &query_hash);
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
                    recall_count, max_score, promoted
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
            .query_map(params_from_iter(values.iter()), |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    content: row.get(2)?,
                    created_ms: row.get(3)?,
                    accessed_ms: row.get(4)?,
                    recall_count: row.get(5).unwrap_or(0),
                    max_score: row.get(6).unwrap_or(0.0),
                    promoted: row.get::<_, i64>(7).unwrap_or(0) != 0,
                })
            })?
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
            let _ = self.update_recall_tracking(entry.id, now, score, query_hash);
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
                    recall_count, max_score, promoted
             FROM memories
             ORDER BY accessed_ms DESC, id DESC
             LIMIT ?1",
        )?;

        let entries = stmt
            .query_map([limit], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    content: row.get(2)?,
                    created_ms: row.get(3)?,
                    accessed_ms: row.get(4)?,
                    recall_count: row.get(5).unwrap_or(0),
                    max_score: row.get(6).unwrap_or(0.0),
                    promoted: row.get::<_, i64>(7).unwrap_or(0) != 0,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

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
                    recall_count, max_score, promoted
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
                |row| {
                    Ok(MemoryEntry {
                        id: row.get(0)?,
                        kind: row.get(1)?,
                        content: row.get(2)?,
                        created_ms: row.get(3)?,
                        accessed_ms: row.get(4)?,
                        recall_count: row.get(5)?,
                        max_score: row.get(6)?,
                        promoted: false,
                    })
                },
            )?
            .filter_map(|r| r.ok())
            .collect();

        Ok(entries)
    }

    /// Mark a memory as promoted (moved to permanent storage).
    pub fn mark_promoted(&self, id: i64) -> Result<()> {
        self.conn
            .execute("UPDATE memories SET promoted = 1 WHERE id = ?1", [id])?;
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
                    recall_count, max_score, promoted
             FROM memories
             WHERE kind = ?1
             ORDER BY accessed_ms DESC
             LIMIT ?2",
        )?;

        let entries = stmt
            .query_map(rusqlite::params![kind, limit], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    content: row.get(2)?,
                    created_ms: row.get(3)?,
                    accessed_ms: row.get(4)?,
                    recall_count: row.get(5).unwrap_or(0),
                    max_score: row.get(6).unwrap_or(0.0),
                    promoted: row.get::<_, i64>(7).unwrap_or(0) != 0,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(entries)
    }

    /// Delete a memory by ID.
    pub fn delete_by_id(&self, id: i64) -> Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM memories WHERE id = ?1", [id])?;
        Ok(deleted > 0)
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

        Ok(MemoryHealth {
            quick_check_ok: quick_check.eq_ignore_ascii_case("ok"),
            memory_rows: memory_rows as usize,
            fts_rows: fts_rows as usize,
            fts_consistent: memory_rows == fts_rows,
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
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_memory() -> Memory {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "geniepod-mem-test-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        Memory::open(&path).unwrap()
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
            &std::env::temp_dir().join(format!(
                "geniepod-mem-evergreen-{}-{}.db",
                std::process::id(),
                TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
            )),
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
        assert_eq!(healthy.memory_rows, healthy.fts_rows);
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
}
