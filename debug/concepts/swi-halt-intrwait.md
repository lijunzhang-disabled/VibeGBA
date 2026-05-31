# SWI Dispatch, Halt, and the IntrWait Re-halt Gate

How GBA games call BIOS system functions, how halt mode works, and why
IntrWait must selectively ignore non-matching IRQs.

## SWI — Software Interrupts

SWI is the ARM instruction for system calls. The game executes
`SWI #nn` (ARM) or `SWI #nn` (THUMB), passing a function number in the
comment field. On real hardware, this triggers an exception that jumps
to the BIOS vector at `0x00000008`. The BIOS reads the comment byte,
dispatches to the right function, and returns.

### HLE dispatch in our emulator

We don't run real BIOS code (unless the user supplies a dump). Instead
we implement each SWI function in Rust ("High-Level Emulation"). But
there's a borrow-checker constraint: during `cpu.step(&mut bus)`, the
CPU holds `&mut self` and `&mut Bus`. A BIOS function also needs both.
Calling it from inside `step()` would be a double mutable borrow.

Solution — the **pending flag pattern**:

```
CPU decode (arm.rs / thumb.rs)         Main loop (lib.rs)
─────────────────────────────          ───────────────────
SWI instruction detected               after step() returns:
  → self.pending_swi = Some(0x05)       if let Some(n) = cpu.pending_swi.take() {
  → step() returns                          bios::handle_swi(&mut cpu, &mut bus, n);
                                        }
```

`cpu` and `bus` are sibling fields of `Gba`, so the main loop can
borrow them independently — no conflict.

`handle_swi` (`bios.rs`) matches on the SWI number:

```rust
match comment {
    0x00 => swi_soft_reset(cpu, bus),
    0x02 => swi_halt(cpu),
    0x04 => swi_intr_wait(cpu, bus),
    0x05 => swi_vblank_intr_wait(cpu, bus),
    0x06 => swi_div(cpu),
    // ... 23 total
}
```

Code: `arm7tdmi/arm.rs` line 679, `arm7tdmi/thumb.rs` line 660,
`lib.rs` line 340, `bios.rs` line 32.

## Halt — why the CPU sleeps

Every GBA game's main loop looks like this:

```c
while (1) {
    process_input();
    update_game_state();
    render();
    VBlankIntrWait();   // SWI 0x05 — sleep until next VBlank
}
```

The game finishes its work in well under one frame (~16.7 ms) and then
**halts** — stops executing instructions and waits for the VBlank
interrupt. Without halt, the CPU would spin through the main loop many
times per frame, wasting power and potentially re-running game logic
when it shouldn't.

### Three ways to halt

| Mechanism | SWI / register | What sets it | Wake condition |
|---|---|---|---|
| **HALTCNT** | `bus.halt_requested` | Write to `0x04000301` | Any enabled IRQ (`IE & IF != 0`) |
| **SWI 0x02 Halt** | `cpu.halted` | `swi_halt()` | Any enabled IRQ |
| **SWI 0x04/0x05 IntrWait** | `cpu.halted` + `cpu.intrwait_mask` | `swi_intr_wait()` | Only IRQs matching the mask |

In the emulator, when `cpu.halted == true`, the main loop skips
`cpu.step()` and fast-forwards the scheduler to the next event. The
CPU sits idle until an interrupt wakes it.

Code: `bios.rs` lines 178–180 (Halt), `lib.rs` lines 356–380 (wake check).

## IntrWait — selective wake

`SWI 0x04` (`IntrWait`) and `SWI 0x05` (`VBlankIntrWait`) are more
specific than plain halt. They tell the BIOS "sleep until this
particular IRQ fires."

### Arguments

```
R0 = discard_old   (if non-zero, clear matching bits in BIOS_IF first)
R1 = irq_flags     (bitmask of which IRQs to wait for)
```

`VBlankIntrWait` is just shorthand for `IntrWait(1, 1)` — discard old,
wait for VBlank (bit 0).

### What real BIOS does (pseudocode)

On real hardware, the BIOS implements IntrWait as a **loop**:

```
IntrWait(discard_old, mask):
    if discard_old:
        BIOS_IF &= ~mask          // clear stale flags at 0x03007FF8

    loop:
        HALT                      // sleep (any IRQ will wake the CPU)
        if (BIOS_IF & mask) != 0: // did the IRQ we care about fire?
            BIOS_IF &= ~mask      // acknowledge
            return                // done — back to game code
        // wrong IRQ → go back to sleep
```

The key behaviour: it **re-halts** after each non-matching IRQ. The CPU
physically wakes on every IRQ (the ARM7TDMI doesn't know about masks),
but the BIOS code checks and goes right back to sleep if it wasn't the
one it wanted.

### HLE implementation

We can't run a real BIOS loop (that would require stepping through BIOS
instructions). Instead we emulate the **steady-state semantic** directly
with a mask field on the CPU:

```rust
// bios.rs — swi_intr_wait
fn swi_intr_wait(cpu: &mut Cpu, bus: &mut Bus) {
    let discard_old = cpu.regs[0] != 0;
    let irq_flags = cpu.regs[1] as u16;

    if discard_old {
        let current = bus.read16(0x0300_7FF8);
        bus.write16(0x0300_7FF8, current & !irq_flags);
    }

    cpu.intrwait_mask = if irq_flags != 0 { irq_flags } else { 0xFFFF };
    cpu.halted = true;
}
```

