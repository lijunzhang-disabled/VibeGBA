//! Run a jsmolka test ROM and report pass/fail.
//!
//! jsmolka's m_test_eval renders text to the mode-4 VRAM framebuffer:
//!   - "All tests passed" starts at (x=56, y=76)
//!   - "Failed test NNN"  starts at (x=60, y=76)
//! Either way, the failure number is also stored as 32-bit BCD digits at
//! IWRAM[0], [4], [8] *only* in the .failed branch. Reading IWRAM is
//! ambiguous though, because tests freely scratch IWRAM as part of their
//! probes (e.g. memory.gba t002 stores 1 to IWRAM[0] to test the mirror).
//!
//! So we use the rendered framebuffer as the authoritative signal:
//! pixels at x∈[54..60), y∈[76..84) are set ONLY when "All ..." was
//! rendered (the 'A' glyph spans that region). Fall back to IWRAM digits
//! if the screen is completely blank.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = args.get(1).expect("usage: check_test <rom> [frames]");
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600);
    let data = std::fs::read(rom).expect("read rom");
    let mut gba = Gba::new(None, data);
    gba.cpu = Cpu::new_skip_bios();
    for _ in 0..frames { gba.run_frame(); }

    // Authoritative check: did "All tests passed" render? The 'A' of "All"
    // covers x=56..63 / y=76..82. The .failed text "Failed test ..." starts
    // at x=60 instead, so x∈[54..60) at y=76..83 is non-zero only on pass.
    let vram = &gba.bus.vram;
    let mut all_marker = 0u32;
    for y in 76u32..84 {
        for x in 54u32..60 {
            let off = (y * 240 + x) as usize;
            if off < vram.len() && vram[off] != 0 { all_marker += 1; }
        }
    }
    if all_marker >= 4 {
        println!("{}: ALL TESTS PASSED", rom);
        return;
    }

    // Otherwise check the IWRAM BCD slots. The .failed path stores 32-bit
    // words (so upper bytes are zero), each ≤ 9.
    let iw = gba.bus.iwram_ref();
    let read_digit = |off: usize| -> Option<u8> {
        let w = u32::from_le_bytes([iw[off], iw[off+1], iw[off+2], iw[off+3]]);
        if w <= 9 { Some(w as u8) } else { None }
    };
    match (read_digit(0), read_digit(4), read_digit(8)) {
        (Some(h), Some(t), Some(o)) => {
            let num = h as u32 * 100 + t as u32 * 10 + o as u32;
            if num == 0 {
                println!("{}: UNKNOWN (no text rendered, digits all zero) PC=0x{:08X}",
                    rom, gba.cpu.regs[15]);
            } else {
                println!("{}: Failed test {}", rom, num);
            }
        }
        _ => {
            println!("{}: UNKNOWN (no text rendered, digit slots contaminated) PC=0x{:08X}",
                rom, gba.cpu.regs[15]);
        }
    }
}
