use std::sync::Arc;

use anyhow::Result;

use crate::skills::{LoadedSkill, SkillLoader};
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct SkillRunnerTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub skills: Arc<std::sync::Mutex<SkillLoader>>,
}

impl ToolEntry for SkillRunnerTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }

    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let name = self.name.clone();
        let args_json = serde_json::to_string(args).unwrap_or_default();
        let skills = Arc::clone(&self.skills);
        Box::pin(async move {
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

            {
                let mut loader = skills
                    .lock()
                    .map_err(|e| anyhow::anyhow!("skill loader lock: {}", e))?;
                if outcome.faulted
                    && let Some(skill) = loader.get_mut(&name)
                {
                    skill.fault_count += 1;
                }
                let pruned = loader.prune_faulted();
                if pruned.iter().any(|skill_name| skill_name.as_str() == name) {
                    tracing::warn!(skill = %name, "skill auto-unloaded after repeated faults");
                }
            }

            if outcome.success {
                Ok(outcome.output)
            } else {
                Err(anyhow::anyhow!("{}", outcome.output))
            }
        })
    }
}

pub(crate) fn runtime_skill_description(skill: &LoadedSkill) -> String {
    if skill.name == "hello_world" {
        "Demo greeting skill. Only use when the user explicitly asks you to say hello to someone or test the hello_world demo skill.".into()
    } else {
        skill.description.clone()
    }
}
