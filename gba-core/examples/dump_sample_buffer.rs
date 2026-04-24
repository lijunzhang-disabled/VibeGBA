//! After running N frames, dump the IWRAM region Pokémon uses as its
//! M4A sample buffer. Shows the raw signed-byte samples and a simple
//! ASCII plot to visualise what DMA is reading out.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = std::fs::read(&args[1]).unwrap();
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(200);
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();
    for _ in 0..frames { gba.run_frame(); }

    let sad = gba.bus.dma.channels[1].sad & 0x7FFF;
    let iw = gba.bus.iwram_ref();

    println!("DMA1 sad=0x{:08X} → IWRAM offset 0x{:04X}", gba.bus.dma.channels[1].sad, sad);
    println!("Dumping 512 bytes of sample buffer (signed int8):");

    let base = sad as usize;
    for row in 0..32 {
        let off = base + row * 16;
        if off + 16 > iw.len() { break; }
        print!("0x{:04X}: ", off);
        for b in &iw[off..off + 16] {
            print!("{:+4} ", *b as i8);
        }
        println!();
    }

    // Statistics over 1KB of the buffer
    let window: Vec<i8> = (0..1024)
        .map(|i| iw[(base + i) & (iw.len() - 1)] as i8)
        .collect();
    let zeros = window.iter().filter(|&&x| x == 0).count();
    let max = window.iter().map(|&x| x.unsigned_abs() as u32).max().unwrap();
    let avg = window.iter().map(|&x| x.unsigned_abs() as u32).sum::<u32>() / 1024;
    let nonzero = 1024 - zeros;
    println!("\n1024-byte stats: zeros={} ({:.0}%)  nonzero={} ({:.0}%)  max|x|={}  avg|x|={}",
        zeros, 100.0 * zeros as f32 / 1024.0,
        nonzero, 100.0 * nonzero as f32 / 1024.0,
        max, avg);

    // ASCII plot: 64 samples wide, amplitude scaled to 21 rows
    println!("\nASCII waveform (first 128 bytes, amplitude on Y):");
    for row in (-3..=3).rev() {
        let thresh_low = row * 30;
        let thresh_high = thresh_low + 30;
        print!("{:+4}: ", thresh_low);
        for i in 0..128 {
            let v = window[i] as i32;
            if v > thresh_low && v <= thresh_high { print!("#"); }
            else if row == 0 && v == 0 { print!("-"); }
            else { print!(" "); }
        }
        println!();
    }
}
