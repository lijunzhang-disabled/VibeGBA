# Followups & Open Items

Tracking known issues, accuracy gaps, and "we'll get to it later" items
across the emulator. Per-bug investigation logs go in dated files; this is
the rolling list of things still worth doing.

Last reviewed: **2026-04-27** (after first round of Pokémon save flakiness diagnosis).

## ⭐ NEXT SESSION: Pokémon Emerald save/play flakiness

**Pick this up first.** Pokémon Emerald **sometimes** works correctly
(boots, plays, in-game save round-trips, "Continue" works on reload) and
**sometimes** misbehaves (mid-save hang, or soft-reset back to title
screen during normal play). Same code, no apparent input difference,
flaky outcome.

### What we know
- The 8-bit-bus broadcast read fix (commit 64b04c0, 2026-04-26) is
  *necessary* but not sufficient. Pokémon's read-back checksum needs
  it.
- We have one confirmed-good round-trip (.sav had 14/14 valid sectors,
  reload showed Continue, game resumed correctly).
- We've also seen: mid-save hang (CPU stops touching flash but no PC
  escape — stuck in valid code waiting for something), and full
  soft-reset to title during normal play before save was even
  attempted.
- The first-save-hang trace earlier showed Pokémon partway through
  ~37k byte writes (out of ~57k expected) when activity stopped.
- BANK_TRACE confirmed `switch_mode` is not the bug — banked.sp[Irq]
  cycles cleanly between post-push and post-pop values every IRQ.

### Suspected causes (in rough order of likelihood)
1. **Cycle-timing inaccuracy.** We don't model wait states (item #4
   below). Pokémon's flash-write polling and vblank handler interact
   on cycle boundaries. Slight inaccuracy in our cycle counts could
   cause vblank IRQ to fire at "wrong" moment relative to flash
   command sequence in some runs but not others.
2. **RTC inconsistency.** We added Seiko S-3511 RTC emulation. If
   reads of the RTC return slightly different values across runs (or
   inconsistent within one run), Pokémon may soft-reset thinking the
   cart RTC is malfunctioning. Worth double-checking the RTC state
   transitions.
3. **An undiagnosed CPU instruction edge case** that's only hit by a
   specific Pokémon code path activated occasionally. Less likely
   given our test ROMs all pass — but possible.

### Where to start when picking this back up
- Run `/tmp/save_test.sh` 5–10 times and tabulate {PASS / PARTIAL /
  BAD / EMPTY / pre-save-soft-reset}. Establish failure rate first.
- If failure rate is high, capture FLASH_TRACE + INSTR_TRACE_RING +
  IRQ_TRACE for one failing and one passing run. Diff them looking
  for divergence point.
- If failure rate is low (<20%), suspect timing — try running with a
  fixed CPU step count instead of audio-synced (in `main.rs`) to see
  if pacing affects flake rate. If yes → cycle accuracy.
- The existing diagnostic infrastructure is all still in
  (`FLASH_TRACE`, `INSTR_TRACE_RING`, `IRQ_TRACE`, `DUMP_PC`,
  `BANK_TRACE`) — just env-set and run.
- Working `.sav` file from the successful round-trip is preserved at
  `~/Documents/PokemonEmeraldVersion.sav.bak-1` (or further back),
  thanks to the rotating backup. Recoverable if needed.

### Note: save state (`]`/`[`) works fine
Whatever the flaky issue is, it's specific to the in-ROM save path,
not our state snapshot/restore.

---

## High priority — known correctness bugs

### 1. Pokémon Emerald: residual minor audio noise
Status: open  
The DMA re-anchor fix (2026-04-25) made Pokémon play actual music, but
the user reports "very minor background noise" still present. Likely
candidates:
- Mixer amplitude scaling (we multiply averaged samples by 120 — could be
  miscalibrated).
- Oversampling box-car artifact (boxcar averaging 349-sample windows
  introduces some aliasing).
- DC offset / SOUNDBIAS handling.

Probably small, but want to chase with a reference recording from mGBA or
a real cartridge for direct comparison.

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
