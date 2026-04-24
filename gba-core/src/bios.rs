//! BIOS High-Level Emulation (HLE).
//!
//! Implements GBA BIOS system calls (SWI) in Rust, so the emulator
//! can run without a real BIOS dump. Reference: GBATEK "BIOS Functions".
//!
//! SWI numbers are passed in the comment field:
//!   ARM mode: SWI #nn -> comment = nn (bits 23:0, but only 0-0x2B used)
//!   THUMB mode: SWI #nn -> comment = nn (bits 7:0)
//!
//! Parameters are passed in R0-R3, results returned in R0-R3.
//!
//! ## Coverage
//!
//! We HLE 22 of ~40 GBA BIOS SWIs. The missing ones are mostly sound-related
//! (SWI 0x19-0x1F), music player (0x20-0x24), MultiBoot (0x25), and a handful
//! of undocumented/rarely-used calls (0x26-0x2A).
//!
//! Missing SWIs just log a warning and do nothing. Games that rely on BIOS
//! sound functions will silently have broken audio. Workaround: load a real
//! BIOS dump with `Gba::new(Some(bios), rom)`.
//!
//! Note: Pokémon Ruby/Sapphire/Emerald ship their own M4A sound driver in
//! ROM and do NOT call the BIOS sound SWIs — so a missing 0x1A-0x1F is not
//! the cause of their audio issues.
//!
//! See `PLAN.md` Phase 9 for the full list of missing SWIs.

use crate::arm7tdmi::Cpu;
use crate::bus::Bus;

/// Handle a BIOS SWI call via HLE. Returns true if handled.
pub fn handle_swi(cpu: &mut Cpu, bus: &mut Bus, comment: u8) -> bool {
    match comment {
        0x00 => swi_soft_reset(cpu, bus),
        0x01 => swi_register_ram_reset(cpu, bus),
        0x02 => swi_halt(cpu),
        0x03 => swi_stop(cpu),
        0x04 => swi_intr_wait(cpu, bus),
        0x05 => swi_vblank_intr_wait(cpu, bus),
        0x06 => swi_div(cpu),
        0x07 => swi_div_arm(cpu),
        0x08 => swi_sqrt(cpu),
        0x09 => swi_arctan(cpu),
        0x0A => swi_arctan2(cpu),
        0x0B => swi_cpu_set(cpu, bus),
        0x0C => swi_cpu_fast_set(cpu, bus),
        0x0D => swi_get_bios_checksum(cpu),
        0x0E => swi_bg_affine_set(cpu, bus),
        0x0F => swi_obj_affine_set(cpu, bus),
        0x10 => swi_bit_unpack(cpu, bus),
        0x11 => swi_lz77_uncomp_wram(cpu, bus),
        0x12 => swi_lz77_uncomp_vram(cpu, bus),
        0x13 => swi_huffman_uncomp(cpu, bus),
        0x14 => swi_rl_uncomp_wram(cpu, bus),
        0x15 => swi_rl_uncomp_vram(cpu, bus),
        _ => {
            log::warn!("Unhandled SWI 0x{:02X}", comment);
            return false;
        }
    }
    true
}

// ─── SWI 0x00: SoftReset ─────────────────────────────────────────

fn swi_soft_reset(cpu: &mut Cpu, bus: &mut Bus) {
    // Check flag at 0x03007FFA: 0=ROM, 1=RAM
    let flag = bus.read8(0x0300_7FFA);

    // Clear 0x03007E00-0x03007FFF (512 bytes of IWRAM)
    for addr in 0x0300_7E00..0x0300_8000u32 {
        bus.write8(addr, 0);
    }

    // Reset registers
    for i in 0..13 {
        cpu.regs[i] = 0;
    }
    cpu.regs[14] = 0;

    // Set stack pointers
    cpu.regs[13] = 0x0300_7F00; // SP_usr/sys
    cpu.banked.sp[crate::arm7tdmi::CpuMode::Irq.bank_index()] = 0x0300_7FA0;
    cpu.banked.sp[crate::arm7tdmi::CpuMode::Supervisor.bank_index()] = 0x0300_7FE0;

    // Jump to entry
    if flag != 0 {
        cpu.regs[15] = 0x0200_0000; // RAM entry
    } else {
        cpu.regs[15] = 0x0800_0000; // ROM entry
    }

    cpu.cpsr = crate::arm7tdmi::Psr::new(crate::arm7tdmi::CpuMode::System);
    cpu.cpsr.bits &= !(1 << 7); // Enable IRQ
    cpu.pipeline_flushed = true;
}

