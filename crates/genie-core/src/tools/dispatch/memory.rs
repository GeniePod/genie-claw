//! `memory_recall` / `memory_status` / `memory_forget` / `memory_store` tools:
//! parse the query/content args, gate person-scoped access through the trusted
//! read context, and read or write the memory backend.

use anyhow::Result;

use super::{ToolDef, ToolDispatcher, ToolExecutionContext};

pub(super) fn tool_defs() -> Vec<ToolDef> {
    vec![
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
        },
        ToolDef {
            name: "memory_status".into(),
            description: "Check memory database health, row counts, FTS consistency, and promoted memory count. Use for memory system diagnostics, not for recalling personal facts.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "memory_forget".into(),
            description: "Forget a specific piece of information. Use ONLY when the user explicitly asks to forget something, like 'forget my age' or 'delete what you know about X'.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to forget (e.g., 'age', 'name', 'favorite color')"}
                },
                "required": ["query"]
            }),
        },
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
        },
    ]
}

fn parse_memory_query_arg(args: &serde_json::Value) -> Result<&str> {
    args.get("query")
        .or_else(|| args.get("topic"))
        .or_else(|| args.get("what"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("memory tool requires non-empty string argument (query/topic/what)")
        })
}

fn parse_memory_recall_query(args: &serde_json::Value) -> Result<String> {
    let raw = parse_memory_query_arg(args)?;
    Ok(normalize_memory_recall_query(raw))
}

fn normalize_memory_recall_query(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("my name") || lower == "name" || lower.contains("who am i") {
        "name".into()
    } else if lower.contains("about me") || lower == "me" || lower == "user" {
        "user".into()
    } else {
        raw.to_string()
    }
}

fn parse_memory_forget_query(args: &serde_json::Value) -> Result<&str> {
    parse_memory_query_arg(args)
}

/// Reject a `memory_store` call with no usable `content` (#416). Delegates to
/// `normalize_memories_to_store` so all valid shapes (aliases, catch-all, `name`)
/// still pass; only missing/empty/wrong-type content is rejected.
fn parse_memory_store_content(args: &serde_json::Value) -> Result<Vec<(String, String)>> {
    let memories = normalize_memories_to_store(args);
    if memories.is_empty() {
        anyhow::bail!("memory_store requires non-empty string argument 'content'");
    }
    Ok(memories)
}

pub(super) fn household_role_query(query: &str) -> Option<&'static str> {
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

pub(super) fn format_household_role_answer(
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
    format!("{names} are the {}.", pluralize_household_role(role))
}

/// Pluralize a canonical household role. Roles come from
/// `normalize_household_role_query_token`, whose set is regular `+s` except for
/// the two irregular plurals `child` -> `children` and `wife` -> `wives`.
/// Without this, the multi-profile answer produced "childs"/"wifes".
fn pluralize_household_role(role: &str) -> String {
    match role {
        "child" => "children".to_string(),
        "wife" => "wives".to_string(),
        other => format!("{other}s"),
    }
}

