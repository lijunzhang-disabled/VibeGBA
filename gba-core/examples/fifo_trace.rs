//! Trace DMA1 FIFO activity frame-by-frame to diagnose audio underrun.
//! Prints how many times DMA1 ran per frame and what samples it delivered.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = std::fs::read(&args[1]).unwrap();
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400);
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    let mut prev_sad = gba.bus.dma.channels[1].internal_sad;
    let mut prev_sad_b = gba.bus.dma.channels[2].internal_sad;
    let mut prev_latched_sad = gba.bus.dma.channels[1].sad;

    println!("frame | DMA1 internal_sad  sad    enabled | DMA2 internal_sad  sad    | FIFO A (count/sample) | FIFO B");
    for f in 0..frames {
        gba.run_frame();

        let d1 = &gba.bus.dma.channels[1];
        let d2 = &gba.bus.dma.channels[2];
        let fa = &gba.bus.apu.fifo_a;
        let fb = &gba.bus.apu.fifo_b;

        let sad_advanced_1 = d1.internal_sad.wrapping_sub(prev_sad);
        let sad_advanced_2 = d2.internal_sad.wrapping_sub(prev_sad_b);
        let sad_reg_changed = d1.sad != prev_latched_sad;

        if f % 10 == 0 || sad_reg_changed {
            println!("{:5} | int=0x{:08X} sad=0x{:08X} en={} adv={:5} | int=0x{:08X} sad=0x{:08X} adv={:5} | len={:2} cur={:4} | len={:2} cur={:4}{}",
                f,
                d1.internal_sad, d1.sad, d1.enabled(), sad_advanced_1,
                d2.internal_sad, d2.sad, sad_advanced_2,
                fa.len(), fa.current_sample,
                fb.len(), fb.current_sample,
                if sad_reg_changed {" ←SAD reg changed!"} else {""},
            );
        }
        prev_sad = d1.internal_sad;
        prev_sad_b = d2.internal_sad;
        prev_latched_sad = d1.sad;
    }
}
