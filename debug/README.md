# Debug Folder

This folder stores two kinds of documents:

1. **Bug investigations** — one markdown per bug, recording symptom → root cause → fix → verification. Named `YYYY-MM-DD_<short-slug>.md`.
2. **Concept notes** — explanations of how parts of the emulator work, for self-reference. Named by topic (e.g., `concepts.md`).

## Bug index

| Date | Bug | Status |
|---|---|---|
| 2026-04-22 | [Pokémon Emerald black screen: pipeline + MRS decoder](2026-04-22_emerald-black-screen.md) | **Fixed** |
| 2026-04-24 | [Pokémon Emerald noisy audio: sound engine producing garbage](2026-04-24_pokemon-emerald-noisy-audio.md) | **Open** (root cause identified) |
| 2026-04-24 | [ARM MSR: mode-bit banking was silently skipped](2026-04-24_arm-msr-banking.md) | **Fixed** |
| 2026-04-24 | [CPU accuracy sweep — 5 fixes from jsmolka test ROMs](2026-04-24_cpu-accuracy-sweep.md) | **In progress** |

## Concept notes

| Topic | Doc |
|---|---|
| Emulator basics + event scheduler deep-dive | [concepts.md](concepts.md) |

## Template

New bug reports should roughly follow the structure of existing entries:

- **Symptom** — observable behavior
- **How it was found** — diagnostic tool / test / manual testing
- **Investigation** — step-by-step trace of the reasoning
- **Root cause(s)** — what was actually wrong and why
- **Fix** — the code change, with file/line references
- **Regression tests** — unit tests added to prevent recurrence
- **Verification** — proof that the fix works end-to-end
- **Related issues** — any lingering bugs discovered but not fixed

Each entry should be self-contained enough that a reader can understand what went wrong without needing other context.
