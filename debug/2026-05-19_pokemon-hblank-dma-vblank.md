# Pokémon Emerald cave-flash: HBlank DMA firing during VBlank

Date: 2026-05-19
Status: **Fixed** (commit pending)

## Symptom

In Pokémon Emerald's dark caves (Granite Cave without Flash, etc.) the
visible circular region was rendered at the **top of the screen**
instead of around the character. The circle's shape was preserved
(still circular, not distorted), and the offset was constant — moving
the character did not change the offset relative to the screen edges.

## Plain-English summary

The game streams a 160-entry "circle table" via HBlank DMA, one entry
per scanline, to dynamically reshape `WIN0H` for each visible line —
that's how the circle gets its smooth curved edges. Real GBA hardware
fires HBlank DMA **only during visible scanlines (lines 0-159), not
during the VBlank period (160-227)**. Our emulator was firing it on
every HBlank including VBlank lines, so the DMA was consuming ~68
table entries per frame during VBlank that real hardware wouldn't.

By the time visible line 0 was rendered, the DMA source pointer had
already drifted to roughly `table[68]` — which happens to be near the
**equator** of the circle (the widest entry). The visible scanlines
then rendered the **bottom half** of the circle, drawn from the top
of the screen downward, ending around scanline 60. That's exactly
what the user saw: a circle near the top of the screen rather than
centered on the character.

## Investigation chain

1. **User report**: vertical offset, ~30-60 px, circle pushed toward
   top of screen, character in the middle. Shape preserved.

2. **First check** — WIN0H/V byte ordering, register routing, and
   `is_in_window_range` logic in `compute_window_line`. All looked
   correct against GBATEK.

3. **Added `WIN_TRACE` env-gated logging** of every write to
   `0x04000040..0x04000046` (WIN0H / WIN1H / WIN0V / WIN1V) with the
   decoded X1/X2 or Y1/Y2 and the current VCOUNT.

4. **Captured WIN0H trace during the cave**:
   ```
   vcount=167  WIN0H = (X1=111, X2=129)   width=18   ← narrow top of circle
   vcount=168  WIN0H = (X1=105, X2=135)   width=30
   vcount=170  WIN0H = (X1=97,  X2=143)   width=46
   ...
   vcount=227  WIN0H = ...                            ← end of vblank
   vcount=0    WIN0H = (X1=49,  X2=191)   width=142  ← circle equator!
   vcount=12   WIN0H = (X1=48,  X2=192)   width=144
   ```

   The pattern is unmistakable: **the circle table is being streamed
   starting from VBlank line 167**, with the equator entries landing
   at visible line 0+. The DMA had consumed ~68 entries before
   visible rendering even began.

5. **Root cause identified**: HBlank DMA firing during VBlank
   lines. Per Cowbite Spec (and confirmed empirically in our trace):
   real GBA fires HBlank DMA only on lines 0-159. Our HBlank event
   handler in `lib.rs` fired `run_dma_for_timing(HBlank)` for every
   scanline including 160-227.

## Fix

`lib.rs` HBlank event handler:

```rust
// Trigger HBlank DMA — but ONLY for visible scanlines (0..159).
// Real GBA does not fire HBlank DMA during VBlank lines (160..227).
if line < VISIBLE_LINES {
    self.run_dma_for_timing(dma::DmaTiming::HBlank);
}
```

Single branch, in the hot HBlank event path (fires 228× per frame),
but the branch itself is essentially free.

## Verification

- All 90 unit tests pass.
- Pokémon Emerald cave-flash: visible circle now centered around the
  character (mid-screen) instead of pushed to the top.
- No regressions in non-cave Pokémon gameplay.
- SRTOG still boots and plays as before (it doesn't use HBlank DMA
  for window effects, so unaffected).

## Why this didn't break anything else

HBlank DMA is mostly used for two things:
1. Per-scanline register updates (BG scroll, BG affine params,
   window coords, palette gradients) — these need to apply during
   visible rendering, so gating to visible-only is correct.
2. Streaming sample data to audio FIFOs — but those use Special
   timing (DMA1/2 FIFO mode), not HBlank timing. Unaffected.

In fact, for any game that streams a 160-entry per-scanline table,
this fix moves us toward hardware accuracy: the table no longer
drifts forward during VBlank.

## Diagnostics kept in tree

- `WIN_TRACE=1` env var: logs every WIN0H/WIN0V/WIN1H/WIN1V write
  with decoded coords and current VCOUNT. Cached via `OnceLock`,
  so the env var lookup is essentially free when unset. Useful for
  future window/HDMA debugging.
