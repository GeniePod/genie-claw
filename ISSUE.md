# [bug] security: `LoopGuard` circuit-breaker and `Tainted<T>` IFC are dead code — zero wiring into any live chat, voice, or Telegram path

**Labels:** `bug`, `security`, `critical`

---

## Summary

`security/loop_guard.rs` and `security/taint.rs` are fully implemented but never instantiated or invoked in any production execution path — the tool-call circuit-breaker and the information-flow control (IFC) layer have zero effect on runtime behavior, creating a false security narrative while providing no actual protection.

---

## Steps to reproduce

```bash
# LoopGuard has no callers outside its own module
grep -rn "LoopGuard\|LoopCheck\|loop_guard" \
  crates/genie-core/src/ --include="*.rs" \
  | grep -v "security/loop_guard.rs" \
  | grep -v "security/mod.rs"
# → (empty)

# Tainted / TaintLabel / TaintSink have no callers
grep -rn "Tainted\|TaintLabel\|TaintSink" \
  crates/genie-core/src/ --include="*.rs" \
  | grep -v "security/taint.rs" \
  | grep -v "security/mod.rs"
# → (empty)
```

Every live turn goes through one of five paths — none create a `LoopGuard`, call `LoopGuard::check()`, apply any `TaintLabel`, or call `Tainted::check_sink()`:

| Entry point | File |
|---|---|
| HTTP streaming chat | `server.rs` → `process_chat_stream()` |
| HTTP non-streaming chat | `server.rs` → `process_chat_turn()` |
| REPL | `repl.rs` → `run()` |
| Voice loop | `voice_loop.rs` → `run()` |
| Telegram adapter | `telegram.rs` → `handle_update()` |

---

## Expected behavior

**`LoopGuard`:** every tool execution within a conversation turn is checked against the per-turn circuit-breaker. Calls to the same tool + args beyond `max_repeat_calls`, total calls beyond `max_total_calls`, or A→B→A→B ping-pong beyond `max_pingpong_cycles` are blocked before the tool dispatches and a clear error is surfaced to the LLM.

**`Tainted<T>`:** data from external APIs (weather, web search) carries `TaintLabel::ExternalNetwork`; LLM outputs carry `TaintLabel::LlmOutput`. Before a value reaches the user, `check_sink(TaintSink::DisplayToUser)` must pass (blocks `Secret`). Before a value is sent to an external network endpoint, `check_sink(TaintSink::NetworkSend)` must pass (blocks `Secret` and `Pii`). Before external-network data enters a side-effecting tool, `check_sink(TaintSink::ToolExec)` must block the flow.

---

## Actual behavior

Neither module executes at runtime. Both are compiled, unit-tested, and exported from `security/mod.rs`, but nothing in live code ever calls them.

* **`LoopGuard::check()` is never called.** An adversarially prompted or looping LLM can trigger repeated tool executions across turns with no accumulated call counter — each turn re-enters tool dispatch with a clean slate. The configured `max_total_calls = 20` and `max_repeat_calls = 3` thresholds never fire.

* **`Tainted<T>` labels are never applied.** A web-search result, a weather API body, or an LLM-hallucinated token string flows from tool execution back to the user with no taint annotation and no sink check. The `TaintSink::NetworkSend` policy that would block `Secret`-labeled values from reaching external services is never invoked.

The injection scanner (`security/injection.rs`) and output sanitizer (`security/sandbox.rs::sanitize_output`) are wired correctly; the gap is exclusively `loop_guard` and `taint`.

---

## Concrete impact scenarios

### 1 — Tool-call flood via crafted conversation history

An attacker with access to the Telegram adapter (`allow_all_chats = true`) or the API sends a sequence of turns engineered to cause the LLM to emit the same high-cost tool call (e.g., `web_search`, `home_control`) on every reply. On Jetson Orin Nano 8 GB, each `web_search` spawns an outbound HTTP request plus a second LLM call for summarization. Without a circuit-breaker, this flood exhausts the bounded LLM request queue and degrades all concurrent sessions. The `LoopGuard` thresholds in `LoopGuardConfig::default()` exist precisely for this scenario — they just never execute.

### 2 — PII leakage from memory injection to external search

`memory::inject::build_memory_context()` hydrates household context strings (names, relationships, schedules) into the system prompt. If the LLM then issues a `web_search` and includes PII from the hydrated context in the query argument, that PII is forwarded to the configured SearXNG instance — which may be a public one. `TaintSink::NetworkSend` blocks `TaintLabel::Pii` from reaching external sinks; it is never invoked, so the block never fires.

