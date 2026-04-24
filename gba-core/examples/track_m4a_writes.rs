//! For one frame, snapshot IWRAM before and after, list every CHANGED
//! byte with its IWRAM offset, and show where DMA1 is reading.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let rom = std::fs::read("/Users/lijunzhang/Documents/PokemonEmeraldVersion.gba").unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    // Boot into gameplay.
    for _ in 0..500 { gba.run_frame(); }

    // Snapshot A
    let snap_a: Vec<u8> = gba.bus.iwram_ref().to_vec();
    let sad_before = gba.bus.dma.channels[1].internal_sad;

    gba.run_frame();
    let snap_b: Vec<u8> = gba.bus.iwram_ref().to_vec();
    let sad_after = gba.bus.dma.channels[1].internal_sad;

    println!("DMA1 internal_sad: 0x{:08X} → 0x{:08X}", sad_before, sad_after);
    println!("IWRAM offsets the DMA traversed this frame: 0x{:04X}..0x{:04X}",
        sad_before & 0x7FFF, sad_after & 0x7FFF);

    // Group changes in 64-byte chunks
    let mut chunks: std::collections::BTreeMap<u16, u32> = std::collections::BTreeMap::new();
    for off in 0..snap_a.len() {
        if snap_a[off] != snap_b[off] {
            *chunks.entry((off / 64) as u16).or_insert(0) += 1;
        }
    }
    println!("\n64-byte chunks with writes this frame:");
    for (chunk, count) in &chunks {
        let start = *chunk as u32 * 64;
        let end = start + 63;
        println!("  0x{:04X}-0x{:04X}: {} bytes written", start, end, count);
    }

    // Focused: look at 2KB around DMA pre-position
    println!("\n2KB centred on DMA pre-position 0x{:04X}:", sad_before & 0x7FFF);
    let center = (sad_before & 0x7FFF) as i32;
    for off in (-1024..1024i32).step_by(64) {
        let a = ((center + off) as u32 & 0x7FFF) as usize;
        let mut changes = 0;
        for i in 0..64 {
            let aa = (a + i) & 0x7FFF;
            if snap_a[aa] != snap_b[aa] { changes += 1; }
        }
        if changes > 0 {
            println!("  off {:+5} → IWRAM 0x{:04X}: {} writes", off, a, changes);
        }
    }
}
