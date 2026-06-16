//! Streaming voice pipeline — speak while the LLM is still generating.
//!
//! Unlike the previous implementation (which only sliced the *completed*
//! LLM response into sentences before speaking), this module detects
//! sentence boundaries inside the streaming token callback and forwards
//! each completed sentence to a concurrent TTS task immediately. The
//! first sentence reaches the speaker as soon as the LLM finishes
//! emitting it, not after the entire response is collected.
//!
//! Pipeline:
//!   chat_stream tokens
//!       └─► SentenceStreamer.feed() ──► mpsc ──► TTS task ──► aplay
//!                                                  (Arc<TtsEngine>)
//!
//! The TTS task drains the channel sequentially so audio plays in order
//! and the half-duplex `post_silence_ms` gate still works between
//! sentences.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use super::tts::TtsEngine;

/// Maximum sentences to speak per response. Matches the cap that
/// `format::for_voice` enforces on the non-streaming path, so voice
/// replies stay short whether the caller streams or not.
const MAX_SPOKEN_SENTENCES: usize = 3;

/// Stream the LLM response and speak each sentence in parallel.
///
/// Returns the full assembled response text (untruncated, so callers can
/// log/persist the complete reply even though only the first
/// `MAX_SPOKEN_SENTENCES` are spoken).
pub async fn stream_and_speak(
    llm: &crate::llm::LlmClient,
    messages: &[crate::llm::Message],
    max_tokens: u32,
    tts_engine: Arc<TtsEngine>,
) -> Result<String> {
    stream_and_speak_with_hints(llm, messages, max_tokens, tts_engine, None).await
}

