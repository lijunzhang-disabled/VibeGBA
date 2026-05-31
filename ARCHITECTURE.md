# GBA Emulator — Technical Architecture

## High-Level Architecture

```
┌──────────────────────────────────────────────────────┐
│                     Gba (top-level)                   │
│                                                       │
│  ┌─────────┐     ┌──────────────────────────────┐    │
│  │   Cpu   │────>│            Bus                │    │
│  │ ARM7TDMI│ &mut│                               │    │
│  │         │     │  ┌───────┐ ┌──────┐ ┌──────┐ │    │
│  └─────────┘     │  │ EWRAM │ │IWRAM │ │ BIOS │ │    │
│                  │  │ 256KB │ │ 32KB │ │ 16KB │ │    │
│  ┌──────────┐    │  └───────┘ └──────┘ └──────┘ │    │
│  │Scheduler │    │                               │    │
│  │  Events  │    │  ┌─────┐ ┌──────┐ ┌───────┐  │    │
│  └──────────┘    │  │VRAM │ │ OAM  │ │Palette│  │    │
│                  │  │96KB │ │ 1KB  │ │ 1KB   │  │    │
│  ┌──────────┐    │  └─────┘ └──────┘ └───────┘  │    │
│  │Framebuf  │    │                               │    │
│  │240x160   │    │  ┌─────┐ ┌──────┐ ┌───────┐  │    │
│  └──────────┘    │  │ PPU │ │ APU  │ │  DMA  │  │    │
│                  │  └─────┘ └──────┘ └───────┘  │    │
│  ┌──────────┐    │                               │    │
│  │BIOS HLE │    │  ┌──────┐ ┌──────┐ ┌───────┐  │    │
│  │(23 SWIs)│    │  │Timers│ │Keypad│ │ IRQ   │  │    │
│  └──────────┘    │  └──────┘ └──────┘ └───────┘  │    │
│                  │                               │    │
│                  │  ┌──────────┐ ┌────────────┐  │    │
│                  │  │   ROM    │ │  Backup    │  │    │
│                  │  │ <=32MB   │ │SRAM/Flash/ │  │    │
│                  │  │          │ │  EEPROM    │  │    │
│                  │  └──────────┘ └────────────┘  │    │
│                  └──────────────────────────────┘    │
└──────────────────────────────────────────────────────┘
         │                              │
         ▼                              ▼
┌─────────────────┐          ┌──────────────────┐
│  SDL2 Video     │          │  SDL2 Audio      │
│  (texture       │          │  (ring buffer    │
│   streaming)    │          │   callback)      │
└─────────────────┘          └──────────────────┘
```

## Ownership Model (Borrow-Checker Friendly)

The key architectural insight: **the CPU and Bus are sibling fields**, not nested. This lets Rust borrow them independently.

```rust
pub struct Gba {
    pub cpu: Cpu,           // ARM7TDMI state
    pub bus: Bus,           // Owns ALL memory and peripherals
    pub scheduler: Scheduler,
    frame_buffer: Vec<u16>, // 240x160 = 38400 pixels, 15-bit RGB
}
```

### Why this works

```rust
// In the main loop — Rust allows borrowing disjoint fields:
let cycles = self.cpu.step(&mut self.bus);  // cpu: &mut, bus: &mut — OK!
```

If cpu and bus were behind the same `Rc<RefCell<>>`, this would require runtime borrow checking. The sibling-field pattern gives us compile-time safety with zero overhead.

### PPU rendering — disjoint field borrows

```rust
// The Bus passes its own fields as separate references:
self.bus.ppu.render_scanline(
    line,
    &self.bus.io,        // immutable borrow of io
    &self.bus.palette,   // immutable borrow of palette
    &self.bus.vram,      // immutable borrow of vram
    &self.bus.oam,       // immutable borrow of oam
    &mut self.frame_buffer,
);
// ppu: &mut, io/palette/vram/oam: & — all disjoint, Rust allows this!
```

### SWI dispatch — pending flag pattern

The CPU can't call BIOS HLE directly (it would need `&mut Bus` which it already holds during `step()`). Instead:

```rust
// CPU sets a flag:
self.pending_swi = Some(swi_number);

// Main loop consumes it after step() returns:
if let Some(swi_num) = self.cpu.pending_swi.take() {
    if self.bus.has_bios {
        self.cpu.software_interrupt(swi_num);  // Jump to BIOS vector
    } else {
        bios::handle_swi(&mut self.cpu, &mut self.bus, swi_num);  // HLE
    }
}
```

