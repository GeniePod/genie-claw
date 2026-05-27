# Contributing to GenieClaw

GenieClaw is a local home AI runtime that ships on Jetson hardware. Most pull requests touch code that has to actually work on a real Jetson Orin Nano with a microphone, a Home Assistant install, and a chat UI in front of an end user. That constraint shapes how this project reviews contributions.

## Welcome

**Quality, engineering, and bug fixes are always welcome.** That includes:

- Bug fixes against any open issue (or a clear new one you've filed alongside the PR).
- Test coverage improvements, even for existing behavior nothing else is changing.
- Documentation fixes — typos, broken links, out-of-date setup instructions.
- Performance work backed by real measurements on Jetson hardware.
- Refactors that reduce surface area, kill latent bugs, or simplify the public API.
- New features that fit the [product direction in README.md](README.md) — please open an issue first to confirm scope.

What we are slower to land:

- Wholesale rewrites of subsystems without a clearly-filed motivating issue.
- New runtime dependencies (especially anything that needs CUDA, ALSA, or root). Discuss in an issue first.
- Cosmetic-only changes to working code (style sweeps, mass renames). These churn `git blame` and slow review for everyone else.

## Required: Real Behavior Proof

Every PR body must include a **"Real Behavior Proof"** section that demonstrates you have actually run the code you are changing. This is **enforced by CI** — a PR with no proof section will fail the `Contribution checklist` job and cannot be merged.

The bar is **truth, not theatre**: tell us what you ran, where you ran it, what hardware or runtime profile it represents, and what happened. Real validation > performative completeness.

### Minimum content

Copy the section from the PR template and fill it in honestly:

```markdown
## Real Behavior Proof

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

<commands you executed, with environment/profile details and output if useful>

### What I observed

<the actual result — log lines, screenshots, latency numbers, error messages>
```

### What counts as "verified end-to-end on Jetson"

In rough order of preference:

1. **You ran it on a real Jetson** (Orin Nano, Orin AGX, or Xavier — any L4T target the repo supports). Include the device, the model loaded, and the output of the affected subsystem (chat reply, voice loop banner, `journalctl -u genie-core`, `genie-ctl status`, dashboard screenshot, etc.).
2. **You ran the affected path on another declared profile** (`raspberry_pi`, `portable_sbc`, `laptop`, or `mac`). Include the board or machine, OS, config profile, and why that is an equivalent path for the change.
3. **The `aarch64-unknown-linux-gnu release build` CI job built your branch cleanly**, AND the code path you changed is exercised by an existing unit / integration test you can show passing under `cargo test`. Document the test names you ran.
4. **You cannot run on Jetson** (no hardware, no cross-compile toolchain on your dev machine, etc.) — tick the "NOT verified on Jetson hardware" checkbox and say so explicitly under "What I ran". Explain what verification you did do (`cargo fmt --check`, `cargo clippy -- -D warnings`, targeted unit tests, manual code reading). The reviewer will then decide whether a maintainer needs to run your branch on Jetson before merge.

### What does NOT count

- "Looks correct to me." — judgment without execution.
- "Tests pass" without naming which tests or what they cover for the change.
- A green CI run as the only proof — CI runs `cargo fmt / clippy / test / aarch64 cross-compile`, none of which exercise voice, audio, or Home Assistant.
- Auto-generated PR bodies from `git log --format=%s | head` or AI-coding-tool default templates with nothing filled in.

If you suspect your change is too small to need proof (e.g., a one-line typo fix in a comment), say that explicitly and the reviewer will agree or push back.

## Filing issues before PRs

For anything non-trivial, file an issue first. Most successful PRs in this repo close a specific issue and reference it in the body (`Fixes #NN` or `Closes #NN`). That keeps the discussion attached to the issue and lets reviewers see motivation before reading code.

Bug reports should include:

- The Jetson model + L4T / JetPack version
- The git SHA you're running (`/opt/geniepod/bin/genie-core --version` or the SHA you deployed via `make deploy`)
- The exact reproduction steps
- Relevant `journalctl -u <unit>` excerpts or dashboard error output
- What you expected to happen vs. what happened

## Code style and CI gates

CI runs against every push and PR:

- `cargo fmt --all -- --check` (workflow: `CI / cargo fmt`)
- `cargo clippy --workspace --all-targets --locked -- -D warnings` (workflow: `CI / cargo clippy`)
- `cargo test --workspace --locked --all-targets` + doc tests (workflow: `CI / cargo test`)
- `cargo clippy + test -p genie-core -p genie-ctl --no-default-features` (workflow: `CI / cargo clippy + test (--no-default-features)`)
- `aarch64-unknown-linux-gnu` cross-compile of all five release binaries (workflow: `Cross-compile (aarch64 / Jetson)`)
- `cargo audit` + `cargo deny` on `Cargo.{toml,lock}` / `deny.toml` / workflow changes, plus a weekly cron (workflow: `Audit`)
- `shellcheck` + `ruff` on tracked `*.sh` and `*.py` (workflow: `Scripts`)
- **Contribution checklist** — verifies the PR body has the Real Behavior Proof section (workflow: `Contribution / PR body checklist`)

All checks must be green before merge. We squash-merge PRs, with the maintainer writing the squash body — your commit history is a working journal, not the public record.

## Commit hygiene

- Write commit subjects in the imperative mood, scoped if useful: `fix(voice/stt): per-call nonce on transcribe_pcm tempfile`.
- Keep commit bodies useful — explain *why*, not *what*. The diff already shows what changed.
- Do **not** add `Co-Authored-By: Claude` / Copilot / other AI-assistant trailers to commit messages. Tools to draft code are fine; we keep `git log` attribution to the human contributor so credit is unambiguous.
- Do **not** include AI-generated attribution footers like `🤖 Generated with Claude Code` (or any variant) in **PR bodies** either. Enforced by the `Contribution / PR body checklist` CI job. Same reason as the commit-trailer rule: attribution stays with the human contributor.
- Don't `--no-verify` past pre-commit hooks. Fix the underlying issue.

## Security disclosures

**Do not file security issues on the public tracker.** Email the security team privately at **<contact@genieclaw.org>** with:

- A description of the issue and where in the codebase it lives
- Steps to reproduce
- The potential impact (data exposure, RCE, DoS, privilege escalation, etc.)
- Any proof-of-concept you've developed

See [SECURITY.md](SECURITY.md) for the full vulnerability disclosure policy and response timeline.

## License

By contributing, you agree that your contribution is licensed under the [GNU Affero General Public License v3.0](LICENSE), the same license as the rest of the project. The standard "if you don't own the rights, don't paste it in" rule applies — don't commit code you copied from a non-AGPL-compatible source.

## Questions?

Open a [Discussion](https://github.com/GeniePod/genie-claw/discussions) or comment on the issue you're picking up.
