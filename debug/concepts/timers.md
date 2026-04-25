# GBA timers — counter, reload, prescaler, cascade, overflow

The GBA has **4 hardware timers** (Timer 0–3). Each one is a tiny counter + control register pair that can fire periodic events at any rate from a few hundred Hz up to ~16 MHz. Audio sample rates, animation tickers, randomness seeds, debouncing — anything periodic in a GBA game ultimately runs off one of these timers.

## The model

Each timer is **a 16-bit counter** that ticks upward, plus three programmable knobs:

| Concept | Register / bit | Purpose |
|---|---|---|
| **Reload value** | write `TMxCNT_L` | What the counter is re-seeded with after each overflow. Read `TMxCNT_L` to get the current counter. |
| **Enable** | `TMxCNT_H` bit 7 | Master on/off. |
| **Prescaler** | `TMxCNT_H` bits 0–1 | How many CPU cycles per timer tick: `00`=1, `01`=64, `10`=256, `11`=1024. |
| **Cascade** | `TMxCNT_H` bit 2 | If set (Timer 1–3 only), this timer ticks on the *previous* timer's overflow instead of using the prescaler. |
| **IRQ on overflow** | `TMxCNT_H` bit 6 | If set, every overflow raises the corresponding timer IRQ. |

Same dual-register pattern as DMA's `sad`/`internal_sad` (see [dma-registers.md](dma-registers.md)): `TMxCNT_L` is dual-purpose. **Writes** set the reload value; **reads** return the live counter. The two share the same address but mean different things in each direction.

## What "overflow" actually means

When the counter hits `0xFFFF` and tries to tick once more, hardware wraps it — but **not back to 0** like a normal integer wrap. It **reloads from `TMxCNT_L`** and continues counting from there. That moment is the *overflow event*.

```
counter:  0xFB1A → 0xFB1B → 0xFB1C → ... → 0xFFFE → 0xFFFF
                                                     ↓ tick once more
                                                  OVERFLOW EVENT  ─────► (IRQ / DMA / cascade)
                                                     ↓
counter:  0xFB1A (reload) → 0xFB1B → ...   (and around again)
```

The number of ticks between overflows is `0x10000 - reload`. The number of CPU cycles between overflows is `(0x10000 - reload) × prescaler`. So the **overflow rate** in Hz is:

```
f_overflow = 16_777_216 / ((0x10000 - reload) × prescaler)
```

Reload is the rate knob. Prescaler is the coarse-step knob.

## What an overflow event triggers

Three things — they're independent and any combination can be enabled.

1. **IRQ** — if `TMxCNT_H` bit 6 is set, the corresponding timer IRQ fires. The CPU jumps to its IRQ vector.
2. **DMA trigger** — if any DMA channel is configured for `Special` timing on Timer 0 or Timer 1 (sound FIFO mode), that DMA fires. See [fifo-dma-vblank.md](fifo-dma-vblank.md).
3. **Cascade** — if Timer N+1 has cascade mode set, it ticks once per overflow of Timer N. This lets you build long counts: Timer 0 overflows feed Timer 1, which feed Timer 2, etc., effectively combining 16-bit counters into 32/48/64-bit ones.

## Concrete examples

### Pokémon's M4A audio sample rate

From the audit dump:

```
Timer 0: enabled, reload=0xFB1A, prescaler=/1, irq=false, cascade=false
```

- Ticks per overflow: `0x10000 - 0xFB1A = 0x4E6 = 1254`.
- CPU cycles per overflow: `1254 × 1 = 1254`.
- Overflow rate: `16_777_216 / 1254 ≈ 13,379 Hz`.

Each overflow drains one byte from the FIFO and (when DMA1 is configured for `Special` timing) triggers a DMA refill. Net effect: M4A's audio plays at 13,379 samples/sec with no IRQ overhead — the FIFO + DMA + timer hardware handles delivery in pure silicon, and the CPU just has to write fresh samples into the buffer each frame.

### Frame counter via cascade

To count frames in hardware (instead of polling VBlank in software):

```
Timer 2: enabled, reload=0xFFFF, prescaler=/1024, irq=false      ; ticks slowly
Timer 3: enabled, reload=0x0000, cascade=true, irq=true          ; counts Timer-2 overflows
```

Timer 2 overflows every `1 × 1024 = 1024` cycles ≈ 60 µs. Timer 3 ticks once each time and overflows every 65,536 of Timer 2's overflows ≈ ~4 seconds, raising an IRQ. Doesn't really match VBlank rate exactly — this is just illustrating how cascade lets you build long counters cheaply.

(Real games typically just use the VBlank IRQ for per-frame counting. Cascade is more useful for generating very-low-frequency signals or for combining timers to count up to large values like a stopwatch.)

## In our emulator

`gba-core/src/timer.rs`:

```rust
pub struct Timer {
    pub reload: u16,            // last value written to CNT_L
    pub counter: u16,           // current counter value (what's read from CNT_L)
    pub control: u16,           // CNT_H
    pub(crate) prescaler_counter: u32,  // tracks fractional CPU cycles for /64, /256, /1024
}
```

Note `reload` and `counter` are two distinct fields, even though they share an address from the game's perspective — same architectural reason as `sad`/`internal_sad` for DMA.

`Timers::tick(cycles)` is called once per CPU step batch, advancing each timer by `cycles / prescaler` ticks. When a counter wraps, it sets a per-channel `irqs[i]` flag and (for Timer 0/1) `timer0_overflow` / `timer1_overflow` flags. These are returned to `lib.rs::handle_event`, which then:

```rust
for i in 0..4 {
    if result.irqs[i] { self.bus.interrupt.request_irq(TIMER_IRQS[i]); }
}
if result.timer0_overflow {
    let (a, b) = self.bus.apu.on_timer_overflow(0);
    if a || b { self.run_dma_for_timing(DmaTiming::Special); }
}
```

So overflow → check enabled triggers → fire each. Same shape as real hardware.

## The pattern, generalised

Once you've understood timers, the GBA's whole "scheduling primitive" model gets clearer:

- **Timers** generate periodic clock pulses at any rate the game picks.
- **DMA** moves data on those pulses, no CPU involvement.
- **IRQs** wake the CPU only when something interesting needs it.

A well-tuned GBA game spends most of its CPU time `HALT`ed waiting for IRQs, while timers + DMA handle the high-rate stuff in the background. The CPU is the slow, expensive resource; everything else is built to run autonomously between IRQs.

## Related

- [dma-registers.md](dma-registers.md) — Timer overflows are how `Special` (FIFO) DMA fires; same dual-register `value vs cursor` pattern.
- [fifo-dma-vblank.md](fifo-dma-vblank.md) — concrete example of Timer 0 driving sound DMA at 13,379 Hz.
- [scheduler.md](scheduler.md) — our scheduler models timer overflows as scheduled events (`EventKind::TimerOverflow`).
- `gba-core/src/timer.rs` — the implementation.
