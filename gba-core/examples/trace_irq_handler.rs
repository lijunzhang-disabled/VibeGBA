//! Run Pokémon a bit, then single-step into the IRQ handler entry
//! to verify our HLE BIOS IRQ stub is jumping to the right user handler
//! and the handler's first few instructions make sense.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let rom = std::fs::read("/Users/lijunzhang/Documents/PokemonEmeraldVersion.gba").unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    // Run until we've entered the IRQ handler at least once.
    for _ in 0..20 {
        gba.run_frame();
    }

    // Dump ISR vector storage.
    let handler = gba.bus.read32(0x03007FFC);
    println!("User IRQ handler pointer [0x03007FFC] = 0x{:08X}", handler);
    println!("BIOS IRQ stub at 0x00000018:");
    for off in (0..0x18u32).step_by(4) {
        let a = 0x18 + off;
        let op = gba.bus.read32(a);
        println!("  0x{:08X}: 0x{:08X}", a, op);
    }

    // Read 10 words from the user handler's address.
    let handler_base = handler & !1;
    let thumb = handler & 1 != 0;
    println!("\nUser handler at 0x{:08X}  (THUMB={})", handler_base, thumb);
    for i in 0..12 {
        let a = handler_base + i * 4;
        let op = gba.bus.read32(a);
        println!("  0x{:08X}: 0x{:08X}", a, op);
    }

    // Dump IRQ-mode SP & LR pre-IRQ
    println!("\nCurrent state:");
    println!("  PC=0x{:08X} mode={:?}", gba.cpu.regs[15], gba.cpu.cpsr.mode());
    println!("  SP=0x{:08X}", gba.cpu.regs[13]);
    println!("  IRQ entries so far: {}", gba.cpu.irq_entries);
    println!("  VBlank IRQs raised: {}", gba.vblank_irqs_raised);
    println!("  IE=0x{:04X}  IR=0x{:04X}  IME={}",
        gba.bus.interrupt.ie, gba.bus.interrupt.ir, gba.bus.interrupt.ime);

    // Game's polling value: read the memory word at the poll target
    // PC stuck around 0x080008C6 — single-step a bit to see what it reads.
    println!("\nRunning 600_000 more steps (≈2 frames) to cross VBlank:");
    let mut poll_loop_pc = 0u32;
    let mut in_poll = true;
    for i in 0..600_000u32 {
        let thumb = gba.cpu.cpsr.thumb();
        let pc = if gba.cpu.pipeline_flushed {
            gba.cpu.regs[15]
        } else if thumb {
            gba.cpu.regs[15].wrapping_sub(4)
        } else {
            gba.cpu.regs[15].wrapping_sub(8)
        };
        // Detect when we leave the poll loop
        if in_poll && !(0x080008C0..=0x080008E0).contains(&pc) {
            println!("  [{:6}] ESCAPED poll loop! Now at PC=0x{:08X} mode={:?} irq_entries={}",
                i, pc, gba.cpu.cpsr.mode(), gba.cpu.irq_entries);
            in_poll = false;
        }
        if !in_poll && (0x080008C0..=0x080008E0).contains(&pc) {
            println!("  [{:6}] RE-entered poll loop at PC=0x{:08X}", i, pc);
            in_poll = true;
        }
        poll_loop_pc = pc;
        if i % 100_000 == 0 {
            println!("  [{:6}] PC=0x{:08X} mode={:?} halted={} R2=0x{:08X} R1=0x{:08X} irq={}",
                i, pc, gba.cpu.cpsr.mode(), gba.cpu.halted,
                gba.cpu.regs[2], gba.cpu.regs[1], gba.cpu.irq_entries);
        }
        gba.step_one();
    }
    let _ = poll_loop_pc;
}
