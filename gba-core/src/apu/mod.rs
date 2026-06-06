pub mod psg;
pub mod fifo;

use psg::{Channel1, Channel2, Channel3, Channel4};
use fifo::FifoChannel;
use serde::{Deserialize, Serialize};

/// Output sample rate delivered to the frontend.
/// Chosen to match the macOS Core Audio native rate (48 kHz) so SDL2 does
/// not need to resample internally — that resampling is a major source of
/// aliasing artifacts.
pub const OUTPUT_SAMPLE_RATE: u32 = 48_000;

/// GBA CPU clock. Each tick of the APU corresponds to one CPU cycle.
pub const CPU_CLOCK_HZ: u32 = 16_777_216;

/// Frame sequencer runs at 512 Hz → one step every CPU_CLOCK_HZ / 512 cycles.
pub const CYCLES_PER_FRAME_SEQ: u32 = 32_768;

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
    pub psg_enable_right: [bool; 4],
    pub psg_enable_left: [bool; 4],
    // SOUNDCNT_H: DMA sound control
    pub psg_master_volume: u8, // 0=25%, 1=50%, 2=100%
    // SOUNDBIAS
    pub bias_level: u16,

    // Frame sequencer
    frame_seq_step: u8,
    frame_seq_counter: u32,

    // Oversampling decimation state.
    // Every CPU cycle we compute the current mixed sample and add it to the
    // accumulator. When the fractional counter crosses an output-sample
    // boundary, we emit the average. The averaging acts as a boxcar
    // anti-aliasing filter (decimation factor ≈ 349), which naturally
    // suppresses the harsh high-frequency content that plagues the naive
    // "emit raw current sample" approach.
    sample_frac: u64,  // accumulates OUTPUT_SAMPLE_RATE per cycle; wraps at CPU_CLOCK_HZ
    accum_left: i64,
    accum_right: i64,
    accum_count: u32,

    // 1-stage post-IIR LPF — gentle smoothing only; the boxcar averaging
    // does the heavy anti-aliasing.
    lpf_left: [i32; 1],
    lpf_right: [i32; 1],

    /// Output sample buffer — interleaved stereo i16 at OUTPUT_SAMPLE_RATE.
    pub sample_buffer: Vec<i16>,
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
            sample_frac: 0,
            accum_left: 0,
            accum_right: 0,
            accum_count: 0,
            lpf_left: [0; 1],
            lpf_right: [0; 1],
            sample_buffer: Vec::with_capacity(8192),
            sample_buffer_max: 16384,
        }
    }

    /// Tick the APU by N CPU cycles. Called after each CPU step.
    pub fn tick(&mut self, cycles: u32) -> (bool, bool) {
        // Fast path: when no PSG channels are active (the common case in
        // M4A-engine games like Pokémon and SRTOG), the only mixed signal
        // is the held FIFO value, which is constant for the entire batch.
        // We can compute it once and bulk-accumulate, reducing per-cycle
        // work from ~100 ns to amortized ~5 ns. Critical for halt-period
        // ticking where `cycles` can be many thousands.
        let psg_active = self.ch1.enabled
            || self.ch2.enabled
            || self.ch3.enabled
            || self.ch4.enabled;
        if !psg_active {
            self.tick_fast(cycles);
            return (false, false);
        }
        for _ in 0..cycles {
            // Advance each channel by one CPU cycle so their internal timers
            // and waveform state step at the correct emulated rate.
            self.ch1.tick();
            self.ch2.tick();
            self.ch3.tick();
            self.ch4.tick();

            // Frame sequencer (512 Hz)
            self.frame_seq_counter += 1;
            if self.frame_seq_counter >= CYCLES_PER_FRAME_SEQ {
                self.frame_seq_counter -= CYCLES_PER_FRAME_SEQ;
                self.clock_frame_sequencer();
            }

            // Accumulate the current mixed sample for oversampling
            if self.master_enable {
                let (l, r) = self.current_mix();
                self.accum_left += l as i64;
                self.accum_right += r as i64;
            }
            self.accum_count += 1;

            // Fractional counter: emit an output sample when we cross a
            // sample boundary at OUTPUT_SAMPLE_RATE relative to CPU_CLOCK_HZ.
            self.sample_frac += OUTPUT_SAMPLE_RATE as u64;
            if self.sample_frac >= CPU_CLOCK_HZ as u64 {
                self.sample_frac -= CPU_CLOCK_HZ as u64;
                self.emit_sample();
            }
        }
        // FIFO refill is handled in on_timer_overflow()
        (false, false)
    }

    /// Bulk-tick when all PSG channels are disabled (M4A common case).
    /// Mixed signal is the held FIFO value — constant for the whole batch.
    /// Iterates per-emit-boundary (~349 cycles) instead of per-cycle.
    fn tick_fast(&mut self, cycles: u32) {
        // Compute mix once — held FIFO value, constant for the whole batch.
        let (l, r) = if self.master_enable {
            self.current_mix()
        } else {
            (0, 0)
        };
        let l = l as i64;
        let r = r as i64;

        let mut remaining = cycles as u64;
        while remaining > 0 {
            // Cycles until the next sample-emit boundary
            let cpu_clock = CPU_CLOCK_HZ as u64;
            let out_rate = OUTPUT_SAMPLE_RATE as u64;
            let to_next_emit = (cpu_clock - self.sample_frac).div_ceil(out_rate);
            let chunk = remaining.min(to_next_emit);

            self.accum_left += l * chunk as i64;
            self.accum_right += r * chunk as i64;
            self.accum_count += chunk as u32;
            self.sample_frac += out_rate * chunk;

            if self.sample_frac >= cpu_clock {
                self.sample_frac -= cpu_clock;
                self.emit_sample();
            }

            remaining -= chunk;
        }

        // Frame sequencer can be batched; clock multiple times if cycles spans
        // multiple periods. Frame seq drives PSG envelopes/sweeps, but we
        // already know all PSGs are disabled — still update so re-enabling
        // them later sees a correct envelope state.
        self.frame_seq_counter += cycles;
        while self.frame_seq_counter >= CYCLES_PER_FRAME_SEQ {
            self.frame_seq_counter -= CYCLES_PER_FRAME_SEQ;
            self.clock_frame_sequencer();
        }
    }

    /// Compute the current mixed sample (pre-scaling). Each PSG channel is
    /// in -15..15 and each FIFO channel is in -128..127, so `left` and
    /// `right` span roughly -300..300.
    fn current_mix(&self) -> (i32, i32) {
        // PSG channel outputs (do NOT advance state — tick() already did)
        let ch1 = self.ch1.output() as i32;
        let ch2 = self.ch2.output() as i32;
        let ch3 = self.ch3.output() as i32;
        let ch4 = self.ch4.output() as i32;

        let mut psg_left = 0i32;
        let mut psg_right = 0i32;
        if self.psg_enable_left[0] { psg_left += ch1; }
        if self.psg_enable_left[1] { psg_left += ch2; }
        if self.psg_enable_left[2] { psg_left += ch3; }
        if self.psg_enable_left[3] { psg_left += ch4; }
        if self.psg_enable_right[0] { psg_right += ch1; }
        if self.psg_enable_right[1] { psg_right += ch2; }
        if self.psg_enable_right[2] { psg_right += ch3; }
        if self.psg_enable_right[3] { psg_right += ch4; }

        // SOUNDCNT_L per-side volume (0-7)
        psg_left = psg_left * (self.psg_volume_left as i32 + 1) / 8;
        psg_right = psg_right * (self.psg_volume_right as i32 + 1) / 8;

        // SOUNDCNT_H PSG master ratio: 0=25%, 1=50%, 2/3=100%
        let psg_ratio = match self.psg_master_volume { 0 => 1, 1 => 2, _ => 4 };
        psg_left = psg_left * psg_ratio / 4;
        psg_right = psg_right * psg_ratio / 4;

        // FIFO channels (held values)
        let fifo_a = self.fifo_a.output() as i32;
        let fifo_b = self.fifo_b.output() as i32;
        let mut left = psg_left;
        let mut right = psg_right;
        if self.fifo_a.enable_left { left += fifo_a; }
        if self.fifo_a.enable_right { right += fifo_a; }
        if self.fifo_b.enable_left { left += fifo_b; }
        if self.fifo_b.enable_right { right += fifo_b; }

        (left, right)
    }

    /// Decimate accumulated samples and emit one output sample.
    fn emit_sample(&mut self) {
        if self.accum_count == 0 {
            // No samples accumulated — push silence (shouldn't happen normally)
            self.push_pair(0, 0);
            return;
        }

        // Average: boxcar filter with decimation factor ≈ 349.
        // Strong anti-alias property due to the large filter length.
        let n = self.accum_count as i64;
        let avg_left = (self.accum_left / n) as i32;
        let avg_right = (self.accum_right / n) as i32;
        self.accum_left = 0;
        self.accum_right = 0;
        self.accum_count = 0;

        // Scale: averaging over ~349 cycles suppresses peaks, so we compensate.
        // 200× was calibrated against mGBA's SDL-disk-audio output for
        // Shining Soul II's title screen: post-RMS-normalised spectra are
        // already within ±0.3 dB of mGBA, but our absolute RMS came in
        // ~40 % lower (peaks ~10k vs mGBA's ~18k), making everything sound
        // too quiet. Bumping the multiplier matches mGBA's loudness with
        // comfortable headroom — pre-clamp peaks land around 0.6× full
        // scale on dense mixes (HoD, SS2). The clamp() below catches any
        // outlier transients on louder games.
        let scaled_left = (avg_left * 200).clamp(-32768, 32767);
        let scaled_right = (avg_right * 200).clamp(-32768, 32767);

        // No post-IIR. The boxcar averaging over ~349 cycles already
        // anti-aliases — its first null sits at fs_in/L ≈ 48 kHz, on top
        // of the output Nyquist. The previous y[n] = (y[n-1] + x[n]) / 2
        // post-filter (α=0.5) added another −3 dB at 5.5 kHz and −8 dB
        // at 16 kHz, which made everything sound muffled compared to a
        // hardware/mGBA reference. Drop it. lpf_* fields kept for serde
        // backward compatibility but unused on the active path.
        let left_out = scaled_left.clamp(-32768, 32767) as i16;
        let right_out = scaled_right.clamp(-32768, 32767) as i16;
        self.push_pair(left_out, right_out);
    }

    fn push_pair(&mut self, left: i16, right: i16) {
        if self.sample_buffer.len() < self.sample_buffer_max {
            self.sample_buffer.push(left);
            self.sample_buffer.push(right);
        }
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

    /// Drain samples from the buffer (called by the audio thread).
    pub fn drain_samples(&mut self, out: &mut [i16]) -> usize {
        let available = self.sample_buffer.len().min(out.len());
        out[..available].copy_from_slice(&self.sample_buffer[..available]);
        self.sample_buffer.drain(..available);
        available
    }

    // ─── Register read/write ──────────────────────────────────────

    pub fn write_reg(&mut self, offset: u16, value: u16) {
        match offset {
            // SOUND1CNT_L — Sweep
            0x00 => {
                self.ch1.sweep_shift = (value & 7) as u8;
                self.ch1.sweep_negate = value & (1 << 3) != 0;
                self.ch1.sweep_period = ((value >> 4) & 7) as u8;
            }
            // SOUND1CNT_H — Duty/Length/Envelope
            0x02 => {
                self.ch1.length_load = (value & 0x3F) as u8;
                self.ch1.duty = ((value >> 6) & 3) as u8;
                self.ch1.envelope_period = (value >> 8 & 7) as u8;
                self.ch1.envelope_dir = value & (1 << 11) != 0;
                self.ch1.envelope_init = ((value >> 12) & 0xF) as u8;
            }
            // SOUND1CNT_X — Frequency/Control
            0x04 => {
                self.ch1.frequency = value & 0x7FF;
                self.ch1.length_enabled = value & (1 << 14) != 0;
                if value & (1 << 15) != 0 { self.ch1.trigger(); }
            }
            // SOUND2CNT_L
            0x08 => {
                self.ch2.length_load = (value & 0x3F) as u8;
                self.ch2.duty = ((value >> 6) & 3) as u8;
                self.ch2.envelope_period = (value >> 8 & 7) as u8;
                self.ch2.envelope_dir = value & (1 << 11) != 0;
                self.ch2.envelope_init = ((value >> 12) & 0xF) as u8;
            }
            // SOUND2CNT_H
            0x0C => {
                self.ch2.frequency = value & 0x7FF;
                self.ch2.length_enabled = value & (1 << 14) != 0;
                if value & (1 << 15) != 0 { self.ch2.trigger(); }
            }
            // SOUND3CNT_L
            0x10 => {
                self.ch3.dimension = value & (1 << 5) != 0;
                self.ch3.bank_select = ((value >> 6) & 1) as u8;
                self.ch3.dac_enabled = value & (1 << 7) != 0;
            }
            // SOUND3CNT_H
            0x12 => {
                self.ch3.length_load = (value & 0xFF) as u16;
                self.ch3.volume_code = ((value >> 13) & 3) as u8;
                self.ch3.force_75 = value & (1 << 15) != 0;
            }
            // SOUND3CNT_X
            0x14 => {
                self.ch3.frequency = value & 0x7FF;
                self.ch3.length_enabled = value & (1 << 14) != 0;
                if value & (1 << 15) != 0 { self.ch3.trigger(); }
            }
            // SOUND4CNT_L
            0x18 => {
                self.ch4.length_load = (value & 0x3F) as u8;
                self.ch4.envelope_period = (value >> 8 & 7) as u8;
                self.ch4.envelope_dir = value & (1 << 11) != 0;
                self.ch4.envelope_init = ((value >> 12) & 0xF) as u8;
            }
            // SOUND4CNT_H
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
            // FIFO_A low (0xA0) and high (0xA2) halves — each writes 2 samples
            0x40 => self.fifo_a.write16(value),
            0x42 => self.fifo_a.write16(value),
            // FIFO_B low (0xA4) and high (0xA6)
            0x44 => self.fifo_b.write16(value),
            0x46 => self.fifo_b.write16(value),
            _ => {}
        }
    }

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
            // SOUNDCNT_L — PSG L/R volume + per-channel L/R enables.
            // All bits 0..15 are R/W except 3,7 (unused, read as 0).
            0x20 => {
                let mut v = (self.psg_volume_right as u16 & 7)
                    | ((self.psg_volume_left as u16 & 7) << 4);
                for i in 0..4 {
                    if self.psg_enable_right[i] { v |= 1 << (8 + i); }
                    if self.psg_enable_left[i]  { v |= 1 << (12 + i); }
                }
                v
            }
            // SOUNDCNT_H — DirectSound mix control. Bits 4-7 unused, bits 11/15
            // are write-only FIFO-reset triggers — readback mask is 0x770F.
            0x22 => {
                let mut v = self.psg_master_volume as u16 & 3;
                if self.fifo_a.volume_full   { v |= 1 << 2; }
                if self.fifo_b.volume_full   { v |= 1 << 3; }
                if self.fifo_a.enable_right  { v |= 1 << 8; }
                if self.fifo_a.enable_left   { v |= 1 << 9; }
                v |= (self.fifo_a.timer_select as u16 & 1) << 10;
                if self.fifo_b.enable_right  { v |= 1 << 12; }
                if self.fifo_b.enable_left   { v |= 1 << 13; }
                v |= (self.fifo_b.timer_select as u16 & 1) << 14;
                v
            }
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
        apu.write_reg(0x24, 1 << 7);
        assert!(apu.master_enable);
        apu.write_reg(0x24, 0);
        assert!(!apu.master_enable);
    }

    #[test]
    fn test_apu_generates_samples_at_48khz() {
        let mut apu = Apu::new();
        apu.master_enable = true;
        apu.sample_buffer_max = 200_000; // lift cap for the test

        // 16777216 CPU cycles = 1 emulated second = 48000 samples = 96000 i16 values.
        apu.tick(CPU_CLOCK_HZ);
        let n_samples = apu.sample_buffer.len() / 2;
        // Allow ±1 due to fractional rounding
        assert!(
            (n_samples as i64 - 48000).abs() <= 1,
            "expected ~48000 samples, got {}",
            n_samples
        );
    }

    #[test]
    fn test_fifo_timer_overflow() {
        let mut apu = Apu::new();
        apu.fifo_a.timer_select = 0;
        apu.fifo_a.volume_full = true;
        apu.fifo_a.enable_left = true;
        apu.fifo_a.enable_right = true;
        apu.fifo_a.write32(0x10203040);

        let (needs_a, _) = apu.on_timer_overflow(0);
        assert_eq!(apu.fifo_a.current_sample, 0x40);
        assert!(needs_a);
    }

    #[test]
    fn test_soundcnt_h_parse() {
        let mut apu = Apu::new();
        let val: u16 = (1 << 2) | (1 << 8) | (1 << 9) | (1 << 12) | (1 << 14);
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
