use serde::{Deserialize, Serialize};

/// All GBA I/O registers (0x04000000 - 0x040003FE).
/// Organized by subsystem for readability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoRegisters {
    // LCD Control
    /// 0x000 - DISPCNT: LCD Control
    pub dispcnt: u16,
    /// 0x002 - Undocumented (Green Swap)
    pub green_swap: u16,
    /// 0x004 - DISPSTAT: General LCD Status (STAT, LYC)
    pub dispstat: u16,
    /// 0x006 - VCOUNT: Vertical Counter (read-only, set by PPU)
    pub vcount: u16,

    // BG Control
    /// 0x008-0x00E - BG0CNT through BG3CNT
    pub bgcnt: [u16; 4],
    /// 0x010-0x01E - BG scroll offsets: [BG][X=0/Y=1]
    pub bg_ofs: [[u16; 2]; 4],

    // BG Affine (BG2 and BG3)
    /// 0x020-0x026 - BG2 rotation/scaling parameters (PA, PB, PC, PD)
    pub bg2_affine: [u16; 4],
    /// 0x028-0x02C - BG2 reference point X/Y (28-bit signed, written as two 16-bit halves)
    pub bg2x_latch: i32,
    pub bg2y_latch: i32,
    /// 0x030-0x036 - BG3 rotation/scaling parameters
    pub bg3_affine: [u16; 4],
    /// 0x038-0x03C - BG3 reference point X/Y
    pub bg3x_latch: i32,
    pub bg3y_latch: i32,

    // Window
    /// 0x040-0x042 - WIN0H, WIN1H
    pub winh: [u16; 2],
    /// 0x044-0x046 - WIN0V, WIN1V
    pub winv: [u16; 2],
    /// 0x048 - WININ
    pub winin: u16,
    /// 0x04A - WINOUT
    pub winout: u16,

    // Mosaic
    /// 0x04C - MOSAIC
    pub mosaic: u16,

    // Color Special Effects
    /// 0x050 - BLDCNT
    pub bldcnt: u16,
    /// 0x052 - BLDALPHA
    pub bldalpha: u16,
    /// 0x054 - BLDY (write-only)
    pub bldy: u16,

    // Sound registers (Phase 6 - store raw values for now)
    pub sound_regs: Vec<u8>, // 0x060-0x0A8 (roughly, 80 bytes)

    // DMA registers are in DmaController
    // Timer registers are in Timers

    // Serial/Keypad handled by their own modules

    // System Control
    /// 0x204 - WAITCNT
    pub waitcnt: u16,
    /// 0x300 - POSTFLG / HALTCNT
    pub postflg: u8,
    pub haltcnt: u8,
}

impl IoRegisters {
    pub fn new() -> Self {
        IoRegisters {
            dispcnt: 0,
            green_swap: 0,
            dispstat: 0,
            vcount: 0,
            bgcnt: [0; 4],
            bg_ofs: [[0; 2]; 4],
            bg2_affine: [0; 4],
            bg2x_latch: 0,
            bg2y_latch: 0,
            bg3_affine: [0; 4],
            bg3x_latch: 0,
            bg3y_latch: 0,
            winh: [0; 2],
            winv: [0; 2],
            winin: 0,
            winout: 0,
            mosaic: 0,
            bldcnt: 0,
            bldalpha: 0,
            bldy: 0,
            sound_regs: vec![0; 0x50],
            waitcnt: 0,
            postflg: 0,
            haltcnt: 0,
        }
    }

    /// Sign-extend a 28-bit value to i32.
    fn sign_extend_28(val: u32) -> i32 {
        if val & (1 << 27) != 0 {
            (val | 0xF000_0000) as i32
        } else {
            val as i32
        }
    }

    /// Write to a BG reference point register (low 16 bits).
    pub fn write_bg_ref_low(&mut self, bg: usize, coord: usize, val: u16) {
        let latch = match (bg, coord) {
            (2, 0) => &mut self.bg2x_latch,
            (2, 1) => &mut self.bg2y_latch,
            (3, 0) => &mut self.bg3x_latch,
            (3, 1) => &mut self.bg3y_latch,
            _ => return,
        };
        *latch = (*latch & !0xFFFF) | val as i32;
        *latch = Self::sign_extend_28(*latch as u32);
    }

    /// Write to a BG reference point register (high 16 bits).
    pub fn write_bg_ref_high(&mut self, bg: usize, coord: usize, val: u16) {
        let latch = match (bg, coord) {
            (2, 0) => &mut self.bg2x_latch,
            (2, 1) => &mut self.bg2y_latch,
            (3, 0) => &mut self.bg3x_latch,
            (3, 1) => &mut self.bg3y_latch,
            _ => return,
        };
        *latch = (*latch & 0xFFFF) | ((val as i32) << 16);
        *latch = Self::sign_extend_28(*latch as u32);
    }
}
