# How audio works in VibeGBA

A walkthrough of the audio pipeline, paired with the actual code so you can
read it line by line. Start here, then open the files in the order listed at
the bottom.

The goal of this doc: by the time you're done reading it, you should be able
to point at any line in `apu/`, `dma.rs`, or `timer.rs` and explain what role
it plays in the path from "game writes a register" to "PCM sample played by
your speakers."

## 30,000-ft view

```
   ROM / IWRAM (sample data, mixer buffers)
        │
        │  CPU instructions / DMA
        ▼
  ┌─────────────────────────────────────────────────┐
  │                APU (gba-core/src/apu/)          │
  │                                                 │
  │   ┌──── PSG ────┐         ┌── DirectSound ──┐   │
  │   │  ch1 ch2    │         │  FIFO A         │   │
  │   │  ch3 ch4    │         │  FIFO B         │   │
  │   └──────┬──────┘         └─────────┬───────┘   │
  │          │                          │           │
  │          └─────────┬────────────────┘           │
  │                    ▼                            │
  │              current_mix()                      │
  │                    │                            │
  │                    ▼                            │
  │       boxcar decimator (≈349:1)                 │
  │                    │                            │
  │                    ▼                            │
  │         sample_buffer  (i16 stereo, 48 kHz)     │
  └────────────────────┬────────────────────────────┘
                       │
                       │  Gba::drain_audio()
                       ▼
            ┌──────────────────────┐
            │  SDL2 audio callback │  → speakers
            └──────────────────────┘
```

Six concurrent things have to happen for one sample to come out right:

1. **Game writes** to a sound register (e.g. `0x04000060` for ch1 sweep).
2. **Bus dispatches** the write into `apu::write_reg`, which updates the
   relevant channel's state fields (`apu::mod.rs:342`).
3. **The APU is ticked** once per CPU cycle by `apu.tick(cycles)` called from
   `lib.rs::run_cycles` after every instruction.
4. **Channel state advances**: square wave duty position, wave-table index,
   noise LFSR, FIFO held-sample — each on its own clock divider.
5. **The mixer** combines all six channels into a left/right pair, every CPU
   cycle. Per-cycle pairs accumulate.
6. **Decimation**: when the fractional counter crosses a 48 kHz output
   boundary, the average of the accumulated pairs is emitted into
   `sample_buffer`. The frontend drains this into SDL2.

If you understand all six, you understand the audio pipeline.

## The two channel families

GBA audio has **six** simultaneous channels: four PSG (carried over from the
original Game Boy) and two DirectSound (added for the GBA).

### PSG (channels 1–4) — synthesis

PSG channels generate sound from register state alone. No sample data, no
DMA. The game just writes "play a square wave at 440 Hz, decreasing volume"
into the registers and the hardware synthesises the waveform.

Each PSG channel has:

- **A frequency/period timer** — counts down CPU cycles; on hitting 0 it
  advances the channel's waveform position (duty step for squares, sample
  index for the wave channel, LFSR shift for noise).
- **A length counter** — decremented at 256 Hz; when it hits 0 the channel
  disables itself. Used to make a note stop after a fixed duration without
  the CPU intervening.
