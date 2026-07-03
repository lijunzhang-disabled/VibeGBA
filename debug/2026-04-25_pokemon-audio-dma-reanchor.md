# Pokémon audio: FIFO DMA must be re-anchored every VBlank

Date: 2026-04-25
Status: **Fixed** (brute-force behavioural fix)

## Symptom

Pokémon Emerald audio sounded like a periodic "zizizi" (clicky impulse
train) with long silent gaps. User described it as "noisy every 3 seconds
ish, no music". The same symptom persisted across all our ARM7TDMI
accuracy fixes from the jsmolka sweep.

## Investigation

After the full test-ROM sweep left arm.gba/thumb.gba/memory.gba passing,
audio was still broken. We verified in order:

1. **Audio pipeline** (APU mixer, SDL2 callback, FIFO decode) — correct.
   48 kHz output, histogram showed 57% zero samples interleaved with
   real audio-looking content.
2. **Timer/DMA/FIFO timing** (`examples/irq_audit.rs`) — exactly correct.
   Timer 0 overflowing at 13379 Hz; DMA1/2 active in FIFO mode with
   right destinations; FIFO A/B enabled correctly in SOUNDCNT_H.
3. **VBlank IRQ generation** — the PPU fires VBlank IRQ each frame
   (`examples/irq_count.rs` counted 200 raises in 200 frames).
4. **CPU IRQ handler entry** — the game's ISR runs 189/200 times
   (DISPSTAT bit 3 is set by frame ~11, matching the first 11 "misses").
5. **IRQ-to-poll-loop flow** (`examples/trace_irq_handler.rs`) — game
   polls at `0x080008C6`, IRQ fires, user handler at IWRAM `0x03002750`
   runs, poll loop correctly breaks out to main vblank code at
   `0x080004BE`. All normal.
6. **Sample buffer content** (`examples/dump_sample_buffer.rs`) — M4A
   writes ~224 real signed-int8 samples to IWRAM at `0x030066D0` each
   frame (FIFO A buffer) and another ~224 at `0x03006D00` (FIFO B).
   Very small amplitude (±4 of 127) but real music.
7. **DMA source tracking** (`examples/fifo_trace.rs`) — the smoking gun.
   `DMA1.sad` stayed at the initial `0x030066D0` for 400 frames, never
   rewritten. But `DMA1.internal_sad` advanced 224 bytes per frame
   (the correct FIFO drain rate), so by frame 200 the read cursor had
   drifted ~45 KB away from the M4A buffer — reading random IWRAM
   mirror content instead of samples.
8. **SWI histogram** (`examples/check_test.rs` + SWI_TRACE) — Pokémon
   calls only SWI 0x0F (ObjAffineSet), 0x0B (CpuSet), 0x12 (LZ77VRAM),
   0x11 (LZ77WRAM), and 0x01 (RegisterRAMReset). **Zero sound SWIs.**
   No `0x1D SoundDriverVSync`, no `0x1A SoundDriverInit`, no polling
   via `0x05 VBlankIntrWait` (it uses a busy poll on IWRAM).
9. **DMA register writes** (`examples/trace_dma_writes.rs` + DMA_TRACE) —
   DMA1/DMA2 control and SAD registers are written exactly four times
   during boot and never again.

So: Pokémon sets up the DMA once, writes samples to a fixed IWRAM
buffer every frame, and *expects the DMA to keep reading from the
start of that buffer*. The standard M4A pattern (disable DMA → write
SAD → enable DMA each vblank) either never happens in this ROM or is
handled via some path we can't see.

## Root cause

Our FIFO DMA correctly advances `internal_sad` across timer triggers
— which matches what ARM documentation says the hardware does. But in
Pokémon's case, nothing ever resets `internal_sad`, so it drifts
through the entire IWRAM mirror space, reading garbage.

On real hardware, the canonical mechanism is documented in GBATEK:

> **SoundDriverVSync (SWI 1Dh):** "An extremely short system call that
> resets the sound DMA. The timing is extremely critical, so call this
> function immediately after the V-Blank interrupt every 1/60 second."

So on real hardware the BIOS function `SoundDriverVSync` is what resets
the DMA each VBlank. Games that use the BIOS sound driver call it
explicitly; games that roll their own M4A typically inline equivalent
code into their own IRQ handler (toggle DMAxCNT_H enable to force a
re-latch).

Pokémon Emerald does **neither** visibly in our emulator — our SWI
trace caught zero sound SWIs and our DMA-register-write trace caught
zero control/SAD writes after boot. The game's in-ROM IRQ handler at
IWRAM `0x03002750` evidently contains the equivalent code but we
never observe it running the DMA-write section. Possibilities:
- A CPU bug we haven't found causes the handler to early-return before
  the DMA-rewrite code.
- The game uses an indirect path (function pointer table, computed
  jump) that skips the DMA-rewrite branch in our emulator state.
- There's a pre-VBlank check we're failing that causes M4A to
  short-circuit.

What we know empirically: forcing the re-anchor at VBlank (matching
what SWI 0x1D does) makes audio work. This is the right shape of
fix; identifying *why* Pokémon's own code doesn't drive it is a
follow-up.

## Fix (behavioural)

In `gba-core/src/lib.rs::handle_event`, on each VBlank entry (scanline
160) after running VBlank-triggered DMA, force-reset `internal_sad`
back to `sad` for any DMA1/DMA2 channel that is active in Special
(FIFO) timing:

```rust
for ch in [1usize, 2] {
    let c = &mut self.bus.dma.channels[ch];
    if c.active && matches!(c.timing(), dma::DmaTiming::Special) {
        c.internal_sad = c.sad & 0x07FF_FFFF;
    }
}
```

This is empirically motivated — we don't fully understand *why* real
hardware effectively does this, but without it, M4A-based games write
samples to a buffer that DMA has already moved past.

## Verification

- Running Pokémon Emerald in the frontend plays actual music (title
  screen, in-game BGM) instead of the previous clicky noise.
- Minor background noise remains (user reports "just very minor
  background noise"), likely due to amplitude scaling or oversampling
  artefacts; much smaller than the original fault.
- All 87 `cargo test` unit tests still pass.
- arm.gba / thumb.gba / memory.gba still "All tests passed"; bios.gba
  still fails test 001 (unrelated HLE stale-bus quirk).

## Follow-ups

- [ ] Figure out why Pokémon's own IRQ handler isn't driving the DMA
  reset in our emulator. GBATEK documents SWI 0x1D as the canonical
  way ("resets the sound DMA… call immediately after V-Blank"), and
  Pokémon's M4A build inlines equivalent code — but our DMA-write
  trace shows the inlined code never runs. Probably a CPU/memory
  accuracy bug along a specific path our test ROMs don't exercise.
- [ ] Investigate remaining "minor background noise" — likely mixer
  scaling or oversampling-related, not CPU.
- [x] Confirm this fix doesn't break other games (e.g. ones that DO
  explicitly manage DMA source across buffers and might not want our
  re-anchor). **It did.** The re-anchor's recency gate was hardcoded at
  2 frames — narrower than the 7-frame M4A `pcmDmaPeriod` that HoD and
  Emerald use — so it rewound their self-managed buffers mid-playback and
  smeared SFX transients. Fixed 2026-07-01 by widening the gate to 8 frames.
  See `2026-07-01_hod-emerald-m4a-sfx-smearing.md`.
