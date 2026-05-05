pub mod io_regs;

use crate::apu::Apu;
use crate::backup::{self, BackupMedia};
use crate::dma::DmaController;
use crate::interrupt::InterruptController;
use crate::keypad::Keypad;
use crate::ppu::Ppu;
use crate::rtc::Rtc;
use crate::timer::Timers;
use io_regs::IoRegisters;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Cached MEM_WATCH state — env::var is too expensive to call on every
/// memory write. Returns (enabled, lo, hi). When disabled, lo == hi.
fn mem_watch_range() -> (bool, u32, u32) {
    static V: OnceLock<(bool, u32, u32)> = OnceLock::new();
    *V.get_or_init(|| {
        let enabled = std::env::var("MEM_WATCH").is_ok();
        let lo = std::env::var("MEM_WATCH_LO")
            .ok().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x0200_1940);
        let hi = std::env::var("MEM_WATCH_HI")
            .ok().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x0200_194A);
        (enabled, lo, hi)
    })
}

fn dispcnt_trace_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("DISPCNT_TRACE").is_ok())
}

#[derive(Serialize, Deserialize)]
pub struct Bus {
    /// 16KB BIOS ROM
    bios: Vec<u8>,
    /// 256KB external work RAM (16-bit bus, 3-cycle access)
    ewram: Vec<u8>,
    /// 32KB internal work RAM (32-bit bus, 1-cycle access)
    pub(crate) iwram: Vec<u8>,
    /// I/O registers
    pub io: IoRegisters,
    /// PPU state
    pub ppu: Ppu,
    /// APU state
    pub apu: Apu,
    /// 1KB palette RAM (BG palette + OBJ palette, 256 colors each)
    pub palette: Vec<u8>,
    /// 96KB Video RAM
    pub vram: Vec<u8>,
    /// 1KB OAM (128 OBJ attributes + 32 affine parameter sets)
    pub oam: Vec<u8>,
    /// Game Pak ROM (up to 32MB)
    rom: Vec<u8>,
    /// Backup save media (SRAM/Flash/EEPROM)
    pub backup: BackupMedia,
    /// DMA controller
    pub dma: DmaController,
    /// Timers
    pub timers: Timers,
    /// Interrupt controller
    pub interrupt: InterruptController,
    /// Keypad
    pub keypad: Keypad,
    /// Cartridge RTC (Pokémon gen 3 etc.)
    pub rtc: Rtc,
    /// Open bus: last value read (for unmapped reads)
    last_read: u32,
    /// BIOS open bus latch (protects BIOS memory)
    bios_latch: u32,
    /// Whether we have a BIOS loaded
    pub has_bios: bool,
    /// Set when HALTCNT is written — CPU should halt
    pub halt_requested: bool,
    /// Last PC the CPU was executing — set by CPU at top of step().
    /// Used by MEM_WATCH and similar diagnostics. Not authoritative for
    /// emulation correctness; debug-only.
    pub last_pc: u32,
}

impl Bus {
    pub fn new(bios: Option<Vec<u8>>, rom: Vec<u8>) -> Self {
        let has_bios = bios.is_some();
        let bios_data = bios.unwrap_or_else(|| make_hle_bios());
        let backup = backup::detect_backup_type(&rom);
        let mut rtc = Rtc::new();
        rtc.enabled = Rtc::detect(&rom);

        Bus {
            bios: bios_data,
            ewram: vec![0; 0x40000],
            iwram: vec![0; 0x8000],
            io: IoRegisters::new(),
            ppu: Ppu::new(),
            apu: Apu::new(),
            palette: vec![0; 0x400],
            vram: vec![0; 0x18000],
            oam: vec![0; 0x400],
            rom,
            backup,
            dma: DmaController::new(),
            timers: Timers::new(),
            interrupt: InterruptController::new(),
            keypad: Keypad::new(),
            rtc,
            last_read: 0,
            bios_latch: 0,
            has_bios,
            halt_requested: false,
            last_pc: 0,
        }
    }

    // ─── 8-bit reads ──────────────────────────────────────────────

