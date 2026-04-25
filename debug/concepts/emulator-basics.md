# How a GBA emulator works (the basic principle)

At the simplest level, an emulator is a **simulator of hardware running in software**. The GBA has specific chips that do specific things at specific times; we recreate each chip as a Rust data structure and tick them forward together.

## The core idea: a big loop that advances time

```
loop forever:
  1. CPU executes one instruction            → returns "I used N cycles"
  2. Advance the clock by N cycles
  3. Tick every other component by N cycles  (PPU, APU, timers, DMA)
  4. If any scheduled event fires (HBlank/VBlank/timer overflow),
     handle it — which may update hardware state, fire IRQs, etc.
```

Every 280,896 cycles = one frame = ~16.7 ms of real time. Output the framebuffer to the screen, play audio samples, repeat at 60 Hz.

## The four main components we emulate

**CPU (ARM7TDMI)** — Reads instructions from memory, decodes them into enum variants, executes them. Has 16 registers, mode-switching (User/IRQ/FIQ/etc.), and runs in either ARM (32-bit) or THUMB (16-bit) instruction mode. Every instruction reads from or writes to memory via the bus. We implement each instruction as a function that manipulates registers and calls `bus.read32()` / `bus.write16()` etc.

**Memory bus** — A giant switch statement keyed on the top byte of the address:
- `0x00xxxxxx` → BIOS
- `0x02xxxxxx` → EWRAM (External Work RAM, 256 KB, slow)
- `0x03xxxxxx` → IWRAM (Internal Work RAM, 32 KB, fast)
- `0x04xxxxxx` → I/O registers (writing here triggers hardware behavior — e.g., writing to DMA control starts a transfer)
- `0x05xxxxxx` → Palette
- `0x06xxxxxx` → Video RAM
- `0x07xxxxxx` → Sprite attributes (OAM)
- `0x08xxxxxx` → Game cartridge ROM

See [memory-map.md](memory-map.md) for the full table, sizes, and mirroring rules.

**PPU (video)** — Renders the screen one scanline at a time. Given the current video mode (tile-based or bitmap), it reads tile maps from VRAM, looks up colors in the palette, applies scrolling/rotation/blending/windowing, and writes 240 pixels into the framebuffer row. Called once per visible scanline (160 times per frame).

**APU (audio)** — Runs 6 sound channels in parallel (4 tone generators + 2 DMA-fed sample channels). Every 512 CPU cycles (= 32,768 Hz sample rate), it mixes the current output of all channels into a stereo sample and queues it for SDL2 to play.

## The glue that makes it fast: event scheduler

Instead of checking "did anything happen this cycle?" every cycle (wasteful), we use a priority queue. Events like "HBlank in 960 cycles" get scheduled. The main loop runs the CPU at full speed until the next event's time, then dispatches the event. This skips millions of no-op checks per second. See [scheduler.md](scheduler.md) for the full deep-dive.

## How a game frame actually flows

```
Game: writes DISPCNT, BGCNT, scroll offsets, tile map, palette, etc.
        ↓ (these writes route through the I/O bus dispatcher)
PPU:  at HBlank, reads all that state and renders 240 pixels
        ↓
Repeat 160 times, then VBlank fires
        ↓
Game: receives VBlank IRQ, does work (e.g., updates sprite positions),
      eventually HALT's waiting for next VBlank
        ↓
Loop forever at 60 FPS
```

See [blanking-periods.md](blanking-periods.md) for what HBlank and VBlank actually are.

## The key trick for correctness

The GBA runs real game code — we don't know what it's going to do. So we must faithfully implement:
- Every CPU instruction exactly per the ARM7TDMI spec
- Every memory region with the right mirroring/alignment rules
- Every I/O register with correct read/write side effects
- Every hardware event (HBlank, VBlank, timer overflow, DMA trigger) at the right cycle

Bugs typically come from small corners being slightly wrong — each one manifests as "game runs for a bit, then breaks." See the per-bug records in `debug/` for concrete examples.

The entire `gba-core` crate is just a very detailed implementation of that loop: **loop, execute instruction, advance clock, dispatch events, repeat.**
