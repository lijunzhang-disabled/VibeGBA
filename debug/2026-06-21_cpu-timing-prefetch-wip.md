# CPU timing accuracy: wait-states + prefetch buffer (WIP)

Branch: `cpu-timing-prefetch`. Goal: model GamePak wait-states + the prefetch
buffer so ROM-resident code (the M4A sound engine) runs at hardware speed —
fixing the HoD jump-SFX timing divergence vs mGBA (and improving accuracy
generally). The alyosha timing tests are the objective gate; the emu-agent
differential audio oracle is the regression gate.

## Baseline (before)
- Instruction correctness (arm/thumb/memory): ALL PASS.
- alyosha prefetcher tests: ALL FAIL (prefetch buffer unmodeled).
- alyosha fifo_dma: 5/6 fail. Halt_DMA_IRQ/Halt_IRQ: fail.
- Root cause: VibeGBA times ROM code identically to IWRAM (flat per-instruction
  cycle counts; add_mem_cycles was a no-op; WAITCNT stored but never consulted).

## What was implemented (gated behind TIMING_MODE env, default 0 = off)
- `gba-core/src/bus/timing.rs`: WaitTable (WAITCNT decode → N/S extra cycles per
  ROM region + SRAM + prefetch-enable) and Prefetch (credit-based buffer).
- bus `add_mem_cycles` dispatches: opcode fetch (prefetch-aware, mode&1) vs data
  access (per-region/width wait-states, mode&2). New `fetch16/fetch32` set
  `fetch_mode`; the CPU pipeline (advance/refill) uses them. CPU calls
  `prefetch_advance(total)` per instruction. WAITCNT write calls `wait.update`.

## Validation results
- **mode 2 (data wait-states only): SAFE.** arm/thumb pass; Emerald/HoD/GoldenSun
  all run + produce audio. Real accuracy improvement.
- **mode 1/3 (opcode-fetch prefetch): improves prefetcher tests** (full_arm_2
  26→73, full_thumb 16→35; full_thumb correctness preserved) **BUT HANGS Emerald.**

## The Emerald hang (the blocker — START HERE)
Under mode 1, Emerald freezes early in boot, spinning at ROM `0x080008C6`:
```
0x8B8: STRH r0,[r2,#0x1C]
0x8BA: LDRH r1,[r2,#0x1C]
0x8BC: MOVS r0,#1 ; ANDS r0,r1 ; CMP r0,#0 ; BNE 0x8D0   (exit if bit0 set)
0x8C4: MOVS r3,#1
0x8C6: LDRH r1,[r2,#0x1C] ; ADDS r0,r3,#0 ; ANDS r0,r1 ; CMP r0,#0 ; BEQ 0x8C6  ← spins
```
It polls a 16-bit register at `[r2+0x1C]` for bit 0, which never sets under the
fetch-timing change. At that point `IE=0x0085` (VBlank+VCount+**Serial**) vs
`0x0005` normally — so Emerald took a different/early path. Nearby literal pool
stores to EWRAM `0x0203CF5C`. Looks like an SIO / serial / save-detection
boot-time wait whose completion depends on cycle timing.

### Next steps to root-cause
1. Identify `r2` at the loop (add a CPU-reg dump to the harness PC_SAMPLE, or a
   gba-core trace) → which register is `[r2+0x1C]`. Likely an SIO reg
   (0x040000xx) or a RAM flag set by an IRQ that isn't firing.
2. If it's an IRQ-set RAM flag: the fetch timing likely shifted IRQ/scheduler
   timing so the expected IRQ (Serial?) never fires or fires at the wrong point.
3. Check the interaction with the VBlank FIFO-DMA re-anchor + halt sub-stepping
   (lib.rs) — those heuristics were tuned to the OLD (flat) timing and may
   misfire under accurate fetch timing.
4. Likely the prefetch model needs to NOT change cycle counts for the very early
   boot path, or the model's per-instruction phase (prefetch_advance after the
   instruction vs during) is wrong enough to break a tight timing-locked loop.

Reproduce: `TIMING_MODE=1 PC_SAMPLE=1` via the emu-agent gba harness on Emerald.
