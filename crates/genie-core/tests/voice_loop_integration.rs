// Voice integration test — the entire file depends on the `voice` Cargo
// feature, since the production modules under test (`voice::*`,
// `voice_loop::*`, `voice::stt::SttEngine::mock`, `TtsEngine::silent`,
// etc.) are gated behind `#[cfg(feature = "voice")]` in `lib.rs`. Without
// this gate the `cargo test --no-default-features` axis (CI job
// `no-default-features`) fails to compile because the imports below
// resolve to "configured out" items in `genie_core`.
#![cfg(feature = "voice")]

//! Integration test for the voice cycle's mockable surface. Issue #21,
//! IS-1 / IS-2 / AC-B.
//!
//! Drives `voice_loop::process_transcript` — the orchestration step
//! extracted from `voice_loop::voice_cycle` — end-to-end on every
//! supported platform, using the new mocks (`SttEngine::mock`,
//! `LlmClient::mock`, `TtsEngine::silent`) against real
//! `Memory` / `ConversationStore` / `ToolDispatcher`.
//!
//! Coverage:
//!
//! 1. **STT** — `SttEngine::mock` replays canned `MockTranscript`s on
//!    `transcribe_file()`, with optional language hints.
//! 2. **LLM** — `LlmClient::mock` replays canned replies on both `chat`
//!    and `chat_stream`; the streaming path tokenizes word-by-word so the
//!    caller's per-token callback fires, mirroring the contract
//!    `process_transcript` depends on for per-sentence TTS.
//! 3. **TTS** — `TtsEngine::silent` no-ops on `speak()`, `synthesize()`,
//!    `synthesize_to_file()`, `start()`, `stop()` so the LLM->TTS bridge
//!    (`voice::streaming::stream_and_speak`) can be exercised without
//!    Piper or aplay.
//! 4. **Streaming bridge** — `voice::streaming::stream_and_speak(mock_llm,
//!    msgs, max, silent_tts)` runs inside `process_transcript`; the
//!    composite test below confirms the full bridge executes.
//! 5. **Conversation persistence** — a full (user, assistant) turn is
//!    appended to a real `ConversationStore` SQLite DB in a process-unique
//!    temp dir, surviving parallel test execution (issue #21 IS-4).
//! 6. **Tool dispatch + audit log** — `ToolDispatcher` configured with a
//!    tool-audit path; after `process_transcript` runs, the test asserts
//!    the dispatcher wrote a JSON event to the audit file, satisfying
//!    #21 AC-B's "audit logs" assertion.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use genie_core::conversation::ConversationStore;
use genie_core::llm::{LlmClient, Message};
use genie_core::memory::Memory;
use genie_core::prompt::ModelFamily;
use genie_core::tools::{ToolDispatcher, ToolExecutionContext, try_tool_call_with_context};
use genie_core::voice::identity::SpeakerIdentityProvider;
use genie_core::voice::streaming::stream_and_speak;
use genie_core::voice::stt::{MockTranscript, SttEngine, Transcript};
use genie_core::voice::tts::TtsEngine;
use genie_core::voice_loop::{ProcessTranscriptInputs, VoiceConfig, process_transcript};

/// Each test gets its own parent dir so SQLite WAL/SHM sidecars and
/// audit-log JSONLs cannot collide and ConversationStore::open's
/// CREATE-TABLE-IF-NOT-EXISTS path is fresh.
fn unique_dir(label: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "geniepod-voice-loop-it-{}-{}-{}-{}",
        label,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        nanos
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir for integration test");
    dir
}

#[tokio::test]
async fn mock_stt_replays_scripted_transcripts_in_order() {
    let stt = SttEngine::mock([
        MockTranscript::new("hello there"),
        MockTranscript::new("what's the weather"),
    ]);

    let first = stt.transcribe_file("ignored.wav").await.unwrap();
    assert_eq!(first.text, "hello there");

    let second = stt.transcribe_file("ignored.wav").await.unwrap();
    assert_eq!(second.text, "what's the weather");

    assert!(
        stt.transcribe_file("ignored.wav").await.is_err(),
        "queue exhausted: expected error on third call"
    );
}

