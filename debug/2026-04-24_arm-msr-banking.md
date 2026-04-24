# ARM MSR: mode-bit banking was silently skipped

Date: 2026-04-24
Status: **Fixed**

## Symptom

`arm.gba` from jsmolka's test suite showed a mostly-white framebuffer with a
few scattered dots — neither the "All tests passed" nor a "Failed test NNN"
glyph. The PPU palette had entry 0 set to `0xFFFF` (white) and the CPU PC had
escaped to `0x801Cxxxx` (unmapped memory), with PC walking linearly through
unmapped addresses executing open-bus garbage.

## How it was found

Running the test ROM via `examples/run_test_rom.rs` for 600 frames and
inspecting `/tmp/arm_result.png`. Then `examples/trace_arm_escape.rs`
and `examples/track_sp.rs` located the first step where SP became invalid.

## Investigation

The ARM-test init routine runs a canonical "verify banking" sequence:

```
0x08000AE0  MOV  R8, #0x20                 ; System mode R8
0x08000AE4  MSR  CPSR_fc, #0x11            ; → FIQ mode
0x08000AE8  MOV  R8, #0x40                 ; FIQ mode R8 (banked)
0x08000AEC  MSR  SPSR_fc, #0x1F            ; prepare exception return to System
0x08000AF0  SUBS PC, PC, #4                ; exception return → System mode
0x08000AF4  CMP  R8, #0x20                 ; expect System R8 == 0x20
0x08000AF8  BNE  <fail>
```

Per-step tracing showed SP transitioning from `0x03007F00` to `0x00000000` on
`SUBS PC, PC, #4`, and CPSR mode going from System to FIQ back to System —
without R13/R14/R8 ever being banked. Adding `eprintln!` tracing to
`arm_msr()` and `switch_mode()` revealed that **the MSR-to-FIQ never performed
any register banking at all**.

## Root cause

`arm_msr()` in `gba-core/src/arm7tdmi/arm.rs` mutated `self.cpsr.bits` *before*
calling `switch_mode()`:

```rust
let old_mode = self.cpsr.mode();
self.cpsr.bits = (self.cpsr.bits & !mask) | (val & mask);  // ← mutate first
let new_mode = self.cpsr.mode();
if old_mode != new_mode {
    self.switch_mode(new_mode);   // ← then call switch_mode
}
```

`switch_mode()` re-derives its own `old_mode` from `self.cpsr.mode()` — but
`cpsr.bits` was already updated, so `old_mode` inside `switch_mode` equals
`new_mode`, and the function early-returns via its "nothing to do" guard:

```rust
pub fn switch_mode(&mut self, new_mode: CpuMode) {
    let old_mode = self.cpsr.mode();   // reads the already-updated cpsr
    if old_mode == new_mode { return; } // always true from arm_msr
    // ... banking code never runs ...
}
```

Consequence: every MSR-CPSR that changed mode looked successful (the mode
bits were updated) but **no registers were banked**. The CPU kept using the
same R13/R14/R8-R12 across modes, storing them in whichever bank slot it
"happened to be in" as seen by other code paths.

The symptom surfaced on `SUBS PC, PC, #4` because that flows through
`set_reg_with_flags()`, which *correctly* calls `switch_mode()` before
mutating CPSR (via `self.cpsr = spsr`). So the exception return is the first
time banking actually ran — and it swapped in slots that had never been
populated (e.g. `banked.sp[System] = 0` because `new_skip_bios` only
initialized IRQ and Supervisor SPs, relying on the test's own MSR sequence
to set System's when switching out of it).

## Fix

`gba-core/src/arm7tdmi/arm.rs` — compute the new CPSR value and new mode
first, call `switch_mode()` with the OLD cpsr still in place (so banking
uses the correct source bank), then commit the new bits:

```rust
let old_mode = self.cpsr.mode();
let new_bits = (self.cpsr.bits & !mask) | (val & mask);
let new_mode = super::Psr { bits: new_bits }.mode();
if old_mode != new_mode {
    self.switch_mode(new_mode);
}
self.cpsr.bits = new_bits;
```

## Verification

- `examples/track_mode.rs` shows the canonical FIQ round-trip now preserves
  System's SP (0x03007F00) and R8 (0x20) across the mode switch.
- `arm.gba` no longer produces a white screen — it reaches the normal
  pass/fail text display.

## Related

- Found during investigation of Pokémon Emerald noisy audio
  ([2026-04-24_pokemon-emerald-noisy-audio.md](2026-04-24_pokemon-emerald-noisy-audio.md)).
  Any ROM whose IRQ handler relies on mode banking (which is effectively
  every game that uses a BIOS-vectored IRQ) would be affected. Strong
  candidate to explain M4A garbage writes.
- The same banking-on-mutation footgun does **not** exist in
  `set_reg_with_flags` or the IRQ/SWI entry in `arm7tdmi/mod.rs` — those
  paths call `switch_mode` before touching CPSR.
