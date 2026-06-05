use std::sync::{Arc, Mutex};

/// Maximum number of concurrent in-memory timers per process.
pub const MAX_ACTIVE_TIMERS: usize = 64;
/// Longest duration the timer tool accepts, in seconds (7 days).
pub const MAX_TIMER_SECONDS: u64 = 7 * 24 * 3600;

#[derive(Debug, Clone)]
struct Timer {
    label: String,
    end_ms: u64,
}

/// Simple in-memory timer manager.
///
/// Timers are checked by the voice orchestrator on each tick.
/// When a timer fires, the orchestrator speaks the notification.
pub struct TimerManager {
    timers: Arc<Mutex<Vec<Timer>>>,
}

impl Default for TimerManager {
    fn default() -> Self {
        Self {
            timers: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl TimerManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a countdown timer.
    ///
    /// Returns an error (instead of growing memory unbounded or wrapping the
    /// end timestamp) when the duration is zero/oversized or when the active
    /// timer cap is already reached.
    pub fn set(&self, seconds: u64, label: &str) -> Result<(), String> {
        if seconds == 0 {
            return Err("timer duration must be at least 1 second".to_string());
        }
        if seconds > MAX_TIMER_SECONDS {
            return Err(format!(
                "timer duration cannot exceed {MAX_TIMER_SECONDS} seconds (7 days)"
            ));
        }

        let end_ms = seconds
            .checked_mul(1000)
            .and_then(|ms| now_ms().checked_add(ms))
            .ok_or_else(|| "timer end time overflowed".to_string())?;

        let mut timers = self.timers.lock().unwrap();
        if timers.len() >= MAX_ACTIVE_TIMERS {
            return Err(format!(
                "too many active timers (max {MAX_ACTIVE_TIMERS}); wait for one to fire before setting another"
            ));
        }
        timers.push(Timer {
            label: label.to_string(),
            end_ms,
        });
        tracing::info!(seconds, label, active = timers.len(), "timer set");
        Ok(())
    }

    /// Check and drain any fired timers.
    pub fn check_fired(&self) -> Vec<String> {
        let now = now_ms();
        let mut timers = self.timers.lock().unwrap();
        let mut fired = Vec::new();

        timers.retain(|t| {
            if t.end_ms <= now {
                fired.push(t.label.clone());
                false
            } else {
                true
            }
        });

        fired
    }

    pub fn count(&self) -> usize {
        self.timers.lock().unwrap().len()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_accepts_valid_duration() {
        let mgr = TimerManager::new();
        assert!(mgr.set(60, "pasta").is_ok());
        assert_eq!(mgr.count(), 1);
    }

    #[test]
    fn set_rejects_zero_duration() {
        let mgr = TimerManager::new();
        let err = mgr.set(0, "instant").unwrap_err();
        assert!(err.contains("at least 1 second"), "got: {err}");
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn set_rejects_duration_above_cap() {
        let mgr = TimerManager::new();
        let err = mgr.set(MAX_TIMER_SECONDS + 1, "too-long").unwrap_err();
        assert!(err.contains("cannot exceed"), "got: {err}");
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn set_accepts_max_duration_boundary() {
        let mgr = TimerManager::new();
        assert!(mgr.set(MAX_TIMER_SECONDS, "max").is_ok());
        assert_eq!(mgr.count(), 1);
    }

    #[test]
    fn set_rejects_overflowing_duration() {
        let mgr = TimerManager::new();
        // u64::MAX would wrap the end timestamp with unchecked arithmetic.
        let err = mgr.set(u64::MAX, "overflow").unwrap_err();
        // Caught by the duration cap before the multiply, but either way it must error.
        assert!(!err.is_empty());
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn set_enforces_active_timer_cap() {
        let mgr = TimerManager::new();
        for i in 0..MAX_ACTIVE_TIMERS {
            mgr.set(60, &format!("t{i}")).unwrap();
        }
        let err = mgr.set(60, "one-too-many").unwrap_err();
        assert!(err.contains("too many active timers"), "got: {err}");
        assert_eq!(mgr.count(), MAX_ACTIVE_TIMERS);
    }

    #[test]
    fn check_fired_drains_expired_and_keeps_pending() {
        let mgr = TimerManager::new();
        {
            let mut timers = mgr.timers.lock().unwrap();
            timers.push(Timer {
                label: "expired".into(),
                end_ms: 0,
            });
        }
        mgr.set(3600, "pending").unwrap();

        let fired = mgr.check_fired();
        assert_eq!(fired, vec!["expired".to_string()]);
        assert_eq!(mgr.count(), 1);
    }
}
