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
    /// Microsoft Phi-3 (compact, needs explicit format).
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

        match self.model_family {
            ModelFamily::Nemotron | ModelFamily::Llama | ModelFamily::Qwen => {
                self.prompt_capable_model(&tool_section, &memory_section, home_tools_available)
            }
            ModelFamily::Phi | ModelFamily::Small | ModelFamily::Generic => {
                self.prompt_simple_model(&tool_section, &memory_section, home_tools_available)
            }
        }
    }

    /// Prompt for models with good instruction following (Nemotron, Llama 3, Qwen).
    fn prompt_capable_model(
        &self,
        tools: &str,
        memories: &str,
        home_tools_available: bool,
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
- For math, always use the calculate tool.
- For weather, always use the get_weather tool.
- For time, always use the get_time tool.
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
    ) -> String {
        let home_note = if home_tools_available {
            ""
        } else {
            "Home control is currently unavailable. If asked to control or check a device, say Home Assistant is not connected.\n\n"
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

User: "weather in Tokyo"
You: {{"tool": "get_weather", "arguments": {{"location": "Tokyo"}}}}

{home_note}
If no tool is needed, just answer briefly (1-2 sentences).
Assume replies may be heard in a shared room. Do not volunteer secrets or highly sensitive details.

{memories}"#
        )
    }

    /// Format tool definitions for the system prompt.
    fn format_tools(&self, tools: &[ToolDef]) -> String {
        match self.model_family {
            ModelFamily::Nemotron | ModelFamily::Llama | ModelFamily::Qwen => {
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
}
