use anyhow::Result;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::conversation::ConversationStore;
use crate::llm::{LlmClient, LlmRequestHints, Message};
use crate::memory::{self, SharedMemory, with_shared_memory};
use crate::prompt::ModelFamily;
use crate::reasoning::InteractionKind;
use crate::tools;

/// Interactive REPL for genie-core.
///
/// Reads from stdin, sends to LLM, prints response.
/// Runs alongside the HTTP server — useful for development and SSH sessions.
pub async fn run(
    llm: &LlmClient,
    tools_dispatch: &tools::ToolDispatcher,
    memory: &SharedMemory,
    conversations: &ConversationStore,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
) -> Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    // Get or create a REPL conversation.
    let conv_id = conversations.create().await?;
    tracing::info!(conv_id = %conv_id, "REPL conversation started");

    let tool_names = tools_dispatch
        .tool_defs()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>()
        .join(", ");

    eprintln!("\nGeniePod REPL — type a message (Ctrl+C to quit)");
    eprintln!("  Tools: {}\n", tool_names);

    loop {
        eprint!("> ");

        let line = match lines.next_line().await? {
            Some(l) => l,
            None => break, // EOF
        };

        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        if text == "quit" || text == "exit" {
            break;
        }
        if text == "clear" {
            eprintln!("(conversation cleared)");
            continue;
        }

        // Security: scan for prompt injection (issue #196).
        crate::security::injection::scan_and_warn(text, crate::security::injection::source::REPL);

        // Persist user message.
        conversations
            .append_or_log(&conv_id, "user", text, None)
            .await;

        if let Some(call) = tools::quick::route_for_available_tools(
            text,
            tools_dispatch.has_home_automation(),
            tools_dispatch.has_web_search(),
        ) {
            let tool_result = tools_dispatch
                .execute_with_context(
                    &call,
                    tools::ToolExecutionContext {
                        request_origin: tools::RequestOrigin::Repl,
                        ..tools::ToolExecutionContext::default()
                    },
                )
                .await;
            let response = if tool_result.success {
                tool_result.output.clone()
            } else {
                format!("{} failed: {}", tool_result.tool, tool_result.output)
            };
            let response = crate::security::sandbox::sanitize_output(&response);
            let tool_json = serde_json::json!({
                "tool": call.name,
                "arguments": call.arguments,
            })
            .to_string();

            eprintln!("\nGeniePod: {}", response);
            conversations
                .append_or_log(&conv_id, "assistant", &tool_json, Some(&tool_result.tool))
                .await;
            conversations
                .append_or_log(
                    &conv_id,
                    "system",
                    &format!("Tool: {}", tool_result.output),
                    None,
                )
                .await;
            conversations
                .append_or_log(&conv_id, "assistant", &response, None)
                .await;

            let stored = with_shared_memory(memory, |memory| {
                memory::extract::extract_and_store(memory, text)
            });
            if stored > 0 {
                eprintln!(
                    "(remembered {} fact{})",
                    stored,
                    if stored == 1 { "" } else { "s" }
                );
            }
            continue;
        }

        // Build context with per-query memory injection.
        let memory_context = with_shared_memory(memory, |memory| {
            memory::inject::build_memory_context(memory, text)
        });
        let full_prompt = format!(
            "{}\n\nRelevant household context:\n{}",
            system_prompt, memory_context
        );

        let history = conversations
            .get_recent(&conv_id, max_history)
            .await
            .unwrap_or_default();
        let mut messages = vec![Message {
            role: "system".into(),
            content: full_prompt,
        }];
        messages.extend(history);
        let (messages, decision) = crate::reasoning::apply_reasoning_mode(
            model_family,
            &messages,
            text,
            InteractionKind::Repl,
        );
        tracing::debug!(?model_family, ?decision, "applied reasoning mode for repl");

        // Stream LLM response.
        eprint!("\nGeniePod: ");
        let request_hints = LlmRequestHints::agent_turn(&conv_id, 512);
        match llm
            .chat_stream_with_hints(&messages, Some(512), &request_hints, |token| {
                eprint!("{}", token);
            })
            .await
        {
            Ok(response) => {
                eprintln!();

                // Check for tool call.
                if let Some(tool_result) = tools::try_tool_call_with_context(
                    &response,
                    tools_dispatch,
                    tools::ToolExecutionContext {
                        request_origin: tools::RequestOrigin::Repl,
                        ..tools::ToolExecutionContext::default()
                    },
                )
                .await
                {
                    eprintln!("[TOOL: {}] {}", tool_result.tool, tool_result.output);
                    conversations
                        .append_or_log(&conv_id, "assistant", &response, Some(&tool_result.tool))
                        .await;
                    conversations
                        .append_or_log(
                            &conv_id,
                            "system",
                            &format!("Tool: {}", tool_result.output),
                            None,
                        )
                        .await;

                    let preserve_raw = matches!(
                        tool_result.tool.as_str(),
                        "system_info"
                            | "web_search"
                            | "memory_recall"
                            | "memory_status"
                            | "memory_store"
                            | "memory_forget"
                    );

                    if preserve_raw {
                        conversations
                            .append_or_log(&conv_id, "assistant", &tool_result.output, None)
                            .await;
                    } else {
                        // Get follow-up summary.
                        let recent = conversations
                            .get_recent(&conv_id, 6)
                            .await
                            .unwrap_or_default();
                        let mut summary_msgs = vec![Message {
                            role: "system".into(),
                            content: "Summarize the tool result in one sentence.".into(),
                        }];
                        summary_msgs.extend(recent);
                        let (summary_msgs, _) = crate::reasoning::apply_reasoning_mode(
                            model_family,
                            &summary_msgs,
                            "",
                            InteractionKind::ToolSummary,
                        );

                        eprint!("GeniePod: ");
                        let summary_hints = LlmRequestHints::tool_summary(&conv_id, 128);
                        match llm
                            .chat_stream_with_hints(&summary_msgs, Some(128), &summary_hints, |t| {
                                eprint!("{}", t)
                            })
                            .await
                        {
                            Ok(summary) => {
                                eprintln!();
                                conversations
                                    .append_or_log(&conv_id, "assistant", &summary, None)
                                    .await;
                            }
                            Err(_) => eprintln!(),
                        }
                    }
                } else {
                    conversations
                        .append_or_log(&conv_id, "assistant", &response, None)
                        .await;
                }
            }
            Err(e) => {
                eprintln!("\n[ERROR] {}", e);
            }
        }

        // Auto-capture facts.
        let stored = with_shared_memory(memory, |memory| {
            memory::extract::extract_and_store(memory, text)
        });
        if stored > 0 {
            eprintln!(
                "(remembered {} fact{})",
                stored,
                if stored == 1 { "" } else { "s" }
            );
        }
    }

    tracing::info!("REPL exited");
    Ok(())
}
