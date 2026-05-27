<!--
Thanks for contributing to GenieClaw! Please fill this template in honestly.
The "Real Behavior Proof" section is required and enforced by CI.
See CONTRIBUTING.md for the full rules.

NOTE: Strip any AI-attribution footer (e.g. "🤖 Generated with Claude Code")
from your PR body before submitting. Using AI tooling to draft the PR is
fine; attribution in the PR body and in git history stays with the human
contributor. CI enforces this.
-->

## Summary

<!-- 1-3 sentences: what does this PR do and why. Link the issue it closes with `Fixes #NN` or `Closes #NN`. -->

## Changes

<!-- Bullet list of the meaningful changes. -->

-

## Real Behavior Proof

<!--
REQUIRED. CI will fail if this section is missing or empty.
See CONTRIBUTING.md "Required: Real Behavior Proof" for the rules.

The bar is: tell us what you ran, where you ran it, which runtime profile or
hardware it represents, and what happened.
A green CI run is NOT sufficient by itself — it doesn't exercise voice,
audio, Home Assistant, or the dashboard.
-->

- [ ] I have built and run the affected code locally (or noted why I could not).
- [ ] I have verified the change end-to-end on Jetson hardware.
- [ ] I have NOT verified on Jetson hardware, and I explain the equivalent verification path or validation gap below.

Tested profile / hardware (check all that apply):

- [ ] `jetson`
- [ ] `raspberry_pi`
- [ ] `portable_sbc`
- [ ] `laptop`
- [ ] `mac`
- [ ] CI-only / docs-only
- [ ] Not run locally

### What I ran

<!-- Commands you executed, with environment/profile details: Jetson model + L4T version, Raspberry Pi / SBC model + OS, laptop/mac OS, CI-only/docs-only, or "not run locally". -->

### What I observed

<!-- The actual result — log lines, screenshots, latency numbers, error messages, dashboard state, etc. -->

## Test plan

<!-- Optional but encouraged. Steps a reviewer can follow to re-verify on their own hardware. -->

-

## Notes for reviewers

<!-- Anything reviewer-specific: open questions, known limitations, deferred follow-ups. -->