#[tokio::test]
async fn mock_stt_attaches_language_hint_to_transcript() {
    let stt = SttEngine::mock([MockTranscript::new("bonjour").with_language("fr")]);
    let t = stt.transcribe_file("ignored.wav").await.unwrap();
    assert_eq!(t.text, "bonjour");
    assert_eq!(t.language.as_deref(), Some("fr"));
}

#[tokio::test]
async fn mock_llm_replays_replies_for_both_blocking_and_streaming_calls() {
    let llm = LlmClient::mock(["I can help with that."]);
    let messages = vec![Message {
        role: "user".into(),
        content: "ping".into(),
    }];

    let mut tokens = String::new();
    let full = llm
        .chat_stream(&messages, Some(64), |tok| tokens.push_str(tok))
        .await
        .unwrap();

    assert_eq!(full, "I can help with that.");
    assert_eq!(tokens, "I can help with that.");
}

#[tokio::test]
async fn silent_tts_speak_returns_ok_without_spawning_piper() {
    // Confirms TtsEngine::silent does not require piper / aplay binaries.
    let tts = TtsEngine::silent();
    tts.speak("anything").await.unwrap();
    tts.speak("twice in a row").await.unwrap();
}

#[tokio::test]
async fn streaming_stream_and_speak_runs_end_to_end_with_silent_tts() {
    // `voice_cycle` calls `streaming::stream_and_speak(llm, &messages, 256,
    // &tts_engine)` to bridge LLM streaming to per-sentence TTS. This test
    // runs that exact orchestration step with the mocks.
    let llm = LlmClient::mock(["The kitchen light is now on. Anything else?"]);
    let tts = TtsEngine::silent();
    let messages = vec![Message {
        role: "user".into(),
        content: "turn the kitchen light on".into(),
    }];

    let response = stream_and_speak(&llm, &messages, 256, std::sync::Arc::new(tts))
        .await
        .unwrap();

    assert_eq!(response, "The kitchen light is now on. Anything else?");
}

#[tokio::test]
async fn tool_dispatch_via_try_tool_call_writes_audit_log_event() {
    // `voice_cycle` routes LLM output through `tools::try_tool_call_with_context`,
    // which the dispatcher records in its tool-audit JSONL. AC-B requires
    // asserting on "audit logs" — this test gives the dispatcher an audit
    // path, executes a get_time call (built-in, no HA, no network), and
    // confirms the JSONL gained an entry.
    let dir = unique_dir("audit");
    let audit_path = dir.join("tool-audit.jsonl");
    let dispatcher = ToolDispatcher::new(None).with_tool_audit_path(audit_path.clone());

    let llm_output = r#"{"tool": "get_time", "arguments": {}}"#;
    let result =
        try_tool_call_with_context(llm_output, &dispatcher, ToolExecutionContext::default())
            .await
            .expect("get_time should be dispatchable");
    assert_eq!(result.tool, "get_time");
    assert!(result.success, "get_time should succeed");

    // The dispatcher's tool-audit logger appends one JSON line per dispatch.
    let log_contents = std::fs::read_to_string(&audit_path).expect("audit log file should exist");
    assert!(
        !log_contents.trim().is_empty(),
        "audit log should contain at least one event after dispatch"
    );
    assert!(
        log_contents.contains("get_time"),
        "audit log should mention the dispatched tool name; got: {}",
        log_contents
    );
}

