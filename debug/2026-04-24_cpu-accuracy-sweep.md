# ARM7TDMI accuracy sweep — 2026-04-24

Date: 2026-04-24
Status: **In progress** (Pokémon audio still broken after these fixes)

## Context

Pokémon Emerald produces garbled audio. Audio pipeline (DMA/FIFO/mixer/SDL2)
was verified correct, so the root cause is CPU execution corrupting the M4A
sound engine's state in memory. To find CPU bugs systematically, ran
jsmolka's `arm.gba` / `thumb.gba` / `memory.gba` / `bios.gba` test ROMs.

Baseline results before this sweep:
- `arm.gba` → white screen, CPU escape to unmapped memory
- `thumb.gba` → Failed test 211
- `memory.gba` → All tests passed
- `bios.gba` → Failed test 001

This doc catalogs the five CPU fixes made during the sweep. Each has its
own entry for the specific bug; this is the index and overview.

## Fixes

### 1. Misaligned LDRH rotation (ARM halfword + THUMB format 7/8)

**Where:** `gba-core/src/arm7tdmi/arm.rs` (halfword transfer), `thumb.rs`
(format 7/8 register-offset LDRH).

**What:** ARM7TDMI quirk: when `LDRH` is issued at an odd address, the
hardware reads the aligned halfword and rotates the result right by 8 bits
(exposing the upper byte in the low position). Our emulator was silently
aligning and returning the raw halfword.

**Impact:** thumb.gba test 211 — advances to test 219.

**Not yet fixed:** THUMB format 10 (immediate-offset) LDRH. Applying the
rotation there triggers a downstream CPU escape around instruction 700 in
Pokémon — not yet root-caused. Has a `TODO` comment pointing here.

### 2. MSR register-banking silently skipped *(severe)*

**Where:** `gba-core/src/arm7tdmi/arm.rs::arm_msr`.

**What:** `arm_msr` mutated `self.cpsr.bits` **before** calling
`switch_mode(new_mode)`. `switch_mode` then re-derived its `old_mode` from
the already-updated CPSR, found `old_mode == new_mode`, and took its
early-return path — **skipping all register banking**. Every MSR that
changed CPU mode left R13/R14/R8–R12 in the wrong bank.

**Fix:** compute the new CPSR value and the new mode first, call
`switch_mode` while the old CPSR is still in place, then commit the new
bits.

**Impact:** `arm.gba` stopped escaping to unmapped memory and started
producing real "Failed test NNN" output. This was the most severe bug —
any program that uses MSR to change mode (i.e. essentially every program
with an IRQ handler) was running with corrupted banked registers.

**Detail:** [`2026-04-24_arm-msr-banking.md`](2026-04-24_arm-msr-banking.md)

### 3. PC+12 on shift-by-register for Rn and Rs

**Where:** `gba-core/src/arm7tdmi/arm.rs::arm_data_processing`.

**What:** When a data-processing instruction uses a register to specify
the shift amount (bit 4 = 1), the internal extra cycle advances the
prefetch one more word: **every** PC read inside that instruction — Rn,
Rm, *or* Rs — returns PC+12, not PC+8. Our code handled the Rm=PC case
but not Rn=PC or Rs=PC.

**Impact:** `arm.gba` test 225 (`ADD R0, PC, R0, LSL R0`) now passes —
advances to test 360.

### 4. CMPP / TSTP / TEQP / CMNP (P-variants) must not write PC

**Where:** `gba-core/src/arm7tdmi/arm.rs::arm_data_processing`.

**What:** For `TST`, `TEQ`, `CMP`, `CMN` with `Rd=R15` and `S=1` — the
ARMv2-era "P variants" — the instruction restores CPSR from SPSR but
**does not write the ALU comparison result to PC**. Our code fell into
the generic `Rd=R15 + S` path and branched to the garbage comparison
result (for e.g. `CMP`, `result = Rn - Rm`, which has no reason to be a
valid code address).

**Fix:** split the `Rd=R15 + S` handling into two cases based on
`op.is_test()`. Test ops: restore CPSR from SPSR, leave PC alone.
Non-test ops: keep the existing "write PC + restore CPSR" path.

**Impact:** in the arm.gba psr_transfer section, this was escaping the
CPU into unmapped memory. After the fix, arm.gba advances to test 360.

### 5. LDR with writeback, Rn==Rd

**Where:** `gba-core/src/arm7tdmi/arm.rs::arm_single_transfer`.

**What:** ARM7TDMI behavior: for a load with writeback where base (`Rn`)
equals destination (`Rd`), the **load wins** — the loaded value stays in
the register; the writeback address is discarded. Our code did both
writes in order, so the writeback address overwrote the loaded value.

**Fix:** skip writeback when `l && rn == rd`.

**Impact:** `arm.gba` test 360 (`LDR R0, [R0, #4]!`) now passes.

## Status after sweep

- `arm.gba` → Failed test 360 → *<user reports new number after rebuild>*
- `thumb.gba` → Failed test 219 (format-10 LDRH misalign; TODO above)
- `memory.gba` → All tests passed (unchanged)
- `bios.gba` → Failed test 001 (unchanged)

**Pokémon Emerald audio:** still noisy. Five CPU bug fixes in one session
— including one severe (MSR banking) — did not resolve the M4A garbage.
This strongly suggests there are more CPU bugs left, or the bug is
somewhere else entirely (DMA, timer interrupt timing, memory region
quirks). Next moves:

- Continue arm.gba sweep past current failure
- Investigate bios.gba test 001 (BIOS SWI implementations)
- Return to thumb.gba 219 (format-10 LDRH crash root cause)
- Compare instruction trace against mGBA on a small window of Pokémon
  boot to find first divergence

## Tooling added

- `examples/run_test_rom.rs` — runs a test ROM for N frames, dumps the
  framebuffer as PPM, prints per-row pixel density for quick
  pass/fail eyeballing.
- `examples/trace_arm_escape.rs` — steps until PC leaves valid memory
  regions (ROM/IWRAM/EWRAM) and dumps the last N instructions plus the
  stack around SP. Essential for catching the MSR-banking escape.
- `examples/track_sp.rs` — first-delta tracker for SP; reports the step
  where SP first becomes invalid and the PC that caused it.
- `examples/track_mode.rs` — window-trace over a specific PC range
  showing mode/SP/R8/LR at each step. Used to verify the FIQ round-trip.
- `examples/probe_arm_test.rs`, `probe_thumb_test.rs`, `dump_sp_bug.rs` —
  ad-hoc probes for specific test-ROM failures.

All of these exit cleanly and are useful for future ARM7TDMI debugging.
