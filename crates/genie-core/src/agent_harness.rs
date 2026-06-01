//! Limited-context harness checks for GenieClaw agent contracts.
//!
//! The goal is to keep prompt, tools, memory hydration, safety, and optional
//! provider paths usable inside the Jetson 4096-token baseline before any
//! deployment opts into a larger adaptive context. Low latency and answer
//! quality should come from selecting the right home context, not from widening
//! prompts by default.

use genie_common::config::{AgentConfig, OptionalAiProviderConfig};
use serde::Serialize;

use crate::memory::Memory;
use crate::runtime_boundary::JETSON_BASELINE_CONTEXT_TOKENS;
use crate::tools::dispatch::ToolDef;

pub const RESPONSE_RESERVE_TOKENS: usize = 512;
pub const TOOL_MANIFEST_BUDGET_TOKENS: usize = 900;
pub const MEMORY_HYDRATION_BUDGET_TOKENS: usize = 700;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HarnessCheck {
    pub name: &'static str,
    pub pass: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LimitedContextHarnessReport {
    pub pass: bool,
    pub context_window_tokens: usize,
    pub estimated_prompt_tokens: usize,
    pub estimated_tool_manifest_tokens: usize,
    pub estimated_memory_tokens: usize,
    pub estimated_total_tokens: usize,
    pub response_reserve_tokens: usize,
    pub checks: Vec<HarnessCheck>,
}

pub fn validate_limited_context_agent(
    system_prompt: &str,
    tools: &[ToolDef],
    memory_context: &str,
    agent: &AgentConfig,
    provider: &OptionalAiProviderConfig,
) -> LimitedContextHarnessReport {
    let context_window_tokens = agent.context_window_tokens.max(1) as usize;
    let tool_manifest = serde_json::to_string(tools).unwrap_or_else(|_| "[]".into());
    let estimated_prompt_tokens = estimate_tokens(system_prompt);
    let estimated_tool_manifest_tokens = estimate_tokens(&tool_manifest);
    let estimated_memory_tokens = estimate_tokens(memory_context);
    let estimated_total_tokens =
        estimated_prompt_tokens + estimated_tool_manifest_tokens + estimated_memory_tokens;

    let mut checks = Vec::new();
    checks.push(HarnessCheck {
        name: "jetson_baseline_context",
        pass: context_window_tokens <= JETSON_BASELINE_CONTEXT_TOKENS as usize,
        detail: format!(
            "{} tokens configured; Jetson baseline is {}",
            context_window_tokens, JETSON_BASELINE_CONTEXT_TOKENS
        ),
    });
    checks.push(HarnessCheck {
        name: "prompt_tool_memory_budget",
        pass: estimated_total_tokens + RESPONSE_RESERVE_TOKENS <= context_window_tokens,
        detail: format!(
            "{} estimated input tokens + {} reserved response tokens <= {}",
            estimated_total_tokens, RESPONSE_RESERVE_TOKENS, context_window_tokens
        ),
    });
    checks.push(HarnessCheck {
        name: "tool_manifest_budget",
        pass: estimated_tool_manifest_tokens <= TOOL_MANIFEST_BUDGET_TOKENS,
        detail: format!(
            "{} estimated tool tokens <= {}",
            estimated_tool_manifest_tokens, TOOL_MANIFEST_BUDGET_TOKENS
        ),
    });
    checks.push(HarnessCheck {
        name: "memory_hydration_budget",
        pass: estimated_memory_tokens <= MEMORY_HYDRATION_BUDGET_TOKENS,
        detail: format!(
            "{} estimated memory tokens <= {}",
            estimated_memory_tokens, MEMORY_HYDRATION_BUDGET_TOKENS
        ),
    });
    checks.push(HarnessCheck {
        name: "provider_limited_context",
        pass: provider.limited_context_compatible(agent),
        detail: format!(
            "provider enabled={} context={} agent_context={}",
            provider.enabled, provider.context_window_tokens, agent.context_window_tokens
        ),
    });

    let pass = checks.iter().all(|check| check.pass);
    LimitedContextHarnessReport {
        pass,
        context_window_tokens,
        estimated_prompt_tokens,
        estimated_tool_manifest_tokens,
        estimated_memory_tokens,
        estimated_total_tokens,
        response_reserve_tokens: RESPONSE_RESERVE_TOKENS,
        checks,
    }
}

pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Evaluate the Jetson limited-context harness against the boot-time prompt,
/// live tool manifest, and a representative per-turn memory hydration sample.
pub fn validate_boot_harness(
    system_prompt: &str,
    tools: &[ToolDef],
    memory: &Memory,
    agent: &AgentConfig,
    provider: &OptionalAiProviderConfig,
) -> LimitedContextHarnessReport {
    let memory_context =
        crate::memory::inject::build_memory_context(memory, "turn on the kitchen lights");
    validate_limited_context_agent(system_prompt, tools, &memory_context, agent, provider)
}

pub fn log_harness_report(report: &LimitedContextHarnessReport) {
    if report.pass {
        tracing::info!(
            pass = report.pass,
            context_window_tokens = report.context_window_tokens,
            estimated_total_tokens = report.estimated_total_tokens,
            response_reserve_tokens = report.response_reserve_tokens,
            "agent harness passed Jetson limited-context checks"
        );
        return;
    }

    for check in report.checks.iter().filter(|check| !check.pass) {
        tracing::warn!(
            check = check.name,
            detail = %check.detail,
            "agent harness check failed"
        );
    }
    tracing::warn!(
        pass = report.pass,
        context_window_tokens = report.context_window_tokens,
        estimated_total_tokens = report.estimated_total_tokens,
        response_reserve_tokens = report.response_reserve_tokens,
        "agent harness failed Jetson limited-context checks"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_common::config::{OptionalAiProviderAuthMode, OptionalAiProviderKind};

    fn sample_tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: format!("{name} test tool"),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                }
            }),
        }
    }

    #[test]
    fn compact_agent_contract_passes_4096_token_budget() {
        let agent = AgentConfig::default();
        let provider = OptionalAiProviderConfig::default();
        let tools = vec![
            sample_tool("get_time"),
            sample_tool("memory_recall"),
            sample_tool("home_control"),
        ];

        let report = validate_limited_context_agent(
            "You are GenieClaw, a local home AI native to NVIDIA Jetson Orin 8GB. Use tools only when needed.",
            &tools,
            "Household context: kitchen light is in the kitchen.",
            &agent,
            &provider,
        );

        assert!(report.pass, "{:?}", report.checks);
        assert!(report.estimated_total_tokens < 4096);
        assert_eq!(MEMORY_HYDRATION_BUDGET_TOKENS, 700);
    }

    #[test]
    fn oversized_memory_hydration_fails_the_harness() {
        let agent = AgentConfig::default();
        let provider = OptionalAiProviderConfig::default();
        let memory_context = "remembered household detail. ".repeat(500);

        let report = validate_limited_context_agent(
            "You are GenieClaw, a local home AI native to NVIDIA Jetson Orin 8GB.",
            &[sample_tool("memory_recall")],
            &memory_context,
            &agent,
            &provider,
        );

        assert!(!report.pass);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "memory_hydration_budget" && !check.pass)
        );
    }

    #[test]
    fn optional_provider_over_context_budget_fails_the_harness() {
        let agent = AgentConfig::default();
        let provider = OptionalAiProviderConfig {
            enabled: true,
            provider: OptionalAiProviderKind::OpenAiCompatible,
            auth_mode: OptionalAiProviderAuthMode::ApiKey,
            base_url: "https://provider.example/v1".into(),
            api_key_env: "GENIE_PROVIDER_KEY".into(),
            oauth_token_env: "GENIE_PROVIDER_OAUTH_TOKEN".into(),
            context_window_tokens: 8192,
            allow_remote_base_url: true,
        };

        let report = validate_limited_context_agent(
            "You are GenieClaw, a local home AI native to NVIDIA Jetson Orin 8GB.",
            &[sample_tool("get_time")],
            "",
            &agent,
            &provider,
        );

        assert!(!report.pass);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "provider_limited_context" && !check.pass)
        );
    }
}
