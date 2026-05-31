# GBA Emulator — Implementation Plan

## Overview

Building a GBA (Game Boy Advance) emulator from scratch in Rust. Scanline-accurate rendering, SDL2 frontend, with debugger, save states, and full audio support.

## Current Status

| Phase | Status | Tests |
|---|---|---|
| Phase 1: CPU + Memory Bus | **Done** | 23 tests |
| Phase 2: I/O + PPU Bitmap + BIOS HLE | **Done** | 6 tests |
| Phase 3: Tile PPU + Sprites | **Done** | 13 tests |
| Phase 4: DMA + Timers + Input | **Done** | 9 tests |
| Phase 5: Windows + Blending + Effects | **Done** | 13 tests |
| Phase 6: Audio | **Done** | 10 tests |
| Phase 7: Saves + Save States | **Done** | 6 tests |
| Phase 8: Debugger | **Optional** — add small utilities on demand | — |
| Phase 9: Accuracy Polish | In progress (compatibility + timing polish) | Ongoing |

**Total: 115 checked-in unit tests, ~11,500 lines of Rust (core + frontend)**

### Phase 9 progress so far (see `debug/` for details)

| Bug | Fix | Doc |
|---|---|---|
| Pipeline advance ordering (regs[15] off by +4 during exec) | Move advance to after execute | [2026-04-22](debug/2026-04-22_emerald-black-screen.md) |
| MRS decoder mask 0xF9 → 0xFB (was misdecoding MSR) | Tightened mask | (same) |
| No IRQ handler stub in HLE BIOS → CPU fell off BIOS end | Install 6-instruction ARM stub at 0x18 | (same) |
| SDL2 RGB24 broken on macOS → white screen | Switched to ARGB8888 + software renderer | — |
| Flash save IRQ resume bug | Refill pipeline before IRQ delivery after PC-writing instructions | [2026-04-29](debug/2026-04-29_pokemon-save-irq-pipeline-refill.md) |
| Flash 8-bit bus read semantics | Broadcast 8-bit Flash/SRAM reads across 16/32-bit reads | [2026-04-26](debug/2026-04-26_pokemon-save-irq-banking.md) |
| Pokémon Emerald BIOS/open-bus edge case | Track BIOS latch for HLE IRQ stub/open-bus reads | [2026-05-23](debug/2026-05-23_pokemon-emerald-bios-open-bus.md) |
| HBlank DMA during VBlank | Gate HBlank DMA to visible lines only | [2026-05-19](debug/2026-05-19_pokemon-hblank-dma-vblank.md) |
| SRTOG FIFO cross-trigger | Refill only the FIFO whose DMA destination matches the low FIFO | [2026-05-05](debug/2026-05-05_srtog-fifo-b-cross-trigger.md) |
| FE7 HBlank/audio cascade | Fixed — IntrWait HLE re-halt gate (commit `bb4b916`) | [2026-05-24](debug/2026-05-24_fe7-hblank-irq-cascade.md) |

## GBA Hardware Summary

| Component | Details |
|---|---|
| CPU | ARM7TDMI, 32-bit RISC, 16.78 MHz, ARM (32-bit) + THUMB (16-bit) modes |
| Display | 240x160 pixels, ~59.737 Hz, 15-bit RGB color |
| Sound | 4 PSG channels (CGB-compatible) + 2 DMA FIFO channels |
| DMA | 4 channels with priority, HBlank/VBlank/FIFO triggers |
| Timers | 4 x 16-bit with prescaler and cascade |
| Input | 10 buttons: A, B, Select, Start, D-pad (4), L, R |
| Memory | 16KB BIOS + 256KB EWRAM + 32KB IWRAM + 96KB VRAM + 1KB palette + 1KB OAM |
| Cartridge | Up to 32MB ROM + SRAM/Flash/EEPROM save media |

### Timing Constants

- Cycles/dot: 4
- Dots/line: 308 (240 visible + 68 HBlank)
- Cycles/line: 1,232
- Lines/frame: 228 (160 visible + 68 VBlank)
- Cycles/frame: 280,896