// ─── SWI 0x01: RegisterRamReset ──────────────────────────────────

fn swi_register_ram_reset(cpu: &mut Cpu, bus: &mut Bus) {
    let flags = cpu.regs[0];

    // Bit 0: Clear 256KB EWRAM
    if flags & (1 << 0) != 0 {
        for addr in (0x0200_0000..0x0204_0000u32).step_by(4) {
            bus.write32(addr, 0);
        }
    }
    // Bit 1: Clear 32KB IWRAM (except last 0x200 bytes)
    if flags & (1 << 1) != 0 {
        for addr in (0x0300_0000..0x0300_7E00u32).step_by(4) {
            bus.write32(addr, 0);
        }
    }
    // Bit 2: Clear palette
    if flags & (1 << 2) != 0 {
        for addr in (0x0500_0000..0x0500_0400u32).step_by(4) {
            bus.write32(addr, 0);
        }
    }
    // Bit 3: Clear VRAM
    if flags & (1 << 3) != 0 {
        for addr in (0x0600_0000..0x0601_8000u32).step_by(4) {
            bus.write32(addr, 0);
        }
    }
    // Bit 4: Clear OAM
    if flags & (1 << 4) != 0 {
        for addr in (0x0700_0000..0x0700_0400u32).step_by(4) {
            bus.write32(addr, 0);
        }
    }
    // Bit 5: Reset SIO registers
    // Bit 6: Reset sound registers
    // Bit 7: Reset other I/O registers
    // (simplified: skip for now)
}

// ─── SWI 0x02: Halt ──────────────────────────────────────────────

fn swi_halt(cpu: &mut Cpu) {
    cpu.halted = true;
}

// ─── SWI 0x03: Stop ──────────────────────────────────────────────

fn swi_stop(cpu: &mut Cpu) {
    // Stop is like halt but deeper — for now, treat as halt
    cpu.halted = true;
}

// ─── SWI 0x04: IntrWait ──────────────────────────────────────────

fn swi_intr_wait(cpu: &mut Cpu, bus: &mut Bus) {
    let discard_old = cpu.regs[0] != 0;
    let irq_flags = cpu.regs[1] as u16;

    if discard_old {
        // Clear the requested flags in the BIOS IRQ flags mirror at 0x03007FF8
        let current = bus.read16(0x0300_7FF8);
        bus.write16(0x0300_7FF8, current & !irq_flags);
    }

    // Store what we're waiting for — the main loop will check this
    // For HLE, we just halt the CPU. The interrupt handler will resume it
    // when the requested IRQ fires.
    cpu.halted = true;

    // Store the wait flags for the IRQ handler to check
    // We use R1 to remember what we're waiting for
    // The real BIOS loops checking 0x03007FF8 & R1
}

// ─── SWI 0x05: VBlankIntrWait ────────────────────────────────────

fn swi_vblank_intr_wait(cpu: &mut Cpu, bus: &mut Bus) {
    // Equivalent to IntrWait(1, 1) — wait for VBlank
    cpu.regs[0] = 1;
    cpu.regs[1] = 1; // VBlank IRQ flag
    swi_intr_wait(cpu, bus);
}

// ─── SWI 0x06: Div ───────────────────────────────────────────────

fn swi_div(cpu: &mut Cpu) {
    let numer = cpu.regs[0] as i32;
    let denom = cpu.regs[1] as i32;

    if denom == 0 {
        log::warn!("SWI Div: division by zero");
        return;
    }

    cpu.regs[0] = (numer / denom) as u32;           // Quotient
    cpu.regs[1] = (numer % denom) as u32;           // Remainder
    cpu.regs[3] = (numer / denom).unsigned_abs();    // Abs(quotient)
}

