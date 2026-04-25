# GBA memory map — IWRAM, EWRAM, VRAM, and where everything lives

Every memory access in the emulator goes through `bus.read*` / `bus.write*`, which dispatches on the top byte of the 32-bit address to figure out which physical memory it's hitting. This doc explains those regions: what they are, what they're for, and the gotchas (mirroring, wait states) that crop up while debugging.

## The two work RAMs

The GBA has **two main RAMs** — fast-but-tiny and slow-but-roomy. They're both software-addressable; choosing one over the other is just choosing the linker section your code/data lives in.

| Name | Full name | Size | Address | Bus width | Access cost |
|---|---|---|---|---|---|
| **IWRAM** | **I**nternal **W**ork **RAM** | 32 KB | `0x03000000` | 32-bit | 1 cycle |
| **EWRAM** | **E**xternal **W**ork **RAM** | 256 KB | `0x02000000` | 16-bit | ~3 cycles per 16-bit access (so 6 cycles for a 32-bit word!) |

The split exists because **fast memory is expensive**:

- **IWRAM** lives **on the same chip** as the CPU. Wide 32-bit bus, zero wait states — every access is a single cycle. But it's tiny because on-die RAM costs silicon area.
- **EWRAM** is a **separate physical chip** outside the CPU. The bus to it is only 16 bits wide and slower. It's roomy but every access takes ~3× longer than IWRAM.

So you get a classic two-tier memory hierarchy *built into the address space* (no cache — just two regions you choose between by where you allocate).

### How games actually use them

```
EWRAM (256 KB, slow):           IWRAM (32 KB, fast):
  ─ huge maps                     ─ stack
  ─ NPC tables                    ─ hot inner loops (M4A sound engine,
  ─ savegame buffers                 sprite rotation, decompression)
  ─ AI state                      ─ frequently-touched globals
  ─ generally bulk data           ─ DMA buffers for sound
                                  ─ IRQ handler code
```

The compiler/programmer explicitly tags symbols with section attributes (`__attribute__((section(".iwram")))`, etc.). The link script lays out the binary and the C runtime copies IWRAM-tagged sections from ROM into IWRAM at boot. After that, those functions execute from fast RAM.

