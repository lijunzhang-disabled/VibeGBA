//! Audio output via SDL2.
//!
//! Uses a simple ring buffer approach: the emulation loop pushes samples
//! into a shared buffer, and the SDL2 audio callback pulls from it.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

/// Streaming WAV writer. 48 kHz stereo i16, hardcoded for our output.
///
/// Triggered by setting WAV_DUMP=path.wav. Useful for spectrum-analyzing
/// audio against a reference emulator: open both files in Audacity and
/// look at the difference.
pub struct WavWriter {
    file: File,
    samples_written: u32,
}

impl WavWriter {
    pub fn create(path: &str) -> std::io::Result<Self> {
        let mut file = File::create(path)?;
        // 44-byte header. We'll fix up the size fields on Drop.
        let header = build_wav_header(0);
        file.write_all(&header)?;
        Ok(WavWriter { file, samples_written: 0 })
    }

    pub fn append(&mut self, samples: &[i16]) {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let _ = self.file.write_all(&bytes);
        self.samples_written += samples.len() as u32;
    }
}

impl Drop for WavWriter {
    fn drop(&mut self) {
        // Patch RIFF chunk size and data chunk size with the actual byte count.
        let data_bytes = self.samples_written * 2;
        let _ = self.file.seek(SeekFrom::Start(4));
        let _ = self.file.write_all(&(36 + data_bytes).to_le_bytes());
        let _ = self.file.seek(SeekFrom::Start(40));
        let _ = self.file.write_all(&data_bytes.to_le_bytes());
    }
}

fn build_wav_header(data_bytes: u32) -> [u8; 44] {
    let sample_rate: u32 = 48_000;
    let channels: u16 = 2;
    let bits_per_sample: u16 = 16;
    let byte_rate: u32 = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align: u16 = channels * bits_per_sample / 8;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());        // fmt chunk size
    h[20..22].copy_from_slice(&1u16.to_le_bytes());         // PCM
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&block_align.to_le_bytes());
    h[34..36].copy_from_slice(&bits_per_sample.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    h
}

/// Shared audio buffer between emulation thread and SDL2 audio callback.
/// We pre-fill with silence at startup so the SDL2 callback never underruns
/// during the first few frames before the emulator has produced samples.
/// Buffer target level is ~2 × callback size (2 × 1024 stereo = 4096 i16),
/// i.e. ~42 ms of audio at 48 kHz — enough to absorb frame-time jitter
/// (our frame production is bursty: 800 samples in 4-5 ms, then 12 ms idle).
pub struct AudioBuffer {
    buffer: Arc<Mutex<Vec<i16>>>,
}

const BUFFER_CAP: usize = 8192;     // ~85 ms latency — drop when growing past this

impl AudioBuffer {
    pub fn new() -> Self {
        // Pre-fill with silence at BUFFER_TARGET so the SDL2 callback has
        // samples to read immediately, preventing initial underruns.
        let mut initial = Vec::with_capacity(BUFFER_CAP);
        initial.resize(BUFFER_TARGET, 0);  // pre-fill with silence
        AudioBuffer {
            buffer: Arc::new(Mutex::new(initial)),
        }
    }

    /// Push samples from the emulation thread.
    /// If the buffer exceeds BUFFER_CAP, drop oldest samples instead of
    /// refusing to push new ones — this keeps the buffer fresh and
    /// bounded without causing gaps in the newer content.
    pub fn push_samples(&self, samples: &[i16]) {
        if let Ok(mut buf) = self.buffer.lock() {
            buf.extend_from_slice(samples);
            if buf.len() > BUFFER_CAP {
                let excess = buf.len() - BUFFER_CAP;
                buf.drain(..excess);
            }
        }
    }

    /// Get a clone of the Arc for the SDL2 callback.
    pub fn shared(&self) -> Arc<Mutex<Vec<i16>>> {
        self.buffer.clone()
    }

    /// Current buffer level (i16 count) — used by audio-synced pacing.
    pub fn level(&self) -> usize {
        self.buffer.lock().map(|b| b.len()).unwrap_or(0)
    }
}

// Public thresholds for audio-synced pacing (in i16 count, stereo).
pub const BUFFER_HIGH: usize = 6144; // ~64ms latency — wait when above
pub const BUFFER_TARGET: usize = 3072; // ~32ms latency — drain target

/// Initialize SDL2 audio and return the AudioBuffer + AudioDevice.
pub fn init_audio(sdl: &sdl2::Sdl) -> Option<(AudioBuffer, sdl2::audio::AudioDevice<AudioCallback>)> {
    let audio_subsystem = sdl.audio().ok()?;

    let desired_spec = sdl2::audio::AudioSpecDesired {
        freq: Some(48_000),  // macOS Core Audio native rate — avoids SDL2 resampling
        channels: Some(2),
        samples: Some(1024), // ~21.3 ms latency at 48 kHz; smooths over frame-time jitter
    };

    let buffer = AudioBuffer::new();
    let shared = buffer.shared();

    let device = audio_subsystem.open_playback(None, &desired_spec, |spec| {
        eprintln!(
            "SDL2 audio: requested freq=48000 samples=1024, obtained freq={} Hz, channels={}, samples={}, format={:?}",
            spec.freq, spec.channels, spec.samples, spec.format
        );
        AudioCallback { buffer: shared }
    }).ok()?;

    // Start playback
    device.resume();

    Some((buffer, device))
}

/// SDL2 audio callback — pulls samples from the shared buffer.
pub struct AudioCallback {
    buffer: Arc<Mutex<Vec<i16>>>,
}

impl sdl2::audio::AudioCallback for AudioCallback {
    type Channel = i16;

    fn callback(&mut self, out: &mut [i16]) {
        if let Ok(mut buf) = self.buffer.lock() {
            let available = buf.len().min(out.len());
            if available > 0 {
                out[..available].copy_from_slice(&buf[..available]);
                buf.drain(..available);
            }
            // Fill remainder with silence if buffer underrun
            for sample in out[available..].iter_mut() {
                *sample = 0;
            }
        } else {
            // Lock failed — output silence
            for sample in out.iter_mut() {
                *sample = 0;
            }
        }
    }
}
