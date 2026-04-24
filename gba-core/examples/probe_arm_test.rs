//! Probe where the ARM test rom ends up — detect loop, dump recent PC trace.

use gba_core::{Gba, arm7tdmi::Cpu};
use std::collections::HashMap;

fn main() {
    let rom = std::fs::read("/Users/lijunzhang/secagentinfra/gba/test-roms/arm.gba").unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    // Run for enough cycles to finish the test
    for _ in 0..600 {
        gba.run_frame();
    }

    // Step a few thousand more and count PC occurrences
    let mut hits: HashMap<u32, u64> = HashMap::new();
    for _ in 0..200_000u64 {
        let thumb = gba.cpu.cpsr.thumb();
        let pc = gba.cpu.regs[15].wrapping_sub(if thumb { 4 } else { 8 });
        *hits.entry(pc).or_insert(0) += 1;
        gba.step_one();
    }

    let mut sorted: Vec<_> = hits.into_iter().collect();
    sorted.sort_by_key(|&(_, c)| std::cmp::Reverse(c));
    println!("Top 20 PCs (likely loop / idle):");
    for (pc, c) in sorted.iter().take(20) {
        let thumb = gba.cpu.cpsr.thumb();
        let op = if thumb { gba.bus.read16(*pc) as u32 } else { gba.bus.read32(*pc) };
        println!("  PC=0x{:08X}  count={:6}  op=0x{:08X}", pc, c, op);
    }

    println!("\nFinal state:");
    println!("  R0=0x{:08X} R1=0x{:08X} R7=0x{:08X}",
        gba.cpu.regs[0], gba.cpu.regs[1], gba.cpu.regs[7]);
    println!("  PC=0x{:08X} mode={:?} thumb={}",
        gba.cpu.regs[15], gba.cpu.cpsr.mode(), gba.cpu.cpsr.thumb());
    println!("  DISPCNT=0x{:04X} BG0CNT=0x{:04X}", gba.bus.io.dispcnt, gba.bus.io.bgcnt[0]);
    println!("  Palette[0]=0x{:04X} Palette[1]=0x{:04X}",
        gba.bus.read16(0x05000000), gba.bus.read16(0x05000002));
}