This avoids the double-mutable-borrow problem entirely.

## ARM7TDMI CPU Architecture

### Register File

```
User/System  FIQ       IRQ       SVC       ABT       UND
R0           R0        R0        R0        R0        R0
R1           R1        R1        R1        R1        R1
R2           R2        R2        R2        R2        R2
R3           R3        R3        R3        R3        R3
R4           R4        R4        R4        R4        R4
R5           R5        R5        R5        R5        R5
R6           R6        R6        R6        R6        R6
R7           R7        R7        R7        R7        R7
R8           R8_fiq    R8        R8        R8        R8
R9           R9_fiq    R9        R9        R9        R9
R10          R10_fiq   R10       R10       R10       R10
R11          R11_fiq   R11       R11       R11       R11
R12          R12_fiq   R12       R12       R12       R12
R13 (SP)     R13_fiq   R13_irq   R13_svc   R13_abt   R13_und
R14 (LR)     R14_fiq   R14_irq   R14_svc   R14_abt   R14_und
R15 (PC)     R15       R15       R15       R15       R15
CPSR         CPSR      CPSR      CPSR      CPSR      CPSR
             SPSR_fiq  SPSR_irq  SPSR_svc  SPSR_abt  SPSR_und
```

### CPSR (Current Program Status Register)

```
Bit 31: N (Negative)     Bit 7: I (IRQ disable)
Bit 30: Z (Zero)         Bit 6: F (FIQ disable)
Bit 29: C (Carry)        Bit 5: T (Thumb state)
Bit 28: V (Overflow)     Bits 4-0: Mode
```

Mode bits: USR=0x10, FIQ=0x11, IRQ=0x12, SVC=0x13, ABT=0x17, UND=0x1B, SYS=0x1F

### ARM vs THUMB: When Each Mode Is Used

The ARM7TDMI dynamically switches between two instruction sets at runtime:

**ARM mode** (32-bit instructions, T=0):
- BIOS code (address 0x00000000)
- Interrupt handlers (hardware forces ARM mode on IRQ/SWI/exceptions)
- Performance-critical code in IWRAM (32-bit bus, no penalty for 32-bit fetches)
- Code needing full instruction set: conditional execution, barrel shifter on every op

**THUMB mode** (16-bit instructions, T=1):
- Majority of game code (ROM has 16-bit bus — THUMB fetches are 1 access vs 2 for ARM)
- Better code density (smaller binaries)
- Trade-off: only R0-R7 accessible in most instructions, no conditional execution

Switching: `BX Rn` — bit 0 of Rn determines the mode (1=THUMB, 0=ARM). Exceptions always enter ARM mode.

### Pipeline (3-stage)

```
                 Fetch → Decode → Execute
PC points here ──┘
(PC = executing instruction address + 8 in ARM, + 4 in THUMB)
```

When a branch occurs, the pipeline is flushed and refilled. This means:
- Reading PC during execution returns `current_addr + 8` (ARM) or `current_addr + 4` (THUMB)
- Branch targets take effect after pipeline refill (2 cycles wasted on flush)

Our implementation tracks this with `pipeline: [u32; 2]` (two prefetched opcodes) and `pipeline_flushed: bool`. On each step:
1. Consume `pipeline[0]` as current instruction
2. Shift `pipeline[1]` to `pipeline[0]`
3. Fetch next instruction into `pipeline[1]`
4. Advance PC

### Mode Switching Implementation

```rust
pub fn switch_mode(&mut self, new_mode: CpuMode) {
    // 1. Bank current SP/LR to banked[old_mode]
    // 2. If switching to/from FIQ, also bank R8-R12
    // 3. Restore SP/LR from banked[new_mode]
    // 4. Update mode bits in CPSR
}
```

## Instruction Decoding Strategy

### ARM (32-bit): Bit-pattern matching

```
Opcode:  [31:28] cond | [27:20] format | [19:8] operands | [7:4] sub-format | [3:0] operands
```

Currently decoded via cascading match on `bits[27:25]`, then sub-dispatching based on specific bit patterns. The major categories:

| bits[27:25] | Category |
|---|---|
| 000 | Data processing / Multiply / SWP / Halfword transfer / BX |
| 001 | Data processing immediate / MSR immediate |
| 010 | Single data transfer (immediate offset) |
| 011 | Single data transfer (register offset) / Undefined |
| 100 | Block data transfer (LDM/STM) |
| 101 | Branch / Branch with Link |
| 111 | SWI / Coprocessor |

