use crate::memory::Memory;
use crate::tools::dispatch::ToolDef;

/// System prompt builder.
///
/// Different LLMs respond to tool-calling instructions differently.
/// This module generates optimized system prompts per model family,
/// maximizing tool-call reliability.
pub struct PromptBuilder {
    model_family: ModelFamily,
}

#[derive(Debug, Clone, Copy)]
pub enum ModelFamily {
    /// NVIDIA Nemotron (ChatML format, good at JSON).
    Nemotron,
    /// Meta Llama 3.x (strong instruction following).
    Llama,
    /// Alibaba Qwen 2.5+ (excellent tool calling).
    Qwen,
    /// Microsoft Phi-4 mini / Phi-family instruct models.
    Phi,
    /// TinyLlama or other small models (needs very explicit instructions).
    Small,
    /// Generic fallback.
    Generic,
}

impl ModelFamily {
    /// Detect model family from model filename or name.
    pub fn from_model_name(name: &str) -> Self {
        let lower = name.to_lowercase();
        if lower.contains("nemotron") {
            Self::Nemotron
        } else if lower.contains("llama") && !lower.contains("tiny") {
            Self::Llama
        } else if lower.contains("qwen") {
            Self::Qwen
        } else if lower.contains("phi") {
            Self::Phi
        } else if lower.contains("tiny") || lower.contains("small") || lower.contains("1b") {
            Self::Small
        } else {
            Self::Generic
        }
    }
}

impl PromptBuilder {
    pub fn new(model_family: ModelFamily) -> Self {
        Self { model_family }
    }

    pub fn from_model_name(name: &str) -> Self {
        Self::new(ModelFamily::from_model_name(name))
    }

    /// Build the system prompt with tools and memory context.
    pub fn build(&self, tools: &[ToolDef], memory: &Memory) -> String {
        let tool_section = self.format_tools(tools);
        let memory_section = format_memories(memory);
        let home_tools_available = tools.iter().any(|tool| tool.name == "home_control");
        let hello_world_available = tools.iter().any(|tool| tool.name == "hello_world");

        match self.model_family {
            ModelFamily::Nemotron | ModelFamily::Llama | ModelFamily::Qwen | ModelFamily::Phi => {
                self.prompt_capable_model(
                    &tool_section,
                    &memory_section,
                    home_tools_available,
                    hello_world_available,
                )
            }
            ModelFamily::Small | ModelFamily::Generic => self.prompt_simple_model(
                &tool_section,
                &memory_section,
                home_tools_available,
                hello_world_available,
            ),
        }
    }

    /// Prompt for models with good instruction following (Nemotron, Llama 3, Qwen, Phi-4).
    fn prompt_capable_model(
        &self,
        tools: &str,
        memories: &str,
        home_tools_available: bool,
        hello_world_available: bool,
    ) -> String {
        let role_summary = if home_tools_available {
            "You help the household control the home, answer everyday questions, manage timers, check weather, and handle simple calculations."
        } else {
            "You help the household answer everyday questions, manage timers, check weather, and handle simple calculations. Home control is currently unavailable."
        };
        let home_rule = if home_tools_available {
            "- For smart home commands, always use the home_control or home_status tool."
        } else {
            "- Home control is currently unavailable. If asked to control or check a device, say Home Assistant is not connected."
        };
        let hello_world_rule = if hello_world_available {
            "- Only use hello_world when the user explicitly asks you to say hello to someone or test the hello_world demo skill. Do not use it for time, weather, memory, math, or general conversation."
        } else {
            ""
        };

        format!(
            r#"You are GeniePod Home, a local home AI for a shared living space.
{role_summary}
Your tone should be calm, concise, and natural for spoken replies.

## Tool Calling

When the user's request requires a tool, respond with ONLY a JSON object (no other text):
{{"tool": "<tool_name>", "arguments": {{<arguments>}}}}

Do NOT wrap the JSON in markdown code blocks. Do NOT add explanation before or after the JSON.

Available tools:
{tools}

## Rules
- If no tool is needed, respond naturally in 1-3 short sentences (optimized for voice).
- Never make up information. If unsure, say so.
{home_rule}
{hello_world_rule}
- Risky home actions such as locks, garage doors, cameras, alarms, purchases, or non-voice-safe scripts require local confirmation and may be blocked by policy.
- For math, always use the calculate tool.
- For weather, always use the get_weather tool.
- For time, always use the get_time tool.
- For system status, Home Assistant connection status, memory, uptime, governor mode, or load average, always use the system_info tool.
- When the user asks what you remember, what you know about them, or asks for their name back, use the memory_recall tool.
- When the user asks about memory database health, memory index health, or memory diagnostics, use the memory_status tool.
- Only use memory_store when the user explicitly asks you to remember or save something.
- Do not store passwords, one-time codes, payment details, API keys, tokens, or private secrets as memory.
- If the user casually shares a fact like "my name is Jared", answer naturally and do not call memory_store just for that. The memory system can capture that automatically.
- Assume replies may be heard in a shared room. Do not volunteer secrets or highly sensitive details.

## Household Context
{memories}"#
        )
    }

