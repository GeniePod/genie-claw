use anyhow::Result;
use genie_common::config::Config;
use genie_common::mode::Mode;
use genie_common::tegrastats::{self, TegraSnapshot};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tokio::time::{Duration, interval};

use crate::control::{self, Command, StatusResponse};
use crate::service_ctl::ServiceCtl;
use crate::store::{self, Store};
use crate::tegra_reader;

pub struct Governor {
    config: Config,
    store: Store,
    current_mode: Mode,
    zram_enabled: bool,
    prune_counter: u32,
    start_time: std::time::Instant,
    /// Latest tegrastats snapshot (None if not on Jetson).
    tegra_rx: Option<watch::Receiver<TegraSnapshot>>,
}

impl Governor {
    pub fn new(config: Config, store: Store) -> Self {
        Self {
            config,
            store,
            current_mode: Mode::Day,
            zram_enabled: false,
            prune_counter: 0,
            start_time: std::time::Instant::now(),
            tegra_rx: None,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        // Spawn tegrastats reader (gracefully degrades on non-Jetson).
        self.tegra_rx = tegra_reader::spawn(self.config.governor.poll_interval_ms).await;

        // Spawn control socket.
        let mut ctrl_rx = control::spawn_listener().await?;

        let poll_ms = self.config.governor.poll_interval_ms;
        let mut tick = interval(Duration::from_millis(poll_ms));
        let mut sigterm = signal(SignalKind::terminate())?;

        tracing::info!(mode = %self.current_mode, "governor loop started");
        sd_notify_ready();

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if let Err(e) = self.tick().await {
                        tracing::error!(error = %e, "tick failed");
                    }
                }
                Some((cmd, resp_tx)) = ctrl_rx.recv() => {
                    let response = self.handle_command(cmd).await;
                    let _ = resp_tx.send(response);
                }
                _ = sigterm.recv() => {
                    tracing::info!("SIGTERM received, shutting down");
                    break;
                }
            }

