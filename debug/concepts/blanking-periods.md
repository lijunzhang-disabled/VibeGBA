# HBlank and VBlank — what they are and why every game pivots on them

## The CRT legacy

These concepts come from CRT television hardware, but they're still real on the GBA because the LCD pretends to be a CRT for software-compatibility reasons (every Game Boy and GBA game relies on them, so the hardware models them faithfully).

Imagine an electron beam (or, on the GBA, an emulated equivalent) sweeping across the screen drawing pixels:

```
   ─────────────────────────►   draws scanline 0 (top row)
                            ↓
   ◄─────────────────────────   "fly back" to the left edge — HBlank
                            ↓
   ─────────────────────────►   draws scanline 1
                            ↓
   ...
                            ↓
   ◄─────────────────────────   bottom-right → top-left — VBlank
```

The beam *only draws pixels while it's sweeping rightward across a visible row*. The rest of the time it's "flying back" to the next start position. Those flyback periods are the **blanks**.

## HBlank — Horizontal Blank

The gap between drawing one scanline and starting the next.

On the GBA:
- A scanline is **308 "dots"** total: **240 visible** + **68 HBlank**.
- Each dot = 4 CPU cycles, so HBlank = **272 cycles** per line.
- During HBlank, the PPU isn't drawing pixels, so VRAM is safe to write to.
- The PPU also fires an HBlank IRQ if `DISPSTAT` bit 4 is set.

## VBlank — Vertical Blank

The gap between finishing the bottom row of the visible image and starting the next frame's top row.

On the GBA:
- A frame is **228 lines** total: **160 visible** + **68 VBlank**.
- VBlank = **68 lines × 1232 cycles/line ≈ 83,776 cycles** of off-screen time per frame.
- During VBlank, the PPU isn't reading VRAM/OAM/palette, so games can do *big* updates safely (sprite tables, scrolling, sound buffer rendering, decompression, etc.).
- Fires the VBlank IRQ if `DISPSTAT` bit 3 is set.

## Why games care

These are the only "safe" windows to mutate display memory. If you write to VRAM mid-scanline, you can produce flicker or torn frames. The standard GBA loop is:

```
main loop:
  wait for VBlank IRQ (or busy-poll DISPSTAT bit 0)
  do everything (game logic, render sprites, render audio samples, ...)
  ← when VBlank ends, the next frame starts drawing
  loop
```

VBlank is also the heartbeat of the whole console — it fires at exactly **59.737 Hz**, which is the frame rate, and almost every game ties its main loop to it. Audio rendering, animation, input polling — all happen in VBlank.

## In our emulator

`gba-core/src/lib.rs::handle_event` simulates the timing with two scheduler events:

- `EventKind::HBlank`: PPU has just finished drawing the visible part of a scanline. Sets DISPSTAT bit 1 (HBlank flag), fires HBlank IRQ if enabled, runs HBlank-timed DMA.
- `EventKind::HBlankEnd`: scanline is over. Increments `VCOUNT`, checks for VCount match, and *if we just rolled over from line 159 to 160*, that's the start of VBlank — sets DISPSTAT bit 0, fires VBlank IRQ, runs VBlank-timed DMA, re-anchors sound DMA (see [fifo-dma-vblank.md](fifo-dma-vblank.md)).

The constants live near the top of `lib.rs`:

```rust
pub const VISIBLE_LINES: u16 = 160;
pub const VBLANK_LINES: u16 = 68;
pub const LINES_PER_FRAME: u16 = 228;          // 160 + 68
pub const HDRAW_CYCLES: u32 = 240 * 4;          // 960
pub const HBLANK_CYCLES: u32 = 68 * 4;          // 272
pub const CYCLES_PER_FRAME: u64 = 280_896;      // 228 × 1232
```

So when you see "fire VBlank IRQ at line 160" in our scheduler, that's literally the moment the imaginary CRT beam finishes the last visible row and starts its long flyback to the top — and that's the moment the game gets to run its per-frame work.

## Why scanline 160, not 161?

Lines are numbered 0–227. Lines 0–159 (160 of them) are visible. Line 160 is the *first* scanline of VBlank. So "VBlank starts when VCOUNT becomes 160" is correct — the moment we've just finished drawing line 159 and the beam is heading off-screen.

Lines 160–227 are all VBlank lines (no rendering). Line 0 of the next frame restarts visible rendering.

## Related

- [emulator-basics.md](emulator-basics.md) — where blanking fits into the overall frame loop.
- [scheduler.md](scheduler.md) — how HBlank/HBlankEnd events are scheduled and dispatched.
- [fifo-dma-vblank.md](fifo-dma-vblank.md) — what happens during VBlank for sound DMA, and why our scheduler does extra work there.
