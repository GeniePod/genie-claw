# Security Policy

## Reporting a Vulnerability

**Please do not file security issues on the public GitHub tracker.** Public disclosure of an unpatched issue puts every GenieClaw deployment at risk.

Email the security team privately at **<contact@genieclaw.org>** with:

- A description of the issue and where in the codebase it lives (file paths, line numbers, commit SHA if known).
- Steps to reproduce.
- The potential impact — data exposure, remote code execution, denial of service, privilege escalation, information leak, tampering with audit logs, etc.
- Any proof-of-concept you've developed.
- Your contact info for follow-up.

If you do not have email available, open a private security advisory via the [GitHub Security Advisories](https://github.com/GeniePod/genie-claw/security/advisories/new) flow instead. Do **not** open a regular public issue or pull request.

## What We Treat as Security-Sensitive

In a local home AI runtime that touches voice, household memory, Home Assistant actuation, and tool execution, the following are all in scope:

- Anything that could let an unauthenticated caller execute tools, actuate Home Assistant devices, or read household memory.
- Anything that could let a malicious skill, prompt, or LLM response escape its sandbox.
- Anything that could leak `geniepod.toml` contents, secrets (`HA_TOKEN`, `TELEGRAM_BOT_TOKEN`, etc.), or raw audio/transcripts off the device.
- Anything that lets a remote network caller crash, hang, or trigger unbounded resource use in `genie-core`, `genie-api`, `genie-governor`, `genie-health`, or the LLM backend.
- Tampering with the actuation audit log or the redacted security posture surface.
- Authentication bypasses against the dashboard, chat API, or the `genie-ai-runtime` HTTP path.
- Supply-chain risks: a dependency we ship that is itself compromised, or a setup script that fetches an unverified binary.

Out of scope:

- Vulnerabilities in upstream dependencies that don't reach a GenieClaw code path (file those upstream).
- Issues that require physical access plus root to exploit (operator-side trust boundary).
- Best-practice recommendations that aren't tied to a concrete exploit (those are welcome as regular `enhancement`-tagged issues).

## Response Timeline

We aim to:

- **Acknowledge** your report within 3 business days.
- **Triage** (confirm reproducibility + estimated severity) within 7 business days.
- **Ship a fix** for critical / high-severity issues within 30 days of triage, sooner if exploitation is in the wild.

If a fix takes longer, we'll communicate why. We will credit you in the release notes and CHANGELOG when the fix ships, unless you prefer to stay anonymous.

## Disclosure

We follow coordinated disclosure. Once a fix is released:

- The CHANGELOG and release notes name the issue, the fix, and (with your permission) the reporter.
- A GitHub Security Advisory is published with the affected version range and the fix version.
- We do not publicly tag commits or PRs as security fixes until the advisory is live.

## Scope

This policy covers the `GeniePod/genie-claw` repository and the prebuilt artifacts published from its release tags. The companion runtime `GeniePod/genie-ai-runtime` has its own SECURITY.md — file there for issues specific to the C++ inference layer.

## Bug Bounty

GenieClaw is an alpha-stage open-source project. We do not currently offer a paid bug bounty, but well-written reports against the in-scope surfaces above get acknowledged in release notes and earn the reporter's name in the project credits.