// ─── SWI 0x07: DivArm ───────────────────────────────────────────

fn swi_div_arm(cpu: &mut Cpu) {
    // Same as Div but with swapped parameters
    let temp = cpu.regs[0];
    cpu.regs[0] = cpu.regs[1];
    cpu.regs[1] = temp;
    swi_div(cpu);
}

// ─── SWI 0x08: Sqrt ──────────────────────────────────────────────

fn swi_sqrt(cpu: &mut Cpu) {
    let val = cpu.regs[0];
    cpu.regs[0] = (val as f64).sqrt() as u32;
}

// ─── SWI 0x09: ArcTan ────────────────────────────────────────────

fn swi_arctan(cpu: &mut Cpu) {
    let tan = cpu.regs[0] as i16 as f64 / 16384.0;
    let result = tan.atan() * (16384.0 / std::f64::consts::FRAC_PI_2);
    cpu.regs[0] = result as i16 as u16 as u32;
}

// ─── SWI 0x0A: ArcTan2 ───────────────────────────────────────────

fn swi_arctan2(cpu: &mut Cpu) {
    let x = cpu.regs[0] as i16 as f64;
    let y = cpu.regs[1] as i16 as f64;
    let result = y.atan2(x);
    // Convert to GBA angle format (0..0xFFFF = 0..2*PI)
    let angle = result * (0x8000 as f64 / std::f64::consts::PI);
    cpu.regs[0] = angle as i16 as u16 as u32;
}

// ─── SWI 0x0B: CpuSet ────────────────────────────────────────────

fn swi_cpu_set(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.regs[0];
    let dst = cpu.regs[1];
    let ctrl = cpu.regs[2];

    let count = ctrl & 0x1F_FFFF;
    let fixed = ctrl & (1 << 24) != 0; // Fill mode (fixed source)
    let word = ctrl & (1 << 26) != 0;  // 32-bit mode

    if word {
        let fill_val = if fixed { bus.read32(src & !3) } else { 0 };
        for i in 0..count {
            let val = if fixed {
                fill_val
            } else {
                bus.read32(src.wrapping_add(i * 4) & !3)
            };
            bus.write32(dst.wrapping_add(i * 4) & !3, val);
        }
    } else {
        let fill_val = if fixed { bus.read16(src & !1) } else { 0 };
        for i in 0..count {
            let val = if fixed {
                fill_val
            } else {
                bus.read16(src.wrapping_add(i * 2) & !1)
            };
            bus.write16(dst.wrapping_add(i * 2) & !1, val);
        }
    }
}

// ─── SWI 0x0C: CpuFastSet ────────────────────────────────────────

fn swi_cpu_fast_set(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.regs[0];
    let dst = cpu.regs[1];
    let ctrl = cpu.regs[2];

    let count = ctrl & 0x1F_FFFF;
    let fixed = ctrl & (1 << 24) != 0;

    // CpuFastSet always operates in 32-bit mode, 8 words at a time
    let fill_val = if fixed { bus.read32(src & !3) } else { 0 };
    let count_rounded = (count + 7) & !7; // Round up to multiple of 8

    for i in 0..count_rounded {
        let val = if fixed {
            fill_val
        } else {
            bus.read32(src.wrapping_add(i * 4) & !3)
        };
        bus.write32(dst.wrapping_add(i * 4) & !3, val);
    }
}

// ─── SWI 0x0D: GetBiosChecksum ───────────────────────────────────

fn swi_get_bios_checksum(cpu: &mut Cpu) {
    cpu.regs[0] = 0xBAAE_187F; // Known GBA BIOS checksum
}

// ─── SWI 0x0E: BgAffineSet ───────────────────────────────────────