    pub fn read8(&mut self, addr: u32) -> u8 {
        let val = match addr >> 24 {
            0x00 => self.read_bios(addr),
            0x02 => self.ewram[(addr & 0x3FFFF) as usize],
            0x03 => self.iwram[(addr & 0x7FFF) as usize],
            0x04 => self.read_io8(addr),
            0x05 => self.palette[(addr & 0x3FF) as usize],
            0x06 => self.read_vram8(addr),
            0x07 => self.oam[(addr & 0x3FF) as usize],
            0x08..=0x0D => self.read_rom8(addr),
            0x0E..=0x0F => self.backup.read(addr & 0xFFFF),
            _ => (self.last_read & 0xFF) as u8,
        };
        self.last_read = val as u32;
        val
    }

    // ─── 16-bit reads ─────────────────────────────────────────────

    pub fn read16(&mut self, addr: u32) -> u16 {
        let addr = addr & !1; // Force halfword alignment
        let val = match addr >> 24 {
            0x00 => {
                let lo = self.read_bios(addr) as u16;
                let hi = self.read_bios(addr + 1) as u16;
                lo | (hi << 8)
            }
            0x02 => {
                let base = (addr & 0x3FFFF) as usize;
                u16::from_le_bytes([self.ewram[base], self.ewram[base + 1]])
            }
            0x03 => {
                let base = (addr & 0x7FFF) as usize;
                u16::from_le_bytes([self.iwram[base], self.iwram[base + 1]])
            }
            0x04 => self.read_io16(addr),
            0x05 => {
                let base = (addr & 0x3FF) as usize;
                u16::from_le_bytes([self.palette[base], self.palette[base + 1]])
            }
            0x06 => self.read_vram16(addr),
            0x07 => {
                let base = (addr & 0x3FF) as usize;
                u16::from_le_bytes([self.oam[base], self.oam[base + 1]])
            }
            0x08..=0x0D => self.read_rom16(addr),
            0x0E..=0x0F => {
                // SRAM/Flash is on an 8-bit bus: 16-bit reads broadcast the
                // single byte to both halves of the result.
                let b = self.backup.read(addr & 0xFFFF) as u16;
                b | (b << 8)
            }
            _ => self.last_read as u16,
        };
        self.last_read = val as u32;
        val
    }

    // ─── 32-bit reads ─────────────────────────────────────────────

    pub fn read32(&mut self, addr: u32) -> u32 {
        let addr = addr & !3; // Force word alignment
        let val = match addr >> 24 {
            0x02 => {
                let base = (addr & 0x3FFFF) as usize;
                u32::from_le_bytes([
                    self.ewram[base],
                    self.ewram[base + 1],
                    self.ewram[base + 2],
                    self.ewram[base + 3],
                ])
            }
            0x03 => {
                let base = (addr & 0x7FFF) as usize;
                u32::from_le_bytes([
                    self.iwram[base],
                    self.iwram[base + 1],
                    self.iwram[base + 2],
                    self.iwram[base + 3],
                ])
            }
            0x04 => {
                let lo = self.read_io16(addr) as u32;
                let hi = self.read_io16(addr + 2) as u32;
                lo | (hi << 16)
            }
            0x05 => {
                let base = (addr & 0x3FF) as usize;
                u32::from_le_bytes([
                    self.palette[base],
                    self.palette[base + 1],
                    self.palette[base + 2],
                    self.palette[base + 3],
                ])
            }
            0x06 => {
                let lo = self.read_vram16(addr) as u32;
                let hi = self.read_vram16(addr + 2) as u32;
                lo | (hi << 16)
            }
            0x07 => {
                let base = (addr & 0x3FF) as usize;
                u32::from_le_bytes([
                    self.oam[base],
                    self.oam[base + 1],
                    self.oam[base + 2],
                    self.oam[base + 3],
                ])
            }
            0x08..=0x0D => {
                let lo = self.read_rom16(addr) as u32;
                let hi = self.read_rom16(addr + 2) as u32;
                lo | (hi << 16)
            }
            0x0E..=0x0F => {
                // SRAM/Flash is on an 8-bit bus: 32-bit reads broadcast the
                // single byte to all four positions of the result.
                let b = self.backup.read(addr & 0xFFFF) as u32;
                b | (b << 8) | (b << 16) | (b << 24)
            }
            _ => {
                // Fall through: read two 16-bit values
                let lo = self.read16(addr) as u32;
                let hi = self.read16(addr + 2) as u32;
                lo | (hi << 16)
            }
        };
        self.last_read = val;
        val
    }

