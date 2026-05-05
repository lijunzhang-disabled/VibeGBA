# Followups & Open Items

Tracking known issues, accuracy gaps, and "we'll get to it later" items
across the emulator. Per-bug investigation logs go in dated files; this is
the rolling list of things still worth doing.

Last reviewed: **2026-05-05** (after SRTOG buzz fix; residual noise still open).

## ✅ RESOLVED: Pokémon Emerald save flakiness (commit b29226f)

Root cause: in `cpu::step()`, the IRQ check ran **before** the pipeline
refill. After a branch (or any PC-writing instruction),
`pipeline_flushed = true` and `regs[15]` held the raw branch target —
not the +4/+8 pipeline-ahead value. `handle_interrupt()` reads
`regs[15]` to compute the saved `LR_irq`, so it stored a value four
(THUMB) or eight (ARM) bytes too low. The BIOS stub's
`SUBS PC, LR, #4` then resumed mid-instruction at `target-4` /
`target-8`, landing in garbage that often happened to decode as
`BX R1` into an unmapped flash address.

Why flaky: only fires when an IRQ lands in the narrow window between a
branch and its first post-branch instruction. With ~60 Hz vblank IRQs
and a thousands-of-branches save loop, collision rate ended up around
~60% (2/5 PASS pre-fix → 5/5 PASS post-fix).

Diagnostic helper kept in tree: `EXPERIMENT_GATE=1` blocks IRQ delivery
while flash is mid-save. That gate is what proved the "IRQs during
flash" hypothesis (5/5 with gate vs 2/5 without). Zero runtime impact
unless the env var is set; useful if anything similar resurfaces.

The 8-bit-bus broadcast read fix (commit 64b04c0) was necessary but
not sufficient — both fixes are needed for round-trip saves to work.

---

## High priority — known correctness bugs

### 0. Super Robot Taisen: Original Generation — visuals fixed; audio NOT playable
Status: visual path fixed (chip ID, see
`debug/2026-04-30_srtog-flash-chip-id.md`). Game boots past chip-ID
gate, music plays. **Audio is still not playable** — the "zizizi"
comb-tone changed character (no longer the tight 1880 Hz cluster
after commit af3b9ba added halt-period APU ticking) but it was
replaced by a constant-amplitude background noise loud enough that
the user can barely hear the music.

Spectrum signature: top peak now at 59.3 Hz, suspiciously close to
GBA frame rate (59.737 Hz). RMS-over-time on SRTOG never drops to
silence (min RMS 826) while Pokémon does (min RMS 0) — so it's an
always-present output, not music.

Pokémon (the only confirmed-clean game) is unaffected by the halt-
period APU change because Pokémon busy-waits and never halts. The
SRTOG noise floor only appears in halt-using games.

Ruled out via A/B testing:
- APU fast-path bulk-accumulate vs per-cycle: APU_NO_FAST_PATH=1
  sounds the same noisy.
- Mixer scale 120× vs 100×: same sound.
- 1-stage vs 3-stage IIR: 3-stage muffles music without removing
  noise.
- Reverting halt-period APU ticking entirely restores the original
  "zizizi" buzz — so we have two bad sounds and no clean one.

Suspects worth investigating next:
1. **DMA re-anchor at vblank.** We set `internal_sad = sad` for
   DMA1/2 every vblank. If the buffer length doesn't match one
   frame's FIFO consumption (350 samples at 21024 Hz), we skip or
   over-read at each vblank — producing a 60 Hz periodic glitch
   matching our observed 59.3 Hz peak. Diagnostic: log per-vblank
   how many bytes DMA actually transferred between re-anchors;
   compare to expected ~350.
2. **SOUNDBIAS not applied at output.** We read into `bias_level`
   but never subtract from output. Could be a constant DC component
   or low-frequency rumble.
3. **Halt-period sub-step phasing.** Our chunk-to-next-overflow
   sub-stepping is correct in principle but might be sensitive to
   off-by-one on overflow timing. Worth comparing the FIFO pop
   cycle counts vs slow path.
4. **Sample-and-hold spectral images.** FIFO at 21024 Hz creates
   spectral images at 20–22 kHz. A proper sinc-interpolation between
   FIFO samples would suppress these.

Reproduction: `WAV_DUMP=/tmp/srtog.wav ./target/release/gba-frontend
~/Documents/SuperRobotTaisen-OriginalGeneration.gba`, then
`/tmp/wav_analyze.py /tmp/srtog.wav` — top peak should be at ~59 Hz
with min RMS > 800 (constant background).

### 1. 8-bit Flash region: 16-bit / 32-bit writes
Status: known-incomplete  
`bus::write16` / `write32` for `0x0E…/0x0F…` currently fall through to
no-op. Per gbatek, real hardware should write the LSB byte. We tried
adding that and it broke Pokémon save (made it hang again — different
code path).
Next time: identify *which* code path Pokémon takes that depends on the
write being dropped, OR figure out what the hang's root cause is.
Reads are correct (8-bit broadcast, fixed in 64b04c0).