fn swi_bg_affine_set(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.regs[0];
    let dst = cpu.regs[1];
    let count = cpu.regs[2];

    for i in 0..count {
        let src_addr = src.wrapping_add(i * 20);
        let dst_addr = dst.wrapping_add(i * 16);

        // Source: center X/Y (32-bit), display X/Y (16-bit), scaleX/Y (16-bit), angle (16-bit)
        let cx = bus.read32(src_addr) as i32;
        let cy = bus.read32(src_addr + 4) as i32;
        let disp_x = bus.read16(src_addr + 8) as i16 as i32;
        let disp_y = bus.read16(src_addr + 10) as i16 as i32;
        let sx = bus.read16(src_addr + 12) as i16 as f64 / 256.0;
        let sy = bus.read16(src_addr + 14) as i16 as f64 / 256.0;
        let angle = bus.read16(src_addr + 16);

        let theta = (angle as f64) * 2.0 * std::f64::consts::PI / 65536.0;
        let cos_a = theta.cos();
        let sin_a = theta.sin();

        let pa = (sx * cos_a * 256.0) as i16;
        let pb = (-sx * sin_a * 256.0) as i16;
        let pc = (sy * sin_a * 256.0) as i16;
        let pd = (sy * cos_a * 256.0) as i16;

        let start_x = cx - (pa as i32 * disp_x + pb as i32 * disp_y);
        let start_y = cy - (pc as i32 * disp_x + pd as i32 * disp_y);

        bus.write16(dst_addr, pa as u16);
        bus.write16(dst_addr + 2, pb as u16);
        bus.write16(dst_addr + 4, pc as u16);
        bus.write16(dst_addr + 6, pd as u16);
        bus.write32(dst_addr + 8, start_x as u32);
        bus.write32(dst_addr + 12, start_y as u32);
    }
}

// ─── SWI 0x0F: ObjAffineSet ──────────────────────────────────────

fn swi_obj_affine_set(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.regs[0];
    let dst = cpu.regs[1];
    let count = cpu.regs[2];
    let stride = cpu.regs[3]; // Offset between parameter groups in dest (typically 8 for OAM, 2 for BG)

    for i in 0..count {
        let src_addr = src.wrapping_add(i * 8);
        let dst_addr = dst.wrapping_add(i * stride * 4); // stride is in halfwords? Actually offset

        let sx = bus.read16(src_addr) as i16 as f64 / 256.0;
        let sy = bus.read16(src_addr + 2) as i16 as f64 / 256.0;
        let angle = bus.read16(src_addr + 4);

        let theta = (angle as f64) * 2.0 * std::f64::consts::PI / 65536.0;
        let cos_a = theta.cos();
        let sin_a = theta.sin();

        let pa = (sx * cos_a * 256.0) as i16;
        let pb = (-sx * sin_a * 256.0) as i16;
        let pc = (sy * sin_a * 256.0) as i16;
        let pd = (sy * cos_a * 256.0) as i16;

        let offset = stride; // Stride between pa/pb/pc/pd entries (in bytes)
        bus.write16(dst_addr, pa as u16);
        bus.write16(dst_addr + offset, pb as u16);
        bus.write16(dst_addr + offset * 2, pc as u16);
        bus.write16(dst_addr + offset * 3, pd as u16);
    }
}

// ─── SWI 0x10: BitUnPack ─────────────────────────────────────────

fn swi_bit_unpack(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.regs[0];
    let dst = cpu.regs[1];
    let info_ptr = cpu.regs[2];

    let src_len = bus.read16(info_ptr) as u32;
    let src_width = bus.read8(info_ptr + 2);
    let dst_width = bus.read8(info_ptr + 3);
    let data_offset = bus.read32(info_ptr + 4);
    let zero_flag = data_offset & (1 << 31) != 0;
    let data_offset = data_offset & 0x7FFF_FFFF;

    if src_width == 0 || dst_width == 0 {
        return;
    }

    let mut src_pos = 0u32;
    let mut dst_pos = 0u32;
    let mut dst_buffer = 0u32;
    let mut dst_bits = 0u32;

    let src_mask = (1u32 << src_width) - 1;

    while src_pos < src_len {
        let byte = bus.read8(src.wrapping_add(src_pos));
        src_pos += 1;

        let mut bit_offset = 0u8;
        while bit_offset < 8 {
            let val = ((byte >> bit_offset) as u32) & src_mask;
            bit_offset += src_width;

            let out = if val == 0 && !zero_flag {
                0
            } else {
                val + data_offset
            };

            dst_buffer |= out << dst_bits;
            dst_bits += dst_width as u32;

            if dst_bits >= 32 {
                bus.write32(dst.wrapping_add(dst_pos), dst_buffer);
                dst_pos += 4;
                dst_buffer = 0;
                dst_bits = 0;
            }
        }
    }

    if dst_bits > 0 {
        bus.write32(dst.wrapping_add(dst_pos), dst_buffer);
    }
}

