//! Audio output via SDL2.
//!
//! Uses a simple ring buffer approach: the emulation loop pushes samples
//! into a shared buffer, and the SDL2 audio callback pulls from it.

use std::sync::{Arc, Mutex};

/// Shared audio buffer between emulation thread and SDL2 audio callback.
pub struct AudioBuffer {
    buffer: Arc<Mutex<Vec<i16>>>,
}

impl AudioBuffer {
    pub fn new() -> Self {
        AudioBuffer {
            buffer: Arc::new(Mutex::new(Vec::with_capacity(8192))),
        }
    }

    /// Push samples from the emulation thread.
    pub fn push_samples(&self, samples: &[i16]) {
        if let Ok(mut buf) = self.buffer.lock() {
            // Cap the buffer to prevent unbounded growth when audio lags
            if buf.len() < 16384 {
                buf.extend_from_slice(samples);
            }
        }
    }

    /// Get a clone of the Arc for the SDL2 callback.
    pub fn shared(&self) -> Arc<Mutex<Vec<i16>>> {
        self.buffer.clone()
    }
}

/// Initialize SDL2 audio and return the AudioBuffer + AudioDevice.
pub fn init_audio(sdl: &sdl2::Sdl) -> Option<(AudioBuffer, sdl2::audio::AudioDevice<AudioCallback>)> {
    let audio_subsystem = sdl.audio().ok()?;

    let desired_spec = sdl2::audio::AudioSpecDesired {
        freq: Some(32768),  // Match GBA sample rate
        channels: Some(2),  // Stereo
        samples: Some(512), // ~15.6ms latency at 32768 Hz
    };

    let buffer = AudioBuffer::new();
    let shared = buffer.shared();

    let device = audio_subsystem.open_playback(None, &desired_spec, |_spec| {
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
