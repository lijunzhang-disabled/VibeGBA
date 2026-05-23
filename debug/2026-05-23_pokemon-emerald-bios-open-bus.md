# Pokémon Emerald cave auto-warp: BIOS open-bus on read from NULL

Date: 2026-05-23
Status: **Fixed**

## Symptom

In Pokémon Emerald (BPEE US), Granite Cave entrance puzzle:

1. Player falls through a hole on Map 1.
2. Lands on Map 2 (= map group/num `(24, 8)`) at coords `(11, 28)`.
3. Walkthrough videos show the player standing freely on this tile —
   it's an `MB_LADDER` you can walk off.
4. **Our emulator immediately fired another warp**, dropping the player
   to Map 3 (`(24, 9)`) without any input.

User cross-checked many Chinese- and English-community walkthroughs; the
extra warp is unique to our emulator.

## Investigation log

A long, layered trace — kept here because each layer ruled out a class of
bugs and the chain of "next layer up" probes is what eventually located
the bug.

### 1. Confirm the warp is being committed in ROM, not synthesised

Used the existing `MEM_WATCH=1 MEM_WATCH_LO=0x020322E4 MEM_WATCH_HI=0x020322EC`
to capture every write to `gWarpDestination` (the in-RAM struct read by
the warp-fade transition). Confirmed the buggy warp was a real
`(group=24, num=9, warpId=0xFF, x=4, y=21)` commit, with `x/y` derived
from the player's raw tile coords on Map 2 — i.e. the engine was
running a *real* hole-warp path with player coords passed.

### 2. Find the function that commits the warp

PC of the byte writes was `0x08084A3A` (inside `SetWarpDestination`).
Function entry decoded to `0x08084A34`. Hooked the entry: `lr=08084C1D`
came from `SetWarpDestinationToCoords` (the wrapper that pre-loads
`gWarpDestination` as the destination pointer).

### 3. Walk the call chain one frame at a time

Followed `lr` up the stack:

- `SetWarpDestination` @ 0x08084A34 ← called by
- `SetWarpDestinationToCoords` @ 0x08084BEC ← called by
- `SetWarpDestinationToFixedHoleWarp` @ 0x08084EBC ← called by
- a script command handler @ 0x0809A054 ← dispatched by
- the script engine (`ScriptContext_RunScript` @ 0x08098D86) running a
  script in `gScriptContext2` (= `0x03000E40` in BPEE; map scripts).

The handler at 0x0809A054 turned out to be **`ScrCmd_warphole`** (script
opcode 0x3C). Its operands at the active script PC `0x082A834B` were
`0xFF, 0xFF` → it took the "use player coords" branch and called
`SetWarpDestinationToFixedHoleWarp(playerX - 7, playerY - 7)`.

### 4. Identify the trigger

Map 2's script table (gMapHeader.mapScripts = `0x0822DC5E`) had:

```
ON_FRAME_TABLE  -> 0x082A8327
   entry:  var=0x4022  value=0x0000  script=0x082A8337
ON_TRANSITION   -> 0x082A8331
ON_RESUME       -> 0x0822DC6E
```

So every frame on Map 2, the engine checked `var_0x4022 == 0`; if so it
ran the script at `0x082A8337`, which contained the `warphole`. **The
warp is gated by var_0x4022 being zero.** Vanilla expects this var to
be **non-zero** during normal play on Map 2.

The ON_TRANSITION script at `0x082A8331` is supposed to make it so:

```
0x082A8331: 19 22 40 01 00     copyvar 0x4022, 0x0001
0x082A8336: 02                 end
```

