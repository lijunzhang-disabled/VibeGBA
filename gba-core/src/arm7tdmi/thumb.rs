//! THUMB mode instruction decoder and executor.
//!
//! THUMB is a 16-bit compressed instruction set. Instructions are decoded
//! primarily by bits [15:8], with 19 distinct instruction formats.

use super::Cpu;
use super::alu::{ShiftType, add_with_carry, barrel_shift, sub_with_carry};
use crate::bus::Bus;

impl Cpu {
    /// Execute a single THUMB instruction. Returns cycles consumed.
    pub fn execute_thumb(&mut self, bus: &mut Bus, opcode: u16) -> u32 {
        match opcode >> 8 {
            // Format 1: Move shifted register (LSL/LSR/ASR)
            0x00..=0x07 => self.thumb_shift_imm(opcode),       // LSL Rd, Rs, #Offset
            0x08..=0x0F => self.thumb_shift_imm(opcode),       // LSR Rd, Rs, #Offset
            0x10..=0x17 => self.thumb_shift_imm(opcode),       // ASR Rd, Rs, #Offset

            // Format 2: Add/subtract
            0x18..=0x19 => self.thumb_add_sub_reg(opcode),     // ADD Rd, Rs, Rn
            0x1A..=0x1B => self.thumb_add_sub_reg(opcode),     // SUB Rd, Rs, Rn
            0x1C..=0x1D => self.thumb_add_sub_imm(opcode),     // ADD Rd, Rs, #nn
            0x1E..=0x1F => self.thumb_add_sub_imm(opcode),     // SUB Rd, Rs, #nn

            // Format 3: Move/compare/add/subtract immediate
            0x20..=0x27 => self.thumb_mov_imm(opcode),         // MOV Rd, #nn
            0x28..=0x2F => self.thumb_cmp_imm(opcode),         // CMP Rd, #nn
            0x30..=0x37 => self.thumb_add_imm(opcode),         // ADD Rd, #nn
            0x38..=0x3F => self.thumb_sub_imm(opcode),         // SUB Rd, #nn

            // Format 4: ALU operations
            0x40..=0x43 => self.thumb_alu(bus, opcode),

            // Format 5: Hi register operations / BX
            0x44..=0x47 => self.thumb_hi_reg_bx(opcode),

            // Format 6: PC-relative load
            0x48..=0x4F => self.thumb_ldr_pc(bus, opcode),

            // Format 7/8: Load/store with register offset
            0x50..=0x5F => self.thumb_load_store_reg(bus, opcode),

            // Format 9: Load/store with immediate offset
            0x60..=0x7F => self.thumb_load_store_imm(bus, opcode),

            // Format 10: Load/store halfword
            0x80..=0x8F => self.thumb_load_store_half(bus, opcode),

            // Format 11: SP-relative load/store
            0x90..=0x9F => self.thumb_load_store_sp(bus, opcode),

            // Format 12: Load address (PC or SP + offset)
            0xA0..=0xAF => self.thumb_load_address(opcode),

            // Format 13: Add offset to SP
            0xB0 => self.thumb_add_sp(opcode),

            // Format 14: Push/pop registers
            0xB4..=0xB5 => self.thumb_push(bus, opcode),
            0xBC..=0xBD => self.thumb_pop(bus, opcode),

            // Format 15: Multiple load/store (STMIA/LDMIA)
            0xC0..=0xC7 => self.thumb_stmia(bus, opcode),
            0xC8..=0xCF => self.thumb_ldmia(bus, opcode),

            // Format 16: Conditional branch
            0xD0..=0xDD => self.thumb_cond_branch(opcode),

            // Format 17: SWI
            0xDF => self.thumb_swi(opcode),

            // Format 18: Unconditional branch
            0xE0..=0xE7 => self.thumb_branch(opcode),

            // Format 19: Long branch with link (BL)
            0xF0..=0xF7 => self.thumb_bl_prefix(opcode),  // BL high half
            0xF8..=0xFF => self.thumb_bl_suffix(bus, opcode),  // BL low half

            _ => {
                log::warn!("THUMB undefined: 0x{:04X} at PC=0x{:08X}",
                    opcode, self.regs[15].wrapping_sub(4));
                1
            }
        }
    }