### THUMB (16-bit): Match on bits [15:8]

```rust
match opcode >> 8 {
    0x00..=0x17 => // Format 1: Shifted register (LSL/LSR/ASR)
    0x18..=0x1F => // Format 2: Add/subtract
    0x20..=0x3F => // Format 3: Mov/cmp/add/sub immediate
    0x40..=0x43 => // Format 4: ALU operations
    0x44..=0x47 => // Format 5: Hi register / BX
    0x48..=0x4F => // Format 6: PC-relative load
    // ... 19 formats total
}
```

## Scanline Rendering Pipeline

```
For each frame (228 lines):
│
├── Lines 0-159 (visible):
│   │
│   ├── Dots 0-239 (visible pixels):
│   │   └── CPU runs for 960 cycles (240 * 4)
│   │
│   ├── HBlank begins (dot 240):
│   │   ├── Set HBlank flag in DISPSTAT
│   │   ├── Fire HBlank IRQ (if enabled)
│   │   ├── Execute HBlank DMA transfers (scroll effects, HDMA)
│   │   └── PPU: render_scanline() → composites BG + OBJ layers
│   │
│   └── Dots 240-307 (HBlank):
│       └── CPU runs for 272 cycles (68 * 4)
│
├── Line 160 (VBlank begins):
│   ├── Set VBlank flag in DISPSTAT
│   ├── Fire VBlank IRQ (if enabled)
│   ├── Execute VBlank DMA transfers (bulk copies to VRAM)
│   ├── Reload affine reference points from latches
│   └── Present framebuffer to SDL2
│
└── Lines 160-227 (VBlank):
    └── CPU runs normally, no rendering
```

## PPU: How Tile Rendering Works

### The Data Flow (Text BG)

```
DISPCNT register
  ├── Mode (0-5) → determines which BGs are text vs affine
  └── Enable bits (8-12) → which BGs and OBJs are visible

BGCNT[n] register
  ├── Priority (0-3)
  ├── Character base block (0-3 × 16KB)
  ├── Screen base block (0-31 × 2KB)
  ├── Color mode: 4bpp or 8bpp
  └── Screen size: 256×256 to 512×512

Step 1: Find tile map entry
  screen_base + (tile_y * 32 + tile_x) * 2
  → 16-bit entry: [tile_number:10][h_flip:1][v_flip:1][palette:4]

Step 2: Fetch character (pixel) data
  char_base + tile_number * (32 or 64) + row * (4 or 8) + col/2
  → 4bpp: 4 bits per pixel (two pixels per byte)
  → 8bpp: 8 bits per pixel (one byte per pixel)

Step 3: Look up color in palette
  4bpp: palette[palette_number * 16 + color_index]
  8bpp: palette[color_index]
  → 15-bit RGB (5 bits each for R, G, B)
```

### The Data Flow (Affine BG)

```
Same BGCNT setup, but:
  - Map entries are 8-bit (tile number only, no flip/palette)
  - Always 8bpp, single 256-color palette
  - Position calculated by affine transform each pixel:
      tex_x += PA per pixel    (PA = dx/dpixel)
      tex_y += PC per pixel    (PC = dy/dpixel)
    And per scanline:
      ref_x += PB              (PB = dx/dscanline)
      ref_y += PD              (PD = dy/dscanline)
```

### The Data Flow (Sprites/OBJ)

```
OAM (1KB = 128 entries × 8 bytes)
  ├── Attr0: Y position, mode, color mode, shape
  ├── Attr1: X position, flip/affine param, size
  └── Attr2: Tile number, priority, palette

For each of 128 OAM entries:
  1. Check if sprite is on current scanline (Y <= line < Y + height)
  2. For each pixel in sprite width:
     - Apply H/V flip (regular) or affine transform (affine sprites)
     - Fetch tile from OBJ VRAM (starts at VRAM + 0x10000)
     - Look up color in OBJ palette (palette + 0x200)
     - Write to OBJ line buffer if higher priority than existing pixel

OBJ size table (shape × size → width × height):
  Square:     8×8   16×16  32×32  64×64
  Horizontal: 16×8  32×8   32×16  64×32
  Vertical:   8×16  8×32   16×32  32×64

Tile mapping modes (DISPCNT bit 6):
  1D: tiles are sequential in memory (tile_base + row_of_tiles * tiles_per_row + col)
  2D: tiles arranged in a 32-tile-wide grid (like a 256-pixel-wide bitmap)
```

