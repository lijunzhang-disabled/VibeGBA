//! PSG (Programmable Sound Generator) channels — CGB-compatible.
//!
//! ## Channel 1: Square wave with frequency sweep and volume envelope
//! ## Channel 2: Square wave with volume envelope (no sweep)
//! ## Channel 3: Programmable waveform (32 x 4-bit samples, two banks)
//! ## Channel 4: Noise (LFSR-based, configurable width)
//!
//! The frame sequencer runs at 512 Hz (CPU_CLOCK / 32768) and drives:
//! - Step 0: Length counter
//! - Step 1: (nothing)
//! - Step 2: Length counter, Sweep (Ch1)
//! - Step 3: (nothing)
//! - Step 4: Length counter
//! - Step 5: (nothing)
//! - Step 6: Length counter, Sweep (Ch1)
//! - Step 7: Volume envelope

use serde::{Deserialize, Serialize};

/// Duty cycle waveforms for square channels (8 steps each).
const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1], // 12.5%
    [1, 0, 0, 0, 0, 0, 0, 1], // 25%
    [1, 0, 0, 0, 0, 1, 1, 1], // 50%
    [0, 1, 1, 1, 1, 1, 1, 0], // 75%
];

// ─── Channel 1: Square + Sweep ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel1 {
    pub enabled: bool,
    // Sweep (NR10 = 0x04000060)
    pub sweep_period: u8,
    pub sweep_negate: bool,
    pub sweep_shift: u8,
    sweep_timer: u8,
    sweep_shadow: u16,
    sweep_enabled: bool,
    // Duty/Length (NR11 = 0x04000062)
    pub duty: u8,
    pub length_load: u8,
    length_counter: u16,
    pub length_enabled: bool,
    // Envelope (NR12 = 0x04000063)
    pub envelope_init: u8,
    pub envelope_dir: bool, // true = increase
    pub envelope_period: u8,
    envelope_volume: u8,
    envelope_timer: u8,
    // Frequency (NR13/NR14 = 0x04000064-65)
    pub frequency: u16, // 11-bit
    // Internal
    timer: u32,
    duty_pos: u8,
}

impl Channel1 {
    pub fn new() -> Self {
        Channel1 {
            enabled: false,
            sweep_period: 0, sweep_negate: false, sweep_shift: 0,
            sweep_timer: 0, sweep_shadow: 0, sweep_enabled: false,
            duty: 0, length_load: 0, length_counter: 0, length_enabled: false,
            envelope_init: 0, envelope_dir: false, envelope_period: 0,
            envelope_volume: 0, envelope_timer: 0,
            frequency: 0, timer: 0, duty_pos: 0,
        }
    }

    pub fn trigger(&mut self) {
        self.enabled = true;
        if self.length_counter == 0 { self.length_counter = 64; }
        self.timer = (2048 - self.frequency as u32) * 4;
        self.envelope_volume = self.envelope_init;
        self.envelope_timer = self.envelope_period;
        self.sweep_shadow = self.frequency;
        self.sweep_timer = if self.sweep_period > 0 { self.sweep_period } else { 8 };
        self.sweep_enabled = self.sweep_period > 0 || self.sweep_shift > 0;
        if self.sweep_shift > 0 {
            let _ = self.calc_sweep_freq(); // Overflow check
        }
        if self.envelope_init == 0 && !self.envelope_dir {
            self.enabled = false;
        }
    }

    fn calc_sweep_freq(&mut self) -> u16 {
        let delta = self.sweep_shadow >> self.sweep_shift;
        let new_freq = if self.sweep_negate {
            self.sweep_shadow.wrapping_sub(delta)
        } else {
            self.sweep_shadow + delta
        };
        if new_freq > 2047 { self.enabled = false; }
        new_freq
    }

    pub fn clock_sweep(&mut self) {
        if self.sweep_timer > 0 { self.sweep_timer -= 1; }
        if self.sweep_timer == 0 {
            self.sweep_timer = if self.sweep_period > 0 { self.sweep_period } else { 8 };
            if self.sweep_enabled && self.sweep_period > 0 {
                let new_freq = self.calc_sweep_freq();
                if new_freq <= 2047 && self.sweep_shift > 0 {
                    self.sweep_shadow = new_freq;
                    self.frequency = new_freq;
                    let _ = self.calc_sweep_freq(); // Overflow check again
                }
            }
        }
    }

