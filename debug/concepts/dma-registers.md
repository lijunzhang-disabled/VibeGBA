# DMA registers — SAD, DAD, and the two-register pattern

If you've worked on modern accelerators (GPU/NPU/NIC) you're used to "build a descriptor in memory, push it onto a submission queue, ring a doorbell, hardware does work async, posts to a completion queue". The GBA's DMA is much more primitive than that, and this doc explains the model and the bare-name terminology you'll see all over the codebase.

## Glossary

- **`SAD`** — **S**ource **AD**dress. Where DMA reads from. (E.g. for sound DMA, this points at a sample buffer in IWRAM.)
- **`DAD`** — **D**estination **AD**dress. Where DMA writes to. (E.g. for sound DMA, this is the FIFO register at `0x040000A0` or `0x040000A4`.)
- **`CNT_L`** — Word **C**ou**NT** (low half). How many transfers to do. Ignored for FIFO timing.
- **`CNT_H`** — Co**NT**rol (high half). Bitfield: enable, repeat, timing source, IRQ-on-finish, source/destination address-increment modes.
- **`FIFO`** — **F**irst **I**n, **F**irst **O**ut: a hardware queue. The first byte written is the first byte read out. The DirectSound A/B FIFOs (32 bytes each) are exactly this: DMA pushes samples in one end, the sound output pops them out the other end at the timer-driven rate. Contrast with a stack (LIFO).

## The four channels

The GBA has exactly **4 DMA channels** (DMA0–DMA3), each with its own dedicated register block at `0x040000B0` upward:

| Channel | SAD addr | DAD addr | CNT_L | CNT_H | Typical use |
|---|---|---|---|---|---|
| DMA0 | `0x040000B0` | `0x040000B4` | `0x040000B8` | `0x040000BA` | sensitive transfers (e.g. HDMA from internal RAM) |
| DMA1 | `0x040000BC` | `0x040000C0` | `0x040000C4` | `0x040000C6` | sound FIFO A |
| DMA2 | `0x040000C8` | `0x040000CC` | `0x040000D0` | `0x040000D2` | sound FIFO B |
| DMA3 | `0x040000D4` | `0x040000D8` | `0x040000DC` | `0x040000DE` | general-purpose, big copies, decompression destinations |

DMA0 has the highest priority; DMA3 the lowest. If multiple channels are eligible at the same cycle, lower-numbered ones run first. Software can't reorder this.

## The two-register pattern: `sad` vs `internal_sad`

When a game writes the SAD register, that value lands in our `channel.sad` field. But the register the DMA controller actually *uses while transferring* is a separate, hidden register. We call it `internal_sad`. On real hardware these "internal" registers aren't software-visible at all — the game can't read them.

```
                   software view              hardware view
                  ─────────────              ──────────────
write to 0xBC →   channel.sad ────latch───→  channel.internal_sad
                       (frozen,                    (cursor that
                        keeps                       advances during
                        original                    transfers)
                        value)
```

### Why two registers?

