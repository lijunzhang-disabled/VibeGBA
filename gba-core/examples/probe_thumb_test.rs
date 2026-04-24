//! Print state step-by-step from #685 to #700 to see where LR changes.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let rom = std::fs::read("/Users/lijunzhang/secagentinfra/gba/test-roms/thumb.gba").unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    for i in 0..710u64 {
        if (685..=705).contains(&i) {
            let thumb = gba.cpu.cpsr.thumb();
            let regs15 = gba.cpu.regs[15];
            let pc_exec = regs15.wrapping_sub(if thumb { 4 } else { 8 });
            let opcode = if thumb {
                gba.bus.read16(pc_exec) as u32
            } else {
                gba.bus.read32(pc_exec)
            };
            println!(
                "[{:3}] PC=0x{:08X} {} op=0x{:08X}  R1=0x{:08X}  LR=0x{:08X}  SP=0x{:08X}  mode={:?}",
                i, pc_exec,
                if thumb {"T"} else {"A"},
                opcode,
                gba.cpu.regs[1], gba.cpu.regs[14], gba.cpu.regs[13],
                gba.cpu.cpsr.mode()
            );
        }
        gba.step_one();
    }
}
