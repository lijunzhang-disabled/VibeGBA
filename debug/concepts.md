# Emulator Concepts — Reference Notes

Conceptual explanations of how the emulator works, separate from the per-bug records. Written for self-reference when you come back to the code after a break.

---

## Table of contents

1. [How a GBA emulator works (the basic principle)](#how-a-gba-emulator-works-the-basic-principle)
2. [The event scheduler (priority queue)](#the-event-scheduler-priority-queue)
3. [DirectSound FIFO DMA and the VBlank reset](#directsound-fifo-dma-and-the-vblank-reset)

---

## How a GBA emulator works (the basic principle)

At the simplest level, an emulator is a **simulator of hardware running in software**. The GBA has specific chips that do specific things at specific times; we recreate each chip as a Rust data structure and tick them forward together.

### The core idea: a big loop that advances time

```
loop forever:
  1. CPU executes one instruction            → returns "I used N cycles"
  2. Advance the clock by N cycles
  3. Tick every other component by N cycles  (PPU, APU, timers, DMA)
  4. If any scheduled event fires (HBlank/VBlank/timer overflow),
     handle it — which may update hardware state, fire IRQs, etc.
```

Every 280,896 cycles = one frame = ~16.7 ms of real time. Output the framebuffer to the screen, play audio samples, repeat at 60 Hz.

### The four main components we emulate

**CPU (ARM7TDMI)** — Reads instructions from memory, decodes them into enum variants, executes them. Has 16 registers, mode-switching (User/IRQ/FIQ/etc.), and runs in either ARM (32-bit) or THUMB (16-bit) instruction mode. Every instruction reads from or writes to memory via the bus. We implement each instruction as a function that manipulates registers and calls `bus.read32()` / `bus.write16()` etc.

**Memory bus** — A giant switch statement keyed on the top byte of the address:
- `0x00xxxxxx` → BIOS
- `0x02xxxxxx` → External RAM
- `0x03xxxxxx` → Internal RAM
- `0x04xxxxxx` → I/O registers (writing here triggers hardware behavior — e.g., writing to DMA control starts a transfer)
- `0x05xxxxxx` → Palette
- `0x06xxxxxx` → Video RAM
- `0x07xxxxxx` → Sprite attributes
- `0x08xxxxxx` → Game cartridge ROM

**PPU (video)** — Renders the screen one scanline at a time. Given the current video mode (tile-based or bitmap), it reads tile maps from VRAM, looks up colors in the palette, applies scrolling/rotation/blending/windowing, and writes 240 pixels into the framebuffer row. Called once per visible scanline (160 times per frame).

**APU (audio)** — Runs 6 sound channels in parallel (4 tone generators + 2 DMA-fed sample channels). Every 512 CPU cycles (= 32,768 Hz sample rate), it mixes the current output of all channels into a stereo sample and queues it for SDL2 to play.

### The glue that makes it fast: event scheduler

Instead of checking "did anything happen this cycle?" every cycle (wasteful), we use a priority queue. Events like "HBlank in 960 cycles" get scheduled. The main loop runs the CPU at full speed until the next event's time, then dispatches the event. This skips millions of no-op checks per second. Detailed below.

### How a game frame actually flows

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

### The key trick for correctness

The GBA runs real game code — we don't know what it's going to do. So we must faithfully implement:
- Every CPU instruction exactly per the ARM7TDMI spec
- Every memory region with the right mirroring/alignment rules
- Every I/O register with correct read/write side effects
- Every hardware event (HBlank, VBlank, timer overflow, DMA trigger) at the right cycle

Bugs typically come from small corners being slightly wrong — each one manifests as "game runs for a bit, then breaks." See the per-bug records in this folder for concrete examples.

The entire `gba-core` crate is just a very detailed implementation of that loop: **loop, execute instruction, advance clock, dispatch events, repeat.**

---

## The event scheduler (priority queue)

### The problem it solves

Imagine the naive approach:

```
loop forever:
  1. Execute one CPU cycle
  2. Ask PPU: "Is it HBlank yet?"
  3. Ask PPU: "Is it VBlank yet?"
  4. Ask each timer: "Have you overflowed yet?"
  5. Ask each DMA channel: "Should you trigger now?"
  6. Ask APU: "Time for a new audio sample?"
```

That's ~10 checks per CPU cycle × 16.78 million cycles per second = **168 million checks per second, 99.9% of which return "no"**. Wasteful.

### The insight

We can **predict the future** for each hardware component:
- HBlank will fire at cycle 960 (next scanline boundary)
- VBlank will fire at cycle 197,120 (start of line 160)
- Timer 0 (prescaler 64) will overflow in 64 × remaining_count cycles
- Next audio sample at cycle 512 (and every 512 cycles after that)

So instead of checking every cycle, we **put all predicted events in a queue sorted by when they'll fire**, and run the CPU flat-out until we hit one.

### The data structure: min-heap

A min-heap is a tree where the smallest value is always at the top. "Smallest" here means "earliest fire_time":

```
         HBlank@960         ← root (soonest)
        /          \
  APUSample@1024   HBlankEnd@1232
     /
 VBlank@197120
```

Popping the root gives us the next event in O(log n). Inserting a new event is also O(log n). Rust's `std::collections::BinaryHeap` is a max-heap by default, so we just reverse the `Ord` impl on our `Event` struct to make it a min-heap.

### A concrete walkthrough

Let's say the emulator just started. `timestamp = 0`. Initial state:

```
events = [
  { fire_time: 960, kind: HBlank },     ← first HBlank
]
```

**Step 1** — Main loop peeks at the queue: next event at 960. Run CPU until clock = 960.

The CPU might execute 200 instructions, each taking 1-5 cycles, summing to 960 cycles. We don't check anything during those 200 instructions — just run.

After the CPU loop, `timestamp = 960`.

**Step 2** — Pop the HBlank event. Dispatch it:

```rust
EventKind::HBlank => {
    // Set HBlank bit in DISPSTAT
    // Render scanline 0 into the framebuffer
    // Fire HBlank DMA if configured
    // Schedule the end of HBlank
    scheduler.push(Event {
        fire_time: 960 + 272,  // HBlank lasts 272 cycles
        kind: HBlankEnd,
    });
}
```

Queue is now:

```
events = [
  { fire_time: 1232, kind: HBlankEnd }
]
```

**Step 3** — Peek again: next at 1232. Run CPU 272 more cycles. `timestamp = 1232`.

**Step 4** — Pop HBlankEnd. Dispatch:

```rust
EventKind::HBlankEnd => {
    io.vcount += 1;           // Move to next scanline
    // Schedule the next HBlank
    scheduler.push(Event {
        fire_time: 1232 + 960,
        kind: HBlank,
    });
    // If vcount == 160, also schedule VBlank
}
```

**Each event schedules the next one.** That's the key to keeping the queue populated without any central "scheduler planner."

### Events re-schedule themselves

This is the clever bit. Look at the lifecycle of just HBlank events:

```
HBlank (at 960) → pushes HBlankEnd (at 1232)
  HBlankEnd (at 1232) → pushes next HBlank (at 2192)
    HBlank (at 2192) → pushes HBlankEnd (at 2464)
      ...forever
```

Same for timers:

```
TimerOverflow (at cycle X) →
  - increments counter (or reloads)
  - fires IRQ if enabled
  - pushes next overflow at X + (reload_ticks × prescaler)
```

### How the main loop actually looks

```rust
pub fn run_frame(&mut self) {
    let frame_end = self.scheduler.timestamp() + CYCLES_PER_FRAME;

    while self.scheduler.timestamp() < frame_end {
        // How long can we run the CPU freely?
        let next_event = self.scheduler.peek_time().unwrap_or(frame_end);
        let target = next_event.min(frame_end);

        // Run CPU at full speed until that time
        while self.scheduler.timestamp() < target {
            let cycles = self.cpu.step(&mut self.bus);
            self.scheduler.add_cycles(cycles as u64);
        }

        // Fire all events that are now ready
        while let Some(event) = self.scheduler.pop_if_ready() {
            self.handle_event(event);
        }
    }
}
```

Three nested layers:

1. **Outer**: run until end of frame
2. **Middle**: peek at next event, run CPU until it
3. **Inner**: step CPU one instruction at a time

### Why it's fast

In a typical frame:

- ~280,000 CPU cycles
- ~500 scheduler events total (HBlanks, VBlanks, timer overflows, audio samples)

So we amortize ~560 CPU cycles per heap operation. With a heap of ~5-10 events, `push`/`pop` is essentially a handful of comparisons. **Overhead is ~1% of total runtime**, vs 50-80% with the naive polling approach.

### The mental model

Picture the CPU as a runner and the scheduler as a meeting schedule:

> "The runner sprints until the next meeting on the calendar. At the meeting, the hardware does its thing, schedules its next meeting, and the runner goes again."

The runner never stops to ask "is there a meeting yet?" — the calendar answers that question once, at the start of each sprint.

---

## DirectSound FIFO DMA and the VBlank reset

This explains a non-obvious thing the emulator does in `Gba::handle_event`: on every VBlank entry it force-resets `internal_sad` to `sad` for any DMA1/DMA2 channel running in Special (FIFO) timing. If you read that code cold, "why is the scheduler poking at DMA registers?" is a fair question. Here's the model.

### The DirectSound FIFO architecture

Each of the GBA's two DirectSound channels (FIFO A at `0x040000A0`, FIFO B at `0x040000A4`) is a 32-byte hardware queue. Sound output drains it byte-by-byte — one signed-int8 sample at a time — at a rate set by Timer 0 overflow. Pokémon's M4A configures Timer 0 to overflow at exactly 13379 Hz, so the FIFO drains 13379 samples/sec.

To keep the FIFO supplied, the game sets up DMA1 (and/or DMA2) like this:
- **`SAD`**: pointer to a sample buffer in IWRAM/EWRAM that the CPU continuously refills with rendered samples.
- **`DAD`**: fixed at the FIFO register address.
- **Timing**: "Special", which on DMA1/2 means *trigger-on-Timer-0-overflow*.
- **Repeat**: set, so the channel stays armed across triggers.

Each Timer 0 overflow, the DMA controller transfers exactly **4 words (16 bytes = 16 samples)** from the source to the FIFO, then advances the *internal* source pointer by 16. That internal pointer never gets reset implicitly — it just keeps walking forward through memory.

### The problem this creates

If you set `SAD = buffer_start` and let DMA run, the internal source advances 224 bytes per frame (13379 × 1/60). After a few frames it has walked past the end of the buffer and starts reading whatever is in the next region of memory — typically uninitialised IWRAM, game state, or VRAM. The FIFO then plays that as "audio", which sounds like noise.

### How the hardware expects games to fix this

GBATEK documents `SoundDriverVSync` (SWI `0x1D`) as the canonical mechanism:

> "An extremely short system call that resets the sound DMA. The timing is extremely critical, so call this function immediately after the V-Blank interrupt every 1/60 second."

Internally this just toggles the enable bit of `DMAxCNT_H` for both DMA1 and DMA2. The 0→1 transition causes the DMA controller to **re-latch** `internal_sad` from the user-programmed `SAD` register — effectively snapping the read cursor back to the start of the buffer. Meanwhile, the CPU has spent the last frame writing fresh samples to that buffer, so DMA finds new data each time it loops.

So the canonical contract is:

```
Every VBlank:
  1. CPU renders samples for the next frame into the buffer at SAD.
  2. CPU calls SWI 0x1D (or inlines its body): toggles DMA1/2 enable.
  3. DMA controller re-latches internal_sad = SAD on the 0→1 edge.
  4. DMA reads from start-of-buffer for the next ~13379/60 ≈ 224 samples,
     fed to the FIFO at the timer-driven rate.
```

### What we do, and why

Most games either call `SWI 0x1D` directly or inline equivalent register-toggle code into their VBlank IRQ handler. Pokémon Emerald is an outlier: our SWI tracer caught zero sound SWIs and our DMA-register-write tracer caught zero `DMA1/2` register writes after the boot init. The game must be relying on some path we haven't traced — possibly a CPU-accuracy bug along an instruction sequence the jsmolka tests don't exercise — to invoke the reset, and on real hardware it Just Works.

Rather than guess at the missing path, we do the safe thing: at every VBlank, in the scheduler's `HBlankEnd` handler when scanline becomes 160, we look at DMA1 and DMA2; if either is `active` and timing is `Special`, we force `internal_sad = sad`. This is exactly what `SoundDriverVSync` does, just unconditional rather than triggered by the SWI.

The behavioural cost: for games that *do* call SWI 0x1D themselves, our reset is redundant (idempotent — re-latching to the same value is a no-op). We never worsen behaviour; we only rescue games like Pokémon that depend on the reset happening but don't drive it through a path our emulator handles.

If you ever find a game whose audio breaks because of this auto-reset (e.g. a game intentionally lets DMA stream through a long contiguous buffer without resetting), the right fix is to remove the auto-reset and trace down why the game's SWI 0x1D / inlined equivalent isn't running.

### Where this lives in code

- `gba-core/src/lib.rs::handle_event`, in the `EventKind::HBlankEnd` arm, right after the `if line == VISIBLE_LINES` block runs the VBlank-timed DMA.
- `gba-core/src/bios.rs::swi_sound_driver_vsync` (handler for SWI 0x1D, also implemented for games that do call it).
- See `debug/2026-04-25_pokemon-audio-dma-reanchor.md` for the full investigation that led here.
