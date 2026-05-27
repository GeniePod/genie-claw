//! Integration test for the M1 system-prompt determinism fingerprint (issue #110).
//!
//! M1 exit criterion: "system prompt SHA is identical across full-stack
//! restart." These tests assemble the system prompt the same way `genie-core`
//! does at boot (model-aware [`PromptBuilder`] over the live tool defs and
//! hydrated memory), then hash it with the real SHA-256 in [`prompt_sha`].
//!
//! Two independent assemblies stand in for two stack boots: each uses its own
//! fresh SQLite memory file, mirroring a process restart that re-reads the same
//! persisted configuration and household state.

use std::sync::atomic::{AtomicU32, Ordering};

use genie_core::Memory;
use genie_core::prompt::PromptBuilder;
use genie_core::prompt_sha::sha256_hex;
use genie_core::tools::ToolDispatcher;

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Assemble the boot-time system prompt for `model_name` over a fresh memory
/// file hydrated with `identity_facts`, then return its SHA-256 — the same
/// digest genie-core logs at boot and serves on `/api/health`.
fn assemble_system_prompt_sha(model_name: &str, identity_facts: &[&str]) -> String {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "geniepod-prompt-sha-test-{}-{}.db",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_file(&path);

    let memory = Memory::open(&path).unwrap();
    for &fact in identity_facts {
        memory.store("identity", fact).unwrap();
    }

    let tools = ToolDispatcher::new(None);
    let builder = PromptBuilder::from_model_name(model_name);
    let system_prompt = builder.build(&tools.tool_defs(), &memory);
    let sha = sha256_hex(&system_prompt);

    let _ = std::fs::remove_file(&path);
    sha
}

/// Core acceptance criterion: two stack boots from identical configuration and
/// hydrated state produce an identical system-prompt SHA.
#[test]
fn identical_config_and_state_produce_identical_sha() {
    let model = "Qwen3-4B-Q4_K_M.gguf";
    let facts = ["Household member name is Jared"];

    let boot_a = assemble_system_prompt_sha(model, &facts);
    let boot_b = assemble_system_prompt_sha(model, &facts);

    assert_eq!(
        boot_a, boot_b,
        "same config + hydrated state must hash identically across restarts"
    );
    assert_eq!(boot_a.len(), 64, "expected a 64-char SHA-256 hex digest");
    assert!(boot_a.chars().all(|c| c.is_ascii_hexdigit()));
}

/// Empty-memory boots are deterministic too — the no-hydration baseline must
/// not drift between restarts.
#[test]
fn identical_empty_boots_match() {
    let model = "Qwen3-4B-Q4_K_M.gguf";
    assert_eq!(
        assemble_system_prompt_sha(model, &[]),
        assemble_system_prompt_sha(model, &[]),
    );
}

/// A change in prompt-assembly logic — here the model family selects a
/// different prompt template — must be observable as a different SHA.
#[test]
fn prompt_assembly_change_is_observable_in_sha() {
    // Qwen routes through the "capable model" template; TinyLlama routes
    // through the simpler small-model template. Different assembly => different
    // digest.
    let capable = assemble_system_prompt_sha("Qwen3-4B-Q4_K_M.gguf", &[]);
    let small = assemble_system_prompt_sha("tinyllama-1.1b-chat-v1.0.Q4_K_M.gguf", &[]);

    assert_ne!(
        capable, small,
        "different prompt-assembly logic must change the SHA"
    );
}

/// Hydrated state is part of the fingerprint: adding household context changes
/// the assembled prompt and therefore the SHA.
#[test]
fn hydrated_state_change_is_observable_in_sha() {
    let model = "Qwen3-4B-Q4_K_M.gguf";
    let empty = assemble_system_prompt_sha(model, &[]);
    let hydrated = assemble_system_prompt_sha(model, &["Household member name is Jared"]);

    assert_ne!(
        empty, hydrated,
        "changing hydrated household state must change the SHA"
    );
}
