# The event scheduler (priority queue)

## The problem it solves

Imagine the naive approach:

```
loop forever:
  1. Execute one CPU cycle
  2. Ask PPU: "Is it HBlank yet?"
  3. Ask PPU: "Is it VBlank yet?"
  4. Ask each timer: "Have you overflowed yet?"
  5. Ask each DMA channel: "Should you trigger now?"
  6. Ask APU: "Time for a new audio sample?"
```

That's ~10 checks per CPU cycle × 16.78 million cycles per second = **168 million checks per second, 99.9% of which return "no"**. Wasteful.

## The insight

We can **predict the future** for each hardware component:
- HBlank will fire at cycle 960 (next scanline boundary)
- VBlank will fire at cycle 197,120 (start of line 160)
- Timer 0 (prescaler 64) will overflow in 64 × remaining_count cycles
- Next audio sample at cycle 512 (and every 512 cycles after that)

So instead of checking every cycle, we **put all predicted events in a queue sorted by when they'll fire**, and run the CPU flat-out until we hit one.

## The data structure: min-heap

A min-heap is a tree where the smallest value is always at the top. "Smallest" here means "earliest fire_time":

```
         HBlank@960         ← root (soonest)
        /          \
  APUSample@1024   HBlankEnd@1232
     /
 VBlank@197120
```

Popping the root gives us the next event in O(log n). Inserting a new event is also O(log n). Rust's `std::collections::BinaryHeap` is a max-heap by default, so we just reverse the `Ord` impl on our `Event` struct to make it a min-heap.

## A concrete walkthrough

Let's say the emulator just started. `timestamp = 0`. Initial state:

```
events = [
  { fire_time: 960, kind: HBlank },     ← first HBlank
]
```

**Step 1** — Main loop peeks at the queue: next event at 960. Run CPU until clock = 960.

The CPU might execute 200 instructions, each taking 1-5 cycles, summing to 960 cycles. We don't check anything during those 200 instructions — just run.

After the CPU loop, `timestamp = 960`.

**Step 2** — Pop the HBlank event. Dispatch it:

```rust
EventKind::HBlank => {
    // Set HBlank bit in DISPSTAT
    // Render scanline 0 into the framebuffer
    // Fire HBlank DMA if configured
    // Schedule the end of HBlank
    scheduler.push(Event {
        fire_time: 960 + 272,  // HBlank lasts 272 cycles
        kind: HBlankEnd,
    });
}
```

Queue is now:

```
events = [
  { fire_time: 1232, kind: HBlankEnd }
]
```

**Step 3** — Peek again: next at 1232. Run CPU 272 more cycles. `timestamp = 1232`.

**Step 4** — Pop HBlankEnd. Dispatch:

```rust
EventKind::HBlankEnd => {
    io.vcount += 1;           // Move to next scanline
    // Schedule the next HBlank
    scheduler.push(Event {
        fire_time: 1232 + 960,
        kind: HBlank,
    });
    // If vcount == 160, also schedule VBlank
}
```

**Each event schedules the next one.** That's the key to keeping the queue populated without any central "scheduler planner."

## Events re-schedule themselves

This is the clever bit. Look at the lifecycle of just HBlank events:

```
HBlank (at 960) → pushes HBlankEnd (at 1232)
  HBlankEnd (at 1232) → pushes next HBlank (at 2192)
    HBlank (at 2192) → pushes HBlankEnd (at 2464)
      ...forever
```

Same for timers:

```
TimerOverflow (at cycle X) →
  - increments counter (or reloads)
  - fires IRQ if enabled
  - pushes next overflow at X + (reload_ticks × prescaler)
```

## How the main loop actually looks

```rust
pub fn run_frame(&mut self) {
    let frame_end = self.scheduler.timestamp() + CYCLES_PER_FRAME;

    while self.scheduler.timestamp() < frame_end {
        // How long can we run the CPU freely?
        let next_event = self.scheduler.peek_time().unwrap_or(frame_end);
        let target = next_event.min(frame_end);

        // Run CPU at full speed until that time
        while self.scheduler.timestamp() < target {
            let cycles = self.cpu.step(&mut self.bus);
            self.scheduler.add_cycles(cycles as u64);
        }

        // Fire all events that are now ready
        while let Some(event) = self.scheduler.pop_if_ready() {
            self.handle_event(event);
        }
    }
}
```

Three nested layers:

1. **Outer**: run until end of frame
2. **Middle**: peek at next event, run CPU until it
3. **Inner**: step CPU one instruction at a time

## Why it's fast

In a typical frame:

- ~280,000 CPU cycles
- ~500 scheduler events total (HBlanks, VBlanks, timer overflows, audio samples)

So we amortize ~560 CPU cycles per heap operation. With a heap of ~5-10 events, `push`/`pop` is essentially a handful of comparisons. **Overhead is ~1% of total runtime**, vs 50-80% with the naive polling approach.

## The mental model

Picture the CPU as a runner and the scheduler as a meeting schedule:

> "The runner sprints until the next meeting on the calendar. At the meeting, the hardware does its thing, schedules its next meeting, and the runner goes again."

The runner never stops to ask "is there a meeting yet?" — the calendar answers that question once, at the start of each sprint.
