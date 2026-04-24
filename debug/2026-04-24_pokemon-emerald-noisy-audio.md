# Pokémon Emerald Noisy Audio — Sound Engine Producing Garbage

**Date investigated:** 2026-04-23 through 2026-04-24
**Phase context:** Phase 9 (accuracy polish)
**Status:** Open — root cause identified, fix deferred

## Symptom

Pokémon Emerald audio sounds noisy from the emulator. Specifically:
- "Completely random, no melody at all"
- Amplitude fluctuates "up and down" with an approximate period of ~3 seconds
- No recognizable music in any game screen (boot, title, name selection)
- Game otherwise works — display renders, input responds, menus navigable

## How it was found

User reported after the initial audio implementation (Phase 6) that Pokémon Emerald sound was "weird, almost like noise." Assumed to be filtering issue but persistent noise even after significant APU improvements led to deeper diagnosis.

## Investigation timeline

### Day 1 (2026-04-23): Audio path investigation

**Initial hypothesis:** Our audio is aliased / unfiltered / quiet.

**Changes applied:**
1. Added 2-stage IIR low-pass filter (α=0.5 per stage)
2. Increased gain from 64× → 128×
3. Fixed FIFO 32-bit write zero-stuffing bug (every other pair of samples was 0s)
4. Fixed PSG double-tick bug (channels advanced 2× per sample cycle)
5. Upgraded to 4-stage LPF

**Statistical improvements:**
- Big jumps (>4000): 1352 → 1
- Max sample jump: 15232 → 4176
- Zero-crossings: 1230 → 1083 Hz

**User verdict:** Still noisy.

### Day 1 (cont.): Reverted, confirmed baseline

User asked "are we going in a wrong direction?" Reverted all audio changes to yesterday's state. User confirmed "sounds same as today" — the noise had always been there since Phase 6, just not noticed yesterday.

### Day 2 (2026-04-24): Proper APU rewrite

**New hypothesis:** Poor sample rate matching between emulator (32768 Hz) and macOS native (48000 Hz) causes SDL2 to resample with low quality.

**Major rewrite:**
1. Changed output to 48000 Hz native (matches macOS Core Audio → no SDL2 resampling)
2. Added per-cycle oversampling: accumulate mixed samples every CPU cycle, emit the average every ~349 cycles
3. 349:1 boxcar averaging acts as a strong anti-aliasing filter
4. 3-stage IIR post-filter for smoothing
5. Pre-filled SDL2 buffer with silence to prevent initial underruns
6. Smart buffer drop-oldest instead of drop-newest

**Verification:**
- SDL2 confirmed obtaining exactly 48000 Hz (no resampling)
- 0 big jumps in output (was 1352)
- 0 clipping
- Stats looked clean

**User verdict:** Still noisy.

### Day 2 (cont.): Audio-synced emulation

**Hypothesis:** Burst production (800 samples every 16 ms) vs continuous consumption creates audible modulation.

**Changes:**
1. Split emulation into 4 sub-frame chunks (pump audio 4× more often)
2. Audio-synced pacing: emulator waits when buffer above 64 ms latency, runs freely below 32 ms
3. SDL2 consumption becomes the master clock

**User verdict:** Still noisy, same pattern.

### Day 2 (cont.): Memory inspection — the key finding

Written [`gba-core/examples/audio_buf_probe.rs`](../gba-core/examples/audio_buf_probe.rs) to scan IWRAM and EWRAM for audio-like data after 5 seconds of emulation.

**Criteria for "audio-like":**
- Small adjacent-byte differences (avg |dx| < 20 for smooth waveforms)
- High unique-value diversity
- Significant non-zero content

**Results:**

```
IWRAM (32 KB):
  74.4% zeros
  Top non-zero 1KB regions:
    0x03001C00: 886 nonzero, avg|dx|=63.3, 155 unique  ← noise-like
    0x03002800: 873 nonzero, avg|dx|=58.0, 159 unique  ← noise-like

EWRAM (256 KB):
  96.9% zeros
  Top non-zero 1KB regions:
    0x0203AC00: 1024 nonzero, avg|dx|=0.0, 1 unique   ← filled constant
    0x0203B000: 1024 nonzero, avg|dx|=0.0, 1 unique   ← filled constant
```

**No audio-like data anywhere in memory.**

Pokémon's M4A sound engine decodes MIDI sequences into PCM samples and places them in IWRAM for FIFO DMA to play. If it was working correctly, we'd expect:
- A clear buffer region of ~2-4 KB with smooth waveform data
- Average adjacent-byte diff under 20 (waveforms have smooth transitions)
- Dozens to low hundreds of unique byte values

Instead, every non-zero region looks like either noise (`avg|dx|=55-63`) or filled constants (`avg|dx|=0`). **No structured audio waveforms exist anywhere in the game's memory.**

## Root cause (identified)

