//! Run a ROM for N frames; report timer/IRQ/DMA state to sanity-check M4A
//! sample-rate timing.
//!
//! Usage: irq_audit <rom-path> [frames]
//!
//! M4A typical config (Pokémon):
//!   Timer 0 reload ≈ 0xFF80 → overflow every 128 cycles → 131072 Hz
//!     (or /10 for actual sample rate ≈ 13379 Hz)

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom_path = args.get(1).expect("usage: irq_audit <rom> [frames]");
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120);

    let rom = std::fs::read(rom_path).expect("read rom");
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    println!("Running {} frames of {}...", frames, rom_path);
    for _ in 0..frames { gba.run_frame(); }

    let t0 = &gba.bus.timers.timers[0];
    let t1 = &gba.bus.timers.timers[1];
    println!("\n--- Timer 0 (M4A sample clock) ---");
    println!("  enabled:       {}", t0.enabled());
    println!("  reload:        0x{:04X}  ({})", t0.reload, t0.reload as i16);
    println!("  counter:       0x{:04X}", t0.counter);
    println!("  control:       0x{:04X}", t0.control);
    println!("  prescaler:     /{}", t0.prescaler());
    println!("  irq_enabled:   {}", t0.irq_enabled());
    println!("  cascade:       {}", t0.cascade());
    let period_cycles = match t0.reload {
        0 => 65536,
        r => (0x10000u32).saturating_sub(r as u32),
    } * t0.prescaler();
    if period_cycles > 0 {
        let hz = 16_777_216.0 / period_cycles as f64;
        println!("  ⇒ overflow every {} CPU cycles ≈ {:.1} Hz", period_cycles, hz);
    }
    println!("    (M4A Pokémon default: ~13379 Hz ≈ reload 0xFF80 @ /1)");

    println!("\n--- Timer 1 (M4A buffer clock) ---");
    println!("  enabled: {}  reload: 0x{:04X}  control: 0x{:04X}  cascade: {}  irq: {}",
        t1.enabled(), t1.reload, t1.control, t1.cascade(), t1.irq_enabled());

    println!("\n--- Interrupts ---");
    println!("  IE:  0x{:04X}  (VBL=bit0, HBL=bit1, VCT=bit2, TM0=bit3, DMA0-3=bit8-11)",
        gba.bus.interrupt.ie);
    println!("  IR:  0x{:04X}", gba.bus.interrupt.ir);
    println!("  IME: {}", gba.bus.interrupt.ime);

    println!("\n--- DMA (M4A uses DMA1/2 in FIFO mode) ---");
    for idx in 1..=2 {
        let d = &gba.bus.dma.channels[idx];
        println!("  DMA{}: sad=0x{:08X} dad=0x{:08X} cnt=0x{:04X} ctrl=0x{:04X} active={} enabled={} timing={:?} repeat={}",
            idx, d.sad, d.dad, d.count, d.control, d.active, d.enabled(), d.timing(), d.repeat());
    }

    // SOUNDCNT_H lives in sound_regs. Offset from 0x04000060 base: SOUNDCNT_H = 0x82-0x81
    // but we access the backing vec directly. Let me scan for nonzero bytes.
    println!("\n--- Sound registers (first 32 bytes of 0x4000060 block) ---");
    let sr = &gba.bus.io.sound_regs;
    for i in 0..sr.len().min(80) {
        if sr[i] != 0 {
            println!("  sound_regs[0x{:02X}] = 0x{:02X}", i, sr[i]);
        }
    }
}