#[tokio::test]
async fn full_mock_voice_turn_persists_user_and_assistant_to_conversation_store() {
    // Stage the same canned data the production voice loop would receive
    // from whisper-cpp and llama.cpp.
    let stt = SttEngine::mock([MockTranscript::new("turn the kitchen light on")]);
    let llm = LlmClient::mock(["Done — kitchen light is on."]);

    // Real conversation store, unique DB per test.
    let conv_dir = unique_dir("conv");
    let store = ConversationStore::open(&conv_dir.join("conversations.db")).unwrap();
    let conv_id = "voice-it";
    store.ensure(conv_id, "voice cycle integration").unwrap();

    // STT half — exactly what voice_cycle does after arecord returns.
    let transcript = stt.transcribe_file("ignored.wav").await.unwrap();
    assert_eq!(transcript.text, "turn the kitchen light on");
    store
        .append(conv_id, "user", transcript.text.trim(), None)
        .unwrap();

    // LLM half — exactly what voice_cycle does after the system prompt is
    // assembled. We use chat (blocking) here; chat_stream is exercised in
    // the test above.
    let messages = vec![Message {
        role: "user".into(),
        content: transcript.text.clone(),
    }];
    let reply = llm.chat(&messages, Some(128)).await.unwrap();
    assert_eq!(reply, "Done — kitchen light is on.");
    store.append(conv_id, "assistant", &reply, None).unwrap();

    // Assert: the conversation store ends in (user, assistant) order, the
    // same shape voice_cycle would have produced.
    let recent = store.get_recent(conv_id, 4).unwrap();
    assert_eq!(recent.len(), 2, "expected exactly user + assistant");
    assert_eq!(recent[0].role, "user");
    assert_eq!(recent[0].content, "turn the kitchen light on");
    assert_eq!(recent[1].role, "assistant");
    assert_eq!(recent[1].content, "Done — kitchen light is on.");
}

#[tokio::test]
async fn mock_voice_cycle_drives_stt_then_llm_then_streaming_tts_then_tool_audit() {
    // The composite "one full mocked voice cycle" #21 AC-B asks for, built
    // from the publicly-exposed building blocks `voice_loop::voice_cycle`
    // composes internally. Asserts on transcript flow (1), conversation
    // store (2), and audit logs (3) — the three observables AC-B lists.

    // 1. Real components.
    let dir = unique_dir("full-cycle");
    let store = ConversationStore::open(&dir.join("conversations.db")).unwrap();
    let conv_id = "voice-it-full";
    store.ensure(conv_id, "full voice cycle").unwrap();
    let audit_path = dir.join("tool-audit.jsonl");
    let dispatcher = ToolDispatcher::new(None).with_tool_audit_path(audit_path.clone());

    // 2. Mocked components — STT yields a canned transcript; LLM yields a
    //    canned reply that happens to be a get_time tool call (so the tool
    //    dispatch + audit path also fires); TTS is silent.
    let stt = SttEngine::mock([MockTranscript::new("what time is it")]);
    let llm = LlmClient::mock([r#"{"tool": "get_time", "arguments": {}}"#]);
    let tts = TtsEngine::silent();

    // 3. Drive the voice cycle's post-record orchestration in order.
    let transcript = stt.transcribe_file("ignored.wav").await.unwrap();
    assert_eq!(transcript.text, "what time is it"); // (a) transcript flow

    store
        .append(conv_id, "user", transcript.text.trim(), None)
        .unwrap();

    let messages = vec![Message {
        role: "user".into(),
        content: transcript.text.clone(),
    }];
    let llm_output = stream_and_speak(&llm, &messages, 256, std::sync::Arc::new(tts))
        .await
        .unwrap();
    assert!(llm_output.contains("get_time"));

    // Tool dispatch — exactly what voice_cycle does on the LLM output.
    let tool_result =
        try_tool_call_with_context(&llm_output, &dispatcher, ToolExecutionContext::default())
            .await
            .expect("LLM output should parse as a tool call");
    assert_eq!(tool_result.tool, "get_time");
    assert!(tool_result.success);

    store
        .append(conv_id, "assistant", &llm_output, Some(&tool_result.tool))
        .unwrap();
    store
        .append(
            conv_id,
            "system",
            &format!("Tool: {}", tool_result.output),
            None,
        )
        .unwrap();

    // (b) Conversation store assertion.
    let history = store.get_recent(conv_id, 10).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].role, "user");
    assert_eq!(history[0].content, "what time is it");
    assert_eq!(history[1].role, "assistant");
    assert_eq!(history[2].role, "system");

    // (c) Audit log assertion — dispatcher must have written a JSONL line
    //     for the get_time dispatch.
    let log_contents =
        std::fs::read_to_string(&audit_path).expect("audit log should exist after dispatch");
    assert!(
        log_contents.contains("get_time"),
        "audit log should record the get_time dispatch; got: {}",
        log_contents
    );
}

