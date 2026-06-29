//! Reproduce + diagnose the Emerald boot hang under TIMING_MODE=1.
//!
//! Runs Emerald with skip-BIOS, watches the THUMB spin loop near 0x080008C6,
//! and on detecting the spin dumps CPU regs, the polled [r2+0x1C] address +
//! value, and IRQ/IO state. Also prints periodic PC samples so we can see how
//! far boot got.
//!
//! Usage: TIMING_MODE=1 cargo run -p gba-core --example emerald_hang --release -- <rom> [max_frames]

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom_path = args.get(1).map(|s| s.as_str())
        .unwrap_or("/Users/lijunzhang/Documents/PokemonEmeraldVersion.gba");
    let max_frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120);

    let rom = std::fs::read(rom_path).expect("read rom");
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();
    // NOTE: Bus::new now seeds DISPCNT=0x0080 (forced blank) on the HLE/skip-
    // BIOS path, matching the real BIOS handoff. That is the fix for the hang
    // this example was written to diagnose; running it should report "No hang".

    let mode: u8 = std::env::var("TIMING_MODE").ok()
        .and_then(|v| v.parse().ok()).unwrap_or(0);
    eprintln!("TIMING_MODE={} rom={}", mode, rom_path);

    let watch_dispstat = std::env::var("WATCH_DISPSTAT").is_ok();
    let mut last_dispstat_irq = 0u16; // only the IRQ-enable bits (0x38)
    // Count visits to the CopyBufferedValueToGpuReg(DISPSTAT) hardware-write
    // path (PC ~0x08001040-0x08001060 in mode 0).
    let mut setgpu_hits = 0u64;

    // Spin detection: count consecutive visits to the poll loop PC range.
    let mut last_pc = 0u32;
    let mut same_count = 0u64;
    let mut steps = 0u64;
    let mut dumped = false;

    for frame in 0..max_frames {
        for _ in 0..50_000 {
            // PC in THUMB: regs[15] is +4 ahead. Executing instr at regs[15]-4.
            let pc = gba.cpu.regs[15].wrapping_sub(if gba.cpu.cpsr.thumb() { 4 } else { 8 });
            gba.step_one();
            steps += 1;

            if (0x0800_1040..=0x0800_1060).contains(&pc) { setgpu_hits += 1; }

            if watch_dispstat {
                let ds = gba.bus.read16(0x0400_0004) & 0x0038; // VB/HB/VC IRQ enables
                if ds != last_dispstat_irq {
                    eprintln!("[DISPSTAT-IRQ] step {:>9} PC=0x{:08X} {:#06X} -> {:#06X} (IE=0x{:04X})",
                        steps, pc, last_dispstat_irq, ds, gba.bus.interrupt.read_ie());
                    last_dispstat_irq = ds;
                }
            }

            let _ = last_pc;
            // Detect the documented spin region. Require *consecutive* in-range
            // steps (resets on leaving) so we only fire on a true sustained spin.
            if (0x0800_0880..=0x0800_08E0).contains(&pc) {
                same_count += 1;
                if !dumped && same_count > 200_000 {
                    eprintln!("setgpu_hits(0x1040-60)={}", setgpu_hits);
                    dump_state(&mut gba, pc, steps);
                    dumped = true;
                }
            } else {
                same_count = 0;
            }
            // Generic deep-spin detection anywhere.
            if !dumped && same_count > 2_000_000 {
                eprintln!("\n=== GENERIC SPIN at PC=0x{:08X} after {} steps ===", pc, steps);
                dump_state(&mut gba, pc, steps);
                dumped = true;
            }
        }
        if frame % 10 == 0 {
            let pc = gba.cpu.regs[15];
            let intrcheck = gba.bus.read16(0x0300_22DC);
            let dispstat = gba.bus.read16(0x0400_0004);
            let vcount = gba.bus.read16(0x0400_0006);
            eprintln!("frame {:3}: PC=0x{:08X} IE=0x{:04X} IF=0x{:04X} IME={} irqE={} vblE={} vblR={} DISPSTAT=0x{:04X} VC={} intrChk=0x{:04X}",
                frame, pc,
                gba.bus.interrupt.read_ie(), gba.bus.interrupt.read_if(),
                gba.bus.interrupt.ime, gba.cpu.irq_entries, gba.vblank_entries,
                gba.vblank_irqs_raised, dispstat, vcount, intrcheck);
        }
        if dumped { break; }
    }
    if !dumped {
        eprintln!("No hang detected in {} frames. Final PC=0x{:08X}", max_frames, gba.cpu.regs[15]);
    }
}

fn dump_state(gba: &mut Gba, pc: u32, steps: u64) {
    let r = &gba.cpu.regs;
    eprintln!("\n=== SPIN DETECTED at PC=0x{:08X} (step {}) ===", pc, steps);
    for i in 0..16 {
        eprint!("r{:<2}=0x{:08X}  ", i, r[i]);
        if i % 4 == 3 { eprintln!(); }
    }
    let dispcnt = gba.bus.read16(0x0400_0000);
    eprintln!("DISPCNT=0x{:04X} forced_blank={} VCOUNT={}",
        dispcnt, (dispcnt >> 7) & 1, gba.bus.read16(0x0400_0006));
    let r2 = r[2];
    let poll_addr = r2.wrapping_add(0x1C);
    eprintln!("poll target [r2+0x1C] = 0x{:08X}", poll_addr);
    let v16 = gba.bus.read16(poll_addr);
    eprintln!("  value (read16) = 0x{:04X}  bit0={}", v16, v16 & 1);
    eprintln!("IE=0x{:04X} IF=0x{:04X} IME={} CPSR.I={}",
        gba.bus.interrupt.read_ie(), gba.bus.interrupt.read_if(),
        gba.bus.interrupt.ime, gba.cpu.cpsr.irq_disabled());
    // Dump key SIO/IO regs
    for &a in &[0x0400_0128u32, 0x0400_012A, 0x0400_0134, 0x0400_0200, 0x0400_0202, 0x0400_0208] {
        eprintln!("  IO[0x{:08X}] = 0x{:04X}", a, gba.bus.read16(a));
    }
}
