pub mod psg;
pub mod fifo;

use psg::{Channel1, Channel2, Channel3, Channel4};
use fifo::FifoChannel;
use serde::{Deserialize, Serialize};

/// GBA sample rate determined by SOUNDBIAS (default: 32768 Hz).
pub const SAMPLE_RATE: u32 = 32768;
/// CPU cycles per audio sample: 16777216 / 32768 = 512.
pub const CYCLES_PER_SAMPLE: u32 = 512;
/// Frame sequencer rate: 512 Hz → 32768 cycles between steps.
pub const CYCLES_PER_FRAME_SEQ: u32 = 32768;

#[derive(Serialize, Deserialize)]
pub struct Apu {
    // PSG channels
    pub ch1: Channel1,
    pub ch2: Channel2,
    pub ch3: Channel3,
    pub ch4: Channel4,
    // FIFO channels
    pub fifo_a: FifoChannel,
    pub fifo_b: FifoChannel,
    // Master enable (SOUNDCNT_X bit 7)
    pub master_enable: bool,
    // SOUNDCNT_L: PSG volume and panning
    pub psg_volume_right: u8, // 0-7
    pub psg_volume_left: u8,  // 0-7
    pub psg_enable_right: [bool; 4], // Ch1-4 right enable
    pub psg_enable_left: [bool; 4],  // Ch1-4 left enable
    // SOUNDCNT_H: DMA sound control
    pub psg_master_volume: u8, // 0=25%, 1=50%, 2=100%
    // SOUNDBIAS
    pub bias_level: u16, // Default 0x200
    // Frame sequencer
    frame_seq_step: u8,
    frame_seq_counter: u32,
    // Sample accumulator
    sample_counter: u32,
    /// Output sample buffer — emulation thread writes, audio thread reads.
    /// Stores interleaved (left, right) i16 samples.
    pub sample_buffer: Vec<i16>,
    /// Maximum samples to buffer before dropping (prevents unbounded growth).
    pub sample_buffer_max: usize,
}

impl Apu {
    pub fn new() -> Self {
        Apu {
            ch1: Channel1::new(),
            ch2: Channel2::new(),
            ch3: Channel3::new(),
            ch4: Channel4::new(),
            fifo_a: FifoChannel::new(),
            fifo_b: FifoChannel::new(),
            master_enable: false,
            psg_volume_right: 7,
            psg_volume_left: 7,
            psg_enable_right: [false; 4],
            psg_enable_left: [false; 4],
            psg_master_volume: 2,
            bias_level: 0x200,
            frame_seq_step: 0,
            frame_seq_counter: 0,
            sample_counter: 0,
            sample_buffer: Vec::with_capacity(4096),
            sample_buffer_max: 8192,
        }
    }

    /// Tick the APU by N CPU cycles. Called after each CPU step.
    /// Returns true if FIFO A or B needs a DMA refill.
    pub fn tick(&mut self, cycles: u32) -> (bool, bool) {
        if !self.master_enable {
            self.sample_counter += cycles;
            while self.sample_counter >= CYCLES_PER_SAMPLE {
                self.sample_counter -= CYCLES_PER_SAMPLE;
                self.push_silence();
            }
            return (false, false);
        }

        // Tick PSG channels
        for _ in 0..cycles {
            self.ch1.tick();
            self.ch2.tick();
            self.ch3.tick();
            self.ch4.tick();

            // Frame sequencer
            self.frame_seq_counter += 1;
            if self.frame_seq_counter >= CYCLES_PER_FRAME_SEQ {
                self.frame_seq_counter -= CYCLES_PER_FRAME_SEQ;
                self.clock_frame_sequencer();
            }

            // Sample output
            self.sample_counter += 1;
            if self.sample_counter >= CYCLES_PER_SAMPLE {
                self.sample_counter -= CYCLES_PER_SAMPLE;
                self.generate_sample();
            }
        }

        // FIFO refill is handled separately via on_timer_overflow()
        (false, false)
    }

    /// Called when Timer 0 or Timer 1 overflows — advance FIFO sample.
    pub fn on_timer_overflow(&mut self, timer_id: u8) -> (bool, bool) {
        let mut fifo_a_refill = false;
        let mut fifo_b_refill = false;

        if self.fifo_a.timer_select == timer_id {
            fifo_a_refill = self.fifo_a.pop_sample();
        }
        if self.fifo_b.timer_select == timer_id {
            fifo_b_refill = self.fifo_b.pop_sample();
        }

        (fifo_a_refill, fifo_b_refill)
    }

