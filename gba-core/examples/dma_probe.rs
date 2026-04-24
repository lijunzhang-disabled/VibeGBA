//! Probe DMA + timer + FIFO state during emulation.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = std::fs::read(&args[1]).unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(300);

    for f in 0..frames {
        gba.run_frame();
        if f % 30 == 0 {
            let dma = &gba.bus.dma;
            let timers = &gba.bus.timers;
            let apu = &gba.bus.apu;
            println!("frame {}: ", f);
            for i in 0..4 {
                let ch = &dma.channels[i];
                println!("  DMA{}: enabled={} active={} timing={:?} src=0x{:08X} dst=0x{:08X} count={} ctrl=0x{:04X}",
                    i, ch.enabled(), ch.active, ch.timing(),
                    ch.internal_sad, ch.internal_dad, ch.internal_count, ch.control);
            }
            for i in 0..4 {
                let t = &timers.timers[i];
                println!("  TMR{}: enabled={} counter=0x{:04X} reload=0x{:04X} prescaler={} ctrl=0x{:04X}",
                    i, t.enabled(), t.counter, t.reload, t.prescaler(), t.control);
            }
            println!("  FIFO_A: count={} current={} tsel={} L={} R={} vol_full={}",
                apu.fifo_a.len(), apu.fifo_a.current_sample, apu.fifo_a.timer_select,
                apu.fifo_a.enable_left, apu.fifo_a.enable_right, apu.fifo_a.volume_full);
            println!("  FIFO_B: count={} current={} tsel={} L={} R={} vol_full={}",
                apu.fifo_b.len(), apu.fifo_b.current_sample, apu.fifo_b.timer_select,
                apu.fifo_b.enable_left, apu.fifo_b.enable_right, apu.fifo_b.volume_full);
            println!();
        }
    }
}
