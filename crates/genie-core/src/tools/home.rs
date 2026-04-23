use crate::ha::{HomeAction, HomeActionKind, HomeAutomationProvider, assess_home_action};
use anyhow::Result;

/// Execute a structured home control action via the HA provider.
pub async fn control(
    home: &dyn HomeAutomationProvider,
    target_query: &str,
    action: &str,
    value: Option<f64>,
) -> Result<String> {
    let action_kind = parse_action(action)?;
    let target = home.resolve_target(target_query, Some(action_kind)).await?;
    let action = HomeAction {
        kind: action_kind,
        target,
        value,
    };
    let policy = assess_home_action(&action);
    if !policy.allowed {
        if policy.requires_confirmation {
            anyhow::bail!(
                "Confirmation required before I can do that: {}. Please confirm from the local dashboard or use a safer routine.",
                policy.reason
            );
        }
        anyhow::bail!("Home action blocked by local policy: {}", policy.reason);
    }

    let result = home.execute(action).await?;
    Ok(result.spoken_summary)
}

/// Query entity or room status via the HA provider.
pub async fn status(home: &dyn HomeAutomationProvider, target_query: &str) -> Result<String> {
    let target = home.resolve_target(target_query, None).await?;
    let state = home.get_state(&target).await?;
    Ok(state.spoken_summary)
}

fn parse_action(action: &str) -> Result<HomeActionKind> {
    let parsed = match action {
        "turn_on" => HomeActionKind::TurnOn,
        "turn_off" => HomeActionKind::TurnOff,
        "toggle" => HomeActionKind::Toggle,
        "set_brightness" => HomeActionKind::SetBrightness,
        "set_temperature" => HomeActionKind::SetTemperature,
        "open" => HomeActionKind::Open,
        "close" => HomeActionKind::Close,
        "lock" => HomeActionKind::Lock,
        "unlock" => HomeActionKind::Unlock,
        "activate" | "activate_scene" => HomeActionKind::Activate,
        other => anyhow::bail!("unknown home action: {}", other),
    };
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::{
        ActionResult, DeviceRef, HomeGraph, HomeState, HomeTarget, HomeTargetKind,
        IntegrationHealth, SceneRef,
    };

    #[test]
    fn parse_activate_alias() {
        assert_eq!(
            parse_action("activate_scene").unwrap(),
            HomeActionKind::Activate
        );
    }

    #[test]
    fn parse_open_and_close() {
        assert_eq!(parse_action("open").unwrap(), HomeActionKind::Open);
        assert_eq!(parse_action("close").unwrap(), HomeActionKind::Close);
    }

    struct StubHome {
        domain: &'static str,
        voice_safe: bool,
    }

    #[async_trait::async_trait]
    impl HomeAutomationProvider for StubHome {
        async fn health(&self) -> IntegrationHealth {
            IntegrationHealth {
                connected: true,
                cached_graph: false,
                message: "ok".into(),
            }
        }

        async fn sync_structure(&self) -> Result<HomeGraph> {
            anyhow::bail!("unused")
        }

        async fn resolve_target(
            &self,
            query: &str,
            _action_hint: Option<HomeActionKind>,
        ) -> Result<HomeTarget> {
            Ok(HomeTarget {
                kind: HomeTargetKind::Entity,
                query: query.into(),
                display_name: query.into(),
                entity_ids: vec![format!("{}.test", self.domain)],
                domain: Some(self.domain.into()),
                area: Some("Living Room".into()),
                confidence: 0.9,
                voice_safe: self.voice_safe,
            })
        }

        async fn get_state(&self, _target: &HomeTarget) -> Result<HomeState> {
            Ok(HomeState {
                target_name: "Living room lamp".into(),
                domain: Some(self.domain.into()),
                area: Some("Living Room".into()),
                entities: Vec::new(),
                available: true,
                spoken_summary: "Living room lamp is on".into(),
            })
        }

        async fn execute(&self, action: crate::ha::HomeAction) -> Result<ActionResult> {
            Ok(ActionResult {
                success: true,
                spoken_summary: format!("Executed {:?}", action.kind),
                affected_targets: vec![action.target.display_name],
                state_snapshot: None,
                confidence: Some(0.9),
            })
        }

        async fn list_scenes(&self, _room: Option<&str>) -> Result<Vec<SceneRef>> {
            Ok(Vec::new())
        }

        async fn list_devices(&self, _room: Option<&str>) -> Result<Vec<DeviceRef>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn control_allows_safe_light_action() {
        let home = StubHome {
            domain: "light",
            voice_safe: true,
        };

        let result = control(&home, "Living room lamp", "turn_on", None)
            .await
            .unwrap();

        assert!(result.contains("TurnOn"));
    }

    #[tokio::test]
    async fn control_blocks_lock_without_confirmation_flow() {
        let home = StubHome {
            domain: "lock",
            voice_safe: false,
        };

        let err = control(&home, "Front door", "unlock", None)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("Confirmation required"));
    }
}