### Memory Map

| Address Range | Size | Region |
|---|---|---|
| 0x00000000-0x00003FFF | 16 KB | BIOS ROM |
| 0x02000000-0x0203FFFF | 256 KB | EWRAM (external work RAM) |
| 0x03000000-0x03007FFF | 32 KB | IWRAM (internal work RAM) |
| 0x04000000-0x040003FE | ~1 KB | I/O Registers |
| 0x05000000-0x050003FF | 1 KB | Palette RAM |
| 0x06000000-0x06017FFF | 96 KB | VRAM |
| 0x07000000-0x070003FF | 1 KB | OAM (sprite attributes) |
| 0x08000000-0x0DFFFFFF | up to 32 MB | Game Pak ROM (3 wait state mirrors) |
| 0x0E000000-0x0E00FFFF | 64 KB | Game Pak SRAM |

## Implementation Phases

### Phase 1: Project Skeleton + ARM7TDMI CPU [DONE]

**Goal**: CPU executes ARM and THUMB instructions against a flat memory bus.

**What was built:**
- Cargo workspace: `gba-core` (library, no platform deps) + `gba-frontend` (SDL2 binary)
- **Memory Bus** (`bus/mod.rs`): address decoding by `addr >> 24`, all memory regions (BIOS, EWRAM, IWRAM, palette, VRAM, OAM, ROM, SRAM), read/write 8/16/32-bit, VRAM mirroring, open bus behavior
- **CPU** (`arm7tdmi/mod.rs`): 16 registers (R0-R15), CPSR/SPSR, banked registers for all 6 modes (User/System, FIQ, IRQ, Supervisor, Abort, Undefined), 3-stage pipeline (fetch/decode/execute), condition code evaluation (14 conditions), mode switching with register banking
- **ALU** (`arm7tdmi/alu.rs`): barrel shifter (LSL, LSR, ASR, ROR, RRX) with carry-out, all edge cases for shift-by-0/32/>32, 13 ALU operations, add/subtract with carry and overflow detection
- **ARM executor** (`arm7tdmi/arm.rs`): data processing (13 ops with immediate and register operands), multiply/multiply-long (signed and unsigned), single data transfer (LDR/STR with immediate/register offset, pre/post-indexing, write-back), halfword/signed transfer (LDRH/LDRSH/LDRSB/STRH), block transfer (LDM/STM with all addressing modes), branch/branch-with-link, branch-and-exchange (BX), SWP/SWPB, MRS/MSR, SWI
- **THUMB executor** (`arm7tdmi/thumb.rs`): all 19 instruction formats — shifted register, add/sub, mov/cmp/add/sub immediate, ALU operations (16 ops), hi-register/BX, PC-relative load, register/immediate offset loads and stores, halfword load/store, SP-relative load/store, load address, add to SP, push/pop, multiple load/store (STMIA/LDMIA), conditional branch, SWI, unconditional branch, long branch with link (BL two-instruction sequence)
- **I/O registers** (`bus/io_regs.rs`): full read/write dispatch for LCD, BG control, scroll offsets, affine parameters, window, blending, sound, DMA, timer, keypad, interrupt, and system control registers
- **Peripherals (stubs)**: interrupt controller, keypad, timers, DMA controller, backup media detection (SRAM/Flash/EEPROM via ROM signature scanning)
- **Event scheduler** (`scheduler.rs`): min-heap for cycle-accurate event scheduling (HBlank, VBlank, timer, DMA, audio events)
- **SDL2 frontend**: window with texture streaming (240x160 scaled), keyboard input mapping, frame-timed main loop

**Key Rust patterns:**
- Sibling fields (`cpu` and `bus` as separate fields of `Gba`) enable `self.cpu.step(&mut self.bus)` without borrow conflicts
- Enums for CPU modes and ALU operations with match-based dispatch
- `#[derive(Serialize, Deserialize)]` on all state for future save states

### Phase 2: I/O + Minimal PPU + BIOS HLE [DONE]

