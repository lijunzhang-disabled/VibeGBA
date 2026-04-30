# Super Robot Taisen: Original Generation — flash chip-ID gate

Date: 2026-04-30
Status: **Fixed** (commit pending)

## Symptom

Super Robot Taisen: Original Generation (SRTOG, US) ran but stayed on a
black screen indefinitely. CPU was healthy: IRQs delivered every vblank,
main loop iterating, no PC escape, no unhandled SWIs. Just `dispcnt=0`
forever.

## Investigation

Triage steps and what each ruled out:

1. **`DUMP_PC=1`** showed CPU halted at PC=0x080506E8 (the BX LR after
   VBlankIntrWait helper) every frame, with `halt=true`, `IF=0x0001`,
   `IME=true`. → CPU stuck halted with vblank pending.

2. **Halt-wake bug found and fixed (separate, commit 27722c4):** in
   `run_cycles()` the inner loop fast-forwarded the scheduler when
   `cpu.halted` was true and never called `step()`, but `step()` is the
   only place that delivers IRQs and clears `halted`. Real ARM7TDMI
   wakes on `(IE & IF) != 0` independent of IME/CPSR.I. Fix: clear
   `halted` after dispatching scheduler events.

3. After the halt fix, IRQs delivered fine but the screen still stayed
   black. `DISPCNT_TRACE=1` showed the game **never wrote DISPCNT**
   in 11 seconds of running.

4. Disassembly of the main game loop at 0x08000304 showed it dispatches
   on a state ID at `0x02001942`, with separate handlers for states
   0x32, 0x64, 0x6E, 0xA0, 0xBE, 0xC8, 0x12C, 0x190, 0x1F4, 0x3E8,
   0x7D0. State `0xFFFF` (sentinel) routes through several BGT branches
   to a default handler at `0x0800045C` that reads a "next state"
   pointer at `[+6]`; if that's also `-1`, no transition.

5. `MEM_WATCH=1 MEM_WATCH_LO=0x02001940 HI=0x0200194A` confirmed boot
   init writes `0xFFFF` to state_id, prev, and next as a sentinel
   triple. Nothing else writes after init.

6. `MEM_WATCH` on the DISPCNT-shadow address `0x020234D0` showed the
   game's vblank handler IS writing the high byte every frame — but
   always writing `0x00` (no BG/OBJ enables). Tracing the writers
   (PC=0x080053A8) led to a "set BG enable" function called by game
   logic that was never reached because the state machine was stuck.

7. **Critical step:** Re-read the early init at 0x08000270:

   ```
   BL 0x08011A50          ; some setup
   BL 0x0803A570          ; returns 16-bit value into R2
   CMP R2, #0
   BEQ 0x080002F4         ; ← if R2==0: alt path → state_id = 0x32
   MOV R0, #0xFFFF        ; ← R2!=0: state_id = 0xFFFF
   STRH R0, [R1, #2]
   ```

   We were always taking the `R2 != 0` branch, hence the sentinel.
   `BL 0x0803A570` is a thin wrapper around `BL 0x08050BEC`.

8. Disassembling `0x08050BEC`:
   - Sets WAITCNT bottom 2 bits.
   - Calls `0x08050710` which writes the standard chip-ID command
     sequence (`0xAA → 0x5555`, `0x55 → 0x2AAA`, `0x90 → 0x5555`)
     and reads back the 16-bit chip ID from the flash region.
   - Searches a table at `0x087485B8` for an entry whose halfword
     at `+0x28` matches that ID.
   - Returns 0 if found, non-zero if not.

9. The table at `0x087485B8` lists three supported chips:

   | Halfword (LE) | Manufacturer | Device | Part |
   | --- | --- | --- | --- |
   | `0xD4BF` | 0xBF SST | 0xD4 | SST 39VF512 |
   | `0x1B32` | 0x32 Panasonic | 0x1B | MN63F805MNP |
   | `0x1CC2` | 0xC2 Macronix | 0x1C | MX29L010 / MX29L512 |

   Our 64 KB chip-ID was hard-coded to **Atmel AT29LV512 (0x1F, 0x3D)**
   — not in the table.

## Root cause

Our 64 KB Flash chip ID didn't appear in SRTOG's supported-chip table,
so the boot init concluded "save hardware unrecognised" and parked the
state machine on its `0xFFFF` sentinel. From there, no state handler
ever set the DISPCNT shadow, so the vblank handler kept writing
DISPCNT=0 every frame.

The 128 KB chip-ID we report (Macronix MX29L1100B = 0xC2, 0x09) was
already correct for Pokémon Emerald and that title; this only affected
games in the FLASH_V / FLASH512_V / FLASH512 size class.

## Fix

`backup/flash.rs::pick_chip_id` now picks the chip-ID at construction
time:

- Scans the ROM for any of the well-known chip-ID halfwords for the
  size class.
- Returns the first one found, in a preference order that puts the
  most commonly-supported chips first (Panasonic > SST > Macronix >
  Sanyo > Atmel for 64 KB; Macronix variants > Sanyo for 128 KB).
- Falls back to Panasonic (64 KB) / Macronix MX29L1100B (128 KB) if
  no candidate appears in the ROM.

This means each title gets a chip-ID that's actually in its own
supported-chip table (because the table is in the ROM, and the scan
finds entries from that table). SRTOG now boots; Pokémon Emerald
saves still round-trip 5/5.

## Verification

- All 90 unit tests pass (the 64 KB chip-ID test was updated to expect
  Panasonic).
- SRTOG: title screen renders within ~1 second of launching. `dispcnt`
  becomes `0x0300` (BG0+BG1) on the title screen.
- Pokémon Emerald in-game save: still 5/5 PASS (128 KB path unchanged).

## Why we didn't catch this with Pokémon

Pokémon Emerald is 128 KB Flash (FLASH1M_V), not 64 KB. We had picked
Macronix MX29L1100B for 128 KB explicitly back in the Pokémon save
investigation (commit 64b04c0), and that *was* in Pokémon's table.
SRTOG is the first 64 KB-Flash commercial game we tested.

## Related

- `debug/2026-04-26_pokemon-save-irq-banking.md` — original chip-ID
  selection (Macronix vs Sanyo) for Pokémon Emerald.
- `debug/2026-04-29_pokemon-save-irq-pipeline-refill.md` — IRQ
  pipeline-refill ordering bug fixed alongside.
- Commit 27722c4 — halt-wake fix surfaced by SRTOG triage.
