//! Find CPU escape in thumb.gba (similar to trace_arm_escape).

use gba_core::{Gba, arm7tdmi::Cpu};

fn in_valid_code(pc: u32) -> bool {
    matches!(pc >> 24, 0x08 | 0x09 | 0x03 | 0x02 | 0x00)
}

fn main() {
    let rom = std::fs::read("/Users/lijunzhang/secagentinfra/gba/test-roms/thumb.gba").unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    const TRAIL: usize = 80;
    let mut ring: Vec<(u64, u32, u32, bool, u32, u32, bool, [u32; 8])> = Vec::with_capacity(TRAIL);
    let mut i: u64 = 0;
    gba.step_one();
    i += 1;

    for _ in 0..50_000_000u64 {
        let thumb = gba.cpu.cpsr.thumb();
        let flushed = gba.cpu.pipeline_flushed;
        let pc = if flushed {
            gba.cpu.regs[15]
        } else if thumb {
            gba.cpu.regs[15].wrapping_sub(4)
        } else {
            gba.cpu.regs[15].wrapping_sub(8)
        };
        let op = if thumb { gba.bus.read16(pc) as u32 } else { gba.bus.read32(pc) };
        let sp = gba.cpu.regs[13];
        let lr = gba.cpu.regs[14];
        let rs = [
            gba.cpu.regs[0], gba.cpu.regs[1], gba.cpu.regs[2], gba.cpu.regs[3],
            gba.cpu.regs[4], gba.cpu.regs[5], gba.cpu.regs[6], gba.cpu.regs[7],
        ];

        if !in_valid_code(pc) {
            println!("[{}] ESCAPED: PC=0x{:08X} thumb={} op=0x{:08X} SP=0x{:08X} LR=0x{:08X}",
                i, pc, thumb, op, sp, lr);
            println!("\nLast {} valid instructions:", TRAIL);
            for (idx, p, o, t, s, l, f, r) in &ring {
                println!("  [{:8}] PC=0x{:08X} {} op=0x{:04X} SP=0x{:08X} LR=0x{:08X} flush={} r0-7={:08X},{:08X},{:08X},{:08X},{:08X},{:08X},{:08X},{:08X}",
                    idx, p, if *t {"T"} else {"A"}, o, s, l, f,
                    r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]);
            }
            return;
        }
        if ring.len() >= TRAIL { ring.remove(0); }
        ring.push((i, pc, op, thumb, sp, lr, flushed, rs));
        gba.step_one();
        i += 1;
    }
    println!("No escape in {} steps. PC=0x{:08X}", i, gba.cpu.regs[15]);
}