    /// Prompt for smaller/simpler models that need more explicit guidance.
    fn prompt_simple_model(
        &self,
        tools: &str,
        memories: &str,
        home_tools_available: bool,
        hello_world_available: bool,
    ) -> String {
        let home_note = if home_tools_available {
            ""
        } else {
            "Home control is currently unavailable. If asked to control or check a device, say Home Assistant is not connected.\n\n"
        };
        let hello_world_note = if hello_world_available {
            "Only use hello_world when the user explicitly asks you to say hello to someone or test the hello_world demo skill. Do not use it for time, weather, memory, math, or general conversation.\n\n"
        } else {
            ""
        };
        let home_examples = if home_tools_available {
            r#"User: "turn on the kitchen light"
You: {"tool": "home_control", "arguments": {"entity": "kitchen light", "action": "turn_on"}}

User: "set movie night"
You: {"tool": "home_control", "arguments": {"entity": "movie night", "action": "activate"}}

"#
        } else {
            ""
        };

        format!(
            r#"You are GeniePod Home, a local home AI for a shared household.
Keep your tone concise and natural for voice.

IMPORTANT: When the user asks you to do something, check if a tool can help.
If yes, reply with ONLY this JSON format (nothing else):
{{"tool": "TOOL_NAME", "arguments": {{"key": "value"}}}}

Tools you can use:
{tools}

EXAMPLES:
User: "what time is it"
You: {{"tool": "get_time", "arguments": {{}}}}

{home_examples}
User: "what's 15 percent of 200"
You: {{"tool": "calculate", "arguments": {{"expression": "200 * 0.15"}}}}

User: "get current system status"
You: {{"tool": "system_info", "arguments": {{}}}}

User: "is Home Assistant connected?"
You: {{"tool": "system_info", "arguments": {{}}}}

User: "did you remember my name?"
You: {{"tool": "memory_recall", "arguments": {{"query": "name"}}}}

User: "is the memory database healthy?"
You: {{"tool": "memory_status", "arguments": {{}}}}

User: "remember that my dog's name is Milo"
You: {{"tool": "memory_store", "arguments": {{"content": "my dog's name is Milo", "category": "relationship"}}}}

User: "my name is Jared"
You: Nice to meet you, Jared.

User: "weather in Tokyo"
You: {{"tool": "get_weather", "arguments": {{"location": "Tokyo"}}}}

{hello_world_note}\
{home_note}
Risky home actions such as locks, garage doors, cameras, alarms, purchases, or non-voice-safe scripts require local confirmation and may be blocked by policy.
If the user asks what you remember, what you know about them, or asks for their name back, use memory_recall.
If the user asks about memory database health, memory index health, or memory diagnostics, use memory_status.
Only use memory_store when the user explicitly asks you to remember or save something.
Do not store passwords, one-time codes, payment details, API keys, tokens, or private secrets as memory.
If no tool is needed, just answer briefly (1-2 sentences).
Assume replies may be heard in a shared room. Do not volunteer secrets or highly sensitive details.

{memories}"#
        )
    }

