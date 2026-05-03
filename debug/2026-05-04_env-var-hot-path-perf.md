# Hot-path `std::env::var` calls killing emulation speed

Date: 2026-05-04
Status: **Fixed** (commit a14c551)

## Symptom

After landing the SRTOG fixes (chip-ID detection, halt-wake, IRQ
pipeline-refill), Pokémon Emerald started running noticeably slow:

- Audio sounded glitchy — buffer underruns, clicking, the music
  cadence audibly off.
- Input lag — pressing left/right took perceptible time to move the
  character; the game wasn't keeping up with 60 Hz.

Tests still passed, the emulator still booted everything, framebuffer
output looked correct — just *slow*.

## Root cause

I had added several env-gated diagnostic switches during the SRTOG
triage:

- `EXPERIMENT_GATE` — checked on every CPU step (in `cpu::step`)
- `INSTR_TRACE_RING` — checked on every step (in `step_arm` / `step_thumb`)
- `MEM_WATCH`, `MEM_WATCH_LO`, `MEM_WATCH_HI` — checked on every
  memory write (`write8`, `write16`, `write32`)
- `DISPCNT_TRACE` — checked on every IO write

Each was implemented as a direct `if std::env::var("X").is_ok() { ... }`
inline. That looks innocuous — single line, common Rust pattern — but
at hot-path frequencies it explodes.

### What `std::env::var` actually does

```
1. Acquire a global Mutex on the process environment table.
2. Call getenv() — a linear scan of the env array (typically dozens of vars).
3. Heap-allocate a String to wrap the matched value (or an Error).
4. Drop the String / Error immediately (.is_ok() ignores the value).
5. Release the Mutex.
```

The Rust stdlib wraps env in a mutex because env vars can be mutated
at runtime. So every call touches a process-wide lock + an allocation.
Roughly **100–200 ns each on macOS** — fine in isolation, ruinous in
a hot loop.

### The math

At real-time emulation, the CPU runs at ~16.78 MHz, so `step()` is
called ~16 M times per second. Per step, our code did:

| Site | Calls per step |
| --- | --- |
| `EXPERIMENT_GATE` | 1 |
| `INSTR_TRACE_RING` | 1 |
| `MEM_WATCH` (× 3 vars) | 3 × N where N = memory writes per instr |
| `DISPCNT_TRACE` | only on IO writes |

Average instr = ~0.5 memory writes. Total ≈ 4 env-var lookups per CPU
step ⇒ **~64 million env-var lookups per second**.

```
64 M × 150 ns ≈ 9.6 s of CPU time per 1 s wallclock
```

So the emulator ran at ~10 % real-time speed — entirely because of
diagnostic switches that were *off*.

## Symptoms explained

- **Audio glitching:** the SDL audio thread expected fresh samples at
  48 kHz. We produced them ~10× slower than needed → buffer underruns
  → SDL filled with silence → clicks and "zizizi"-like noise.
- **Input lag:** the frame loop ran much slower than 60 Hz, so the
  gap between key-press and on-screen response stretched into the
  100s of ms.
- **Tests didn't catch it:** unit tests run a few thousand
  instructions max — too small for the per-call overhead to add up
  to anything visible.

## Fix

Cache each env-var lookup in a `OnceLock<bool>` (or `OnceLock<(bool,
u32, u32)>` for the multi-var `MEM_WATCH` case). First call evaluates
the env::var; subsequent calls are a single atomic-relaxed pointer
load.

```rust
fn experiment_gate_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("EXPERIMENT_GATE").is_ok())
}
```

Behaviour identical when env vars are unset; per-step overhead drops
from ~600 ns to ~5 ns.

After the fix, Pokémon Emerald felt smooth again — same as it did
before the diagnostic switches were added.

## Lessons

1. **At hot-path frequencies, "small" things become huge.** What
   looks like a constant-time operation may include a syscall, a
   lock acquire, and a heap allocation. Multiply by 10⁷/sec and even
   `<1 µs` operations dominate the budget.
2. **Tests don't catch perf cliffs.** A unit test running 1k
   instructions doesn't show a 5× slowdown. You only see it under
   real-time, full-frame emulation. Worth keeping a "play Pokémon
   for 30 seconds" sanity check on the side.
3. **Diagnostic switches need to be free when off.** Never drop a
   raw `std::env::var(...)` into per-step / per-memory-access code.
   The "cache via OnceLock" pattern should be standard for any new
   debug knob.
4. **The Rust compiler can't lift this for you.** `env::var` has
   side effects (potentially a syscall), so the optimizer can't
   hoist it out of the loop. Has to be done by hand.

## Related

- Commit a14c551 — the fix.
- `debug/2026-04-30_srtog-flash-chip-id.md` — context for why the
  diagnostic switches were added in the first place.
- Other env-gated switches still live in cold paths (`SWI_TRACE`,
  `IRQ_TRACE`, `FLASH_TRACE_*`) — those are called rarely enough
  that direct `env::var` is fine, but if they ever get added to a
  hot path, cache them too.
