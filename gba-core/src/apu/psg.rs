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
        // GBATEK: f_audio = 131072 / (2048-F) Hz; 8 duty steps per waveform.
        // PSG period divider runs at 1.048576 MHz = CPU_CLOCK / 16, so one
        // duty step = (2048-F) PSG ticks = (2048-F) * 16 CPU cycles.
        self.timer = (2048 - self.frequency as u32) * 16;
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
            self.timer = (2048 - self.frequency as u32) * 16;
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
        self.timer = (2048 - self.frequency as u32) * 16;
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
            self.timer = (2048 - self.frequency as u32) * 16;
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
        // GBATEK: per-sample rate = 2097152 / (2048-F) Hz.
        // 2.097152 MHz = CPU_CLOCK / 8, so one sample = (2048-F) * 8 CPU cycles.
        self.timer = (2048 - self.frequency as u32) * 8;
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
            self.timer = (2048 - self.frequency as u32) * 8;
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
        // GBATEK: LFSR clock = 524288 / r / 2^(s+1) Hz. divisor() encodes
        // the (r, +1-shift) part in PSG-clock cycles; convert to CPU
        // cycles by ×4 (PSG runs at CPU_CLOCK / 4 internally).
        self.timer = (self.divisor() << self.clock_shift) * 4;
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
            self.timer = (self.divisor() << self.clock_shift) * 4;
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
        // LFSR should produce varying output. At divisor_code=1, shift=0
        // the LFSR clocks every 64 CPU cycles (16 PSG ticks × 4), so we
        // need enough cycles to see ≥1 bit transition — the LFSR starts at
        // 0x7FFF (all ones in the low bits) and takes ~15 steps before
        // bit 0 changes.
        let mut samples = std::collections::HashSet::new();
        for _ in 0..4000 {
            samples.insert(ch.tick());
        }
        assert!(samples.len() > 1, "Noise should produce varied output");
    }

    /// GBA spec (GBATEK): ch1 audio frequency = 131072 / (2048-F) Hz.
    /// For F=1024 that's 128 Hz, so a full waveform takes 131072 CPU cycles.
    /// A previous bug used `(2048-F) * 4` for the duty-step timer, making
    /// every PSG channel run 4× too fast (2 octaves up) — most audible on
    /// isolated SFX bursts. This test pins the spec rate so it can't drift.
    #[test]
    fn ch1_full_waveform_period_matches_gba_spec() {
        let mut ch = Channel1::new();
        ch.duty = 2; // 50%
        ch.frequency = 1024;
        ch.envelope_init = 15;
        ch.envelope_dir = false;
        ch.envelope_period = 0;
        ch.length_enabled = false;
        ch.trigger();

        // Find the first H→L transition (output goes from + to -).
        let mut last = ch.tick();
        let mut first_h_to_l = 0u64;
        for c in 2u64..400_000 {
            let v = ch.tick();
            if v < last && last > 0 { first_h_to_l = c; last = v; break; }
            last = v;
        }
        assert!(first_h_to_l != 0, "never saw first H→L transition");

        // Find the next H→L transition. The gap is one full waveform.
        let mut second_h_to_l = 0u64;
        for c in first_h_to_l + 1..1_000_000 {
            let v = ch.tick();
            if v < last && last > 0 { second_h_to_l = c; break; }
            last = v;
        }
        let waveform_cycles = second_h_to_l - first_h_to_l;
        let expected = (2048u64 - 1024) * 16 * 8; // 131072 cycles
        assert_eq!(
            waveform_cycles, expected,
            "ch1 full-waveform period: got {} CPU cycles, spec wants {} for F=1024",
            waveform_cycles, expected,
        );
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
