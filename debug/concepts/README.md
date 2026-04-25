# Emulator Concepts — Reference Notes

Conceptual explanations of how the emulator works, separate from the per-bug records. Written for self-reference when you come back to the code after a break, or for anyone trying to understand *why* a piece of code does what it does.

Each file is one self-contained topic. Add a new file when you find yourself re-explaining something, or when a piece of code's reasoning is non-obvious enough that it deserves a write-up.

## Index

| Topic | File |
|---|---|
| The big picture: what an emulator actually is, what we build, and how it ticks | [emulator-basics.md](emulator-basics.md) |
| The event scheduler — priority queue that drives the main loop | [scheduler.md](scheduler.md) |
| HBlank / VBlank — what they are, where they come from, why every GBA game's main loop pivots on them | [blanking-periods.md](blanking-periods.md) |
| Memory map — IWRAM, EWRAM, VRAM, BIOS, mirroring rules, wait states | [memory-map.md](memory-map.md) |
| DMA register model — SAD/DAD/FIFO meaning, the two-register pattern (`sad` vs `internal_sad`), and why GBA DMA looks primitive next to modern queue-based DMA | [dma-registers.md](dma-registers.md) |
| DirectSound FIFO DMA + the VBlank reset hook | [fifo-dma-vblank.md](fifo-dma-vblank.md) |

## Style

- One topic per file. If a single concept doc gets longer than ~250 lines, consider splitting.
- Lead with what the reader is most likely confused about, then layer in detail.
- Quote external specs (GBATEK, ARM ARM) verbatim where it matters — don't paraphrase the spec.
- Link to relevant code with `path::function` form so future-you can grep.
- Cross-link to other concept docs and to bug records (`../YYYY-MM-DD_*.md`) where useful.

## Adding a new concept doc

1. Pick a short kebab-case filename (e.g. `dma-priorities.md`, `arm-thumb-pipeline.md`).
2. Write the doc.
3. Add a row to the Index table above.
4. Done — no separate "register" step.
