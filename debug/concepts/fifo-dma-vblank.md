# DirectSound FIFO DMA and the VBlank reset

This explains a non-obvious thing the emulator does in `Gba::handle_event`: on every VBlank entry it force-resets `internal_sad` to `sad` for any DMA1/DMA2 channel running in Special (FIFO) timing. If you read that code cold, "why is the scheduler poking at DMA registers?" is a fair question. Here's the model.

## The DirectSound FIFO architecture

Each of the GBA's two DirectSound channels (FIFO A at `0x040000A0`, FIFO B at `0x040000A4`) is a 32-byte hardware queue. Sound output drains it byte-by-byte — one signed-int8 sample at a time — at a rate set by Timer 0 overflow. Pokémon's M4A configures Timer 0 to overflow at exactly 13379 Hz, so the FIFO drains 13379 samples/sec.

To keep the FIFO supplied, the game sets up DMA1 (and/or DMA2) like this:
- **`SAD`**: pointer to a sample buffer in IWRAM/EWRAM that the CPU continuously refills with rendered samples.
- **`DAD`**: fixed at the FIFO register address.
- **Timing**: "Special", which on DMA1/2 means *trigger-on-Timer-0-overflow*.
- **Repeat**: set, so the channel stays armed across triggers.

Each Timer 0 overflow, the DMA controller transfers exactly **4 words (16 bytes = 16 samples)** from the source to the FIFO, then advances the *internal* source pointer by 16. That internal pointer never gets reset implicitly — it just keeps walking forward through memory.

## The problem this creates

If you set `SAD = buffer_start` and let DMA run, the internal source advances 224 bytes per frame (13379 × 1/60). After **one** frame it has already walked past the end of a typical M4A buffer and starts reading whatever is in the next region of memory — typically uninitialised IWRAM, game state, or VRAM. The FIFO then plays that as "audio", which sounds like noise.

