# Milestone 1 Jetson Agent Harness

Milestone 1 is the current GenieClaw focus: keep the local home agent fast,
reliable, and measurable on NVIDIA Jetson Orin Nano 8GB with a 4096-token
context.

The goal is not a broader assistant, a larger prompt, a hosted provider demo, or
generic product expansion. The goal is a small-context home agent that chooses
the right typed tool, retrieves the right family/home memory, and behaves
deterministically under Jetson constraints.

## Current Focus

- BFCL scoring for quick-router and local-LLM tool-call accuracy
- deterministic typed-tool routing for home control, home status, memory, and safety
- high-signal family memory and home-state retrieval inside the 4096-token budget
- STT-like typo/noise robustness for real voice input
- deterministic device state and structured memory before prompt growth
- Jetson Orin Nano 8GB validation for native, performance-sensitive, and runtime changes
- CI gates that measure regressions before a PR reaches hardware testing

Everything else is noise until routing, memory retrieval, typed-tool accuracy,
BFCL score, and Jetson behavior are stable.

## Non-Negotiables

- GenieClaw is a local home automation agent, not a broad chatbot shell.
- NVIDIA Jetson Orin Nano 8GB is the flagship runtime target.
- The default agent context remains 4096 tokens.
- Larger context windows and stronger providers are optimization paths, not the
  product baseline.
- Device state, memory retrieval, and tool schemas must stay compact enough to
  fit the Jetson context contract.
- PRs that touch routing, memory, tool calls, prompt assembly, latency, native
  runtime behavior, or hardware-facing paths need real tests.
- PRs that make the agent less native, slower, less deterministic, or harder to
  test should be rejected.

## In Scope

- BFCL quick-router and local-LLM scoring
- Home Assistant Intents import and other legally usable public test sources
- generated local stress fixtures under ignored paths such as `tests/bfcl/local/`
- typed tool fixtures for home control, home status, timers, media, memory, and safety
- robust parsing for misspellings, STT errors, slot variation, and short voice utterances
- deterministic fake home/device state for repeatable tests
- family memory retrieval tests that fit the small context budget
- prompt/tool/memory budget checks for the 4096-token harness
- Jetson aarch64 cross-builds and Jetson hardware smoke tests when relevant

## Out Of Scope

- generic AI assistant features
- broad prompt expansion without measured BFCL or retrieval improvement
- hosted-provider churn that does not improve the Jetson contract
- UI, mobile app, hardware product, OS image, and community-growth work
- voice runtime internals except where GenieClaw's agent contract needs a boundary
- demo-only tools or routes that do not improve measured home-agent behavior
- untested native/runtime changes

## M1 Quality Gate

A PR aimed at M1 should explain:

- which home-agent behavior changed
- which typed tools, memory retrieval paths, or deterministic device-state paths it improves
- how it affects the 4096-token harness
- which BFCL, unit, integration, or fake-home tests were added or updated
- whether Jetson Orin Nano 8GB validation is required, completed, or still a gap

Docs-only PRs can use static checks. Code PRs need tests that match the risk.
Native/runtime, performance-sensitive, and hardware-facing changes should be
validated on Jetson whenever possible before merge.

## Immediate Engineering Plan

1. Keep BFCL quick-router and local-LLM scoring easy to run.
2. Expand BFCL fixtures for typed tools, family memory, home state, and STT-like noise.
3. Track expected tool names and arguments, not just natural-language answers.
4. Add score thresholds to CI once the fixture set is stable enough to be a fair gate.
5. Use BFCL failures to improve routing, memory retrieval, and typed-tool accuracy.
6. Preserve the 4096-token prompt/tool/memory budget before adding larger prompts.
7. Validate native and runtime-sensitive behavior on Jetson Orin Nano 8GB.

## Principle

Accuracy should come from deterministic state, typed tools, and reliable memory
retrieval, not from larger prompts.
