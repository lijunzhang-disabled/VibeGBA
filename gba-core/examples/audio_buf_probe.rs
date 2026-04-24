//! Inspect memory for audio data patterns.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let rom = std::fs::read(&std::env::args().nth(1).unwrap()).unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    // Run for 5 seconds
    for _ in 0..300 {
        gba.run_frame();
    }

    let dma_a_src = gba.bus.dma.channels[1].internal_sad;
    let dma_b_src = gba.bus.dma.channels[2].internal_sad;
    println!("DMA1 (FIFO A): src=0x{:08X}", dma_a_src);
    println!("DMA2 (FIFO B): src=0x{:08X}", dma_b_src);

    let iwram = gba.bus.iwram_ref().to_vec();
    let ewram = gba.bus.ewram_ref().to_vec();

    // Scan for regions that look like audio data (non-trivial byte variation
    // with small adjacent-sample jumps — typical of a waveform).
    let scan_region = |data: &[u8], base_addr: u32, label: &str| {
        println!("\n=== {} ({} bytes) ===", label, data.len());
        let zeros = data.iter().filter(|&&b| b == 0).count();
        let unique = data.iter().collect::<std::collections::HashSet<_>>().len();
        println!("  Zeros: {} ({:.1}%)   Unique values: {}",
            zeros, 100.0 * zeros as f64 / data.len() as f64, unique);

        // Scan 1 KB windows for audio-like content:
        //   - has diverse bytes (lots of unique values)
        //   - small adjacent jumps (typical waveform: avg |dx| < 20)
        //   - not all zero/all same
        let win = 1024;
        let mut audio_regions: Vec<(u32, usize, f64, usize)> = Vec::new();
        for start in (0..data.len()).step_by(win) {
            let end = (start + win).min(data.len());
            let slice = &data[start..end];
            let nz = slice.iter().filter(|&&b| b != 0).count();
            if nz < 50 { continue; } // mostly zero, skip
            let unique_count: usize = slice.iter().collect::<std::collections::HashSet<_>>().len();
            let avg_jump: f64 = slice.windows(2)
                .map(|w| (w[1] as i8 as i32 - w[0] as i8 as i32).abs() as f64)
                .sum::<f64>() / slice.len() as f64;
            audio_regions.push((base_addr + start as u32, nz, avg_jump, unique_count));
        }

        audio_regions.sort_by(|a, b| b.1.cmp(&a.1));
        println!("  Top 5 non-zero 1KB regions (by nonzero count):");
        for (addr, nz, avg_jump, uniq) in audio_regions.iter().take(5) {
            let looks_like_audio = if *avg_jump < 25.0 && *uniq > 20 { " (audio-like)" } else { "" };
            println!("    0x{:08X}: {} nonzero, avg|dx|={:.1}, {} unique{}",
                addr, nz, avg_jump, uniq, looks_like_audio);
        }
    };

    scan_region(&iwram, 0x0300_0000, "IWRAM");
    scan_region(&ewram, 0x0200_0000, "EWRAM");
}
