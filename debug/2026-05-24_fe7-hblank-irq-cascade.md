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

## Update 2026-05-24: cascade trigger is NOT a cycle-budget issue

A finer-grained probe (`FE7_PROBE=1` with bit-level breakpoints) traced the
cascade trigger to a SPECIFIC instruction-level corruption — not a per-call
timing overflow.

### Root mechanism

At cyc=31335028..31335045 (between vc=99 HBlank handler exit and vc=100
HBlank handler entry), FE7's own code at IWRAM `0x03003200` writes three
halfwords:

```
[WR16] 0x03003984 = 0x000E  pc=0x03003200 cyc=31335028
[WR16] 0x03003986 = 0x0096  pc=0x03003220 cyc=31335039
[WR16] 0x03003988 = 0x3985  pc=0x0300322C cyc=31335045
```

These overwrite the IRQ handler's dispatch instructions:
- IWRAM `0x03003984` was `0xE2822004` (`ADD R2, R2, #4`) → becomes `0x0096000E`
- IWRAM `0x03003988` was `0xE2110002` (`ANDS R0, R1, #2`, the bit-1 HBlank
  match test) → becomes `0xE2113985` (`ANDS R3, R1, #0x214000`)

The corrupted bit-1 test writes to R3 instead of R0, so:
1. The HBlank match is never detected (R0 stays 0, Z flag becomes 1 since
   R1 & 0x214000 = 0)
2. The handler walks past all bit tests, eventually ACKing with R0=0 at
   `0x03003A20`, which writes nothing to REG_IF
3. After MSR re-enables IRQs, IF.bit1 is still set → HBlank IRQ re-delivers
   immediately → depth-1 nested entry
4. Same corruption affects every nested invocation → unbounded cascade

The `0x03003984` corruption ALSO breaks the previous ADD: `0x0096000E`
decodes as `ADDEQS R0, R6, R14` (cond=EQ, opcode=ADD, S=1, Rn=R6, Rd=R0).
Since Z=1 from the prior bit-0 ANDS and R14 (LR_irq) = 0x28, R0 becomes
0x28 + R6 — observable in the probe log.

### What we DON'T yet understand

FE7 is INTENTIONALLY corrupting its own IRQ handler region. On real GBA
this presumably works — three possible explanations:

a. **HBlank IRQ is disabled by FE7 around the self-mod**, e.g. via clearing
   DISPSTAT bit 4, IE bit 1, or IME=0. Our probe shows DISPSTAT=0x001B
   (HBlank IRQ enabled) right through the cascade window, so if FE7 does
   disable, we're missing some write. Need to add MEM_WATCH on REG_IME
   (`0x04000208`), REG_IE (`0x04000200`), and DISPSTAT (`0x04000004`)
   across the cascade window.

b. **The IRQ vector is not actually 0x03003950 on real GBA at this point.**
   FE7 might relocate the handler entry to a different address whose code
   doesn't pass through 0x3984+. Our [0x03007FFC] reads as 0x03003950
   throughout. Either FE7 doesn't relocate (so this isn't it) or our IWRAM
   mirror handling for the vector at 0x03FFFFFC is broken.

c. **The 0x3984+ region is the SOUND MIXER buffer in FE7's design**, and
   the IRQ handler is only ~13 instructions long (ending before 0x3984).
   FE7 boots with a longer handler placed at 0x3950 then replaces it. We
   verified the bytes at 0x3984+ are still the original LONG handler bytes
   in healthy iter at vc=99, so this hypothesis requires FE7 to overwrite
   the dispatch chain WITHOUT us seeing a clean rewrite first — possible
   if FE7 writes piecewise.

The writes from `0x03003200` look like an audio mixer inner loop (LDRH /
ORR / AND / STRH with halfword shift-merge patterns) — consistent with
M4A/MP2K sound engine writing to a sample buffer.

### Tools added in this round

* `FE7_PROBE=1` extended to log opcode + memory contents at each dispatch
  PC. Reveals when the in-memory bytes diverge from what we expect.