fn normalize_memories_to_store(args: &serde_json::Value) -> Vec<(String, String)> {
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

impl ToolDispatcher {
    pub(super) fn exec_memory_recall(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
    ) -> Result<String> {
        let query = parse_memory_recall_query(args)?;
        // Identity context must come only from trusted runtime surfaces (voice
        // pipeline via exec_ctx.memory_read_context). Do not read
        // identity_confidence or related fields from LLM tool arguments — an
        // attacker could otherwise bypass shared-room privacy controls (#430).
        let read_context = exec_ctx
            .memory_read_context
            .unwrap_or_else(crate::memory::policy::MemoryReadContext::shared_room_voice);
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
        let query_ref = query.as_str();

        if let Some(answer) = mem.structured_household_answer(query_ref)? {
            return Ok(answer);
        }

        if let Some(role) = household_role_query(query_ref) {
            let profiles = mem.household_profiles_by_role(role)?;
            if !profiles.is_empty() {
                return Ok(format_household_role_answer(role, &profiles));
            }
        }

        let results =
            crate::memory::recall::recall_with_context(&mem, query_ref, 10, read_context)?;
        if results.is_empty() {
            return Ok(match query_ref {
                "name" => "I don't remember your name yet.".to_string(),
                "user" => "I don't remember anything about you yet.".to_string(),
                other => format!("I don't remember anything about {} yet.", other),
            });
        }

        if query_ref == "name"
            && let Some(entry) = results
                .iter()
                .find(|entry| entry.entry.content.to_lowercase().contains("name is "))
        {
            return Ok(entry
                .entry
                .content
                .replace("User's name is ", "Your name is "));
        }

        if query_ref == "user" || query_ref == "me" {
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
    }

    pub(super) fn exec_memory_status(&self) -> Result<String> {
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;
        let health = mem.health()?;
        let promoted = mem.promoted_count()?;
        let state = if health.quick_check_ok && health.fts_consistent && !health.migration_degraded
        {
            "ok"
        } else {
            "degraded"
        };

        Ok(format!(
            "Memory status: {}. Rows: {}. FTS rows: {}. FTS consistent: {}. Migration degraded: {}. Promoted memories: {}. Canonical root: {}. Namespace notes: {}. Daily notes: {}. Event logs: {}. Person-scoped memories: {}. Private memories: {}. Restricted memories: {}.",
            state,
            health.memory_rows,
            health.fts_rows,
            if health.fts_consistent { "yes" } else { "no" },
            if health.migration_degraded {
                "yes"
            } else {
                "no"
            },
            promoted,
            if health.canonical_root_exists {
                "present"
            } else {
                "missing"
            },
            health.canonical_namespace_files,
            health.canonical_daily_files,
            health.canonical_event_logs,
            health.person_rows,
            health.private_rows,
            health.restricted_rows,
        ))
    }

    pub(super) fn exec_memory_forget(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
    ) -> Result<String> {
        // Validate the argument at the execution boundary, before touching the
        // memory backend, the same way exec_memory_recall does (PR #362). The
        // previous `unwrap_or("")` silently coerced a missing or non-string
        // `query` into "", and a whitespace-only query slipped past the
        // `is_empty()` check straight into `mem.search("   ", ...)` — running a
        // destructive forget on garbage input instead of being rejected and
        // audited like its read-side sibling.
        let query = parse_memory_forget_query(args)?;
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;

        // Gate deletes through the same MemoryReadContext that exec_memory_recall
        // uses. Without it, an LLM that cannot READ a person-scoped row could
        // still DELETE it by calling memory_forget — destroying data it has no
        // privilege to see. This mirrors the read-side fix landed in
        // PR #201 (commit be4a2da).
        let read_context = exec_ctx
            .memory_read_context
            .unwrap_or_else(crate::memory::policy::MemoryReadContext::shared_room_voice);
        let candidates = mem.search(query, 10)?;
        let allowed = crate::memory::recall::filter_recall_results(candidates, read_context);
        let mut deleted = 0usize;
        for recallable in &allowed {
            if mem.delete_by_id(recallable.entry.id)? {
                deleted += 1;
            }
        }

        if deleted == 0 {
            Ok(format!("No memories found matching '{}'.", query))
        } else {
            Ok(format!(
                "Forgot {} about '{}'.",
                super::count_noun(deleted, "memory", "memories"),
                query
            ))
        }
    }

    pub(super) fn exec_memory_store(
        &self,
        args: &serde_json::Value,
        exec_ctx: ToolExecutionContext,
    ) -> Result<String> {
        // Validate content before the lock; previously a soft Ok() audited as success (#416).
        let memories = parse_memory_store_content(args)?;
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("memory system not available"))?;
        let mem = mem
            .lock()
            .map_err(|e| anyhow::anyhow!("memory lock: {}", e))?;

        // Mirror the read-side context gate from exec_memory_recall/exec_memory_forget
        // (issue #454): person-scoped writes require the same verified identity that
        // person-scoped reads do. Without this, an API/REPL path with no trusted
        // MemoryReadContext could plant person-attributed facts that the voice
        // pipeline would later surface as authoritative.
        let write_context = exec_ctx
            .memory_read_context
            .unwrap_or_else(crate::memory::policy::MemoryReadContext::shared_room_voice);

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
            let metadata = crate::memory::policy::infer_metadata(&category, &content);
            if metadata.scope == crate::memory::policy::MemoryScope::Person
                && !write_context.explicit_named_person
                && write_context.identity_confidence
                    < crate::memory::policy::IdentityConfidence::Medium
            {
                anyhow::bail!(
                    "memory_store: person-linked category '{category}' requires a \
                     verified identity context; use the voice pipeline or supply \
                     an authenticated person context."
                );
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
            let total = super::count_noun(count, "item", "items");
            if removed {
                return Ok(format!(
                    "Removed {added} from the shopping list. You have {total} total."
                ));
            }
            return Ok(format!(
                "Added {added} to the shopping list. You have {total} total."
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
    }
}