// ─── SWI 0x11: LZ77UnCompWram ────────────────────────────────────

fn swi_lz77_uncomp_wram(cpu: &mut Cpu, bus: &mut Bus) {
    lz77_decompress(cpu.regs[0], cpu.regs[1], bus, false);
}

// ─── SWI 0x12: LZ77UnCompVram ────────────────────────────────────

fn swi_lz77_uncomp_vram(cpu: &mut Cpu, bus: &mut Bus) {
    lz77_decompress(cpu.regs[0], cpu.regs[1], bus, true);
}

fn lz77_decompress(src: u32, dst: u32, bus: &mut Bus, vram_mode: bool) {
    let header = bus.read32(src);
    let decompressed_size = header >> 8;

    let mut src_pos = src + 4;
    let mut dst_pos = dst;
    let mut remaining = decompressed_size;

    // VRAM mode writes 16-bit at a time
    let mut vram_buffer = 0u16;
    let mut vram_byte_count = 0u32;

    while remaining > 0 {
        let flags = bus.read8(src_pos);
        src_pos += 1;

        for bit in (0..8).rev() {
            if remaining == 0 {
                break;
            }

            if flags & (1 << bit) != 0 {
                // Compressed: reference to earlier data
                let byte1 = bus.read8(src_pos) as u32;
                let byte2 = bus.read8(src_pos + 1) as u32;
                src_pos += 2;

                let length = ((byte1 >> 4) + 3) as u32;
                let offset = (((byte1 & 0xF) << 8) | byte2) + 1;

                for _ in 0..length {
                    if remaining == 0 {
                        break;
                    }
                    let val = bus.read8(dst_pos.wrapping_sub(offset));

                    if vram_mode {
                        if vram_byte_count & 1 == 0 {
                            vram_buffer = val as u16;
                        } else {
                            vram_buffer |= (val as u16) << 8;
                            bus.write16(dst_pos & !1, vram_buffer);
                        }
                        vram_byte_count += 1;
                    } else {
                        bus.write8(dst_pos, val);
                    }
                    dst_pos += 1;
                    remaining -= 1;
                }
            } else {
                // Uncompressed: literal byte
                let val = bus.read8(src_pos);
                src_pos += 1;

                if vram_mode {
                    if vram_byte_count & 1 == 0 {
                        vram_buffer = val as u16;
                    } else {
                        vram_buffer |= (val as u16) << 8;
                        bus.write16(dst_pos & !1, vram_buffer);
                    }
                    vram_byte_count += 1;
                } else {
                    bus.write8(dst_pos, val);
                }
                dst_pos += 1;
                remaining -= 1;
            }
        }
    }
}

// ─── SWI 0x13: HuffUnComp ────────────────────────────────────────