**Goal**: Boot a ROM without a BIOS dump, render bitmap frames to SDL2.

**What was built:**
- **BIOS HLE** (`bios.rs`, ~450 lines): implements 23 SWI functions in Rust so no BIOS dump is needed:
  - **Math**: Div, DivArm, Sqrt, ArcTan, ArcTan2
  - **Memory**: CpuSet, CpuFastSet (copy/fill in 16-bit and 32-bit modes)
  - **Decompression**: LZ77UnCompWram/Vram, HuffmanUnComp, RLUnCompWram/Vram, BitUnPack
  - **System**: SoftReset, RegisterRamReset, Halt, Stop, IntrWait, VBlankIntrWait
  - **Affine**: BgAffineSet, ObjAffineSet (rotation matrix calculation)
  - **Info**: GetBiosChecksum
- **SWI dispatch**: CPU sets `pending_swi` flag on SWI instruction; main loop routes to HLE (no BIOS) or real BIOS vector (BIOS loaded). This avoids the borrow-checker issue of needing both CPU and Bus during SWI handling
- **HALT support**: writing to HALTCNT (0x04000301) sets `halt_requested` flag; CPU fast-forwards to next scheduler event
- **Bitmap PPU modes**: Mode 3 (240x160 direct-color), Mode 4 (8bpp indexed, double-buffered), Mode 5 (160x128 direct-color, double-buffered)
- **Scanline timing**: event-driven HBlank/VBlank with proper DISPSTAT flag management and VCount matching

