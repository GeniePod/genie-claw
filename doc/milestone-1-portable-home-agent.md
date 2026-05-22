# Milestone 1 Portable Home Agent Architecture

Milestone 1 should keep GenieClaw focused on GeniePod Home while making the
project easier to validate without Jetson hardware.

The goal is not to turn GenieClaw into a generic hosted assistant. The goal is
to keep the Jetson-first home AI agent intact and make most contribution paths
portable, deterministic, and reviewable by CI.

## Why This Matters

The original target remains specific:

- GeniePod Home on Jetson-class hardware
- local-first voice interaction
- `genie-ai-runtime` as the flagship local LLM runtime
- Home Assistant today, `genie-home-runtime` as the longer-term home boundary
- private memory, safety gates, audit trails, and bounded physical actuation
- small context windows because VRAM and latency are tight on edge hardware

The contribution problem is that most developers do not own Jetson hardware.
When a PR can only be validated on a real device, quality depends on maintainer
manual testing. That does not scale.

Milestone 1 should make the important behavior testable on ordinary developer
machines while preserving the final Jetson appliance shape.

## Non-Negotiables

- GenieClaw remains a home automation AI agent, not a broad chatbot shell.
- Jetson remains the default flagship target.
- The full default-feature build must continue to represent the original
  GeniePod Home goal: local runtime, voice, home control, memory, safety, audit,
  and `genie-ai-runtime` integration.
- Small context is a product constraint. The baseline target is currently 4096
  tokens.
- Every provider path must work correctly under the small-context contract before
  larger adaptive context is treated as an optimization.
- API-key providers must be optional and must not materially bloat the final
  device image. Small config/type overhead is acceptable; heavy provider
  dependencies should be feature-gated.
- Voice remains optional for headless devices, laptops, CI, and remote
  development.

## Runtime Profiles

The project should name and test distinct runtime profiles.

| Profile | Purpose |
| --- | --- |
| `geniepod-full` | Flagship deployment: Jetson, `genie-ai-runtime`, voice, home automation, memory, safety, and audit. |
| `portable-home` | Development and non-Jetson installs: Home Assistant or fake home runtime, optional API-compatible LLM provider, headless or local UI. |
| `headless-agent` | No audio stack. Runs HTTP/chat/tool APIs and agent behavior for servers, laptops, CI, and SBCs. |
| `contributor-ci` | Deterministic validation profile: mock LLM, fake home automation, fixed memory fixtures, no hardware or API keys. |

These profiles are validation and packaging tools. They do not change the
product identity.

## Provider Contract

LLM providers should sit behind one contract. The agent should not learn
provider-specific details outside that boundary.

Provider capabilities should describe:

- provider id
- local or remote execution
- streaming support
- JSON / tool-call reliability
- maximum context tokens
- recommended context tokens
- default output-token reserve
- required API key environment variable, if any
- whether the provider is compiled into the current binary

All provider requests should pass through the same context budgeter before the
provider sees them.

## Small-Context Budget Contract

The default small-context contract should be explicit and testable.

Baseline target:

```toml
[agent.context]
default_max_tokens = 4096
output_reserve_tokens = 512
system_prompt_max_tokens = 900
tool_schema_max_tokens = 900
memory_max_tokens = 700
history_max_tokens = 900
```

Larger providers may use adaptive context, but they must still pass the 4096
baseline tests. Strong remote models are an enhancement path, not the baseline
contract.

## Contributor Test Harness

Milestone 1 should establish a hardware-free validation harness:

- mock LLM provider for deterministic text, JSON, and streaming responses
- fake home automation provider with controllable entity state and failures
- golden conversation tests for tool calls, home actions, undo, and memory
- provider compliance tests for missing API keys, health checks, streaming
  cancellation, and JSON/tool-call handling
- context-budget tests that prove the default prompt/tool/memory assembly fits
  within the 4096-token target

Jetson tests still matter, but they should be release and hardware-behavior
proof, not the only way to review ordinary agent logic.

## PR Validation Tiers

Review should map PRs to the smallest meaningful proof tier.

| Tier | Applies to | Expected proof |
| --- | --- | --- |
| 1 | Pure logic or docs | Unit tests or docs review. |
| 2 | Agent behavior | Golden conversation test with mock LLM and fake home automation. |
| 3 | Provider integration | Mock HTTP/provider compliance test; real API-key run optional. |
| 4 | Jetson/audio/runtime behavior | Cross-compile plus maintainer or contributor hardware smoke test before release. |

This makes PR expectations clearer for contributors and reduces maintainer
guesswork.

## CI Direction

CI should eventually include:

- default workspace fmt, clippy, and tests
- no-default-features headless build
- contributor profile tests with mock LLM and fake home automation
- provider-contract tests without real API keys
- small-context budget tests
- aarch64 cross-compile for Jetson release confidence

The CI goal is not to emulate a Jetson perfectly. The goal is to catch
architecture, prompt, provider, memory, and tool regressions before hardware
validation is needed.

## Migration Path

1. Document the architecture movement and PR validation tiers.
2. Add a `genie-testkit` or equivalent internal test harness.
3. Add the small-context budget contract and tests.
4. Introduce a mock LLM provider for deterministic agent tests.
5. Introduce a fake home automation provider for tool and safety tests.
6. Formalize provider capabilities and compliance tests.
7. Add optional API-compatible provider support behind features.
8. Keep `geniepod-full` as the flagship release target.

## Principle

The community path should be portable.

The product identity should stay edge-first, private, and home-native.
