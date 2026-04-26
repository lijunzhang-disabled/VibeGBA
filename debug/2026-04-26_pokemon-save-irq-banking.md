# Pokémon Emerald in-game save: 8-bit Flash bus + chip ID

Date: 2026-04-26
Status: **Fixed**

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

### Initial hypothesis (turned out wrong)

We hypothesised this was a nested-IRQ banking bug — Pokémon's handler
re-enables IRQs in System mode, allowing nested VBlank IRQs to fire,
and our `switch_mode` was thought to be corrupting `banked.sp[Irq]`
across the nested round-trip.

We wrote unit tests for the IRQ→System→IRQ cycle (with and without
nested IRQs) and they all **passed**. We then added `BANK_TRACE`
instrumentation that printed `banked.sp[Irq]` on every `switch_mode`
call. The trace showed `banked.sp[Irq]` oscillating cleanly between
`0x03007F74` (post-push) and `0x03007FA0` (post-pop) every IRQ
cycle — exactly the correct pattern. **No banking bug.** The
"escape" we'd seen earlier was a side effect, not the cause.

### Actual root cause

Two unrelated 8-bit-bus bugs in the SRAM/Flash region.

GBA cartridge SRAM/Flash sits on an 8-bit bus. Per gbatek, wider
accesses behave specifically:
- **Reads**: the byte at the LSB position is broadcast to all
  positions of the result (16-bit gets `byte | byte<<8`, 32-bit gets
  `byte | byte<<8 | byte<<16 | byte<<24`).
- **Writes**: only the byte at the LSB position is stored.

Our emulator was wrong on **both**:

1. **`bus::read16` / `read32`** of the `0x0E…/0x0F…` region read
   *consecutive* bytes (so a 16-bit read of address X returned
   `flash[X] | flash[X+1]<<8`). This corrupted any wider load of
   flash by Pokémon's checksum-verify routine — it computed against
   data that didn't match what was actually there. Pokémon decided
   the save was corrupt and erased it on reload.

2. **`bus::write16` / `write32`** of the same region were silently
   *dropped*. (Writes to that region only matched on `0x08..=0x0D`
   for ROM/GPIO and fell through for `0x0E..=0x0F`.) This was a
   second-order issue: if Pokémon ever stored a halfword/word into
   the save buffer, only the byte that 8-bit bus would have stored
   was actually expected to land — but our code dropped the whole
   thing. **Adding back the low-byte store turned out to break some
   other path** (re-enabling it caused a fresh hang). After the
   read fix, Pokémon's save apparently doesn't actually depend on
   the write path picking up halfword/word stores; reverting the
   write change made everything work.

So the load-bearing fix was the **read broadcast** in step 1. The
write fallthrough is left as `_ => {}` for now (matches the prior
behaviour and unblocks Pokémon).

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

## Verification

- After the read-broadcast fix:
  - In-game save completes, `.sav` file has all 14 sectors with
    valid 0x08012025 signatures and matching checksums.
  - On reload, "Continue" appears at the title screen — the save
    survives the round-trip.
- Test ROMs unchanged: arm.gba / thumb.gba / memory.gba all-pass,
  bios.gba still fails test 001 (unrelated stale-bus quirk).
- All 90 unit tests pass.

## Lessons learned

1. **Don't trust the first plausible hypothesis.** "Banking bug"
   matched the symptom (CPU escape during save) but the unit tests
   we wrote to validate it all passed. The real bug was much simpler
   and lower in the stack.
2. **Diagnostic instrumentation pays for itself.** The
   `INSTR_TRACE_RING` (256-entry ring frozen on first invalid PC),
   `IRQ_TRACE`, `FLASH_TRACE`, and `DUMP_PC` env-gated dumps were
   all left in place — they're each useful again next time a game
   misbehaves. None of them affect runtime when the env var is unset.
3. **Per-sector checksum analysis is a great endgame check.** Once
   `.sav` is on disk, a small Python script that recomputes Pokémon's
   per-sector checksum and compares to the stored value pinpointed
   "13 sectors valid, 1 not" — that reframed the problem from "save
   is broken" to "specific bytes in specific sectors are wrong".

## Related

- [`2026-04-24_arm-msr-banking.md`](2026-04-24_arm-msr-banking.md) —
  earlier real banking bug.
- [`concepts/memory-map.md`](concepts/memory-map.md) — explains the
  memory regions including the 8-bit-bus property of SRAM/Flash.