fn swi_huffman_uncomp(cpu: &mut Cpu, bus: &mut Bus) {
    let src = cpu.regs[0];
    let dst = cpu.regs[1];

    let header = bus.read32(src);
    let data_size = header >> 8;
    let bit_length = (header >> 4) & 0xF; // 4 or 8

    let tree_size = (bus.read8(src + 4) as u32 + 1) * 2;
    let tree_start = src + 5;
    let data_start = tree_start + tree_size - 1; // Align to 4 bytes
    let data_start = (data_start + 3) & !3;

    let mut src_pos = data_start;
    let mut dst_pos = dst;
    let mut remaining = data_size;
    let mut dst_buffer = 0u32;
    let mut dst_bits = 0u32;

    while remaining > 0 {
        let data_word = bus.read32(src_pos);
        src_pos += 4;

        for bit_idx in (0..32).rev() {
            if remaining == 0 {
                break;
            }

            let bit = (data_word >> bit_idx) & 1;

            // Walk the Huffman tree
            let mut node_addr = tree_start;
            let node = bus.read8(node_addr);
            let child_offset = (node & 0x3F) as u32;
            let is_leaf_0 = node & 0x80 != 0;
            let is_leaf_1 = node & 0x40 != 0;

            if bit == 0 {
                node_addr = node_addr + (child_offset + 1) * 2;
                if is_leaf_0 {
                    let leaf_val = bus.read8(node_addr) as u32;
                    dst_buffer |= leaf_val << dst_bits;
                    dst_bits += bit_length;
                    if dst_bits >= 32 {
                        bus.write32(dst_pos, dst_buffer);
                        dst_pos += 4;
                        remaining = remaining.saturating_sub(4);
                        dst_buffer = 0;
                        dst_bits = 0;
                    }
                }
            } else {
                node_addr = node_addr + (child_offset + 1) * 2 + 1;
                if is_leaf_1 {
                    let leaf_val = bus.read8(node_addr) as u32;
                    dst_buffer |= leaf_val << dst_bits;
                    dst_bits += bit_length;
                    if dst_bits >= 32 {
                        bus.write32(dst_pos, dst_buffer);
                        dst_pos += 4;
                        remaining = remaining.saturating_sub(4);
                        dst_buffer = 0;
                        dst_bits = 0;
                    }
                }
            }
        }
    }
}

// ─── SWI 0x14: RLUnCompWram ──────────────────────────────────────

fn swi_rl_uncomp_wram(cpu: &mut Cpu, bus: &mut Bus) {
    rl_decompress(cpu.regs[0], cpu.regs[1], bus, false);
}

// ─── SWI 0x15: RLUnCompVram ──────────────────────────────────────

fn swi_rl_uncomp_vram(cpu: &mut Cpu, bus: &mut Bus) {
    rl_decompress(cpu.regs[0], cpu.regs[1], bus, true);
}

