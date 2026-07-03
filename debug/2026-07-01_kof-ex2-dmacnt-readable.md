# KOF EX2 white screen — DMAxCNT_H read-back stubbed to 0

Date: 2026-07-01
Status: **Fixed** (commit `0a0f291`)

## Symptom

The King of Fighters EX2: Howling Blood boot-hung to a **white screen**. It
never reached its splash — the game froze early in boot and produced no video.

## How it was found

Traced boot execution and watched where it stopped making progress: the CPU
ended up parked in `VBlankIntrWait` forever (waiting on a VBlank IRQ that its
own state could no longer service). Working backwards from "why can't it leave
IntrWait", the interrupt registers `IE`/`IME` in the I/O block had been
**overwritten with garbage** — something had written sound-buffer bytes across
the I/O register region. The only thing that writes wide swaths of I/O is a
misprogrammed DMA, so the investigation focused on the DMA channels the sound
engine sets up.

## Investigation

1. **What clobbered `IE`/`IME`?** A DMA transfer with the wrong destination /
   timing / increment mode. A correctly-configured FIFO sound DMA has
   destination `Fixed` (always the FIFO register) and `Special` timing (fires
   on Timer 0 overflow, 4 words per trigger). Something turned one of the sound
   DMAs into an `Immediate`-timing, destination-`Increment` transfer, which ran
   immediately and walked its destination pointer straight through the I/O
   block, spraying sound samples over `IE`/`IME`/etc.

2. **Where does the corruption originate?** In KOF EX2's **Timer 2 IRQ
   handler**, which restarts its FIFO DMA with the standard read-modify-write
   idiom on the control register `DMAxCNT_H`:

   ```asm
   LDRH r0, [CNT_H]      ; read current control
   BIC  r0, r0, #0x8000  ; clear enable bit
   STRH r0, [CNT_H]      ; disable
   LDRH r0, [CNT_H]      ; read again
   ORR  r0, r0, #0x8000  ; set enable bit
   STRH r0, [CNT_H]      ; re-enable → the 0→1 edge re-latches the DMA
   ```

   The whole point of this idiom is to preserve every *other* control bit
   (timing, dest-control, repeat, IRQ) while toggling only the enable bit.

3. **The stub.** Our I/O read handler stubbed the entire DMA register range to
   0:

   ```rust
   0x0B0..=0x0DE => 0, // TODO: DMA register reads
   ```

   So `LDRH [CNT_H]` returned **0**, not the channel's real control bits. The
   `BIC`/`ORR` then operated on 0, and the final `STRH` wrote back a control
   word with **only the enable bit set** — timing bits cleared (→ `Immediate`),
   dest-control cleared (→ `Increment`), repeat cleared. Re-enabling produced a
   runaway immediate incrementing transfer that copied the sound buffer across
   the I/O block, corrupting `IE`/`IME` → the game could never leave
   `VBlankIntrWait` → white-screen hang.

## Root cause

`DMAxCNT_H` (the control halfword, at `0xBA`/`0xC6`/`0xD2`/`0xDE`) is a
**readable** register per GBATEK, but our I/O read path returned 0 for the
whole DMA register block. Games that restart DMA via a read-modify-write of
`CNT_H` therefore lost every control bit except enable, converting a
`Special`/`Fixed` FIFO DMA into an `Immediate`/`Increment` runaway.

The address and count registers (`SAD`/`DAD`/`CNT_L`) genuinely *are*
write-only on hardware (they read as 0), so the stub was correct for them — the
bug was only that it also swallowed the one readable register in the block.

## Fix

`gba-core/src/bus/mod.rs`, I/O read handler — return the live control word for
the four `DMAxCNT_H` addresses, keep the others write-only:

```rust
// DMA registers. Source/dest/word-count (DMAxSAD/DAD/CNT_L) are
// write-only and read back as 0. The control halfword (DMAxCNT_H,
// at 0xBA/0xC6/0xD2/0xDE) IS readable and returns the current
// control bits. Games' sound engines restart the FIFO DMAs via a
// read-modify-write of DMAxCNT_H ... Returning 0 here dropped the
// timing/dest bits ... (KOF EX2: white screen).
0x0BA => self.dma.channels[0].control,
0x0C6 => self.dma.channels[1].control,
0x0D2 => self.dma.channels[2].control,
0x0DE => self.dma.channels[3].control,
0x0B0..=0x0DD => 0, // DMAxSAD/DAD/CNT_L: write-only
```

(was the single `0x0B0..=0x0DE => 0` stub.)

## Regression tests

No new unit test — the failure is a boot-hang that our harness doesn't yet
assert per-game. Verified:

- `cargo test` — 91 unit tests pass.
- No regression in Emerald / Golden Sun / Castlevania (all still boot/play).

Candidate future guard: a per-game "reaches frame N without hanging" smoke test
would have caught this and the class of boot-hangs generally. Tracked loosely
under followups "Test more commercial games".

## Verification

- KOF EX2 now boots past the hang to its splash screen and plays.
- The Timer 2 IRQ handler's read-modify-write now preserves the `Special`/
  `Fixed`/repeat bits, so the FIFO DMA re-enables correctly instead of turning
  into a runaway immediate transfer; `IE`/`IME` are no longer clobbered.

## Lessons

- **A stubbed `=> 0` read is only safe for genuinely write-only registers.**
  For any register GBATEK documents as readable, returning 0 silently corrupts
  the very common read-modify-write idiom (`LDR; modify one bit; STR`). Here it
  turned a one-bit enable toggle into a full control-word wipe. When stubbing an
  I/O read, check the register's documented access: write-only → 0 is fine;
  readable → you must return real state.
- **Runaway-DMA symptoms point at a mis-latched control word.** "Wide region of
  I/O/RAM overwritten with what looks like audio samples" + "stuck in
  IntrWait" is the fingerprint of a FIFO DMA that lost its `Special`/`Fixed`
  bits and ran as `Immediate`/`Increment`.

## Related

- [concepts/dma-registers.md](concepts/dma-registers.md) — "Register
  readability" section documents which DMA registers are readable and this trap.
- [concepts/fifo-dma-vblank.md](concepts/fifo-dma-vblank.md) — the FIFO DMA
  timing/dest configuration that the corrupted write-back destroyed.
