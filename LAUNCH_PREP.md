# Launch Prep Checklist

Internal-facing. Not a public marketing doc. The questions here are the
ones I'd want answered before posting GenieClaw anywhere external — Reddit
`r/homeassistant`, HN Show, Twitter, GitTensor, anywhere.

The goal is **honest readiness**, not maximum hype. Launching a real
private-AI-for-home project to an audience that distrusts AI marketing
means the project has to look like it works, not like it's pretending.

---

## 1. Demo (gate)

The single biggest lever in the marketing plan, and the only one I cannot
fake in code. Without it, none of the rest matters.

- [ ] 20-30 second screen + audio capture: speak → STT → LLM → Piper TTS reply → Home Assistant action.
- [ ] Show real wall-clock latency on a stop-watch overlay (TTFT, total turn).
- [ ] Run on the actual Jetson Orin Nano Super, 25 W mode. No desktop GPU fakery.
- [ ] One real device action — turning a light on, asking room temperature, etc. — not a canned response.
- [ ] Captions on the speech turn for accessibility + skim-readers.
- [ ] Upload to YouTube unlisted first; review for any leaked household details before flipping public.
- [ ] Replace the `<!-- TODO -->` placeholder in `README.md` with the embed.

If the demo isn't representative of what a stranger would experience on
their own hardware, the launch will produce more pain than stars.

## 2. Install audit

The marketing plan calls for `./install.sh && ./run.sh`. We do not have
that today. Before posting externally, either deliver it or be very
explicit it isn't there.

- [ ] Run `GETTING_STARTED.md` Option A (dev-machine path) end-to-end on a clean Ubuntu container. Time it. Note every step that took manual fiddling.
- [ ] Same for the Jetson path. Note every place a non-author would get stuck (`nvpmodel`, audio device picking, whisper-server / Piper / llama-server port collisions, HA token entry).
- [ ] If total Jetson time > 30 min, write a `scripts/jetson-bringup.sh` that compresses what it can. Don't over-promise; leave manual steps that genuinely need a human (HA token, model download).
- [ ] Add a known-bad-and-good versions table to `GETTING_STARTED.md`: L4T R36.x, JetPack 6.x, CUDA 12.x, llama.cpp commit, whisper.cpp commit. Drift here costs hours.

## 3. Repo hygiene

The README hook will pull people into the repo. Anything broken there is
visible immediately.

- [ ] All links in `README.md` resolve (the new ARCHITECTURE.md, CHANGELOG.md, GETTING_STARTED.md, genie-ai-runtime links).
- [ ] CI is green on main. `make test` passes locally and in CI.
- [ ] `make release` builds cleanly on a fresh Jetson clone.
- [ ] No secrets, tokens, or local paths in committed config files. `deploy/config/geniepod.dev.toml` reviewed.
- [ ] `LICENSE` is the intended one (AGPL-3.0 today; integrating projects need to know upfront).
- [ ] `cargo audit` clean, or known issues documented.
- [ ] Issues template + PR template present, or explicitly removed if not wanted.
- [ ] At least one `good-first-issue` open so contributors have a foothold.

## 4. Roadmap visibility

Stars come from "I might use this later" as much as from "I use this now."
The roadmap has to be on the repo, not in someone's head.

- [ ] One-page roadmap visible from README. Quarter granularity is fine; "v1.0 = 24-hour soak + packaging" is fine.
- [ ] At least one near-term milestone labeled with the model (Qwen3-4B today, whatever target next).
- [ ] Hardware roadmap referenced (GeniePod custom carrier). If it's still aspirational, say so — don't imply it exists.

## 5. Performance numbers — honest

Anyone in the local-AI scene will compare against llama.cpp directly. The
README should pre-empt that with real measurements, not vibes.

- [ ] Run llama.cpp baseline on the same Jetson, same model, same prompt. Record prefill tok/s, decode tok/s, TTFT. (This closes [genie-ai-runtime#2](https://github.com/GeniePod/genie-ai-runtime/issues/2) as a bonus.)
- [ ] Add a small benchmarks table to `README.md` (or link to a `BENCHMARKS.md`): GenieClaw end-to-end voice-turn latency, broken down (mic → STT → first LLM token → first TTS audio → speaker). Honest about variance.
- [ ] Flag what isn't tuned yet (decode tok/s vs llama.cpp's reference, currently behind — Path C is open).

## 6. Positioning anchors

These are the lines that will get pasted into Reddit/HN/Twitter copy.
Decide them now so they're consistent everywhere.

- One-liner: "A private, always-on AI for your home. Runs entirely on a Jetson Orin Nano. Voice in, voice out, controls Home Assistant, no cloud."
- 30-second pitch (for HN intro paragraph): TBD — draft + critique before posting.
- The "this is not" list (already in README under `What It Is Not`): keep it short, keep it true.

## 7. Distribution sequencing

Once everything above is green, post in this order. Each step gives feedback
that should shape the next.

1. **GitHub repo polish** — README, demo, install. Sit on it for a day, re-read with fresh eyes, fix the things that bug you.
2. **Soft launch on r/homeassistant** — Mid-week, mid-day pacific. Title: "I built a local AI assistant that actually controls Home Assistant — fully on-device on a Jetson". Read every comment in the first 2 hours, reply to all of them honestly. Expect "this already exists" — have the answer ready (it'll be in `What It Is Not`).
3. **Home Assistant community forum** — Same week. Different audience tone (more deployers, less reddit-snark). Link the reddit post; don't repost the same body.
4. **GitTensor + Twitter/X** — Same week. Use the same demo clip. Twitter thread can do the architecture deep-dive that doesn't fit in Reddit.
5. **HN Show** — Wait at least 1 week after Reddit. Use the feedback to tighten the README. Title: "Show HN: GenieClaw — local AI agent for your home, on a Jetson". Be online for the first 4 hours after post.

Do **not** queue Reddit + HN + Twitter same-day. The fastest way to lose a
viral moment is to scatter feedback across channels you can't keep up with.

## 8. What we are NOT promising

Easy to over-promise on a marketing pass. The following are deliberately
absent from the README and should stay absent until they're real.

- "Plug-and-play": setup is 30-60 min, not five.
- "Faster than X": we don't have the comparison numbers yet (item 5).
- "Multi-language": Whisper supports it; the agent doesn't tune for it.
- "Mobile app": planned, not built.
- "Custom hardware": GeniePod carrier exists as direction, not product.
- "Production-ready": this is alpha.4. Say alpha. Always.

If we keep the README honest about all of these, the demo + the architecture
do the selling. If we don't, the first Reddit comment will catch it.