### 3 — Credential fragment echo through LLM summary

If a tool result (e.g., a 401 error body from `home_control`) inadvertently includes a fragment of the HA token, the LLM summary call in `finalize_tool_turn()` may echo that fragment in its natural-language response. `TaintSink::DisplayToUser` blocks `TaintLabel::Secret`; it is never invoked.

---

## Hardware

- Jetson Orin Nano Super 8 GB (all five call paths are live in production)
- Non-Jetson x86_64 — also affected (REPL + HTTP paths present on all platforms)

## JetPack / L4T version

Any — the bug is architectural, not hardware-specific.

## GenieClaw version / commit

Confirmed present on `HEAD` (`fix/sandbox/enforce-landlock-ruleset`, commit `4c70bb7`). Both modules were introduced early in the security layer and have never been connected to a call site.

## Relevant logs

No log output — neither module produces log lines because neither is ever called. Absence of any line matching `loop_guard: blocked` or `taint: sink violation` in `journalctl -u genie-core` after a tool-heavy session is the indicator.

---

## Files requiring structural changes

| File | Change required |
|---|---|
| `crates/genie-core/src/server.rs` | Instantiate `LoopGuard` at turn start; pass `&mut LoopGuard` into `try_tool_call_with_context()`; call `guard.check(name, args)` before dispatch; call `guard.reset()` at turn end. Apply `check_sink(TaintSink::DisplayToUser)` on tool output before writing to the response stream. |
| `crates/genie-core/src/repl.rs` | Same LoopGuard and taint-sink wiring as `server.rs`. |
| `crates/genie-core/src/voice_loop.rs` | Same LoopGuard and taint-sink wiring. |
| `crates/genie-core/src/telegram.rs` | Same wiring; LoopGuard state scoped to the per-chat lock's critical section so it resets between Telegram turns. |
| `crates/genie-core/src/tools/dispatch.rs` | `execute_with_context()` accepts `&mut LoopGuard`; calls `check()` before dispatching any tool; return type carries a `DataOrigin` tag so callers can apply taint labels. |
| `crates/genie-core/src/tools/weather.rs` | Tag HTTP response body with `TaintLabel::ExternalNetwork` before returning `ToolResult`. |
| `crates/genie-core/src/tools/web_search.rs` | Same: tag SearXNG response with `TaintLabel::ExternalNetwork`. |
| `crates/genie-core/src/tools/mod.rs` | Re-export updated signatures; expose `DataOrigin` or `Tainted<ToolResult>` to call sites. |
| `crates/genie-core/src/security/loop_guard.rs` | Expose per-origin `LoopGuardConfig` overrides (e.g., stricter limits for `Telegram` / `Api` origins vs `Voice` / `Dashboard`). |
| `crates/genie-core/src/security/taint.rs` | Add `Tainted::from_tool_result(result, origin)` constructor and a `DataOrigin` enum mirroring `ToolActionClass::Network` → `TaintLabel::ExternalNetwork`. |

---

## Additional context

This parallels earlier dead-code security bugs in this codebase:

* **#286** — `CredentialStore` was dead code (secrets stored as plain `String`s). Fixed.
* **#181** — `RetryLlmClient` was dead code; `chat_turn_lock` unbounded. Fixed.
* **#196** — `scan_and_warn` wired only to the OpenAI-compat bridge. Fixed.

The pattern recurs here at a higher level: both `LoopGuard` and `Tainted<T>` are well-implemented, well-tested modules that are simply never called from any production code path. Unlike the prior cases, this gap spans all five entry points and multiple tool implementations, making it a broader structural issue requiring coordinated changes across the tool dispatch pipeline.

**Minimum viable fix:**

1. Thread `&mut LoopGuard` through the five chat-turn entry points listed above.
2. Call `guard.check(name, args_json)` in `ToolDispatcher::execute_with_context()` before any tool runs; surface `LoopCheck::Block` as a `ToolResult` error so the LLM sees a clear rejection instead of a silent no-op.
3. Add `TaintLabel::ExternalNetwork` to `ToolResult::output` for `ToolActionClass::Network` results (weather, web_search) and call `check_sink(TaintSink::DisplayToUser)` in the response-finalization helpers.

**Complete fix also:**

4. Expose per-origin `LoopGuardConfig` overrides in `[core.tool_policy]` (stricter limits for remote/Telegram origins, relaxed for Voice/Dashboard).
5. Propagate `TaintLabel::Pii` when memory injection hydrates named-person context strings, and enforce `TaintSink::NetworkSend` before those values reach `web_search` query arguments.