    /// Clock the 512 Hz frame sequencer.
    fn clock_frame_sequencer(&mut self) {
        match self.frame_seq_step {
            0 => {
                self.ch1.clock_length();
                self.ch2.clock_length();
                self.ch3.clock_length();
                self.ch4.clock_length();
            }
            2 => {
                self.ch1.clock_length();
                self.ch2.clock_length();
                self.ch3.clock_length();
                self.ch4.clock_length();
                self.ch1.clock_sweep();
            }
            4 => {
                self.ch1.clock_length();
                self.ch2.clock_length();
                self.ch3.clock_length();
                self.ch4.clock_length();
            }
            6 => {
                self.ch1.clock_length();
                self.ch2.clock_length();
                self.ch3.clock_length();
                self.ch4.clock_length();
                self.ch1.clock_sweep();
            }
            7 => {
                self.ch1.clock_envelope();
                self.ch2.clock_envelope();
                self.ch4.clock_envelope();
            }
            _ => {}
        }
        self.frame_seq_step = (self.frame_seq_step + 1) % 8;
    }

    /// Generate one output sample (left + right) and push to the buffer.
    fn generate_sample(&mut self) {
        let ch1 = self.ch1.tick();
        let ch2 = self.ch2.tick();
        let ch3 = self.ch3.tick();
        let ch4 = self.ch4.tick();

        // PSG mixing: each channel output is -15..15
        let mut psg_left: i32 = 0;
        let mut psg_right: i32 = 0;

        if self.psg_enable_left[0] { psg_left += ch1 as i32; }
        if self.psg_enable_left[1] { psg_left += ch2 as i32; }
        if self.psg_enable_left[2] { psg_left += ch3 as i32; }
        if self.psg_enable_left[3] { psg_left += ch4 as i32; }
        if self.psg_enable_right[0] { psg_right += ch1 as i32; }
        if self.psg_enable_right[1] { psg_right += ch2 as i32; }
        if self.psg_enable_right[2] { psg_right += ch3 as i32; }
        if self.psg_enable_right[3] { psg_right += ch4 as i32; }

        // Apply PSG master volume (0-7, effectively multiply by (vol+1)/8)
        psg_left = psg_left * (self.psg_volume_left as i32 + 1) / 8;
        psg_right = psg_right * (self.psg_volume_right as i32 + 1) / 8;

        // Apply PSG ratio from SOUNDCNT_H
        let psg_ratio = match self.psg_master_volume {
            0 => 1, // 25%
            1 => 2, // 50%
            _ => 4, // 100%
        };
        psg_left = psg_left * psg_ratio / 4;
        psg_right = psg_right * psg_ratio / 4;

        // FIFO mixing: output is -128..127
        let fifo_a = self.fifo_a.output() as i32;
        let fifo_b = self.fifo_b.output() as i32;

        let mut left: i32 = psg_left;
        let mut right: i32 = psg_right;

        if self.fifo_a.enable_left { left += fifo_a; }
        if self.fifo_a.enable_right { right += fifo_a; }
        if self.fifo_b.enable_left { left += fifo_b; }
        if self.fifo_b.enable_right { right += fifo_b; }

        // Apply SOUNDBIAS and clamp to 10-bit (0..0x3FF)
        let bias = self.bias_level as i32;
        left = (left + bias).clamp(0, 0x3FF);
        right = (right + bias).clamp(0, 0x3FF);

        // Convert to signed 16-bit for output (-32768..32767)
        let left_out = ((left - bias) * 64).clamp(-32768, 32767) as i16;
        let right_out = ((right - bias) * 64).clamp(-32768, 32767) as i16;

        if self.sample_buffer.len() < self.sample_buffer_max {
            self.sample_buffer.push(left_out);
            self.sample_buffer.push(right_out);
        }
    }

    fn push_silence(&mut self) {
        if self.sample_buffer.len() < self.sample_buffer_max {
            self.sample_buffer.push(0);
            self.sample_buffer.push(0);
        }
    }

    /// Drain samples from the buffer (called by the audio thread).
    pub fn drain_samples(&mut self, out: &mut [i16]) -> usize {
        let available = self.sample_buffer.len().min(out.len());
        out[..available].copy_from_slice(&self.sample_buffer[..available]);
        self.sample_buffer.drain(..available);
        available
    }

    // ─── Register read/write ──────────────────────────────────────

