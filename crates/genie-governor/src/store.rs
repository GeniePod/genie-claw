use anyhow::Result;
use genie_common::tegrastats::TegraSnapshot;
use rusqlite::Connection;
use std::path::Path;

/// SQLite store for tegrastats history and mode transitions.
///
/// Retains 24 hours of 5-second samples (~17,280 rows/day).
/// Older rows pruned on startup and every hour.
pub struct Store {
    conn: Connection,
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

    /// Remove rows older than 24 hours.
    pub fn prune(&self) -> Result<usize> {
        let cutoff_ms = now_ms().saturating_sub(24 * 3600 * 1000);
        let deleted = self.conn.execute(
            "DELETE FROM tegrastats WHERE ts_ms < ?1",
            rusqlite::params![cutoff_ms],
        )?;
        if deleted > 0 {
            tracing::debug!(deleted, "pruned old tegrastats rows");
        }
        Ok(deleted)
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_common::tegrastats::TegraSnapshot;

    fn sample_snapshot(ts_ms: u64) -> TegraSnapshot {
        TegraSnapshot {
            timestamp_ms: ts_ms,
            ram_used_mb: 1000,
            ram_total_mb: 8000,
            swap_used_mb: 0,
            swap_total_mb: 0,
            gpu_freq_pct: 50,
            cpu_loads: Vec::new(),
            gpu_temp_c: Some(45.0),
            cpu_temp_c: Some(50.0),
            power_mw: Some(5000),
        }
    }

    #[test]
    fn insert_snapshot_and_prune_on_writable_db() {
        let dir = std::env::temp_dir().join(format!(
            "genie-governor-store-writable-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("governor.db");

        let store = Store::open(&db_path).unwrap();
        store.insert_snapshot(&sample_snapshot(1_000)).unwrap();
        store.insert_snapshot(&sample_snapshot(2_000)).unwrap();
        assert!(store.prune().is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn insert_snapshot_errors_on_readonly_db_without_panic() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "genie-governor-store-readonly-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("governor.db");
        {
            let store = Store::open(&db_path).unwrap();
            store.insert_snapshot(&sample_snapshot(1)).unwrap();
            drop(store);
        }

        let mut perms = std::fs::metadata(&db_path).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&db_path, perms).unwrap();

        let store = Store::open(&db_path).unwrap();
        let err = store
            .insert_snapshot(&sample_snapshot(2))
            .expect_err("readonly db should reject insert");
        assert!(!err.to_string().is_empty());

        let mut perms = std::fs::metadata(&db_path).unwrap().permissions();
        perms.set_mode(0o644);
        let _ = std::fs::set_permissions(&db_path, perms);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
