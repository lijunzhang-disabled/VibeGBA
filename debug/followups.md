# Followups & Open Items

Tracking known issues, accuracy gaps, and "we'll get to it later" items
across the emulator. Per-bug investigation logs go in dated files; this is
the rolling list of things still worth doing.

Last reviewed: **2026-05-30** (synced with FE7 fix, BIOS latch fix, wait-state revert).

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

## ✅ RESOLVED: Fire Emblem 7 — HBlank/audio gate cascade (commit bb4b916)

Root cause: HLE SWI 0x04/0x05 (`IntrWait`/`VBlankIntrWait`) set
`cpu.halted = true` and let *any* pending IRQ wake the CPU. Real BIOS
implements a halt-then-recheck loop that re-halts on non-matching IRQs.
Without the gate, HBlank IRQs (159/frame) woke FE7's `WaitForVBlank`
prematurely → main loop ran 3.5×/frame → M4A audio engine overflowed
its 192-slot channel buffer at IWRAM 0x2930 → corrupted IRQ handler at
0x03003950 → cascade freeze.

Fix (~10 lines): new `cpu.intrwait_mask` field set by SWI 0x04/0x05;
halt-wake check gated so only a matching IRQ clears `halted`.

The wait-state/prefetch-buffer investigation that ran alongside this
(parts 1–12 of the debug doc) turned out to be a dead end and was
reverted in commit `d9b57ea`. See `debug/2026-05-24_fe7-hblank-irq-cascade.md`.

---

## ✅ RESOLVED: `bios.gba` test 001 — BIOS stale-bus latch (commit db9aaf8)

Previously listed as item 3. The HLE BIOS now primes `bios_latch` at
startup (0xE129F000), after SWI (0xE3A02004), and during IRQ
(0xE25EF004). All four jsmolka `bios.gba` latch tests now pass.

---

## High priority — known correctness bugs

### 1. Super Robot Taisen: Original Generation — visuals + most audio fixed; residual
Status: visual path fixed (chip ID, see
`debug/2026-04-30_srtog-flash-chip-id.md`). Major audio bug fixed
2026-05-05 — the constant 59 Hz noise floor that was drowning out
the music came from FIFO_B underflow cross-triggering DMA1 to over-
push FIFO_A; see
`debug/2026-05-05_srtog-fifo-b-cross-trigger.md`. Music is now
audible.

Remaining audio issues (lower priority):
- A periodic component still audible "on top" of normal sound
- Combat scenes have noticeable distortion

Possibly different roots — dual-FIFO content (where SRTOG actually
uses FIFO_B during combat for SFX) might trip a related but not
identical bug. Worth re-running the WAV/spectrum analysis on a
combat-scene capture.

### 2. 8-bit Flash region: 16-bit / 32-bit writes
Status: known-incomplete  
`bus::write16` / `write32` for `0x0E…/0x0F…` currently fall through to
no-op. Per gbatek, real hardware should write the LSB byte. We tried
adding that and it broke Pokémon save (made it hang again — different
code path).
Next time: identify *which* code path Pokémon takes that depends on the
write being dropped, OR figure out what the hang's root cause is.
Reads are correct (8-bit broadcast, fixed in 64b04c0).

## Medium priority — accuracy gaps

### 4. Wait state / prefetch accuracy
Status: **not modelled** (reverted)
The per-region wait-state model and GamePak prefetch buffer added during
the FE7 investigation caused subtle audio-timing regressions in Pokémon
Emerald without fixing the actual bug (which was a missing IntrWait
re-halt, not a timing gap). All that code was **reverted in commit
`d9b57ea`** (2026-05-29). `bus::add_mem_cycles` is currently a no-op
stub; every memory access effectively takes 1 cycle. The API surface is
kept so a future cycle-accurate model can slot in without touching every
call site. Low risk for game compat — most commercial games are
tolerant of timing inaccuracy.

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
Status: ~23 of ~40 SWIs implemented (see header comment in
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
Status: Pokémon Emerald, SRTOG, FE7, and jsmolka test ROMs have had focused attention; broader coverage is still thin.
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
(`FLASH_TRACE`, `IRQ_TRACE`, `INSTR_TRACE_RING`, `DUMP_PC`, `FE7_PROBE`,
`MEM_WATCH`, `DMA_FIRE_TRACE`, `TIMER_TRACE`, etc.). They're useful for
next-time debugging but add code volume. Periodic review: either keep,
gate behind `cfg(debug_assertions)` instead of env, or move to a separate
debug crate.

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
