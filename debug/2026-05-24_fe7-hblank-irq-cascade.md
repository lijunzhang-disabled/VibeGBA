# FE7 intro hang — nested HBlank IRQ cascade

Status: **NOT FIXED.** Workaround: `DISABLE_HBLANK_IRQ=1`. Documents the investigation and where to pick up.

## Symptom

`./target/release/gba-frontend ~/Documents/FireEmblem/FireEmblem7.gba` boots, plays a few seconds of intro, then freezes mid-cutscene with constant audio. Game becomes unresponsive; CPU is stuck running garbage in IWRAM.

## What we know

The CPU is stuck in an endless nesting of HBlank IRQs. The chain:

1. FE7 sets DISPSTAT bit 4 (HBlank-IRQ-enable) at around vc=160 in some early frame so per-line effects can run.
2. HBlank IRQ fires once per scanline (verified, scheduler is correct — 1232 cycles apart).
3. The user IRQ handler at IWRAM `0x03003950` runs the standard pattern:
   - prologue (set R3 = 0x04000200, read REG_IE/IF, save SPSR, STMFD push)
   - dispatch ANDS+BNE chain to find the matched IRQ bit
   - ACK via `STRH R0, [R3, #2]` at `0x03003A20` (write the bit back to REG_IF)
   - MRS/BIC/ORR to construct a System-mode CPSR value with I=0
   - `MSR CPSR_FC, R3` at `0x03003A30` — re-enables IRQs
   - `BX R0` at `0x03003A48` calls the per-IRQ subhandler. For HBlank, R0=`0x080BB355` (THUMB → real PC `0x080BB354`). For VBlank, R0=`0x0801524D`.
4. On real GBA the depth-2 HBlank subhandler completes within ≤ 1232 cycles, so HBlank N+2 finds the CPU back in user code. Bounded nesting.
5. On our emulator the depth-2 HBlank subhandler does **not** complete in 1232 cycles, so HBlank N+2 fires *during* depth-2's execution → depth-3. Then depth-3 also doesn't complete → depth-4. Etc.
6. Stack frame per nested IRQ = BIOS push 24 + handler STMFD 16 + LR push 4 = **44 bytes**.
7. SP_irq starts at `0x03007FA0`. After ~447 nested entries SP_irq has descended to `0x03003950` and **the stack has overwritten the IRQ handler code itself**. From then on R3 doesn't get set, ACK writes to address `2` (BIOS), IF never clears, MSR keeps firing nested IRQs forever, CPU eventually runs off into garbage.

The transition is sharp. Running with `FE7_PROBE=1` and analysing handler-entry SP values:
- Indexes 0–23987: SP at handler entry is either `0x03007F88` (depth 1) or `0x03007F60` (depth 2 — normal nested HBlank during VBlank subhandler). 24,000+ healthy iterations.
- Index 23988+ (at vc=100, a *visible* scanline): SP drops by 0x28 every iteration — depth 3, 4, 5, … unbounded. Cascade starts.

Cascade only kicks in on **visible scanlines**. VBlank lines (160–227) stay at depth 2 because something about visible-line HBlank handling tips us over the 1232-cycle budget. The most plausible difference is HBlank DMA + scanline-render work happening just before/during the IRQ window on visible lines.

## Definitive test

Setting `DISABLE_HBLANK_IRQ=1` (env-gated short-circuit in `gba-core/src/lib.rs` HBlank event) — FE7 boots and runs to gameplay. So the cascade is the cause, not some other corruption.

## What was eliminated

- **Multiple HBlank IRQs per scanline**: instrumented with `HBLANK_IRQ_TRACE=1`. Found exactly one fire per line at exactly 1232 cycles apart (min=1232, max=1242, no consecutive same-vc fires across 49k events). Scheduler is correct.
- **ACK reaching the wrong address in healthy state**: with `R3=0x04000200`, `STRH R0, [R3, #2]` does write to `0x04000202` = REG_IF and `write_if(val)` correctly does `ir &= !val`. Verified — IF is 0 at PC=0x3A30 in healthy iterations.
- **IRQ delivery firing too eagerly between A20 and A30**: in healthy iterations IF=0 at A30, so the cascade isn't from a missed ACK.
- **Subhandler table corruption**: in healthy iterations R0 at the BX dispatch is the correct ROM-resident subhandler (`0x080BB355` for HBlank). Late iterations have R0=0 but only *after* the cascade is well underway — that's a *consequence* of stack overwriting the table at `0x030028E0`, not the cause.

## What's actually wrong

Depth-2 HBlank handler (which is just the same handler code, recursed) takes > 1232 cycles in our emulator. Most likely culprits, in rough order:

1. **CPU cycle accounting** for some THUMB instruction (or ARM-mode handler prologue instruction) is too high. The handler is ~50 ARM instructions + the subhandler is THUMB. If any one instruction has the wrong cycle cost, accumulated error could push us over.
2. **Memory wait states** are not modelled — but `gba-core/src/bus/mod.rs:720` shows we read/write WAITCNT, we just don't apply wait penalties anywhere. Note: this should make us *faster* than real hardware, not slower. So this alone isn't the explanation. (It could still matter if real GBA's *user code* runs slower and gives MORE budget to the handler somehow.)
3. **DMA cycles discarded**: `run_dma_for_timing` does `let (_cycles, irq) = self.bus.run_dma(ch_id);` — `_cycles` is thrown away. Real GBA blocks the CPU during DMA. In our model, DMA is instantaneous from the CPU's point of view. Again, this should make us *faster*, not slower.

Plausible single-instruction culprits to check:
- THUMB LDR with PC-relative addressing
- THUMB PUSH/POP timing
- ARM STMFD/LDMFD with large register lists (FE7 handler pushes 4 regs)
- MRS/MSR cycle costs

## Tools left in the tree

All env-gated, no perf impact when unset:

- `FE7_PROBE=1` — logs at PC = 0x03003950 (handler entry), 0x03003A20 (post-ACK), 0x03003A30 (pre-MSR), 0x03003A48 (BX R0). Shows R0/R1/R2/R3, SP, dispstat, vc, IE/IF. Lives in `gba-core/src/arm7tdmi/mod.rs:step_arm`.
- `HBLANK_IRQ_TRACE=1` — logs every HBlank IRQ fire with time, vc, ir, ie. In `gba-core/src/lib.rs` HBlank event handler.
- `DISABLE_HBLANK_IRQ=1` — short-circuits the HBlank IRQ fire path. The current workaround for booting FE7.

## Where to pick up

The path forward is to measure the actual cycle cost of one full depth-2 HBlank handler iteration in our emulator (entry-to-exit), compare to the real-hardware budget of < 1232 cycles, and find the discrepancy. Concrete next steps:

1. Add an instruction-counter probe at HBlank-subhandler entry (PC=`0x080BB354`) and exit (the POP {…, PC} instructions in that function). Print delta cycles per call.
2. Disassemble the full subhandler in `~/Documents/FireEmblem/FireEmblem7.gba` from `0x080BB354` to its terminal POP. Identify the hot path.
3. Cycle-budget the hot path by hand using ARM7TDMI cycle rules; compare to what our `cycles` return values say.
4. Likely fix is in `gba-core/src/arm7tdmi/thumb.rs` cycle returns. If LDR/STR-via-pool costs aren't tracking the 1S+1N+1I model correctly, that could be it.
