//! Runtime skills: expose loaded skills as tools and dispatch calls to them.

use anyhow::Result;

use super::{ToolDef, ToolDispatcher};

fn runtime_skill_description(skill: &crate::skills::LoadedSkill) -> String {
    if skill.name == "hello_world" {
        "Demo greeting skill. Only use when the user explicitly asks you to say hello to someone or test the hello_world demo skill.".into()
    } else {
        skill.description.clone()
    }
}

impl ToolDispatcher {
    pub(super) fn skill_tool_defs(&self) -> Option<Vec<ToolDef>> {
        let skills = self.skills.as_ref()?;
        let loader = skills.lock().ok()?;
        Some(
            loader
                .loaded()
                .iter()
                .map(|skill| ToolDef {
                    name: skill.name.clone(),
                    description: runtime_skill_description(skill),
                    parameters: serde_json::from_str(&skill.parameters_json).unwrap_or_else(
                        |_| serde_json::json!({"type": "object", "properties": {}}),
                    ),
                })
                .collect(),
        )
    }

    /// Whether a loaded skill exposes a tool called `name`, without building the
    /// skill's `ToolDef` (no description build, no `parameters_json` parse).
    /// Mirrors the availability of [`skill_tool_defs`](Self::skill_tool_defs): a
    /// missing skill loader or a poisoned lock means no skill tools are known.
    pub(super) fn skill_tool_name_loaded(&self, name: &str) -> bool {
        let Some(skills) = self.skills.as_ref() else {
            return false;
        };
        let Ok(loader) = skills.lock() else {
            return false;
        };
        loader.loaded().iter().any(|skill| skill.name == name)
    }

    pub(super) async fn exec_skill(&self, name: &str, args: &serde_json::Value) -> Result<String> {
        let skills = self
            .skills
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", name))?;

        let args_json = serde_json::to_string(args)?;

        // Build a Send invocation handle under a short lock, then drop the lock
        // BEFORE awaiting the (possibly blocking) C call. The invocation owns an
        // Arc to the skill's library, so the native code stays mapped for the
        // whole call even though the loader lock is released. Holding a
        // std::sync::Mutex guard across the await would both serialize every
        // other skill access and trip clippy's `await_holding_lock`.
        let invocation = {
            let loader = skills
                .lock()
                .map_err(|e| anyhow::anyhow!("skill loader lock: {}", e))?;
            let skill = loader
                .loaded()
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", name))?;
            skill.prepare(&args_json)
        };

        let outcome = invocation.run().await;

        // Re-acquire the lock to record the fault and reap a skill that has
        // exceeded its fault budget. The skill may have been unloaded meanwhile;
        // that is fine — the Arc kept its library alive for the call above.
        {
            let mut loader = skills
                .lock()
                .map_err(|e| anyhow::anyhow!("skill loader lock: {}", e))?;
            if outcome.faulted
                && let Some(skill) = loader.get_mut(name)
            {
                skill.fault_count += 1;
            }
            let pruned = loader.prune_faulted();
            if pruned.iter().any(|skill_name| skill_name == name) {
                tracing::warn!(skill = name, "skill auto-unloaded after repeated faults");
            }
        }

        if outcome.success {
            Ok(outcome.output)
        } else {
            Err(anyhow::anyhow!("{}", outcome.output))
        }
    }
}