    /// Write to a sound register (address relative to 0x04000060).
    pub fn write_reg(&mut self, offset: u16, value: u16) {
        match offset {
            // SOUND1CNT_L (NR10) — Sweep
            0x00 => {
                self.ch1.sweep_shift = (value & 7) as u8;
                self.ch1.sweep_negate = value & (1 << 3) != 0;
                self.ch1.sweep_period = ((value >> 4) & 7) as u8;
            }
            // SOUND1CNT_H (NR11/NR12) — Duty/Length/Envelope
            0x02 => {
                self.ch1.length_load = (value & 0x3F) as u8;
                self.ch1.duty = ((value >> 6) & 3) as u8;
                self.ch1.envelope_period = (value >> 8 & 7) as u8;
                self.ch1.envelope_dir = value & (1 << 11) != 0;
                self.ch1.envelope_init = ((value >> 12) & 0xF) as u8;
            }
            // SOUND1CNT_X (NR13/NR14) — Frequency/Control
            0x04 => {
                self.ch1.frequency = value & 0x7FF;
                self.ch1.length_enabled = value & (1 << 14) != 0;
                if value & (1 << 15) != 0 { self.ch1.trigger(); }
            }
            // SOUND2CNT_L (NR21/NR22)
            0x08 => {
                self.ch2.length_load = (value & 0x3F) as u8;
                self.ch2.duty = ((value >> 6) & 3) as u8;
                self.ch2.envelope_period = (value >> 8 & 7) as u8;
                self.ch2.envelope_dir = value & (1 << 11) != 0;
                self.ch2.envelope_init = ((value >> 12) & 0xF) as u8;
            }
            // SOUND2CNT_H (NR23/NR24)
            0x0C => {
                self.ch2.frequency = value & 0x7FF;
                self.ch2.length_enabled = value & (1 << 14) != 0;
                if value & (1 << 15) != 0 { self.ch2.trigger(); }
            }
            // SOUND3CNT_L (NR30)
            0x10 => {
                self.ch3.dimension = value & (1 << 5) != 0;
                self.ch3.bank_select = ((value >> 6) & 1) as u8;
                self.ch3.dac_enabled = value & (1 << 7) != 0;
            }
            // SOUND3CNT_H (NR31/NR32)
            0x12 => {
                self.ch3.length_load = (value & 0xFF) as u16;
                self.ch3.volume_code = ((value >> 13) & 3) as u8;
                self.ch3.force_75 = value & (1 << 15) != 0;
            }
            // SOUND3CNT_X (NR33/NR34)
            0x14 => {
                self.ch3.frequency = value & 0x7FF;
                self.ch3.length_enabled = value & (1 << 14) != 0;
                if value & (1 << 15) != 0 { self.ch3.trigger(); }
            }
            // SOUND4CNT_L (NR41/NR42)
            0x18 => {
                self.ch4.length_load = (value & 0x3F) as u8;
                self.ch4.envelope_period = (value >> 8 & 7) as u8;
                self.ch4.envelope_dir = value & (1 << 11) != 0;
                self.ch4.envelope_init = ((value >> 12) & 0xF) as u8;
            }
            // SOUND4CNT_H (NR43/NR44)
            0x1C => {
                self.ch4.divisor_code = (value & 7) as u8;
                self.ch4.width_mode = value & (1 << 3) != 0;
                self.ch4.clock_shift = ((value >> 4) & 0xF) as u8;
                self.ch4.length_enabled = value & (1 << 14) != 0;
                if value & (1 << 15) != 0 { self.ch4.trigger(); }
            }
            // SOUNDCNT_L — PSG volume/panning
            0x20 => {
                self.psg_volume_right = (value & 7) as u8;
                self.psg_volume_left = ((value >> 4) & 7) as u8;
                for i in 0..4 {
                    self.psg_enable_right[i] = value & (1 << (8 + i)) != 0;
                    self.psg_enable_left[i] = value & (1 << (12 + i)) != 0;
                }
            }
            // SOUNDCNT_H — DMA sound control
            0x22 => {
                self.psg_master_volume = (value & 3) as u8;
                self.fifo_a.volume_full = value & (1 << 2) != 0;
                self.fifo_b.volume_full = value & (1 << 3) != 0;
                self.fifo_a.enable_right = value & (1 << 8) != 0;
                self.fifo_a.enable_left = value & (1 << 9) != 0;
                self.fifo_a.timer_select = ((value >> 10) & 1) as u8;
                if value & (1 << 11) != 0 { self.fifo_a.reset(); }
                self.fifo_b.enable_right = value & (1 << 12) != 0;
                self.fifo_b.enable_left = value & (1 << 13) != 0;
                self.fifo_b.timer_select = ((value >> 14) & 1) as u8;
                if value & (1 << 15) != 0 { self.fifo_b.reset(); }
            }
            // SOUNDCNT_X — Master enable / status
            0x24 => {
                self.master_enable = value & (1 << 7) != 0;
                if !self.master_enable {
                    self.ch1.enabled = false;
                    self.ch2.enabled = false;
                    self.ch3.enabled = false;
                    self.ch4.enabled = false;
                }
            }
            // SOUNDBIAS
            0x28 => {
                self.bias_level = value & 0x3FF;
            }
            // Wave RAM (0x90-0x9F → offsets 0x30-0x3F)
            0x30..=0x3F => {
                let idx = (offset - 0x30) as usize * 2;
                let bytes = value.to_le_bytes();
                if idx < 32 { self.ch3.wave_ram[idx] = bytes[0]; }
                if idx + 1 < 32 { self.ch3.wave_ram[idx + 1] = bytes[1]; }
            }
            // FIFO_A (0xA0)
            0x40 => {
                self.fifo_a.write32(value as u32);
            }
            // FIFO_B (0xA4)
            0x44 => {
                self.fifo_b.write32(value as u32);
            }
            _ => {}
        }
    }