Then in the main loop wake-up check:

```rust
// lib.rs — halt wake gate
if self.cpu.halted {
    let pending = self.bus.interrupt.ie & self.bus.interrupt.ir;
    let mask = self.cpu.intrwait_mask;

    let wake = if mask != 0 {
        pending & mask != 0     // IntrWait: only matching IRQs wake
    } else {
        pending != 0            // plain Halt: any IRQ wakes
    };

    if wake {
        self.cpu.halted = false;
        self.cpu.intrwait_mask = 0;
    }
}
```

`intrwait_mask == 0` means "plain halt" (wake on anything).
`intrwait_mask != 0` means "IntrWait" (wake only when a matching IRQ is
pending).

Code: `arm7tdmi/mod.rs` line 186 (field), `bios.rs` lines 191–218
(IntrWait + VBlankIntrWait), `lib.rs` lines 356–380 (wake gate).

## The FE7 bug — why this matters

Fire Emblem 7's main loop calls `VBlankIntrWait()` (mask = `0x0001`,
VBlank) at the end of each iteration. FE7 also enables **HBlank IRQs**
(bit 1) for scroll effects — these fire 159 times per frame.

### Without the re-halt gate (broken)

```
Main loop finishes → VBlankIntrWait → cpu.halted = true, mask = 0x0001

  HBlank fires (1232 cycles later)
    IE & IF != 0 → wake! ← WRONG: HBlank doesn't match mask 0x0001
    Main loop runs again → M4A mixer runs → mixer output slot #1

  HBlank fires again
    wake! → main loop → mixer runs → slot #2

  HBlank fires again
    wake! → main loop → mixer runs → slot #3

  VBlank finally fires
    wake → main loop → mixer runs → slot #4

  Result: mixer ran ~3.5× per frame instead of 1×
```

The M4A audio engine's `SoundMain` mixer writes to a fixed 192-slot
channel buffer starting at IWRAM `0x2930`. Running it 3.5×/frame
produces ~670 writes instead of ~192, overflowing into the IRQ handler
code at `0x03003950`. The corrupted handler causes a nested IRQ cascade
→ hard freeze.

### With the re-halt gate (fixed, commit `bb4b916`)

```
Main loop finishes → VBlankIntrWait → cpu.halted = true, mask = 0x0001

  HBlank fires (bit 1)
    pending & mask = 0x0002 & 0x0001 = 0 → stay halted ✓

  HBlank fires again → still halted ✓
  ... 157 more HBlanks → still halted ✓

  VBlank fires (bit 0)
    pending & mask = 0x0001 & 0x0001 = 1 → WAKE ✓
    Main loop runs exactly once → mixer runs once → no overflow
```

The fix was ~10 lines of code: one new `u16` field on the CPU, set it
in `swi_intr_wait`, check it in the wake-up path.

### Lesson

The FE7 investigation ran for 5 days exploring wait-state timing and
prefetch-buffer models (all reverted — dead ends). The actual root cause
was purely **functional**, not timing-related: a missing BIOS loop
semantic in the HLE. The trap was attributing a behavioural bug to
cycle-accuracy when the real issue was a broken SWI contract.

See: `debug/2026-05-24_fe7-hblank-irq-cascade.md` for the full
investigation log.

## Re-framing the FE7 bug from the scheduler's perspective

(See [`scheduler.md`](scheduler.md) for the scheduler/wake-gate model.)

It's tempting to summarise the FE7 bug as "the CPU shouldn't take the
HBlank interrupts." That's slightly off. The HBlank IRQs are real and
intended — FE7 enables them for raster effects, the IRQ handler runs,
does its raster work, and returns. None of that is the bug.

The bug is one layer up. After the IRQ handler returns, the CPU was
supposed to stay **halted** (because FE7 called `VBlankIntrWait`,
meaning "wake me only for VBlank"). Our wake-gate didn't know about
the mask, so it treated *any* pending IRQ as a wake signal. So instead
of re-halting after the HBlank handler, the CPU fell through to FE7's
main loop body — running the audio mixer and game logic ~160× per
frame instead of once.

Said in scheduler-loop terms:

- The scheduler ran every frame the same way regardless: HBlank →
  HBlankEnd → HBlank → … → VBlank, ~320 events between two consecutive
  `VBlankIntrWait` calls. All on time. All firing the right IRQ bits
  in `IF`.
- The CPU was in halt-mode, so the inner loop fast-forwarded
  `timestamp` between events instead of stepping instructions. That
  also worked correctly.
- The **wake-gate** was the single point of failure. It saw
  `IE & IF != 0` on every HBlank and cleared `cpu.halted`, even though
  `intrwait_mask` said "VBlank only."

The fix doesn't change the scheduler, doesn't change event handling,
doesn't change IRQ delivery. It changes one condition in the wake-gate:
`pending != 0` → `pending & mask != 0`. That single filter turns 159
correct-but-unwanted wake signals back into a single VBlank wake — the
semantic the real BIOS achieves with its `HALT; check IF; loop` block.

The CPU isn't ignoring HBlank IRQs. It's running the handler, then
going back to sleep — which is what the mask is for.
