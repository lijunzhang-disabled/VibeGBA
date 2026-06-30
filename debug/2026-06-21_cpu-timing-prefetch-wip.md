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

## RESOLVED (2026-06-29): the Emerald hang was a skip-BIOS DISPCNT bug
Root cause was **not** in the prefetch/fetch-timing model at all. `skip_bios`
(and the HLE-BIOS path) left `DISPCNT = 0`, but the real GBA BIOS hands control
to the cartridge with **forced-blank set (DISPCNT bit 7 = 0x0080)**.

Why that deadlocks Emerald: its GPU register manager `SetGpuReg`
(pokeemerald `src/gpu_regs.c`) writes the *hardware* DISPSTAT directly only when
`VCOUNT ∈ [161,225]` **or** forced-blank is set; otherwise it queues to a shadow
buffer that is flushed only inside `VBlankIntr`. The very first
`EnableInterrupts(INTR_FLAG_VBLANK)` (InitIntrHandlers) → `SetGpuReg(DISPSTAT)`
is what bootstraps the hardware VBlank-IRQ-enable bit (DISPSTAT 0x0008). With
forced-blank set at handoff that write always lands; without it, the bootstrap
becomes sensitive to the CPU↔PPU phase. Mode 0 happened to catch VCOUNT in the
VBlank window; the mode-1 fetch timing shifted the phase so every
`SetGpuReg(DISPSTAT)` queued instead → DISPSTAT 0x0008 never set in hardware →
VBlank IRQ never fires → `WaitForVBlank` (ROM `0x080008C6`, polling
`gMain.intrCheck` bit0 at IWRAM `gMain+0x1C` = `0x030022DC`) spins forever.

Fix: `Bus::new` seeds `io.dispcnt = 0x0080` when `!has_bios` (HLE/skip path).
Verified: Emerald boots under TIMING_MODE 0/1/2/3 (VBlank IRQs fire); jsmolka
arm/thumb/memory ALL PASS in modes 0/2/3; 91 unit tests pass; HoD/GoldenSun
render identically to the pre-fix baseline (no mode-0 regression). Diagnostic:
`gba-core/examples/emerald_hang.rs`.

Audio validation (emu-agent oracle, 2026-06-29): DONE.
- Aggregate spectral A/B vs mGBA: TIMING_MODE=3 safe and marginally CLOSER to
  mGBA than mode 0 on HoD / Emerald / Golden Sun. No regression.
- HoD jump-SFX onset (the original motivation): trigger press at frame 513,
  mGBA SFX onset at frame 514. Mode 3 = frame 514 (frame-accurate); mode 0 =
  frame 517-518 (~3-4 frames / ~55 ms late). Prefetch timing fixes the lag.
  (Method: LLM-driven HoD save→gameplay trace + appended quiet→A→quiet tail;
  single-pass save-loaded replay on emu modes 0/3 and mGBA. The built-in
  `diff --sfx` is unusable for save traces — skips load_save + double-replays;
  use ../emu-agent/compare_sfx_proper.py + inspect_sfx.py instead.)
- Separate finding (not timing): mGBA has a constant ~0.07 RMS background in the
  quiet standing segment where our emu is silent — ambient drone or DC artifact.

Recommendation: flip TIMING_MODE default 0 -> 3. All gates pass.

## (HISTORICAL) The Emerald hang investigation notes
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
