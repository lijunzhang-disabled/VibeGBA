# Pokémon Emerald in-game save: IRQ pipeline-refill ordering

Date: 2026-04-29
Status: **Fixed** (commit b29226f)

## Symptom

After the 2026-04-26 fixes (8-bit-bus broadcast on Flash reads, Macronix
chip ID), Pokémon Emerald in-game saves *sometimes* round-tripped
correctly — and sometimes did not. Same code, no apparent input
difference, flaky outcome.

Measured failure rate via `/tmp/save_test.sh` (clean .sav, save in-game,
close, verify all 14 sectors checksum-valid):

```
Run 1: PASS    14/14
Run 2: BAD     1 good, 13 bad
Run 3: BAD     2 good, 12 bad
Run 4: PASS    14/14
Run 5: BAD     0 good, 14 bad
```

≈40% PASS, ≈60% BAD across many trials.

Failing runs always hit the same escape pattern: the CPU eventually
jumped to an unmapped flash address (e.g. `0x0EE276F4`) via a `BX R1`
at `0x082E197C`. Same instruction, same kind of decoded garbage, every
BAD run.

## Investigation log

### 1. Confirm a CPU-state corruption hypothesis

Suspected an IRQ during Pokémon's flash-save loop was corrupting CPU
state somehow. To test this hypothesis without yet finding the bug,
added an `EXPERIMENT_GATE=1` env var that blocks IRQ delivery while
flash is mid-save (sticky for 200k cycles after each Flash write, so
the entire save loop is gated even though individual command sequences
are brief).

Result with gate enabled:

```
Run 1-5: PASS    14/14   (5/5)
```

5/5 with gate vs 2/5 without → IRQ delivery during the save loop is
definitely the trigger. Now to find the actual CPU-side bug.

### 2. Read the IRQ trace, look for clues

Added IME/IE/IF fields to the existing `IRQ_TRACE` log. Every IRQ that
fired during Pokémon's flash code had `ime=true` — i.e., the outer
flash code was running with IRQs globally enabled, taking vblank IRQs
mid-save. So the question becomes: why does our IRQ entry/exit
sometimes leave Pokémon resuming at the wrong PC?

### 3. Walk the IRQ entry path, check return-address math

Our `cpu::handle_interrupt()`:

```rust
fn handle_interrupt(&mut self, bus: &mut Bus) {
    let return_addr = if self.cpsr.thumb() {
        self.regs[15]                   // expects regs[15] = next + 4
    } else {
        self.regs[15].wrapping_sub(4)   // expects regs[15] = next + 8 → next + 4
    };
    // ... save SPSR, switch to IRQ mode, set LR_irq = return_addr,
    //     jump to vector 0x18 ...
}
```

This is the *correct* math under the ARM7TDMI invariant that during
execution, R15 reads as `executing_address + 8` (ARM) or `+ 4` (THUMB).
The HLE BIOS stub at `0x18` ends with `SUBS PC, LR, #4`, which restores
CPSR from SPSR and sets `PC = LR_irq − 4`. So `LR_irq = next + 4` →
`PC = next` on return. Correct.

The bug isn't the math here. It's *when* this math runs.

### 4. The pipeline-flushed window

Our `cpu::step()` (pre-fix) had IRQ check **before** pipeline refill:

```rust
pub fn step(&mut self, bus: &mut Bus) -> u32 {
    // (1) IRQ check — reads regs[15]
    if bus.interrupt.has_pending() && !self.cpsr.irq_disabled() {
        self.handle_interrupt(bus);
    }
    if self.halted { return 1; }

    // (2) Pipeline refill — repairs regs[15] after a branch
    if self.pipeline_flushed {
        self.refill_pipeline(bus);
    }

    // (3) Decode + execute
    if self.cpsr.thumb() { self.step_thumb(bus) } else { self.step_arm(bus) }
}
```

