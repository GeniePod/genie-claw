# Changelog

## 1.0.0-alpha.4 - 2026-04-25

Alpha 4 is a control-plane hardening release. It moves GenieClaw closer to a
safe local physical agent by making runtime state, tool use, actuation, and
native skills observable and policy-controlled.

### Added

- Runtime contract endpoint and boot log for prompt, tool, policy, and
  hydration fingerprints.
- Optional runtime contract drift detection through
  `[core].expected_runtime_contract_hash`.
- `genie-ctl support-bundle` for local field diagnostics.
- Privacy-preserving tool audit log at `<data_dir>/runtime/tool-audit.jsonl`.
- Actuation channel allowlist and per-origin physical-action rate limits.
- Origin-aware tool policy through `[core.tool_policy]`.
- Native skill sidecar manifest audit metadata.
- Configurable native skill load policy through `[core.skill_policy]`.
- Support-bundle tails for runtime contract, tool audit, and actuation audit logs.

### Changed

- Skill listing now reports manifest status, permissions, capabilities, review
  identity, and signing-material presence.
- Runtime policy status now exposes tool policy, tool audit status, actuation
  limits, skill policy, and loaded skill manifest metadata.
- Documentation now separates current implementation from later work such as
  cryptographic skill signatures and stronger native skill sandboxing.

### Notes

- Skill signature checking is presence-only in this alpha; cryptographic
  verification is still future signed-skill-platform work.
- Tool audit intentionally records argument keys and output length, not argument
  values or outputs.
- Defaults preserve current behavior unless an operator enables stricter
  `skill_policy`, `tool_policy`, or actuation origin/rate settings.
