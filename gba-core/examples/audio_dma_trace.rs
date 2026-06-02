//! Diagnose timer-driven FIFO DMA flow for velipso's rates.gba.
//! Boots the demo, navigates to the timer-driven 32K* mode via DOWN+D-pad,
//! then traces Timer 1 IRQ cadence and DMA1 latch cadence to verify the
//! timer-driven path is actually exercised.

use gba_core::{Gba, arm7tdmi::Cpu, keypad::*};

const RUN_FRAMES: usize = 360;

fn press(gba: &mut Gba, key: u16, frames: usize) {
    for _ in 0..frames {
        gba.bus.keypad.set_keys(key);
        gba.run_frame();
    }
    // release
    gba.bus.keypad.set_keys(0);
    gba.run_frame();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = std::fs::read(&args[1]).expect("read rom");

    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    // Let demo boot + idle ~60 frames.
    for _ in 0..60 { gba.run_frame(); }

    // Navigate the menu: the top row of rates.gba is the tone rate selector;
    // the bottom row is the hardware sample rate / bit depth. Per README,
    // D-pad changes the focused setting. We try a few presses to navigate
    // towards a 32K* mode. If this lands somewhere else, we still get useful
    // trace data — just not necessarily 32K*.
    press(&mut gba, KEY_DOWN, 2);
    press(&mut gba, KEY_RIGHT, 2);
    press(&mut gba, KEY_RIGHT, 2);
    press(&mut gba, KEY_RIGHT, 2);

    // Settle for 60 frames so any new timer-driven setup stabilises.
    for _ in 0..60 { gba.run_frame(); }

    println!("=== After menu navigation, tracing {} frames ===", RUN_FRAMES);

    let mut prev_irq_entries = gba.cpu.irq_entries;
    let mut prev_latch_cycle = gba.bus.dma.channels[1].last_latch_cycle;
    let mut latch_count = 0u64;
    let mut timer1_irq_frames = 0u32;
    let mut prev_ir_t1 = false;

    for f in 0..RUN_FRAMES {
        gba.run_frame();
        let dma1 = &gba.bus.dma.channels[1];
        let cur_latch = dma1.last_latch_cycle;
        if cur_latch != prev_latch_cycle {
            latch_count += 1;
            prev_latch_cycle = cur_latch;
        }
        let ir_t1 = (gba.bus.interrupt.ir & 0x0010) != 0;
        if ir_t1 && !prev_ir_t1 { timer1_irq_frames += 1; }
        prev_ir_t1 = ir_t1;

        if f < 12 || f % 60 == 0 {
            let now_irq = gba.cpu.irq_entries;
            let irq_delta = now_irq - prev_irq_entries;
            prev_irq_entries = now_irq;
            println!(
                "f={:3} IE=0x{:04X} IR=0x{:04X} IRQ+{:>3}  DMA1(act={} sad=0x{:07X} isad=0x{:07X} ctl=0x{:04X}) T0_ctl=0x{:04X} T1_ctl=0x{:04X} latch={}",
                f, gba.bus.interrupt.ie, gba.bus.interrupt.ir, irq_delta,
                dma1.active, dma1.sad & 0x07FF_FFFF, dma1.internal_sad, dma1.control,
                gba.bus.timers.timers[0].control,
                gba.bus.timers.timers[1].control,
                cur_latch,
            );
        }
    }

    println!("\nSummary:");
    println!("  IRQ handler entries:    {}", gba.cpu.irq_entries);
    println!("  VBlank IRQs raised:     {}", gba.vblank_irqs_raised);
    println!("  DMA1 latches:           {}  (≈ {:.2}/frame)",
        latch_count, latch_count as f64 / RUN_FRAMES as f64);
    println!("  Timer 1 IRQ sightings:  {}", timer1_irq_frames);
    let t1 = &gba.bus.timers.timers[1];
    println!("  Timer 1 ctl=0x{:04X} reload=0x{:04X} counter=0x{:04X} enabled={} irq_en={}",
        t1.control, t1.reload, t1.counter, t1.control & 0x80 != 0, t1.control & 0x40 != 0);
}