### 2. `bios.gba` test 001 — BIOS stale-bus latch
Status: known-failing  
Test reads from address 0 outside BIOS context and expects the last
BIOS-fetched instruction (`0xE129F000`) to be returned (open-bus latch).
Our HLE BIOS doesn't naturally produce this latched value. Narrow fix:
hardcode the canonical latch value when `read32(0x0)` is called outside
BIOS execution. Doesn't affect any real game.

## Medium priority — accuracy gaps

### 3. Wait state emulation (`WAITCNT`)
Status: not modelled  
Every memory access takes 1 cycle in our emulator. Real GBA has variable
wait states for ROM (configurable via WAITCNT 0x04000204), EWRAM (3
cycles per 16-bit access), Flash, etc. Net effect: our CPU is "too fast"
by ~10-30% depending on workload.
Doesn't break anything observable so far, but cycle-tight games
(precision platformers, anything polling I/O in tight inner loops) could
expose timing bugs.

### 4. Game Pak prefetch buffer
Status: not modelled  
The GBA Game Pak has a small instruction prefetch buffer that hides ROM
wait states for sequential code execution. Without modelling it, ROM
fetch is artificially slow — but since item 3 (WAITCNT) is also
missing, the two inaccuracies partially cancel.

### 5. Open-bus read accuracy
Status: simplified  
Currently we return `last_read` for unmapped memory reads. Real
behaviour depends on what was last fetched (CPU pipeline state, last
DMA, etc.). Edge cases likely diverge from real hardware.

### 6. Misaligned ARM access quirks
Status: partial  
We fixed misaligned `LDRH` rotation (`addr & 1` rotates result right by
8). ARM has similar quirks for:
- Misaligned `LDR` (rotates result right by `(addr & 3) * 8`).
- `LDRSH` at odd address: behaves like `LDRSB` (sign-extends a byte).
We have these in our ARM halfword code but they're worth a focused
review against the ARM ARM spec.

## Low priority — wide-coverage / nice-to-have

### 7. Missing BIOS SWIs
Status: ~22 of ~40 SWIs implemented (see header comment in
`gba-core/src/bios.rs`).  
Notable missing ones:
- `0x19 SoundBias` (audio gain ramping)
- `0x1A SoundDriverInit`, `0x1B SoundDriverMode`,
  `0x1C SoundDriverMain` (we have 0x1D `SoundDriverVSync` already)
- `0x1E SoundChannelClear`, `0x1F MidiKey2Freq`
- `0x20–0x24 MusicPlayer*` (alternate music engine some games use)
- `0x25 MultiBoot`
- `0x26–0x2A` (mostly undocumented)

Most modern Game Boy Advance commercial games ship their own audio
driver and don't depend on these. But some older / simpler games rely on
the BIOS audio path.

### 8. Test more commercial games
Status: only Pokémon Emerald + jsmolka test ROMs exercised.  
Bring up at least a handful of representatives:
- Pokémon Ruby/Sapphire (same M4A engine, should work)
- Zelda: The Minish Cap (different rendering tricks)
- Castlevania: Aria of Sorrow / Dawn of Sorrow (effects-heavy)
- Super Mario Advance series
- Metroid Fusion / Zero Mission
- Final Fantasy I/II/IV/V/VI advance ports

Each will probably surface 1–3 new accuracy bugs. The way to bring this
up is "boot it, take screenshots at known frames, eyeball or diff
against mGBA captures."

### 9. Audio: real-BIOS sound SWI testing
Status: unverified  
Once we land more sound SWIs (item 8), validate against games that
specifically use them. Note: HLE will never be perfect for games that
expect cycle-exact BIOS sound timing — those should use a real BIOS dump.

## Tooling / UX

### 10. Save state vs `.sav` interaction
Status: works but confusing.  
Currently:
- `.sav` is rewritten on emulator exit from current Flash state.
- Save state (`]`) snapshots Flash too; load state (`[`) restores it.
- If user mixes save state and in-game save, the load-state will
  silently revert in-game save progress.

Possible improvements:
- Track a "Flash modified since last save state" flag and warn on
  conflicting actions.
- Or just document this behaviour clearly in a README.

### 11. Diagnostic instrumentation cleanup
Status: env-gated, not affecting runtime.  
Several debug prints are in the codebase, all gated by env vars
(`FLASH_TRACE`, `IRQ_TRACE`, `INSTR_TRACE_RING`, `DUMP_PC`). They're
useful for next-time debugging but add a little code volume. Periodic
review: either keep, gate behind a `cfg(debug_assertions)` instead of
env, or move to a separate debug crate.

## Frontend

### 12. Better save state UX
Status: bare-bones (`]` save, `[` load).  
Could add:
- Multiple save state slots (`Shift+1`–`9`).
- Visual confirmation when state is saved/loaded.
- Auto-save state on exit.

### 13. Configurable controls
Status: hardcoded.  
Mapping is fixed in `gba-frontend/src/input.rs`. A config file or
runtime arguments would make it easier for users with different
keyboard layouts.

---

## How to use this file

When you fix something here, delete the entry and (if substantial) link
to the fix commit / debug doc. When something new turns up that doesn't
warrant its own bug doc yet, add a one-paragraph entry here.

Don't let it get longer than ~1 screen — if too many items pile up,
they're not really getting picked up; demote to backlog or close as
won't-fix.
