//! Dump state around SP corruption at step 461.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let rom = std::fs::read("/Users/lijunzhang/secagentinfra/gba/test-roms/arm.gba").unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    gba.step_one();
    let mut i: u64 = 1;

    for _ in 0..200_000u64 {
        let in_window = (455..=465).contains(&i);
        if in_window {
            let thumb = gba.cpu.cpsr.thumb();
            let pc = if gba.cpu.pipeline_flushed {
                gba.cpu.regs[15]
            } else if thumb {
                gba.cpu.regs[15].wrapping_sub(4)
            } else {
                gba.cpu.regs[15].wrapping_sub(8)
            };
            let op = if thumb { gba.bus.read16(pc) as u32 } else { gba.bus.read32(pc) };
            println!("[{}] PC=0x{:08X} {} op=0x{:08X} SP=0x{:08X} LR=0x{:08X} R0..R4={:08X},{:08X},{:08X},{:08X},{:08X} flush={}",
                i, pc, if thumb {"T"} else {"A"}, op,
                gba.cpu.regs[13], gba.cpu.regs[14],
                gba.cpu.regs[0], gba.cpu.regs[1], gba.cpu.regs[2], gba.cpu.regs[3], gba.cpu.regs[4],
                gba.cpu.pipeline_flushed);
        }
        gba.step_one();
        i += 1;
        if i > 465 { break; }
    }
}