    // ─── 8-bit writes ─────────────────────────────────────────────

    pub fn write8(&mut self, addr: u32, val: u8) {
        let (mw, lo, hi) = mem_watch_range();
        if mw && addr >= lo && addr < hi {
            eprintln!("[WR8 ] 0x{:08X} = 0x{:02X}  pc=0x{:08X}", addr, val, self.last_pc);
        }
        match addr >> 24 {
            0x02 => self.ewram[(addr & 0x3FFFF) as usize] = val,
            0x03 => self.iwram[(addr & 0x7FFF) as usize] = val,
            0x04 => self.write_io8(addr, val),
            0x05 => {
                // 8-bit palette writes: write the byte to both the lower and upper byte
                // of the addressed halfword (mirrors the byte)
                let base = (addr & 0x3FE) as usize;
                self.palette[base] = val;
                self.palette[base + 1] = val;
            }
            0x06 => {
                // 8-bit VRAM writes: similar mirroring behavior
                // In bitmap modes, writes to OBJ VRAM region are ignored
                let offset = self.vram_addr(addr);
                if offset + 1 < self.vram.len() {
                    self.vram[offset] = val;
                    self.vram[offset + 1] = val;
                }
            }
            // 8-bit OAM writes are ignored
            0x07 => {}
            0x08..=0x0D => {
                // GPIO byte writes — forward to RTC. Games rarely write 8-bit to GPIO
                // but we handle it by reading the current 16-bit value and merging.
                if self.rtc.enabled {
                    let rel = addr & 0x01FF_FFFF;
                    let reg_addr = rel & !1;
                    let reg_off = match reg_addr {
                        0xC4 => Some(0u32),
                        0xC6 => Some(2),
                        0xC8 => Some(4),
                        _ => None,
                    };
                    if let Some(off) = reg_off {
                        let cur = self.rtc.read_reg(off);
                        let new = if rel & 1 == 0 {
                            (cur & 0xFF00) | val as u16
                        } else {
                            (cur & 0x00FF) | ((val as u16) << 8)
                        };
                        self.rtc.write_reg(off, new);
                    }
                }
            }
            0x0E..=0x0F => self.backup.write(addr & 0xFFFF, val),
            _ => {}
        }
    }

    // ─── 16-bit writes ────────────────────────────────────────────

