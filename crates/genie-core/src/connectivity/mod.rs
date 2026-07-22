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

    let Some(name) = resolved_device_name(path) else {
        return UartPathState::Invalid("the configured path is not a valid tty device path");
    };

    if name.starts_with("tty") {
        UartPathState::LikelyUartDevice
    } else {
        UartPathState::Invalid("the configured path does not look like a tty device")
    }
}

/// File name of the real device node behind `path`.
///
/// udev exposes stable aliases — `/dev/serial/by-id/...`, or a custom
/// `SYMLINK+=` rule — as symlinks to the actual `ttyUSB*`/`ttyACM*`/`ttyTHS*`
/// node, and they are the usual way to name a USB serial device because the
/// `ttyUSB*` numbering is not stable across reboots. `fs::metadata` above
/// already follows the link for the char-device check, so the name check has to
/// resolve it too; judging the unresolved alias reported a real UART as "not a
/// tty device". Falls back to the given path when it cannot be resolved.
fn resolved_device_name(path: &Path) -> Option<String> {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    resolved
        .file_name()
        .and_then(|value| value.to_str())
        .map(|name| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn resolved_device_name_follows_a_udev_style_symlink() {
        // A udev alias is a symlink to the real ttyUSB*/ttyACM* node. Since the
        // char-device check follows the link, the name check must too — judging
        // the alias name reported a real UART as "not a tty device".
        let dir = std::env::temp_dir().join(format!("genie-uart-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let real = dir.join("ttyUSB0");
        std::fs::write(&real, b"").unwrap();
        let alias = dir.join("usb-Espressif_USB_JTAG_serial_debug_unit-if00");
        let _ = std::fs::remove_file(&alias);
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        assert_eq!(resolved_device_name(&alias).as_deref(), Some("ttyUSB0"));
        // A plain (non-symlink) path still reports its own name.
        assert_eq!(resolved_device_name(&real).as_deref(), Some("ttyUSB0"));

        let _ = std::fs::remove_dir_all(&dir);
    }

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