### Layer Compositing

```
Priority (0 = highest):

   ┌─────────────┐
   │   Backdrop   │  (palette[0], always behind everything)
   ├─────────────┤
   │ BG3 prio=3  │
   ├─────────────┤
   │ OBJ prio=3  │
   ├─────────────┤
   │ BG2 prio=2  │
   ├─────────────┤
   │ OBJ prio=2  │  Compositing order: lower priority number = on top
   ├─────────────┤  Within same priority: OBJ beats BG, lower BG# beats higher BG#
   │ BG1 prio=1  │
   ├─────────────┤
   │ OBJ prio=1  │
   ├─────────────┤
   │ BG0 prio=0  │
   ├─────────────┤
   │ OBJ prio=0  │  ← topmost (highest priority)
   └─────────────┘
```

Implementation: for each pixel, iterate through all layers and track the best (lowest priority number, with OBJ>BG and lower BG index winning ties).

## Event Scheduler

Instead of checking every subsystem every cycle (wasteful), we use a priority queue:

```
Scheduler {
    timestamp: u64,          // global cycle counter
    events: MinHeap<Event>,  // sorted by fire_time
}

Main loop:
    1. peek next event time
    2. run CPU until that time (or fast-forward if halted)
    3. pop and dispatch event
    4. schedule follow-up events
    5. repeat
```

Events: HBlank, HBlankEnd (new scanline), VBlank, Timer overflow, DMA complete, Audio sample

This is O(log n) per event with typically < 10 events in the queue, so effectively O(1).

The `BinaryHeap` is a max-heap by default, so we reverse the `Ord` implementation to get min-heap behavior (lowest `fire_time` popped first).

## BIOS High-Level Emulation

When no BIOS dump is provided, 23 SWI functions are emulated in Rust:

| SWI | Name | What It Does |
|---|---|---|
| 0x00 | SoftReset | Clear IWRAM, reset stack pointers, jump to ROM/RAM |
| 0x01 | RegisterRamReset | Selective clear of EWRAM, IWRAM, palette, VRAM, OAM |
| 0x02 | Halt | Stop CPU until next interrupt |
| 0x03 | Stop | Deep halt (treated as Halt) |
| 0x04 | IntrWait | Halt until specific IRQ flag(s) set |
| 0x05 | VBlankIntrWait | Halt until VBlank (shorthand for IntrWait(1,1)) |
| 0x06 | Div | Signed 32-bit division: R0/R1 → quotient, remainder, abs |
| 0x07 | DivArm | Same as Div with swapped arguments |
| 0x08 | Sqrt | Integer square root |
| 0x09 | ArcTan | Arctangent (fixed-point) |
| 0x0A | ArcTan2 | Two-argument arctangent |
| 0x0B | CpuSet | Memory copy/fill (16-bit or 32-bit) |
| 0x0C | CpuFastSet | Fast memory copy/fill (32-bit, 8-word aligned) |
| 0x0D | GetBiosChecksum | Returns 0xBAAE187F |
| 0x0E | BgAffineSet | Calculate BG affine matrix from center/scale/angle |
| 0x0F | ObjAffineSet | Calculate OBJ affine matrix from scale/angle |
| 0x10 | BitUnPack | Expand bit width (e.g., 1bpp → 4bpp) |
| 0x11 | LZ77UnCompWram | LZ77 decompression to WRAM (byte writes) |
| 0x12 | LZ77UnCompVram | LZ77 decompression to VRAM (halfword writes) |
| 0x13 | HuffUnComp | Huffman decompression |
| 0x14 | RLUnCompWram | Run-length decompression to WRAM |
| 0x15 | RLUnCompVram | Run-length decompression to VRAM |
| 0x1D | SoundDriverVSync | Sets timer reload for sound mixer sync |

## DMA (Direct Memory Access)

The GBA has 4 DMA channels (0-3) for hardware-driven memory transfers that bypass the CPU.

### Channel Properties

| Channel | Src Range | Dst Range | Max Count | Special Mode |
|---|---|---|---|---|
| DMA0 | 27-bit (internal) | 27-bit (internal) | 0x4000 | — |
| DMA1 | 27-bit | 27-bit | 0x4000 | Sound FIFO A |
| DMA2 | 27-bit | 27-bit | 0x4000 | Sound FIFO B |
| DMA3 | 28-bit (can reach ROM) | 28-bit | 0x10000 | Video Capture |