Concretely for Pokémon Emerald, the FIFO A buffer at `0x030066D0` is exactly **224 bytes** (one frame's worth). M4A overwrites those 224 bytes with the next frame's samples every VBlank. DMA reads 224 bytes per frame too — so if `internal_sad` doesn't reset back to `0x030066D0` each VBlank, by the next frame DMA is reading from `0x030067B0` onwards, where M4A has never written anything.

## How the hardware expects games to fix this

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

## What we do, and why

Most games either call `SWI 0x1D` directly or inline equivalent register-toggle code into their VBlank IRQ handler. Pokémon Emerald is an outlier: our SWI tracer caught zero sound SWIs and our DMA-register-write tracer caught zero `DMA1/2` register writes after the boot init. The game must be relying on some path we haven't traced — possibly a CPU-accuracy bug along an instruction sequence the jsmolka tests don't exercise — to invoke the reset, and on real hardware it Just Works.

Rather than guess at the missing path, we do the safe thing: at every VBlank, in the scheduler's `HBlankEnd` handler when scanline becomes 160, we look at DMA1 and DMA2; if either is `active` and timing is `Special`, we force `internal_sad = sad`. This is exactly what `SoundDriverVSync` does, just unconditional rather than triggered by the SWI.

The behavioural cost: for games that *do* call SWI 0x1D themselves, our reset is redundant (idempotent — re-latching to the same value is a no-op). We never worsen behaviour; we only rescue games like Pokémon that depend on the reset happening but don't drive it through a path our emulator handles.

If you ever find a game whose audio breaks because of this auto-reset (e.g. a game intentionally lets DMA stream through a long contiguous buffer without resetting), the right fix is to remove the auto-reset and trace down why the game's SWI 0x1D / inlined equivalent isn't running.

## Why only Special-timed DMA needs this

A natural follow-up: if `internal_sad` walks forever, doesn't *every* DMA channel have this problem — including the VBlank/HBlank ones used for graphics? Why is our reset specifically targeting `Special`-timed DMA1/DMA2?

The answer is a combination of **firing rate** and **whether the walk is wanted**.

| Timing | Fires when… | Rate | Walk per frame | Game's intent |
|---|---|---|---|---|
| `Immediate` | enable bit goes 0→1 | once per "submit" | n/a (one-shot) | Channel disables itself when count hits 0. No repetition. |
| `VBlank` | scanline → 160 | 60 Hz | small (`count × 4` bytes per frame) | Usually a fresh setup each frame anyway. |
| `HBlank` | every scanline | ~14,400 Hz | tracks scanline count | **Wants** the walk — one entry per scanline (HDMA effects). |
| `Special` (FIFO) | every Timer 0 overflow | ~13,379 Hz | 224 bytes per frame | **Doesn't** want the walk — replay one buffer on a loop. |

**Immediate.** One-shot. Hardware clears the enable bit after the transfer completes. Nothing to walk.

**VBlank.** Fires once per frame. A typical use is "copy 1 KB from a sprite-table buffer in EWRAM into OAM at VBlank." If the game uses `Repeat` mode, `internal_sad` does walk forward — but most games disable+re-enable each frame anyway (because the source changes per-frame, e.g. different sprite list), which re-latches everything. When games *do* leave it running with `Repeat` and a small `count`, the walk is slow enough that it usually points at fresh CPU-rendered data.

**HBlank.** The classic HDMA trick: each scanline, copy *one* entry from a 160-entry table into a register. E.g. update `BG2X` per line for a wavy water effect, or per-line palette swaps for split-screen, or Mode 7-style affine transforms.

```
 source: [scroll[0], scroll[1], scroll[2], ..., scroll[159]]
                ↓        ↓         ↓                  ↓
 line 0:    BG2X    line 1: BG2X   ...    line 159: BG2X
```

Here the walking source pointer is the **whole point**. After 160 lines, the game uses `IncrementReload` on the destination so the *destination* snaps back at next VBlank, while the source either continues (if the next-frame's table is contiguous in memory) or gets re-anchored by the game.

**Special (FIFO sound).** This is the odd one out. The game writes a **single buffer** and expects DMA to replay it on a loop while the CPU overwrites it each frame. The hardware's natural behaviour ("source advances every transfer") is *exactly wrong* for this. That's why the BIOS has a dedicated function — `SoundDriverVSync` (SWI 0x1D) — whose only job is to re-latch DMA1/DMA2 internal_sad every VBlank. It's a workaround for an architectural mismatch between FIFO DMA's source-advancement and the sample-replay use case.

Hence our reset is filtered to `Special` timing only:

```rust
for ch in [1usize, 2] {
    let c = &mut self.bus.dma.channels[ch];
    if c.active && matches!(c.timing(), dma::DmaTiming::Special) {
        c.internal_sad = c.sad & 0x07FF_FFFF;
    }
}
```

We don't touch HBlank-timed DMA channels (the HDMA scroll-table use case would break). We don't touch VBlank-timed DMA either — games handle their own re-latching there, and unconditionally snapping `internal_sad` could break long copies that intentionally span multiple VBlanks. We only fix what the BIOS's `SoundDriverVSync` would have fixed.

## Where this lives in code

- `gba-core/src/lib.rs::handle_event`, in the `EventKind::HBlankEnd` arm, right after the `if line == VISIBLE_LINES` block runs the VBlank-timed DMA.
- `gba-core/src/bios.rs::swi_sound_driver_vsync` (handler for SWI 0x1D, also implemented for games that do call it).

## Related

- [dma-registers.md](dma-registers.md) — SAD/DAD/FIFO terminology, `sad` vs `internal_sad`, and why GBA DMA is so different from queue-based modern DMA.
- [timers.md](timers.md) — what "Timer 0 overflow" means and how the reload value sets the audio sample rate.
- [memory-map.md](memory-map.md) — IWRAM mirroring is the reason DMA reads "garbage" instead of just crashing when it walks past the buffer.
- [blanking-periods.md](blanking-periods.md) — what VBlank actually is.
- [scheduler.md](scheduler.md) — how the VBlank moment is dispatched (`HBlankEnd` event, `line == 160` branch).
- `../2026-04-25_pokemon-audio-dma-reanchor.md` — full investigation log that led to this auto-reset.
