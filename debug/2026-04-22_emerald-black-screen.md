# Pokémon Emerald Black Screen — Pipeline Ordering + MRS/MSR Misdecoding

**Date fixed:** 2026-04-22
**Phase context:** Phase 1 (CPU) — latent bugs not caught by existing unit tests
**Status:** Fixed

## Symptom

Pokémon Emerald (`PokemonEmeraldVersion.gba`, a valid BPEE ROM) boots to a pure black screen. `DISPCNT` is never written. The game never advances past its earliest init code.

## How it was found

Running the diagnose example (`gba-core/examples/diagnose.rs`) against the ROM showed PC escaping ROM within the first 3 instructions:

```
step 0: PC=0x08000000  ← entry, executes B +offset
step 1: PC=0x08000204  ← landed (but at the wrong target — should have been considered normal)
step 2: PC=0x08000210  ← also advanced past MSR
step 3: PC=0x00000023  ← derailed, now in BIOS open-bus territory
```

After step 3, R0 = 0x12 (which was the expected FIQ mode constant), but PC had escaped to 0x23 — suspicious because 0x1F + 4 = 0x23, and 0x1F is the System mode constant.

## Investigation

### First hypothesis: pipeline refill off-by-one after branch

The standard GBA ARM startup stub at 0x08000204 is:

```
0x08000204:  MOV R0, #0x12        ; FIQ mode constant   (0xE3A00012)
0x08000208:  MSR CPSR_fc, R0      ; switch to FIQ       (0xE129F000)
0x0800020C:  LDR SP, [PC, #0x28]  ; load FIQ stack      (0xE59FD028)
0x08000210:  MOV R0, #0x1F        ; System mode constant
0x08000214:  MSR CPSR_fc, R0      ; switch to System
```

R0=0x12 after the MOV is correct. But PC=0x23 after the MSR is weird.

Traced through `Cpu::step_arm`:

```rust
fn step_arm(&mut self, bus: &mut Bus) -> u32 {
    let opcode = self.pipeline[0];
    // Advance pipeline (this is the buggy ordering!)
    self.pipeline[0] = self.pipeline[1];
    self.pipeline[1] = bus.read32(self.regs[15]);
    self.regs[15] = self.regs[15].wrapping_add(4);
    // ...
    self.execute_arm(bus, opcode)
}
```

After `refill_pipeline`, `regs[15] = X + 8` (correct per ARM spec — during execution of instruction at X, reads of PC return X+8). But `step_arm` then advances to `X + 12` BEFORE calling `execute_arm`. So every instruction saw `regs[15] = X + 12`, not `X + 8`.

For branches: `target = regs[15] + offset = (X+12) + offset`, off by +4.

### Fix attempt 1: move pipeline advance to AFTER execute

Rewrote `step_arm` and `step_thumb`:

```rust
fn step_arm(&mut self, bus: &mut Bus) -> u32 {
    // Invariant: at start of step, regs[15] = executing_address + 8
    let opcode = self.pipeline[0];
    if !self.check_condition(opcode >> 28) {
        self.advance_arm_pipeline(bus);
        return 1;
    }
    let cycles = self.execute_arm(bus, opcode);
    if !self.pipeline_flushed {
        self.advance_arm_pipeline(bus);
    }
    cycles
}
```

All 80 existing tests still passed. Re-ran diagnose against Emerald:

```
step 0: PC=0x08000000
step 1: PC=0x08000204  R0=0x00000012  (MOV correctly executed)
step 2: PC=0x08000210  R0=0x00000012  R13=0x03007F00
step 3: PC=0x00000023  R0=0x00000012  R13=0x00000000  ← STILL BROKEN
```

R13 did switch from System's SP (0x03007F00) to FIQ's SP (0, uninitialized), so **MSR did fire** and switch modes. But PC still escaped to 0x23.

### Second hypothesis: MSR being misdecoded

Traced manually through the decoder for opcode `0xE129F000`:

- `bits_27_20` = `(0xE129F000 >> 20) & 0xFF` = `0x12`
- Top-level dispatch: `bits_27_20 >> 5` = 0 → enters "0b000" branch
- Inside that branch:

```rust
if (bits_27_20 & 0xF9) == 0x10 && bits_7_4 == 0x0 {
    // MRS
    self.arm_mrs(opcode)
} else if (bits_27_20 & 0xFB) == 0x12 && bits_7_4 == 0x0 {
    // MSR (register)
    self.arm_msr(opcode)
}
```