    pub fn clock_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 { self.enabled = false; }
        }
    }

    pub fn clock_envelope(&mut self) {
        if self.envelope_period == 0 { return; }
        if self.envelope_timer > 0 { self.envelope_timer -= 1; }
        if self.envelope_timer == 0 {
            self.envelope_timer = self.envelope_period;
            if self.envelope_dir && self.envelope_volume < 15 {
                self.envelope_volume += 1;
            } else if !self.envelope_dir && self.envelope_volume > 0 {
                self.envelope_volume -= 1;
            }
        }
    }

    /// Tick the channel timer by one CPU cycle. Returns current output sample (0-15).
    pub fn tick(&mut self) -> i16 {
        if !self.enabled { return 0; }
        if self.timer > 0 { self.timer -= 1; }
        if self.timer == 0 {
            self.timer = (2048 - self.frequency as u32) * 4;
            self.duty_pos = (self.duty_pos + 1) % 8;
        }
        self.output()
    }

    /// Read the current sample without advancing state.
    pub fn output(&self) -> i16 {
        if !self.enabled { return 0; }
        let wave = DUTY_TABLE[self.duty as usize & 3][self.duty_pos as usize];
        if wave != 0 { self.envelope_volume as i16 } else { -(self.envelope_volume as i16) }
    }
}

// ─── Channel 2: Square (no sweep) ───────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel2 {
    pub enabled: bool,
    pub duty: u8,
    pub length_load: u8,
    length_counter: u16,
    pub length_enabled: bool,
    pub envelope_init: u8,
    pub envelope_dir: bool,
    pub envelope_period: u8,
    envelope_volume: u8,
    envelope_timer: u8,
    pub frequency: u16,
    timer: u32,
    duty_pos: u8,
}

impl Channel2 {
    pub fn new() -> Self {
        Channel2 {
            enabled: false, duty: 0, length_load: 0, length_counter: 0,
            length_enabled: false, envelope_init: 0, envelope_dir: false,
            envelope_period: 0, envelope_volume: 0, envelope_timer: 0,
            frequency: 0, timer: 0, duty_pos: 0,
        }
    }

    pub fn trigger(&mut self) {
        self.enabled = true;
        if self.length_counter == 0 { self.length_counter = 64; }
        self.timer = (2048 - self.frequency as u32) * 4;
        self.envelope_volume = self.envelope_init;
        self.envelope_timer = self.envelope_period;
        if self.envelope_init == 0 && !self.envelope_dir { self.enabled = false; }
    }

    pub fn clock_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 { self.enabled = false; }
        }
    }

    pub fn clock_envelope(&mut self) {
        if self.envelope_period == 0 { return; }
        if self.envelope_timer > 0 { self.envelope_timer -= 1; }
        if self.envelope_timer == 0 {
            self.envelope_timer = self.envelope_period;
            if self.envelope_dir && self.envelope_volume < 15 {
                self.envelope_volume += 1;
            } else if !self.envelope_dir && self.envelope_volume > 0 {
                self.envelope_volume -= 1;
            }
        }
    }

    pub fn tick(&mut self) -> i16 {
        if !self.enabled { return 0; }
        if self.timer > 0 { self.timer -= 1; }
        if self.timer == 0 {
            self.timer = (2048 - self.frequency as u32) * 4;
            self.duty_pos = (self.duty_pos + 1) % 8;
        }
        self.output()
    }

    pub fn output(&self) -> i16 {
        if !self.enabled { return 0; }
        let wave = DUTY_TABLE[self.duty as usize & 3][self.duty_pos as usize];
        if wave != 0 { self.envelope_volume as i16 } else { -(self.envelope_volume as i16) }
    }
}

// ─── Channel 3: Programmable Wave ────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel3 {
    pub enabled: bool,
    pub dac_enabled: bool,
    pub length_load: u16,
    length_counter: u16,
    pub length_enabled: bool,
    pub volume_code: u8,        // 0=0%, 1=100%, 2=50%, 3=25%
    pub force_75: bool,         // GBA: force 75% volume
    pub frequency: u16,
    pub wave_ram: [u8; 32],     // Two banks of 16 bytes (32 x 4-bit samples)
    pub bank_select: u8,        // Which bank to play (0 or 1)
    pub dimension: bool,        // false=32 steps, true=64 steps (both banks)
    timer: u32,
    sample_pos: u8,
}

impl Channel3 {
    pub fn new() -> Self {
        Channel3 {
            enabled: false, dac_enabled: false,
            length_load: 0, length_counter: 0, length_enabled: false,
            volume_code: 0, force_75: false, frequency: 0,
            wave_ram: [0; 32], bank_select: 0, dimension: false,
            timer: 0, sample_pos: 0,
        }
    }

    pub fn trigger(&mut self) {
        self.enabled = self.dac_enabled;
        if self.length_counter == 0 { self.length_counter = 256; }
        self.timer = (2048 - self.frequency as u32) * 2;
        self.sample_pos = 0;
    }