**1. The programmed value is the "ground truth" to snap back to.** The cursor has to advance during a transfer, but the value the game programmed must survive so it can be reloaded on repeat/re-trigger. If there were only one register, a repeating channel would have no start-of-buffer to return to after the cursor walked off the end. Keeping `sad` frozen and advancing a separate `internal_sad` preserves that ground truth. (Note: on the GBA the programmed `SAD`/`DAD`/`CNT_L` registers are *write-only* — a game can't `LDR` them back at all; they read as 0. So this isn't about software read-back, it's purely internal bookkeeping. The one DMA register that *is* readable is `CNT_H` — see "Register readability" below.)

**2. Repeat / re-trigger semantics.** When DMA repeats, the controller can reload `internal_count` from `count` (and optionally `internal_dad` from `dad`) without losing what was programmed. The cursor advances during transfers; the programmed register is the "ground truth" you snap back to. The whole trick of `SoundDriverVSync` (see [fifo-dma-vblank.md](fifo-dma-vblank.md)) relies on this: it forces a re-latch of `internal_sad` from `sad` to snap the cursor back to the start of the buffer.

### When each one is touched

In `gba-core/src/dma.rs`:

| Field | Written by | Read by |
|---|---|---|
| `sad` | I/O register handler (`bus/mod.rs::write_io16` for `0x0BC`/`0x0BE` etc.) — i.e. by the *game* | `latch()` to seed `internal_sad` |
| `internal_sad` | `latch()` (on enable bit 0→1), `run_dma_channel()` (advances per-word during transfer) | `run_dma_channel()` (the actual `bus.read32(internal_sad)`) |

So a game's lifecycle for DMA1 looks like:

```
1.  STR  buffer_addr → DMA1SAD          ; sets channel.sad = buffer_addr
2.  STR  fifo_addr   → DMA1DAD          ; sets channel.dad
3.  STR  control     → DMA1CNT_H        ; enable bit 0→1
                                          → latch() runs:
                                            internal_sad = sad
                                            internal_dad = dad
                                            internal_count = count
4.  Timer 0 overflows                    ; run_dma_channel() runs:
                                            transfer 4 words from internal_sad
                                            internal_sad += 16   ← cursor advances
                                          (sad still = buffer_addr, untouched)
5.  Game later does SWI 0x1D / toggles enable
                                          → latch() runs again:
                                            internal_sad = sad   ← snap back to start
```

`sad` is what the game programmed. `internal_sad` is where the controller is *currently* reading from. The same applies to `dad`/`internal_dad` and `count`/`internal_count`.

## Register readability (and the trap it hides)

Not all DMA registers are readable. Per GBATEK:

| Register | Addr (DMA1) | Access |
|---|---|---|
| `SAD` | `0x0BC` | **write-only** (reads 0) |
| `DAD` | `0x0C0` | **write-only** (reads 0) |
| `CNT_L` (word count) | `0x0C4` | **write-only** (reads 0) |
| `CNT_H` (control) | `0x0C6` | **read/write** |

Only the control halfword `CNT_H` reads back its current bits (`0xBA`/`0xC6`/`0xD2`/`0xDE` for DMA0–3). The address and count registers read as 0.

This matters because sound engines **restart** their FIFO DMAs with a read-modify-write of `CNT_H` — clear the enable bit, then set it again to force a re-latch:

```asm
LDRH  r0, [CNT_H]      ; read current control
BIC   r0, r0, #0x8000  ; clear enable
STRH  r0, [CNT_H]      ; disable
LDRH  r0, [CNT_H]      ; read again
ORR   r0, r0, #0x8000  ; set enable
STRH  r0, [CNT_H]      ; re-enable → 0→1 edge re-latches internal_sad
```

If `CNT_H` reads back 0 instead of its real bits, the write-back drops the timing bits (`Special`/FIFO), the dest-control bits (`Fixed`), repeat, etc. — so re-enabling produces enable + `Immediate` timing + incrementing destination. That fires a runaway transfer that walks the whole I/O block, corrupting `IE`/`IME` and hanging the game. This was the King of Fighters EX2 white-screen bug — see [../2026-07-01_kof-ex2-dmacnt-readable.md](../2026-07-01_kof-ex2-dmacnt-readable.md). The moral: for a register whose *documented* behaviour is "readable", a stub `=> 0` is not a safe placeholder — games do read-modify-write on it.

## How GBA DMA differs from modern submission-queue DMA

If your mental model comes from NVMe / PCIe DMA / modern NPUs/GPUs, the GBA looks very minimalist:

| | Modern accelerator DMA | GBA DMA |
|---|---|---|
| Number of "slots" | many (queue depth typically 64–4096+) | exactly 4 (the channels) |
| Where descriptors live | RAM-resident SQ ring | each channel's MMIO register block |
| How to submit | write descriptor + ring doorbell | write SAD/DAD/CNT, then set enable bit (0→1) |
| Batching | yes — push N descriptors, ring once | no — one transfer per channel at a time |
| Async work | hardware processes queue, software does other work | also async, but you can't queue another transfer on the same channel until current finishes |
| Completion | dedicated CQ ring with status entries | optional IRQ when count exhausts; no per-result status |
| Priority | software-managed (queues, weights) | hard-coded by channel number (DMA0 > DMA1 > DMA2 > DMA3) |
| Recurring transfers | software re-submits descriptors | `Repeat` bit + event timing (VBlank/HBlank/Timer 0) — the channel auto-fires |

So on the GBA, the entire "queue" is the four register blocks. Two transfers in parallel = use two different channels. Two sequential transfers on the same channel = wait for the first to finish (poll the enable bit, or wait for IRQ) before re-writing SAD/DAD/CNT_H.

A few features bridge the gap a little:

- **Repeat bit** (`CNT_H` bit 9): the channel re-arms automatically on its next event. Sound DMA stays "alive" across thousands of timer ticks without per-trigger software involvement.
- **Event-triggered timing** (`CNT_H` bits 12–13): instead of "start now", the channel can be set to `Special` (FIFO refill on Timer 0), `VBlank`, `HBlank`, or `Immediate`. One configuration write arms the channel; subsequent firings happen automatically.

These are tiny pre-canned scheduling primitives baked into the silicon — closer to "here are 4 channels, each can be configured for one of a small handful of recurring trigger conditions" than to a real queue.

## Why this design

GBA is from 2001, ARM7TDMI-based, ~16.78 MHz, with 384 KB total internal memory. Adding an SQ/CQ ring would have been overkill: 4 channels with dedicated registers are simpler in silicon, lower latency, and sufficient for the sprite/audio/scanline use cases the console needs. Modern NPUs/GPUs need queueing because they have thousands of in-flight ops with complex scheduling. GBA has at most ~6 distinct DMA workflows running across 4 channels, all with predictable triggers, so direct register programming is fine.

## Related

- [fifo-dma-vblank.md](fifo-dma-vblank.md) — uses the `sad` vs `internal_sad` distinction to explain why we re-anchor sound DMA every VBlank.
- [blanking-periods.md](blanking-periods.md) — VBlank timing, the recurring trigger that drives the sound DMA reset.
- [../2026-07-01_kof-ex2-dmacnt-readable.md](../2026-07-01_kof-ex2-dmacnt-readable.md) — the CNT_H read-back bug that motivated the "Register readability" section.
- `gba-core/src/dma.rs` — the actual implementation.
