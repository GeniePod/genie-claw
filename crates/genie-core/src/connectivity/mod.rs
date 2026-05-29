use anyhow::Result;
use async_trait::async_trait;
use genie_common::config::{ConnectivityConfig, ConnectivityTransport};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Connectivity subsystem boundary.
///
/// GenieClaw should treat external Thread/Matter radio hardware as a
/// coprocessor behind a small interface, not as ad-hoc transport code mixed
/// into chat, tools, or prompt logic.
///
/// The current target is an ESP32-C6 connected to Jetson over UART for
/// Thread/Matter sidecar work.
///
/// OS-level networking such as `esp-hosted-ng` belongs in the platform/OS
/// layer, not in the core assistant runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectivityState {
    Disabled,
    Starting,
    Ready,
    Degraded,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectivityCapability {
    WifiSta,
    WifiAp,
    Ble,
    Thread,
    Matter,
    Zigbee,
    IpBridge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectivityHealth {
    pub state: ConnectivityState,
    pub transport: String,
    pub device: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectivityFrame {
    pub channel: String,
    pub payload: Vec<u8>,
}

#[async_trait]
pub trait ConnectivityController: Send + Sync {
    async fn health(&self) -> ConnectivityHealth;
    async fn capabilities(&self) -> Vec<ConnectivityCapability>;
    async fn send(&self, frame: ConnectivityFrame) -> Result<()>;
}

/// Minimal placeholder controller used until a real transport implementation is
/// wired in. It provides a stable boundary for the rest of the codebase.
pub struct NullConnectivityController {
    health: ConnectivityHealth,
    capabilities: Vec<ConnectivityCapability>,
}

impl NullConnectivityController {
    pub fn from_config(config: &ConnectivityConfig) -> Self {
        let (state, message, capabilities) = match (config.enabled, config.transport) {
            (false, _) => (
                ConnectivityState::Disabled,
                "connectivity disabled in config".to_string(),
                Vec::new(),
            ),
            (true, ConnectivityTransport::None) => (
                ConnectivityState::Disabled,
                "connectivity enabled but no transport configured".to_string(),
                Vec::new(),
            ),
            (true, ConnectivityTransport::Esp32c6Uart) => {
                let capabilities = vec![
                    ConnectivityCapability::Thread,
                    ConnectivityCapability::Matter,
                ];
                match classify_uart_path(&config.esp32c6_uart.device_path) {
                    UartPathState::Missing => (
                        ConnectivityState::Offline,
                        format!(
                            "ESP32-C6 Thread/Matter UART sidecar configured on {} but the serial device is not present",
                            config.esp32c6_uart.device_path
                        ),
                        capabilities,
                    ),
                    UartPathState::Invalid(reason) => (
                        ConnectivityState::Degraded,
                        format!(
                            "ESP32-C6 Thread/Matter UART sidecar configured on {} but {}",
                            config.esp32c6_uart.device_path, reason
                        ),
                        capabilities,
                    ),
                    UartPathState::LikelyUartDevice => (
                        ConnectivityState::Degraded,
                        format!(
                            "ESP32-C6 Thread/Matter UART sidecar configured on {} and the UART device is present, but the UART controller is not initialized yet",
                            config.esp32c6_uart.device_path
                        ),
                        capabilities,
                    ),
                }
            }
        };

        Self {
            health: ConnectivityHealth {
                state,
                transport: transport_name(config.transport).to_string(),
                device: config.device.clone(),
                message,
            },
            capabilities,
        }
    }
}

#[async_trait]
impl ConnectivityController for NullConnectivityController {
    async fn health(&self) -> ConnectivityHealth {
        self.health.clone()
    }

    async fn capabilities(&self) -> Vec<ConnectivityCapability> {
        self.capabilities.clone()
    }

    async fn send(&self, _frame: ConnectivityFrame) -> Result<()> {
        anyhow::bail!("connectivity transport not initialized")
    }
}

pub fn transport_name(transport: ConnectivityTransport) -> &'static str {
    match transport {
        ConnectivityTransport::None => "none",
        ConnectivityTransport::Esp32c6Uart => "esp32c6_uart",
    }
}

enum UartPathState {
    Missing,
    Invalid(&'static str),
    LikelyUartDevice,
}

fn classify_uart_path(path: &str) -> UartPathState {
    let path = Path::new(path);
    let Ok(metadata) = std::fs::metadata(path) else {
        return UartPathState::Missing;
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        if !metadata.file_type().is_char_device() {
            return UartPathState::Invalid("the configured path is not a character device");
        }
    }

    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return UartPathState::Invalid("the configured path is not a valid tty device path");
    };

    if name.starts_with("tty") {
        UartPathState::LikelyUartDevice
    } else {
        UartPathState::Invalid("the configured path does not look like a tty device")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_config_reports_disabled_health() {
        let controller = NullConnectivityController::from_config(&ConnectivityConfig::default());
        let health = controller.health().await;
        assert_eq!(health.state, ConnectivityState::Disabled);
        assert_eq!(health.transport, "none");
    }

    #[tokio::test]
    async fn esp32_uart_config_reports_offline_when_serial_device_is_missing() {
        let mut config = ConnectivityConfig {
            enabled: true,
            transport: ConnectivityTransport::Esp32c6Uart,
            ..ConnectivityConfig::default()
        };
        config.esp32c6_uart.device_path = "/dev/ttyFAKE0".into();

        let controller = NullConnectivityController::from_config(&config);
        let health = controller.health().await;
        assert_eq!(health.state, ConnectivityState::Offline);
        assert!(health.message.contains("/dev/ttyFAKE0"));
        assert_eq!(
            controller.capabilities().await,
            vec![
                ConnectivityCapability::Thread,
                ConnectivityCapability::Matter
            ]
        );
    }

    #[tokio::test]
    async fn esp32_uart_config_reports_degraded_when_serial_device_exists() {
        let temp_path = std::env::temp_dir().join("genie-core-connectivity-uart.sock");
        std::fs::write(&temp_path, b"placeholder").unwrap();

        let mut config = ConnectivityConfig {
            enabled: true,
            transport: ConnectivityTransport::Esp32c6Uart,
            ..ConnectivityConfig::default()
        };
        config.esp32c6_uart.device_path = temp_path.to_string_lossy().to_string();

        let controller = NullConnectivityController::from_config(&config);
        let health = controller.health().await;
        assert_eq!(health.state, ConnectivityState::Degraded);
        assert!(health.message.contains("not a character device"));

        let _ = std::fs::remove_file(temp_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_tty_character_device_is_not_treated_as_uart() {
        let mut config = ConnectivityConfig {
            enabled: true,
            transport: ConnectivityTransport::Esp32c6Uart,
            ..ConnectivityConfig::default()
        };
        config.esp32c6_uart.device_path = "/dev/null".into();

        let controller = NullConnectivityController::from_config(&config);
        let health = controller.health().await;
        assert_eq!(health.state, ConnectivityState::Degraded);
        assert!(health.message.contains("does not look like a tty device"));
    }
}
