//! Flash backup memory (64KB Atmel / 128KB Sanyo/Macronix).
//!
//! Flash uses a command sequence protocol:
//!   1. Write 0xAA to 0x5555
//!   2. Write 0x55 to 0x2AAA
//!   3. Write command byte to 0x5555
//!
//! Commands:
//!   0x90 — Enter Chip ID mode (reads manufacturer/device ID)
//!   0xF0 — Exit Chip ID / Terminate command
//!   0x80 — Erase (followed by second sequence: 0x30=sector erase, 0x10=chip erase)
//!   0xA0 — Write byte (next write stores the byte)
//!   0xB0 — Bank switch (128KB only: next write to 0x0000 selects bank 0 or 1)

use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::sync::Mutex;

static TRACE_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

fn trace_log(msg: &str) {
    if std::env::var("FLASH_TRACE").is_err() {
        return;
    }
    let mut guard = TRACE_FILE.lock().unwrap();
    if guard.is_none() {
        let path = std::env::var("FLASH_TRACE_FILE").unwrap_or_else(|_| "/tmp/flash.log".to_string());
        if let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(&path) {
            *guard = Some(f);
        }
    }
    if let Some(f) = guard.as_mut() {
        let _ = writeln!(f, "[FLASH] {}", msg);
        let _ = f.flush();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum FlashState {
    Ready,
    Cmd1,           // Received 0xAA at 0x5555
    Cmd2,           // Received 0x55 at 0x2AAA
    ChipId,         // In chip ID mode
    PrepareErase,   // Received erase command (0x80), waiting for second sequence
    Erase1,         // In erase: received 0xAA at 0x5555
    Erase2,         // In erase: received 0x55 at 0x2AAA
    WriteByte,      // Next write stores a byte
    BankSwitch,     // Next write to 0x0000 selects bank
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flash {
    pub data: Vec<u8>,
    state: FlashState,
    /// Current bank (0 or 1, only relevant for 128KB flash)
    bank: u8,
    /// Total size (64KB or 128KB)
    size: usize,
    /// Cycles remaining during which `is_busy()` should return true after
    /// the most recent write. Lets the EXPERIMENT_GATE block IRQs across
    /// the entire save loop, not just the brief command-sequence window.
    #[serde(default, skip)]
    busy_cycles: u32,
    /// Chip ID we report to the game (manufacturer, device).
    /// Selected at construction time so that the value appears in the
    /// game's own list of supported chips — see `pick_chip_id`.
    #[serde(default)]
    reported_id: (u8, u8),
}

impl Flash {
    pub fn new(size: usize) -> Self {
        Self::new_with_rom(size, None)
    }

    /// Construct a Flash chip with the given size, optionally scanning the
    /// ROM to pick a chip ID the game will accept.
    pub fn new_with_rom(size: usize, rom: Option<&[u8]>) -> Self {
        let reported_id = pick_chip_id(size, rom);
        Flash {
            data: vec![0xFF; size],
            state: FlashState::Ready,
            bank: 0,
            size,
            busy_cycles: 0,
            reported_id,
        }
    }

    fn is_128k(&self) -> bool {
        self.size > 64 * 1024
    }

    /// True when flash is mid-command-sequence OR was written recently.
    /// Used by an experimental IRQ gate to test whether vblank IRQs during
    /// Pokémon's flash save loop are corrupting state.
    pub fn is_busy(&self) -> bool {
        self.state != FlashState::Ready || self.busy_cycles > 0
    }

    /// Decrement the sticky-busy counter as CPU cycles elapse.
    pub fn tick(&mut self, cycles: u32) {
        self.busy_cycles = self.busy_cycles.saturating_sub(cycles);
    }

    /// Manufacturer + Device ID we report to the game when it queries
    /// the chip in identification mode. Selected at construction time
    /// based on the ROM's chip-ID table (see `pick_chip_id`).
    fn chip_id(&self) -> (u8, u8) {
        self.reported_id
    }

    fn bank_offset(&self) -> usize {
        if self.is_128k() { self.bank as usize * 0x10000 } else { 0 }
    }

    pub fn read(&self, addr: u32) -> u8 {
        let offset = (addr & 0xFFFF) as usize;

        if self.state == FlashState::ChipId {
            let (manufacturer, device) = self.chip_id();
            let v = match offset {
                0 => manufacturer,
                1 => device,
                _ => 0,
            };
            trace_log(&format!("CHIP-ID read 0x{:04X} → 0x{:02X}", offset, v));
            return v;
        }

        let index = self.bank_offset() + offset;
        let v = if index < self.data.len() { self.data[index] } else { 0xFF };
        if std::env::var("FLASH_TRACE_READS").is_ok() {
            trace_log(&format!("read 0x{:04X} → 0x{:02X}  bank={}", offset, v, self.bank));
        }
        v
    }

    pub fn write(&mut self, addr: u32, val: u8) {
        let offset = (addr & 0xFFFF) as usize;

        // Sticky window: keep is_busy() true for ~200k cycles after any
        // write, so the EXPERIMENT_GATE can hold IRQs across the entire
        // save loop (Pokémon writes ~57k bytes spaced over many cycles).
        self.busy_cycles = 200_000;

        trace_log(&format!(
            "state={:?} write 0x{:04X} = 0x{:02X}  bank={}",
            self.state, offset, val, self.bank
        ));

        match self.state {
            FlashState::Ready => {
                if offset == 0x5555 && val == 0xAA {
                    self.state = FlashState::Cmd1;
                }
            }
            FlashState::Cmd1 => {
                if offset == 0x2AAA && val == 0x55 {
                    self.state = FlashState::Cmd2;
                } else {
                    self.state = FlashState::Ready;
                }
            }
            FlashState::Cmd2 => {
                if offset == 0x5555 {
                    match val {
                        0x90 => self.state = FlashState::ChipId,
                        0xF0 => self.state = FlashState::Ready,
                        0x80 => self.state = FlashState::PrepareErase,
                        0xA0 => self.state = FlashState::WriteByte,
                        0xB0 if self.is_128k() => self.state = FlashState::BankSwitch,
                        _ => self.state = FlashState::Ready,
                    }
                } else {
                    self.state = FlashState::Ready;
                }
            }
            FlashState::ChipId => {
                // Any write of 0xF0 exits chip ID mode
                if val == 0xF0 {
                    self.state = FlashState::Ready;
                } else if offset == 0x5555 && val == 0xAA {
                    self.state = FlashState::Cmd1;
                }
            }
            FlashState::PrepareErase => {
                if offset == 0x5555 && val == 0xAA {
                    self.state = FlashState::Erase1;
                } else {
                    self.state = FlashState::Ready;
                }
            }
            FlashState::Erase1 => {
                if offset == 0x2AAA && val == 0x55 {
                    self.state = FlashState::Erase2;
                } else {
                    self.state = FlashState::Ready;
                }
            }
            FlashState::Erase2 => {
                if offset == 0x5555 && val == 0x10 {
                    // Chip erase: fill all data with 0xFF
                    self.data.fill(0xFF);
                    self.state = FlashState::Ready;
                } else if val == 0x30 {
                    // Sector erase: erase 4KB sector
                    let sector = offset & 0xF000;
                    let base = self.bank_offset() + sector;
                    let end = (base + 0x1000).min(self.data.len());
                    if base < self.data.len() {
                        self.data[base..end].fill(0xFF);
                    }
                    self.state = FlashState::Ready;
                } else {
                    self.state = FlashState::Ready;
                }
            }
            FlashState::WriteByte => {
                let index = self.bank_offset() + offset;
                if index < self.data.len() {
                    // Flash write can only clear bits (AND with existing)
                    self.data[index] &= val;
                }
                self.state = FlashState::Ready;
            }
            FlashState::BankSwitch => {
                if offset == 0x0000 {
                    self.bank = val & 1;
                }
                self.state = FlashState::Ready;
            }
        }
    }
}

/// Each GBA flash cartridge has a 16-bit chip ID (manufacturer | device
/// in little-endian halfword order). When a save-capable game boots, it
/// puts the chip into ID mode (write 0xAA / 0x55 / 0x90), reads the ID,
/// and looks it up in a built-in table of supported chips. If the ID
/// isn't in the table the game treats save hardware as broken and
/// often refuses to advance past early init.
///
/// We only have one chip in the box, so we pick a chip ID up front:
/// scan the ROM for any of the well-known IDs of the right size class
/// and use the first one we find. If nothing matches we fall back to
/// the most commonly-supported chip in each size class (Panasonic for
/// 64 KB, Macronix MX29L1100B for 128 KB).
///
/// Returned as (manufacturer, device).
fn pick_chip_id(size: usize, rom: Option<&[u8]>) -> (u8, u8) {
    // Candidate chip IDs in preference order, (manufacturer, device).
    // Both halves combined as little-endian halfword = mfr | (dev<<8).
    let candidates_64k: &[(u8, u8, &str)] = &[
        (0x32, 0x1B, "Panasonic MN63F805MNP"),
        (0xBF, 0xD4, "SST 39VF512"),
        (0xC2, 0x1C, "Macronix MX29L512"),
        (0x62, 0x13, "Sanyo LE26FV10N1TS"),
        (0x1F, 0x3D, "Atmel AT29LV512"),
    ];
    let candidates_128k: &[(u8, u8, &str)] = &[
        (0xC2, 0x09, "Macronix MX29L1100B"),
        (0xC2, 0x1C, "Macronix MX29L010"),
        (0x62, 0x13, "Sanyo LE26FV10N1TS"),
    ];
    let candidates: &[(u8, u8, &str)] = if size > 64 * 1024 {
        candidates_128k
    } else {
        candidates_64k
    };

    if let Some(rom) = rom {
        // Halfword-aligned scan for any candidate ID that appears in the
        // ROM. The chip-ID table stores each supported chip's ID as a
        // halfword (LDRH at +0x28 in the dispatch we saw), so a hit means
        // the ROM's table includes that chip.
        for &(mfr, dev, _name) in candidates {
            let id_le = (mfr as u16) | ((dev as u16) << 8);
            let lo = id_le as u8;
            let hi = (id_le >> 8) as u8;
            // Walk halfword-aligned offsets. Bail out early on first hit.
            for i in (0..rom.len().saturating_sub(2)).step_by(2) {
                if rom[i] == lo && rom[i + 1] == hi {
                    return (mfr, dev);
                }
            }
        }
    }

    // Default: first candidate. Panasonic for 64 KB, Macronix for 128 KB.
    let (mfr, dev, _) = candidates[0];
    (mfr, dev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flash_chip_id_64k() {
        let mut flash = Flash::new(64 * 1024);
        // Enter chip ID mode
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0x90);

        assert_eq!(flash.read(0), 0x32); // Panasonic manufacturer
        assert_eq!(flash.read(1), 0x1B); // Panasonic MN63F805MNP device

        // Exit chip ID
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0xF0);

        // Should read normal data now (0xFF = erased)
        assert_eq!(flash.read(0), 0xFF);
    }

    #[test]
    fn test_flash_write_byte() {
        let mut flash = Flash::new(64 * 1024);
        // Write 0x42 to address 0x100
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0xA0);
        flash.write(0x0100, 0x42);

        assert_eq!(flash.read(0x0100), 0x42);
    }

    #[test]
    fn test_flash_sector_erase() {
        let mut flash = Flash::new(64 * 1024);
        // Write a byte first
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0xA0);
        flash.write(0x0100, 0x42);
        assert_eq!(flash.read(0x0100), 0x42);

        // Erase sector 0 (0x0000-0x0FFF)
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0x80);
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x0000, 0x30);

        assert_eq!(flash.read(0x0100), 0xFF); // Erased
    }

    #[test]
    fn test_flash_128k_bank_switch() {
        let mut flash = Flash::new(128 * 1024);

        // Write to bank 0
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0xA0);
        flash.write(0x0000, 0xAA);

        // Switch to bank 1
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0xB0);
        flash.write(0x0000, 0x01);

        // Write to bank 1
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0xA0);
        flash.write(0x0000, 0xBB);

        // Read bank 1
        assert_eq!(flash.read(0x0000), 0xBB);

        // Switch back to bank 0 and verify
        flash.write(0x5555, 0xAA);
        flash.write(0x2AAA, 0x55);
        flash.write(0x5555, 0xB0);
        flash.write(0x0000, 0x00);

        assert_eq!(flash.read(0x0000), 0xAA);
    }
}
