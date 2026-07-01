//! Print DMA1 re-latch cadence + internal_sad advance per frame, to understand
//! how a game drives its FIFO DMA (and whether the VBlank re-anchor matters).
//! Usage: dma1_latch_probe <rom> [frames]
use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = std::fs::read(&args[1]).expect("read rom");
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120);
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    let mut prev_latch = 0u64;
    let mut prev_sad = 0u32;
    println!("frame | en | ctl   | latch_cyc  | Δlatch(f) | internal_sad | sad        | Δisad");
    for f in 0..frames {
        gba.run_frame();
        let c = &gba.bus.dma.channels[1];
        let latch = c.last_latch_cycle;
        let relatched = latch != prev_latch;
        let dlatch_frames = if relatched {
            format!("{:.1}", (latch.saturating_sub(prev_latch)) as f64 / 280896.0)
        } else { "-".into() };
        let disad = c.internal_sad.wrapping_sub(prev_sad) as i32;
        // Only print frames of interest: relatch events or every 15th frame.
        if relatched || f % 15 == 0 {
            println!("{:5} | {:2} | 0x{:04X}| {:10} | {:>9} | 0x{:08X}   | 0x{:08X} | {}",
                f, c.enabled() as u8, c.control, latch, dlatch_frames,
                c.internal_sad, c.sad & 0x07FF_FFFF, disad);
        }
        prev_latch = latch;
        prev_sad = c.internal_sad;
    }
}