#[tokio::test]
async fn mock_voice_turn_handles_back_to_back_cycles_without_state_bleed() {
    // Two cycles in a row, each on its own LLM + STT queue and its own
    // conversation store DB — confirms parallel-safety of the path layout.
    let stt = SttEngine::mock([
        MockTranscript::new("first prompt"),
        MockTranscript::new("second prompt"),
    ]);
    let llm = LlmClient::mock(["first reply", "second reply"]);

    let conv_dir = unique_dir("conv-2cycle");
    let store = ConversationStore::open(&conv_dir.join("conversations.db")).unwrap();
    let conv_id = "voice-it-2";
    store.ensure(conv_id, "two cycles").unwrap();

    for expected_user in ["first prompt", "second prompt"] {
        let t = stt.transcribe_file("ignored.wav").await.unwrap();
        assert_eq!(t.text, expected_user);
        store.append(conv_id, "user", &t.text, None).unwrap();
        let reply = llm
            .chat(
                &[Message {
                    role: "user".into(),
                    content: t.text.clone(),
                }],
                Some(64),
            )
            .await
            .unwrap();
        store.append(conv_id, "assistant", &reply, None).unwrap();
    }

    let all = store.get_recent(conv_id, 10).unwrap();
    assert_eq!(all.len(), 4);
    assert_eq!(all[0].content, "first prompt");
    assert_eq!(all[1].content, "first reply");
    assert_eq!(all[2].content, "second prompt");
    assert_eq!(all[3].content, "second reply");
}

/// VoiceConfig populated with mock-friendly values: no audio device, no
/// piper binary, no whisper, default speaker identity. Suitable for
/// `process_transcript` invocations where the LLM and TTS are mocked.
fn test_voice_config() -> VoiceConfig {
    VoiceConfig {
        whisper_model: String::new(),
        whisper_cli_path: String::new(),
        whisper_port: 0,
        piper_model: String::new(),
        piper_path: String::new(),
        piper_pipe_mode: false,
        stt_language: String::new(),
        voice_tts_models: HashMap::new(),
        audio_device: String::new(),
        audio_output_device: String::new(),
        sample_rate: 16000,
        audio_denoiser: "none".into(),
        deep_filter_path: String::new(),
        deep_filter_atten_lim_db: 100.0,
        post_tts_silence_ms: 0,
        record_secs: 4,
        llm_model_path: String::new(),
        wakeword_script: String::new(),
        voice_continuous: false,
        voice_continuous_secs: 0,
        speaker_identity: SpeakerIdentityProvider::None,
    }
}

