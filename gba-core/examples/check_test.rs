//! Run a jsmolka test ROM for N frames and print the failed test number,
//! by reading the BCD digits m_test_eval stores at IWRAM[0..12].

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = args.get(1).expect("usage: check_test <rom> [frames]");
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600);
    let data = std::fs::read(rom).expect("read rom");
    let mut gba = Gba::new(None, data);
    gba.cpu = Cpu::new_skip_bios();
    for _ in 0..frames { gba.run_frame(); }

    let iw = gba.bus.iwram_ref();
    let h = iw[0];
    let t = iw[4];
    let o = iw[8];
    if h <= 9 && t <= 9 && o <= 9 {
        let num = h as u32 * 100 + t as u32 * 10 + o as u32;
        if num == 0 {
            println!("{}: ALL TESTS PASSED", rom);
        } else {
            println!("{}: Failed test {}", rom, num);
        }
    } else {
        println!("{}: IWRAM[0,4,8]={:02X} {:02X} {:02X} (not in digit range — escape or unfinished)",
            rom, h, t, o);
    }
}