Check MRS condition: `0x12 & 0xF9` = `0x10`. **Matches!** So `0xE129F000` (MSR) was being dispatched to the MRS handler.

Why? `0xF9` = `1111 1001` in binary. This mask CLEARS bit 21 (and bit 22). But bit 21 is exactly what distinguishes MRS (bit 21=0) from MSR-register (bit 21=1). With bit 21 masked out, both `0x10` (MRS) and `0x12` (MSR) satisfy the condition.

Looking at `arm_mrs`:

```rust
fn arm_mrs(&mut self, opcode: u32) -> u32 {
    let spsr = opcode & (1 << 22) != 0;
    let rd = ((opcode >> 12) & 0xF) as u8;

    let psr = if spsr { self.spsr() } else { self.cpsr };
    self.regs[rd as usize] = psr.bits;  // <-- writes CPSR directly to regs[rd]
    1
}
```

For opcode `0xE129F000`: `rd = (0xE129F >> 12) & 0xF` — wait, let me recompute. `(opcode >> 12) & 0xF` = `(0xE129F000 >> 12) & 0xF` = `0xE129F & 0xF` = `0xF` = **15 = PC**!

So the bogus MRS path was doing `regs[15] = CPSR.bits = 0x1F`. Then `step_arm`'s pipeline advance bumped `regs[15]` to `0x1F + 4 = 0x23`.

**0x23 in the diagnose trace is exactly the artifact of `MRS PC, CPSR` writing 0x1F to PC, then the pipeline advance.**

### Fix 2: tighten MRS decoder mask

Changed the MRS mask from `0xF9` to `0xFB`:

```rust
// MRS: bits[27:20] = 0001 0P00 (bit 21 = 0)
// MSR: bits[27:20] = 0001 0P10 (bit 21 = 1)
// Distinguish via bit 21: mask 0xFB includes bit 21.
if (bits_27_20 & 0xFB) == 0x10 && bits_7_4 == 0x0 {
    self.arm_mrs(opcode)
} else if (bits_27_20 & 0xFB) == 0x12 && bits_7_4 == 0x0 {
    self.arm_msr(opcode)
}
```

Verify:
- MRS opcode (bits_27_20 = 0x10): `0x10 & 0xFB = 0x10`. Match MRS. ✓
- MSR opcode (bits_27_20 = 0x12): `0x12 & 0xFB = 0x12`. Does NOT match MRS, matches MSR. ✓

## Root cause(s)

Two independent bugs, both in `gba-core/src/arm7tdmi/`:

1. **Pipeline advance ordering** (`mod.rs:step_arm`, `step_thumb`): the pipeline was advanced BEFORE execute, making `regs[15] = X + 12` during execution. The ARM7TDMI spec requires `regs[15] = X + 8` (`X + 4` in THUMB) during execution so that PC-relative addressing works.

2. **MRS decoder mask too loose** (`arm.rs` instruction dispatch): mask `0xF9` failed to distinguish MRS from MSR-register because bit 21 (the distinguishing bit) was masked out. All MSR instructions were mis-routed to the MRS handler.

Both were latent because:
- The existing `test_arm_branch` test set up `regs[15] = 0x08000008` manually and called `execute_arm` directly, bypassing `step_arm`.
- No existing test covered the MSR path.

## Fix

### Files changed

**`gba-core/src/arm7tdmi/mod.rs`**

Split the pipeline fetch/advance into a helper, moved to after execute:

```rust
fn step_arm(&mut self, bus: &mut Bus) -> u32 {
    // Invariant: at start of step, regs[15] = executing_instruction_address + 8.
    let opcode = self.pipeline[0];

    if !self.check_condition(opcode >> 28) {
        self.advance_arm_pipeline(bus);
        return 1;
    }

    let cycles = self.execute_arm(bus, opcode);

    if !self.pipeline_flushed {
        self.advance_arm_pipeline(bus);
    }

    cycles
}

#[inline]
fn advance_arm_pipeline(&mut self, bus: &mut Bus) {
    self.pipeline[0] = self.pipeline[1];
    self.pipeline[1] = bus.read32(self.regs[15]);
    self.regs[15] = self.regs[15].wrapping_add(4);
}
```

