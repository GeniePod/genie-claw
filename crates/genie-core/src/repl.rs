use anyhow::Result;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::conversation::ConversationStore;
use crate::llm::{LlmClient, Message};
use crate::memory::{self, Memory};
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
    memory: &Memory,
    conversations: &ConversationStore,
    system_prompt: &str,
    max_history: usize,
    model_family: ModelFamily,
) -> Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    // Get or create a REPL conversation.
    let conv_id = conversations.create()?;
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

        // Persist user message.
        let _ = conversations.append(&conv_id, "user", text, None);

        // Build context with per-query memory injection.
        let memory_context = memory::inject::build_memory_context(memory, text);
        let full_prompt = format!(
            "{}\n\nRelevant household context:\n{}",
            system_prompt, memory_context
        );

        let history = conversations
            .get_recent(&conv_id, max_history)
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
        match llm
            .chat_stream(&messages, Some(512), |token| {
                eprint!("{}", token);
            })
            .await
        {
            Ok(response) => {
                eprintln!();

                // Check for tool call.
                if let Some(tool_result) = tools::try_tool_call(&response, tools_dispatch).await {
                    eprintln!("[TOOL: {}] {}", tool_result.tool, tool_result.output);
                    let _ = conversations.append(
                        &conv_id,
                        "assistant",
                        &response,
                        Some(&tool_result.tool),
                    );
                    let _ = conversations.append(
                        &conv_id,
                        "system",
                        &format!("Tool: {}", tool_result.output),
                        None,
                    );

                    // Get follow-up summary.
                    let recent = conversations.get_recent(&conv_id, 6).unwrap_or_default();
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
                    match llm
                        .chat_stream(&summary_msgs, Some(128), |t| eprint!("{}", t))
                        .await
                    {
                        Ok(summary) => {
                            eprintln!();
                            let _ = conversations.append(&conv_id, "assistant", &summary, None);
                        }
                        Err(_) => eprintln!(),
                    }
                } else {
                    let _ = conversations.append(&conv_id, "assistant", &response, None);
                }
            }
            Err(e) => {
                eprintln!("\n[ERROR] {}", e);
            }
        }

        // Auto-capture facts.
        let stored = memory::extract::extract_and_store(memory, text);
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
