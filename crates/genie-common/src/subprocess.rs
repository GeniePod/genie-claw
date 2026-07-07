//! Bounded subprocess helpers shared across GeniePod services.
//!
//! Issue #617: several call sites shell out with no deadline. This module
//! centralizes the `tokio::time::timeout` + `kill_on_drop` pattern so a hung
//! child is reaped instead of wedging the daemon.

use std::process::Output;
use std::time::Duration;

use tokio::process::Command;

/// Why a subprocess invocation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubprocessError {
    /// The whole invocation exceeded the caller's deadline.
    Timeout,
    /// Spawning or waiting on the child failed.
    Io(String),
}

impl std::fmt::Display for SubprocessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubprocessError::Timeout => write!(f, "subprocess timed out"),
            SubprocessError::Io(msg) => write!(f, "subprocess io error: {msg}"),
        }
    }
}

impl std::error::Error for SubprocessError {}

/// Run `cmd` to completion under `timeout`, killing the child if the deadline
/// elapses (`kill_on_drop` ensures the process is reaped on timeout/cancel).
pub async fn output_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> Result<Output, SubprocessError> {
    cmd.kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(SubprocessError::Io(e.to_string())),
        Err(_) => Err(SubprocessError::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn output_with_timeout_returns_success_for_fast_command() {
        #[cfg(unix)]
        let output = output_with_timeout(Command::new("true"), Duration::from_secs(5))
            .await
            .expect("fast command should succeed");
        #[cfg(windows)]
        let output = {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", "echo", "ok"]);
            output_with_timeout(cmd, Duration::from_secs(5))
                .await
                .expect("fast command should succeed")
        };
        assert!(output.status.success());
    }

    #[tokio::test]
    async fn output_with_timeout_kills_slow_command() {
        let start = std::time::Instant::now();
        #[cfg(unix)]
        let err = output_with_timeout(Command::new("sleep").arg("60"), Duration::from_millis(500))
            .await
            .expect_err("slow command should time out");
        #[cfg(windows)]
        let err = {
            let mut cmd = Command::new("powershell");
            cmd.args(["-Command", "Start-Sleep", "-Seconds", "60"]);
            output_with_timeout(cmd, Duration::from_millis(500))
                .await
                .expect_err("slow command should time out")
        };
        assert_eq!(err, SubprocessError::Timeout);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout should return promptly"
        );
    }
}
