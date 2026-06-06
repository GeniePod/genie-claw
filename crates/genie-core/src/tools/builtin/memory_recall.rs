use std::sync::Arc;

use anyhow::Result;

use crate::memory::Memory;
use crate::tools::dispatch::{ToolDef, ToolEntry, ToolExecutionContext};

pub struct MemoryRecallTool {
    pub memory: Arc<std::sync::Mutex<Memory>>,
}

impl ToolEntry for MemoryRecallTool {
    fn name(&self) -> &str {
        "memory_recall"
    }

    fn schema(&self) -> ToolDef {
        ToolDef {
            name: "memory_recall".into(),
            description: "Recall what you know about a topic. Use when the user asks 'what do you know about me', 'do you remember my name', etc.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Topic to search memories for (e.g., 'name', 'age', 'preferences')"}
                },
                "required": ["query"]
            }),
        }
    }

    fn execute<'a>(
        &'a self,
        args: &'a serde_json::Value,
        ctx: ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let query_result = super::super::dispatch::parse_memory_recall_query(args);
        let read_context = ctx
            .memory_read_context
            .unwrap_or_else(|| memory_read_context(args));
        let memory = Arc::clone(&self.memory);
        Box::pin(async move {
            let query = query_result?;
            let query = query.as_str();
            let mem = memory
                .lock()
                .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;

            if let Some(answer) = mem.structured_household_answer(query)? {
                return Ok(answer);
            }

            if let Some(role) = household_role_query(query) {
                let profiles = mem.household_profiles_by_role(role)?;
                if !profiles.is_empty() {
                    return Ok(format_household_role_answer(role, &profiles));
                }
            }

            let results =
                crate::memory::recall::recall_with_context(&mem, query, 10, read_context)?;
            if results.is_empty() {
                return Ok(match query {
                    "name" => "I don't remember your name yet.".to_string(),
                    "user" => "I don't remember anything about you yet.".to_string(),
                    other => format!("I don't remember anything about {} yet.", other),
                });
            }

            if query == "name"
                && let Some(entry) = results
                    .iter()
                    .find(|entry| entry.entry.content.to_lowercase().contains("name is "))
            {
                return Ok(entry
                    .entry
                    .content
                    .replace("User's name is ", "Your name is "));
            }

            if query == "user" || query == "me" {
                let items = results
                    .iter()
                    .take(3)
                    .map(|entry| entry.entry.content.clone())
                    .collect::<Vec<_>>();
                return Ok(format!("I remember:\n- {}", items.join("\n- ")));
            }

            if results.len() == 1 {
                return Ok(format!("I remember: {}", results[0].entry.content));
            }

            let items = results
                .iter()
                .map(|entry| format!("- [{}] {}", entry.entry.kind, entry.entry.content))
                .collect::<Vec<_>>();
            Ok(format!("I found these memories:\n{}", items.join("\n")))
        })
    }
}

pub(crate) fn memory_query(args: &serde_json::Value) -> &str {
    let raw = args
        .get("query")
        .or_else(|| args.get("topic"))
        .or_else(|| args.get("what"))
        .and_then(|v| v.as_str())
        .unwrap_or("user");

    let lower = raw.to_lowercase();
    if lower.contains("my name") || lower == "name" || lower.contains("who am i") {
        "name"
    } else if lower.contains("about me") || lower == "me" || lower == "user" {
        "user"
    } else {
        raw
    }
}

pub(crate) fn household_role_query(query: &str) -> Option<&'static str> {
    let normalized = query
        .trim()
        .to_ascii_lowercase()
        .replace(
            |ch: char| !ch.is_ascii_alphanumeric() && !ch.is_whitespace(),
            " ",
        )
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let role = tokens
        .iter()
        .find_map(|token| normalize_household_role_query_token(token))?;

    let is_role_question = normalized.starts_with("who is ")
        || normalized.starts_with("who are ")
        || normalized.starts_with("whos ")
        || normalized.starts_with("who s ")
        || normalized.contains(" in this house")
        || normalized.contains(" in our house")
        || normalized.contains(" household");
    let is_direct_role_topic = tokens.len() == 1
        || (tokens.len() == 2
            && normalize_household_role_query_token(tokens[0]).is_some()
            && matches!(tokens[1], "name" | "names"));

    if is_role_question || is_direct_role_topic {
        Some(role)
    } else {
        None
    }
}

fn normalize_household_role_query_token(token: &str) -> Option<&'static str> {
    match token {
        "dad" | "father" => Some("dad"),
        "mom" | "mother" | "mum" => Some("mom"),
        "son" | "sons" => Some("son"),
        "daughter" | "daughters" => Some("daughter"),
        "child" | "children" | "kid" | "kids" => Some("child"),
        "wife" => Some("wife"),
        "husband" => Some("husband"),
        "partner" => Some("partner"),
        "dog" | "dogs" => Some("dog"),
        "cat" | "cats" => Some("cat"),
        "pet" | "pets" => Some("pet"),
        _ => None,
    }
}

pub(crate) fn format_household_role_answer(
    role: &str,
    profiles: &[crate::memory::HouseholdProfile],
) -> String {
    if profiles.len() == 1 {
        return format!("{} is the {}.", profiles[0].name, role);
    }

    let names = profiles
        .iter()
        .map(|profile| profile.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{names} are the {role}s.")
}

pub(crate) fn memory_read_context(
    args: &serde_json::Value,
) -> crate::memory::policy::MemoryReadContext {
    let identity_confidence = match args
        .get("identity_confidence")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase()
        .as_str()
    {
        "high" => crate::memory::policy::IdentityConfidence::High,
        "medium" => crate::memory::policy::IdentityConfidence::Medium,
        "low" => crate::memory::policy::IdentityConfidence::Low,
        _ => crate::memory::policy::IdentityConfidence::Unknown,
    };

    crate::memory::policy::MemoryReadContext {
        identity_confidence,
        explicit_named_person: args
            .get("explicit_named_person")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        explicit_private_intent: args
            .get("explicit_private_intent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        shared_space_voice: args
            .get("shared_space_voice")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
    }
}
