//! # genie-core
//!
//! Core runtime for GeniePod Home.
//!
//! Built for the GeniePod Home appliance, but exposed as modular Rust components.
//! Any Rust project can use these modules independently:
//!
//! ```rust,no_run
//! use genie_core::llm::LlmClient;
//! use genie_core::ha::HomeAssistantProvider;
//! use genie_core::tools::ToolDispatcher;
//! use genie_core::memory::Memory;
//! ```
//!
//! ## Modules
//!
//! | Module | What it does |
//! |--------|-------------|
//! | [`llm`] | OpenAI-compatible local LLM client (llama.cpp, Ollama, any API) |
//! | [`ha`] | Home Assistant provider boundary, structure cache, and REST client |
//! | [`tools`] | Compiled tool dispatch + parser for LLM JSON output |
//! | [`memory`] | SQLite + FTS5 persistent memory with confidence decay |
//! | [`conversation`] | Multi-session persistent conversation store |
//! | [`connectivity`] | Boundary for an ESP32-C6 UART Thread/Matter coprocessor |
//! | [`context`] | LLM context window management with summarization |
//! | [`prompt`] | Model-aware system prompt builder (6 LLM families) |
//! | [`voice`] | STT/TTS subprocess management + voice output formatter |
//! | [`ota`] | OTA update checker via GitHub Releases |
//! | [`server`] | HTTP chat API server |
//!
//! ## Design principles
//!
//! - **No HTTP framework** — raw tokio TcpListener (keeps binary small)
//! - **No AI framework** — direct OpenAI API over TCP (no langchain, no autogen)
//! - **Bundled SQLite** — no external database dependency
//! - **Single-threaded** — `tokio::main(flavor = "current_thread")`
//! - **AGPL-3.0-only** — network-facing modifications must stay available to users

// Allow dead code during development — modules are built incrementally.
#![allow(dead_code, unused_variables, unused_assignments)]
#![allow(clippy::too_many_arguments, clippy::empty_line_after_doc_comments)]

pub mod connectivity;
pub mod context;
pub mod conversation;
pub mod ha;
pub mod llm;
pub mod memory;
pub mod ota;
pub mod profile;
pub mod prompt;
pub mod reasoning;
pub mod repl;
pub mod runtime_contract;
pub mod security;
pub mod server;
pub mod skills;
#[cfg(feature = "telegram")]
pub mod telegram;
pub mod tools;
pub mod voice;
pub mod voice_loop;

// Re-export key types at crate root for convenience.
pub use connectivity::{
    ConnectivityCapability, ConnectivityController, ConnectivityFrame, ConnectivityHealth,
    ConnectivityState, NullConnectivityController,
};
pub use conversation::ConversationStore;
pub use ha::{HaClient, HomeAssistantProvider, HomeAutomationProvider};
pub use llm::{LlmClient, Message};
pub use memory::Memory;
pub use prompt::PromptBuilder;
pub use tools::{ToolCall, ToolDispatcher, ToolResult};