This is why M4A (Pokémon's sound engine) runs from IWRAM — the per-sample mixing inner loop runs ~13,400 times per second and needs every cycle. We saw this directly: the user IRQ handler we found was at IWRAM `0x03002750`, copied there from ROM at boot.

## The full GBA memory map

The whole 32-bit address space, dispatched by the top byte of the address (`addr >> 24`):

| Top byte | Region | Size | Notes |
|---|---|---|---|
| `0x00` | BIOS | 16 KB | Read-only. Reads return open-bus latch when PC is outside BIOS |
| `0x01` | (unused) | — | Open bus / undefined |
| `0x02` | **EWRAM** | 256 KB | Slow 16-bit work RAM |
| `0x03` | **IWRAM** | 32 KB | Fast 32-bit work RAM |
| `0x04` | I/O registers | 1 KB | DISPCNT, DMA, timers, sound, etc. |
| `0x05` | Palette | 1 KB | 256 BG colours + 256 sprite colours |
| `0x06` | VRAM | 96 KB | Tile data, framebuffer, sprite tiles |
| `0x07` | OAM | 1 KB | Sprite attribute table (128 entries) |
| `0x08`–`0x0D` | Cartridge ROM | up to 32 MB | Three mirror windows at different wait-state speeds |
| `0x0E`–`0x0F` | Cartridge SRAM/Flash | up to 64 KB | Battery-backed save memory, 8-bit bus |

So when you see addresses in our codebase:
- `0x02xxxxxx` → EWRAM
- `0x03xxxxxx` → IWRAM
- `0x04000xxx` → I/O register
- `0x06xxxxxx` → VRAM
- `0x08xxxxxx` → game ROM
- `0x0E000xxx` → cartridge save memory

## Mirroring — the gotcha that bit us in audio

Several regions are smaller than the address range allocated to them. Hardware handles this by **mirroring**: addresses past the end of the real memory wrap around to the beginning.

### IWRAM mirroring

IWRAM is 32 KB but the region `0x03000000`–`0x03FFFFFF` is 16 MB. Real hardware just ignores the high bits, so:

```
0x03000000  →  IWRAM[0x0000]   (real)
0x03007FFF  →  IWRAM[0x7FFF]   (last real byte)
0x03008000  →  IWRAM[0x0000]   (mirror — wraps back to start)
0x03018000  →  IWRAM[0x0000]   (mirror)
0x03FFFFFC  →  IWRAM[0x7FFC]   (mirror)
```

In our code: `iwram[(addr & 0x7FFF) as usize]`.

This bit us hard during the Pokémon audio investigation. M4A set `DMA1SAD = 0x030066D0` — a position about 26 KB into IWRAM. Each frame, DMA1's `internal_sad` advanced 224 bytes. After ~26 frames it crossed `0x03007FFF` and *kept advancing as `0x03008000`, `0x03008010`, …*, which all mirrored back into IWRAM, reading garbage instead of samples. See [fifo-dma-vblank.md](fifo-dma-vblank.md) for the full story.

### EWRAM mirroring

Same concept: 256 KB at `0x02000000`–`0x02FFFFFF`. Mirror mask is 18 bits: `ewram[(addr & 0x3FFFF)]`.

### Other mirrored regions

- **Palette** (1 KB): masks with `0x3FF`.
- **VRAM** (96 KB): odd shape — 64 KB at `0x06000000`–`0x0600FFFF` plus 32 KB at `0x06010000`–`0x06017FFF`, then `0x06018000`–`0x0601FFFF` mirrors that last 32 KB. The `0x06020000`+ range mirrors the whole 128-KB-shaped block.
- **OAM** (1 KB): masks with `0x3FF`.

The exact mirror masks are encoded in `bus/mod.rs::read*` / `write*`.

## Wait states (briefly)

When the CPU reads from EWRAM, the bus is 16 bits wide, so a 32-bit read takes two bus transactions. Each EWRAM transaction also has built-in wait states. Net result: ~3 cycles per 16-bit access, ~6 cycles per 32-bit access.

Cartridge ROM has *configurable* wait states (the `WAITCNT` register). Games can pick fast access for time-critical code paths and slow access for bulk data. A common trick: copy hot code from slow ROM into fast IWRAM at boot, run from there.

**We don't currently model these wait states** — every emulated access takes 1 cycle in our code. That's a known accuracy gap; it makes our emulated CPU "too fast" by ~10–30% depending on the workload. For most games it doesn't matter; for cycle-tight ones (precision platformers, any game that polls hardware in tight inner loops) it could cause subtle timing bugs. It's a Phase 9 polish item and lives on the TODO list in `PLAN.md`.

## In our emulator

`gba-core/src/bus/mod.rs`:

```rust
pub struct Bus {
    bios: Vec<u8>,                  //  16 KB
    ewram: Vec<u8>,                 // 256 KB
    pub(crate) iwram: Vec<u8>,      //  32 KB
    pub palette: Vec<u8>,           //   1 KB
    vram: Vec<u8>,                  //  96 KB
    oam: Vec<u8>,                   //   1 KB
    rom: Vec<u8>,                   //  up to 32 MB
    backup: BackupMedia,            //  SRAM / Flash / EEPROM
    pub io: IoRegisters,            //  the 0x04000xxx block
    // ... peripherals ...
}
```

The `Bus::read*` / `Bus::write*` family is one big `match addr >> 24 {...}` switch. Each arm picks the right region, applies its mirror mask, and either reads bytes directly or routes to `read_io16` / `write_io16` for the I/O block. That's the entire memory subsystem in one file.

## Related

- [emulator-basics.md](emulator-basics.md) — has the high-level bus dispatch sketch.
- [dma-registers.md](dma-registers.md) — DMA can move data between any two of these regions (subject to alignment rules).
- [fifo-dma-vblank.md](fifo-dma-vblank.md) — concrete example of why IWRAM mirroring matters when emulating sound DMA.
- `gba-core/src/bus/mod.rs` — the actual implementation.