    pub fn write16(&mut self, addr: u32, val: u16) {
        let (mw, lo, hi) = mem_watch_range();
        if mw && addr >= lo && addr < hi {
            eprintln!("[WR16] 0x{:08X} = 0x{:04X}  pc=0x{:08X}", addr, val, self.last_pc);
        }
        let addr = addr & !1;
        let bytes = val.to_le_bytes();
        match addr >> 24 {
            0x02 => {
                let base = (addr & 0x3FFFF) as usize;
                self.ewram[base] = bytes[0];
                self.ewram[base + 1] = bytes[1];
            }
            0x03 => {
                let base = (addr & 0x7FFF) as usize;
                self.iwram[base] = bytes[0];
                self.iwram[base + 1] = bytes[1];
            }
            0x04 => self.write_io16(addr, val),
            0x05 => {
                let base = (addr & 0x3FF) as usize;
                self.palette[base] = bytes[0];
                self.palette[base + 1] = bytes[1];
            }
            0x06 => {
                let offset = self.vram_addr(addr);
                if offset + 1 < self.vram.len() {
                    self.vram[offset] = bytes[0];
                    self.vram[offset + 1] = bytes[1];
                }
            }
            0x07 => {
                let base = (addr & 0x3FF) as usize;
                self.oam[base] = bytes[0];
                self.oam[base + 1] = bytes[1];
            }
            0x08..=0x0D => {
                // Cartridge writes — only handled for GPIO (RTC) registers.
                if self.rtc.enabled {
                    let rel = addr & 0x01FF_FFFE;
                    match rel {
                        0xC4 => self.rtc.write_reg(0, val),
                        0xC6 => self.rtc.write_reg(2, val),
                        0xC8 => self.rtc.write_reg(4, val),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // ─── 32-bit writes ────────────────────────────────────────────

    pub fn write32(&mut self, addr: u32, val: u32) {
        let (mw, lo, hi) = mem_watch_range();
        if mw && addr >= lo && addr < hi {
            eprintln!("[WR32] 0x{:08X} = 0x{:08X}  pc=0x{:08X}", addr, val, self.last_pc);
        }
        let addr = addr & !3;
        let bytes = val.to_le_bytes();
        match addr >> 24 {
            0x02 => {
                let base = (addr & 0x3FFFF) as usize;
                self.ewram[base..base + 4].copy_from_slice(&bytes);
            }
            0x03 => {
                let base = (addr & 0x7FFF) as usize;
                self.iwram[base..base + 4].copy_from_slice(&bytes);
            }
            0x04 => {
                self.write_io16(addr, val as u16);
                self.write_io16(addr + 2, (val >> 16) as u16);
            }
            0x05 => {
                let base = (addr & 0x3FF) as usize;
                self.palette[base..base + 4].copy_from_slice(&bytes);
            }
            0x06 => {
                let offset = self.vram_addr(addr);
                if offset + 3 < self.vram.len() {
                    self.vram[offset..offset + 4].copy_from_slice(&bytes);
                }
            }
            0x07 => {
                let base = (addr & 0x3FF) as usize;
                self.oam[base..base + 4].copy_from_slice(&bytes);
            }
            _ => {
                self.write16(addr, val as u16);
                self.write16(addr + 2, (val >> 16) as u16);
            }
        }
    }

    pub fn iwram_ref(&self) -> &[u8] { &self.iwram }
    pub fn iwram_mut(&mut self) -> &mut [u8] { &mut self.iwram }
    pub fn ewram_ref(&self) -> &[u8] { &self.ewram }

    /// True when the backup chip is mid-command-sequence. Used by the
    /// EXPERIMENT_GATE in cpu::step() to test the "IRQs during flash"
    /// hypothesis.
    pub fn backup_busy(&self) -> bool {
        self.backup.is_busy()
    }

    /// Tick backup chip's sticky-busy timer.
    pub fn tick_backup(&mut self, cycles: u32) {
        self.backup.tick(cycles);
    }

    // ─── DMA execution ─────────────────────────────────────────────

    /// Execute a DMA transfer for the given channel.
    /// Called directly on Bus since it owns both DMA state and memory.
    /// Returns (cycles, irq_requested).
    pub fn run_dma(&mut self, channel_id: usize) -> (u32, bool) {
        use crate::dma::AddrControl;

        let ch = &self.dma.channels[channel_id];
        if !ch.enabled() || !ch.active {
            return (0, false);
        }
        self.dma.channels[channel_id].run_count += 1;
        if std::env::var("DMA_FIRE_TRACE").is_ok() && channel_id == 1 {
            // Print stack-ish info: this fire happened, with FIFO count for context
            let fifo_count = self.apu.fifo_a.count;
            let fifo_b_count = self.apu.fifo_b.count;
            let refill_req_a = self.apu.fifo_a.refill_request_count;
            let refill_req_b = self.apu.fifo_b.refill_request_count;
            let timing = self.dma.channels[1].timing();
            eprintln!(
                "  run_dma(1) timing={:?} fifo_a_count={} fifo_b_count={} \
                 refill_req_a={} refill_req_b={}",
                timing, fifo_count, fifo_b_count, refill_req_a, refill_req_b
            );
        }
        let ch = &self.dma.channels[channel_id];

        let word32 = ch.word_size_32();
        let word_size: u32 = if word32 { 4 } else { 2 };
        let count = ch.internal_count;
        let irq_on_done = ch.irq_enabled();
        let is_repeat = ch.repeat() && ch.timing() != crate::dma::DmaTiming::Immediate;

        let src_step: i32 = match ch.src_control() {
            AddrControl::Increment | AddrControl::IncrementReload => word_size as i32,
            AddrControl::Decrement => -(word_size as i32),
            AddrControl::Fixed => 0,
        };
        let dst_step: i32 = match ch.dest_control() {
            AddrControl::Increment | AddrControl::IncrementReload => word_size as i32,
            AddrControl::Decrement => -(word_size as i32),
            AddrControl::Fixed => 0,
        };

        // Check for FIFO special mode (DMA1/2 with Special timing)
        let is_fifo = (channel_id == 1 || channel_id == 2)
            && ch.timing() == crate::dma::DmaTiming::Special;

        let mut src = ch.internal_sad;
        let mut dst = ch.internal_dad;

        if is_fifo {
            // FIFO: transfer 4 x 32-bit words, dest fixed, src increments
            for _ in 0..4 {
                let val = self.read32(src);
                self.write32(dst, val);
                src = src.wrapping_add(4);
            }
            self.dma.channels[channel_id].internal_sad = src;
            return (4, irq_on_done);
        }

        // Normal transfer
        for _ in 0..count {
            if word32 {
                let val = self.read32(src & !3);
                self.write32(dst & !3, val);
            } else {
                let val = self.read16(src & !1);
                self.write16(dst & !1, val);
            }
            src = (src as i32).wrapping_add(src_step) as u32;
            dst = (dst as i32).wrapping_add(dst_step) as u32;
        }

        // Update internal addresses
        self.dma.channels[channel_id].internal_sad = src;
        self.dma.channels[channel_id].internal_dad = dst;

        if is_repeat {
            self.dma.channels[channel_id].reload_for_repeat(channel_id);
        } else {
            // Disable channel
            self.dma.channels[channel_id].control &= !(1 << 15);
            self.dma.channels[channel_id].active = false;
        }

        (count, irq_on_done)
    }

    /// Write DMA control register, handling enable-bit transition.
    /// Returns Some(channel_id) if an immediate DMA should run.
    pub fn write_dma_control(&mut self, channel_id: usize, value: u16) -> Option<usize> {
        self.dma.write_control(channel_id, value)
    }

    // ─── Internal helpers ─────────────────────────────────────────

    fn read_bios(&mut self, addr: u32) -> u8 {
        // TODO: proper BIOS read protection (only readable when PC is in BIOS)
        let index = (addr & 0x3FFF) as usize;
        if index < self.bios.len() {
            let val = self.bios[index];
            self.bios_latch = val as u32;
            val
        } else {
            0
        }
    }

    fn read_vram8(&self, addr: u32) -> u8 {
        let offset = self.vram_addr(addr);
        if offset < self.vram.len() {
            self.vram[offset]
        } else {
            0
        }
    }

    fn read_vram16(&self, addr: u32) -> u16 {
        let offset = self.vram_addr(addr & !1);
        if offset + 1 < self.vram.len() {
            u16::from_le_bytes([self.vram[offset], self.vram[offset + 1]])
        } else {
            0
        }
    }

    /// VRAM address with mirroring. VRAM is 96KB (0x18000 bytes).
    /// Addresses 0x06000000-0x06017FFF map to VRAM[0..0x18000].
    /// Addresses 0x06018000-0x0601FFFF mirror VRAM[0x10000..0x18000] (last 32KB).
    fn vram_addr(&self, addr: u32) -> usize {
        let offset = (addr & 0x1FFFF) as usize;
        if offset >= 0x18000 {
            offset - 0x8000 // Mirror: 0x18000-0x1FFFF -> 0x10000-0x17FFF
        } else {
            offset
        }
    }

    fn read_rom8(&self, addr: u32) -> u8 {
        // GPIO registers for cartridges with RTC etc. (0x080000C4..0x080000C8)
        if self.rtc.enabled {
            let rel = addr & 0x01FF_FFFF;
            if rel == 0xC4 { return self.rtc.read_reg(0) as u8; }
            if rel == 0xC5 { return (self.rtc.read_reg(0) >> 8) as u8; }
            if rel == 0xC6 { return self.rtc.read_reg(2) as u8; }
            if rel == 0xC7 { return (self.rtc.read_reg(2) >> 8) as u8; }
            if rel == 0xC8 { return self.rtc.read_reg(4) as u8; }
            if rel == 0xC9 { return (self.rtc.read_reg(4) >> 8) as u8; }
        }

        let offset = (addr & 0x01FF_FFFF) as usize;
        if offset < self.rom.len() {
            self.rom[offset]
        } else {
            // Out of bounds: return open bus (last prefetch)
            ((offset >> 1) & 0xFF) as u8
        }
    }

    fn read_rom16(&self, addr: u32) -> u16 {
        // GPIO registers (halfword reads)
        if self.rtc.enabled {
            let rel = addr & 0x01FF_FFFE;
            if rel == 0xC4 { return self.rtc.read_reg(0); }
            if rel == 0xC6 { return self.rtc.read_reg(2); }
            if rel == 0xC8 { return self.rtc.read_reg(4); }
        }

        let offset = (addr & 0x01FF_FFFE) as usize;
        if offset + 1 < self.rom.len() {
            u16::from_le_bytes([self.rom[offset], self.rom[offset + 1]])
        } else {
            // Out-of-bounds ROM read: return open bus
            (offset >> 1) as u16
        }
    }

    // ─── I/O register reads ───────────────────────────────────────

    fn read_io8(&mut self, addr: u32) -> u8 {
        let val16 = self.read_io16(addr & !1);
        if addr & 1 == 0 {
            val16 as u8
        } else {
            (val16 >> 8) as u8
        }
    }

    fn read_io16(&mut self, addr: u32) -> u16 {
        match addr & 0x3FF {
            0x000 => self.io.dispcnt,
            0x002 => self.io.green_swap,
            0x004 => self.io.dispstat,
            0x006 => self.io.vcount,
            0x008 => self.io.bgcnt[0],
            0x00A => self.io.bgcnt[1],
            0x00C => self.io.bgcnt[2],
            0x00E => self.io.bgcnt[3],
            // BG offsets are write-only
            0x010..=0x01E => 0,
            // Affine params are write-only
            0x020..=0x03E => 0,
            0x040 => self.io.winh[0],
            0x042 => self.io.winh[1],
            0x044 => self.io.winv[0],
            0x046 => self.io.winv[1],
            0x048 => self.io.winin,
            0x04A => self.io.winout,
            0x04C => self.io.mosaic,
            0x050 => self.io.bldcnt,
            0x052 => self.io.bldalpha,
            // BLDY is write-only
            0x054 => 0,
            // Sound registers
            0x060..=0x0A8 => {
                let offset = (addr & 0x3FF) - 0x60;
                self.apu.read_reg(offset as u16)
            }
            // DMA (write-only except for control which returns current state)
            0x0B0..=0x0DE => 0, // TODO: DMA register reads
            // Timers
            0x100 => self.timers.read_counter(0),
            0x102 => self.timers.timers[0].control,
            0x104 => self.timers.read_counter(1),
            0x106 => self.timers.timers[1].control,
            0x108 => self.timers.read_counter(2),
            0x10A => self.timers.timers[2].control,
            0x10C => self.timers.read_counter(3),
            0x10E => self.timers.timers[3].control,
            // Keypad
            0x130 => self.keypad.read_keyinput(),
            0x132 => self.keypad.keycnt,
            // Interrupt
            0x200 => self.interrupt.read_ie(),
            0x202 => self.interrupt.read_if(),
            0x208 => self.interrupt.read_ime(),
            // WAITCNT
            0x204 => self.io.waitcnt,
            // POSTFLG
            0x300 => self.io.postflg as u16,
            _ => 0,
        }
    }

    // ─── I/O register writes ──────────────────────────────────────

    fn write_io8(&mut self, addr: u32, val: u8) {
        // Special case: HALTCNT at 0x04000301
        if addr & 0x3FF == 0x301 {
            self.io.haltcnt = val;
            self.halt_requested = true;
            return;
        }

        // For most I/O registers, 8-bit writes affect the appropriate byte
        // of the 16-bit register. Read-modify-write.
        let aligned = addr & !1;
        let current = self.read_io16(aligned);
        let new_val = if addr & 1 == 0 {
            (current & 0xFF00) | val as u16
        } else {
            (current & 0x00FF) | ((val as u16) << 8)
        };
        self.write_io16(aligned, new_val);
    }

    fn write_io16(&mut self, addr: u32, val: u16) {
        match addr & 0x3FF {
            0x000 => {
                if dispcnt_trace_enabled() && self.io.dispcnt != val {
                    eprintln!("[DISPCNT] 0x{:04X} → 0x{:04X}", self.io.dispcnt, val);
                }
                self.io.dispcnt = val;
            }
            0x002 => self.io.green_swap = val,
            0x004 => {
                // DISPSTAT: bits 0-2 are read-only (VBlank, HBlank, VCount match)
                // Only bits 3-15 are writable
                self.io.dispstat = (self.io.dispstat & 0x07) | (val & !0x07);
            }
            // VCOUNT is read-only
            0x006 => {}
            0x008 => self.io.bgcnt[0] = val,
            0x00A => self.io.bgcnt[1] = val,
            0x00C => self.io.bgcnt[2] = val,
            0x00E => self.io.bgcnt[3] = val,
            // BG offsets
            0x010 => self.io.bg_ofs[0][0] = val & 0x1FF,
            0x012 => self.io.bg_ofs[0][1] = val & 0x1FF,
            0x014 => self.io.bg_ofs[1][0] = val & 0x1FF,
            0x016 => self.io.bg_ofs[1][1] = val & 0x1FF,
            0x018 => self.io.bg_ofs[2][0] = val & 0x1FF,
            0x01A => self.io.bg_ofs[2][1] = val & 0x1FF,
            0x01C => self.io.bg_ofs[3][0] = val & 0x1FF,
            0x01E => self.io.bg_ofs[3][1] = val & 0x1FF,
            // BG2 affine
            0x020 => self.io.bg2_affine[0] = val,
            0x022 => self.io.bg2_affine[1] = val,
            0x024 => self.io.bg2_affine[2] = val,
            0x026 => self.io.bg2_affine[3] = val,
            0x028 => self.io.write_bg_ref_low(2, 0, val),
            0x02A => self.io.write_bg_ref_high(2, 0, val),
            0x02C => self.io.write_bg_ref_low(2, 1, val),
            0x02E => self.io.write_bg_ref_high(2, 1, val),
            // BG3 affine
            0x030 => self.io.bg3_affine[0] = val,
            0x032 => self.io.bg3_affine[1] = val,
            0x034 => self.io.bg3_affine[2] = val,
            0x036 => self.io.bg3_affine[3] = val,
            0x038 => self.io.write_bg_ref_low(3, 0, val),
            0x03A => self.io.write_bg_ref_high(3, 0, val),
            0x03C => self.io.write_bg_ref_low(3, 1, val),
            0x03E => self.io.write_bg_ref_high(3, 1, val),
            // Window
            0x040 => self.io.winh[0] = val,
            0x042 => self.io.winh[1] = val,
            0x044 => self.io.winv[0] = val,
            0x046 => self.io.winv[1] = val,
            0x048 => self.io.winin = val,
            0x04A => self.io.winout = val,
            0x04C => self.io.mosaic = val,
            0x050 => self.io.bldcnt = val,
            0x052 => self.io.bldalpha = val,
            0x054 => self.io.bldy = val,
            // Sound registers
            0x060..=0x0A8 => {
                let offset = (addr & 0x3FF) - 0x60;
                self.apu.write_reg(offset as u16, val);
            }
            // DMA registers
            0x0B0 => self.dma.channels[0].sad = (self.dma.channels[0].sad & 0xFFFF0000) | val as u32,
            0x0B2 => self.dma.channels[0].sad = (self.dma.channels[0].sad & 0x0000FFFF) | ((val as u32) << 16),
            0x0B4 => self.dma.channels[0].dad = (self.dma.channels[0].dad & 0xFFFF0000) | val as u32,
            0x0B6 => self.dma.channels[0].dad = (self.dma.channels[0].dad & 0x0000FFFF) | ((val as u32) << 16),
            0x0B8 => self.dma.channels[0].count = val,
            0x0BA => {
                if let Some(_ch) = self.write_dma_control(0, val) {
                    self.run_dma(0);
                }
            }
            // DMA1-3 follow same pattern at +12 byte offsets
            0x0BC => self.dma.channels[1].sad = (self.dma.channels[1].sad & 0xFFFF0000) | val as u32,
            0x0BE => self.dma.channels[1].sad = (self.dma.channels[1].sad & 0x0000FFFF) | ((val as u32) << 16),
            0x0C0 => self.dma.channels[1].dad = (self.dma.channels[1].dad & 0xFFFF0000) | val as u32,
            0x0C2 => self.dma.channels[1].dad = (self.dma.channels[1].dad & 0x0000FFFF) | ((val as u32) << 16),
            0x0C4 => self.dma.channels[1].count = val,
            0x0C6 => {
                if let Some(_ch) = self.write_dma_control(1, val) {
                    if std::env::var("DMA_FIRE_TRACE").is_ok() {
                        eprintln!("    [from CPU write to DMA1 control / Immediate]");
                    }
                    self.run_dma(1);
                }
            }
            0x0C8 => self.dma.channels[2].sad = (self.dma.channels[2].sad & 0xFFFF0000) | val as u32,
            0x0CA => self.dma.channels[2].sad = (self.dma.channels[2].sad & 0x0000FFFF) | ((val as u32) << 16),
            0x0CC => self.dma.channels[2].dad = (self.dma.channels[2].dad & 0xFFFF0000) | val as u32,
            0x0CE => self.dma.channels[2].dad = (self.dma.channels[2].dad & 0x0000FFFF) | ((val as u32) << 16),
            0x0D0 => self.dma.channels[2].count = val,
            0x0D2 => {
                if let Some(_ch) = self.write_dma_control(2, val) {
                    self.run_dma(2);
                }
            }
            0x0D4 => self.dma.channels[3].sad = (self.dma.channels[3].sad & 0xFFFF0000) | val as u32,
            0x0D6 => self.dma.channels[3].sad = (self.dma.channels[3].sad & 0x0000FFFF) | ((val as u32) << 16),
            0x0D8 => self.dma.channels[3].dad = (self.dma.channels[3].dad & 0xFFFF0000) | val as u32,
            0x0DA => self.dma.channels[3].dad = (self.dma.channels[3].dad & 0x0000FFFF) | ((val as u32) << 16),
            0x0DC => self.dma.channels[3].count = val,
            0x0DE => {
                if let Some(_ch) = self.write_dma_control(3, val) {
                    self.run_dma(3);
                }
            }
            // Timers
            0x100 => self.timers.write_reload(0, val),
            0x102 => self.timers.write_control(0, val),
            0x104 => self.timers.write_reload(1, val),
            0x106 => self.timers.write_control(1, val),
            0x108 => self.timers.write_reload(2, val),
            0x10A => self.timers.write_control(2, val),
            0x10C => self.timers.write_reload(3, val),
            0x10E => self.timers.write_control(3, val),
            // Keypad control
            0x132 => self.keypad.keycnt = val,
            // Interrupts
            0x200 => self.interrupt.write_ie(val),
            0x202 => self.interrupt.write_if(val),
            0x208 => self.interrupt.write_ime(val),
            // WAITCNT
            0x204 => self.io.waitcnt = val,
            // HALTCNT (written via 0x04000301, 8-bit write)
            0x300 => self.io.postflg = val as u8,
            _ => {
                log::trace!("Unhandled I/O write: 0x{:08X} = 0x{:04X}", addr, val);
            }
        }
    }
}

/// Build a minimal "fake BIOS" blob (16KB) containing exception vectors and
/// a working IRQ handler stub. Used when no real BIOS dump is provided.
///
/// The GBA exception vector table is at offset 0 of the BIOS:
///   0x00: Reset
///   0x04: Undefined
///   0x08: SWI
///   0x0C: Prefetch abort
///   0x10: Data abort
///   0x14: Reserved
///   0x18: IRQ
///   0x1C: FIQ
///
/// The standard BIOS IRQ handler saves R0-R3, R12, LR, reads the user IRQ
/// vector pointer from [0x03FFFFFC] (mirror of 0x03007FFC), calls it, then
/// restores and returns via SUBS PC, LR, #4.
fn make_hle_bios() -> Vec<u8> {
    let mut bios = vec![0u8; 0x4000];

    // IRQ handler stub installed at 0x18:
    //   0x18: STMFD SP!, {R0-R3, R12, LR}    E92D500F
    //   0x1C: MOV   R0, #0x04000000           E3A00404
    //   0x20: ADD   LR, PC, #0                E28FE000   ; LR = 0x28
    //   0x24: LDR   PC, [R0, #-4]             E510F004   ; PC = [0x03FFFFFC]
    //   0x28: LDMFD SP!, {R0-R3, R12, LR}    E8BD500F
    //   0x2C: SUBS  PC, LR, #4                E25EF004   ; return from IRQ
    let stub: [(u32, u32); 6] = [
        (0x18, 0xE92D500F),
        (0x1C, 0xE3A00404),
        (0x20, 0xE28FE000),
        (0x24, 0xE510F004),
        (0x28, 0xE8BD500F),
        (0x2C, 0xE25EF004),
    ];

    for (addr, opcode) in stub.iter() {
        let a = *addr as usize;
        let bytes = opcode.to_le_bytes();
        bios[a] = bytes[0];
        bios[a + 1] = bytes[1];
        bios[a + 2] = bytes[2];
        bios[a + 3] = bytes[3];
    }

    bios
}
