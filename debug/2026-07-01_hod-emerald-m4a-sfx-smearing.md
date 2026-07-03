# HoD / Emerald M4A DirectSound SFX smearing — VBlank re-anchor gate too tight

Date: 2026-07-01
Status: **Fixed** (commit `99a5c5f`)

## Symptom

In Castlevania: Harmony of Dissonance (HoD), the jump sound effect — a short
percussive "shot" at the start of a jump — was almost inaudible during play.
The player heard the background heartbeat/ambience fine, but the jump SFX
attack was smeared into near-silence: "the jump sound is noise out during
play", "there is almost no jump shot". In an mGBA capture of the same input
the jump "shot" is clear and sharp.

Pokémon Emerald was not obviously broken, but its music also improved audibly
once the fix was in ("emerald sounds better").

This is the counterpart bug that the [2026-04-25 re-anchor
doc](2026-04-25_pokemon-audio-dma-reanchor.md) *predicted* in its Follow-ups:
> Confirm this fix doesn't break other games (e.g. ones that DO explicitly
> manage DMA source across buffers and might not want our re-anchor).

That is exactly what happened. HoD manages its own DMA source across a
multi-frame buffer, and our re-anchor was fighting it.

## How it was found

**Methodology note (important).** For several prior sessions this bug had been
mis-diagnosed and marked "unfixable boot-cadence phase" because I kept
substituting spectral / RMS / cross-correlation *aggregate* metrics for the
one oracle that actually matters here: the user's ears. Those aggregates
averaged over the whole capture and were dominated by the steady background
heartbeat, so they showed "close enough" while the transient SFX was gutted.

The breakthrough was switching to **ear-in-the-loop** with a *per-frame*
diagnostic aimed at the transient, not the whole clip:

- `../emu-agent/compare_pcm.py` — peeks the M4A-rendered PCM buffer in RAM
  (`0x03005420`) on both our core and the mGBA oracle. This proved **our M4A
  software mixer renders the jump correctly** — the samples are in the buffer.
  So the bug was downstream of mixing: in how DMA delivered that buffer to the
  FIFO.
- `../emu-agent/jump_output_probe.py` — the key diagnostic. It measures
  **per-frame OUTPUT RMS across the jump window** (final speaker output, not
  the RAM buffer). At the attack frame (~f517) it showed:

  | Core / config | attack-frame output RMS |
  |---|---|
  | mGBA (oracle) | 6170 |
  | ours, `REANCHOR_FRAMES=2` (old) | **3461** (smeared) |
  | ours, `REANCHOR_FRAMES=8` (fix) | 6451 (matches mGBA) |

- `gba-core/examples/dma1_latch_probe.rs` — prints, per frame, the DMA1
  re-latch cadence and how far `internal_sad` advanced. This is what pinned
  the root cause: it proved **HoD re-latches DMA1 every 7.0 frames**, and
  (surprise) **Emerald re-latches every 7.0 frames too** — not "never", as an
  earlier note had claimed.

## Investigation

Chain of reasoning:

1. **Is the SFX rendered at all?** `compare_pcm.py` says yes — the M4A mixer
   writes the jump into the PCM buffer, byte-for-byte comparable to mGBA. So
   the CPU / sound engine is fine. The loss is between "buffer in RAM" and
   "FIFO output".
2. **Where does buffer→FIFO delivery differ from mGBA?** The only thing our
   core does to that delivery path that hardware/mGBA don't is the VBlank
   **FIFO-DMA re-anchor** (`internal_sad = sad` each VBlank; see
   [[fifo-dma-vblank]]). That reset was added for Pokémon and gated to skip
   channels "recently latched" — with the window hardcoded at **2 frames**
   (`RECENT_LATCH_CYCLES = 2 * CYCLES_PER_FRAME`).
3. **What is HoD's re-latch period?** `dma1_latch_probe.rs`: HoD uses M4A's
   `pcmDmaPeriod = 7`, i.e. a **7-frame** PCM double-buffer, and re-latches
   DMA1 on that 7-frame cadence. So on any given VBlank, HoD's last self-latch
   is frequently **3–6 frames old** — *outside* the 2-frame gate.
4. **Consequence.** On the 4 of every 7 frames where HoD's last latch is older
   than 2 frames, our re-anchor fired and rewound `internal_sad` back to the
   buffer start *mid-playback*. That rewind re-reads already-played samples and
   delays/overwrites the freshly-written attack samples before the FIFO reaches
   them — smearing the transient into the noise floor. The steady heartbeat
   (long, self-similar) survived; the short jump attack did not.
5. **Confirm with the output probe.** Widening the gate so HoD's own 7-frame
   re-latch always falls inside the window (no spurious re-anchor) restores the
   attack-frame output RMS from 3461 → 6451, matching mGBA's 6170.

## Root cause

The re-anchor's "recently latched" gate window (**2 frames**) was **narrower
than the M4A `pcmDmaPeriod` re-latch period (7 frames)** used by real games.

The gate's whole purpose is to leave a channel alone when the *game* is
already driving its DMA source. But the threshold was tuned only against
Pokémon (which never re-latches → always outside any window → always
re-anchored, correctly) and velipso's `rates.gba` (~1.1-frame period). It was
never checked against the common M4A double-buffer cadence of 7 frames. Any
game using `pcmDmaPeriod > 2` — which includes HoD *and* Emerald — was being
re-anchored on the frames between its own re-latches, corrupting delivery of
whatever it had most recently written (most audibly, short SFX transients).

