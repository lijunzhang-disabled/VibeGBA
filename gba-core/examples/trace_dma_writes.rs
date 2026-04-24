//! Poll DMA1/2 sad/dad/count/control every step and report changes.
//! If Pokémon's M4A vblank handler is updating DMA registers, we should
//! see writes to sad/control on each vblank.

use gba_core::{Gba, arm7tdmi::Cpu};

fn main() {
    let rom = std::fs::read("/Users/lijunzhang/Documents/PokemonEmeraldVersion.gba").unwrap();
    let mut gba = Gba::new(None, rom);
    gba.cpu = Cpu::new_skip_bios();

    // Run until the game has stabilised.
    for _ in 0..20 { gba.run_frame(); }
    println!("Post-boot: starting DMA1/2 trace over 600_000 steps");

    let mut last_sad1 = gba.bus.dma.channels[1].sad;
    let mut last_sad2 = gba.bus.dma.channels[2].sad;
    let mut last_ctl1 = gba.bus.dma.channels[1].control;
    let mut last_ctl2 = gba.bus.dma.channels[2].control;

    let mut changes = 0u32;
    for i in 0..600_000u32 {
        let d1 = &gba.bus.dma.channels[1];
        let d2 = &gba.bus.dma.channels[2];
        if d1.sad != last_sad1 || d2.sad != last_sad2 ||
           d1.control != last_ctl1 || d2.control != last_ctl2 {
            println!("[{:6}] DMA1 sad 0x{:08X}->0x{:08X}  ctl 0x{:04X}->0x{:04X}  (int=0x{:08X}) | DMA2 sad 0x{:08X}->0x{:08X}  ctl 0x{:04X}->0x{:04X}  (int=0x{:08X})",
                i,
                last_sad1, d1.sad, last_ctl1, d1.control, d1.internal_sad,
                last_sad2, d2.sad, last_ctl2, d2.control, d2.internal_sad,
            );
            last_sad1 = d1.sad;
            last_sad2 = d2.sad;
            last_ctl1 = d1.control;
            last_ctl2 = d2.control;
            changes += 1;
            if changes > 60 { println!("(more than 60 changes — stopping print)"); return; }
        }
        gba.step_one();
    }
    println!("\nTotal changes: {}", changes);
}
