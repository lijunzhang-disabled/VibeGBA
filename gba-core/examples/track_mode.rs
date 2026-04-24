//! Trace mode/SP/R8 across the FIQ round-trip at 0x08000AE0..0x08000AF8.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let rom = std::fs::read("/Users/lijunzhang/secagentinfra/gba/test-roms/arm.gba").unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();
    gba.step_one();
    let mut i: u64 = 1;
    for _ in 0..200_000u64 {
        let thumb = gba.cpu.cpsr.thumb();
        let pc = if gba.cpu.pipeline_flushed {
            gba.cpu.regs[15]
        } else if thumb {
            gba.cpu.regs[15].wrapping_sub(4)
        } else {
            gba.cpu.regs[15].wrapping_sub(8)
        };
        if (0x08000AD0..=0x08000B10).contains(&pc) {
            let op = if thumb { gba.bus.read16(pc) as u32 } else { gba.bus.read32(pc) };
            println!("[{:5}] PC=0x{:08X} op=0x{:08X} mode={:?} SP=0x{:08X} R8=0x{:08X} LR=0x{:08X}",
                i, pc, op, gba.cpu.cpsr.mode(),
                gba.cpu.regs[13], gba.cpu.regs[8], gba.cpu.regs[14]);
        }
        gba.step_one();
        i += 1;
    }
}