### Transfer Flow

```
CPU writes DMA control register (enable bit 0→1)
  │
  ├── Latch internal addresses from SAD/DAD/COUNT registers
  │
  ├── If timing = Immediate → execute transfer now
  │   └── Bus::run_dma(channel_id)
  │       ├── Read from internal_sad, write to internal_dad
  │       ├── Advance addresses by ±word_size per step (inc/dec/fixed)
  │       ├── Repeat for internal_count words
  │       └── One-shot: disable channel. Repeat: reload count, stay armed
  │
  ├── If timing = VBlank → arm channel, fire at VBlank event
  ├── If timing = HBlank → arm channel, fire at each HBlank event
  └── If timing = Special:
      ├── DMA1/2: FIFO mode — 4×32-bit words on Timer overflow
      └── DMA3: Video Capture (not yet implemented)
```

### Address Control Modes

| Mode | Source | Destination |
|---|---|---|
| 0: Increment | addr += word_size | addr += word_size |
| 1: Decrement | addr -= word_size | addr -= word_size |
| 2: Fixed | addr unchanged | addr unchanged |
| 3: Inc+Reload | (prohibited) | addr += word_size, reload on repeat |

### Borrow-Checker Solution

DMA needs to read/write Bus memory, but DMA state lives inside Bus. We can't have `dma_controller.transfer(&mut bus)` because that's a double mutable borrow. Solution:

```rust
// Bus::run_dma() executes as a method on Bus itself:
impl Bus {
    pub fn run_dma(&mut self, channel_id: usize) -> (u32, bool) {
        // Read DMA channel state (self.dma.channels[channel_id])
        // Perform memory copies (self.read32() / self.write32())
        // Update DMA channel state
        // All within one &mut self — no conflict
    }
}
```

## Timers

Four 16-bit incrementing timers, each with configurable prescaler and cascade mode.

### Timer Tick Model

```
Each CPU step returns N cycles
  │
  └── tick_timers(N)
      │
      ├── Timer 0 (if enabled, not cascade):
      │   prescaler_counter += N
      │   ticks = prescaler_counter / divider
      │   counter += ticks
      │   if counter overflows → reload, set overflow flag
      │
      ├── Timer 1 (if enabled):
      │   if cascade → increment by Timer 0's overflow count
      │   else → same prescaler logic as Timer 0
      │
      ├── Timer 2, 3 → same pattern
      │
      └── For each overflow:
          ├── If IRQ enabled → request_irq(TimerN)
          └── If Timer 0 or 1 → trigger FIFO DMA (sound sample refill)
```

### Prescaler Values

| Bits 0-1 | Divider | Frequency | Period |
|---|---|---|---|
| 0 | F/1 | 16.78 MHz | ~59.6 ns |
| 1 | F/64 | 262.2 kHz | ~3.81 μs |
| 2 | F/256 | 65.5 kHz | ~15.3 μs |
| 3 | F/1024 | 16.4 kHz | ~61.0 μs |

### Cascade Mode

When bit 2 of TMCNT_H is set, the timer ignores its prescaler and instead increments once per overflow of the previous timer. This allows chaining timers for longer intervals:

```
Timer 0 (prescaler=1024, reload=0) → overflows every ~3.97 seconds
Timer 1 (cascade from 0) → increments once per Timer 0 overflow
→ Timer 1 overflows after 65536 × 3.97s ≈ 72 hours
```

Timer 0 cannot be cascade (it has no "previous timer").

## Audio Architecture