(Same pattern for `step_thumb` / `advance_thumb_pipeline`.)

**`gba-core/src/arm7tdmi/arm.rs`**

Tightened MRS/MSR decoder masks:

```rust
// Was: (bits_27_20 & 0xF9) == 0x10
// Now: (bits_27_20 & 0xFB) == 0x10
if (bits_27_20 & 0xFB) == 0x10 && bits_7_4 == 0x0 {
    self.arm_mrs(opcode)
} else if (bits_27_20 & 0xFB) == 0x12 && bits_7_4 == 0x0 {
    self.arm_msr(opcode)
}
```

## Regression tests

Added to `gba-core/src/lib.rs` (in the top-level `mod tests`):

1. **`test_branch_then_mov_pipeline`** — builds a ROM with `B +0x200` at 0x0 and `MOV R0, #0x12` at the target. Verifies R0=0x12 (catches the pipeline-ordering bug — with it, the branch lands 4 bytes too far and we'd execute a different instruction).

2. **`test_msr_not_decoded_as_mrs`** — executes `MOV R0, #0x12; MSR CPSR_fc, R0`. Verifies that CPSR mode becomes FIQ (0x12) and PC does not equal 0x1F (PC=0x1F is the telltale sign of MSR-as-MRS with Rd=PC).

3. **`test_arm_pc_read_during_execute`** — `MOV R0, R15` at 0x08000000. Verifies R0=0x08000008 (the ARM PC invariant: during execution, PC reads return `instruction_addr + 8`).

## Verification

### Unit tests
- Before fix: 80 tests passing
- After fix: 83 tests passing (80 original + 3 new regression tests)
- Zero warnings, clean release build

### Pokémon Emerald diagnose trace

Before fix (PC escapes at step 2):
```
step 0: PC=0x08000000
step 1: PC=0x08000204  (R0=0x12)
step 2: PC=0x08000210
step 3: PC=0x00000023  ← ESCAPED
```

After fix (PC stays in ROM for 10,000+ instructions):
```
step 0: PC=0x08000000              (B)
step 1: PC=0x08000204  R0=0x12     (MOV R0, #0x12)
step 2: PC=0x08000210  R0=0x12     (MSR CPSR, R0 → switch to FIQ)
step 3: PC=0x08000214  R13=0x3007FA0  (LDR SP → FIQ stack set)
step 4: PC=0x08000218  R0=0x1F     (MOV R0, #0x1F)
step 5: PC=0x0800021C              (MSR CPSR, R0 → switch to System)
step 6: PC=0x08000220  R13=0x3007E40  (LDR SP → System stack set)
...
step 12: PC=0x08000238  THUMB=true  (BX R1 → switched to THUMB, jumping to ROM entry)
step 13: PC=0x080003A4  THUMB=true  (game code proper begins)
```

### Frame-level verification

Running `Gba::run_frame()` in a loop:
- Frame 0: `DISPCNT=0x0000` (game still in init)
- Frame 11: `DISPCNT=0x0140` (BG0 enabled, Mode 0) — **display active, 38,400 nonzero pixels in framebuffer**

All four criteria from the original bug doc now pass:
- [x] PC stays inside `0x08000000+` for the full 1000 instructions
- [x] DISPCNT becomes nonzero within the first few frames
- [x] Framebuffer has nonzero pixels
- [x] The game progresses through init and begins rendering

## Related issues (NOT fixed by this)

Around frame 11, PC escapes to the 0x00xxxxxx region (BIOS/open-bus). The PC value increases by a consistent ~0x112500 per subsequent frame, indicating the CPU is fetching open-bus data as 1-cycle instructions. This is a **separate bug** — likely an unhandled CPU instruction, DMA pattern, or BIOS HLE function used by the game. The fixes in this doc resolve only the earliest-init failure.

## Artifacts

- Diagnostic tool used: `gba-core/examples/diagnose.rs` (kept — useful for future CPU bugs)
- Test ROM: `PokemonEmeraldVersion.gba` (not checked in; user-supplied)

## Timeline

- **2026-04-15 → 2026-04-20**: Phases 1-7 implemented (session `12f8aecb-...`)
- **2026-04-22**: Bug discovered via diagnose tool (session `8fda7aa5-...`)
- **2026-04-22**: Both root causes found and fixed in one session
