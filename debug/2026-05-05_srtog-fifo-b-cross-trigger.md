# SRTOG audio: FIFO_B empty cross-triggering DMA1 → over-pushing FIFO_A

Date: 2026-05-05
Status: **Fixed** (commit pending)

## Symptom

After commit af3b9ba (halt-period APU ticking fixed the original
"zizizi" comb-buzz), SRTOG's audio still sounded broken — a constant
loud background noise loud enough to drown out the music. Pokémon
Emerald (the only known-clean reference) was unaffected.

WAV captures and `/tmp/wav_analyze.py` showed:
- SRTOG top peak at 59.3 Hz (suspiciously close to GBA frame rate)
- SRTOG RMS over 100 ms windows never dropped to silence (min 826)
  while Pokémon did (min 0)
- Spectrum density that looked like sidebands around an AM-modulated
  carrier rather than music

## Investigation

The clue came from a per-vblank DMA1 source-pointer trace
(`DMA_AUDIO_TRACE=1`):

```
[DMA1] vbl: sad=0x02024600 internal_sad=0x02025C00 advanced=5632 bytes → re-anchor
```

5632 bytes/frame at 21024 Hz audio rate. Expected: 21024/60 × 4
(bytes per 32-bit DMA transfer split into 4 samples) ÷ 16 (samples
per refill) × 16 (bytes per refill) = **352 bytes/frame**. So we
were transferring **16× more than needed**.

A FIFO push/pop counter showed:
```
[FIFO_A] count=32 push=384 pop=352 refill_req=2 dropped=5264
```
Meaning per-frame ~352 pops (correct), ~352 *useful* pushes (correct
match), plus **5264 dropped pushes per frame** because FIFO_A was
full and `push_byte` was discarding excess samples.

A per-DMA-fire trace narrowed it further:
```
run_dma(1) timing=Special fifo_a_count=31 fifo_b_count=0
                          refill_req_a=2 refill_req_b=8
```
- `fifo_a_count=31` (full minus 1)
- `fifo_b_count=0` (empty)
- `refill_req_a` static, `refill_req_b` growing on every fire

So DMA1 was firing because **FIFO_B requested refill** every Timer 0
overflow (FIFO_B was empty since SRTOG only uses FIFO_A; pop on empty
returns "need refill" because count=0 satisfies `count <= 16`).

## Root cause

`tick_timers` calls `run_dma_for_timing(DmaTiming::Special)` whenever
*any* FIFO requests refill. `run_dma_for_timing` then runs *every*
Special-timed DMA channel — not just the one for the specific FIFO.

Concrete chain:
1. SRTOG configures DMA1 → FIFO_A (Special timing).
2. SRTOG never configures DMA2; FIFO_B starts empty and stays empty.
3. Every Timer 0 overflow, our APU pops both FIFO_A AND FIFO_B
   (because both FIFOs default `timer_select = 0`).
4. FIFO_B's pop returns "need refill" (count=0 ≤ 16).
5. `run_dma_for_timing(Special)` fires all Special-timed channels.
6. Only DMA1 is active, so DMA1 fires — pushes 16 samples to FIFO_A,
   which is already full → 15 of them dropped, 1 accepted.
7. Source pointer advances 16 bytes regardless.
8. Repeat 350× per frame, advancing the M4A buffer pointer way past
   real consumption. Every vblank, the auto-re-anchor pulls it back
   to the buffer start, but during the frame the pointer has read
   past stale buffer locations producing the constant noise.

The FIFO sample-and-hold + the constant 60 Hz periodic re-anchor +
the noise from over-reading buffer all add up to a 59 Hz-dominated
constant noise floor.

## Fix

`tick_timers` no longer calls `run_dma_for_timing(Special)`. Instead
it calls a new `run_dma_for_fifo(addr)` which only fires DMA channels
whose destination address matches the specific FIFO that requested
refill:

```rust
const FIFO_A_ADDR: u32 = 0x0400_00A0;
const FIFO_B_ADDR: u32 = 0x0400_00A4;

if fifo_a_refill { self.run_dma_for_fifo(FIFO_A_ADDR); }
if fifo_b_refill { self.run_dma_for_fifo(FIFO_B_ADDR); }
```

```rust
fn run_dma_for_fifo(&mut self, fifo_addr: u32) {
    for ch_id in 0..4 {
        let c = &self.bus.dma.channels[ch_id];
        if c.enabled() && c.active
            && c.timing() == DmaTiming::Special
            && (c.dad & 0x07FF_FFFF) == (fifo_addr & 0x07FF_FFFF)
        {
            ...
            self.bus.run_dma(ch_id);
        }
    }
}
```

Now FIFO_B's refill request only fires DMAs that write to FIFO_B_L.
SRTOG (no DMA configured for FIFO_B) gets no spurious DMA1 fires.

## Verification

- All 90 unit tests pass.
- SRTOG's constant background noise is gone; the music is now
  audible.
- Pokémon Emerald still sounds correct (the fix is more conservative
  — fires fewer DMAs, only the right ones).
- DMA1 `run_count` for SRTOG drops from ~352/frame to ~22/frame,
  matching expected 21024 Hz / 16 samples-per-refill.

## Diagnostics kept in tree

- `DMA_AUDIO_TRACE=1`: per-vblank DMA + FIFO state dump
- `DMA_FIRE_TRACE=1`: per-fire DMA + FIFO state, plus
  `[from tick_timers/T0]` / `[from CPU write]` call-site labels
- `WAV_DUMP=path.wav` (frontend): capture audio for spectrum analysis
- `TIMER_TRACE=1`: log timer reload + FIFO sample rate
- `/tmp/wav_analyze.py file.wav [...]`: spectrum + RMS analysis script

## Residual issues (followups)

After this fix SRTOG audio is much improved but not perfect:
- A periodic component is still audible on top of the music
- Combat scenes have noticeable distortion

These need further investigation — possibly DMA-buffer-length / vblank
re-anchor mismatch (different from the over-push), or a different
mixer issue specific to dual-FIFO content.