```
┌───────────────────────────────────────────────────────┐
│                        APU                             │
│                                                        │
│  PSG (tick per CPU cycle):                            │
│  ┌──────────┐ ┌──────┐ ┌──────────┐ ┌──────────┐    │
│  │Ch1 Square│ │Ch2   │ │Ch3 Wave  │ │Ch4 Noise │    │
│  │+Sweep    │ │Square│ │32×4-bit  │ │LFSR 7/15 │    │
│  │+Envelope │ │+Envl │ │2 banks   │ │+Envelope │    │
│  └────┬─────┘ └──┬───┘ └────┬─────┘ └────┬─────┘    │
│       └──────────┴──────────┴─────────────┘           │
│       SOUNDCNT_L: L/R panning, volume 0-7             │
│                                                        │
│  FIFO (pop on timer overflow):                        │
│  ┌─────────┐ ┌─────────┐                              │
│  │ FIFO A  │ │ FIFO B  │   DMA1/2 refills when       │
│  │ 32-byte │ │ 32-byte │   count ≤ 16 bytes           │
│  │ Timer0/1│ │ Timer0/1│                               │
│  └────┬────┘ └────┬────┘                               │
│       └───────────┘                                    │
│       SOUNDCNT_H: vol (50%/100%), L/R, timer select   │
│                                                        │
│  Mixer (every 512 CPU cycles = 32768 Hz):             │
│  ┌────────────────────────────────────────┐            │
│  │ PSG×ratio + FIFO_A + FIFO_B + BIAS    │            │
│  │ → clamp 10-bit → scale to i16 stereo  │            │
│  └──────────────────┬─────────────────────┘            │
│                     ▼                                  │
│              sample_buffer (Vec<i16>)                  │
└─────────────────────┬──────────────────────────────────┘
                      ▼
         SDL2 Audio Callback (Arc<Mutex<Vec>>)
         pulls stereo i16 at 32768 Hz
```

### Frame Sequencer (512 Hz = every 32768 CPU cycles)

| Step | Action |
|---|---|
| 0 | Length counters (Ch1-4) |
| 1 | — |
| 2 | Length counters + Sweep (Ch1) |
| 3 | — |
| 4 | Length counters |
| 5 | — |
| 6 | Length counters + Sweep (Ch1) |
| 7 | Volume envelope (Ch1, Ch2, Ch4) |

### Sound Register Map

| Offset | Register | Purpose |
|---|---|---|
| 0x60 | SOUND1CNT_L | Ch1 sweep |
| 0x62 | SOUND1CNT_H | Ch1 duty, length, envelope |
| 0x64 | SOUND1CNT_X | Ch1 frequency, trigger |
| 0x68 | SOUND2CNT_L | Ch2 duty, length, envelope |
| 0x6C | SOUND2CNT_H | Ch2 frequency, trigger |
| 0x70 | SOUND3CNT_L | Ch3 wave bank, DAC enable |
| 0x72 | SOUND3CNT_H | Ch3 length, volume |
| 0x74 | SOUND3CNT_X | Ch3 frequency, trigger |
| 0x78 | SOUND4CNT_L | Ch4 length, envelope |
| 0x7C | SOUND4CNT_H | Ch4 noise params, trigger |
| 0x80 | SOUNDCNT_L | PSG volume/panning |
| 0x82 | SOUNDCNT_H | DMA sound control |
| 0x84 | SOUNDCNT_X | Master enable + status |
| 0x88 | SOUNDBIAS | Bias level |
| 0x90-0x9F | Wave RAM | Ch3 waveform data |
| 0xA0 | FIFO_A | DMA sound A data |
| 0xA4 | FIFO_B | DMA sound B data |

## Windows and Color Effects

### Window Regions

The GBA supports three overlapping window regions that control per-pixel layer visibility:

```
Screen (240×160)
┌──────────────────────────────────────────┐
│  Outside (WINOUT bits 0-5)               │
│    ┌────────────────────────┐            │
│    │  WIN0 (WININ bits 0-5) │            │
│    │   ┌──────────┐        │            │
│    │   │  WIN1    │        │            │
│    │   │(WININ    │        │            │
│    │   │bits 8-13)│        │            │
│    │   └──────────┘        │            │
│    └────────────────────────┘            │
│        OBJWIN (WINOUT bits 8-13)         │
│        [wherever OBJ Window sprites are] │
└──────────────────────────────────────────┘

Priority: WIN0 > WIN1 > OBJWIN > Outside
Each region's 6-bit flags: [BG0, BG1, BG2, BG3, OBJ, Effects]
```

### Color Special Effects Pipeline