`copyvar destVar, srcVar` is supposed to write var 0x4022 := value of var 0x0001.
Since 0x0001 < `VARS_START` (0x4000), `VarGet(0x0001)` returns the literal
value 1 on the conceptual level. The intended effect is `var 0x4022 := 1`,
which arms the on-frame gate (1 != 0 ⇒ don't fire warphole).

### 5. Verify ON_TRANSITION actually ran

`MEM_WATCH` on `gScriptContext1.scriptPtr` (0x03000EC0) showed the
script pointer being set to `0x082A8331` and then advancing one byte at
a time through `0x082A8332..0x082A8337` — exactly the opcode + two
halfword-operand reads + `end`. The script body executed normally.

### 6. Find the actual STRH and locate the bug

But: **no `WR16` write of value `0x0001` (or any value) reached the
vars[] region.** With `MEM_WATCH` widened to cover all of EWRAM, only
sound-engine WR16s appeared. The script ran, but the value it wrote
went nowhere visible — or rather, somewhere wrong.

Located BPEE's `ScrCmd_copyvar` at 0x08099744:

```
PUSH {r4, r5, lr}
r4 = ctx
BL ScriptReadHalfword            ; r0 = destVar (= 0x4022)
r0 = u16(r0)
BL 0x0809D648  (GetVarPointer)    ; r0 = destPtr
r5 = destPtr
r0 = ctx
BL ScriptReadHalfword            ; r0 = srcVar  (= 0x0001)
r0 = u16(r0)
BL 0x0809D648  (GetVarPointer)    ; r0 = srcPtr
LDRH r0, [r0, #0]                 ; r0 = *srcPtr
STRH r0, [r5, #0]                 ; *destPtr = r0
return 0
```

**BPEE's `copyvar` is implemented as `*destPtr = *srcPtr` — not
`*destPtr = VarGet(srcVar)`.** Both halves go through `GetVarPointer`
and the result is dereferenced. For `srcVar = 0x0001` (< `VARS_START`),
`GetVarPointer` returns `NULL`, and the `LDRH r0, [NULL]` reads from
address 0 — the BIOS region.

So the script's intent ("set var to 1") *only* works because of GBA
hardware quirk: when the CPU reads BIOS with PC outside the BIOS
region, the bus returns a byte of the last fetched BIOS instruction
(the BIOS open-bus latch). After `IntrWait`/`VBlankIntrWait` returns
to user code that latch holds `0xE3A02004` — its low halfword `0x2004`
is non-zero, so `var 0x4022 := 0x2004` and the on-frame gate stays disarmed.

### 7. The bug in our emulator

`gba-core/src/bus/mod.rs::read_bios` had a `// TODO` for BIOS read
protection and unconditionally returned actual BIOS bytes regardless
of PC. In HLE-BIOS mode our BIOS image is mostly zero (only the IRQ
handler stub at 0x18..0x2C is populated), so `LDRH r0, [NULL]` returned
`0`. `copyvar` then wrote `0` to var 0x4022, and the on-frame gate fired
every frame on Map 2.

## Root cause

Three independent gaps that combined into the bug:

1. **`read_bios` ignored PC.** No "BIOS protected from non-BIOS PC" check;
   reads from outside-BIOS PC returned the raw BIOS byte instead of the
   latched open-bus value.
2. **`bios_latch` was maintained but never used.** Each BIOS read updated
   `bios_latch`, but the latch was never returned to anyone.
3. **HLE BIOS had no post-stub instruction.** Even with (1) and (2) fixed,
   the ARM pipeline always fetches two instructions ahead — so the
   advance-fetch after `LDMFD` at 0x28 lands at 0x30, *past* the
   stub. Real BIOS has real code there; our HLE image had zeros, so the
   latch ended up at 0 after every IRQ.

## Fix

Three small changes:

### a) `read_bios` honours PC

`gba-core/src/bus/mod.rs`:

```rust
fn read_bios(&mut self, addr: u32) -> u8 {
    if self.last_pc < 0x0000_4000 {
        // PC inside BIOS: return actual bytes and update latch
        // with the 32-bit word containing the read.
        let index = (addr & 0x3FFF) as usize;
        if index + 3 < self.bios.len() {
            let word_idx = index & !3;
            self.bios_latch = u32::from_le_bytes([
                self.bios[word_idx], self.bios[word_idx + 1],
                self.bios[word_idx + 2], self.bios[word_idx + 3],
            ]);
            self.bios[index]
        } else if index < self.bios.len() {
            self.bios[index]
        } else {
            0
        }
    } else {
        // PC outside BIOS: open-bus — return the appropriate byte
        // of the latched word.
        let shift = (addr & 3) * 8;
        ((self.bios_latch >> shift) & 0xFF) as u8
    }
}
```

### b) `refill_pipeline` updates `bus.last_pc` to the fetch PC

`gba-core/src/arm7tdmi/mod.rs::refill_pipeline` previously used the
old `last_pc` (set at the start of the current `step()` call) while
fetching from a brand-new region (e.g. IRQ entry jumping ROM → 0x18).
Without this, the IRQ stub's first instruction was fetched with
`last_pc` still in ROM → open-bus byte 0 → CPU saw `0x00000000` → the
stub never executed → `VBlankIntrWait` never returned → white screen.

```rust
fn refill_pipeline(&mut self, bus: &mut Bus) {
    if self.cpsr.thumb() {
        let pc = self.regs[15] & !1;
        bus.last_pc = pc;          // <— NEW
        self.pipeline[0] = bus.read16(pc) as u32;
        self.pipeline[1] = bus.read16(pc + 2) as u32;
        self.regs[15] = pc + 4;
    } else {
        let pc = self.regs[15] & !3;
        bus.last_pc = pc;          // <— NEW
        self.pipeline[0] = bus.read32(pc);
        self.pipeline[1] = bus.read32(pc + 4);
        self.regs[15] = pc + 8;
    }
    self.pipeline_flushed = false;
}
```

### c) HLE BIOS populates the post-stub fetch slot

`gba-core/src/bus/mod.rs::make_hle_bios` now writes
`0xE3A02004` at BIOS offset 0x30 (the canonical post-`IntrWait` latch
value). The ARM pipeline's advance-fetch after `LDMFD` at 0x28 reads
this word and the latch becomes `0xE3A02004` for the rest of the
session.

## Verification

- Game boots normally (no white screen).
- Walking onto Map 1's ladder warps to Map 2.
- Player can stand and walk freely on Map 2 — **no auto-warp to Map 3**.
- `V` key on Map 2 confirms `var 0x4022 = 0x2004` (the low halfword of the
  BIOS open-bus latch).

## Why this bug only surfaced in the cave

A natural question: the emulator had been running Pokémon Emerald for
thousands of frames before the cave — opening, menus, the overworld,
battles, the entire pre-cave story — without any visible problem. Why
did this BIOS open-bus bug only trigger here?

Four conditions had to align simultaneously:

1. **Game code had to read from BIOS region (0x00..0x3FFF) with PC in
   ROM.** The vast majority of CPU memory accesses in a game don't go
   through `read_bios` at all — they hit ROM, EWRAM, IWRAM, VRAM, OAM,
   palette, or I/O. SWI calls *execute* BIOS code (PC enters BIOS,
   read_bios's "PC in BIOS" branch handles them correctly); they
   don't *read data* from BIOS with PC in ROM. The realistic way to
   land at BIOS-region addresses with PC in ROM is a NULL pointer
   dereference, which game code generally avoids.

2. **Specifically a `*srcPtr` deref where srcPtr happens to be NULL.**
   In BPEE this only occurs in `ScrCmd_copyvar`. Other "set var"
   commands don't have this shape:

   - `setvar destVar, literal` (opcode 0x16) — the literal is read
     straight from script bytecode and stored. No pointer deref. Safe.
   - `VarGet(id)` — if `GetVarPointer(id)` returns NULL, it returns
     `id` itself as the literal. NULL-safe.
   - `ScrCmd_copyvar` (opcode 0x19) — BPEE's compiled body is
     `*destPtr = *srcPtr` (`LDRH r0, [r0]; STRH r0, [r5]`),
     skipping the VarGet NULL check entirely. For
     `srcVar < VARS_START`, `srcPtr = NULL`, the LDRH lands on BIOS
     address 0, and the value depends on the open-bus latch.

   So the bug needs the unusual `copyvar destVar, <low_literal>`
   idiom. Game authors generally prefer `setvar` for literals.

3. **The script's effect had to be checked against zero.** Even when
   `copyvar destVar, <low_literal>` *is* used, it only matters if
   something later cares whether the value is zero or not. Many uses
   write to scratch vars that get overwritten before being read, or
   compare with `>`/`<` ranges where exact zero doesn't matter.
   Map (24, 8)'s `ON_FRAME_TABLE` has a `value == 0` comparison —
   the strictest possible check.

4. **And the comparison had to gate a *destructive* action.** Plenty of
   on-frame entries gate cosmetic effects (animation flips, encounter
   tables, NPC visibility). Map (24, 8) gates a **warphole** — an
   immediate map transition that moves the player. The result is
   instantly visible and breaks navigation, so a "got the wrong value"
   bug here can't hide.

Map (24, 8) is the perfect storm. Other BPEE locations could, in
principle, exhibit the same root cause if they hit the four conditions
above, but the cave's ladder-landing tile is the spot you encounter
in normal gameplay. Until then the bug sat dormant.

## Lessons

1. **A `// TODO` for a niche hardware quirk hid a game-breaking dependency.**
   No game we'd tested before relied on BIOS open-bus, so the TODO sat
   uncovered for a long time. BPEE's `copyvar destVar, <literal>` idiom
   is essentially "read from address 0 and hope the bus returns
   something useful" — a pattern only sensible because the
   ScrCmd_copyvar author knew about BIOS open-bus.

2. **The pipeline fetches two ahead.** When emulating BIOS stubs, you
   have to either fill the bytes past the stub with something sensible
   or accept that the latch ends up wrong. The two-ahead fetch is
   architectural; the latch consequence is per-game.

3. **`bus.last_pc` was a debug-only field that became load-bearing.**
   Previously it was set once per `step()` for `MEM_WATCH` to attribute
   writes. The BIOS open-bus fix made it a correctness signal, which
   meant the pipeline refill (which runs *within* a step) had to keep
   it accurate too.

4. **The investigation went seven layers deep before reaching the bug.**
   Each layer's instrumentation (`MEM_WATCH`, `WARP_TRACE`,
   `TILE_TRACE`, `dump_map_scripts`, `dump_vars`, GetVarPointer
   entry/exit probes) ruled out a category of bug and pointed one
   level up the chain. Worth keeping the frontend `M`/`V`/`S`/`D`
   probes for future BPEE work — they're already paid-for and gated
   to key presses, so cost nothing in steady state.

## Files changed

- `gba-core/src/bus/mod.rs` — `read_bios` PC check; `make_hle_bios`
  post-stub latch word
- `gba-core/src/arm7tdmi/mod.rs` — `refill_pipeline` sets `bus.last_pc`

Debug-only additions kept for future use:

- `gba-core/src/bus/mod.rs::peek8`/`peek32` — side-effect-free reads
  for frontend dumps
- `gba-frontend/src/main.rs::dump_metatile_at_player` (M),
  `dump_vars` (V), `dump_map_scripts` (S), `TILE_TRACE` per-frame poll
