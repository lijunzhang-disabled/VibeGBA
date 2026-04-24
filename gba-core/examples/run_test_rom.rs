//! Run a test ROM for N frames and dump the final framebuffer as a PPM.
//! For jsmolka tests, the ROM displays pass/fail text on the screen,
//! so we can eyeball the result via the generated PPM image.

use gba_core::{Gba, arm7tdmi::Cpu, SCREEN_WIDTH, SCREEN_HEIGHT};
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom_path = args.get(1).expect("usage: run_test_rom <rom> [frames]");
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600);

    let rom = std::fs::read(rom_path).expect("read rom");
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    println!("Running {} frames of {}...", frames, rom_path);
    for _ in 0..frames {
        gba.run_frame();
    }

    // Dump framebuffer to PPM for visual inspection
    let fb = gba.framebuffer();
    let base = std::path::Path::new(rom_path)
        .file_stem().unwrap().to_string_lossy();
    let ppm_path = format!("/tmp/{}_result.ppm", base);
    let mut f = std::fs::File::create(&ppm_path).expect("create ppm");
    writeln!(f, "P6\n{} {}\n255", SCREEN_WIDTH, SCREEN_HEIGHT).unwrap();
    for &p in fb {
        let r = ((p & 0x1F) as u8) << 3;
        let g = (((p >> 5) & 0x1F) as u8) << 3;
        let b = (((p >> 10) & 0x1F) as u8) << 3;
        f.write_all(&[r, g, b]).unwrap();
    }
    println!("Framebuffer → {}", ppm_path);

    // Extract rows as rough "text" by counting non-black pixels per row
    // (useful to detect if anything is drawn at all)
    println!("\nNon-zero pixel density per row (each # = 10 pixels):");
    for y in (0..SCREEN_HEIGHT).step_by(4) {
        let count = (0..SCREEN_WIDTH)
            .filter(|&x| fb[y * SCREEN_WIDTH + x] != 0)
            .count();
        let bar = "#".repeat(count / 10);
        println!("  y={:3}: {:3} {}", y, count, bar);
    }

    // Summary: does the screen show SOMETHING?
    let total_nonzero: usize = fb.iter().filter(|&&p| p != 0).count();
    println!(
        "\nTotal non-zero pixels: {}/{}  ({:.1}%)",
        total_nonzero,
        SCREEN_WIDTH * SCREEN_HEIGHT,
        100.0 * total_nonzero as f64 / (SCREEN_WIDTH * SCREEN_HEIGHT) as f64
    );
    println!("DISPCNT=0x{:04X} mode={}", gba.bus.io.dispcnt, gba.bus.io.dispcnt & 7);
    println!("Open {} in Preview to see the test output.", ppm_path);
}