- **A volume envelope** — incremented or decremented at 64 Hz; gives notes a
  fade-in or fade-out shape (e.g. "play full volume, then drop one level
  every ~63 ms" → a soft decay).
- (Ch1 only) **A frequency sweep** — adjusts the frequency itself at 128 Hz;
  produces pitch-bend effects (lasers, the Mario coin sound, etc.).

The length / envelope / sweep clocks all come from a single divider running
at 512 Hz called the **frame sequencer** (`apu::mod.rs::clock_frame_sequencer`,
line 294). It's an 8-step counter; step 0/2/4/6 clock length, 2/6 clock
sweep, step 7 clocks envelope. This is the same design as the Game Boy and
it's worth memorising.

Per channel:

| | Ch1 | Ch2 | Ch3 | Ch4 |
|---|---|---|---|---|
| Type | Square wave | Square wave | Programmable waveform | Noise |
| Has sweep? | ✓ | – | – | – |
| Has envelope? | ✓ | ✓ | – | ✓ |
| Length range | 0–63 | 0–63 | 0–255 | 0–63 |
| Output range | ±15 | ±15 | ±8 | ±15 |
| File | `psg.rs:30` | `psg.rs:153` | `psg.rs:228` | `psg.rs:316` |

The **frequency formula** is the same for all square channels:
`f_audio = 131072 / (2048 − F)` Hz where F is the 11-bit register value.
At F=0 the period is 2048 PSG-clock ticks per duty step; at F=2047 it's 1.
The PSG-clock runs at CPU_CLOCK / 16, so in CPU cycles each duty step is
`(2048 − F) * 16`. (Until commit `eb4e878` we had `* 4` here, which made
every PSG channel run two octaves too high.)

### DirectSound (FIFOs A and B) — sample playback

DirectSound channels play 8-bit signed PCM samples streamed from RAM by
DMA. There are two channels (A and B), each backed by a 32-byte hardware
FIFO. The mixer reads one byte per FIFO every Timer-0 (or Timer-1)
overflow, then holds that byte as the channel's output until the next
overflow.

This means a DirectSound channel's "sample rate" is determined by which
timer drives it and how fast that timer is configured to overflow. M4A
games typically run Timer 0 at 13379 Hz (so a sample is popped every
~1254 CPU cycles); Castlevania-style games push it higher.

Refilling the FIFO is the DMA controller's job. DMA1 and DMA2, when in
**Special** timing mode and pointing at `0x040000A0` (FIFO A) or
`0x040000A4` (FIFO B), transfer 4 words = 16 bytes from a buffer in
IWRAM/EWRAM into the FIFO every time the FIFO drops below half-full. The
exact handshake is documented in [`fifo-dma-vblank.md`](fifo-dma-vblank.md);
the short version is *timer overflow pops one byte, DMA tops it back up
in chunks of 16*.

Reading order: `apu/fifo.rs` is small and self-contained — open it first,
read top to bottom. Then `apu::on_timer_overflow` in `mod.rs:279` (which
pops a sample and asks the caller to fire DMA), and finally
`lib.rs::tick_timers` (lines 565–610) which wires that asking-for-DMA
request to the actual DMA controller.

## Where the clocks come from

```
  CPU_CLOCK_HZ = 16777216 (16.78 MHz)
       │
       ├─► apu.tick() — once per CPU cycle
       │     │
       │     ├─► ch1/2/3/4.tick() — own period timer
       │     ├─► frame_seq_counter += 1 → wraps at 32768
       │     │       └─► clock_frame_sequencer() at 512 Hz
       │     ├─► current_mix() → accumulator
       │     └─► sample_frac += 48000; wraps at 16777216
       │             └─► emit_sample() ≈ every 349 cycles
       │
       └─► timers tick (lib.rs::tick_timers)
             └─► T0/T1 overflow
                   └─► apu.on_timer_overflow(id)
                         ├─► fifo_a.pop_sample() (if T0 drives A)
                         └─► fifo_b.pop_sample() (if T1 drives B)
                               └─► returns "need refill?"
                                     └─► lib.rs::run_dma_for_fifo()
                                           └─► dma.execute_dma_transfer()
                                                 └─► fifo.push_byte() ×16
```

Three independent clocks ladder out of the master 16.78 MHz CPU clock:

- **PSG channel timers**: integer countdown of CPU cycles. Each channel has
  its own period.
- **Frame sequencer (512 Hz)**: every 32768 CPU cycles, drives length / sweep
  / envelope. Period = `CYCLES_PER_FRAME_SEQ` constant.
- **Output sample rate (48 kHz)**: fractional counter `sample_frac`.
  Increments by 48000 every CPU cycle, wraps at 16777216. When it wraps, we
  emit one stereo sample. Average emission gap ≈ 349 cycles.

The output sample rate is `OUTPUT_SAMPLE_RATE = 48_000` — chosen to match
the macOS Core Audio native rate so SDL2 doesn't have to resample, which is
a known source of aliasing.

## One sample's life, cycle by cycle

This is the per-cycle inner loop in `apu::tick()` (line 114) when at least
one PSG channel is enabled. Walk through it once mentally — it's the heart
of the synthesiser:

```rust
for _ in 0..cycles {
    self.ch1.tick();                                  // (1)
    self.ch2.tick();
    self.ch3.tick();
    self.ch4.tick();

    self.frame_seq_counter += 1;                      // (2)
    if self.frame_seq_counter >= CYCLES_PER_FRAME_SEQ {
        self.frame_seq_counter -= CYCLES_PER_FRAME_SEQ;
        self.clock_frame_sequencer();
    }

    if self.master_enable {                           // (3)
        let (l, r) = self.current_mix();
        self.accum_left += l as i64;
        self.accum_right += r as i64;
    }
    self.accum_count += 1;

    self.sample_frac += OUTPUT_SAMPLE_RATE as u64;    // (4)
    if self.sample_frac >= CPU_CLOCK_HZ as u64 {
        self.sample_frac -= CPU_CLOCK_HZ as u64;
        self.emit_sample();
    }
}
```

1. **Advance each PSG channel.** Inside each `.tick()`, the channel's own
   countdown timer decrements; when it hits 0 it reloads from the
   period-derived value and advances waveform state (e.g. `duty_pos = (duty_pos + 1) % 8`).
2. **Frame sequencer.** Every 32768 cycles, fire the 512 Hz tick, which may
   step length/envelope/sweep on selected channels.
3. **Take a snapshot of the mix.** `current_mix()` reads every channel's
   *current* output (no state advance — that already happened in step 1),
   applies SOUNDCNT_L per-channel pan/volume and SOUNDCNT_H PSG-vs-DMA
   ratio, then adds DirectSound A/B. The result is one cycle's worth of
   audio. We accumulate it.
4. **Maybe emit an output sample.** The fractional counter rolls over
   approximately every 349 cycles; when it does, average the accumulator
   and push one stereo pair to `sample_buffer`.

There's a `tick_fast` path (line 152) used when all PSGs are silent — the
common case in M4A games where music is DirectSound-only. In that path the
mix is constant for the entire batch (the FIFO held value), so we bulk-
accumulate and only call `emit_sample` at the sample-boundary points.
Critical for halt-period ticking where `cycles` is in the thousands.

## current_mix() — the actual mixer

```rust
fn current_mix(&self) -> (i32, i32) {
    let ch1 = self.ch1.output() as i32;
    let ch2 = self.ch2.output() as i32;
    let ch3 = self.ch3.output() as i32;
    let ch4 = self.ch4.output() as i32;

    let mut psg_left = 0i32;
    let mut psg_right = 0i32;
    // SOUNDCNT_L bits 8–15: per-channel L/R enables
    if self.psg_enable_left[0] { psg_left += ch1; }
    /* …same for ch2/3/4 and the right side… */

    // SOUNDCNT_L bits 0–6: per-side master PSG volume (0–7)
    psg_left = psg_left * (self.psg_volume_left as i32 + 1) / 8;
    psg_right = psg_right * (self.psg_volume_right as i32 + 1) / 8;

    // SOUNDCNT_H bits 0–1: PSG vs DMA ratio (0=25%, 1=50%, 2/3=100%)
    let psg_ratio = match self.psg_master_volume { 0 => 1, 1 => 2, _ => 4 };
    psg_left = psg_left * psg_ratio / 4;
    psg_right = psg_right * psg_ratio / 4;

    // DirectSound: just add the held FIFO values; SOUNDCNT_H bit 2/3 for
    // ratio is handled inside fifo.output() via volume_full.
    let fifo_a = self.fifo_a.output() as i32;
    let fifo_b = self.fifo_b.output() as i32;
    let mut left = psg_left;
    let mut right = psg_right;
    if self.fifo_a.enable_left  { left  += fifo_a; }
    if self.fifo_a.enable_right { right += fifo_a; }
    if self.fifo_b.enable_left  { left  += fifo_b; }
    if self.fifo_b.enable_right { right += fifo_b; }

    (left, right)
}
```

Three things to remember about the mixer:

- **PSG channels are gated by both pan-enable bits AND by master volume.**
  Even a maxed-out PSG channel can be muted if SOUNDCNT_L's per-channel L/R
  enable bit for it is off.
- **The PSG/DMA ratio in SOUNDCNT_H bits 0–1 only scales the PSG side.**
  DirectSound channels always go through at full level, with their own
  `volume_full` (50% vs 100%) inside `fifo.output()`.
- **Range expectations:** each PSG channel outputs ±15; ±60 after summing
  four. Each FIFO sample is ±128. So one channel of the final mix sits in
  roughly ±300 before the scaling chain runs. The `× 120` multiplier in
  `emit_sample` (line 256) is calibrated to bring this into the i16 range
  without clipping a typical M4A mix.

## emit_sample() — boxcar decimation

```rust
fn emit_sample(&mut self) {
    let n = self.accum_count as i64;
    let avg_left  = (self.accum_left  / n) as i32;
    let avg_right = (self.accum_right / n) as i32;
    self.accum_left = 0; self.accum_right = 0; self.accum_count = 0;

    let scaled_left  = (avg_left  * 120).clamp(-32768, 32767);
    let scaled_right = (avg_right * 120).clamp(-32768, 32767);

    let left_out  = scaled_left .clamp(-32768, 32767) as i16;
    let right_out = scaled_right.clamp(-32768, 32767) as i16;
    self.push_pair(left_out, right_out);
}
```

This is the *only* anti-alias filter we have. It works because averaging N
samples is mathematically a length-N **boxcar FIR**, whose frequency
response is `sin(πf·N/fs) / (πf·N/fs)` (a sinc). With N ≈ 349 and
fs = 16.78 MHz, the first null sits at fs/N ≈ 48 kHz — right on top of the
output Nyquist. Frequencies above 48 kHz fold back, but they hit
suppressed lobes of the sinc.

A previous version had a second-stage IIR (`y[n] = (y[n-1] + x[n]) / 2`)
chained after this. It dropped 8 dB at 16 kHz, audible as muffling.
Removed in commit `2b5b69d` — read the doc comment in `emit_sample` if
you wonder why those `lpf_*` fields are still on the struct.

## From `sample_buffer` to your speakers

Once `emit_sample` pushes a stereo pair, two more steps get it to the
speakers. The frontend (`gba-frontend/src/audio.rs`) sets up an SDL2 audio
device with a callback at 48 kHz stereo. The main loop, after each chunk of
emulator cycles, calls `Gba::drain_audio(&mut audio_tmp)` (a thin wrapper
over `Apu::drain_samples`, line 333) to copy whatever has been emitted into
the SDL2 audio buffer:

- `apu/mod.rs:333` — `drain_samples()` copies from `self.sample_buffer` into
  a caller-supplied slice and shrinks the buffer.
- `gba-frontend/src/audio.rs:100` — `push_samples()` appends into a
  shared-state ring buffer.
- `gba-frontend/src/audio.rs:160` — `AudioCallback::callback()` is invoked
  by SDL2 (typically at ~2.7 kHz, asking for ~17 ms of samples each time)
  and drains from that ring.

This split is intentional: the emulator emits at the rate the CPU emulates
at (potentially much faster or slower than realtime), and the SDL2 ring
absorbs the difference. If the emulator runs too slow, the ring underruns
and you hear a click. If it runs too fast, push back-pressure builds and
either the buffer fills (clamped at `sample_buffer_max`) or the frontend
sleeps — see `lib.rs::run_cycles` callers in the frontend for the
back-pressure logic.

## Things that often go wrong

These are the failure modes that have actually shown up; checking them
first when audio is off saves time.

1. **Pitch off by 4× / 2 octaves.** PSG timer reload formula confused
   between PSG-clock and CPU-clock cycles. Fixed in `eb4e878` — see the
   spec-pinning test `ch1_full_waveform_period_matches_gba_spec` in
   `psg.rs`.
2. **Muffled / no treble.** Extra post-IIR low-pass on top of the boxcar.
   Removed in `2b5b69d`.
3. **DMA reads garbage past the end of a buffer (Pokémon-style).** Internal
   SAD pointer walks forward forever; game expects an implicit reset each
   VBlank. Worked around with a gated VBlank re-anchor — see
   [`fifo-dma-vblank.md`](fifo-dma-vblank.md) and `lib.rs:516`.
4. **Crackling at periodic intervals on cycle-sensitive timer-driven sound
   techniques (velipso's `*` modes).** Cycle-accurate IRQ alignment is off
   by a few cycles, breaking buffer-swap timing. Documented as a known
   limit in `fifo-dma-vblank.md`.
5. **SFX vs music balance wrong, or one missing.** Easy to spot by dumping
   per-channel state in `audio_dump.rs` and watching which channels are
   enabled when the sound plays. M4A engine games often mix all SFX
   *into the FIFO A buffer in software*, so a missing SFX is a missing CPU
   write, not a missing channel — debug the game's sound engine pointer,
   not the APU.

## Reading order

If you want to read the audio code top to bottom, here's the recommended
order. Each file is small enough to read in one sitting.

1. **`debug/concepts/timers.md`** — refresher on Timer 0/1, prescaler, and
   how overflow events propagate. Audio depends on this.
2. **`gba-core/src/apu/fifo.rs`** (178 lines) — start here. Smallest,
   easiest, and the FIFO model is the foundation for understanding
   DirectSound. Read top to bottom in one go.
3. **`gba-core/src/apu/psg.rs`** (510 lines) — read in this internal order:
   the `DUTY_TABLE` constant, then `Channel2` (simplest), then `Channel1`
   (adds sweep), then `Channel3` (wave RAM), then `Channel4` (noise + LFSR).
   The tests at the bottom show how each channel is exercised.
4. **`gba-core/src/apu/mod.rs`** (582 lines) — top down:
   - `Apu` struct (the state lives here).
   - `new()` for defaults.
   - `tick()` and `tick_fast()` — the per-cycle loop.
   - `current_mix()` — the mixer.
   - `emit_sample()` and `push_pair()` — the sample output.
   - `on_timer_overflow()` — wiring to timers.
   - `clock_frame_sequencer()` — the 512 Hz divider.
   - `write_reg()` and `read_reg()` — register dispatch.
5. **`gba-core/src/lib.rs::tick_timers` and `run_dma_for_fifo`**
   (≈ lines 565–640) — how a timer overflow ends up triggering a DMA
   refill into a FIFO.
6. **`debug/concepts/fifo-dma-vblank.md`** — the VBlank re-anchor and why
   it exists (DirectSound-specific quirk; impossible to get right without
   knowing the M4A model).
7. **`gba-frontend/src/audio.rs`** (180 lines) — the SDL2 sink. Read the
   `AudioBuffer` and `AudioCallback` types and the `init_audio` setup.

That's the full audio path. Everything else (`audio_dump.rs`,
`fifo_trace.rs`, `dump_sample_buffer.rs` examples) is diagnostics built on
top of these primitives — open them only when you need a specific probe.

## Related

- [`fifo-dma-vblank.md`](fifo-dma-vblank.md) — DirectSound DMA, why
  internal_sad walks forward, and the latch-recency gate.
- [`dma-registers.md`](dma-registers.md) — the SAD/DAD/CNT register model
  shared by FIFO and non-FIFO DMA.
- [`timers.md`](timers.md) — Timer 0/1/2/3 hardware model.
- [`fifo-dma-vblank.md`](fifo-dma-vblank.md) — known velipso `*`-mode
  limitation (deep cycle-accuracy ceiling).
