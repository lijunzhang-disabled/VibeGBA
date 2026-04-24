//! Track the first instruction where SP escapes the valid stack region.

use gba_core::{Gba, arm7tdmi::Cpu};

fn sp_valid(sp: u32) -> bool {
    // Valid stack targets: IWRAM (0x03000000-0x03007FFF) or EWRAM (0x02000000-0x0203FFFF)
    matches!(sp >> 24, 0x03 | 0x02)
}

fn main() {
    let rom = std::fs::read("/Users/lijunzhang/secagentinfra/gba/test-roms/arm.gba").unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    gba.step_one();
    let mut i: u64 = 1;
    let mut last_sp = gba.cpu.regs[13];
    let mut prev_pc = 0u32;

    println!("Starting SP=0x{:08X}", last_sp);

    for _ in 0..200_000u64 {
        let sp = gba.cpu.regs[13];
        if sp != last_sp {
            let thumb = gba.cpu.cpsr.thumb();
            let pc = if gba.cpu.pipeline_flushed {
                gba.cpu.regs[15]
            } else if thumb {
                gba.cpu.regs[15].wrapping_sub(4)
            } else {
                gba.cpu.regs[15].wrapping_sub(8)
            };
            if !sp_valid(sp) {
                println!("[{:8}] SP CORRUPTED: 0x{:08X} -> 0x{:08X} at prev_pc=0x{:08X} now at PC=0x{:08X} mode={} thumb={}",
                    i, last_sp, sp, prev_pc, pc,
                    if thumb {"T"} else {"A"}, thumb);
                // Context: dump prev few instructions
                return;
            }
            last_sp = sp;
        }
        let thumb = gba.cpu.cpsr.thumb();
        prev_pc = if gba.cpu.pipeline_flushed {
            gba.cpu.regs[15]
        } else if thumb {
            gba.cpu.regs[15].wrapping_sub(4)
        } else {
            gba.cpu.regs[15].wrapping_sub(8)
        };
        gba.step_one();
        i += 1;
    }
    println!("No SP corruption detected. Final SP=0x{:08X}", gba.cpu.regs[13]);
}
