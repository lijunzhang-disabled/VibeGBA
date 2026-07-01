pub mod sram;
pub mod flash;
pub mod eeprom;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackupMedia {
    None,
    Sram(sram::Sram),
    Flash(flash::Flash),
    Eeprom(eeprom::Eeprom),
}

impl BackupMedia {
    /// Read a byte from the backup chip. Takes `&mut self` because EEPROM reads
    /// have a side effect: each read shifts out the next serial bit. SRAM/Flash
    /// reads are side-effect free.
    pub fn read(&mut self, addr: u32) -> u8 {
        match self {
            BackupMedia::None => 0xFF,
            BackupMedia::Sram(s) => s.read(addr),
            BackupMedia::Flash(f) => f.read(addr),
            BackupMedia::Eeprom(e) => e.read_bit(),
        }
    }

    pub fn write(&mut self, addr: u32, val: u8) {
        match self {
            BackupMedia::None => {}
            BackupMedia::Sram(s) => s.write(addr, val),
            BackupMedia::Flash(f) => f.write(addr, val),
            BackupMedia::Eeprom(e) => e.write(addr, val),
        }
    }

    /// True when backup hardware is mid-command-sequence (not idle).
    pub fn is_busy(&self) -> bool {
        match self {
            BackupMedia::Flash(f) => f.is_busy(),
            _ => false,
        }
    }

    /// Tick the backup chip's internal timers.
    pub fn tick(&mut self, cycles: u32) {
        if let BackupMedia::Flash(f) = self {
            f.tick(cycles);
        }
    }

    /// Export raw save data for .sav file.
    pub fn to_raw(&self) -> Option<Vec<u8>> {
        match self {
            BackupMedia::None => None,
            BackupMedia::Sram(s) => Some(s.data.clone()),
            BackupMedia::Flash(f) => Some(f.data.clone()),
            BackupMedia::Eeprom(e) => Some(e.data.clone()),
        }
    }
}

/// Detect backup type by scanning ROM for signature strings.
pub fn detect_backup_type(rom: &[u8]) -> BackupMedia {
    let rom_str = String::from_utf8_lossy(rom);

    // SRAM_V<ver> is the standard SRAM signature. SRAM_F_V<ver> is the FRAM
    // variant used by some games (e.g. Fire Emblem 7) — functionally
    // identical 32KB SRAM-mapped save storage at 0x0E00_0000.
    if rom_str.contains("SRAM_V") || rom_str.contains("SRAM_F_V") {
        BackupMedia::Sram(sram::Sram::new())
    } else if rom_str.contains("FLASH1M_V") {
        BackupMedia::Flash(flash::Flash::new_with_rom(128 * 1024, Some(rom)))
    } else if rom_str.contains("FLASH_V") || rom_str.contains("FLASH512_V") {
        BackupMedia::Flash(flash::Flash::new_with_rom(64 * 1024, Some(rom)))
    } else if rom_str.contains("EEPROM_V") {
        BackupMedia::Eeprom(eeprom::Eeprom::new())
    } else {
        BackupMedia::None
    }
}
