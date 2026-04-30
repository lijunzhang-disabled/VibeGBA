# Followups & Open Items

Tracking known issues, accuracy gaps, and "we'll get to it later" items
across the emulator. Per-bug investigation logs go in dated files; this is
the rolling list of things still worth doing.

Last reviewed: **2026-04-29** (after fixing Pokémon save flakiness).

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

### 0. Super Robot Taisen: Original Generation — visuals fixed; audio buzzy
Status: visual path resolved 2026-04-30 (chip ID). Audio sounds buzzy
("zizizi") with a slow amplitude wobble — same class of issue as
Pokémon's residual noise (item #1 below).

What's working: game boots past chip-ID gate, title screen renders
(`dispcnt=0x0300`), state machine progresses through 0x32 → 0x3C →
0x46 → 0x64 → … (verified via MEM_WATCH). State transitions queue
correctly and complete.

Visual fix: see `debug/2026-04-30_srtog-flash-chip-id.md`. Our
hardcoded 64 KB Atmel chip ID wasn't in SRTOG's supported-chip
table, so the game treated save hardware as broken and parked the
state machine on its 0xFFFF sentinel. `pick_chip_id()` now scans the
ROM for any known chip-ID and returns the first match (Panasonic >
SST > Macronix > Sanyo > Atmel for 64 KB; Macronix variants > Sanyo
for 128 KB). Pokémon (128 KB) and SRTOG (64 KB) both boot correctly.

Audio: see item #1.

### 1. Audio mixer: residual buzz across multiple games
Status: open. Confirmed on Pokémon Emerald (subtle background noise)
and Super Robot Taisen: OG (more pronounced "zizizi" with amplitude
wobble). Both games use M4A engine directly — not BIOS sound driver.

Suspected causes (in rough order of likelihood):
1. **Mixer clipping.** `apu/mod.rs::emit_sample` scales averaged
   samples by 120×, then clamps to ±32767. Theoretical peak signal
   (4×PSG_max + 2×FIFO_max) × 120 ≈ ±37 920 — clamps every loud
   peak, and clipped square-wave edges produce a "zizizi" buzz.
   Fix candidate: reduce scale to ~60-80×, or apply a true soft
   clip rather than hard clamp.
2. **PSG output range doubled.** `psg.rs` outputs ±envelope_volume
   for high/low duty (range −15..+15), but real hardware outputs
   0..15 unsigned (range −7..+8 after DC removal). Our PSG is
   roughly 2× too loud, contributing to the clipping above.
3. **SOUNDCNT_H DSA/DSB volume bits.** Need to verify we apply the
   50% / 100% direct-sound volume scaling correctly.
4. **Boxcar averaging artifact.** 349-sample boxcar at 48 kHz output;
   linear-phase but with poor stop-band attenuation. Could replace
   with a multi-tap FIR or a 2-3 stage IIR.

Right way to chase: capture a reference recording from mGBA on the
same ROMs at known frames, A/B against ours, and tune the mixer
scale + PSG range together until peaks line up without clipping.

### 2. 8-bit Flash region: 16-bit / 32-bit writes
Status: known-incomplete  
`bus::write16` / `write32` for `0x0E…/0x0F…` currently fall through to
no-op. Per gbatek, real hardware should write the LSB byte. We tried
adding that and it broke Pokémon save (made it hang again — different
code path).
Next time: identify *which* code path Pokémon takes that depends on the
write being dropped, OR figure out what the hang's root cause is.
Reads are correct (8-bit broadcast, fixed in 64b04c0).

### 3. `bios.gba` test 001 — BIOS stale-bus latch
Status: known-failing  
Test reads from address 0 outside BIOS context and expects the last
BIOS-fetched instruction (`0xE129F000`) to be returned (open-bus latch).
Our HLE BIOS doesn't naturally produce this latched value. Narrow fix:
hardcode the canonical latch value when `read32(0x0)` is called outside
BIOS execution. Doesn't affect any real game.

## Medium priority — accuracy gaps

### 4. Wait state emulation (`WAITCNT`)
Status: not modelled  
Every memory access takes 1 cycle in our emulator. Real GBA has variable
wait states for ROM (configurable via WAITCNT 0x04000204), EWRAM (3
cycles per 16-bit access), Flash, etc. Net effect: our CPU is "too fast"
by ~10-30% depending on workload.
Doesn't break anything observable so far, but cycle-tight games
(precision platformers, anything polling I/O in tight inner loops) could
expose timing bugs.

### 5. Game Pak prefetch buffer
Status: not modelled  
The GBA Game Pak has a small instruction prefetch buffer that hides ROM
wait states for sequential code execution. Without modelling it, ROM
fetch is artificially slow — but since (4) is also missing, the two
inaccuracies partially cancel.

### 6. Open-bus read accuracy
Status: simplified  
Currently we return `last_read` for unmapped memory reads. Real
behaviour depends on what was last fetched (CPU pipeline state, last
DMA, etc.). Edge cases likely diverge from real hardware.

### 7. Misaligned ARM access quirks
Status: partial  
We fixed misaligned `LDRH` rotation (`addr & 1` rotates result right by
8). ARM has similar quirks for:
- Misaligned `LDR` (rotates result right by `(addr & 3) * 8`).
- `LDRSH` at odd address: behaves like `LDRSB` (sign-extends a byte).
We have these in our ARM halfword code but they're worth a focused
review against the ARM ARM spec.

## Low priority — wide-coverage / nice-to-have

### 8. Missing BIOS SWIs
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

### 9. Test more commercial games
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

### 10. Audio: real-BIOS sound SWI testing
Status: unverified  
Once we land more sound SWIs (item 8), validate against games that
specifically use them. Note: HLE will never be perfect for games that
expect cycle-exact BIOS sound timing — those should use a real BIOS dump.

## Tooling / UX

### 11. Save state vs `.sav` interaction
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

### 12. Diagnostic instrumentation cleanup
Status: env-gated, not affecting runtime.  
Several debug prints are in the codebase, all gated by env vars
(`FLASH_TRACE`, `IRQ_TRACE`, `INSTR_TRACE_RING`, `DUMP_PC`). They're
useful for next-time debugging but add a little code volume. Periodic
review: either keep, gate behind a `cfg(debug_assertions)` instead of
env, or move to a separate debug crate.

## Frontend

### 13. Better save state UX
Status: bare-bones (`]` save, `[` load).  
Could add:
- Multiple save state slots (`Shift+1`–`9`).
- Visual confirmation when state is saved/loaded.
- Auto-save state on exit.

### 14. Configurable controls
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