In short: we were resetting a read pointer that the game was actively managing.

## Fix

Widen the gate from 2 frames to **8 frames**, and make it env-tunable for A/B
testing. `gba-core/src/lib.rs`, VBlank (`line == VISIBLE_LINES`) arm of the
`HBlankEnd` handler:

```rust
// Env-tunable (REANCHOR_FRAMES) for A/B. Default 8 frames.
static REANCHOR_FRAMES: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
let gate_frames = *REANCHOR_FRAMES.get_or_init(|| {
    std::env::var("REANCHOR_FRAMES").ok()
        .and_then(|v| v.parse().ok()).unwrap_or(8)
});
let recent_latch_cycles: u64 = gate_frames * CYCLES_PER_FRAME as u64;
let now = self.scheduler.timestamp();
for ch in [1usize, 2] {
    let c = &mut self.bus.dma.channels[ch];
    if !(c.active && matches!(c.timing(), dma::DmaTiming::Special)) { continue; }
    if now.saturating_sub(c.last_latch_cycle) <= recent_latch_cycles { continue; }
    c.internal_sad = c.sad & 0x07FF_FFFF;
}
```

(was `const RECENT_LATCH_CYCLES: u64 = 2 * CYCLES_PER_FRAME`.)

**Why 8 and not just >7:** the gate must comfortably exceed the largest common
`pcmDmaPeriod` (7) so a game's own re-latch always lands inside the window,
with a frame of margin for jitter. Games that genuinely *never* re-latch
(Pokémon's classic path where `last_latch_cycle` is set once at boot) are
still re-anchored — their last latch is thousands of frames old, far past even
an 8-frame window — so the original Pokémon fix is preserved.

### Also in the same commit (gated OFF, no runtime effect)

These are groundwork from the parallel alyosha fifo_dma timing investigation;
none of them change behaviour unless their env var is set:

- **`DMA_STEAL`** — FIFO-DMA cycle-stealing. `Bus::fifo_dma_cost` mirrors
  mGBA's per-unit cost model (`unit1 + 3*unitn + 2`, wait tables from WAITCNT),
  re-ticked into timers / scheduler / APU via a `tick_timers` cascade loop.
  Does **not** pass the alyosha fifo_dma tests (they need sub-instruction
  accuracy beyond what mGBA achieves — mGBA fails them too). Default off; the
  legacy flat 4-cycle FIFO cost is preserved exactly when off.
- **`FIFO_MASTER_GATE`** — discard FIFO register writes while the sound master
  enable is off.
- **`dma1_latch_probe.rs`** — the DMA re-latch cadence diagnostic used above.

## Regression tests

No new unit test — the failure is an audible transient smear that our current
harness can't assert on without an mGBA output oracle wired into CI (the
`jump_output_probe.py` comparison lives in `../emu-agent`, not in-tree). What
we did verify:

- `cargo test` — 91 unit tests pass.
- jsmolka `arm.gba` / `thumb.gba` / `memory.gba` — still "All tests passed".

Candidate future guard: fold `jump_output_probe.py`'s per-frame attack-RMS-vs-
mGBA check into an emu-agent regression so this can't silently regress. Tracked
loosely under followups "Test more commercial games".

## Verification

- **User-confirmed by ear** (the ground-truth oracle for this bug): HoD's jump
  "shot" is crisp again; Emerald's music sounds better at the wider gate.
- Per-frame output probe: attack-frame RMS 3461 → 6451, matching mGBA's 6170
  (the old 2-frame value was ~44% low; the fix is within ~5% of mGBA).
- `dma1_latch_probe.rs` confirms both HoD and Emerald re-latch every 7.0
  frames, so both now sit safely inside the 8-frame gate and are no longer
  spuriously re-anchored.

## Lessons

1. **Ears are the oracle for audio bugs.** Aggregate RMS / spectral / xcorr
   metrics over a whole clip average away short transients and can read "fine"
   while an SFX attack is destroyed. Diagnose the *specific* audible event with
   a *per-frame* probe aimed at that event, and close the loop on the human
   listener. The prior "unfixable boot-cadence phase" conclusion was wrong and
   had misled the investigation for several sessions.
2. **A behavioural hack tuned to one game must be checked against the class of
   games it touches.** The re-anchor was tuned to Pokémon and velipso, never to
   the common M4A `pcmDmaPeriod = 7` cadence. The 2026-04-25 doc even flagged
   this risk in its follow-ups. When a fix pokes at state a game might manage
   itself, enumerate the ways real games manage it (here: M4A double-buffer
   periods of 2–8 frames) before picking a threshold.
3. **Verify "never re-latches" claims.** An earlier note asserted Emerald never
   re-latches; the probe showed it re-latches every 7 frames like HoD. Nearly
   shipped an Emerald regression on that bad assumption; a deterministic A/B
   (`REANCHOR_FRAMES=2` vs `=8`) caught it.

## Related

- [2026-04-25_pokemon-audio-dma-reanchor.md](2026-04-25_pokemon-audio-dma-reanchor.md)
  — introduced the re-anchor; its Follow-up #3 predicted this exact regression.
- [concepts/fifo-dma-vblank.md](concepts/fifo-dma-vblank.md) — full model of
  FIFO DMA and the re-anchor gate (updated for the 8-frame window).
- The alyosha fifo_dma timing investigation — source of the gated `DMA_STEAL` /
  `FIFO_MASTER_GATE` groundwork shipped alongside (memory: `project_alyosha_fifo_dma`).