The audio pipeline (DMA → FIFO → mixer → SDL2) is functionally correct. It's faithfully playing whatever bytes exist in memory. The problem is that **Pokémon's sound engine is not writing proper PCM samples** to the audio buffer.

This is a deeper bug — almost certainly in CPU emulation. The M4A sound engine does a lot of arithmetic per note:
- Pitch frequency computation (often via lookup tables)
- ADSR envelope multiplication
- Sample interpolation for pitch-shifted playback
- Accumulating mixer math

If any of these operations are subtly wrong due to a CPU instruction bug, the produced samples look like noise.

## What we ruled out

| Hypothesis | Verdict |
|---|---|
| Output rate mismatch (32768 vs 48000) | Ruled out — SDL2 confirmed 48000 exact |
| Filter too aggressive | Ruled out — tried 0, 1, 2, 3, 4 stages; no improvement |
| Filter too gentle | Ruled out — boxcar averaging already covers most |
| Volume/gain issue | Ruled out — stats show no clipping, reasonable amplitude |
| Buffer underruns | Ruled out — pre-fill + smart drop-oldest implemented |
| Burst production vs continuous consumption | Ruled out — chunked + audio-synced pacing implemented |
| Wrong PSG double-tick | Fixed — not the cause of main noise |
| FIFO zero-stuffing | Fixed — not the cause of main noise |
| DMA not refilling | Ruled out — probe showed DMA source advancing correctly |
| Timer rate wrong | Ruled out — measured timer 0 at exactly 13381 Hz (spec: 13379 Hz) |

## What's still possible (likely causes)

1. **CPU instruction edge case** in multiply/MLA or signed arithmetic that M4A relies on heavily
2. **Missing BIOS SWI**: Pokémon likely uses `MidiKey2Freq` (SWI 0x1F), `SoundBias` (SWI 0x19), or `SoundDriverInit` (SWI 0x1A) — none of which we currently HLE
3. **Interrupt timing** for the sound engine — M4A runs via VBlank IRQ, specific cycle timing matters
4. **Memory access pattern bug** — maybe unaligned loads/stores return wrong values for specific cases used by M4A

## Paths forward

**Option 1: Try a real GBA BIOS dump.** Our HLE BIOS implements 22 SWIs but Pokémon uses several more (especially the sound-related ones at 0x19-0x1F). Loading a real BIOS via `--bios` might immediately fix the issue. Requires obtaining a legal BIOS dump.

**Option 2: Accept audio as broken, move on.** Use `--no-audio`. Document as a known limitation. Focus on visible bugs and gameplay. Revisit later.

**Option 3: Deep CPU emulation bug hunt.** Test with a simpler homebrew ROM that has known-good audio. If audio works there, issue is Pokémon-specific (likely missing BIOS SWI). If audio fails there too, general CPU bug. Either way, expect multiple sessions of work.

## Artifacts

Diagnostic tools kept in `gba-core/examples/`:
- `audio_dump.rs` — write N seconds of emulator audio to `/tmp/gba_audio.wav` with statistics
- `audio_buf_probe.rs` — scan IWRAM/EWRAM for audio-like content
- `dma_probe.rs` — inspect DMA channel state per frame

## Tests added

Regression tests in `gba-core/src/apu/mod.rs`:
- `test_apu_generates_samples_at_48khz` — verifies exact output rate with fractional accumulator
- `test_fifo_timer_overflow`, `test_soundcnt_h_parse`, `test_apu_master_enable` (existing)

## Files changed (kept in this session)

- `gba-core/src/apu/mod.rs` — rewritten with 48 kHz oversampling, audio-synced friendly
- `gba-core/src/apu/fifo.rs` — added `write16` for proper 16-bit FIFO writes
- `gba-core/src/apu/psg.rs` — added `output()` methods (stateless channel read)
- `gba-core/src/lib.rs` — added `run_cycles()` for chunked emulation
- `gba-core/src/bus/mod.rs` — added IWRAM/EWRAM accessors for diagnostics
- `gba-frontend/src/audio.rs` — pre-filled buffer, smart drop, buffer thresholds
- `gba-frontend/src/main.rs` — chunked run loop + audio-synced pacing

## Lessons learned

- **Symptom-driven debugging can mislead.** We spent hours optimizing the audio mixer when the actual bug was upstream. Always verify the INPUT is correct before tuning the OUTPUT.
- **Statistical improvements don't always translate to perceptual improvements.** Reducing "big jumps" from 1352 to 0 felt like progress but didn't address the real issue.
- **User feedback is critical.** The clue that finally broke the investigation was the user describing the noise as "completely random, no melody at all" + "every 3 seconds period." The 3-second period ≈ IWRAM size at FIFO rate pointed directly at the memory inspection.
- **Diagnostic tools > clever fixes.** Writing `audio_buf_probe.rs` (the 3rd diagnostic we built) is what finally revealed the true root cause.