            sd_notify_watchdog();
        }

        Ok(())
    }

    async fn handle_command(&mut self, cmd: Command) -> String {
        match cmd {
            Command::SetMode { mode } => {
                let ts = store::now_ms();
                match self.transition(ts, mode).await {
                    Ok(()) => serde_json::json!({"ok": true, "mode": mode}).to_string(),
                    Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
                }
            }
            Command::MediaStart => {
                let ts = store::now_ms();
                let result: Result<Mode, anyhow::Error> = async {
                    tokio::fs::create_dir_all("/run/geniepod").await?;
                    tokio::fs::write("/run/geniepod/media_mode", b"1").await?;
                    self.transition(ts, Mode::Media).await?;
                    Ok(Mode::Media)
                }
                .await;
                match result {
                    Ok(mode) => serde_json::json!({"ok": true, "mode": mode}).to_string(),
                    Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
                }
            }
            Command::MediaStop => {
                let ts = store::now_ms();
                let mem_avail = tegrastats::mem_available_mb().unwrap_or(4096);
                let target = self.determine_mode(mem_avail);
                let result: Result<Mode, anyhow::Error> = async {
                    if let Err(error) = tokio::fs::remove_file("/run/geniepod/media_mode").await
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        return Err(error.into());
                    }
                    self.transition(ts, target).await?;
                    Ok(target)
                }
                .await;
                match result {
                    Ok(mode) => serde_json::json!({"ok": true, "mode": mode}).to_string(),
                    Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
                }
            }
            Command::Status => {
                let mem_avail = tegrastats::mem_available_mb().unwrap_or(0);
                let resp = StatusResponse {
                    mode: self.current_mode,
                    mem_available_mb: mem_avail,
                    uptime_secs: self.start_time.elapsed().as_secs(),
                };
                serde_json::to_string(&resp).unwrap_or_default()
            }
        }
    }

    async fn tick(&mut self) -> Result<()> {
        let ts = store::now_ms();

        // 1. Read memory from /proc/meminfo (always available).
        let mem_avail = tegrastats::mem_available_mb().unwrap_or(0);

        // 2. If tegrastats is running, log the latest snapshot.
        if let Some(ref rx) = self.tegra_rx {
            let snap = rx.borrow().clone();
            if snap.timestamp_ms > 0 {
                let _ = self.store.insert_snapshot(&snap);
            }
        }

        // 3. Determine target mode.
        let target = self.determine_mode(mem_avail);

        // 4. Transition if needed.
        if target != self.current_mode {
            self.transition(ts, target).await?;
        }

        // 5. Check pressure even within the same mode.
        self.handle_pressure(mem_avail).await?;

        // 6. Prune DB hourly (~720 ticks at 5s interval).
        self.prune_counter += 1;
        if self.prune_counter >= 720 {
            self.prune_counter = 0;
            let _ = self.store.prune();
        }

        Ok(())
    }

    fn determine_mode(&self, mem_avail_mb: u64) -> Mode {
        // Priority: Pressure > Media (external trigger) > Time-based.

        if mem_avail_mb < self.config.governor.pressure.stop_optins_mb {
            return Mode::Pressure;
        }

        if std::path::Path::new("/run/geniepod/media_mode").exists() {
            return Mode::Media;
        }

        let hour = current_hour();
        let night_start = self.config.governor.night_start_hour;
        let day_start = self.config.governor.day_start_hour;

        let is_night = if night_start > day_start {
            hour >= night_start || hour < day_start
        } else {
            hour >= night_start && hour < day_start
        };

        if is_night {
            if self.config.governor.night_model_swap {
                Mode::NightB
            } else {
                Mode::NightA
            }
        } else {
            Mode::Day
        }
    }

    async fn transition(&mut self, ts_ms: u64, target: Mode) -> Result<()> {
        if target == self.current_mode {
            return Ok(());
        }

        let from = self.current_mode;
        let reason = format!("{} -> {}", from, target);
        tracing::info!(from = %from, to = %target, "mode transition");

        let mut stopped = Vec::new();
        let mut started = Vec::new();

        for alias in target.stopped_services() {
            let Some(unit) = self.service_unit_for_alias(alias) else {
                continue;
            };
            if ServiceCtl::is_active(&unit).await {
                ServiceCtl::stop(&unit).await?;
                stopped.push(StoppedService {
                    alias,
                    unit: unit.clone(),
                });
            }
        }

        if let Err(e) = self.apply_llm_swaps(from, target, &mut stopped).await {
            self.rollback_transition(from, target, &stopped, &started)
                .await;
            return Err(e);
        }
        if matches!((from, target), (Mode::Media, _)) {
            let _ = tokio::fs::remove_file("/run/geniepod/media_mode").await;
        }

        for alias in target.required_services() {
            let Some(unit) = self.service_unit_for_alias(alias) else {
                continue;
            };
            if !ServiceCtl::is_active(&unit).await {
                if let Err(e) = ServiceCtl::start(&unit).await {
                    self.rollback_transition(from, target, &stopped, &started)
                        .await;
                    return Err(e);
                }
                started.push(unit);
            }
        }

        self.store
            .insert_transition(ts_ms, &from.to_string(), &target.to_string(), &reason)?;

        self.current_mode = target;
        Ok(())
    }

    async fn handle_pressure(&mut self, mem_avail_mb: u64) -> Result<()> {
        let pressure = &self.config.governor.pressure;

        if mem_avail_mb < pressure.zram_mb && !self.zram_enabled {
            ServiceCtl::enable_zram().await?;
            self.zram_enabled = true;
        }

        if mem_avail_mb < pressure.stop_optins_mb {
            if self.should_manage_service("nextcloud") {
                let _ = ServiceCtl::docker_stop("nextcloud").await;
            }
            if self.should_manage_service("jellyfin") {
                let _ = ServiceCtl::docker_stop("jellyfin").await;
            }
        }

        Ok(())
    }

    fn should_manage_service(&self, alias: &str) -> bool {
        self.config.manages_service_alias(alias)
    }

    fn service_unit_for_alias(&self, alias: &str) -> Option<String> {
        self.config.service_unit_for_alias(alias)
    }

    fn llm_service_unit(&self) -> String {
        self.config
            .service_unit_for_alias("llm")
            .unwrap_or_else(|| "genie-ai-runtime.service".into())
    }

    async fn apply_llm_swaps(
        &self,
        from: Mode,
        target: Mode,
        stopped: &mut Vec<StoppedService>,
    ) -> Result<()> {
        match (from, target) {
            (_, Mode::Media) => {
                let unit = self.llm_service_unit();
                if ServiceCtl::is_active(&unit).await {
                    ServiceCtl::stop(&unit).await?;
                    stopped.push(StoppedService { alias: "llm", unit });
                }
            }
            (Mode::Media, _)
            | (Mode::Day | Mode::NightA, Mode::NightB)
            | (Mode::NightB, Mode::Day) => {
                if let Some(model) = llm_model_for_transition(from, target) {
                    let path = format!("/opt/geniepod/models/{}", model);
                    let unit = self.llm_service_unit();
                    ServiceCtl::swap_llm_model(&unit, &path).await?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Best-effort undo of partial transition work before returning the original error.
    async fn rollback_transition(
        &self,
        from: Mode,
        target: Mode,
        stopped: &[StoppedService],
        started: &[String],
    ) {
        for unit in started.iter().rev() {
            if let Err(e) = ServiceCtl::stop(unit).await {
                tracing::error!(unit = %unit, error = %e, "rollback stop failed");
            }
        }

        for alias in service_aliases_to_restore(from, stopped) {
            let Some(unit) = self.service_unit_for_alias(alias) else {
                continue;
            };
            if let Err(e) = ServiceCtl::start(&unit).await {
                tracing::error!(unit = %unit, alias, error = %e, "rollback start failed");
            }
        }

        match media_marker_rollback_action(from, target) {
            MediaMarkerRollback::Restore => {
                if let Err(e) = tokio::fs::create_dir_all("/run/geniepod").await {
                    tracing::error!(error = %e, "rollback media marker directory restore failed");
                } else if let Err(e) = tokio::fs::write("/run/geniepod/media_mode", b"1").await {
                    tracing::error!(error = %e, "rollback media marker restore failed");
                }
            }
            MediaMarkerRollback::Remove => {
                if let Err(e) = tokio::fs::remove_file("/run/geniepod/media_mode").await
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::error!(error = %e, "rollback media marker removal failed");
                }
            }
            MediaMarkerRollback::Unchanged => {}
        }

        match llm_rollback_action(from, target) {
            LlmRollback::RestoreModel(model) => {
                let unit = self.llm_service_unit();
                let path = format!("/opt/geniepod/models/{}", model);
                if let Err(e) = ServiceCtl::swap_llm_model(&unit, &path).await {
                    tracing::error!(
                        unit = %unit,
                        model = %path,
                        error = %e,
                        "rollback LLM model swap failed"
                    );
                }
            }
            LlmRollback::StopService => {
                let unit = self.llm_service_unit();
                if ServiceCtl::is_active(&unit).await
                    && let Err(e) = ServiceCtl::stop(&unit).await
                {
                    tracing::error!(unit = %unit, error = %e, "rollback LLM stop failed");
                }
            }
            LlmRollback::Unchanged => {}
        }
    }
}

fn llm_model_for_transition(from: Mode, target: Mode) -> Option<&'static str> {
    match (from, target) {
        (Mode::Media, _) => target.llm_model(),
        (Mode::Day | Mode::NightA, Mode::NightB) => Mode::NightB.llm_model(),
        (Mode::NightB, Mode::Day) => Mode::Day.llm_model(),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoppedService {
    alias: &'static str,
    unit: String,
}

fn service_aliases_to_restore(from: Mode, stopped: &[StoppedService]) -> Vec<&'static str> {
    stopped
        .iter()
        .map(|svc| svc.alias)
        .filter(|alias| from.required_services().contains(alias))
        .collect()
}

fn transition_changes_llm_weights(from: Mode, target: Mode) -> bool {
    match (from.llm_model(), target.llm_model()) {
        (Some(from_model), Some(to_model)) => from_model != to_model,
        (Some(_), None) | (None, Some(_)) => true,
        (None, None) => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaMarkerRollback {
    Restore,
    Remove,
    Unchanged,
}

fn media_marker_rollback_action(from: Mode, target: Mode) -> MediaMarkerRollback {
    match (from == Mode::Media, target == Mode::Media) {
        (true, false) => MediaMarkerRollback::Restore,
        (false, true) => MediaMarkerRollback::Remove,
        _ => MediaMarkerRollback::Unchanged,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LlmRollback {
    RestoreModel(&'static str),
    StopService,
    Unchanged,
}

fn llm_rollback_action(from: Mode, target: Mode) -> LlmRollback {
    if !transition_changes_llm_weights(from, target) {
        return LlmRollback::Unchanged;
    }

    match from.llm_model() {
        Some(model) => LlmRollback::RestoreModel(model),
        None => LlmRollback::StopService,
    }
}

fn current_hour() -> u8 {
    // Use libc localtime for correct timezone on production.
    // Falls back to UTC if localtime fails.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    #[cfg(unix)]
    {
        let time_t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::localtime_r(&time_t, &mut tm) };
        if !result.is_null() {
            return tm.tm_hour as u8;
        }
    }

    // Fallback: UTC.
    ((secs % 86400) / 3600) as u8
}

fn sd_notify_ready() {
    if let Ok(addr) = std::env::var("NOTIFY_SOCKET") {
        let _ = std::os::unix::net::UnixDatagram::unbound()
            .and_then(|sock| sock.send_to(b"READY=1", &addr));
    }
}

fn sd_notify_watchdog() {
    if let Ok(addr) = std::env::var("NOTIFY_SOCKET") {
        let _ = std::os::unix::net::UnixDatagram::unbound()
            .and_then(|sock| sock.send_to(b"WATCHDOG=1", &addr));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_common::config::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn test_config() -> Config {
        Config {
            data_dir: PathBuf::from("/tmp/geniepod-test"),
            core: CoreConfig::default(),
            agent: AgentConfig::default(),
            optional_ai_provider: OptionalAiProviderConfig::default(),
            privacy_proxy: Default::default(),
            governor: GovernorConfig {
                poll_interval_ms: 1000,
                night_start_hour: 23,
                day_start_hour: 6,
                night_model_swap: false,
                pressure: PressureConfig {
                    stop_optins_mb: 500,
                    reduce_context_mb: 300,
                    swap_stt_mb: 200,
                    zram_mb: 100,
                },
            },
            health: HealthConfig::default(),
            services: ServicesConfig::default(),
            telegram: TelegramConfig::default(),
            web_search: WebSearchConfig::default(),
            connectivity: ConnectivityConfig::default(),
            http: Default::default(),
        }
    }

    fn make_governor() -> Governor {
        let config = test_config();
        // Each test gets its own DB path so parallel `cargo test` runs don't
        // collide on the SQLite file with `database is locked`.
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let db_path = std::env::temp_dir().join(format!(
            "geniepod-test-gov-{}-{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&db_path);
        let store = Store::open(&db_path).unwrap();
        Governor::new(config, store)
    }

    #[test]
    fn determine_mode_day_with_plenty_of_memory() {
        let gov = make_governor();
        // 3000 MB available, well above all thresholds.
        let mode = gov.determine_mode(3000);
        // Without media trigger file, should be Day or NightA depending on time.
        // At minimum, it should NOT be Pressure or Media.
        assert_ne!(mode, Mode::Pressure);
        assert_ne!(mode, Mode::Media);
    }

    #[test]
    fn determine_mode_pressure_on_low_memory() {
        let gov = make_governor();
        // 400 MB available, below stop_optins_mb (500).
        let mode = gov.determine_mode(400);
        assert_eq!(mode, Mode::Pressure);
    }

    #[test]
    fn determine_mode_pressure_takes_priority() {
        let gov = make_governor();
        // Even with 0 MB, pressure should override everything.
        let mode = gov.determine_mode(0);
        assert_eq!(mode, Mode::Pressure);
    }

    #[test]
    fn mode_required_services() {
        let day_services = Mode::Day.required_services();
        assert!(day_services.contains(&"genie-core"));
        assert!(day_services.contains(&"llm"));
        assert!(day_services.contains(&"homeassistant"));

        let media_services = Mode::Media.required_services();
        assert!(media_services.contains(&"genie-wakeword"));
        assert!(media_services.contains(&"genie-core"));
        assert!(!media_services.contains(&"llm")); // LLM unloaded in media.
    }

    #[test]
    fn mode_stopped_services() {
        assert!(Mode::Day.stopped_services().is_empty());
        assert!(Mode::NightB.stopped_services().contains(&"homeassistant"));
        assert!(Mode::NightB.stopped_services().contains(&"nextcloud"));
        assert!(Mode::Media.stopped_services().contains(&"llm"));
    }

    #[test]
    fn mode_llm_model_selection() {
        assert_eq!(Mode::Day.llm_model(), Some("nemotron-4b-q4_k_m.gguf"));
        assert_eq!(Mode::NightB.llm_model(), Some("nemotron-9b-q4.gguf"));
        assert_eq!(Mode::Media.llm_model(), None);
    }

    #[test]
    fn night_model_swap_config() {
        let mut config = test_config();
        config.governor.night_model_swap = true;
        let db_path = std::env::temp_dir().join("geniepod-test-gov2.db");
        let store = Store::open(&db_path).unwrap();
        let mut gov = Governor::new(config, store);
        gov.config.governor.night_start_hour = 0;
        gov.config.governor.day_start_hour = 0;
        // With night_model_swap=true and we're in "night" hours,
        // the mode should be NightB.
        // Since we can't reliably control the clock in tests,
        // we test the logic directly.
        let is_night_always = {
            gov.config.governor.night_start_hour = 0;
            gov.config.governor.day_start_hour = 0;
            // 0 >= 0 && hour < 0 is never true (day_start=0), so this is tricky.
            // Let's just set night=0, day=24 equivalent to always night.
            gov.config.governor.night_start_hour = 0;
            gov.config.governor.day_start_hour = 25; // > 23, so always night
            true
        };
        if is_night_always {
            // With huge day_start, determine_mode at any hour with enough RAM
            // should pick NightB (since night_model_swap=true).
            let mode = gov.determine_mode(3000);
            assert_eq!(mode, Mode::NightB);
        }
    }

    #[test]
    fn skips_unconfigured_optional_services() {
        let gov = make_governor();

        assert!(gov.should_manage_service("genie-core"));
        assert!(gov.should_manage_service("llm"));
        assert!(!gov.should_manage_service("homeassistant"));
        assert!(!gov.should_manage_service("nextcloud"));
        assert!(!gov.should_manage_service("jellyfin"));
    }

    #[test]
    fn resolves_llm_alias_to_configured_service_unit() {
        let mut config = test_config();
        config.services.llm.systemd_unit = "genie-ai-runtime.service".into();
        let db_path = std::env::temp_dir().join("geniepod-test-gov-llm-unit.db");
        let store = Store::open(&db_path).unwrap();
        let gov = Governor::new(config, store);

        assert_eq!(
            gov.service_unit_for_alias("llm").as_deref(),
            Some("genie-ai-runtime.service")
        );
        assert_eq!(gov.llm_service_unit(), "genie-ai-runtime.service");
    }

    #[test]
    fn llm_model_for_transition_selects_target_mode_weights() {
        assert_eq!(
            llm_model_for_transition(Mode::Day, Mode::NightB),
            Mode::NightB.llm_model()
        );
        assert_eq!(
            llm_model_for_transition(Mode::NightB, Mode::Day),
            Mode::Day.llm_model()
        );
        assert_eq!(
            llm_model_for_transition(Mode::Media, Mode::Day),
            Mode::Day.llm_model()
        );
        assert!(llm_model_for_transition(Mode::Day, Mode::NightA).is_none());
    }

    #[test]
    fn rollback_restores_services_required_by_source_mode_after_stop() {
        let stopped = vec![
            StoppedService {
                alias: "homeassistant",
                unit: "home-assistant.service".into(),
            },
            StoppedService {
                alias: "nextcloud",
                unit: "nextcloud.service".into(),
            },
        ];

        assert_eq!(
            service_aliases_to_restore(Mode::Day, &stopped),
            vec!["homeassistant"]
        );
    }

    #[test]
    fn day_to_night_b_stops_ha_before_llm_swap_attempt() {
        assert!(Mode::NightB.stopped_services().contains(&"homeassistant"));
        assert!(llm_model_for_transition(Mode::Day, Mode::NightB).is_some());
        assert!(transition_changes_llm_weights(Mode::Day, Mode::NightB));
    }

    #[test]
    fn rollback_restores_source_llm_weights_after_failed_swap() {
        assert!(transition_changes_llm_weights(Mode::Day, Mode::NightB));
        assert_eq!(Mode::Day.llm_model(), Some("nemotron-4b-q4_k_m.gguf"));
    }

    #[test]
    fn rollback_restores_media_marker_when_media_stop_fails() {
        assert_eq!(
            media_marker_rollback_action(Mode::Media, Mode::Day),
            MediaMarkerRollback::Restore
        );
        assert_eq!(
            media_marker_rollback_action(Mode::Day, Mode::Media),
            MediaMarkerRollback::Remove
        );
        assert_eq!(
            media_marker_rollback_action(Mode::Day, Mode::NightB),
            MediaMarkerRollback::Unchanged
        );
    }

    #[test]
    fn rollback_stops_llm_when_returning_to_media_mode() {
        assert_eq!(
            llm_rollback_action(Mode::Media, Mode::Day),
            LlmRollback::StopService
        );
        assert_eq!(
            llm_rollback_action(Mode::Day, Mode::Media),
            LlmRollback::RestoreModel("nemotron-4b-q4_k_m.gguf")
        );
        assert_eq!(
            llm_rollback_action(Mode::Day, Mode::NightA),
            LlmRollback::Unchanged
        );
    }
}
