//! Dump audio samples from Emerald to a WAV file and print APU state per second.
//!
//! Usage: audio_dump <rom> [seconds]

use gba_core::{Gba, arm7tdmi::Cpu};
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: audio_dump <rom> [seconds]");
        std::process::exit(2);
    }
    let rom = std::fs::read(&args[1]).unwrap();
    let secs: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);

    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    let frames = secs * 60;
    let mut all_samples: Vec<i16> = Vec::with_capacity(secs * 48000 * 2);
    let mut audio_tmp = vec![0i16; 4096];

    println!("Running {} frames ({} seconds of emulated time)…", frames, secs);

    for f in 0..frames {
        let t0 = std::time::Instant::now();
        gba.run_frame();
        let wall_ms = t0.elapsed().as_secs_f32() * 1000.0;

        let n = gba.drain_audio(&mut audio_tmp);
        all_samples.extend_from_slice(&audio_tmp[..n]);

        if f % 60 == 0 {
            let apu = &gba.bus.apu;
            let mut nonsilent = 0;
            let mut max_abs = 0i16;
            let mut sum_abs: i64 = 0;
            for &s in &audio_tmp[..n] {
                if s != 0 { nonsilent += 1; }
                if s.unsigned_abs() > max_abs.unsigned_abs() { max_abs = s; }
                sum_abs += s.unsigned_abs() as i64;
            }
            let avg_abs = if n > 0 { sum_abs / n as i64 } else { 0 };
            println!(
                "frame {:4}: wall={:.2}ms samples={} nonsilent={} max|x|={} avg|x|={} | master={} PSG_L={} PSG_R={} FIFO_A(L={},R={}) FIFO_B(L={},R={}) ch1.en={} ch2.en={} ch3.en={} ch4.en={}",
                f, wall_ms, n, nonsilent, max_abs.unsigned_abs(), avg_abs,
                apu.master_enable,
                apu.psg_volume_left, apu.psg_volume_right,
                apu.fifo_a.enable_left as u8, apu.fifo_a.enable_right as u8,
                apu.fifo_b.enable_left as u8, apu.fifo_b.enable_right as u8,
                apu.ch1.enabled as u8, apu.ch2.enabled as u8,
                apu.ch3.enabled as u8, apu.ch4.enabled as u8,
            );
        }
    }

    // Write WAV (48000 Hz, stereo, 16-bit PCM)
    let path = "/tmp/gba_audio.wav";
    write_wav(path, &all_samples, 48000, 2).expect("wav write");
    println!("\nWrote {} stereo samples to {}", all_samples.len() / 2, path);

    // Simple sample histogram
    let mut bucket_counts = [0u32; 8];
    for &s in &all_samples {
        let magnitude = s.unsigned_abs() as u32;
        let bucket = if magnitude == 0 { 0 }
            else if magnitude < 100 { 1 }
            else if magnitude < 500 { 2 }
            else if magnitude < 2000 { 3 }
            else if magnitude < 8000 { 4 }
            else if magnitude < 20000 { 5 }
            else if magnitude < 32000 { 6 }
            else { 7 };
        bucket_counts[bucket] += 1;
    }
    let total = all_samples.len() as u32;
    println!("\nSample amplitude histogram ({} total samples):", total);
    println!("  zero:           {:>8} ({:>5.1}%)", bucket_counts[0], 100.0 * bucket_counts[0] as f32 / total as f32);
    println!("  1-99:           {:>8} ({:>5.1}%)", bucket_counts[1], 100.0 * bucket_counts[1] as f32 / total as f32);
    println!("  100-499:        {:>8} ({:>5.1}%)", bucket_counts[2], 100.0 * bucket_counts[2] as f32 / total as f32);
    println!("  500-1999:       {:>8} ({:>5.1}%)", bucket_counts[3], 100.0 * bucket_counts[3] as f32 / total as f32);
    println!("  2000-7999:      {:>8} ({:>5.1}%)", bucket_counts[4], 100.0 * bucket_counts[4] as f32 / total as f32);
    println!("  8000-19999:     {:>8} ({:>5.1}%)", bucket_counts[5], 100.0 * bucket_counts[5] as f32 / total as f32);
    println!("  20000-31999:    {:>8} ({:>5.1}%)", bucket_counts[6], 100.0 * bucket_counts[6] as f32 / total as f32);
    println!("  32000+ (clip):  {:>8} ({:>5.1}%)", bucket_counts[7], 100.0 * bucket_counts[7] as f32 / total as f32);
}

fn write_wav(path: &str, samples: &[i16], sample_rate: u32, channels: u16) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    let data_len = samples.len() * 2;
    let byte_rate = sample_rate * channels as u32 * 2;
    let block_align = channels * 2;

    // RIFF header
    f.write_all(b"RIFF")?;
    f.write_all(&((36 + data_len) as u32).to_le_bytes())?;
    f.write_all(b"WAVE")?;

    // fmt chunk
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;            // PCM
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&block_align.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;           // 16 bits per sample

    // data chunk
    f.write_all(b"data")?;
    f.write_all(&(data_len as u32).to_le_bytes())?;
    for &s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}
