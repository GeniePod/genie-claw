use super::actuation::RequestOrigin;
use crate::ha::{
    HomeAction, HomeActionKind, HomeAutomationProvider, HomeTargetKind, assess_home_action,
    assess_runtime_home_action,
};
use anyhow::Result;
use genie_common::config::ActuationSafetyConfig;

#[derive(Debug)]
pub enum ControlOutcome {
    Executed(String, Option<f32>),
    ConfirmationRequired {
        reason: String,
        target_display: String,
    },
}

/// Execute a structured home control action via the HA provider.
pub async fn control(
    home: &dyn HomeAutomationProvider,
    target_query: &str,
    action: &str,
    value: Option<f64>,
    safety_config: &ActuationSafetyConfig,
    request_origin: RequestOrigin,
    confirmed: bool,
) -> Result<ControlOutcome> {
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
            // A confirmable policy is satisfied once the caller has supplied a
            // confirmation. Without `confirmed` we would re-issue a fresh
            // confirmation request on every retry, never executing the action.
            if !confirmed {
                return Ok(ControlOutcome::ConfirmationRequired {
                    reason: policy.reason,
                    target_display: action.target.display_name,
                });
            }
            // confirmed → fall through to the runtime safety gate
        } else {
            // hard deny — not a confirmable action
            anyhow::bail!("Home action blocked by local policy: {}", policy.reason);
        }
    }

    if !confirmed
        && matches!(
            request_origin,
            RequestOrigin::Voice | RequestOrigin::Telegram
        )
        && !matches!(policy.risk, crate::ha::ActionRisk::Low)
    {
        return Ok(ControlOutcome::ConfirmationRequired {
            reason: format!(
                "{} from {} requires local confirmation",
                action.target.display_name,
                match request_origin {
                    RequestOrigin::Voice => "voice",
                    RequestOrigin::Telegram => "telegram",
                    _ => "this origin",
                }
            ),
            target_display: action.target.display_name,
        });
    }

    if !confirmed
        && matches!(request_origin, RequestOrigin::Api)
        && matches!(policy.risk, crate::ha::ActionRisk::High)
    {
        return Ok(ControlOutcome::ConfirmationRequired {
            reason: format!(
                "{} from the API requires local confirmation",
                action.target.display_name
            ),
            target_display: action.target.display_name,
        });
    }

    let health = home.health().await;
    let current_state = match action.target.kind {
        HomeTargetKind::Scene | HomeTargetKind::Script => None,
        _ => Some(home.get_state(&action.target).await?),
    };
    let runtime = assess_runtime_home_action(
        &action,
        &policy,
        &health,
        current_state.as_ref(),
        safety_config,
        request_origin,
        confirmed,
    );
    if !runtime.allowed {
        tracing::warn!(
            target = %action.target.display_name,
            kind = ?action.kind,
            confidence = action.target.confidence,
            risk = ?policy.risk,
            reason = %runtime.reason,
            "blocked home action by runtime safety gate"
        );
        anyhow::bail!("Home action blocked by runtime safety: {}", runtime.reason);
    }

    tracing::info!(
        target = %action.target.display_name,
        kind = ?action.kind,
        confidence = action.target.confidence,
        risk = ?policy.risk,
        "home action passed runtime safety gate"
    );

    let result = home.execute(action).await?;
    Ok(ControlOutcome::Executed(
        result.spoken_summary,
        result.confidence,
    ))
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

        let result = control(
            &home,
            "Living room lamp",
            "turn_on",
            None,
            &ActuationSafetyConfig::default(),
            RequestOrigin::Dashboard,
            false,
        )
        .await
        .unwrap();

        match result {
            ControlOutcome::Executed(output, _) => assert!(output.contains("TurnOn")),
            ControlOutcome::ConfirmationRequired { .. } => panic!("expected execution"),
        }
    }

    #[tokio::test]
    async fn control_blocks_lock_without_confirmation_flow() {
        let home = StubHome {
            domain: "lock",
            voice_safe: false,
        };

        let err = control(
            &home,
            "Front door",
            "unlock",
            None,
            &ActuationSafetyConfig::default(),
            RequestOrigin::Dashboard,
            false,
        )
        .await
        .unwrap();

        match err {
            ControlOutcome::ConfirmationRequired { reason, .. } => {
                assert!(
                    reason.contains("requires confirmation")
                        || reason.contains("not marked voice-safe")
                );
            }
            ControlOutcome::Executed(_, _) => panic!("expected confirmation"),
        }
    }

    #[tokio::test]
    async fn control_executes_sensitive_action_once_confirmed() {
        // Regression for #138: a satisfied confirmation must let a sensitive
        // action proceed instead of looping back into ConfirmationRequired.
        let home = StubHome {
            domain: "lock",
            voice_safe: false,
        };

        let result = control(
            &home,
            "Front door",
            "unlock",
            None,
            &ActuationSafetyConfig::default(),
            RequestOrigin::Telegram,
            true,
        )
        .await
        .unwrap();

        match result {
            ControlOutcome::Executed(output, _) => assert!(output.contains("Unlock")),
            ControlOutcome::ConfirmationRequired { .. } => {
                panic!("confirmed sensitive action should execute, not re-prompt")
            }
        }
    }

    #[tokio::test]
    async fn control_confirmation_origin_executes_sensitive_action_when_confirmed() {
        // Regression for #138 / #162: `confirm_pending_home_action` re-dispatches
        // with `RequestOrigin::Confirmation` and `confirmed: true` — the policy
        // gate must not mint another ConfirmationRequired token.
        let home = StubHome {
            domain: "lock",
            voice_safe: false,
        };

        let result = control(
            &home,
            "Front door",
            "unlock",
            None,
            &ActuationSafetyConfig::default(),
            RequestOrigin::Confirmation,
            true,
        )
        .await
        .unwrap();

        match result {
            ControlOutcome::Executed(output, _) => assert!(output.contains("Unlock")),
            ControlOutcome::ConfirmationRequired { .. } => {
                panic!("Confirmation origin with confirmed=true must execute, not re-prompt")
            }
        }
    }

    #[tokio::test]
    async fn control_confirmation_origin_still_requires_confirm_when_unconfirmed() {
        let home = StubHome {
            domain: "lock",
            voice_safe: false,
        };

        let result = control(
            &home,
            "Front door",
            "unlock",
            None,
            &ActuationSafetyConfig::default(),
            RequestOrigin::Confirmation,
            false,
        )
        .await
        .unwrap();

        match result {
            ControlOutcome::ConfirmationRequired { .. } => {}
            ControlOutcome::Executed(_, _) => {
                panic!("unconfirmed sensitive action must not execute at policy gate")
            }
        }
    }

    #[tokio::test]
    async fn control_blocks_low_confidence_runtime_target() {
        struct LowConfidenceHome;

        #[async_trait::async_trait]
        impl HomeAutomationProvider for LowConfidenceHome {
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
                    entity_ids: vec!["light.test".into()],
                    domain: Some("light".into()),
                    area: Some("Living Room".into()),
                    confidence: 0.55,
                    voice_safe: true,
                })
            }

            async fn get_state(&self, _target: &HomeTarget) -> Result<HomeState> {
                Ok(HomeState {
                    target_name: "Living room lamp".into(),
                    domain: Some("light".into()),
                    area: Some("Living Room".into()),
                    entities: Vec::new(),
                    available: true,
                    spoken_summary: "Living room lamp is on".into(),
                })
            }

            async fn execute(&self, _action: crate::ha::HomeAction) -> Result<ActionResult> {
                anyhow::bail!("should not execute")
            }

            async fn list_scenes(&self, _room: Option<&str>) -> Result<Vec<SceneRef>> {
                Ok(Vec::new())
            }

            async fn list_devices(&self, _room: Option<&str>) -> Result<Vec<DeviceRef>> {
                Ok(Vec::new())
            }
        }

        let err = control(
            &LowConfidenceHome,
            "Living room lamp",
            "turn_on",
            None,
            &ActuationSafetyConfig::default(),
            RequestOrigin::Dashboard,
            false,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("runtime safety"));
        assert!(err.contains("confidence"));
    }
}