**Key insight — SWI HLE approach:**
Rather than intercepting at the BIOS vector address, the SWI instruction sets a `pending_swi: Option<u8>` on the CPU. The `Gba::run_frame()` loop checks this after each instruction and dispatches to either the HLE handler or the real BIOS exception vector. This cleanly separates the CPU (which doesn't know about BIOS HLE) from the system-level dispatch.

### Phase 3: Tile PPU + Sprites [DONE]

**Goal**: Render tile-based backgrounds and sprites for real games.

**What was built:**
- **Text BG rendering** (`ppu/bg.rs`):
  - Tile map parsing: 16-bit entries with tile number (0-1023), H/V flip, palette number
  - Screen block layout: 32x32 tiles per block, multi-block maps for 512x256, 256x512, 512x512
  - Character data: 4bpp (16 colors x 16 palettes, 32 bytes/tile) and 8bpp (256 colors, 64 bytes/tile)
  - Scrolling: per-BG X/Y offset registers
  - Mosaic: configurable H/V pixel size applied to scroll coordinates
  - Configurable character base (16KB units) and screen base (2KB units)
- **Affine BG rendering** (`ppu/bg.rs`):
  - Rotation/scaling via PA, PB, PC, PD 8.8 fixed-point parameters
  - Per-scanline reference point advancement: X += PB, Y += PD
  - Reference point reload from latched values at VBlank
  - 8-bit map entries (tile number only), always 8bpp, no flip
  - Wrapping mode and transparent-outside-boundary mode
  - Map sizes: 128x128 to 1024x1024 pixels (16x16 to 128x128 tiles)
- **Sprite rendering** (`ppu/obj.rs`):
  - OAM parsing: 128 entries x 8 bytes, three 16-bit attribute words
  - All 12 size combinations from the shape x size matrix (8x8 to 64x64)
  - Regular sprites: H/V flip
  - Affine sprites: rotation/scaling via 32 OAM affine parameter groups (PA/PB/PC/PD)
  - Double-size affine mode (mode 3): 2x bounding box for rotation without clipping
  - 4bpp (16 colors, per-sprite palette) and 8bpp (256 colors, single palette) color modes
  - 1D mapping (tiles sequential) and 2D mapping (32-tile-wide grid) via DISPCNT bit 6
  - OBJ VRAM at offset 0x10000, OBJ palette at palette offset 0x200
  - Per-sprite priority (0-3) with lower-index OBJ winning at same priority
- **Layer compositing** (`ppu/mod.rs`):
  - Priority-based sorting: lower priority number = visually on top
  - At same priority: OBJ beats BG, lower BG index beats higher BG index
  - Backdrop color (palette entry 0) as fallback when all layers are transparent
  - Mode-aware layer selection: Mode 0 (4 text BGs), Mode 1 (2 text + 1 affine), Mode 2 (2 affine)
  - Enabled-layer checking via DISPCNT bits 8-12

**How tile rendering works (the data flow):**
```
BGCNT register → screen_base (2KB block) → tile map in VRAM
                                            ↓
                              map entry: tile_number + flip + palette
                                            ↓
               char_base (16KB block) → character data in VRAM
                                            ↓
                              pixel index (4bpp or 8bpp lookup)
                                            ↓
                              Palette RAM → 15-bit RGB color
```

### Phase 4: DMA + Timers + Input [DONE]

**Goal**: Games become interactive — DMA transfers, timer-driven events, input response.

**What was built:**
- **DMA transfers** (`dma.rs`, rewritten ~330 lines):
  - Full address control: increment, decrement, fixed, increment-with-reload (dest only)
  - 16-bit and 32-bit transfer word sizes
  - Channel priority: processed in order 0→3 (channel 0 = highest priority)
  - Enable-bit 0→1 transition: latches source/destination/count from written registers
  - Address masking: DMA0-2 = 27-bit range, DMA3 = 28-bit (can reach ROM at 0x08000000)
  - Count = 0 treated as max: 0x4000 for DMA0-2, 0x10000 for DMA3
  - Repeat mode: reloads count (and dest if IncrementReload) after transfer, stays enabled
  - One-shot mode: auto-disables channel after transfer completes
  - FIFO special mode (DMA1/2): transfers 4 × 32-bit words to fixed dest, always repeats
  - DMA IRQ on completion (per-channel)
  - Bus-integrated `run_dma()` method: executes transfers directly on Bus memory, avoiding borrow-checker issues with closures
- **DMA trigger wiring**:
  - Immediate DMA: fires on control register write (enable bit 0→1 with immediate timing)
  - HBlank DMA: fires during scanline HBlank event (for scroll effects, HDMA tricks)
  - VBlank DMA: fires at VBlank start (line 160, for bulk data transfers)
  - FIFO DMA (Special timing): fires on Timer 0/1 overflow (for audio sample refill)
- **Timer ticking** (`timer.rs`, rewritten ~170 lines):
  - Per-CPU-step prescaler accumulation: fractional cycles tracked per timer
  - Prescaler dividers: F/1, F/64, F/256, F/1024
  - Overflow detection with correct reload behavior (counter wraps → reloads from reload register)
  - Cascade mode: timer N increments when timer N-1 overflows (not by prescaler)
  - Timer IRQ on overflow (Timer0-3)
  - Timer 0/1 overflow triggers FIFO DMA for sound channels
  - Reload on enable: counter set to reload value when start bit goes 0→1
- **Input**: Already functional from Phase 1 (KEYINPUT register + SDL2 keyboard mapping)
- **All interrupt sources wired**: VBlank, HBlank, VCount, Timer0-3, DMA0-3, Keypad

**How DMA avoids borrow-checker issues:**
The naive approach (DmaController borrows Bus for memory access) creates a double-mutable-borrow because Bus owns DmaController. Solution: `Bus::run_dma(channel_id)` executes the transfer directly as a Bus method. It reads DMA channel state, performs the memory copies via `self.read32()`/`self.write32()`, then updates the channel state — all within one `&mut self` borrow.

**How timers integrate with the main loop:**
```
CPU step() → returns N cycles
  → tick_timers(N)
    → accumulate prescaler, check overflows
    → if Timer0/1 overflow: trigger FIFO DMA (for sound)
    → if any timer overflow + IRQ enabled: request_irq(TimerN)
```

### Phase 5: Windows + Blending + Effects [DONE]

**Goal**: Full visual fidelity — window masking, alpha blending, brightness effects.

**What was built:**
- **Window masking** (`ppu/window.rs`, ~130 lines):
  - WIN0/WIN1: rectangular regions defined by WINH (X1,X2) and WINV (Y1,Y2) registers
  - OBJ Window: pixels where sprites with gfx_mode=2 are visible (mask built during OBJ render)
  - Per-pixel region determination with priority: WIN0 > WIN1 > OBJWIN > outside
  - Per-region layer visibility control: WININ bits 0-5 (WIN0/WIN1 each get 6 bits), WINOUT bits 0-5 (outside) and bits 8-13 (OBJWIN)
  - Each region independently enables/disables BG0-3, OBJ, and color effects
  - Wrapping window ranges supported (when X1 > X2 or Y1 > Y2, range wraps around)
  - When no windows are enabled (DISPCNT bits 13-15 all clear), all layers visible everywhere (fast path)
- **Color special effects** (`ppu/effects.rs`, ~170 lines):
  - **Alpha blending**: per-component `min(31, (C1*EVA + C2*EVB) / 16)` with EVA/EVB coefficients (0-16) from BLDALPHA register
  - **Brightness increase**: per-component `C + (31-C)*EVY/16` (fade toward white) from BLDY register
  - **Brightness decrease**: per-component `C - C*EVY/16` (fade toward black)
  - 1st/2nd target layer selection via BLDCNT bits 0-5 and 8-13 (BG0-3, OBJ, Backdrop)
  - Blend mode selection from BLDCNT bits 6-7 (None, Alpha, BrightnessUp, BrightnessDown)
  - Semi-transparent OBJ special case: gfx_mode=1 sprites always alpha-blend regardless of BLDCNT 1st target flags, but still require a valid 2nd target below them
- **Reworked compositing** (`ppu/mod.rs`):
  - Finds top TWO priority-sorted pixels per screen pixel (needed for alpha blending between 1st and 2nd target)
  - Window flags filter which layers participate in compositing per pixel
  - Effects are applied after compositing, conditional on window effects_enable flag
  - Compositing order: for each priority level, OBJ before BG, lower BG index before higher

**How the full compositing pipeline works per pixel:**
```
1. Determine window region → get WindowFlags (which layers visible, effects enabled?)
2. Collect opaque pixels from visible layers, sorted by priority
3. Identify top pixel (1st target candidate) and second pixel (2nd target candidate)
4. If semi-transparent OBJ on top + valid 2nd target → alpha blend (always)
5. Else if alpha mode + 1st target + 2nd target → alpha blend
6. Else if brightness mode + 1st target → brighten/darken
7. Else → use top pixel color directly
```

### Phase 6: Audio [DONE]

**Goal**: Full sound output — PSG channels, FIFO direct sound, mixing, SDL2 output.

**What was built:**
- **PSG Channels** (`apu/psg.rs`, ~340 lines):
  - **Channel 1**: Square wave with frequency sweep (shift, negate, period) and volume envelope (init, direction, period). Duty cycle selectable (12.5%, 25%, 50%, 75%). Length counter for auto-stop.
  - **Channel 2**: Square wave with volume envelope (same as Ch1 minus sweep).
  - **Channel 3**: Programmable waveform — 32 x 4-bit samples in wave RAM (two banks, single or double-bank playback). Volume: mute/100%/50%/25% + GBA force-75% mode.
  - **Channel 4**: Noise via LFSR (Linear Feedback Shift Register). Configurable 7-bit or 15-bit width. Clock shift and divisor for frequency control.
  - **Frame sequencer**: 512 Hz clock drives length counters (steps 0,2,4,6), sweep (steps 2,6), and envelope (step 7).
- **FIFO Direct Sound** (`apu/fifo.rs`, ~110 lines):
  - 32-byte circular FIFO buffers for channels A and B.
  - 8-bit signed samples, popped on Timer 0 or Timer 1 overflow (per SOUNDCNT_H timer select).
  - DMA refill request when FIFO drops to ≤16 bytes (half empty).
  - Write32 interface for DMA/CPU writes (4 samples per write).
  - Volume: 100% or 50% per SOUNDCNT_H.
- **APU Mixer** (`apu/mod.rs`, ~300 lines):
  - Per-channel left/right panning via SOUNDCNT_L.
  - PSG master volume (0-7 per side) and PSG ratio (25%/50%/100%) via SOUNDCNT_H.
  - FIFO channels mixed with independent L/R enable.
  - SOUNDBIAS applied to final output (default 0x200), clamped to 10-bit range.
  - Output scaled to signed 16-bit stereo for SDL2.
  - Sample generation at 32768 Hz (every 512 CPU cycles).
  - Sample buffer with overflow protection (max 8192 samples).
  - Full sound register I/O: read/write handlers for all NR10-NR52, SOUNDCNT_L/H/X, SOUNDBIAS, Wave RAM, FIFO A/B.
- **SDL2 Audio Output** (`gba-frontend/src/audio.rs`, ~80 lines):
  - `Arc<Mutex<Vec<i16>>>` shared buffer between emulation and audio threads.
  - SDL2 audio callback pulls stereo samples at 32768 Hz.
  - Silence fill on buffer underrun.
  - 512-sample callback buffer (~15.6ms latency).
  - `--no-audio` CLI flag to disable audio.
- **Integration with main loop**:
  - APU ticked every CPU step alongside timers.
  - Timer 0/1 overflow triggers `on_timer_overflow()` → pops FIFO sample → requests DMA refill.
  - `Gba::drain_audio()` public API for frontends to pull samples.

**How audio flows through the system:**
```
Game writes sound registers (0x04000060-0x040000A8)
  → APU updates channel state (frequency, envelope, duty, etc.)

Timer 0/1 overflow (e.g., at 16384 Hz for music)
  → APU pops sample from FIFO A/B
  → If FIFO half-empty → DMA1/2 refills 16 bytes from ROM/RAM

Every 512 CPU cycles (32768 Hz):
  → PSG channels produce samples (square/wave/noise)
  → FIFO channels produce samples (current latched value)
  → Mixer: PSG L/R + FIFO A L/R + FIFO B L/R
  → Apply SOUNDBIAS, clamp, scale to i16 stereo
  → Push to sample buffer

SDL2 audio callback (~60x per second):
  → Pull samples from shared buffer → DAC
```

### Phase 7: Saves + Save States [DONE]

**Goal**: Persistent game saves and instant save/load states.

**What was built:**
- **Flash backup** (`backup/flash.rs`, rewritten ~180 lines):
  - Full command state machine: Ready → Cmd1 (0xAA@0x5555) → Cmd2 (0x55@0x2AAA) → Command
  - Commands: Chip ID (0x90), Exit ID (0xF0), Erase (0x80→sector 0x30/chip 0x10), Write byte (0xA0), Bank switch (0xB0)
  - 64KB Atmel (ID: 0x1F/0x3D) and 128KB Sanyo (ID: 0x62/0x13) chip identification
  - 128KB bank switching: bank 0/1 selected via 0xB0 command, write to 0x0000
  - Flash write semantics: can only clear bits (AND with existing data), erase to restore 0xFF
  - Sector erase (4KB sectors) and full chip erase
- **EEPROM backup** (`backup/eeprom.rs`, rewritten ~200 lines):
  - Serial bit-banging protocol with proper state machine (Idle → CmdType → Address → Data → ReadOut/WriteDone)
  - Read command (11b): 6/14-bit address + dummy → outputs 4 dummy bits + 64 data bits MSB-first
  - Write command (10b): 6/14-bit address + 64 data bits + dummy → stores 8-byte block
  - Auto-detect 512B (6-bit address, 64 blocks) vs 8KB (14-bit address, 1024 blocks) by first access
  - Address captured during bit reception, data collected in 64-bit shift register
- **Save file I/O** (frontend):
  - `.sav` files auto-loaded on startup from `<rom_name>.sav`
  - `.sav` files auto-saved on exit
  - `Gba::export_save()` / `Gba::import_save()` public API
- **Save states** (core + frontend):
  - `Gba::save_state()` → `bincode::serialize(&self)` (entire emulator state)
  - `Gba::load_state()` → `bincode::deserialize()` (restores exact state)
  - Frontend: zstd compression/decompression for disk storage
  - Hotkeys: `]` = save state, `[` = load state
  - State file at `<rom_name>.state`

### Phase 8: Debugger [OPTIONAL]

**Status:** Deprioritized. Add small focused utilities when a specific bug calls for them, rather than building a full interactive CLI debugger up front.

**Why optional:** The pattern we've actually been using — one-shot diagnostic tools like `gba-core/examples/diagnose.rs`, `trace_escape.rs`, and `fb_dump.rs` — has covered every bug so far without needing an interactive debugger. Writing a bespoke Rust diagnostic per bug is faster than setting up breakpoints in a generic tool.

**Things to build on demand:**
- ARM + THUMB disassembler (useful when opcodes in traces get confusing — currently we read raw hex)
- Memory inspector example (dump OAM / palette / VRAM as hex, annotated)
- Per-component log levels (via `log` crate — already facility exists, just not wired)

**Skip unless needed:**
- Full CLI debugger with `step`/`break`/`continue`/`watch`
- GDB RSP remote debugging

### Phase 9: Accuracy Polish [IN PROGRESS]

**Goal**: Maximize game compatibility.

**General:**
- WAITCNT wait states were implemented but later reverted (commit `d9b57ea`) due to audio regressions in Pokémon Emerald; `add_mem_cycles` is currently a no-op stub. The API surface is kept for future re-enablement.
- Open bus behavior and BIOS read protection are implemented enough for known games, but not hardware-complete.
- Misaligned access quirks are partially implemented and still worth review.
- Forced blank, HBlank interval free, sprite/window/effect edge cases, and broad commercial game testing remain accuracy-polish work.

**BIOS HLE — incomplete (23 of ~40 SWIs implemented):**

Currently missing SWIs (games that use them won't work correctly without a real BIOS dump):

| SWI  | Name | Purpose |
|---|---|---|
| 0x19 | SoundBias | Sets audio output bias level |
| 0x1A | SoundDriverInit | Initializes the M4A sound driver |
| 0x1B | SoundDriverMode | Sets sound mixer mode |
| 0x1C | SoundDriverMain | Main mixer tick (per frame) |
| 0x1E | SoundChannelClear | Clears all sound channels |
| 0x1F | MidiKey2Freq | Converts MIDI note to GBA frequency register value |
| 0x20 | MusicPlayerOpen | Open music player slot |
| 0x21 | MusicPlayerStart | Start music playback |
| 0x22 | MusicPlayerStop | Stop music |
| 0x23 | MusicPlayerContinue | Resume music |
| 0x24 | MusicPlayerFadeOut | Fade out music |
| 0x25 | MultiBoot | Multi-boot (GBA-to-GBA transfer) |
| 0x26 | HardReset | Full reset (undocumented) |
| 0x27 | CustomHalt | Extended halt (undocumented) |
| 0x28 | SoundDriverVSyncOff | Pause sound VSync |
| 0x29 | SoundDriverVSyncOn | Resume sound VSync |
| 0x2A | GetJumpList | Return jump table (undocumented) |

Implementation note: SWIs that are unhandled currently just log a warning and return. Games that rely on them (likely most commercial titles using BIOS audio) will silently fail audio or other features.

Workaround: load a real GBA BIOS dump via `--bios gba_bios.bin` for games that depend on missing BIOS services. Pokémon Emerald and FE7 ship their own audio engines, so a real BIOS is not expected to fix their audio/timing bugs.

**Known game-specific issues** (tracked in `debug/` folder):
- Fire Emblem 7 HBlank cascade — **fixed** via IntrWait re-halt gate (commit `bb4b916`). See `debug/2026-05-24_fe7-hblank-irq-cascade.md`.
- SRTOG has residual audio distortion after the major FIFO cross-trigger fix.
- Pokémon Emerald RTC — **fixed** via S-3511 HLE in `rtc.rs`

**Milestone**: High compatibility across 50+ commercial titles.

## Project Structure

```
gba/
├── Cargo.toml                    # Workspace root
├── PLAN.md                       # This file
├── ARCHITECTURE.md               # Technical architecture deep-dive
├── .gitignore
│
├── gba-core/                     # Library crate (~10,500 lines)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                # Gba struct, run_frame(), scanline loop
│       ├── arm7tdmi/
│       │   ├── mod.rs            # Cpu struct, registers, CPSR, pipeline, modes
│       │   ├── arm.rs            # ARM instruction decoder + executor
│       │   ├── thumb.rs          # THUMB instruction decoder + executor
│       │   ├── alu.rs            # Barrel shifter, ALU ops, flag calculation
│       │   └── disasm.rs         # Disassembler (stub, Phase 8)
│       ├── bios.rs               # BIOS HLE — 23 SWI functions in Rust
│       ├── bus/
│       │   ├── mod.rs            # Bus struct, memory map, read/write dispatch
│       │   └── io_regs.rs        # I/O register read/write (0x04000000)
│       ├── ppu/
│       │   ├── mod.rs            # PPU orchestration, compositing, bitmap modes
│       │   ├── bg.rs             # Text BG + affine BG rendering
│       │   ├── obj.rs            # Sprite rendering (regular + affine)
│       │   ├── window.rs         # Window 0/1/OBJWIN region masking
│       │   └── effects.rs        # Alpha blending, brightness, target flags
│       ├── apu/
│       │   ├── mod.rs            # APU mixer, register I/O, sample generation
│       │   ├── psg.rs            # 4 PSG channels (square, wave, noise)
│       │   └── fifo.rs           # 2 DMA FIFO channels (A + B)
│       ├── dma.rs                # DMA controller + transfer execution
│       ├── timer.rs              # Timer ticking with prescaler + cascade
│       ├── interrupt.rs          # Interrupt controller (IE, IF, IME)
│       ├── keypad.rs             # Keypad input (KEYINPUT, KEYCNT)
│       ├── scheduler.rs          # Cycle-based event scheduler (min-heap)
│       └── backup/
│           ├── mod.rs            # BackupMedia enum, ROM signature detection
│           ├── sram.rs           # SRAM read/write
│           ├── flash.rs          # Flash (64KB/128KB, command state machine)
│           └── eeprom.rs         # EEPROM (512B/8KB, serial bit-bang protocol)
│
├── gba-frontend/                 # Binary crate (~950 lines)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs               # Entry point, arg parsing, main loop
│       ├── video.rs              # SDL2 window, 15-bit -> RGB24 color conversion
│       ├── audio.rs              # SDL2 audio callback + shared ring buffer
│       └── input.rs              # Keyboard -> GBA button mapping
│
└── test-roms/                    # .gitignored, user-supplied test ROMs
```

## Testing Strategy

- **Unit tests**: Every barrel shifter edge case, ARM/THUMB instruction, BIOS SWI function, BG/OBJ rendering, compositing
- **Integration tests**: End-to-end tests that assemble ARM instructions, run them, and check VRAM/framebuffer output
- **Test ROMs**: jsmolka arm/thumb, tonc demos, DMA/timer tests, AGS aging cart
- **Screenshot comparison**: Framebuffer vs reference images
- **Trace comparison**: Instruction logs vs mGBA
- **Fuzzing**: cargo-fuzz on decoders, bus, save state loading

## Dependencies

```toml
# gba-core
serde = "1" (with derive)   # Serialization for save states
bincode = "1"               # Binary encoding
log = "0.4"                 # Logging facade

# gba-frontend
sdl2 = "0.37" (bundled)     # Window, input, audio
clap = "4" (derive)         # CLI argument parsing
env_logger = "0.11"         # Log output
```

## Reference

- GBATEK specification: https://problemkaputt.de/gbatek.htm
- jsmolka test ROMs: CPU instruction validation
- tonc tutorials: GBA programming reference with demo ROMs
