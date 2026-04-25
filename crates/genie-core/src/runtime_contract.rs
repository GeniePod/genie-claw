//! Deterministic runtime contract for operations and incident response.
//!
//! A persistent local assistant needs a reproducible description of what booted:
//! model family, prompt, tools, policy, and hydrated local state. This module
//! deliberately uses a small stable hash instead of adding a crypto dependency;
//! the hash is an operational fingerprint, not a security primitive.

use serde::Serialize;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::prompt::ModelFamily;
use crate::tools::dispatch::ToolDef;

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeContract {
    pub schema_version: u32,
    pub package: &'static str,
    pub version: &'static str,
    pub model_family: String,
    pub max_history_turns: usize,
    pub prompt_hash: String,
    pub prompt_bytes: usize,
    pub tool_schema_hash: String,
    pub tool_count: usize,
    pub tool_names: Vec<String>,
    pub policy_hash: String,
    pub hydration_hash: String,
    pub contract_hash: String,
    pub policy: serde_json::Value,
    pub hydration: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeContractSummary {
    pub schema_version: u32,
    pub model_family: String,
    pub max_history_turns: usize,
    pub prompt_hash: String,
    pub tool_schema_hash: String,
    pub tool_count: usize,
    pub policy_hash: String,
    pub hydration_hash: String,
    pub contract_hash: String,
}

impl RuntimeContract {
    pub fn summary(&self) -> RuntimeContractSummary {
        RuntimeContractSummary {
            schema_version: self.schema_version,
            model_family: self.model_family.clone(),
            max_history_turns: self.max_history_turns,
            prompt_hash: self.prompt_hash.clone(),
            tool_schema_hash: self.tool_schema_hash.clone(),
            tool_count: self.tool_count,
            policy_hash: self.policy_hash.clone(),
            hydration_hash: self.hydration_hash.clone(),
            contract_hash: self.contract_hash.clone(),
        }
    }
}

pub fn build_runtime_contract(
    system_prompt: &str,
    model_family: ModelFamily,
    max_history_turns: usize,
    tools: &[ToolDef],
    policy: serde_json::Value,
    hydration: serde_json::Value,
) -> RuntimeContract {
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let tool_schema_json = serde_json::to_string(tools).unwrap_or_else(|_| "[]".into());
    let policy_json = serde_json::to_string(&policy).unwrap_or_else(|_| "{}".into());
    let hydration_json = serde_json::to_string(&hydration).unwrap_or_else(|_| "{}".into());

    let prompt_hash = stable_hash(system_prompt);
    let tool_schema_hash = stable_hash(&tool_schema_json);
    let policy_hash = stable_hash(&policy_json);
    let hydration_hash = stable_hash(&hydration_json);
    let model_family = format!("{model_family:?}");

    let contract_seed = serde_json::json!({
        "schema_version": 1,
        "package": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "model_family": model_family,
        "max_history_turns": max_history_turns,
        "prompt_hash": prompt_hash,
        "tool_schema_hash": tool_schema_hash,
        "policy_hash": policy_hash,
        "hydration_hash": hydration_hash,
    });
    let contract_seed_json = serde_json::to_string(&contract_seed).unwrap_or_default();

    RuntimeContract {
        schema_version: 1,
        package: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        model_family,
        max_history_turns,
        prompt_hash,
        prompt_bytes: system_prompt.len(),
        tool_schema_hash,
        tool_count: tools.len(),
        tool_names,
        policy_hash,
        hydration_hash,
        contract_hash: stable_hash(&contract_seed_json),
        policy,
        hydration,
    }
}

pub fn append_runtime_contract_log(path: &Path, contract: &RuntimeContract) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let event = serde_json::json!({
        "ts_ms": now_ms(),
        "event": "runtime_contract",
        "contract": contract,
    });
    writeln!(file, "{event}")?;
    Ok(())
}

pub fn stable_hash(input: &str) -> String {
    // 64-bit FNV-1a. Stable across processes, platforms, and Rust versions.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash_is_deterministic() {
        assert_eq!(stable_hash("genie"), stable_hash("genie"));
        assert_ne!(stable_hash("genie"), stable_hash("claw"));
    }

    #[test]
    fn contract_hash_changes_when_prompt_changes() {
        let tools = vec![ToolDef {
            name: "get_time".into(),
            description: "Get time".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let base = build_runtime_contract(
            "prompt one",
            ModelFamily::Phi,
            8,
            &tools,
            serde_json::json!({"policy": "safe"}),
            serde_json::json!({"memories": 1}),
        );
        let changed = build_runtime_contract(
            "prompt two",
            ModelFamily::Phi,
            8,
            &tools,
            serde_json::json!({"policy": "safe"}),
            serde_json::json!({"memories": 1}),
        );

        assert_ne!(base.prompt_hash, changed.prompt_hash);
        assert_ne!(base.contract_hash, changed.contract_hash);
    }

    #[test]
    fn summary_exposes_compact_contract_fields() {
        let contract = build_runtime_contract(
            "prompt",
            ModelFamily::Phi,
            8,
            &[],
            serde_json::json!({}),
            serde_json::json!({}),
        );
        let summary = contract.summary();

        assert_eq!(summary.model_family, "Phi");
        assert_eq!(summary.tool_count, 0);
        assert_eq!(summary.contract_hash, contract.contract_hash);
    }

    #[test]
    fn append_runtime_contract_log_writes_jsonl_event() {
        let path = std::env::temp_dir().join("genie-runtime-contract-log.jsonl");
        let _ = std::fs::remove_file(&path);
        let contract = build_runtime_contract(
            "prompt",
            ModelFamily::Phi,
            8,
            &[],
            serde_json::json!({"policy": "safe"}),
            serde_json::json!({"memory": 0}),
        );

        append_runtime_contract_log(&path, &contract).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let line = text.lines().next().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(parsed["event"], "runtime_contract");
        assert_eq!(parsed["contract"]["contract_hash"], contract.contract_hash);
    }
}
