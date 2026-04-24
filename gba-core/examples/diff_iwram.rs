//! Snapshot IWRAM before and after a vblank; report what changed.
//! If M4A is working, the audio buffer region at 0x03006000-0x03007000
//! should see ~224 bytes of writes per frame.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let rom = std::fs::read("/Users/lijunzhang/Documents/PokemonEmeraldVersion.gba").unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    // Boot well past any intro logos.
    for _ in 0..500 { gba.run_frame(); }

    // Snapshot A (frame 500)
    let snap_a: Vec<u8> = gba.bus.iwram_ref().to_vec();
    let dma1_sad_a = gba.bus.dma.channels[1].internal_sad;

    // Advance 100 more frames.
    for _ in 0..100 { gba.run_frame(); }

    let snap_b: Vec<u8> = gba.bus.iwram_ref().to_vec();
    let dma1_sad_b = gba.bus.dma.channels[1].internal_sad;
    println!("DISPCNT=0x{:04X} mode={}  BG0CNT=0x{:04X}",
        gba.bus.io.dispcnt, gba.bus.io.dispcnt & 7, gba.bus.io.bgcnt[0]);
    println!("VBlank IRQ entries: {}", gba.vblank_irqs_raised);

    println!("Frame before DMA1 internal_sad = 0x{:08X} → iwram offset 0x{:04X}",
        dma1_sad_a, dma1_sad_a & 0x7FFF);
    println!("Frame after  DMA1 internal_sad = 0x{:08X} → iwram offset 0x{:04X}",
        dma1_sad_b, dma1_sad_b & 0x7FFF);
    println!("DMA advanced {} bytes this frame\n",
        dma1_sad_b.wrapping_sub(dma1_sad_a));

    // Diff every 4-byte word
    let mut changes_per_region: [u32; 8] = [0; 8]; // 8 regions of 4KB each
    let mut total_changes = 0u32;
    let mut change_addrs: Vec<u32> = Vec::new();
    for off in 0..snap_a.len() {
        if snap_a[off] != snap_b[off] {
            total_changes += 1;
            let region = off / 4096;
            changes_per_region[region.min(7)] += 1;
            if change_addrs.len() < 40 {
                change_addrs.push(off as u32);
            }
        }
    }

    println!("Changed bytes in IWRAM this frame: {}/{}", total_changes, snap_a.len());
    println!("By 4KB region:");
    for (i, count) in changes_per_region.iter().enumerate() {
        if *count > 0 {
            println!("  0x{:04X}-0x{:04X}: {}", i * 4096, i * 4096 + 4095, count);
        }
    }

    if !change_addrs.is_empty() {
        println!("\nFirst changed addresses:");
        for a in change_addrs.iter().take(20) {
            println!("  0x{:04X}: 0x{:02X} → 0x{:02X}", a, snap_a[*a as usize], snap_b[*a as usize]);
        }
    }

    // Also check a range AROUND the DMA source position to see if M4A
    // wrote to the region DMA is currently reading.
    let dma_pos = (dma1_sad_a & 0x7FFF) as usize;
    println!("\n256 bytes around DMA1 source (before→after):");
    for off in 0..256 {
        let a = (dma_pos + off) & 0x7FFF;
        if snap_a[a] != snap_b[a] {
            print!("*");
        } else {
            print!(".");
        }
        if off % 64 == 63 { println!(); }
    }
}