    /// Read a sound register (address relative to 0x04000060).
    pub fn read_reg(&self, offset: u16) -> u16 {
        match offset {
            0x00 => {
                (self.ch1.sweep_shift as u16)
                    | ((self.ch1.sweep_negate as u16) << 3)
                    | ((self.ch1.sweep_period as u16) << 4)
            }
            0x02 => {
                ((self.ch1.duty as u16) << 6)
                    | ((self.ch1.envelope_period as u16) << 8)
                    | ((self.ch1.envelope_dir as u16) << 11)
                    | ((self.ch1.envelope_init as u16) << 12)
            }
            0x04 => self.ch1.frequency | ((self.ch1.length_enabled as u16) << 14),
            0x08 => {
                ((self.ch2.duty as u16) << 6)
                    | ((self.ch2.envelope_period as u16) << 8)
                    | ((self.ch2.envelope_dir as u16) << 11)
                    | ((self.ch2.envelope_init as u16) << 12)
            }
            0x0C => self.ch2.frequency | ((self.ch2.length_enabled as u16) << 14),
            0x24 => {
                (self.ch1.enabled as u16)
                    | ((self.ch2.enabled as u16) << 1)
                    | ((self.ch3.enabled as u16) << 2)
                    | ((self.ch4.enabled as u16) << 3)
                    | ((self.master_enable as u16) << 7)
            }
            0x28 => self.bias_level,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apu_master_enable() {
        let mut apu = Apu::new();
        apu.write_reg(0x24, 1 << 7); // Master enable
        assert!(apu.master_enable);
        apu.write_reg(0x24, 0); // Disable
        assert!(!apu.master_enable);
    }

    #[test]
    fn test_apu_generates_samples() {
        let mut apu = Apu::new();
        apu.master_enable = true;

        // Tick for enough cycles to generate at least one sample (512 cycles)
        apu.tick(1024);
        assert!(apu.sample_buffer.len() >= 4); // At least 2 stereo samples
    }

    #[test]
    fn test_fifo_timer_overflow() {
        let mut apu = Apu::new();
        apu.fifo_a.timer_select = 0;
        apu.fifo_a.volume_full = true;
        apu.fifo_a.enable_left = true;
        apu.fifo_a.enable_right = true;

        // Fill FIFO with test data
        apu.fifo_a.write32(0x10203040);

        // Timer 0 overflow should pop a sample
        let (needs_a, _) = apu.on_timer_overflow(0);
        assert_eq!(apu.fifo_a.current_sample, 0x40);
        assert!(needs_a); // Only 3 samples left → needs refill
    }

    #[test]
    fn test_soundcnt_h_parse() {
        let mut apu = Apu::new();
        // FIFO A: 100% vol, right+left, timer 0
        // FIFO B: 50% vol, right only, timer 1
        let val: u16 = (1 << 2)  // FIFO A volume full
            | (1 << 8)           // FIFO A right
            | (1 << 9)           // FIFO A left
            | (0 << 10)          // FIFO A timer 0
            | (1 << 12)          // FIFO B right
            | (0 << 13)          // FIFO B left off
            | (1 << 14);         // FIFO B timer 1
        apu.write_reg(0x22, val);

        assert!(apu.fifo_a.volume_full);
        assert!(apu.fifo_a.enable_right);
        assert!(apu.fifo_a.enable_left);
        assert_eq!(apu.fifo_a.timer_select, 0);
        assert!(!apu.fifo_b.volume_full);
        assert!(apu.fifo_b.enable_right);
        assert!(!apu.fifo_b.enable_left);
        assert_eq!(apu.fifo_b.timer_select, 1);
    }
}