fn rl_decompress(src: u32, dst: u32, bus: &mut Bus, vram_mode: bool) {
    let header = bus.read32(src);
    let decompressed_size = header >> 8;

    let mut src_pos = src + 4;
    let mut dst_pos = dst;
    let mut remaining = decompressed_size;

    let mut vram_buffer = 0u16;
    let mut vram_byte_count = 0u32;

    while remaining > 0 {
        let flag = bus.read8(src_pos);
        src_pos += 1;

        if flag & 0x80 != 0 {
            // Compressed run: repeat next byte (flag & 0x7F) + 3 times
            let length = (flag & 0x7F) as u32 + 3;
            let val = bus.read8(src_pos);
            src_pos += 1;

            for _ in 0..length {
                if remaining == 0 {
                    break;
                }
                if vram_mode {
                    if vram_byte_count & 1 == 0 {
                        vram_buffer = val as u16;
                    } else {
                        vram_buffer |= (val as u16) << 8;
                        bus.write16(dst_pos & !1, vram_buffer);
                    }
                    vram_byte_count += 1;
                } else {
                    bus.write8(dst_pos, val);
                }
                dst_pos += 1;
                remaining -= 1;
            }
        } else {
            // Uncompressed run: copy next (flag & 0x7F) + 1 bytes literally
            let length = (flag & 0x7F) as u32 + 1;

            for _ in 0..length {
                if remaining == 0 {
                    break;
                }
                let val = bus.read8(src_pos);
                src_pos += 1;

                if vram_mode {
                    if vram_byte_count & 1 == 0 {
                        vram_buffer = val as u16;
                    } else {
                        vram_buffer |= (val as u16) << 8;
                        bus.write16(dst_pos & !1, vram_buffer);
                    }
                    vram_byte_count += 1;
                } else {
                    bus.write8(dst_pos, val);
                }
                dst_pos += 1;
                remaining -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cpu_bus() -> (Cpu, Bus) {
        let cpu = Cpu::new_skip_bios();
        let bus = Bus::new(None, vec![0; 256]);
        (cpu, bus)
    }

    #[test]
    fn test_swi_div() {
        let (mut cpu, _bus) = make_cpu_bus();
        cpu.regs[0] = 100;
        cpu.regs[1] = 7;
        swi_div(&mut cpu);
        assert_eq!(cpu.regs[0], 14);  // 100 / 7 = 14
        assert_eq!(cpu.regs[1], 2);   // 100 % 7 = 2
        assert_eq!(cpu.regs[3], 14);  // |14| = 14
    }

    #[test]
    fn test_swi_div_negative() {
        let (mut cpu, _bus) = make_cpu_bus();
        cpu.regs[0] = (-100i32) as u32;
        cpu.regs[1] = 7;
        swi_div(&mut cpu);
        assert_eq!(cpu.regs[0] as i32, -14);
        assert_eq!(cpu.regs[1] as i32, -2);
        assert_eq!(cpu.regs[3], 14);
    }

    #[test]
    fn test_swi_sqrt() {
        let (mut cpu, _bus) = make_cpu_bus();
        cpu.regs[0] = 144;
        swi_sqrt(&mut cpu);
        assert_eq!(cpu.regs[0], 12);
    }

    #[test]
    fn test_swi_cpu_set_fill() {
        let (mut cpu, mut bus) = make_cpu_bus();
        // Fill 4 words at EWRAM with 0xDEADBEEF
        bus.write32(0x0300_0000, 0xDEAD_BEEF);
        cpu.regs[0] = 0x0300_0000; // src
        cpu.regs[1] = 0x0200_0000; // dst (EWRAM)
        cpu.regs[2] = 4 | (1 << 24) | (1 << 26); // 4 words, fill, 32-bit
        swi_cpu_set(&mut cpu, &mut bus);

        assert_eq!(bus.read32(0x0200_0000), 0xDEAD_BEEF);
        assert_eq!(bus.read32(0x0200_0004), 0xDEAD_BEEF);
        assert_eq!(bus.read32(0x0200_0008), 0xDEAD_BEEF);
        assert_eq!(bus.read32(0x0200_000C), 0xDEAD_BEEF);
    }

    #[test]
    fn test_swi_lz77_decompress() {
        let (mut cpu, mut bus) = make_cpu_bus();
        // Create a simple LZ77 compressed stream in EWRAM
        // Header: type=0x10, size=8
        let src = 0x0200_0000u32;
        let dst = 0x0200_1000u32;

        // Header: 0x10 | (8 << 8) = 0x00000810
        bus.write32(src, 0x0000_0810);
        // Flag byte: 0x00 = 8 literal bytes
        bus.write8(src + 4, 0x00);
        // 8 literal bytes
        for i in 0..8u32 {
            bus.write8(src + 5 + i, (i + 1) as u8);
        }

        cpu.regs[0] = src;
        cpu.regs[1] = dst;
        swi_lz77_uncomp_wram(&mut cpu, &mut bus);

        for i in 0..8u32 {
            assert_eq!(bus.read8(dst + i), (i + 1) as u8);
        }
    }

    #[test]
    fn test_swi_rl_decompress() {
        let (mut cpu, mut bus) = make_cpu_bus();
        let src = 0x0200_0000u32;
        let dst = 0x0200_1000u32;

        // Header: 0x30 | (10 << 8) = RLE type, 10 bytes decompressed
        bus.write32(src, 0x0000_0A30);
        // Compressed run: repeat byte 0xAB 5 times (flag = 0x80 | (5-3) = 0x82)
        bus.write8(src + 4, 0x82);
        bus.write8(src + 5, 0xAB);
        // Uncompressed run: 5 literal bytes (flag = 5-1 = 0x04)
        bus.write8(src + 6, 0x04);
        for i in 0..5u32 {
            bus.write8(src + 7 + i, (0x10 + i) as u8);
        }

        cpu.regs[0] = src;
        cpu.regs[1] = dst;
        swi_rl_uncomp_wram(&mut cpu, &mut bus);

        for i in 0..5u32 {
            assert_eq!(bus.read8(dst + i), 0xAB);
        }
        for i in 0..5u32 {
            assert_eq!(bus.read8(dst + 5 + i), (0x10 + i) as u8);
        }
    }
}
