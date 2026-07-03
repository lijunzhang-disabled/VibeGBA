# Debug Folder

This folder stores three kinds of documents:

1. **Bug investigations** — one markdown per bug, recording symptom → root cause → fix → verification. Named `YYYY-MM-DD_<short-slug>.md` and lives at the top of `debug/`.
2. **Concept notes** — explanations of how parts of the emulator work, for self-reference. Lives in `debug/concepts/`, one file per topic. See [concepts/README.md](concepts/README.md) for the index.
3. **Followups** — rolling list of known correctness gaps, accuracy items, and "we'll get to it later" notes. See [followups.md](followups.md).

## Bug index

| Date | Bug | Status |
|---|---|---|
| 2026-04-22 | [Pokémon Emerald black screen: pipeline + MRS decoder](2026-04-22_emerald-black-screen.md) | **Fixed** |
| 2026-04-24 | [Pokémon Emerald noisy audio: sound engine producing garbage](2026-04-24_pokemon-emerald-noisy-audio.md) | **Open** (root cause identified) |
| 2026-04-24 | [ARM MSR: mode-bit banking was silently skipped](2026-04-24_arm-msr-banking.md) | **Fixed** |
| 2026-04-24 | [CPU accuracy sweep — 5 fixes from jsmolka test ROMs](2026-04-24_cpu-accuracy-sweep.md) | **In progress** |
| 2026-04-25 | [Pokémon audio: FIFO DMA must be re-anchored every VBlank](2026-04-25_pokemon-audio-dma-reanchor.md) | **Fixed** (behavioural) |
| 2026-04-26 | [Pokémon Emerald save: 8-bit Flash bus + chip ID](2026-04-26_pokemon-save-irq-banking.md) | **Fixed** |
| 2026-04-29 | [Pokémon Emerald save: IRQ pipeline refill](2026-04-29_pokemon-save-irq-pipeline-refill.md) | **Fixed** |
| 2026-04-30 | [SRTOG Flash chip ID](2026-04-30_srtog-flash-chip-id.md) | **Fixed** |
| 2026-05-04 | [Env-var hot-path perf](2026-05-04_env-var-hot-path-perf.md) | **Fixed** |
| 2026-05-05 | [SRTOG FIFO_B cross-trigger](2026-05-05_srtog-fifo-b-cross-trigger.md) | **Fixed** |
| 2026-05-19 | [Pokémon HBlank DMA during VBlank](2026-05-19_pokemon-hblank-dma-vblank.md) | **Fixed** |
| 2026-05-23 | [Pokémon Emerald BIOS open-bus latch](2026-05-23_pokemon-emerald-bios-open-bus.md) | **Fixed** |
| 2026-05-24 | [FE7 HBlank/audio IRQ cascade](2026-05-24_fe7-hblank-irq-cascade.md) | **Fixed** |
| 2026-06-21 | [CPU timing / GamePak prefetch + wait-states (WIP)](2026-06-21_cpu-timing-prefetch-wip.md) | **WIP** (gated) |
| 2026-07-01 | [HoD/Emerald M4A SFX smearing: VBlank re-anchor gate too tight](2026-07-01_hod-emerald-m4a-sfx-smearing.md) | **Fixed** |

## Concept notes

See [concepts/README.md](concepts/README.md) for the full index. Current topics:

- [emulator-basics](concepts/emulator-basics.md) — what the emulator does at a high level, frame loop, components.
- [scheduler](concepts/scheduler.md) — the priority-queue event dispatcher.
- [blanking-periods](concepts/blanking-periods.md) — HBlank, VBlank, and why they matter.
- [memory-map](concepts/memory-map.md) — IWRAM/EWRAM/VRAM, the full address layout, mirroring rules.
- [timers](concepts/timers.md) — counter/reload/prescaler/cascade, what overflow means, how audio sample rates are generated.
- [dma-registers](concepts/dma-registers.md) — SAD/DAD/FIFO meaning, two-register pattern, contrast with modern queue-based DMA.
- [fifo-dma-vblank](concepts/fifo-dma-vblank.md) — DirectSound FIFO DMA and the VBlank reset hook.
- [swi-halt-intrwait](concepts/swi-halt-intrwait.md) — SWI dispatch, halt mode, and the IntrWait re-halt gate (including the FE7 bug).

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