    // ─── Format 1: Move shifted register ─────────────────────────

    fn thumb_shift_imm(&mut self, opcode: u16) -> u32 {
        let op = (opcode >> 11) & 3;
        let offset = ((opcode >> 6) & 0x1F) as u8;
        let rs = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let shift_type = match op {
            0 => ShiftType::Lsl,
            1 => ShiftType::Lsr,
            2 => ShiftType::Asr,
            _ => unreachable!(),
        };

        let (result, carry) = barrel_shift(self.reg(rs), shift_type, offset, self.cpsr.c(), true);
        self.regs[rd as usize] = result;
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);

        1
    }

    // ─── Format 2: Add/subtract ──────────────────────────────────

    fn thumb_add_sub_reg(&mut self, opcode: u16) -> u32 {
        let sub = opcode & (1 << 9) != 0;
        let rn = ((opcode >> 6) & 7) as u8;
        let rs = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let a = self.reg(rs);
        let b = self.reg(rn);

        let (result, carry, overflow) = if sub {
            sub_with_carry(a, b, true)
        } else {
            add_with_carry(a, b, false)
        };

        self.regs[rd as usize] = result;
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        self.cpsr.set_v(overflow);

        1
    }

    fn thumb_add_sub_imm(&mut self, opcode: u16) -> u32 {
        let sub = opcode & (1 << 9) != 0;
        let imm = ((opcode >> 6) & 7) as u32;
        let rs = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let a = self.reg(rs);

        let (result, carry, overflow) = if sub {
            sub_with_carry(a, imm, true)
        } else {
            add_with_carry(a, imm, false)
        };

        self.regs[rd as usize] = result;
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        self.cpsr.set_v(overflow);

        1
    }

    // ─── Format 3: Move/compare/add/subtract immediate ──────────

    fn thumb_mov_imm(&mut self, opcode: u16) -> u32 {
        let rd = ((opcode >> 8) & 7) as u8;
        let imm = (opcode & 0xFF) as u32;
        self.regs[rd as usize] = imm;
        self.cpsr.set_nz(imm);
        1
    }

    fn thumb_cmp_imm(&mut self, opcode: u16) -> u32 {
        let rd = ((opcode >> 8) & 7) as u8;
        let imm = (opcode & 0xFF) as u32;
        let (result, carry, overflow) = sub_with_carry(self.reg(rd), imm, true);
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        self.cpsr.set_v(overflow);
        1
    }

    fn thumb_add_imm(&mut self, opcode: u16) -> u32 {
        let rd = ((opcode >> 8) & 7) as u8;
        let imm = (opcode & 0xFF) as u32;
        let (result, carry, overflow) = add_with_carry(self.reg(rd), imm, false);
        self.regs[rd as usize] = result;
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        self.cpsr.set_v(overflow);
        1
    }

    fn thumb_sub_imm(&mut self, opcode: u16) -> u32 {
        let rd = ((opcode >> 8) & 7) as u8;
        let imm = (opcode & 0xFF) as u32;
        let (result, carry, overflow) = sub_with_carry(self.reg(rd), imm, true);
        self.regs[rd as usize] = result;
        self.cpsr.set_nz(result);
        self.cpsr.set_c(carry);
        self.cpsr.set_v(overflow);
        1
    }

    // ─── Format 4: ALU operations ────────────────────────────────

    fn thumb_alu(&mut self, _bus: &mut Bus, opcode: u16) -> u32 {
        let op = (opcode >> 6) & 0xF;
        let rs = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let a = self.reg(rd);
        let b = self.reg(rs);

        match op {
            0x0 => { // AND
                let result = a & b;
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
            }
            0x1 => { // EOR
                let result = a ^ b;
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
            }
            0x2 => { // LSL
                let (result, carry) = barrel_shift(a, ShiftType::Lsl, b as u8, self.cpsr.c(), false);
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
            }
            0x3 => { // LSR
                let (result, carry) = barrel_shift(a, ShiftType::Lsr, b as u8, self.cpsr.c(), false);
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
            }
            0x4 => { // ASR
                let (result, carry) = barrel_shift(a, ShiftType::Asr, b as u8, self.cpsr.c(), false);
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
            }
            0x5 => { // ADC
                let (result, carry, overflow) = add_with_carry(a, b, self.cpsr.c());
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
                self.cpsr.set_v(overflow);
            }
            0x6 => { // SBC
                let (result, carry, overflow) = sub_with_carry(a, b, self.cpsr.c());
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
                self.cpsr.set_v(overflow);
            }
            0x7 => { // ROR
                let (result, carry) = barrel_shift(a, ShiftType::Ror, b as u8, self.cpsr.c(), false);
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
            }
            0x8 => { // TST
                let result = a & b;
                self.cpsr.set_nz(result);
            }
            0x9 => { // NEG (0 - Rs)
                let (result, carry, overflow) = sub_with_carry(0, b, true);
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
                self.cpsr.set_v(overflow);
            }
            0xA => { // CMP
                let (result, carry, overflow) = sub_with_carry(a, b, true);
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
                self.cpsr.set_v(overflow);
            }
            0xB => { // CMN
                let (result, carry, overflow) = add_with_carry(a, b, false);
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
                self.cpsr.set_v(overflow);
            }
            0xC => { // ORR
                let result = a | b;
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
            }
            0xD => { // MUL
                let result = a.wrapping_mul(b);
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
            }
            0xE => { // BIC
                let result = a & !b;
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
            }
            0xF => { // MVN
                let result = !b;
                self.regs[rd as usize] = result;
                self.cpsr.set_nz(result);
            }
            _ => unreachable!(),
        }

        1
    }

    // ─── Format 5: Hi register operations / BX ───────────────────

    fn thumb_hi_reg_bx(&mut self, opcode: u16) -> u32 {
        let op = (opcode >> 8) & 3;
        let h1 = (opcode >> 7) & 1; // High bit for Rd
        let h2 = (opcode >> 6) & 1; // High bit for Rs
        let rs = (((h2 << 3) | ((opcode >> 3) & 7)) & 0xF) as u8;
        let rd = (((h1 << 3) | (opcode & 7)) & 0xF) as u8;

        match op {
            0 => { // ADD
                let result = self.reg(rd).wrapping_add(self.reg(rs));
                if rd == 15 {
                    self.branch(result & !1);
                } else {
                    self.regs[rd as usize] = result;
                }
            }
            1 => { // CMP
                let (result, carry, overflow) = sub_with_carry(self.reg(rd), self.reg(rs), true);
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
                self.cpsr.set_v(overflow);
            }
            2 => { // MOV
                let val = self.reg(rs);
                if rd == 15 {
                    self.branch(val & !1);
                } else {
                    self.regs[rd as usize] = val;
                }
            }
            3 => { // BX
                let addr = self.reg(rs);
                self.branch_exchange(addr);
            }
            _ => unreachable!(),
        }

        if (op == 0 || op == 2) && rd == 15 { 3 } else if op == 3 { 3 } else { 1 }
    }

    // ─── Format 6: PC-relative load ──────────────────────────────

    fn thumb_ldr_pc(&mut self, bus: &mut Bus, opcode: u16) -> u32 {
        let rd = ((opcode >> 8) & 7) as u8;
        let offset = ((opcode & 0xFF) as u32) << 2;
        // PC value for this instruction = executing_address + 4, word-aligned.
        // After pipeline advancement, self.regs[15] is already executing_address + 4.
        let addr = (self.regs[15] & !3).wrapping_add(offset);
        let val = bus.read32(addr & !3);
        self.regs[rd as usize] = val;

        3
    }

    // ─── Format 7/8: Load/store with register offset ─────────────

    fn thumb_load_store_reg(&mut self, bus: &mut Bus, opcode: u16) -> u32 {
        let op = (opcode >> 10) & 3;
        let ro = ((opcode >> 6) & 7) as u8;
        let rb = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let addr = self.reg(rb).wrapping_add(self.reg(ro));

        match (opcode >> 9) & 7 {
            0b000 => { // STR Rd, [Rb, Ro]
                bus.write32(addr & !3, self.reg(rd));
            }
            0b001 => { // STRH Rd, [Rb, Ro]
                bus.write16(addr & !1, self.reg(rd) as u16);
            }
            0b010 => { // STRB Rd, [Rb, Ro]
                bus.write8(addr, self.reg(rd) as u8);
            }
            0b011 => { // LDRSB Rd, [Rb, Ro]
                let val = bus.read8(addr) as i8 as i32 as u32;
                self.regs[rd as usize] = val;
            }
            0b100 => { // LDR Rd, [Rb, Ro]
                let val = bus.read32(addr & !3);
                let rotation = (addr & 3) * 8;
                self.regs[rd as usize] = val.rotate_right(rotation);
            }
            0b101 => { // LDRH Rd, [Rb, Ro]
                let val = bus.read16(addr & !1) as u32;
                self.regs[rd as usize] = if addr & 1 != 0 { val.rotate_right(8) } else { val };
            }
            0b110 => { // LDRB Rd, [Rb, Ro]
                self.regs[rd as usize] = bus.read8(addr) as u32;
            }
            0b111 => { // LDRSH Rd, [Rb, Ro]
                if addr & 1 != 0 {
                    self.regs[rd as usize] = bus.read8(addr) as i8 as i32 as u32;
                } else {
                    self.regs[rd as usize] = bus.read16(addr) as i16 as i32 as u32;
                }
            }
            _ => unreachable!(),
        }

        let _ = op;
        2
    }

    // ─── Format 9: Load/store with immediate offset ──────────────

    fn thumb_load_store_imm(&mut self, bus: &mut Bus, opcode: u16) -> u32 {
        let b = opcode & (1 << 12) != 0;  // Byte (1) or Word (0)
        let l = opcode & (1 << 11) != 0;  // Load (1) or Store (0)
        let offset = ((opcode >> 6) & 0x1F) as u32;
        let rb = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let base = self.reg(rb);
        let addr = if b {
            base.wrapping_add(offset) // Byte: offset is direct
        } else {
            base.wrapping_add(offset << 2) // Word: offset is in words (x4)
        };

        if l {
            if b {
                self.regs[rd as usize] = bus.read8(addr) as u32;
            } else {
                let val = bus.read32(addr & !3);
                let rotation = (addr & 3) * 8;
                self.regs[rd as usize] = val.rotate_right(rotation);
            }
        } else {
            if b {
                bus.write8(addr, self.reg(rd) as u8);
            } else {
                bus.write32(addr & !3, self.reg(rd));
            }
        }

        2
    }

    // ─── Format 10: Load/store halfword ──────────────────────────

    fn thumb_load_store_half(&mut self, bus: &mut Bus, opcode: u16) -> u32 {
        let l = opcode & (1 << 11) != 0;
        let offset = (((opcode >> 6) & 0x1F) as u32) << 1; // Halfword offset (x2)
        let rb = ((opcode >> 3) & 7) as u8;
        let rd = (opcode & 7) as u8;

        let addr = self.reg(rb).wrapping_add(offset);

        if l {
            // Format 10 LDRH: offset is always even, so a misaligned address
            // can only come from an odd base register. Pokemon's M4A sound
            // driver triggers a code path where adding the fix here causes a
            // downstream crash we haven't root-caused yet. For now, keep the
            // aligned-only behavior here; the rotation fix is applied in
            // format 7/8 (register-offset) LDRH which covers the common case.
            // TODO(phase9): investigate the format 10 interaction. See
            // debug/2026-04-24_pokemon-emerald-noisy-audio.md follow-ups.
            self.regs[rd as usize] = bus.read16(addr & !1) as u32;
        } else {
            bus.write16(addr & !1, self.reg(rd) as u16);
        }

        2
    }

    // ─── Format 11: SP-relative load/store ───────────────────────

    fn thumb_load_store_sp(&mut self, bus: &mut Bus, opcode: u16) -> u32 {
        let l = opcode & (1 << 11) != 0;
        let rd = ((opcode >> 8) & 7) as u8;
        let offset = ((opcode & 0xFF) as u32) << 2;
        let addr = self.regs[13].wrapping_add(offset);

        if l {
            let val = bus.read32(addr & !3);
            let rotation = (addr & 3) * 8;
            self.regs[rd as usize] = val.rotate_right(rotation);
        } else {
            bus.write32(addr & !3, self.reg(rd));
        }

        2
    }

    // ─── Format 12: Load address ─────────────────────────────────

    fn thumb_load_address(&mut self, opcode: u16) -> u32 {
        let sp = opcode & (1 << 11) != 0;
        let rd = ((opcode >> 8) & 7) as u8;
        let offset = ((opcode & 0xFF) as u32) << 2;

        if sp {
            self.regs[rd as usize] = self.regs[13].wrapping_add(offset);
        } else {
            // PC-relative: PC is word-aligned
            let pc = self.regs[15] & !2;
            self.regs[rd as usize] = pc.wrapping_add(offset);
        }

        1
    }

    // ─── Format 13: Add offset to SP ─────────────────────────────

    fn thumb_add_sp(&mut self, opcode: u16) -> u32 {
        let negative = opcode & (1 << 7) != 0;
        let offset = ((opcode & 0x7F) as u32) << 2;

        if negative {
            self.regs[13] = self.regs[13].wrapping_sub(offset);
        } else {
            self.regs[13] = self.regs[13].wrapping_add(offset);
        }

        1
    }

    // ─── Format 14: Push/Pop ─────────────────────────────────────

    fn thumb_push(&mut self, bus: &mut Bus, opcode: u16) -> u32 {
        let lr = opcode & (1 << 8) != 0;
        let rlist = opcode & 0xFF;
        let reg_count = rlist.count_ones() + lr as u32;

        let mut addr = self.regs[13].wrapping_sub(reg_count * 4);
        self.regs[13] = addr;

        for i in 0..8u8 {
            if rlist & (1 << i) != 0 {
                bus.write32(addr, self.reg(i));
                addr = addr.wrapping_add(4);
            }
        }
        if lr {
            bus.write32(addr, self.regs[14]);
        }

        reg_count + 1
    }

    fn thumb_pop(&mut self, bus: &mut Bus, opcode: u16) -> u32 {
        let pc = opcode & (1 << 8) != 0;
        let rlist = opcode & 0xFF;

        let mut addr = self.regs[13];

        for i in 0..8u8 {
            if rlist & (1 << i) != 0 {
                self.regs[i as usize] = bus.read32(addr);
                addr = addr.wrapping_add(4);
            }
        }
        if pc {
            let val = bus.read32(addr);
            addr = addr.wrapping_add(4);
            // ARM7TDMI: BX behavior based on bit 0
            self.branch_exchange(val);
        }

        self.regs[13] = addr;

        let reg_count = rlist.count_ones() + pc as u32;
        reg_count + 2
    }

    // ─── Format 15: Multiple load/store (STMIA/LDMIA) ───────────

    fn thumb_stmia(&mut self, bus: &mut Bus, opcode: u16) -> u32 {
        let rb = ((opcode >> 8) & 7) as u8;
        let rlist = opcode & 0xFF;
        let mut addr = self.reg(rb);

        for i in 0..8u8 {
            if rlist & (1 << i) != 0 {
                bus.write32(addr, self.reg(i));
                addr = addr.wrapping_add(4);
            }
        }

        self.regs[rb as usize] = addr;

        rlist.count_ones() + 1
    }

    fn thumb_ldmia(&mut self, bus: &mut Bus, opcode: u16) -> u32 {
        let rb = ((opcode >> 8) & 7) as u8;
        let rlist = opcode & 0xFF;
        let mut addr = self.reg(rb);

        for i in 0..8u8 {
            if rlist & (1 << i) != 0 {
                self.regs[i as usize] = bus.read32(addr);
                addr = addr.wrapping_add(4);
            }
        }

        // Write-back only if Rb is not in the register list
        if rlist & (1 << rb) == 0 {
            self.regs[rb as usize] = addr;
        }

        rlist.count_ones() + 2
    }

    // ─── Format 16: Conditional branch ───────────────────────────

    fn thumb_cond_branch(&mut self, opcode: u16) -> u32 {
        let cond = (opcode >> 8) & 0xF;

        if !self.check_condition(cond as u32) {
            return 1;
        }

        // 8-bit signed offset, shifted left by 1
        let offset = ((opcode & 0xFF) as i8 as i32) << 1;
        let target = (self.regs[15] as i32).wrapping_add(offset) as u32;
        self.branch(target);

        3
    }

    // ─── Format 17: SWI ──────────────────────────────────────────

    fn thumb_swi(&mut self, opcode: u16) -> u32 {
        let comment = (opcode & 0xFF) as u8;
        self.pending_swi = Some(comment);
        3
    }

    // ─── Format 18: Unconditional branch ─────────────────────────

    fn thumb_branch(&mut self, opcode: u16) -> u32 {
        // 11-bit signed offset, shifted left by 1
        let offset = (((opcode & 0x7FF) as i32) << 21) >> 20; // Sign-extend 11 bits, shift left 1
        let target = (self.regs[15] as i32).wrapping_add(offset) as u32;
        self.branch(target);

        3
    }

    // ─── Format 19: Long branch with link (two-instruction) ─────

    fn thumb_bl_prefix(&mut self, opcode: u16) -> u32 {
        // First instruction: LR = PC + (offset_high << 12)
        let offset = (((opcode & 0x7FF) as i32) << 21) >> 9; // Sign-extend, shift left 12
        self.regs[14] = (self.regs[15] as i32).wrapping_add(offset) as u32;
        1
    }

    fn thumb_bl_suffix(&mut self, _bus: &mut Bus, opcode: u16) -> u32 {
        // Second instruction: temp = next_instr_addr; PC = LR + (offset_low << 1); LR = temp | 1
        let offset = ((opcode & 0x7FF) as u32) << 1;
        let next_instr = self.regs[15].wrapping_sub(2);
        let target = self.regs[14].wrapping_add(offset);
        self.regs[14] = next_instr | 1;
        self.branch(target);
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cpu_bus() -> (Cpu, Bus) {
        let mut cpu = Cpu::new_skip_bios();
        cpu.cpsr.set_thumb(true);
        let bus = Bus::new(None, vec![0; 256]);
        (cpu, bus)
    }

    #[test]
    fn test_thumb_mov_imm() {
        let (mut cpu, mut bus) = make_cpu_bus();
        // MOV R0, #42
        cpu.execute_thumb(&mut bus, 0x202A);
        assert_eq!(cpu.regs[0], 42);
    }

    #[test]
    fn test_thumb_add_imm() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[0] = 10;
        // ADD R0, #5
        cpu.execute_thumb(&mut bus, 0x3005);
        assert_eq!(cpu.regs[0], 15);
    }

    #[test]
    fn test_thumb_sub_imm() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[0] = 10;
        // SUB R0, #5
        cpu.execute_thumb(&mut bus, 0x3805);
        assert_eq!(cpu.regs[0], 5);
    }

    #[test]
    fn test_thumb_cmp_sets_flags() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[0] = 42;
        // CMP R0, #42
        cpu.execute_thumb(&mut bus, 0x282A);
        assert!(cpu.cpsr.z());
    }

    #[test]
    fn test_thumb_lsl() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[1] = 1;
        // LSL R0, R1, #4
        cpu.execute_thumb(&mut bus, 0x0108);
        assert_eq!(cpu.regs[0], 16);
    }

    #[test]
    fn test_thumb_push_pop() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[0] = 0xAAAA;
        cpu.regs[1] = 0xBBBB;
        cpu.regs[13] = 0x0300_0100; // SP in IWRAM

        // PUSH {R0, R1}
        cpu.execute_thumb(&mut bus, 0xB403);
        assert_eq!(cpu.regs[13], 0x0300_00F8);

        cpu.regs[0] = 0;
        cpu.regs[1] = 0;

        // POP {R0, R1}
        cpu.execute_thumb(&mut bus, 0xBC03);
        assert_eq!(cpu.regs[0], 0xAAAA);
        assert_eq!(cpu.regs[1], 0xBBBB);
        assert_eq!(cpu.regs[13], 0x0300_0100);
    }
}
