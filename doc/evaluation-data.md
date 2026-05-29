# Evaluation Data And License Policy

GenieClaw can use public data to improve BFCL tool-call accuracy and home
context retrieval, but every imported or converted dataset must keep a clear
license trail. If the license is unknown, noncommercial-only, or incompatible
with product use, keep it out of committed fixtures and product CI.

This is the engineering rule for this repo:

- verify the license before import
- keep source URL, license, citation, version/date, and conversion notes
- commit only small fixtures or adapters, not raw public datasets
- keep large generated data under ignored local paths such as `tests/bfcl/local/`
- do not commit private household facts, real secrets, credentials, or raw user
  logs
- treat noncommercial data as research-only unless a separate license is
  obtained

When in doubt, do not import the data.

## Dataset Fit

| Dataset | License posture | GenieClaw use |
|---------|-----------------|---------------|
| [Home Assistant Intents](https://github.com/OHF-Voice/intents) | Source repo is CC BY 4.0. Preserve attribution. | Best first source for BFCL `home_control`, `home_status`, timer, media, weather, and slot cases. Convert templates into Genie typed-tool expectations. |
| [CASAS Smart Home Data Sets](https://casas.wsu.edu/datasets/) | Current Zenodo CASAS records are CC BY 4.0; verify each record before use. | Best public real-home source for sensor timelines, presence, activity, action-history, and device-state context replay. |
| [REFIT Electrical Load Measurements](https://pureportal.strath.ac.uk/en/datasets/refit-electrical-load-measurements-cleaned/) | CC BY 4.0. | Use for deterministic energy/device-state tests such as "what is using the most power" and appliance activity checks. |
| [UK-DALE](https://jack-kelly.com/data/) | CC BY 4.0. | Use for longer appliance electricity traces, power summaries, and anomaly-style home status tests. |
| [SLURP](https://github.com/pswietojanski/slurp) | Text annotations are CC BY 4.0. Audio is CC BY-NC 4.0 unless a separate license is obtained. | Use text only for assistant-style utterance variants and STT-like wording. Keep audio out of product CI. |
| [MASSIVE](https://github.com/alexa/massive) | Dataset is CC BY 4.0; repo code is Apache 2.0. | Use text for multilingual utterance and slot-routing tests after mapping intents to Genie typed tools. |
| [Google Speech Commands](https://www.tensorflow.org/datasets/catalog/speech_commands) | CC BY 4.0 for dataset content. | Primarily belongs in `genie-voice-runtime` for wake/keyword audio tests. Only use tiny derived text smoke cases in GenieClaw if needed. |

No public dataset above is a complete GenieClaw benchmark. The correct target is
a layered eval suite: BFCL cases for typed tools, replayed home state for
context, and separate voice-runtime audio tests in `genie-voice-runtime`.

## BFCL Source Metadata

BFCL cases may include optional `source` metadata. Any committed case derived
from a public dataset should include it:

```json
{
  "id": "ha-turn-on-kitchen",
  "category": "home_control",
  "source": {
    "dataset": "Home Assistant Intents",
    "url": "https://github.com/OHF-Voice/intents",
    "license": "CC BY 4.0",
    "citation": "OHF-Voice/intents",
    "derived_from": "sentences/en/light_HassTurnOn.yaml",
    "notes": "Converted template sentence with local fixture slots."
  },
  "prompt": "turn on the kitchen light",
  "expected_tool_calls": [
    {
      "name": "home_control",
      "arguments": {
        "action": "turn_on",
        "entity": "kitchen light"
      }
    }
  ]
}
```

Synthetic hand-written fixtures do not need source metadata, but they should
not pretend to be public benchmark data.

## Conversion Rules

1. Convert Home Assistant Intents into typed Genie tool calls, not natural
   language answer checks. Use `genie-ctl bfcl-import-ha-intents --source
   tests/bfcl/local/ha-intents --out tests/bfcl/local/ha_home_cases.jsonl` so
   generated cases include source/license metadata. Use `--language all` when
   testing multilingual routing robustness.
2. Convert CASAS timelines into small home-context snapshots before a prompt:
   room activity, presence, door/motion events, and current sensor state.
3. Convert REFIT and UK-DALE into aggregate device-state facts and expected
   `home_status`, `system_info`, or memory/tool calls.
4. Convert MASSIVE and SLURP text into robustness variants only after mapping
   intent and slots to Genie tools.
5. Keep generated stress suites local unless they are small, attributed,
   reviewed, and product-license compatible.

The score target remains Jetson Orin 8GB product behavior: fast, typed,
deterministic tool calls inside the 4096-token home-agent harness.
