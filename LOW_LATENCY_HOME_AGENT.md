# Low-Latency Private Home Agent Goal

This repository is moving toward one product goal:

GenieClaw should be the low-latency, limited-context, on-device AI harness for a
private household agent. Accuracy should come from the right home context, typed
memory, deterministic tools, and local device state, not from sending bigger
prompts to a bigger remote model.

## Product Target

The flagship target is NVIDIA Jetson Orin 8GB hardware.

Raspberry Pi and generic portable SBC profiles are maintained as smaller
deployment and validation targets. They do not need to expose every
Jetson-specific voice, CUDA, or model-runtime feature, but they should keep the
headless agent, family memory, tools, HTTP/CLI surfaces, and home-runtime
boundaries usable.

The agent should:

- answer quickly on local hardware
- stay useful inside a small context window
- preserve household privacy by default
- remember family and household facts as structured local memory
- control IoT devices through typed local interfaces
- degrade gracefully without the internet
- expose audit, confirmation, and memory-management surfaces to the household
- preserve the same limited-context contract across maintained SBC profiles

Cloud or remote model providers are not the product default. OpenAI-compatible
API providers, OpenAI, Anthropic, Gemini, custom providers, and similar adapters
exist only for better testing, development portability, and transitional
validation while the local runtime and harness mature.

## Context Strategy

The default Jetson contract remains 4096 tokens, but ordinary turns should use
less than that whenever possible.

The harness should prefer:

- compact system prompt
- short tool manifest
- top-k relevant memory facts
- current room/device state
- recent action state
- small response reserve
- deterministic fast paths for obvious utility requests

The harness should avoid:

- dumping raw conversation history
- injecting every memory
- carrying stale room/device facts
- using large remote context as a shortcut
- making the model infer state that a typed tool already knows

## Home Context Harness

The home context passed to the model should be curated and explicit.

Recommended shape:

```text
agent policy
family identity and speaker context
top relevant memories
current room or device graph slice
recent action summary
available tools and confirmation policy
user request
```

This keeps prompts small and makes answers more accurate. The model should see
only the context needed for the current turn.

## Family Memory

Household memory is a first-class local subsystem.

Memory should be:

- local by default
- structured with scope, sensitivity, and spoken policy
- editable and forgettable by the household
- retrieved by relevance, not dumped wholesale
- safe for shared-room voice responses
- independent of any particular LLM provider

The memory path should optimize for high-signal facts:

- names and family roles
- preferences
- recurring routines
- household device aliases
- safe durable facts
- recent actions that help explain what happened

Secrets, credentials, one-time codes, payment details, and hostile-user security
decisions do not belong in ordinary agent memory.

## Direct Local IoT Direction

The long-term home-control path should be direct, local, and AI-friendly.

GenieClaw should not become a wireless driver stack, but it should integrate
with a first-party local device graph and actuation boundary. The target split
is:

- The platform/OS layer owns radios, Linux interfaces, drivers, and service
  supervision.
- The external home boundary owns device graph, Matter/Thread/Zigbee/BLE
  adapters, automations, and final physical actuation safety.
- `genie-claw` owns user intent, memory, policy, confirmations, audit, and
  tool routing.

Home Assistant is useful today as a transitional provider. It should stay behind
a provider boundary so it can be replaced by the native home runtime without
rewriting prompt, memory, or channel code.

## Latency Rules

Low latency is a product requirement.

Prioritize:

- local `genie-ai-runtime`
- prompt-prefix reuse and stable prompt hashes
- short history windows
- fast deterministic routing before LLM calls
- hot local STT/TTS services when voice is enabled
- bounded read/request timeouts
- tool execution over model-only reasoning for known facts

Do not optimize by:

- raising context size by default
- enabling remote providers by default
- adding heavyweight retrieval to every request
- making voice or chat wait for optional services
- coupling core chat availability to wireless bring-up

## Provider Policy

Optional AI providers are transitional development and testing tools.

They are useful for:

- CI and contributor validation without Jetson hardware
- comparing tool-call behavior
- testing OAuth/API-key plumbing
- debugging context-budget and streaming behavior
- validating that the harness remains provider-agnostic

They are not:

- the default product path
- a replacement for local `genie-ai-runtime`
- a reason to increase default context
- a place to send household memory by default
- a way around local privacy and actuation policy

Any provider path must pass the same limited-context harness before it is used
for serious validation.

## Acceptance Direction

Future PRs should move the system toward:

- smaller active prompt inputs for common turns
- better memory selection instead of larger memory injection
- more deterministic home intent routing
- local device graph and wireless/IoT boundaries
- stronger audit and confirmation behavior
- better Jetson latency proof
- portable test harnesses that do not weaken the final on-device product

The long-term result should be a private, on-device, home-native agent that is
fast because it knows the household context, not because it depends on a remote
general-purpose assistant.
