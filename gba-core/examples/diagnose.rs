//! Headless diagnostic: trace CPU PC step-by-step and flag when PC leaves ROM.
//!
//! Usage: cargo run --release -p gba-core --example diagnose -- <rom> [instructions]

use gba_core::Gba;
use gba_core::arm7tdmi::Cpu;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: diagnose <rom.gba> [max_instructions]");
        std::process::exit(2);
    }
    let rom = std::fs::read(&args[1]).expect("read rom");
    let max_ins: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2000);

    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    println!("Starting PC=0x{:08X}  THUMB={}  mode={:?}", gba.cpu.regs[15], gba.cpu.cpsr.thumb(), gba.cpu.cpsr.mode());

    let mut prev_pc = gba.cpu.regs[15];
    let mut first_leave: Option<(usize, u32, u32)> = None; // (step, from, to)

    for i in 0..max_ins {
        let pc_before = gba.cpu.regs[15];
        gba.step_n(1);
        let pc_after = gba.cpu.regs[15];

        let in_rom = |p: u32| (0x0800_0000..=0x09FF_FFFF).contains(&p)
            || (0x0300_0000..=0x0300_7FFF).contains(&p)
            || (0x0200_0000..=0x0203_FFFF).contains(&p);

        if !in_rom(pc_after) && in_rom(pc_before) && first_leave.is_none() {
            first_leave = Some((i, pc_before, pc_after));
            println!("  step {}: PC left ROM/RAM: 0x{:08X} -> 0x{:08X}  (THUMB={})",
                i, pc_before, pc_after, gba.cpu.cpsr.thumb());
        }

        // Also print first 30 instructions
        if i < 30 {
            println!("  step {:4}: PC=0x{:08X}  THUMB={}  R0=0x{:08X}  R1=0x{:08X}  R13=0x{:08X}  R14=0x{:08X}",
                i, pc_before, gba.cpu.cpsr.thumb(),
                gba.cpu.regs[0], gba.cpu.regs[1], gba.cpu.regs[13], gba.cpu.regs[14]);
        }
        prev_pc = pc_after;
    }

    println!("\nAfter {} instructions:", max_ins);
    println!("  PC=0x{:08X}  LR=0x{:08X}  SP=0x{:08X}  THUMB={}  mode={:?}",
        gba.cpu.regs[15], gba.cpu.regs[14], gba.cpu.regs[13], gba.cpu.cpsr.thumb(), gba.cpu.cpsr.mode());
    println!("  DISPCNT=0x{:04X}  IE=0x{:04X}  IME={}", gba.bus.io.dispcnt, gba.bus.interrupt.ie, gba.bus.interrupt.ime);

    if let Some((step, from, to)) = first_leave {
        println!("\nFirst escape from ROM/RAM: step {}  {:08X} -> {:08X}", step, from, to);
    } else {
        println!("\nPC stayed inside ROM/RAM for all {} instructions", max_ins);
    }
}