The crux: any instruction that writes PC — `B`, `BX`, `LDR PC`,
`MOV PC`, `LDM` with R15 in list, `SUBS PC, LR, #4`, exception entry —
sets `regs[15] = raw_target` and `pipeline_flushed = true`. The
invariant `regs[15] = next + 4 (or +8)` is **temporarily broken**
between the branch and the next refill.

If an IRQ arrives in that single-step window, `handle_interrupt()`
reads `regs[15] = target` (the raw, un-pipeline-adjusted value) and
stores:

| Mode  | `regs[15]` actually | `LR_irq` becomes | Should have been | Off by |
|-------|---------------------|------------------|------------------|--------|
| THUMB | `target`            | `target`         | `target + 4`     | -4     |
| ARM   | `target`            | `target − 4`     | `target + 4`     | -8     |

After the IRQ handler runs and exits via `SUBS PC, LR, #4`, the CPU
resumes at `target − 4` (THUMB) or `target − 8` (ARM) — **inside** a
preceding instruction or literal pool word. The bytes at that address
decode as some unrelated opcode, often a load + `BX Rn` chain that
walks off into garbage.

### 5. Why this looked like flaky timing

The vulnerable window is exactly one CPU step after each branch. With
~60 Hz vblank IRQs (one every ≈280k CPU cycles) and Pokémon's save
loop containing tens of thousands of branches that take ~1–4 cycles
each, the per-save collision probability worked out to roughly 60%.
That matches the measured failure rate.

Save state (`]`/`[`) snapshots don't exhibit this because they're a
single deterministic memory copy — no IRQ delivery during the snapshot
path.

### 6. The fix

Move the refill **above** the IRQ check so `regs[15]` is always at the
correct pipeline-ahead value before `handle_interrupt()` reads it:

```rust
pub fn step(&mut self, bus: &mut Bus) -> u32 {
    // Refill FIRST — establishes the regs[15] = next + 4 / + 8 invariant.
    if self.pipeline_flushed {
        self.refill_pipeline(bus);
    }

    if bus.interrupt.has_pending() && !self.cpsr.irq_disabled() {
        self.handle_interrupt(bus);   // now reads a correct regs[15]
    }
    if self.halted { return 1; }

    // IRQ entry flushed again — refill at the vector.
    if self.pipeline_flushed {
        self.refill_pipeline(bus);
    }

    if self.cpsr.thumb() { self.step_thumb(bus) } else { self.step_arm(bus) }
}
```

Two refills are possible per step (once before IRQ check, once after
IRQ entry). Each is idempotent — only runs when actually flushed. No
measurable performance impact in the hot path.

## Verification

- All 90 unit tests pass (no regression).
- `/tmp/save_test.sh` 5/5 PASS, including a save+overwrite cycle (the
  PARTIAL 28/14 verdict on one run was Pokémon's two-block save scheme
  working as intended — both old and new save blocks valid).
- `EXPERIMENT_GATE` left in tree as a future debugging aid; zero
  runtime impact unless the env var is set.

## Why our IRQ test ROM didn't catch this

The `cpu_arm.gba` / `cpu_thumb.gba` jsmolka tests verify IRQ entry/exit
register state and basic branch+IRQ interaction, but they don't fire
IRQs in the *single-cycle* window between a branch and its refill —
their IRQ injection is positioned at known stable points. The flaky
window only opens when an external interrupt source happens to coincide
with a pipeline flush, which is a real-game scheduling artifact rather
than an architectural test target.

A useful future test would be: synthesize an IRQ source whose firing
cycle is configurable, sweep the firing time across a branch
instruction, and check that the post-IRQ resume PC matches the branch
target for every alignment.

## Related

- Commit b29226f: the fix.
- Commit 64b04c0 (2026-04-26): 8-bit Flash bus broadcast — *necessary*
  prerequisite. Without that fix, Pokémon's read-back checksum fails
  even when the IRQ pipeline bug doesn't fire.
- `debug/followups.md`: rolling status; this entry promoted from
  "next session" to "resolved" after the b29226f commit.
