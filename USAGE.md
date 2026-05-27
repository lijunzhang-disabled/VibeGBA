# GBA Emulator — Usage Guide

## Building

### Prerequisites

- **Rust toolchain** (1.70+): install via [rustup](https://rustup.rs/)
- **CMake**: required by the bundled SDL2 build (`brew install cmake` on macOS)

### Build

```bash
# Debug build (faster compile, slower execution)
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo build

# Release build (recommended for playing games)
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo build --release
```

The binary is at `target/release/gba-frontend` (or `target/debug/gba-frontend`).

### Run Tests

```bash
cargo test -p gba-core
```

## Running a Game

```bash
# Basic usage — BIOS HLE, no BIOS dump needed
./target/release/gba-frontend path/to/game.gba

# With a real GBA BIOS (for the boot animation)
./target/release/gba-frontend path/to/game.gba --bios gba_bios.bin

# Custom window scale (default is 3x = 720×480)
./target/release/gba-frontend game.gba --scale 4

# Disable audio (if you have audio issues)
./target/release/gba-frontend game.gba --no-audio
```

### CLI Options

| Option | Description |
|---|---|
| `<ROM>` | Path to the `.gba` ROM file (required) |
| `-b, --bios <path>` | Path to GBA BIOS dump (optional, HLE used if omitted) |
| `--skip-bios` | Skip BIOS boot animation (default: true) |
| `-s, --scale <N>` | Window scale factor (default: 3, so 720×480) |
| `--no-audio` | Disable audio output |

## Controls

### Keyboard Mapping

| Keyboard Key | GBA Button | Typical Use |
|---|---|---|
| **Z** | A | Confirm, primary action |
| **X** | B | Cancel, secondary action |
| **Enter** | Start | Pause, menu |
| **Backspace** | Select | Secondary menu |
| **Arrow Up** | D-pad Up | Move up |
| **Arrow Down** | D-pad Down | Move down |
| **Arrow Left** | D-pad Left | Move left |
| **Arrow Right** | D-pad Right | Move right |
| **A** | L (Left shoulder) | Context-dependent |
| **S** | R (Right shoulder) | Context-dependent |

### Emulator Hotkeys

| Key | Action |
|---|---|
| **`]`** (right bracket) | Save state (to `<rom_name>.state`) |
| **`[`** (left bracket) | Load state (from `<rom_name>.state`) |
| **D** | Dump EWRAM to `/tmp/ewram-NNN.bin` for diagnostics |
| **I** | Dump IWRAM to `/tmp/iwram-NNN.bin` for diagnostics |
| **R** | Dump instruction trace ring when `INSTR_TRACE_RING=1` |
| **M/S/V** | Pokémon Emerald-specific map/metatile/var probes |
| **Escape** | Quit the emulator |

> **Save state vs. game save:**
> - **Save state (`]`/`[`)** captures the complete emulator state (CPU, memory, video, audio — everything) at that instant. Works anywhere, even mid-battle. File: `<rom>.state`.
> - **Game save (`.sav` file)** is the game's own save data — the slot you write to by using the in-game save menu. Auto-loaded on startup, auto-written on exit. File: `<rom>.sav`.

## Saves

### Game Saves (.sav)

The emulator automatically handles game saves:

- **On startup**: if `<rom_name>.sav` exists next to the ROM, it's loaded into the backup media
- **On exit**: the current save data is written to `<rom_name>.sav`

For example, running `./gba-frontend pokemon_emerald.gba` will:
- Look for `pokemon_emerald.sav` on startup
- Save to `pokemon_emerald.sav` when you quit

The save type is auto-detected from strings embedded in the ROM:

| ROM Contains | Save Type | Size |
|---|---|---|
| `SRAM_V` | SRAM | 32 KB |
| `FLASH_V` or `FLASH512_V` | Flash | 64 KB |
| `FLASH1M_V` | Flash | 128 KB |
| `EEPROM_V` | EEPROM | 512 B or 8 KB |

#### Automatic backups

Whenever the emulator writes a `.sav` and the data has actually changed,
it rotates a 5-deep history of backups next to the file:

```
pokemon_emerald.sav         ← current (just-written)
pokemon_emerald.sav.bak-1   ← previous
pokemon_emerald.sav.bak-2   ← one before that
…
pokemon_emerald.sav.bak-5   ← oldest kept
```

If you ever lose progress (corrupted save, accidental overwrite, etc.),
restore by copying a backup over the live file:

```bash
cp pokemon_emerald.sav.bak-1 pokemon_emerald.sav
```

Backups only rotate when the save data actually changes — purely opening
and closing the emulator without any in-game save is a no-op and won't
push older backups out of the window.

### Save States (.state)

Save states capture the **entire emulator state** — CPU, memory, video, audio, everything — at a single instant. Unlike game saves, they work regardless of whether the game has a save feature.

- **`]`**: save the current state to `<rom_name>.state`
- **`[`**: load the state from `<rom_name>.state`

Save state files are compressed with zstd (typically 50-100 KB).

## Display

- Native GBA resolution: **240×160 pixels**
- Default window: **720×480** (3x scale)
- Color: 15-bit RGB (32,768 colors), converted to 24-bit for display
- Frame rate: **~59.737 Hz** (matching real GBA hardware)

## Audio

- **6 sound channels**: 4 PSG (square, wave, noise) + 2 DMA FIFO (direct sound)
- Output: stereo, 32768 Hz sample rate
- Use `--no-audio` if you experience audio crackling or issues

## BIOS

The emulator includes **BIOS High-Level Emulation (HLE)** — it implements the most common GBA BIOS functions in software, so you don't need a BIOS dump to play games. Supported SWI functions include:

- Division, square root, arctangent
- Memory copy/fill (CpuSet, CpuFastSet)
- Decompression (LZ77, Huffman, Run-Length, BitUnPack)
- Affine matrix calculation (BgAffineSet, ObjAffineSet)
- System control (SoftReset, Halt, VBlankIntrWait)

If you have a real GBA BIOS dump (`gba_bios.bin`, 16 KB), you can use it with `--bios` to get the Nintendo boot animation. The emulator works fine without it.

## Troubleshooting

### Game doesn't boot / black screen
- Make sure the ROM file is a valid `.gba` file (not `.zip` or `.7z`)
- Try with `--no-audio` in case audio initialization is causing issues
- Some games may have CPU instruction edge cases not yet handled

### No sound
- Audio is enabled by default. Check your system volume.
- Some games use only FIFO audio (DMA-driven), which requires correct timer + DMA interaction

### Save not persisting
- Save data is written on clean exit (Escape key or window close)
- If the emulator crashes, the save may be lost — use save states (`]`) as backup
- Check that the emulator has write permission in the ROM's directory

### Performance
- Use release mode: `cargo build --release`
- Debug builds are significantly slower due to lack of optimization
- The emulator targets real-time speed (~60 FPS). If it runs too fast, frame timing handles it.

## Project Structure

```
gba/
├── gba-core/          # Emulation library (no platform dependencies)
│   └── src/
│       ├── lib.rs     # Top-level Gba struct, main loop
│       ├── arm7tdmi/  # CPU: ARM + THUMB instruction sets
│       ├── bus/       # Memory bus, I/O registers
│       ├── ppu/       # Graphics: BG layers, sprites, effects
│       ├── apu/       # Audio: PSG channels, FIFO, mixer
│       ├── dma.rs     # DMA transfer controller
│       ├── timer.rs   # 4 hardware timers
│       ├── bios.rs    # BIOS HLE (22 SWI functions)
│       └── backup/    # Save media (SRAM, Flash, EEPROM)
│
├── gba-frontend/      # SDL2 frontend
│   └── src/
│       ├── main.rs    # Entry point, save/load, main loop
│       ├── video.rs   # SDL2 window rendering
│       ├── audio.rs   # SDL2 audio output
│       └── input.rs   # Keyboard → GBA button mapping
│
├── PLAN.md            # Implementation plan with per-phase details
├── ARCHITECTURE.md    # Technical architecture deep-dive
├── USAGE.md           # This file
└── test-roms/         # Place test ROMs here (.gitignored)
```

## Accuracy Notes

This emulator is **scanline-oriented**: it renders one line at a time and processes HBlank/VBlank events through the scheduler. This is sufficient for many games but may not handle:

- Mid-scanline register changes (games that modify PPU state during visible pixels)
- Fully cycle-accurate Game Pak prefetch and DMA/CPU contention
- Obscure ARM instruction edge cases
- Some Flash/EEPROM save protocols used by specific games

If a game doesn't work, it's likely a timing, bus, or instruction edge case. The project currently handles these with focused diagnostics under `debug/` rather than a full interactive debugger.
