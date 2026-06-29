//! GamePak wait-state + prefetch-buffer cycle model.
//!
//! The ARM7TDMI core charges a flat "1 cycle per access" baseline inside the
//! instruction handlers (see arm.rs/thumb.rs). This module supplies the
//! *extra* cycles on top of that baseline for each memory access, plus the
//! GamePak prefetch buffer that hides ROM latency on sequential code.
//!
//! Why both are needed together: ROM is slow (WAITCNT wait-states), so every
//! opcode fetch would stall the CPU — except the prefetch unit fetches ahead
//! sequentially during the CPU's internal/non-ROM cycles, so sequential ROM
//! code runs near full speed and only branches / ROM data accesses pay the
//! non-sequential penalty. Modelling wait-states without the prefetch buffer
//! over-slows ROM-resident code (e.g. the M4A sound engine) — which is exactly
//! what regressed Pokémon Emerald audio in the earlier naive attempt.

use serde::{Deserialize, Serialize};

/// Per-region extra access cycles (above the 1-cycle baseline), for 16-bit
/// accesses. 32-bit accesses to a 16-bit bus cost N + S (two halfword beats).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitTable {
    /// ROM wait-state regions WS0/WS1/WS2 (0x08/0x0A/0x0C), [N16, S16] extras.
    pub rom_n16: [u32; 3],
    pub rom_s16: [u32; 3],
    /// SRAM (0x0E) extra cycles (8-bit bus, single access).
    pub sram: u32,
    /// GamePak prefetch buffer enabled (WAITCNT bit 14).
    pub prefetch_enabled: bool,
}

impl WaitTable {
    pub fn new() -> Self {
        // WAITCNT reset value 0 → WS0 N=4,S=2 ; WS1 N=4,S=4 ; WS2 N=4,S=8 cycles
        // total. "Extra above 1": below.
        let mut t = WaitTable {
            rom_n16: [0; 3],
            rom_s16: [0; 3],
            sram: 0,
            prefetch_enabled: false,
        };
        t.update(0);
        t
    }

    /// First-access (non-sequential) wait cycles by 2-bit field: 4,3,2,8 total
    /// → extra above 1 = 3,2,1,7.
    #[inline]
    fn n_extra(field: u16) -> u32 {
        match field & 3 {
            0 => 3,
            1 => 2,
            2 => 1,
            _ => 7,
        }
    }

    /// Recompute the tables from a WAITCNT value (0x4000204).
    pub fn update(&mut self, waitcnt: u16) {
        // WS0: N bits 2-3, S bit 4 (2/1 cycles total → extra 1/0)
        self.rom_n16[0] = Self::n_extra(waitcnt >> 2);
        self.rom_s16[0] = if waitcnt & (1 << 4) != 0 { 0 } else { 1 };
        // WS1: N bits 5-6, S bit 7 (4/1 total → extra 3/0)
        self.rom_n16[1] = Self::n_extra(waitcnt >> 5);
        self.rom_s16[1] = if waitcnt & (1 << 7) != 0 { 0 } else { 3 };
        // WS2: N bits 8-9, S bit 10 (8/1 total → extra 7/0)
        self.rom_n16[2] = Self::n_extra(waitcnt >> 8);
        self.rom_s16[2] = if waitcnt & (1 << 10) != 0 { 0 } else { 7 };
        // SRAM bits 0-1 (4,3,2,8 total → extra 3,2,1,7)
        self.sram = Self::n_extra(waitcnt & 3);
        self.prefetch_enabled = waitcnt & (1 << 14) != 0;
    }

    /// ROM wait-state region index for an address (0x08/0x09 → WS0, 0x0A/0x0B →
    /// WS1, 0x0C/0x0D → WS2).
    #[inline]
    pub fn rom_region(addr: u32) -> usize {
        match (addr >> 24) & 0x0F {
            0x08 | 0x09 => 0,
            0x0A | 0x0B => 1,
            _ => 2,
        }
    }
}

/// Prefetch-buffer state. The unit fetches sequential halfwords ahead of the
/// CPU while the CPU is busy with internal cycles or non-ROM accesses, banking
/// "credit" (cycles already spent) that makes the next sequential ROM opcode
/// fetch cheap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prefetch {
    /// Cycles the prefetch unit has run ahead (capped at the cost of a full
    /// 8-halfword buffer). Consumed by sequential opcode fetches.
    pub credit: u32,
    /// True if the previous bus activity left the prefetcher streaming
    /// sequentially from ROM (so the next sequential fetch can use credit).
    pub streaming: bool,
}

impl Prefetch {
    pub fn new() -> Self {
        Prefetch { credit: 0, streaming: false }
    }

    /// Background-advance the prefetcher by `cycles` of CPU activity that did
    /// not use the ROM bus (internal cycles, non-ROM data accesses). Only
    /// accrues credit while streaming sequentially.
    #[inline]
    pub fn advance(&mut self, cycles: u32, max_credit: u32) {
        if self.streaming {
            self.credit = (self.credit + cycles).min(max_credit);
        }
    }

    /// A non-sequential access (branch target, ROM data access) empties the
    /// buffer and restarts streaming from the new point.
    #[inline]
    pub fn restart(&mut self) {
        self.credit = 0;
        self.streaming = true;
    }
}
