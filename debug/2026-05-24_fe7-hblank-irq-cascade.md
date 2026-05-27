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

## Update 2026-05-25 part 7: HLE SWI 0x04/0x05 fix — partial; still WIP

### What was missing

Our HLE BIOS handled SWI 0x04 (IntrWait) and SWI 0x05 (VBlankIntrWait)
incorrectly: they just set `cpu.halted = true` and returned, letting the
generic halt-wake on **any** IRQ clear `halted`. So with HBlank IRQ
enabled, SWI 0x05 would return on the first HBlank — completely defeating
its purpose.

This was the proximate cause of FE7's audio engine running 8.7×/frame
instead of 1×/frame, leading to the M4A channel-state-table overflow.

### The fix (in tree)

1. **handle_interrupt**: OR the pending `IE & IF` bits into the
   BIOS_IF mirror at IWRAM `0x03007FF8` before delivering the IRQ.
   Per GBATEK, this is what real BIOS does, so user IRQ handlers
   (which typically don't update BIOS_IF) work with SWI 0x04/0x05.
2. **SWI 0x04/0x05 HLE**: store `irq_flags` in `cpu.intrwait_mask`,
   force `IME=1` (per GBATEK), then halt.
3. **CPU step**: at the start of each step (in non-IRQ, non-Supervisor
   mode), if `intrwait_mask != 0` and the BIOS_IF mirror has a matching
   bit, clear it and exit the wait. Otherwise re-halt. This emulates
   the BIOS's `loop { HALT; if BIOS_IF & mask: return; }` semantics.

### Current state

* Pokemon Emerald: still boots and runs (regression-free).
* FE7: progresses past 3 SWI 0x05 calls successfully (verified via
  `INTRWAIT_TRACE=1` — see init phase with `ie=0x2001..0x2003`,
  `dispstat=0x0009..0x0018`). After the third SWI 0x05, the game does
  NOT call SWI 0x05 again — the main loop uses a different mechanism
  to wait for VBlank, so the SWI 0x05 fix doesn't gate the cascade.
* User reports the screen stays black + sustained "beee" audio. The
  SWI trace shows **522 SWI 0xFF calls** (invalid SWI = garbage memory
  execution) over ~30 seconds, indicating PC corruption somewhere.
* Bisection (via `INTRWAIT_DISABLE` / `BIOS_IF_DISABLE` env vars,
  since removed): only-BIOS_IF behaves identically to the original
  (boot anim → cascade). Only-intrwait hangs immediately because
  BIOS_IF never gets updated. So both halves are needed for the SWI
  0x05 path to work — but together they introduce the PC-corruption
  symptom whose cause is not yet identified.

### Bug remaining to find

PC corruption (522 SWI 0xFF in a 30-second run) after the fix is the
mystery. Hypotheses to investigate:

1. **IRQ-return PC computed wrong** when the IRQ is delivered while
   `cpu.halted` was just-set by our intrwait check. handle_interrupt's
   `return_addr = self.regs[15] (THUMB) or self.regs[15] - 4 (ARM)`
   could be one off if pipeline_flushed state isn't right.

2. **Mode-transition handling**: SUBS PC, LR, #4 from the BIOS IRQ
   epilogue restores CPSR from SPSR and sets PC. If our implementation
   doesn't fully restore the saved CPSR (mode, T-bit, IRQ flag), the
   return could land in wrong mode → mis-decoded instructions.

3. **FE7's user IRQ handler also writes BIOS_IF**, conflicting with
   our handle_interrupt write. Static disassembly of FE7's handler at
   IWRAM 0x03003950 didn't find a BIOS_IF write, but it might be
   indirect (via a function call).

### Workaround unchanged

`DISABLE_HBLANK_IRQ=1` still boots FE7 (avoids the cascade by skipping
HBlank IRQs entirely).

### Where to pick up

1. Add a probe at the SUBS PC, LR, #4 instruction (BIOS 0x2C) that
   logs the saved CPSR, LR, the computed return PC, and the popped
   register values. Run for ~1 second and check whether any return
   lands in invalid memory.

2. Add a "first invalid PC" probe that triggers TRACE_FREEZE_PC on the
   first PC outside ROM/IWRAM/EWRAM. The trace ring will show the
   instructions leading to the escape.

3. Check whether FE7's user IRQ handler updates BIOS_IF in a way we
   missed — disassemble all paths after the ACK at IWRAM 0x03003A20.

## Update 2026-05-25 part 8: the SWI 0x05 fix was a dead end

Implemented the IntrWait HLE fix per part 7. Confirmed it's correct
according to GBATEK spec (BIOS_IF mirror, IME=1 forcing, re-halt loop),
and Pokemon Emerald continues to work. But:

**FE7 only calls SWI 0x05 three times during init, then never again.**
The main game loop at ROM `0x08000AEE` does:
```
loop:
  BL audio_wrapper     ; → audio_engine_main → channel iterator → mixer
  BL other_call        ; (0x08002CF4)
  B loop
```
`other_call` does not invoke SWI 0x02/0x05 either. So the main loop runs
without HALTing, calling audio_engine_main as fast as the CPU can iterate.
The SWI 0x05 fix only affects init, not the runtime cascade.

Verified with trace-ring escape detection (INSTR_TRACE_RING=1, tightened
pc_in_valid_code to reject VRAM mirrors): the CPU ends up at PC=0xFFFFFFFC
after SUBS PC, LR, #4 pops LR=0 from a corrupted IRQ-stack frame. The
trace tail shows the IRQ-handler-nested-in-IRQ-handler cascade pattern
we already identified — exactly the same root mechanism (M4A mixer
buffer overflow → IRQ handler self-corruption) as before.

Reverted SWI 0x04/0x05 changes since they don't help. Kept:
* Tightened `pc_in_valid_code` (rejects VRAM, exclusive of 0x06).
* Auto-dump of trace ring on freeze (eprintln directly from push_trace
  rather than waiting for frontend's frame-loop check).
* `GLOBAL_CYCLES` atomic (used by other probes).

### Where to pick up

The real root cause is that FE7's main loop runs unbounded per frame
because it doesn't use any HALT mechanism. To fix the cascade without
the `DISABLE_HBLANK_IRQ=1` workaround, we need one of:

1. **Find how `other_call` (0x08002CF4) waits for VBlank**. Disassemble
   it. If it busy-polls DISPSTAT bit 0 (VBlank flag), then the bug is
   that the loop "succeeds" too quickly — maybe our DISPSTAT bit 0 stays
   set longer than real hardware, or the loop has additional gates we're
   missing.

2. **Possible HBlank-counter throttle in the user IRQ handler**. The
   HBlank subhandler at ROM `0x080BB354` increments a scanline counter
   (R4 = 0..159 cyclic). FE7 may use this counter as a "VBlank gate" —
   e.g., the main loop polls a state variable that the HBlank handler
   sets at line 0 or 160. If our HBlank handler triggers wrap differently
   than real hardware, the gate fires more often.

3. **DMA1 sound underrun**. FE7 may rely on a "FIFO underrun → reset
   mixer state" cycle that we don't model. If real FE7 detects buffer
   high-water and stops calling audio_main, we don't.

The workaround `DISABLE_HBLANK_IRQ=1` boots FE7 by avoiding the cascade
entirely. To preserve correct HBlank behavior for other games, that's
the production workaround until the cascade itself is properly fixed.

## Update 2026-05-26 part 9: cycle profiler reveals user/IRQ breakdown

Implemented CYCLE_PROFILE=1 env var. Per-frame breakdown for FE7 intro
(pre-cascade) in our emulator:
* user: ~265K cycles/frame (94%)
* irq:  ~14K cycles/frame (5%)
* halt: ~8K cycles/frame (3%)
* Total: ~286K (matches expected 280896, within scheduler rounding)

Each HBlank IRQ averages 60 cycles in our IRQ-mode profile. The FE7
handler switches to System mode at PC=0x03003A30 (MSR), so the
subhandler's cycles count as "user" not "IRQ" in the profile. The IRQ
column is just the dispatch + ACK + cleanup.

Cascade signature: post-Start press, user cycles freeze while IRQ
cycles climb monotonically. CPU enters nested IRQ mode and never
returns (= the M4A buffer corruption pattern we identified earlier).

### Status

* All cycle accounting matches GBATEK spec:
  - ROM 32-bit non-seq: 2 + N + S cycles ✓
  - ROM 16-bit seq: 1 + S cycles ✓
  - ARM exception entry: 3 cycles ✓
  - Shift-by-register: +1 internal cycle ✓
  - SUBS PC, LR, #4: +2 cycles ✓
* Yet empirically FE7 needs ROM_SLOW_MULT=3 to avoid cascade.
* Pokemon Emerald: still slight audio noise.

### Two remaining hypotheses

1. **Some non-ROM cycle source is missing**. E.g., DMA cycles aren't
   advancing the scheduler properly (currently they accumulate via the
   bus.mem_access_cycles accumulator and get attributed to the next CPU
   instruction, but the scheduler never sees them as "DMA blocking
   time").

2. **Cycle accounting is "right" but FE7's audio engine takes a
   different code path on real GBA**. Maybe a function called from
   audio_main does buffer-draining work that we don't trigger, so our
   intermediate slot buffer fills monotonically while real GBA's gets
   consumed by an audio output path we're not running.

### Workaround

`ROM_SLOW_MULT=3` env var. Use this to play FE7 until residual
accuracy gap is resolved.

## Update 2026-05-26 part 10: real cascade trigger is slot rate per iter

Instrumented the main loop at PC=0x08000AEE (the loop-back point of FE7's
main game loop) to measure cycles AND slots written per iteration:

```
n=100 cyc_delta=6119  ptr=0x03002930 advance=0x0   slots~0   (silent)
n=200 cyc_delta=80432 ptr=0x03002930 advance=0x0   slots~0
n=300 cyc_delta=114772 ptr=0x03002930 advance=0x0  slots~0
n=400 cyc_delta=154538 ptr=0x03002988 advance=0x58 slots~11  (music starts)
n=500 cyc_delta=184115 ptr=0x03002988 advance=0x58 slots~11
n=600 cyc_delta=88891  ptr=0x030029E0 advance=0x58 slots~11
n=700 cyc_delta=39530  ptr=0x03002DF8 advance=0x198 slots~51  (heavy audio)
n=800 cyc_delta=42015  ptr=0x03002EF0 advance=0x1B0 slots~54
n=900 cyc_delta=42036  ptr=0x03002C90 advance=0x1B0 slots~54  (cascading)
```

**Key insight**: each iteration writes **51-54 slots** in the cascade-leading
phase. At ~3.5 iter/frame, that's 180+ slots/frame. The 192-slot buffer at
IWRAM 0x2930..0x2F30 fills in less than 4 iterations. The once-per-frame
reset (VBlank handler at pc=0x08003310) doesn't fire often enough to stay
ahead of writes.

**So the cascade isn't a cycle-counting issue**. Our iter count is roughly
correct (~3.5/frame). The issue is the audio engine generates too many
slots per iter compared to real GBA.

### Why real GBA writes fewer slots per iter

Hypotheses:
1. **Linked-list pruning**: real FE7's channel iterator only walks ACTIVE
   channels each iter, dynamically rebuilt. Our trace shows 2 channel
   list heads at 0x0202A48C/0x0202A49C, but the linked list may grow to
   8+ entries during gameplay if we don't run the prune step.
2. **Per-iter sample budget**: one of audio_main's 10 BL targets may cap
   total samples mixed per iter (e.g., based on buffer space remaining).
   We aren't running that gate.
3. **Channel state mismatch**: real FE7's audio engine state may keep
   channels in "ready" vs "playing" vs "done" states. We may run all of
   them as "playing", causing more samples than real.

To diagnose: instrument each BL target in audio_main (10 of them at
0x080152A2..0x080152EC) to log entry. Compare to expected behavior.

### Workaround in place

`ROM_SLOW_MULT=3` env var slows ROM accesses by 3x. With slower CPU,
iter rate drops from 3.5/frame to ~1.2/frame. At 51 slots/iter × 1.2
= 61 slots/frame, well under the 192-slot buffer.

This is a TIMING workaround for what is fundamentally an AUDIO ENGINE
STATE issue. The real fix requires understanding why our audio engine
produces ~6× more channel work than real GBA.

## Update 2026-05-26 part 11: gap localized to audio_wrapper cycle cost

Split each main-loop iteration (ROM 0x08000AEE = BL audio_wrapper,
0x08000AF2 = BL other_call) into per-call cycle costs:

```
[LOOP] audio_avg=58000..100000  other_avg=227  ptr=0x0300_29xx..2E58
```

Findings:
* **other_call (0x08002CF4) is a ~227-cycle no-op gate.** It calls
  0x08002CA4 which returns 0, so other_call returns immediately. The
  game's rendering/logic is NOT in the main loop — it runs in IRQ
  handlers. So the main loop is effectively `loop { audio_wrapper(); }`.
* Therefore the audio_wrapper CALL RATE = available_cycles /
  audio_wrapper_cost. Nothing else fills the frame.
* At the title screen, the mixer buffer pointer oscillates 0x2930..0x2E58
  — well under the 0x2F30 overflow point. No cascade there. The cascade
  is Start-only (heavier audio: more samples mixed per call).

**Conclusion**: the residual 3× gap (FE7 needs ROM_SLOW_MULT=3) is
entirely in audio_wrapper's cycle cost. Real GBA's audio_wrapper takes
~3× more cycles than ours, so real GBA calls it ~1.2×/frame vs our
~3.5×/frame. audio_wrapper is ROM-heavy:
  * THUMB code in ROM: audio_main (0x0801529C), channel iterator
    (0x08006A64), thunks (0x08004388) — instruction fetches from ROM.
  * The mixer (IWRAM 0x030031AC) reads sample data from ROM (R2 =
    0x08xxxxxx) with a 6-byte stride → every sample read is a
    non-sequential ROM access.

So the remaining work is purely "audio-path ROM access cycles are
undercounted." Candidates:
1. ROM wait-state LUT semantics (are the {4,3,2,8}/{2,1} values TOTAL
   access cycles or wait-on-top-of-1? If total, our base-1 + extra-LUT
   over-counts by 1; if wait-on-top, our model is right). Needs a
   cross-check against a reference emulator (mGBA/NBA) before touching —
   getting it wrong silently breaks ALL game timing.
2. The mixer's per-sample inner loop may do more ROM accesses than we
   model (e.g., interpolation reads 2 samples, or the loop has more
   iterations on real hardware).
3. We may undercount instruction COUNT in the mixer (a code path or
   loop-trip-count difference), not just per-access cost.

Next concrete diagnostic: instrument add_mem_cycles to tally, over one
audio_wrapper call, accesses-per-region and total extra cycles. That
shows whether ROM-read cycles dominate and whether the proportion is
plausible — pinpointing undercount vs miscount.

Workaround remains: ROM_SLOW_MULT=3.

## Update 2026-05-26 part 12: cycle-accurate prefetch — proves cascade is FUNCTIONAL

Implemented a stateful GamePak prefetch buffer (WAITCNT bit 14):
* 8-halfword buffer, fills one halfword per sequential-access time during
  idle (non-ROM) cycles.
* Sequential ROM code fetch: buffer hit = 1 cycle; in-flight = stall the
  remaining fill; miss = flush + full N.
* Non-sequential code fetch OR any ROM data access flushes the buffer.
* New bus.fetch16/fetch32 (used by the pipeline) route ROM fetches through
  the prefetcher; ROM data accesses (read/write) flush it.

Result on FE7 (FE7_PROBE [LOOP] at the title):
* cyc_per_instr DROPPED 3.5 → 2.75 — audio_wrapper got FASTER, not slower.
* The buffer pointer goes wild (0x03000C1E, 0x03004F7D…) — FE7 cascades
  EARLIER with the (correct) prefetch model.

**This definitively proves the FE7 cascade is NOT a CPU-timing problem.**
Real GBA runs the audio engine even faster than our old model (prefetch
makes sequential ROM fetches cheap), yet real hardware does not overflow.
So the overflow is prevented by something FUNCTIONAL in MP2K's audio
buffer management that we get wrong.

Regression check: Pokemon Emerald still boots and plays normally with the
prefetch model. So the prefetch buffer is kept as a standalone accuracy
improvement (correct GBA behavior), independent of FE7.

### Reframed FE7 root cause (functional, not timing)

Our audio_wrapper does FULL mixing work every call (~55K cycles,
consistently — not occasionally). On real GBA, MP2K's SoundMain mixes a
fixed-size buffer ONCE per sound-frame and early-returns / no-ops the
other calls, gated by a counter in the SOUND_INFO struct that the sound
interrupt (Timer/DMA-driven) advances. We call it back-to-back in the
main loop (0x08000AEE: BL audio_wrapper; BL other_call; B loop — no
halt), and it mixes every time, overflowing the 192-slot buffer.

Candidates for the missing gate (next investigation):
1. MP2K SOUND_INFO `pcmDmaCounter` / `cmd` field: SoundMain checks it and
   only mixes when the sound DMA has consumed the previous buffer. If our
   sound-DMA bookkeeping never advances/updates that field, SoundMain
   mixes every call.
2. A re-entrancy/"already ran this frame" flag set by m4aSoundVSync
   (the 0x08003310 routine) and checked by SoundMain.
3. The sound timer (Timer0) IRQ — we run Timer0 with IRQ disabled; if
   MP2K expects a timer-driven counter advance we don't provide.

Concrete next step: disassemble the first BL target inside audio_main
(0x080152A2 → SoundMain core) and find the early-out condition; then
trace the SOUND_INFO field it reads at runtime and see why ours never
gates.

Workaround unchanged: DISABLE_HBLANK_IRQ=1 boots FE7 (removes the HBlank
wake that lets the main loop run between scanlines). ROM_SLOW_MULT also
mitigates by slowing the call rate.
