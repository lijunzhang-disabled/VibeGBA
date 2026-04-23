//! Diagnostic: dump framebuffer histogram + save as PPM for visual inspection.

use gba_core::{Gba, arm7tdmi::Cpu};
use std::collections::HashMap;
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: fb_dump <rom> [frames]");
        std::process::exit(2);
    }
    let rom = std::fs::read(&args[1]).unwrap();
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);

    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    for f in 0..frames {
        gba.run_frame();

        // Histogram top 5 colors
        let fb = gba.framebuffer();
        let mut hist: HashMap<u16, usize> = HashMap::new();
        for &p in fb {
            *hist.entry(p).or_insert(0) += 1;
        }
        let mut sorted: Vec<_> = hist.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));

        if f < 15 || f % 10 == 0 {
            print!("frame {:3}: DISPCNT=0x{:04X} VCOUNT={:3} PC=0x{:08X}  top colors: ",
                f, gba.bus.io.dispcnt, gba.bus.io.vcount, gba.cpu.regs[15]);
            for (color, count) in sorted.iter().take(3) {
                print!("0x{:04X}×{} ", color, count);
            }
            println!();
        }
    }

    // Save final framebuffer as PPM
    let fb = gba.framebuffer();
    let mut f = std::fs::File::create("/tmp/gba_fb.ppm").unwrap();
    writeln!(f, "P6\n240 160\n255").unwrap();
    for &pixel in fb {
        let r = ((pixel & 0x1F) as u8) << 3;
        let g = (((pixel >> 5) & 0x1F) as u8) << 3;
        let b = (((pixel >> 10) & 0x1F) as u8) << 3;
        f.write_all(&[r, g, b]).unwrap();
    }
    println!("\nFramebuffer saved to /tmp/gba_fb.ppm");

    // Check palette + vram state
    let pal_nonzero = gba.bus.palette.iter().filter(|&&b| b != 0).count();
    let vram_nonzero = gba.bus.vram.iter().filter(|&&b| b != 0).count();
    println!("Palette nonzero bytes: {}/{}", pal_nonzero, gba.bus.palette.len());
    println!("VRAM nonzero bytes:    {}/{}", vram_nonzero, gba.bus.vram.len());
    println!("BG0CNT = 0x{:04X}  BG0 scroll X={} Y={}",
        gba.bus.io.bgcnt[0], gba.bus.io.bg_ofs[0][0], gba.bus.io.bg_ofs[0][1]);
}
