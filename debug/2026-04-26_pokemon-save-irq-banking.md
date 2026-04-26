# Pokémon Emerald in-game save: nested-IRQ banking corruption

Date: 2026-04-26
Status: **In progress** (root cause identified, fix pending)

## Symptom

When the user uses the in-game Save menu in Pokémon Emerald:
1. The "Don't turn off the power" prompt appears.
2. The game freezes with audio reduced to a constant tone (vblank IRQ
   isn't reaching M4A any more).
3. Eventually (or on close + restart) the title screen shows only "New
   Game" — Pokémon detected the save as corrupt and erased it.

Save states (`[`/`]` keys, full memory snapshot) work fine — only the
in-ROM Flash save path is broken.

## Investigation log

The full investigation is detailed across multiple traces. Headline
findings, in the order discovered:

### 1. Save data DOES land in Flash (partially)

Flash trace (`FLASH_TRACE=1`) showed Pokémon issuing chip-ID query →
sector erases → byte writes. Each byte write was followed by a verify
read returning the just-written value. So the flash command/data
state machine is correct.

### 2. Read width matters: 8-bit-only bus

Earlier code dispatched 16-bit and 32-bit reads of the SRAM/Flash
region (`0x0E000000`–`0x0FFFFFFF`) by reading multiple sequential
bytes. Real GBA cartridge SRAM/Flash is on an 8-bit bus and any
wider read should broadcast the same byte to all positions of the
result. Fixed in `bus/mod.rs::read16` and `read32`.

This didn't fix the hang on its own.

### 3. Chip ID

We default 128 KB Flash to Macronix `(0xC2, 0x09)` (most common in
Pokémon cartridges) instead of Sanyo `(0x62, 0x13)`. Pokémon's flash
driver supports both, so this isn't load-bearing here, but matches
what real cartridges most often report.

### 4. The actual hang: nested-IRQ banking corruption

Using `INSTR_TRACE_RING=1` (a 256-entry ring buffer that freezes when
PC enters unmapped memory), we captured the precise instruction
sequence before the escape. The user IRQ handler at IWRAM
`0x03002750` runs this pattern:

```
0x03002764  STMFD SP!, {R0-R3, LR}     ; SP_irq: 0x03007F88 → 0x03007F74
            ... process IRQ flags ...
0x03002860  MSR  CPSR, R3              ; R3 = 0x4000001F: switch to System,
                                       ;   ALSO RE-ENABLES IRQs!
            ... run flash save code in System mode ...
0x0300288C  MSR  CPSR, R3              ; R3 = 0x40000092: switch back to IRQ
0x03002890  LDMFD SP!, {R0-R3, R14}    ; expected SP_irq=0x03007F74
                                       ; observed SP_irq=0x03007F88 ← WRONG
0x0300289C  MSR  SPSR, R0              ; R0=0 (popped from wrong location)
                                       ;   → SPSR_irq corrupted to 0
0x030028A0  BX   LR                    ; LR=0x28: return to BIOS stub
0x00000028  LDMFD SP!, {R0-R3, R12, LR} ; pops zeros from BIOS stack
0x0000002C  SUBS PC, LR, #4            ; LR=0 → PC=0xFFFFFFFC,
                                       ;   CPSR=SPSR_irq=0 → mode=User
                                       ; CPU walks through unmapped memory
```

The handler pushed at `SP_irq = 0x03007F88`, leaving data at
`[0x03007F74..0x03007F84]`. By POP time, `SP_irq` is back to
`0x03007F88` — off by exactly 20 bytes (5 registers — the user
handler's push size). LDMFD reads from the **wrong addresses** and
gets garbage.

### Root cause hypothesis

Pokémon enables IRQs again in step 3 (`R3 = 0x4000001F`, bit 7 = 0
clears the IRQ-disable bit). Nested VBlank IRQs can fire during the
System-mode portion of the user handler. Each nested IRQ entry/exit
goes through `switch_mode` → updates `banked.sp[Irq]`. Somewhere in
that chain we save the wrong value back, so when the original
handler eventually switches `System → Irq` at `0x0300288C`, it
restores `SP_irq` to the BIOS-push level instead of the user-push
level.

This is the same *family* of bug as the MSR banking issue we fixed
during the audio investigation (see
`2026-04-24_arm-msr-banking.md`), but in a different code path.

## Fixes applied so far

- **`bus/mod.rs`**: 16-bit and 32-bit reads of SRAM/Flash region now
  broadcast the single byte across all positions of the result
  (8-bit bus emulation).
- **`backup/flash.rs`**: 128 KB chip ID changed from Sanyo to Macronix
  to match the most common Pokémon cartridge.

## Diagnostic infrastructure added (left in place)

- **`FLASH_TRACE=1`** + **`FLASH_TRACE_READS=1`**: log every flash
  command/data write and (optionally) every read into `/tmp/flash.log`
  with explicit flush. Survives SDL stderr capture on macOS.
- **`IRQ_TRACE=1`**: print every IRQ entry and every CPSR-restore
  exception return.
- **`INSTR_TRACE_RING=1`**: ring buffer of last 256 CPU instructions,
  freezes the moment PC enters unmapped memory. Frontend
  auto-detects, dumps, and exits.
- **`DUMP_PC=1`**: per-frame PC sampler in the frontend with mode,
  IRQ state, R0–R3, SP, LR.

## Open questions

- Where exactly in `switch_mode` does the wrong `banked.sp[Irq]`
  value come from? Need an `eprintln!` of `banked.sp[Irq]` before
  and after each `switch_mode` call to pin down.
- Is it specifically the nested-IRQ path, or does the bug exist in
  any IRQ-mode → System-mode → IRQ-mode round trip even without
  nesting? We should be able to reproduce headlessly by manually
  invoking the same MSR sequence.

## Next steps

1. Add explicit `banked.sp[Irq]` tracing inside `switch_mode` so we
   can see when it diverges from the expected value.
2. Construct a unit test that reproduces the round-trip:
   `IRQ-mode push X → switch System → switch IRQ → expect SP unchanged`.
3. Once fixed, expect this to also improve audio (the bug may explain
   some of the residual noise from the audio fix — same mechanism
   could subtly corrupt other state).

## Related

- [`2026-04-24_arm-msr-banking.md`](2026-04-24_arm-msr-banking.md) —
  earlier MSR/banking bug we fixed.
- [`concepts/dma-registers.md`](concepts/dma-registers.md) — banking
  concept generalises here too (programmed value vs runtime cursor).
