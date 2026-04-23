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
    pub fn read(&self, addr: u32) -> u8 {
        match self {
            BackupMedia::None => 0xFF,
            BackupMedia::Sram(s) => s.read(addr),
            BackupMedia::Flash(f) => f.read(addr),
            BackupMedia::Eeprom(e) => e.read(addr),
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

    if rom_str.contains("SRAM_V") {
        BackupMedia::Sram(sram::Sram::new())
    } else if rom_str.contains("FLASH1M_V") {
        BackupMedia::Flash(flash::Flash::new(128 * 1024))
    } else if rom_str.contains("FLASH_V") || rom_str.contains("FLASH512_V") {
        BackupMedia::Flash(flash::Flash::new(64 * 1024))
    } else if rom_str.contains("EEPROM_V") {
        BackupMedia::Eeprom(eeprom::Eeprom::new())
    } else {
        BackupMedia::None
    }
}
