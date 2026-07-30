use anyhow::Result;
use genie_common::tegrastats::TegraSnapshot;
use rusqlite::Connection;
use std::path::Path;

/// SQLite store for tegrastats history and mode transitions.
///
/// Both tables are bounded by [`Store::prune`], which runs on startup and every
/// hour: 24 hours of 5-second samples (~17,280 rows/day) and 30 days of mode
/// transitions.
pub struct Store {
    conn: Connection,
}

/// Retention for 5-second `tegrastats` samples.
const TEGRASTATS_RETENTION_MS: u64 = 24 * 3600 * 1000;

/// Retention for mode transitions. They arrive orders of magnitude more slowly
/// than samples — tens per day rather than thousands — and the history is what
/// makes mode flapping diagnosable after the fact, so they are kept longer while
/// still being bounded.
const MODE_TRANSITION_RETENTION_MS: u64 = 30 * 24 * 3600 * 1000;

/// Rows removed by one [`Store::prune`] pass, per table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneCounts {
    pub tegrastats: usize,
    pub mode_transitions: usize,
}

impl PruneCounts {
    pub fn total(self) -> usize {
        self.tegrastats + self.mode_transitions
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;

        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA busy_timeout = 3000;

            CREATE TABLE IF NOT EXISTS tegrastats (
                ts_ms       INTEGER NOT NULL,
                ram_used_mb INTEGER NOT NULL,
                ram_total_mb INTEGER NOT NULL,
                gpu_freq_pct INTEGER NOT NULL,
                gpu_temp_c  REAL,
                cpu_temp_c  REAL,
                power_mw    INTEGER,
                swap_used_mb INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_tegrastats_ts ON tegrastats(ts_ms);

            CREATE TABLE IF NOT EXISTS mode_transitions (
                ts_ms       INTEGER NOT NULL,
                from_mode   TEXT NOT NULL,
                to_mode     TEXT NOT NULL,
                reason      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_mode_ts ON mode_transitions(ts_ms);
            ",
        )?;

        let store = Self { conn };
        store.prune()?;
        Ok(store)
    }

    pub fn insert_snapshot(&self, snap: &TegraSnapshot) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tegrastats (ts_ms, ram_used_mb, ram_total_mb, gpu_freq_pct, gpu_temp_c, cpu_temp_c, power_mw, swap_used_mb)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                snap.timestamp_ms,
                snap.ram_used_mb,
                snap.ram_total_mb,
                snap.gpu_freq_pct,
                snap.gpu_temp_c,
                snap.cpu_temp_c,
                snap.power_mw,
                snap.swap_used_mb,
            ],
        )?;
        Ok(())
    }

    pub fn insert_transition(&self, ts_ms: u64, from: &str, to: &str, reason: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO mode_transitions (ts_ms, from_mode, to_mode, reason) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![ts_ms, from, to, reason],
        )?;
        Ok(())
    }

    /// Remove rows past their retention window, from both tables.
    ///
    /// `mode_transitions` previously grew without bound: nothing ever deleted
    /// from it, so a long-running device accumulated one row per transition for
    /// the life of the install even though this type documented pruning. Under
    /// memory-pressure flapping a transition can be recorded on every 5-second
    /// tick, so the table's growth rate is not inherently lower than the sample
    /// table's — only its typical rate is.
    pub fn prune(&self) -> Result<PruneCounts> {
        let now = now_ms();
        let tegrastats = self.conn.execute(
            "DELETE FROM tegrastats WHERE ts_ms < ?1",
            rusqlite::params![now.saturating_sub(TEGRASTATS_RETENTION_MS)],
        )?;
        let mode_transitions = self.conn.execute(
            "DELETE FROM mode_transitions WHERE ts_ms < ?1",
            rusqlite::params![now.saturating_sub(MODE_TRANSITION_RETENTION_MS)],
        )?;

        let counts = PruneCounts {
            tegrastats,
            mode_transitions,
        };
        if counts.total() > 0 {
            tracing::debug!(
                tegrastats = counts.tegrastats,
                mode_transitions = counts.mode_transitions,
                "pruned governor history"
            );
        }
        Ok(counts)
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