* `GLOBAL_CYCLES` atomic in `gba-core/src/lib.rs`, updated before each
  `cpu.step()`. Lets MEM_WATCH and FE7_PROBE include cycle stamps without
  threading the scheduler through every probe.
* `MEM_WATCH` log lines now include `cyc=N` so we can correlate writes
  with handler executions.
* IWRAM dump key (`I`) — captures full IWRAM image to `/tmp/iwram-NNN.bin`
  for offline disassembly of runtime-resident routines (e.g. the writer
  at 0x3200).

## Update 2026-05-24 part 2: the writer at 0x03003200 is the audio mixer

Hypothesis (a) — "FE7 disables HBlank IRQ around the self-mod" — falsified
by `IRQ_GATE_TRACE=1`. In the cascade window (cyc=31334000..31336000) only
two IRQ-state writes happen, and both are normal handler ack/restore:

```
[IRQ-GATE] write IF: 0x0002 -> 0x0000 (ack 0x0002) cyc=31334075
[IRQ-GATE] write IE: 0x2003 -> 0x2003 cyc=31334176
```

No IME toggle, no DISPSTAT bit-4 clear, no IE.bit1 clear. HBlank IRQ is
fully enabled across the self-mod. So the mechanism is NOT IRQ-gating.

### What the writes actually are

The function at IWRAM `0x030031AC` is **FE7's M4A audio sample mixer**.
ARM-mode, ~38 instructions, loops over R4 input samples. Per iteration:

```
LDRH R1, [R2]    ; read source halfword
STRH R6, [R5,#0] ; write halfword 0 to dest
STRH R6, [R5,#2] ; write halfword 2
STRH R6, [R5,#4] ; write halfword 4
ADD R2, R2, #6   ; advance source by 6 bytes
ADD R5, R5, #8   ; advance dest by 8 bytes (3 halfwords + 2 alignment)
```

The destination is a write pointer stored at IWRAM `0x03002F34`. The
function pre-computes the post-loop pointer (`R5 + R4*8`) and saves it
back to `[0x03002F34]` BEFORE the loop runs, so subsequent calls pick
up where the last one left off. The audio sample buffer lives at
`0x03002930..0x03002F30` (192 samples × 8 bytes = 0x600 bytes).

### The actual cascade trigger