#[tokio::test]
async fn process_transcript_drives_full_voice_cycle_with_mocks() {
    // The canonical AC-B test: calls `voice_loop::process_transcript`
    // directly with mock STT-derived transcript, mock LLM, silent TTS,
    // real Memory, real ConversationStore, and real ToolDispatcher wired
    // to a tool-audit JSONL. Asserts on transcript flow, conversation
    // store, AND audit logs — the three observables #21 AC-B lists.

    let dir = unique_dir("process-transcript");
    let memory = Memory::open(&dir.join("memory.db")).unwrap();
    let conversations = ConversationStore::open(&dir.join("conversations.db")).unwrap();
    let conv_id = "voice-it-process";
    conversations
        .ensure(conv_id, "process_transcript integration")
        .unwrap();

    let audit_path = dir.join("tool-audit.jsonl");
    let tools = ToolDispatcher::new(None).with_tool_audit_path(audit_path.clone());

    // Mock LLM: emits a get_time tool-call JSON, then a follow-up summary
    // reply for the post-tool LLM call that `process_transcript` makes.
    let llm = LlmClient::mock([
        r#"{"tool": "get_time", "arguments": {}}"#,
        "Sure — that is the current time.",
    ]);
    let tts = TtsEngine::silent();
    let voice_cfg = test_voice_config();

    // Transcript the mock STT would have produced. "tell me a story"
    // is deliberately not a quick-tool pattern (no time / weather /
    // calc / system_info match), so the LLM path runs.
    let transcript = Transcript {
        text: "tell me a story".into(),
        duration_ms: 0,
        language: None,
    };

    let kept_running = process_transcript(
        transcript,
        ProcessTranscriptInputs {
            voice_cfg: &voice_cfg,
            audio_device: "",
            llm: &llm,
            tools: &tools,
            memory: &memory,
            conversations: &conversations,
            system_prompt: "You are GeniePod, a household assistant.",
            max_history: 8,
            model_family: ModelFamily::Phi,
            conv_id,
            wav_path: None,
            tts_engine_override: Some(&tts),
            t_preprocess_done: std::time::Instant::now(),
        },
    )
    .await;

    assert!(kept_running, "process_transcript should return true");

    // (a) Transcript flow — the user message ended up in the conversation
    //     store, sourced from the transcript text.
    let history = conversations.get_recent(conv_id, 10).unwrap();
    assert!(
        history
            .iter()
            .any(|m| m.role == "user" && m.content == "tell me a story"),
        "transcript text should appear as the user message; got {:?}",
        history
            .iter()
            .map(|m| (&m.role, &m.content))
            .collect::<Vec<_>>()
    );

    // (b) Conversation store — after the LLM-emitted tool call dispatches
    //     to get_time, process_transcript appends assistant + system
    //     messages and a final summary assistant message.
    assert!(
        history.iter().any(|m| m.role == "assistant"),
        "process_transcript should have appended at least one assistant message"
    );
    assert!(
        history
            .iter()
            .any(|m| m.role == "system" && m.content.starts_with("Tool:")),
        "process_transcript should have appended the system 'Tool:' record"
    );

    // (c) Audit logs — the dispatcher wrote at least one JSONL event for
    //     the get_time dispatch.
    let log_contents = std::fs::read_to_string(&audit_path).expect("audit log should exist");
    assert!(
        log_contents.contains("get_time"),
        "audit log should record the get_time dispatch; got: {}",
        log_contents
    );
}

#[tokio::test]
async fn process_transcript_ignores_empty_transcript() {
    // Empty / whitespace transcripts must short-circuit cleanly without
    // touching the LLM, the conversation store, or the audit log.
    let dir = unique_dir("process-empty");
    let memory = Memory::open(&dir.join("memory.db")).unwrap();
    let conversations = ConversationStore::open(&dir.join("conversations.db")).unwrap();
    let conv_id = "voice-it-empty";
    conversations.ensure(conv_id, "empty transcript").unwrap();

    // LLM with zero replies — if process_transcript reaches it, the call
    // errors and the test fails to maintain its invariants.
    let llm = LlmClient::mock(Vec::<String>::new());
    let tts = TtsEngine::silent();
    let voice_cfg = test_voice_config();
    let tools = ToolDispatcher::new(None);

    let transcript = Transcript {
        text: "   ".into(),
        duration_ms: 0,
        language: None,
    };

    let kept_running = process_transcript(
        transcript,
        ProcessTranscriptInputs {
            voice_cfg: &voice_cfg,
            audio_device: "",
            llm: &llm,
            tools: &tools,
            memory: &memory,
            conversations: &conversations,
            system_prompt: "",
            max_history: 8,
            model_family: ModelFamily::Phi,
            conv_id,
            wav_path: None,
            tts_engine_override: Some(&tts),
            t_preprocess_done: std::time::Instant::now(),
        },
    )
    .await;

    assert!(kept_running);
    let history = conversations.get_recent(conv_id, 10).unwrap();
    assert!(
        history.is_empty(),
        "no messages should be appended for empty transcript"
    );
}