```
BLDCNT register:
  ├── 1st target flags (bits 0-5): which layers can be the top pixel for blending
  ├── Mode (bits 6-7): None / Alpha / BrightnessUp / BrightnessDown
  └── 2nd target flags (bits 8-13): which layers can be the bottom pixel for blending

Per-pixel decision:
  ┌─ Is top pixel a semi-transparent OBJ (gfx_mode=1)?
  │  YES → alpha blend with 2nd pixel (if 2nd target), ignoring 1st target flags
  │
  ├─ Is mode = Alpha?
  │  YES → if top is 1st target AND second is 2nd target → blend
  │         Result = min(31, (C1*EVA + C2*EVB) / 16) per R/G/B
  │
  ├─ Is mode = BrightnessIncrease?
  │  YES → if top is 1st target → C + (31-C)*EVY/16 per R/G/B (fade to white)
  │
  └─ Is mode = BrightnessDecrease?
     YES → if top is 1st target → C - C*EVY/16 per R/G/B (fade to black)
```

EVA, EVB (alpha coefficients): 0-16, from BLDALPHA register
EVY (brightness coefficient): 0-16, from BLDY register

## Backup Save Detection

ROMs contain ASCII strings indicating their save type:

| String in ROM | Backup Type | Size |
|---|---|---|
| `SRAM_V` | SRAM | 32 KB |
| `FLASH_V` / `FLASH512_V` | Flash | 64 KB |
| `FLASH1M_V` | Flash | 128 KB |
| `EEPROM_V` | EEPROM | 512 B or 8 KB |

Implementation: `detect_backup_type()` scans the entire ROM as UTF-8 lossy and checks for these substrings. Returns a `BackupMedia` enum variant.

### Flash Command Protocol

```
Idle → Write 0xAA @ 0x5555
     → Write 0x55 @ 0x2AAA
     → Write command @ 0x5555:
         0x90 → Chip ID mode (read manufacturer/device at addr 0/1)
         0xF0 → Exit / Reset
         0xA0 → Write byte mode (next write stores data)
         0x80 → Prepare erase (needs second 0xAA/0x55 sequence):
                 → 0x10 @ 0x5555 = full chip erase
                 → 0x30 @ sector = 4KB sector erase
         0xB0 → Bank switch (128KB only, write 0/1 to 0x0000)
```

### EEPROM Serial Protocol

```
Write: [10] + [address: 6 or 14 bits] + [64 data bits MSB-first] + [dummy]
Read:  [11] + [address: 6 or 14 bits] + [dummy]
       → output: [0000] + [64 data bits MSB-first]

Address width auto-detected on first access:
  6-bit  → 512B  (64 blocks × 8 bytes)
  14-bit → 8KB   (1024 blocks × 8 bytes)
```

## Save States

All core state derives `serde::Serialize` + `Deserialize`. Save states are:

```
Gba::save_state()
  → bincode::serialize(&self)    // ~400KB raw (all CPU, memory, PPU, APU, DMA, timer state)
    → zstd::encode(level=3)     // ~50-100KB compressed
      → write to <rom>.state

Gba::load_state(data)
  → zstd::decode
    → bincode::deserialize::<Gba>
      → *self = state            // Exact restoration of all emulator state
```

Hotkeys: **]** = save state, **[** = load state.

### What's serialized

Every field in the `Gba` struct is captured:
- CPU: all 16 registers, CPSR, SPSR, banked registers (6 modes), pipeline state, halted flag
- Bus: EWRAM (256KB), IWRAM (32KB), VRAM (96KB), palette (1KB), OAM (1KB), all I/O registers
- PPU: affine reference points
- APU: all 4 PSG channel state (timers, envelopes, duty pos, LFSR), FIFO buffers + positions, mixer state
- DMA: 4 channel states (internal addresses, counts, control)
- Timers: 4 timer states (counters, prescaler accumulators)
- Scheduler: timestamp + event queue
- Backup media: full SRAM/Flash/EEPROM contents + Flash state machine + EEPROM serial state

## Crate Separation

```
gba-core (library, ~9500 lines)
├── Pure emulation logic
├── No platform dependencies (no SDL2, no filesystem)
├── All state serializable via serde
└── Usable by any frontend (SDL2, WASM, headless testing)

gba-frontend (binary, ~550 lines)
├── SDL2 window management (240×160 scaled with configurable factor)
├── 15-bit GBA color → 24-bit RGB conversion
├── Keyboard input mapping (Z=A, X=B, arrows=dpad, A/S=L/R)
├── Frame timing (~59.737 Hz via sleep)
├── Audio output (32768 Hz stereo via SDL2 callback)
├── Diagnostic hotkeys (env-gated debug probes)
└── Save file I/O (.sav auto-load/save, .state with zstd compression)
```

This separation means the emulator core could be compiled to WASM with a web frontend, or run headless for automated testing, without changing any core code.