    /// Format tool definitions for the system prompt.
    fn format_tools(&self, tools: &[ToolDef]) -> String {
        match self.model_family {
            ModelFamily::Nemotron | ModelFamily::Llama | ModelFamily::Qwen | ModelFamily::Phi => {
                // JSON schema format for capable models.
                tools
                    .iter()
                    .map(|t| {
                        format!(
                            "- **{}**: {}\n  Parameters: {}",
                            t.name,
                            t.description,
                            serde_json::to_string(&t.parameters).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => {
                // Simple list for smaller models (less token overhead).
                tools
                    .iter()
                    .map(|t| format!("- {}: {}", t.name, t.description))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }
}

fn format_memories(memory: &Memory) -> String {
    match memory.recent(5) {
        Ok(entries) if !entries.is_empty() => {
            let items: Vec<String> = entries.iter().map(|e| format!("- {}", e.content)).collect();
            format!("Relevant household context:\n{}", items.join("\n"))
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_nemotron() {
        assert!(matches!(
            ModelFamily::from_model_name("nemotron-4b-q4_k_m.gguf"),
            ModelFamily::Nemotron
        ));
    }

    #[test]
    fn detect_llama() {
        assert!(matches!(
            ModelFamily::from_model_name("Meta-Llama-3.1-8B-Instruct.Q4_K_M.gguf"),
            ModelFamily::Llama
        ));
    }

    #[test]
    fn detect_tiny_as_small() {
        assert!(matches!(
            ModelFamily::from_model_name("tinyllama-1.1b-chat-v1.0.Q4_K_M.gguf"),
            ModelFamily::Small
        ));
    }

    #[test]
    fn detect_qwen() {
        assert!(matches!(
            ModelFamily::from_model_name("Qwen2.5-7B-Instruct-Q4_K_M.gguf"),
            ModelFamily::Qwen
        ));
    }

    #[test]
    fn detect_phi() {
        assert!(matches!(
            ModelFamily::from_model_name("Phi-4-mini-instruct-Q4_K_M.gguf"),
            ModelFamily::Phi
        ));
    }

    #[test]
    fn detect_unknown_as_generic() {
        assert!(matches!(
            ModelFamily::from_model_name("some-random-model.gguf"),
            ModelFamily::Generic
        ));
    }

    #[test]
    fn capable_prompt_has_json_format() {
        let builder = PromptBuilder::new(ModelFamily::Nemotron);
        let tools = vec![crate::tools::dispatch::ToolDef {
            name: "get_time".into(),
            description: "Get current time".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let mem_path = std::env::temp_dir().join("prompt-test.db");
        let _ = std::fs::remove_file(&mem_path);
        let memory = Memory::open(&mem_path).unwrap();

        let prompt = builder.build(&tools, &memory);
        assert!(prompt.contains("ONLY a JSON object"));
        assert!(prompt.contains("get_time"));
    }

    #[test]
    fn capable_prompt_requires_system_info_for_status_questions() {
        let builder = PromptBuilder::new(ModelFamily::Nemotron);
        let tools = vec![crate::tools::dispatch::ToolDef {
            name: "system_info".into(),
            description: "Get system status".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let mem_path = std::env::temp_dir().join("prompt-test-system-info.db");
        let _ = std::fs::remove_file(&mem_path);
        let memory = Memory::open(&mem_path).unwrap();

        let prompt = builder.build(&tools, &memory);
        assert!(prompt.contains("always use the system_info tool"));
        assert!(prompt.contains("Home Assistant connection status"));
    }

    #[test]
    fn small_prompt_has_examples() {
        let builder = PromptBuilder::new(ModelFamily::Small);
        let tools = vec![crate::tools::dispatch::ToolDef {
            name: "get_time".into(),
            description: "Get current time".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let mem_path = std::env::temp_dir().join("prompt-test-small.db");
        let _ = std::fs::remove_file(&mem_path);
        let memory = Memory::open(&mem_path).unwrap();

        let prompt = builder.build(&tools, &memory);
        assert!(prompt.contains("EXAMPLES:"));
        assert!(prompt.contains("what time is it"));
        assert!(prompt.contains("get current system status"));
        assert!(prompt.contains("is Home Assistant connected?"));
        assert!(prompt.contains("\"system_info\""));
    }

    #[test]
    fn prompt_without_home_tools_marks_home_control_unavailable() {
        let builder = PromptBuilder::new(ModelFamily::Small);
        let tools = vec![crate::tools::dispatch::ToolDef {
            name: "get_time".into(),
            description: "Get current time".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let mem_path = std::env::temp_dir().join("prompt-test-no-home.db");
        let _ = std::fs::remove_file(&mem_path);
        let memory = Memory::open(&mem_path).unwrap();

        let prompt = builder.build(&tools, &memory);
        assert!(prompt.contains("Home control is currently unavailable"));
        assert!(!prompt.contains("turn on the kitchen light"));
    }

    #[test]
    fn prompt_with_hello_world_limits_demo_skill_usage() {
        let builder = PromptBuilder::new(ModelFamily::Phi);
        let tools = vec![crate::tools::dispatch::ToolDef {
            name: "hello_world".into(),
            description: "Demo greeting skill".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}}),
        }];
        let mem_path = std::env::temp_dir().join("prompt-test-hello-world.db");
        let _ = std::fs::remove_file(&mem_path);
        let memory = Memory::open(&mem_path).unwrap();

        let prompt = builder.build(&tools, &memory);
        assert!(prompt.contains("Only use hello_world when the user explicitly asks"));
        assert!(
            prompt
                .contains("Do not use it for time, weather, memory, math, or general conversation")
        );
    }

    #[test]
    fn prompt_guides_memory_recall_and_store_correctly() {
        let builder = PromptBuilder::new(ModelFamily::Small);
        let tools = vec![
            crate::tools::dispatch::ToolDef {
                name: "memory_recall".into(),
                description: "Recall memories".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            },
            crate::tools::dispatch::ToolDef {
                name: "memory_status".into(),
                description: "Memory diagnostics".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
            crate::tools::dispatch::ToolDef {
                name: "memory_store".into(),
                description: "Store a memory".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"content": {"type": "string"}}}),
            },
        ];
        let mem_path = std::env::temp_dir().join("prompt-test-memory-tools.db");
        let _ = std::fs::remove_file(&mem_path);
        let memory = Memory::open(&mem_path).unwrap();

        let prompt = builder.build(&tools, &memory);
        assert!(prompt.contains("did you remember my name?"));
        assert!(prompt.contains("\"memory_recall\""));
        assert!(prompt.contains("\"memory_status\""));
        assert!(prompt.contains("memory database health"));
        assert!(prompt.contains("Only use memory_store when the user explicitly asks"));
        assert!(prompt.contains("my name is Jared"));
    }

    #[test]
    fn phi_uses_capable_prompt_shape() {
        let builder = PromptBuilder::new(ModelFamily::Phi);
        let tools = vec![crate::tools::dispatch::ToolDef {
            name: "get_time".into(),
            description: "Get current time".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        let mem_path = std::env::temp_dir().join("prompt-test-phi.db");
        let _ = std::fs::remove_file(&mem_path);
        let memory = Memory::open(&mem_path).unwrap();

        let prompt = builder.build(&tools, &memory);
        assert!(prompt.contains("ONLY a JSON object"));
        assert!(prompt.contains("Do NOT wrap the JSON in markdown code blocks"));
        assert!(!prompt.contains("EXAMPLES:"));
    }
}