The buffer pointer storage at `0x03002F34` **overlaps with the buffer's
own write region**. Specifically, the 193rd-sample iteration (when
R5 = 0x03002F30) does `STRH R6, [R5, #4]` → writes sample data to
`0x03002F34` → **corrupts the buffer pointer itself**. The next mixer
call loads the corrupted pointer (which happens to land in the IRQ
handler's address space) and writes audio data into the handler.

Verified at cyc=31334913 in `/tmp/fe7_ptr.log`:

```
[WR32] 0x03002F34 = 0x03002F38  pc=0x030031D0 cyc=31334882   ; pre-compute (R5=0x2F30, R4=1)
[WR16] 0x03002F34 = 0x3985      pc=0x0300322C cyc=31334913   ; SELF-CORRUPTION via STRH [R5,#4]
[WR32] 0x03002F34 = 0x0300398D  pc=0x030031D0 cyc=31335014   ; next call: ptr now in IRQ handler
```

The next mixer call has R5 ≈ 0x03003984; its `STRH R6, [R5, #0/2/4]`
writes audio sample bytes into the IRQ handler at `0x03003984..0x03003988`,
corrupting `ADD R2, R2, #4` and `ANDS R0, R1, #2` (the bit-1 HBlank match).
Cascade begins on the very next HBlank IRQ.

### Why FE7 doesn't crash on real hardware

The mixer's caller (at LR=0x080043A7 in ROM) calls the mixer in batches of
~8 samples every ~6300 cycles (~21 kHz audio rate). Over a full GBA frame
(280896 cycles) that's about 350 samples — far more than the 192-sample
buffer holds. So **the buffer MUST wrap mid-frame on real FE7**, but in
our run the only buffer-pointer reset we see is once per frame from ROM
at `pc=0x08003310` (the M4A VBlank routine), with no intermediate wraps.

Real FE7 must have a wrap mechanism we're missing. Candidates:

a. **Sound DMA1 + Timer 0 cascade**: M4A typically uses DMA1 to stream
   samples from the buffer to FIFO_A, clocked by Timer 0 overflow. When
   DMA1 completes a buffer half, a DMA-complete IRQ fires the audio
   engine's swap routine, which presumably resets the pointer. If our
   DMA1-complete IRQ doesn't fire at the right time (or at all), the
   swap routine never runs → no mid-frame wrap.

b. **Some Timer IRQ we're delivering wrong**: if our timer overflow rate
   differs from real GBA (slightly faster), the audio engine's state
   machine could miss a phase transition that would normally wrap.

c. **Our CPU instructions are undercounting cycles**: causing the emulator
   to "fit more work per emulated frame" than real GBA. The mixer caller
   gets more chances to run per frame → more batches → overflow.

The fact that `DISABLE_HBLANK_IRQ=1` fixes the boot is consistent with
all three: removing HBlank IRQs removes the mixer-trigger chain.

### Where to pick up next

1. Trace **Sound DMA1 activity around the cascade window**: with
   `DMA_FIRE_TRACE=1` (already env-gated in `bus/mod.rs`), confirm DMA1
   refill timing, FIFO state, and DMA-complete IRQ delivery for both
   "healthy" frames (e.g., cyc≈12.9M) and the cascade frame
   (cyc≈31.3M). If healthy frames have a DMA1-complete IRQ that fires
   the M4A buffer swap and the cascade frame doesn't, that's the bug.

2. Add **Timer 0 overflow trace** for the same window. M4A's sample
   clock is Timer 0. If our overflow rate is off, mixer rate is off.

3. Disassemble FE7 ROM at `0x080043A0` to find the **mixer caller's
   wrap check** (if any). The caller might compare ptr against a limit
   and reset to 0x03002930 if it overshoots — and we might be skipping
   that path due to a register or memory state we get wrong.

4. Sanity-check our `cycles` return values for the hot instructions in
   the mixer (`STRH`, `LDRH`, `ADD imm`, `ORR reg`). If any are
   undercounting by 1 cycle, accumulated drift over hundreds of
   batches per frame could explain the extra-iteration overflow.

Tools added this round:

* `IRQ_GATE_TRACE=1` env var — logs every write to IE / IF / IME, and
  every change to DISPSTAT bits 3/4/5 (the IRQ-enable bits). Cycle-
  stamped. Lives in `gba-core/src/interrupt.rs` and `bus/mod.rs`.
* Mixer-entry probe: `FE7_PROBE=1` now also logs at PC=0x030031AC and
  the prologue PCs of the mixer function, recording R2/R4/R5/LR.

## Update 2026-05-24 part 3: audio peripherals confirmed correct

With `DMA_FIRE_TRACE=1 TIMER_TRACE=1 DMA_IRQ_TRACE=1`:

* **Timer 0**: reload=0xFB1A, prescaler=1 → 1254 cycles/overflow =
  **13380 Hz** in steady state. Matches M4A's 13379 Hz audio sample rate.
  IRQ enable = false (timer drives Sound DMA via Special timing only,
  no IRQ).
* **DMA1**: fires every ~20060 cycles ≈ 16 timer overflows in steady
  state. SAD cycles through `0x03004E80..0x03004F00` — a 128-byte ring
  buffer. IRQ enable = false.
* **DMA2**: same cadence as DMA1 (synchronized FIFO refills).
* **DMA-IRQ**: **0 fires** across the entire run. No DMA completion
  IRQs ever raised — falsifies hypothesis (a) from part 2.

So the audio peripherals (Timer 0, DMA1, DMA2, FIFO_A, FIFO_B) are all
running correctly. The cascade trigger is NOT a DMA/Timer accuracy
bug — those run at the exactly right rate.

### Layout discovery: TWO buffers in IWRAM

* **0x03002930..0x03002F2F** (1536 bytes, 192 × 8-byte slots): the
  buffer that the mixer at `0x030031AC` writes to. **This is the one
  that overflows.** Each 8-byte slot looks like a channel state /
  voice entry (`a0 00 00 00 <varying> 12 00 00`) — NOT raw audio.
* **0x03004E80..0x03004F00** (128 bytes): the actual DMA1 audio output
  ring buffer. Separate from above. Cycles continuously.

So the mixer's "buffer overflow" isn't an audio-output overflow — it's
an intermediate **channel / voice state table** overflow. The DMA1
output buffer is healthy and being consumed by the DMA at the right
rate.

### Reframed problem

The mixer at `0x030031AC` fills a 192-entry state table at `0x2930..0x2F2F`
per frame. Our emulator runs the mixer ~33 batches × 8 entries = ~264
entries per frame, OVERFLOWING the 192-entry table. Real FE7 must
produce ≤192 entries/frame.

The mixer is called from FE7's HBlank IRQ subhandler at ROM `0x080BB354`
(verified via the dispatch table at IWRAM `0x2924`). `DISABLE_HBLANK_IRQ=1`
removes the trigger → game boots.

**The most-likely-correct hypothesis**: the HBlank subhandler should
conditionally skip mixer batches based on some state (e.g., "has the
state table already been processed by another routine in this frame?")
that our emulator gets wrong. OR our HBlank subhandler runs more
instructions than real GBA per call, allowing more mixer batches to
fit in the IRQ-disabled window.

Tools added this round:

* `DMA_FIRE_TRACE=1` extended to include cycle stamp, irq_enable
  status, SAD/DAD, and now covers DMA2 as well as DMA1. Lives in
  `gba-core/src/bus/mod.rs`.
* `TIMER_TRACE=1` env var — logs Timer 0 and Timer 1 overflows with
  cyc, count, IRQ-enable, reload, prescaler. Lives in
  `gba-core/src/timer.rs`.
* `DMA_IRQ_TRACE=1` env var — logs every DMA-completion IRQ
  request (DMA0..DMA3). Cycle-stamped. Lives in `gba-core/src/lib.rs`.

### Where to pick up

1. Disassemble FE7 ROM at `0x080BB354` (the HBlank subhandler) and
   `0x080043A0` (the mixer caller) to find what gates the mixer call
   rate. The gate is probably a counter or a buffer-position check.

2. If the gate involves the DMA1 SAD (e.g., "skip mixer if SAD has
   wrapped within the current frame"), that points at our DMA1 SAD
   re-anchor logic at VBlank possibly resetting too early.

3. Alternative: count mixer-batches-per-frame in a HEALTHY frame
   (e.g., cyc≈12.9M) vs the CASCADE frame (cyc≈31.1M). If healthy
   frames also have ~33 batches/frame, the bug is something else
   (corrupted state lasting across frames). If healthy frames have
   ~24 batches/frame, the cascade frame is uniquely abnormal and we
   need to find what changes.

## Update 2026-05-24 part 4: HBlank → mixer chain is INDIRECT

Static disassembly of the HBlank subhandler at ROM `0x080BB354` shows
it does per-line BG/palette effects via a scanline counter (R4 = 0..159
cyclic, wrapped at 160 = `VISIBLE_LINES`). It DOES NOT directly call
the mixer.

The mixer's caller chain is:

* Mixer at IWRAM `0x030031AC` is invoked via a callback table at IWRAM
  `0x03002920` (which stores `0x030031AC` as the mixer's function pointer).
* A thunk at ROM `0x080043A8..0x080043AC` loads that pointer and BLs to
  the THUMB→ARM dispatcher at `0x080BFC5C` (a `BX R4` table). The dispatcher
  swaps to ARM mode and invokes the mixer.
* The thunk is invoked from FE7's audio engine main loop (TBD — not yet
  located).

So the HBlank → mixer link is INDIRECT. Most plausible mechanism:

> With HBlank IRQ enabled, the CPU wakes ~228 times per frame (instead
> of just at VBlank if disabled). Each wakeup gives user code a slice
> of CPU time before halt; cumulatively the audio engine main loop
> runs many more times per frame → calls mixer more times → state
> table at 0x2930 overflows.

This is consistent with `DISABLE_HBLANK_IRQ=1` fixing the boot.

Real FE7 must have audio-engine throttle logic (e.g., "if mixer was
already called this frame, skip" or "if buffer >= N entries, skip")
that prevents the per-wake-up mixer invocations from accumulating.

### Where to pick up next (carry-forward)

1. **Find the audio engine main loop**: search ROM for BL references
   to the thunk at `0x080043A8`. Likely a thumb instruction with offset
   computing to 0x80043A8.

2. **Static-disassemble the loop**: identify what counter / state / IRQ-
   correlated variable it consults before calling the mixer thunk. Most
   likely candidates:
   - Frame counter (incremented at VBlank)
   - Sound buffer write position vs read position (DMA1 SAD)
   - A `samples_remaining_this_frame` counter

3. **Verify our state matches real GBA at that decision point**: most
   likely our scanline counter at the literal pool target of `0x080BB354`
   (i.e., wherever R4 is stored) advances differently. The wrap at 160
   suggests it's keyed to visible-vs-vblank lines; if our HBlank fires
   on VBlank lines (160..227) and real GBA only fires HBlank on visible
   lines, that'd give us ~68 extra wakeups per frame — close to the
   factor we observe.

4. **Sanity probe**: count HBlank IRQ entries split by `vc < 160` vs
   `vc >= 160` over a frame. If we deliver HBlank IRQs during VBlank
   that real GBA doesn't, fixing that gate should fix FE7 without
   requiring `DISABLE_HBLANK_IRQ=1`.

## Update 2026-05-24 part 5: full audio call chain mapped

Used `INSTR_TRACE_RING=1 TRACE_FREEZE_PC=0x030031AC` to freeze the trace
ring at the first mixer entry. The 32K-instruction tail shows the
COMPLETE call chain:

```
0x0801529C  audio engine MAIN function (tick-list entry; not BL'd, called
            via function pointer — appears in 9 ROM tables as 0x0801529D)
  ↓ BL 0x6A64 (with R0=0)
0x08006A64  channel-loop function:
              PUSH {R4, LR}
              R4 = R0*16 + 0x0202A48C  (channel array base)
              if R4=0 return
              loop:
                R2 = [R4+12]            (channel.field_12 = sample data ptr)
                if R2=0 → skip
                R0 = signed [R4+4]
                R1 = signed [R4+6]
                R3 = ushort [R4+8]
                BL 0x08004388             ← THUNK
                R4 = [R4+0]             (next node)
                if R4 != 0 → loop
              POP {R4, R0}; BX R0
  ↓ BL 0x08004388
0x08004388  THUNK (one of a family at 0x4338 / 0x4360 / 0x4388 / 0x43B4):
              PUSH {R4, R7, LR}; SUB SP #16; MOV R7,SP
              save args (R0..R3) to stack
              R0 = literal = 0x03002920
              restore R1, R2, R3 from stack
              R4 = [R0] = 0x030031AC  (mixer function pointer from RAM)
              R0 = [R7] (restore R0)
              BL 0x080BFC5C            ← DISPATCHER
              ... return ...
  ↓ BL 0x080BFC5C
0x080BFC5C  DISPATCHER (a BX-table; 0x4720 = BX R4)
              BX R4   →   0x030031AC (the M4A mixer in IWRAM)
```

So the M4A architecture is:
* A **tick-list entry** at `0x0801529C` is called per audio-engine-update
  step.
* It calls a **channel iterator** at `0x08006A64` twice (with `R0=0` and
  `R0=13` — likely separate channel sets, MUSIC vs SFX).
* The iterator walks a **linked list of channels** at `[0x0202A48C + R0*16]`.
* For each active channel (`channel.field_12 != 0`), it calls a thunk
  that resolves to the mixer at `0x030031AC` via the pointer table at
  `0x03002920`.

This explains the call rate: mixer calls per frame =
`(audio_engine_main_calls_per_frame) × (num_active_channels)`.

In our emulator we see ~33 batches/frame × ~8 calls/batch ≈ 264 entries
in the 0x2930 buffer → overflow at 192.

Real FE7 must have:
* (audio_engine_main called less often per frame), AND/OR
* (fewer active channels in the list).

### Where to pick up next next

(Investigated below — see part 6.)

Tools added this round:
* No new env vars. Used existing `INSTR_TRACE_RING=1` + `TRACE_FREEZE_PC=0xADDR`
  + R key for ring dump.

ROM disassembly notes:
* Mixer entry: `0x030031AC` (ARM)
* HBlank subhandler: `0x080BB354` (THUMB) — per-line BG/palette effects
* Audio engine main: `0x0801529C` (THUMB)
* Channel iterator: `0x08006A64` (THUMB)
* Thunk: `0x08004388` (THUMB, plus siblings at 0x4338, 0x4360, 0x43B4)
* Dispatcher: `0x080BFC5C` (THUMB, BX-table)
* Mixer function-pointer slot: `0x03002920` (RAM, holds 0x030031AC)
* HBlank subhandler pointer slot: `0x03002924` (RAM, holds 0x080BB355)
* Mixer write-pointer: `0x03002F34` (RAM, advances 0x2930→0x2F30 per frame)
* Channel array base: `0x0202A48C` (EWRAM)

## Update 2026-05-24 part 6: caller is a gated wrapper, gate at 0x02024C70

Added probe at `PC=0x0801529C` (audio_engine_main entry) under
`FE7_PROBE=1`. Results across a 30M-cycle run (~107 frames):

* **934 audio_engine_main fires** ≈ **8.7 per frame**. Real FE7 should be
  ~1 per frame.
* **Channel count is fine**: only 2 active channels (heads at
  `0x0202A49C` and `0x0202A4AC`).
* **Single caller**: LR=`0x080019E9` everywhere. So one site is
  responsible for all the over-firing.
* **Period between consecutive fires**:
  - 2465-2466 cycles (~2 scanlines, by far the most common)
  - 32091-32094 cycles (~26 scanlines)
  - 34525-34530 cycles (~28 scanlines)

The 2465-cycle period is **exactly 2×HBlank cycle period (2×1232)**.
This is the strongest signal yet: with HBlank IRQ enabled, FE7 wakes
the CPU every line; the caller at 0x080019E4 runs and finds the gate
flag set; audio_main is invoked.

### Caller disassembly at ROM 0x080019D4

```
0x19D4: 0x90b5      PUSH {R4, R7}
0x19D6: 0x6f46      MOV R7, SP
0x19D8: 0x4805      LDR R0, [PC, #20]   ; R0 = 0x02024C70 (literal)
0x19DA: 0x6801      LDR R1, [R0]        ; R1 = [0x02024C70]  ← gate
0x19DC: 0x2900      CMP R1, #0
0x19DE: 0xD003      BEQ +6 → 0x19E8     ; skip if gate clear
0x19E0: 0x4803      LDR R0, [PC, #12]   ; R0 = 0x02024C70 (same literal)
0x19E2: 0x6804      LDR R4, [R0]        ; R4 = [0x02024C70]  (audio_main arg)
0x19E4: 0xBEF0/0xF93A  BL audio_engine_main
0x19E8: POP/BX_LR
```

**The audio gate variable is at EWRAM `0x02024C70`.** When non-zero,
audio_main runs with that value as its R0 argument. We need to find
who writes this variable.

Most likely candidates for the writer:
* VBlank handler / interrupt service routine — increments per frame
  signal that audio is due
* M4A's timer-driven update path — should write once per audio frame

If our emulator writes this variable too often, that explains the 8.7×
overshoot perfectly. Likely scenarios:
1. We deliver some IRQ (HBlank? Timer?) that writes the gate, but real
   GBA gates the IRQ differently → we write too often.
2. Our DMA1 FIFO underrun handling writes the gate (or a related state
   variable) too often.

### Where to pick up next

1. Run with `MEM_WATCH=1 MEM_WATCH_LO=0x02024C70 MEM_WATCH_HI=0x02024C74`
   for a few seconds. The log will show every write to the gate variable
   with PC + cyc. Count writes per frame: expected 1, observed N.

2. The writer's PC will likely point at an IRQ handler / VBlank service.
   Static-disassemble it and compare what triggers it on our emulator
   vs the spec.
