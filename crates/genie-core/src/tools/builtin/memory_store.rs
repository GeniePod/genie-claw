use std::sync::Arc;

use anyhow::Result;

use crate::memory::Memory;
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct MemoryStoreTool {
    pub memory: Arc<std::sync::Mutex<Memory>>,
}

impl ToolEntry for MemoryStoreTool {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "memory_store".into(),
            description: "Explicitly store a safe household fact or preference. Use when the user says 'remember that...' or asks you to save something. Do not store passwords, one-time codes, payment details, keys, tokens, household access codes, lock combinations, sensitive document/key locations, or private secrets.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {"type": "string", "description": "The fact to remember"},
                    "category": {"type": "string", "enum": ["identity", "preference", "relationship", "fact", "context"], "description": "Category of the memory"}
                },
                "required": ["content"]
            }),
        }
    }

    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let args = args.clone();
        let memory = Arc::clone(&self.memory);
        Box::pin(async move {
            let mem = memory
                .lock()
                .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
            let memories = normalize_memories_to_store(&args);
            if memories.is_empty() {
                return Ok("Please specify what to remember.".to_string());
            }

            let mut stored = Vec::new();
            let mut stored_categories = Vec::new();
            let mut rejected = Vec::new();
            let mut replaced = 0;
            for (category, content) in memories {
                let policy = crate::memory::policy::assess_memory_write(&category, &content);
                if !policy.allowed {
                    rejected.push(policy.reason);
                    continue;
                }
                let outcome = mem.store_resolved(&category, &content)?;
                replaced += outcome.replaced;
                stored_categories.push(category);
                stored.push(content);
            }

            if stored.is_empty() {
                return Ok(rejected
                    .first()
                    .copied()
                    .unwrap_or("I could not store that memory.")
                    .to_string());
            }

            if stored_categories
                .iter()
                .any(|category| category == "shopping")
            {
                let count = mem.shopping_list_pending_count().unwrap_or(0);
                let removed = stored.iter().any(|content| {
                    content
                        .trim_start()
                        .to_ascii_lowercase()
                        .starts_with("shopping list removed:")
                });
                let added = stored
                    .iter()
                    .map(|content| {
                        content
                            .trim_start_matches("shopping list pending:")
                            .trim_start_matches("shopping list removed:")
                            .trim()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if removed {
                    return Ok(format!(
                        "Removed {added} from the shopping list. You have {count} item(s) total."
                    ));
                }
                return Ok(format!(
                    "Added {added} to the shopping list. You have {count} item(s) total."
                ));
            }

            if stored.len() == 1 {
                if replaced > 0 {
                    Ok(format!(
                        "I've updated that memory: {}.",
                        stored[0].to_lowercase()
                    ))
                } else {
                    Ok(format!("I'll remember that {}.", stored[0].to_lowercase()))
                }
            } else {
                let prefix = if replaced > 0 {
                    "I've updated these details"
                } else {
                    "I'll remember these details"
                };
                let mut response = format!("{prefix}:\n- {}", stored.join("\n- "));
                if let Some(reason) = rejected.first() {
                    response.push_str(&format!("\nSkipped one memory: {reason}"));
                }
                Ok(response)
            }
        })
    }
}

pub(crate) fn normalize_memories_to_store(
    args: &serde_json::Value,
) -> Vec<(String, String)> {
    let category_hint = args
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("fact");

    let primary = ["content", "fact", "text", "memory", "note"]
        .iter()
        .find_map(|key| args.get(*key).and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            args.as_object().and_then(|obj| {
                obj.iter()
                    .filter(|(key, _)| key.as_str() != "category")
                    .find_map(|(_, value)| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
        });

    let mut normalized = Vec::new();

    if let Some(content) = primary {
        let extracted = crate::memory::extract::extract_facts(&content);
        if extracted.is_empty() {
            normalized.push((category_hint.to_string(), content));
        } else {
            normalized.extend(
                extracted
                    .into_iter()
                    .map(|fact| (fact.category, fact.content))
                    .collect::<Vec<_>>(),
            );
        }
    } else if let Some(name) = args.get("name").and_then(|v| v.as_str()) {
        let name = name.trim();
        if !name.is_empty() {
            normalized.push(("identity".into(), format!("User's name is {}", name)));
        }
    }

    normalized
}
