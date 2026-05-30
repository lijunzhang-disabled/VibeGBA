# VibeGBA — An AI-Coded GBA Emulator in Rust

A Game Boy Advance emulator built from scratch in Rust, largely written through AI-human pair programming. The human steered the direction and made design calls; the AI handled most of the implementation — CPU, memory bus, PPU, audio, DMA, timers, save media, and so on.

```
┌──────────────────────────────┐
│  ARM7TDMI CPU (ARM + THUMB)  │ ← 16.78 MHz, 3-stage pipeline
│        +                      │
│  Memory bus                   │ ← BIOS, EWRAM, IWRAM, VRAM, OAM,
│        +                      │    palette, ROM, SRAM/Flash/EEPROM
│  PPU (6 video modes)          │ ← 240×160, tile + bitmap, sprites,
│        +                      │    windows, alpha blending
│  APU (6 channels)             │ ← 4 PSG + 2 DMA FIFO, stereo 32 kHz
│        +                      │
│  DMA, timers, interrupts      │
└──────────────────────────────┘
```

## Why this exists

This started as an experiment to see how well AI coding holds up on a complex, low-level systems task. Emulator development involves hardware datasheets, fiddly bit manipulation, timing-sensitive logic, and lots of debugging with limited feedback — a good stress test. It turned out to work reasonably well, though plenty of human guidance was needed along the way.

The `debug/` directory documents the investigation and fix process for real-world compatibility bugs — each entry walks through symptoms, hypotheses, and the eventual patch.

## Run a game

```bash
# Build (needs CMake for bundled SDL2)
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo build --release

# Play
./target/release/gba-frontend path/to/game.gba
```

Controls, save-state hotkeys, BIOS info, and troubleshooting are in [`USAGE.md`](USAGE.md).

## Project docs

| Doc | What it's for |
|---|---|
| [`USAGE.md`](USAGE.md) | How to build, run, and control the emulator |
| [`PLAN.md`](PLAN.md) | Phase-by-phase implementation plan with status |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Technical deep-dive on CPU, PPU, memory bus, scheduler, audio, save states |
| [`debug/`](debug/) | Bug investigations, [concept notes](debug/concepts/), and a rolling [followups list](debug/followups.md) |

## Status

- Phases 1-7 (CPU, memory, PPU, DMA, timers, audio, saves) — **Done**
- Phase 8 (debugger) — optional, small utilities added on demand
- Phase 9 (accuracy polish) — ongoing

90 checked-in unit tests, ~10,000 lines of Rust across `gba-core` and `gba-frontend`.

### Test ROM compatibility

[jsmolka's GBA test suite](https://github.com/jsmolka/gba-tests) — all four ROMs pass end-to-end:

| ROM            | Status |
|----------------|--------|
| `arm.gba`      | ✅ ALL PASS |
| `thumb.gba`    | ✅ ALL PASS |
| `memory.gba`   | ✅ ALL PASS |
| `bios.gba`     | ✅ ALL PASS |

[Emu-Docs test ROMs](https://github.com/emudev-org/discord-resources) — CPU, audio, and graphics tests:

| Test Suite | ROMs | Status |
|------------|------|--------|
| CPUTest (Deadbody) | `CPUTest.gba` | ✅ ALL PASS |
| ARM Wrestler (Mic/NORMATT) | `armwrestler.gba` | ✅ ALL PASS |
| Belogic Sound Tests | `soundtest1-4.gba`, `sound3demorom.gba`, `directsoundrom.gba` (5 ROMs) | ✅ All boot, render UI, audio plays correctly |
| Tonc GBA Demos (Jasper Vijn) | 34 demo ROMs covering BG modes, sprites, affine, blending, windows, DMA, timers, IRQ, SWI, text rendering | ✅ All 34 boot and render correctly |

Drop test ROMs into `test-roms/` and run `cargo run --release --example check_test -- test-roms/<rom>.gba` to verify on your machine. The runner detects "All tests passed" by checking the mode-4 framebuffer where the test framework renders its result.

### Playable games (spot-tested)

| Game                                  | Status |
|---------------------------------------|--------|
| Pokémon Emerald (US)                  | Title screen, in-game music, save/load round-trip, PokéNav map all work |
| Fire Emblem 7 (US)                    | Boots to title, save (.sav) works, resume option appears after saving |
| Super Robot Taisen: Original Generation | Playable |
| Golden Sun                            | Playable |

Bug investigations and the rolling list of remaining issues are in [`debug/`](debug/).

## How it was built

Development followed a phased plan (documented in [`PLAN.md`](PLAN.md)). AI generated the bulk of the code, with human review and course-correction throughout:

1. **CPU + Memory Bus** — ARM7TDMI with full ARM and THUMB instruction sets, register banking, pipeline simulation
2. **I/O + PPU Bitmap + BIOS HLE** — Memory-mapped I/O, bitmap video modes, 22 software-emulated BIOS functions
3. **Tile PPU + Sprites** — Text/affine backgrounds, 128 hardware sprites, OAM parsing
4. **DMA + Timers + Input** — 4-channel DMA with FIFO/HBlank/VBlank triggers, cascading timers
5. **Windows + Blending + Effects** — Window regions, alpha blending, brightness fade
6. **Audio** — 4 PSG channels + 2 DMA FIFO channels, stereo mixing at 32 kHz
7. **Saves + Save States** — SRAM, Flash (64K/128K), EEPROM auto-detection; zstd-compressed save states
8. **Debugger** — On-demand diagnostic utilities
9. **Accuracy polish** — Ongoing compatibility fixes, each documented with root-cause analysis in `debug/`

The `debug/` directory contains 13+ investigation reports covering real hardware edge cases — pipeline refill ordering, Flash bus read semantics, BIOS open-bus latch behavior, and more.

## Reference

- [GBATEK](https://problemkaputt.de/gbatek.htm) — the definitive GBA hardware spec
- [jsmolka test ROMs](https://github.com/jsmolka/gba-tests) — CPU instruction validation
- [tonc](https://www.coranac.com/tonc/text/toc.htm) — GBA programming tutorial with demo ROMs
- [emudev.org Discord Resources](https://github.com/emudev-org/discord-resources) — community-curated emulator development resources and learning materials
