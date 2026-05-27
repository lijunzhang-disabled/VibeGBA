# VibeGBA — GBA Emulator in Rust

A Game Boy Advance emulator written from scratch in Rust, with an SDL2 frontend. Built for learning — every hardware component is implemented as a Rust data structure that ticks forward in lockstep with the CPU.

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
- Phase 9 (accuracy polish) — ongoing; recent: Pokémon Emerald boots to title screen with music, in-game save round-trips correctly, jsmolka arm/thumb/memory test ROMs all pass; current active bug is FE7 HBlank/audio gate behavior

90 checked-in unit tests, ~10,000 lines of Rust across `gba-core` and `gba-frontend`.

## Reference

- [GBATEK](https://problemkaputt.de/gbatek.htm) — the definitive GBA hardware spec
- [jsmolka test ROMs](https://github.com/jsmolka/gba-tests) — CPU instruction validation
- [tonc](https://www.coranac.com/tonc/text/toc.htm) — GBA programming tutorial with demo ROMs
