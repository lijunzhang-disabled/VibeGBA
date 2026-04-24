//! Count VBlank IRQ acknowledgements (writes to IF register) per frame.
//! If the game's ISR runs every vblank, we expect ~1 acknowledgement/frame.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = std::fs::read(&args[1]).unwrap();
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(200);
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    // We poll IRQ state each frame and count state changes.
    // On real hardware IR gets set when IRQ occurs, ISR acks it by writing.
    let mut last_ir = 0u16;
    let mut total_vblank_sets = 0u32;
    let mut total_vblank_acks = 0u32;

    for f in 0..frames {
        gba.run_frame();
        let ir = gba.bus.interrupt.ir;
        let pc = gba.cpu.regs[15];
        let mode = gba.cpu.cpsr.mode();
        let halted = gba.cpu.halted;
        if f < 30 || f % 20 == 0 {
            println!("frame {:4}: IE=0x{:04X} IR=0x{:04X} IME={} DISPSTAT=0x{:04X} VCOUNT={} PC=0x{:08X} mode={:?} halted={}",
                f, gba.bus.interrupt.ie, ir, gba.bus.interrupt.ime,
                gba.bus.io.dispstat, gba.bus.io.vcount, pc, mode, halted);
        }
        // Count when VBL bit goes from 0 to 1 (new IRQ latched) and 1 to 0 (ack)
        let prev_vbl = last_ir & 1;
        let curr_vbl = ir & 1;
        if prev_vbl == 0 && curr_vbl == 1 { total_vblank_sets += 1; }
        if prev_vbl == 1 && curr_vbl == 0 { total_vblank_acks += 1; }
        last_ir = ir;
    }
    println!("\nOver {} frames:", frames);
    println!("  (sampled at frame boundaries, possibly missing intra-frame events)");
    println!("  ir-set transitions observed at sample time = {}", total_vblank_sets);
    println!("  ir-ack transitions observed at sample time = {}", total_vblank_acks);
    println!("\nAuthoritative counts (per-event):");
    println!("  VBlank entries (line==160 transitions): {}  (expected ≈ {})",
        gba.vblank_entries, frames);
    println!("  VBlank IRQ requests raised:             {}  (expected ≈ {} if DISPSTAT bit 3 always set)",
        gba.vblank_irqs_raised, frames);
    println!("  CPU IRQ handler entries (any IRQ):       {}  (expected ≈ {} if game's ISR runs)",
        gba.cpu.irq_entries, frames);
}