    pub fn clock_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 { self.enabled = false; }
        }
    }

    pub fn tick(&mut self) -> i16 {
        if !self.enabled || !self.dac_enabled { return 0; }
        if self.timer > 0 { self.timer -= 1; }
        if self.timer == 0 {
            self.timer = (2048 - self.frequency as u32) * 2;
            let total_samples = if self.dimension { 64 } else { 32 };
            self.sample_pos = (self.sample_pos + 1) % total_samples;
        }
        self.output()
    }

    pub fn output(&self) -> i16 {
        if !self.enabled || !self.dac_enabled { return 0; }
        let pos = if !self.dimension {
            let bank_offset = if self.bank_select == 0 { 16 } else { 0 };
            bank_offset + self.sample_pos as usize
        } else {
            self.sample_pos as usize
        };

        let byte_idx = pos / 2;
        let sample = if pos % 2 == 0 {
            (self.wave_ram[byte_idx] >> 4) & 0xF
        } else {
            self.wave_ram[byte_idx] & 0xF
        };

        let shifted = match self.volume_code {
            0 => 0,
            1 => sample,
            2 => sample >> 1,
            3 => sample >> 2,
            _ => sample,
        };

        let shifted = if self.force_75 { (sample * 3) / 4 } else { shifted };

        shifted as i16 - 8
    }
}

// ─── Channel 4: Noise ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel4 {
    pub enabled: bool,
    pub length_load: u8,
    length_counter: u16,
    pub length_enabled: bool,
    pub envelope_init: u8,
    pub envelope_dir: bool,
    pub envelope_period: u8,
    envelope_volume: u8,
    envelope_timer: u8,
    pub clock_shift: u8,
    pub width_mode: bool, // false=15-bit LFSR, true=7-bit LFSR
    pub divisor_code: u8,
    timer: u32,
    lfsr: u16,
}

impl Channel4 {
    pub fn new() -> Self {
        Channel4 {
            enabled: false, length_load: 0, length_counter: 0,
            length_enabled: false, envelope_init: 0, envelope_dir: false,
            envelope_period: 0, envelope_volume: 0, envelope_timer: 0,
            clock_shift: 0, width_mode: false, divisor_code: 0,
            timer: 0, lfsr: 0x7FFF,
        }
    }

    fn divisor(&self) -> u32 {
        match self.divisor_code & 7 {
            0 => 8,
            n => (n as u32) * 16,
        }
    }

    pub fn trigger(&mut self) {
        self.enabled = true;
        if self.length_counter == 0 { self.length_counter = 64; }
        self.timer = self.divisor() << self.clock_shift;
        self.envelope_volume = self.envelope_init;
        self.envelope_timer = self.envelope_period;
        self.lfsr = 0x7FFF;
        if self.envelope_init == 0 && !self.envelope_dir { self.enabled = false; }
    }

    pub fn clock_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 { self.enabled = false; }
        }
    }

    pub fn clock_envelope(&mut self) {
        if self.envelope_period == 0 { return; }
        if self.envelope_timer > 0 { self.envelope_timer -= 1; }
        if self.envelope_timer == 0 {
            self.envelope_timer = self.envelope_period;
            if self.envelope_dir && self.envelope_volume < 15 {
                self.envelope_volume += 1;
            } else if !self.envelope_dir && self.envelope_volume > 0 {
                self.envelope_volume -= 1;
            }
        }
    }

    pub fn tick(&mut self) -> i16 {
        if !self.enabled { return 0; }
        if self.timer > 0 { self.timer -= 1; }
        if self.timer == 0 {
            self.timer = self.divisor() << self.clock_shift;
            let xor_bit = (self.lfsr & 1) ^ ((self.lfsr >> 1) & 1);
            self.lfsr >>= 1;
            self.lfsr |= xor_bit << 14;
            if self.width_mode {
                self.lfsr = (self.lfsr & !(1 << 6)) | (xor_bit << 6);
            }
        }
        self.output()
    }

    pub fn output(&self) -> i16 {
        if !self.enabled { return 0; }
        if self.lfsr & 1 == 0 { self.envelope_volume as i16 } else { -(self.envelope_volume as i16) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ch1_trigger_and_tick() {
        let mut ch = Channel1::new();
        ch.duty = 2; // 50%
        ch.frequency = 2000;
        ch.envelope_init = 15;
        ch.envelope_dir = false;
        ch.envelope_period = 0;
        ch.trigger();
        assert!(ch.enabled);
        // Tick a few times — should produce non-zero samples
        let mut found_nonzero = false;
        for _ in 0..1000 {
            let s = ch.tick();
            if s != 0 { found_nonzero = true; break; }
        }
        assert!(found_nonzero);
    }

    #[test]
    fn test_ch4_noise() {
        let mut ch = Channel4::new();
        ch.envelope_init = 15;
        ch.clock_shift = 0;
        ch.divisor_code = 1;
        ch.trigger();
        assert!(ch.enabled);
        // LFSR should produce varying output
        let mut samples = std::collections::HashSet::new();
        for _ in 0..500 {
            samples.insert(ch.tick());
        }
        assert!(samples.len() > 1, "Noise should produce varied output");
    }

    #[test]
    fn test_length_counter_disables() {
        let mut ch = Channel2::new();
        ch.duty = 2;
        ch.frequency = 2000;
        ch.envelope_init = 15;
        ch.length_enabled = true;
        ch.trigger();
        ch.length_counter = 2;

        ch.clock_length();
        assert!(ch.enabled);
        ch.clock_length();
        assert!(!ch.enabled); // Length reached 0
    }
}