/// Stream the LLM response with optional runtime cache/scheduling hints.
pub async fn stream_and_speak_with_hints(
    llm: &crate::llm::LlmClient,
    messages: &[crate::llm::Message],
    max_tokens: u32,
    tts_engine: Arc<TtsEngine>,
    hints: Option<&crate::llm::LlmRequestHints>,
) -> Result<String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let (pcm_tx, mut pcm_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Synthesis task: reads sentences from the LLM stream, synthesizes each
    // to PCM, and forwards to the playback task. By running concurrently with
    // the playback task, synthesis of sentence N+1 overlaps with playback of
    // sentence N, eliminating the per-sentence synthesis gap.
    let synth_engine = Arc::clone(&tts_engine);
    let synth_task = tokio::spawn(async move {
        while let Some(sentence) = rx.recv().await {
            match synth_engine.synthesize_only(&sentence).await {
                Ok(pcm) => {
                    if pcm_tx.send(pcm).is_err() {
                        break;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "TTS synth error"),
            }
        }
    });

    // Playback task: drains the PCM channel and plays each buffer in order.
    // Post-silence fires once after the last sentence (half-duplex gate).
    let play_engine = Arc::clone(&tts_engine);
    let play_task = tokio::spawn(async move {
        let mut spoken: usize = 0;
        while let Some(pcm) = pcm_rx.recv().await {
            if spoken == 0 {
                eprintln!("[voice] Speaking (streaming, pipelined)...");
            }
            if let Err(e) = play_engine.play_pcm_data(&pcm).await {
                tracing::warn!(error = %e, "TTS playback error on sentence");
            }
            spoken += 1;
        }
        play_engine.post_silence().await;
        spoken
    });

    let mut full_response = String::new();
    let mut streamer = SentenceStreamer::new(MAX_SPOKEN_SENTENCES);

    let stream_result = {
        let mut on_token = |token: &str| {
            full_response.push_str(token);
            for sentence in streamer.feed(token) {
                let _ = tx.send(sentence);
            }
        };
        if let Some(hints) = hints {
            llm.chat_stream_with_hints(messages, Some(max_tokens), hints, &mut on_token)
                .await
        } else {
            llm.chat_stream(messages, Some(max_tokens), &mut on_token)
                .await
        }
    };

    if let Some(tail) = streamer.finish() {
        let _ = tx.send(tail);
    }
    drop(tx);
    // Synthesis task finishes when sentence channel closes; dropping its
    // pcm_tx closes the PCM channel and signals the playback task to stop.
    let _ = synth_task.await;
    // Playback task applies post-silence once after the last sentence.
    let _ = play_task.await;

    stream_result
}

/// Incremental sentence detector with per-sentence voice cleanup.
///
/// The detector consumes raw LLM tokens, tracks markdown code-fence
/// state across calls, strips inline markdown/URLs from each completed
/// sentence, and emits at most `max_sentences` cleaned sentences in
/// total (matching the cap in `format::for_voice`). Sentences shorter
/// than `MIN_SENTENCE_CHARS` are merged with the next one so Piper
/// never gets a tiny fragment that synthesizes into a glitch.
#[derive(Debug)]
pub struct SentenceStreamer {
    pending_sentence: String,
    in_code_fence: bool,
    fence_marker_buffer: String,
    /// Set after we see an ASCII `.!?` so we can check the next char.
    /// Whitespace confirms it as a sentence boundary; anything else
    /// (e.g. the `.` in `example.com` or `3.14`) rejects it. CJK
    /// punctuation `。！？` never sets this — those emit immediately
    /// since CJK convention does not put whitespace after punctuation.
    awaiting_ascii_boundary: bool,
    emitted: usize,
    max_sentences: usize,
}

/// Minimum chars (counted in graphemes, approximated as `chars()`) for a
/// sentence to be emitted on its own. Shorter fragments merge with the
/// following sentence so Piper never gets a 1-4 char glitch like "OK!".
/// Sized to admit dense CJK sentences (~8-12 chars) without merging.
const MIN_SENTENCE_CHARS: usize = 8;

impl SentenceStreamer {
    pub fn new(max_sentences: usize) -> Self {
        Self {
            pending_sentence: String::new(),
            in_code_fence: false,
            fence_marker_buffer: String::new(),
            awaiting_ascii_boundary: false,
            emitted: 0,
            max_sentences,
        }
    }

    /// Feed a chunk of raw LLM output. Returns any complete sentences
    /// that became ready (already cleaned for TTS). Returns an empty
    /// vec once the per-response sentence cap has been reached.
    pub fn feed(&mut self, chunk: &str) -> Vec<String> {
        if self.emitted >= self.max_sentences {
            return Vec::new();
        }
        let mut out = Vec::new();

        for ch in chunk.chars() {
            if self.consume_fence_marker(ch) {
                continue;
            }
            if self.in_code_fence {
                continue;
            }

            // If we set the boundary flag last char, this char decides:
            // whitespace confirms (emit), anything else rejects (URLs,
            // decimals, abbreviations followed immediately by text).
            if self.awaiting_ascii_boundary {
                self.awaiting_ascii_boundary = false;
                if ch.is_whitespace() {
                    if let Some(sentence) = self.try_emit() {
                        out.push(sentence);
                        if self.emitted >= self.max_sentences {
                            return out;
                        }
                    }
                    // Push the confirming whitespace so the next
                    // sentence starts after it; `clean_sentence` later
                    // collapses leading whitespace.
                    self.pending_sentence.push(ch);
                    continue;
                }
            }

            self.pending_sentence.push(ch);

            if is_cjk_boundary(ch) {
                if let Some(sentence) = self.try_emit() {
                    out.push(sentence);
                    if self.emitted >= self.max_sentences {
                        return out;
                    }
                }
            } else if is_ascii_boundary(ch) {
                self.awaiting_ascii_boundary = true;
            }
        }

        out
    }

    /// Try to emit the current pending sentence. Returns the cleaned
    /// sentence and clears the buffer iff it meets `MIN_SENTENCE_CHARS`.
    /// Sub-minimum fragments stay buffered so the next clause merges
    /// in (e.g. "OK!" + " Here is the real answer.").
    fn try_emit(&mut self) -> Option<String> {
        let cleaned = clean_sentence(&self.pending_sentence);
        if cleaned.chars().count() >= MIN_SENTENCE_CHARS {
            self.pending_sentence.clear();
            self.emitted += 1;
            Some(cleaned)
        } else {
            None
        }
    }

    /// Flush any buffered trailing fragment after the LLM stream ends.
    /// Returns the cleaned final fragment; unlike `try_emit`, no
    /// minimum-length check applies because EOF can't be merged into
    /// anything later. Returns `None` only when there is genuinely
    /// nothing to say or the per-response cap has been reached.
    pub fn finish(mut self) -> Option<String> {
        if self.emitted >= self.max_sentences {
            return None;
        }
        // Flush any half-consumed fence marker back into the sentence
        // buffer — if it never matured into a real ``` fence, the
        // partial backticks should still get cleaned and spoken.
        if !self.fence_marker_buffer.is_empty() && !self.in_code_fence {
            let pending = std::mem::take(&mut self.fence_marker_buffer);
            self.pending_sentence.push_str(&pending);
        }

        let trimmed = self.pending_sentence.trim();
        if trimmed.is_empty() {
            return None;
        }
        let cleaned = clean_sentence(trimmed);
        if cleaned.is_empty() {
            return None;
        }
        Some(cleaned)
    }

    /// Track ``` markdown fences across token boundaries. Returns true
    /// when the character was absorbed into the fence marker buffer
    /// (and should not be added to `pending_sentence`).
    fn consume_fence_marker(&mut self, ch: char) -> bool {
        if ch == '`' {
            self.fence_marker_buffer.push(ch);
            if self.fence_marker_buffer.len() == 3 {
                self.in_code_fence = !self.in_code_fence;
                self.fence_marker_buffer.clear();
            }
            return true;
        }

        // Non-backtick: any partially-collected backticks were inline
        // code markers (`like this`), not a fence. Drop them as TTS
        // noise and fall through so the current character is processed
        // normally.
        self.fence_marker_buffer.clear();
        false
    }
}

fn is_ascii_boundary(ch: char) -> bool {
    matches!(ch, '.' | '!' | '?')
}

fn is_cjk_boundary(ch: char) -> bool {
    matches!(ch, '。' | '！' | '？')
}

/// Per-sentence TTS cleanup. The streaming path can't run the full
/// `format::for_voice` pipeline (which is whole-text aware: it strips
/// code blocks, truncates to N sentences, etc.) so this applies the
/// inline subset: strip markdown links, raw URLs, leading list/header
/// markers, common inline emphasis chars, and collapse whitespace.
fn clean_sentence(text: &str) -> String {
    let stripped_links = strip_inline_links(text);
    let stripped_urls = strip_raw_urls(&stripped_links);
    let stripped_prefix = strip_list_or_header_prefix(stripped_urls.trim());
    #[allow(clippy::collapsible_str_replace)]
    let stripped_inline = stripped_prefix
        .replace("**", "")
        .replace("__", "")
        .replace('*', "")
        .replace('`', "");
    let punct_cleaned = stripped_inline
        .replace("...", ", ")
        .replace(" - ", ", ")
        .replace(" — ", ", ")
        .replace(" – ", ", ")
        .replace(['(', ')'], ", ")
        .replace(['[', ']', '{', '}', '"'], "")
        .replace("'s", "s");
    collapse_whitespace(punct_cleaned.trim())
}

fn strip_inline_links(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '[' {
            let mut link_text = String::new();
            let mut closed = false;
            for c in chars.by_ref() {
                if c == ']' {
                    closed = true;
                    break;
                }
                link_text.push(c);
            }
            if closed && chars.peek() == Some(&'(') {
                chars.next();
                for c in chars.by_ref() {
                    if c == ')' {
                        break;
                    }
                }
                result.push_str(&link_text);
            } else {
                // Not a real link — restore the '[' and accumulated text.
                result.push('[');
                result.push_str(&link_text);
                if closed {
                    result.push(']');
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

fn strip_raw_urls(text: &str) -> String {
    text.split_whitespace()
        .filter(|token| {
            let trimmed = token.trim_matches(|c: char| {
                matches!(
                    c,
                    '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':' | '"' | '\''
                )
            });
            !(trimmed.starts_with("http://")
                || trimmed.starts_with("https://")
                || trimmed.starts_with("www."))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_list_or_header_prefix(line: &str) -> String {
    let trimmed = line.trim_start_matches('#').trim_start();
    let trimmed = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("• "))
        .unwrap_or(trimmed);
    strip_numbered_prefix(trimmed).to_string()
}

fn strip_numbered_prefix(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b' ' {
        &line[i + 2..]
    } else {
        line
    }
}

fn collapse_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut last_was_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_was_space && !result.is_empty() {
                result.push(' ');
                last_was_space = true;
            }
        } else {
            result.push(ch);
            last_was_space = false;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(streamer: &mut SentenceStreamer, chunks: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(streamer.feed(chunk));
        }
        out
    }

    #[test]
    fn emits_first_sentence_mid_stream() {
        let mut s = SentenceStreamer::new(3);
        let mut emitted = Vec::new();
        emitted.extend(s.feed("Hello there, this is the first sentence. "));
        assert_eq!(emitted.len(), 1, "first sentence should fire before EOF");
        assert!(emitted[0].contains("Hello there"));
        emitted.extend(s.feed("And here is the second one!"));
        // The trailing '!' has no whitespace after it yet, so finish()
        // is what flushes the second sentence at EOF.
        if let Some(tail) = s.finish() {
            emitted.push(tail);
        }
        assert_eq!(emitted.len(), 2);
        assert!(emitted[1].contains("second one"));
    }

    #[test]
    fn handles_partial_token_chunks() {
        let mut s = SentenceStreamer::new(3);
        let chunks = ["Hel", "lo wor", "ld, how a", "re you do", "ing today?"];
        let mut emitted = feed_all(&mut s, &chunks);
        if let Some(tail) = s.finish() {
            emitted.push(tail);
        }
        assert_eq!(emitted.len(), 1);
        assert!(emitted[0].contains("Hello world"));
        assert!(emitted[0].contains("doing today"));
    }

    #[test]
    fn caps_at_max_sentences() {
        let mut s = SentenceStreamer::new(2);
        let emitted = feed_all(
            &mut s,
            &[
                "First sentence is long enough. ",
                "Second sentence is also fine. ",
                "Third should be dropped. ",
                "Fourth too.",
            ],
        );
        assert_eq!(emitted.len(), 2);
        assert!(s.finish().is_none());
    }

    #[test]
    fn merges_too_short_opener_with_next_clause() {
        let mut s = SentenceStreamer::new(3);
        let mut emitted = feed_all(&mut s, &["OK! Here is the real answer to your question."]);
        // "OK!" alone is below MIN; "...question." sets the ASCII
        // boundary flag but no trailing whitespace confirms it, so the
        // merged sentence only flushes at finish() (EOF).
        if let Some(tail) = s.finish() {
            emitted.push(tail);
        }
        assert_eq!(emitted.len(), 1);
        assert!(emitted[0].contains("OK"));
        assert!(emitted[0].contains("real answer"));
    }

    #[test]
    fn finish_flushes_trailing_fragment() {
        let mut s = SentenceStreamer::new(3);
        let _ = s.feed("Hello world and the rest goes here without final punctuation");
        let tail = s.finish().expect("trailing fragment");
        assert!(tail.contains("Hello world"));
    }

    #[test]
    fn finish_returns_none_for_empty_input() {
        let s = SentenceStreamer::new(3);
        assert!(s.finish().is_none());
    }

    #[test]
    fn strips_markdown_bold_inline() {
        let mut s = SentenceStreamer::new(3);
        let emitted = feed_all(&mut s, &["The **weather** is sunny right now. "]);
        assert_eq!(emitted.len(), 1);
        assert!(!emitted[0].contains('*'));
        assert!(emitted[0].contains("weather"));
    }

    #[test]
    fn strips_markdown_links() {
        let mut s = SentenceStreamer::new(3);
        let emitted = feed_all(
            &mut s,
            &["See [this guide](https://example.com/docs) for more details about it. "],
        );
        assert_eq!(emitted.len(), 1);
        assert!(emitted[0].contains("this guide"));
        assert!(!emitted[0].contains("https://"));
        assert!(!emitted[0].contains('['));
    }

    #[test]
    fn strips_raw_urls() {
        let mut s = SentenceStreamer::new(3);
        let emitted = feed_all(
            &mut s,
            &["Top result was found at https://example.com/x today. "],
        );
        assert_eq!(emitted.len(), 1);
        assert!(!emitted[0].contains("https://"));
        assert!(emitted[0].contains("Top result"));
    }

    #[test]
    fn skips_fenced_code_blocks() {
        let mut s = SentenceStreamer::new(3);
        let emitted = feed_all(
            &mut s,
            &[
                "Here is the code block I prepared.\n",
                "```\nlet x = 5;\nprint(x);\n```\n",
                "That is everything you need to know.",
            ],
        );
        // First sentence ends after "prepared." — emitted mid-stream.
        // Code block is suppressed. Trailing sentence flushed on finish.
        assert!(emitted.iter().any(|s| s.contains("code block")));
        assert!(emitted.iter().all(|s| !s.contains("let x")));
        let tail = s.finish().unwrap_or_default();
        let combined: String = emitted.join(" ") + " " + &tail;
        assert!(!combined.contains("let x"));
        assert!(combined.contains("everything you need"));
    }

    #[test]
    fn fence_marker_split_across_chunks() {
        let mut s = SentenceStreamer::new(3);
        let emitted = feed_all(
            &mut s,
            &[
                "Before block. Here is text. ",
                "`",
                "`",
                "`",
                "let y = 1;",
                "`",
                "`",
                "`",
                " After block continues here now.",
            ],
        );
        let tail = s.finish().unwrap_or_default();
        let combined = emitted.join(" ") + " " + &tail;
        assert!(combined.contains("Before block") || combined.contains("Here is text"));
        assert!(!combined.contains("let y"));
        assert!(combined.contains("After block"));
    }

    #[test]
    fn strips_bullet_prefix_at_sentence_start() {
        let mut s = SentenceStreamer::new(3);
        let emitted = feed_all(&mut s, &["- The first bullet item is right here. "]);
        assert_eq!(emitted.len(), 1);
        assert!(!emitted[0].starts_with("- "));
        assert!(emitted[0].starts_with("The first"));
    }

    #[test]
    fn handles_chinese_punctuation() {
        let mut s = SentenceStreamer::new(3);
        let emitted = feed_all(&mut s, &["这是第一句话已经说完了。这是第二句话也结束了！"]);
        assert_eq!(emitted.len(), 2);
    }

    #[test]
    fn empty_stream_finishes_quietly() {
        let mut s = SentenceStreamer::new(3);
        let emitted = feed_all(&mut s, &["", "", ""]);
        assert!(emitted.is_empty());
        assert!(s.finish().is_none());
    }
}
