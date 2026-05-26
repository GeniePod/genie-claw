use std::sync::{Arc, Mutex};

/// Maximum concurrent in-memory timers per process.
pub const MAX_ACTIVE_TIMERS: usize = 64;
/// Longest timer the tool accepts (7 days).
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

    pub fn set(&self, seconds: u64, label: &str) -> Result<(), String> {
        if seconds == 0 {
            return Err("timer duration must be at least 1 second".into());
        }
        if seconds > MAX_TIMER_SECONDS {
            return Err(format!(
                "timer duration cannot exceed {} seconds (7 days)",
                MAX_TIMER_SECONDS
            ));
        }
        let end_ms = now_ms()
            .checked_add(
                seconds
                    .checked_mul(1000)
                    .ok_or_else(|| "timer duration overflow".to_string())?,
            )
            .ok_or_else(|| "timer end time overflow".to_string())?;

        let mut timers = self.timers.lock().unwrap();
        if timers.len() >= MAX_ACTIVE_TIMERS {
            return Err(format!(
                "too many active timers (max {MAX_ACTIVE_TIMERS}); wait for one to fire or cancel via voice"
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
    fn set_rejects_zero_seconds() {
        let mgr = TimerManager::new();
        let err = mgr.set(0, "x").unwrap_err();
        assert!(err.contains("at least 1 second"));
    }

    #[test]
    fn set_rejects_duration_above_cap() {
        let mgr = TimerManager::new();
        let err = mgr
            .set(MAX_TIMER_SECONDS + 1, "x")
            .unwrap_err();
        assert!(err.contains("cannot exceed"));
    }

    #[test]
    fn set_enforces_active_timer_cap() {
        let mgr = TimerManager::new();
        for i in 0..MAX_ACTIVE_TIMERS {
            mgr.set(60, &format!("t{i}")).unwrap();
        }
        let err = mgr.set(60, "overflow").unwrap_err();
        assert!(err.contains("too many active timers"));
        assert_eq!(mgr.count(), MAX_ACTIVE_TIMERS);
    }

    #[test]
    fn check_fired_drains_expired_timers() {
        let mgr = TimerManager::new();
        let mut timers = mgr.timers.lock().unwrap();
        timers.push(Timer {
            label: "done".into(),
            end_ms: 0,
        });
        drop(timers);
        let fired = mgr.check_fired();
        assert_eq!(fired, vec!["done"]);
        assert_eq!(mgr.count(), 0);
    }
}
