//! Run the emulator step-by-step, detect PC escape from ROM/RAM, and dump
//! the last N instructions leading up to the escape.

use gba_core::{Gba, arm7tdmi::Cpu};
use std::collections::VecDeque;

const TRACE_DEPTH: usize = 60;
const MAX_INSTRUCTIONS: u64 = 5_000_000;

#[derive(Clone)]
struct Entry {
    pc: u32,
    thumb: bool,
    opcode: u32,
    regs: [u32; 16],
    cpsr: u32,
}

fn in_rom_ram(p: u32) -> bool {
    (0x0800_0000..=0x0DFF_FFFF).contains(&p)
        || (0x0300_0000..=0x0300_7FFF).contains(&p)
        || (0x0200_0000..=0x0203_FFFF).contains(&p)
        || (0x0000_0000..=0x0000_3FFF).contains(&p)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: trace_escape <rom>");
        std::process::exit(2);
    }
    let rom = std::fs::read(&args[1]).unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    let mut trace: VecDeque<Entry> = VecDeque::with_capacity(TRACE_DEPTH + 1);

    for i in 0..MAX_INSTRUCTIONS {
        // Capture state before this step
        let thumb = gba.cpu.cpsr.thumb();
        let regs15 = gba.cpu.regs[15];
        // Executing instruction address: regs[15] - 8 (ARM) or - 4 (THUMB)
        let exec_pc = regs15.wrapping_sub(if thumb { 4 } else { 8 });
        let opcode = if thumb {
            gba.bus.read16(exec_pc) as u32
        } else {
            gba.bus.read32(exec_pc)
        };

        let entry = Entry {
            pc: exec_pc,
            thumb,
            opcode,
            regs: gba.cpu.regs,
            cpsr: gba.cpu.cpsr.bits,
        };

        if trace.len() == TRACE_DEPTH {
            trace.pop_front();
        }
        trace.push_back(entry.clone());

        // Run one instruction + pending events
        gba.step_one();

        // Check for escape (only consider the instruction that CAUSED the escape, not subsequent steps in garbage)
        let new_regs15 = gba.cpu.regs[15];
        let new_thumb = gba.cpu.cpsr.thumb();
        let new_exec_pc = new_regs15.wrapping_sub(if new_thumb { 4 } else { 8 });

        if in_rom_ram(exec_pc) && !in_rom_ram(new_exec_pc) {
            println!("*** PC ESCAPED at instruction #{} ***", i);
            println!("Instruction that caused escape:");
            println!("  0x{:08X} (THUMB={}) op=0x{:08X}", exec_pc, thumb, opcode);
            println!("  Before: regs[15]=0x{:08X} R0=0x{:08X} R1=0x{:08X} R2=0x{:08X} R3=0x{:08X} LR=0x{:08X}",
                regs15, entry.regs[0], entry.regs[1], entry.regs[2], entry.regs[3], entry.regs[14]);
            println!("  After:  regs[15]=0x{:08X} R0=0x{:08X} R1=0x{:08X} R2=0x{:08X} R3=0x{:08X} LR=0x{:08X}",
                new_regs15, gba.cpu.regs[0], gba.cpu.regs[1], gba.cpu.regs[2], gba.cpu.regs[3], gba.cpu.regs[14]);
            println!("\nLast {} instructions (oldest → newest):", trace.len());
            for (idx, e) in trace.iter().enumerate() {
                println!("[{:3}] 0x{:08X} {}  op=0x{:08X}  R0={:08X} R1={:08X} R12={:08X} SP={:08X} LR={:08X} CPSR={:08X}",
                    idx, e.pc,
                    if e.thumb { "T" } else { "A" },
                    e.opcode,
                    e.regs[0], e.regs[1], e.regs[12], e.regs[13], e.regs[14], e.cpsr);
            }
            return;
        }
    }

    println!("No PC escape in {} instructions.", MAX_INSTRUCTIONS);
}
