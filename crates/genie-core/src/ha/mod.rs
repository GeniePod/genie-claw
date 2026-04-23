use std::sync::Arc;

use genie_common::config::Config;

mod client;
mod policy;
mod provider;

pub use client::{Entity, HaClient};
pub use policy::{ActionPolicyDecision, ActionRisk, assess_home_action};
pub use provider::{
    ActionResult, AreaRef, DeviceRef, HomeAction, HomeActionKind, HomeAssistantProvider,
    HomeAutomationProvider, HomeGraph, HomeState, HomeTarget, HomeTargetKind, IntegrationHealth,
    SceneRef, ScriptRef, into_provider,
};

/// Build the optional Home Assistant provider from config and environment.
pub fn provider_from_config(config: &Config) -> Option<Arc<dyn HomeAutomationProvider>> {
    let Some(service) = config.homeassistant_service() else {
        tracing::info!("Home Assistant service not configured — integration disabled");
        return None;
    };

    let Some(token) = config.homeassistant_token() else {
        tracing::warn!("Home Assistant token not set — integration disabled");
        return None;
    };

    match HomeAssistantProvider::from_url(&service.url, &token) {
        Ok(provider) => Some(into_provider(provider)),
        Err(err) => {
            tracing::warn!(error = %err, "failed to configure Home Assistant integration");
            None
        }
    }
}
