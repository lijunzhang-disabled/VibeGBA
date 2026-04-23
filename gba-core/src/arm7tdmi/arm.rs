//! ARM mode instruction decoder and executor.
//!
//! ARM7TDMI 32-bit instruction set. Instructions are conditionally executed
//! based on bits [31:28]. The format is identified by bits [27:20] and [7:4].

use super::Cpu;
use super::alu::{AluOp, ShiftType, add_with_carry, barrel_shift, sub_with_carry};
use crate::bus::Bus;

impl Cpu {
    /// Execute a single ARM instruction. Returns cycles consumed.
    pub fn execute_arm(&mut self, bus: &mut Bus, opcode: u32) -> u32 {
        // Dispatch based on instruction format.
        // We use the bit-pattern approach rather than a full LUT for clarity.
        // This can be converted to a LUT later for performance.

        let bits_27_20 = (opcode >> 20) & 0xFF;
        let bits_7_4 = (opcode >> 4) & 0xF;

        match bits_27_20 >> 5 {
            0b000 => {
                if bits_27_20 == 0x12 && bits_7_4 == 0x1 {
                    // BX (Branch and Exchange)
                    self.arm_branch_exchange(opcode)
                } else if (bits_7_4 & 0x9) == 0x9 && (bits_27_20 & 0xE0) == 0 {
                    // Multiply / multiply-long / SWP / halfword transfer
                    match bits_7_4 {
                        0x9 => {
                            if bits_27_20 & 0xFC == 0x00 {
                                // MUL / MLA
                                self.arm_multiply(bus, opcode)
                            } else if bits_27_20 & 0xF8 == 0x08 {
                                // UMULL / UMLAL / SMULL / SMLAL
                                self.arm_multiply_long(bus, opcode)
                            } else if bits_27_20 & 0xFB == 0x10 {
                                // SWP / SWPB
                                self.arm_swap(bus, opcode)
                            } else {
                                self.arm_undefined(opcode)
                            }
                        }
                        0xB | 0xD | 0xF => {
                            // Halfword / signed data transfer
                            self.arm_halfword_transfer(bus, opcode)
                        }
                        _ => {
                            // Data processing with register shift
                            self.arm_data_processing(bus, opcode)
                        }
                    }
                } else {
                    // Data processing / PSR transfer
                    // MRS: bits[27:20] = 0001 0P00 (bit 21 = 0)
                    // MSR: bits[27:20] = 0001 0P10 (bit 21 = 1)
                    // Distinguish via bit 21: mask 0xFB includes bit 21.
                    if (bits_27_20 & 0xFB) == 0x10 && bits_7_4 == 0x0 {
                        // MRS
                        self.arm_mrs(opcode)
                    } else if (bits_27_20 & 0xFB) == 0x12 && bits_7_4 == 0x0 {
                        // MSR (register)
                        self.arm_msr(opcode)
                    } else {
                        self.arm_data_processing(bus, opcode)
                    }
                }
            }
            0b001 => {
                // Data processing immediate / MSR immediate
                if (bits_27_20 & 0xFB) == 0x32 {
                    // MSR immediate
                    self.arm_msr(opcode)
                } else {
                    self.arm_data_processing(bus, opcode)
                }
            }
            0b010 => {
                // Single data transfer (immediate offset)
                self.arm_single_transfer(bus, opcode)
            }
            0b011 => {
                if opcode & (1 << 4) != 0 {
                    // Undefined instruction
                    self.arm_undefined(opcode)
                } else {
                    // Single data transfer (register offset)
                    self.arm_single_transfer(bus, opcode)
                }
            }
            0b100 => {
                // Block data transfer (LDM/STM)
                self.arm_block_transfer(bus, opcode)
            }
            0b101 => {
                // Branch / Branch with Link
                self.arm_branch(opcode)
            }
            0b111 => {
                if opcode >> 24 & 0xF == 0xF {
                    // SWI
                    self.arm_swi(opcode)
                } else {
                    // Coprocessor (unused on GBA)
                    self.arm_undefined(opcode)
                }
            }
            _ => self.arm_undefined(opcode),
        }
    }

    // ─── Data Processing ──────────────────────────────────────────

    fn arm_data_processing(&mut self, _bus: &mut Bus, opcode: u32) -> u32 {
        let i = opcode & (1 << 25) != 0; // Immediate operand
        let s = opcode & (1 << 20) != 0; // Set condition codes
        let op = AluOp::from_u8(((opcode >> 21) & 0xF) as u8);
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rd = ((opcode >> 12) & 0xF) as u8;

        let op1 = self.reg(rn);

        // Compute operand 2 and shifter carry out
        let (op2, shifter_carry) = if i {
            // Immediate: 8-bit value rotated right by 2*rotate
            let imm = opcode & 0xFF;
            let rotate = ((opcode >> 8) & 0xF) * 2;
            if rotate == 0 {
                (imm, self.cpsr.c())
            } else {
                let result = imm.rotate_right(rotate);
                (result, result >> 31 != 0)
            }
        } else {
            // Register: Rm shifted by amount
            let rm = (opcode & 0xF) as u8;
            let shift_type = ShiftType::from_u8(((opcode >> 5) & 3) as u8);

            let (shift_amount, extra_cycle) = if opcode & (1 << 4) != 0 {
                // Shift by register
                let rs = ((opcode >> 8) & 0xF) as u8;
                (self.reg(rs) as u8, true)
            } else {
                // Shift by immediate
                let amount = ((opcode >> 7) & 0x1F) as u8;
                (amount, false)
            };

            let rm_val = if rm == 15 && opcode & (1 << 4) != 0 {
                self.reg(15).wrapping_add(4) // Extra +4 when shift by register and Rm=PC
            } else {
                self.reg(rm)
            };

            let immediate_shift = opcode & (1 << 4) == 0;
            let _ = extra_cycle; // TODO: add extra cycle for register shift
            barrel_shift(rm_val, shift_type, shift_amount, self.cpsr.c(), immediate_shift)
        };

        // Execute the ALU operation
        let (result, carry, overflow) = match op {
            AluOp::And | AluOp::Tst => (op1 & op2, shifter_carry, self.cpsr.v()),
            AluOp::Eor | AluOp::Teq => (op1 ^ op2, shifter_carry, self.cpsr.v()),
            AluOp::Sub | AluOp::Cmp => sub_with_carry(op1, op2, true),
            AluOp::Rsb => sub_with_carry(op2, op1, true),
            AluOp::Add | AluOp::Cmn => add_with_carry(op1, op2, false),
            AluOp::Adc => add_with_carry(op1, op2, self.cpsr.c()),
            AluOp::Sbc => sub_with_carry(op1, op2, self.cpsr.c()),
            AluOp::Rsc => sub_with_carry(op2, op1, self.cpsr.c()),
            AluOp::Orr => (op1 | op2, shifter_carry, self.cpsr.v()),
            AluOp::Mov => (op2, shifter_carry, self.cpsr.v()),
            AluOp::Bic => (op1 & !op2, shifter_carry, self.cpsr.v()),
            AluOp::Mvn => (!op2, shifter_carry, self.cpsr.v()),
        };

        if s {
            if rd == 15 {
                // Rd=R15 with S: restore CPSR from SPSR
                self.set_reg_with_flags(rd, result, true);
            } else {
                self.cpsr.set_nz(result);
                self.cpsr.set_c(carry);
                if !op.is_logical() {
                    self.cpsr.set_v(overflow);
                }
                if !op.is_test() {
                    self.regs[rd as usize] = result;
                }
            }
        } else if !op.is_test() {
            self.set_reg(rd, result);
        }

        1 // Base: 1S cycle
    }

    // ─── Multiply ─────────────────────────────────────────────────

    fn arm_multiply(&mut self, _bus: &mut Bus, opcode: u32) -> u32 {
        let a = opcode & (1 << 21) != 0; // Accumulate
        let s = opcode & (1 << 20) != 0; // Set flags
        let rd = ((opcode >> 16) & 0xF) as u8;
        let rn = ((opcode >> 12) & 0xF) as u8;
        let rs = ((opcode >> 8) & 0xF) as u8;
        let rm = (opcode & 0xF) as u8;

        let result = if a {
            self.reg(rm).wrapping_mul(self.reg(rs)).wrapping_add(self.reg(rn))
        } else {
            self.reg(rm).wrapping_mul(self.reg(rs))
        };

        self.regs[rd as usize] = result;

        if s {
            self.cpsr.set_nz(result);
            // C is destroyed (unpredictable), V is unaffected
        }

        // Cycles depend on Rs value (1S + mI where m is multiplier cycles)
        // Simplified: 2-5 cycles
        4
    }

    fn arm_multiply_long(&mut self, _bus: &mut Bus, opcode: u32) -> u32 {
        let u = opcode & (1 << 22) != 0; // Unsigned (0) / Signed (1)
        let a = opcode & (1 << 21) != 0; // Accumulate
        let s = opcode & (1 << 20) != 0; // Set flags
        let rd_hi = ((opcode >> 16) & 0xF) as u8;
        let rd_lo = ((opcode >> 12) & 0xF) as u8;
        let rs = ((opcode >> 8) & 0xF) as u8;
        let rm = (opcode & 0xF) as u8;

        let result = if u {
            // Signed
            let result = (self.reg(rm) as i32 as i64) * (self.reg(rs) as i32 as i64);
            if a {
                let acc = ((self.reg(rd_hi) as u64) << 32) | self.reg(rd_lo) as u64;
                (result as u64).wrapping_add(acc)
            } else {
                result as u64
            }
        } else {
            // Unsigned
            let result = (self.reg(rm) as u64) * (self.reg(rs) as u64);
            if a {
                let acc = ((self.reg(rd_hi) as u64) << 32) | self.reg(rd_lo) as u64;
                result.wrapping_add(acc)
            } else {
                result
            }
        };

        self.regs[rd_lo as usize] = result as u32;
        self.regs[rd_hi as usize] = (result >> 32) as u32;

        if s {
            self.cpsr.set_n((result >> 63) != 0);
            self.cpsr.set_z(result == 0);
        }

        5 // Simplified cycle count
    }

    // ─── Single Data Transfer (LDR/STR) ──────────────────────────

    fn arm_single_transfer(&mut self, bus: &mut Bus, opcode: u32) -> u32 {
        let i = opcode & (1 << 25) != 0; // Immediate=0, Register=1 (inverted from data proc!)
        let p = opcode & (1 << 24) != 0; // Pre-indexing
        let u = opcode & (1 << 23) != 0; // Up (add offset)
        let b = opcode & (1 << 22) != 0; // Byte transfer
        let w = opcode & (1 << 21) != 0; // Write-back
        let l = opcode & (1 << 20) != 0; // Load
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rd = ((opcode >> 12) & 0xF) as u8;

        let base = self.reg(rn);

        // Calculate offset
        let offset = if !i {
            // Immediate offset (12-bit)
            opcode & 0xFFF
        } else {
            // Register offset with shift
            let rm = (opcode & 0xF) as u8;
            let shift_type = ShiftType::from_u8(((opcode >> 5) & 3) as u8);
            let shift_amount = ((opcode >> 7) & 0x1F) as u8;
            let (shifted, _) = barrel_shift(self.reg(rm), shift_type, shift_amount, self.cpsr.c(), true);
            shifted
        };

        let offset_addr = if u {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };

        let addr = if p { offset_addr } else { base };

        let mut cycles = 1;

        if l {
            // Load
            let val = if b {
                bus.read8(addr) as u32
            } else {
                // Word load: misaligned addresses rotate the result
                let aligned = addr & !3;
                let val = bus.read32(aligned);
                let rotation = (addr & 3) * 8;
                val.rotate_right(rotation)
            };
            self.set_reg(rd, val);
            cycles += 1; // 1N + 1S + 1I for LDR
        } else {
            // Store
            let val = if rd == 15 {
                self.reg(15).wrapping_add(4) // PC+12 for STR
            } else {
                self.reg(rd)
            };
            if b {
                bus.write8(addr, val as u8);
            } else {
                bus.write32(addr & !3, val);
            }
        }

        // Write-back: update Rn with offset address
        if !p || w {
            if rn != 15 {
                self.regs[rn as usize] = offset_addr;
            }
        }

        cycles
    }

    // ─── Halfword / Signed Data Transfer ─────────────────────────

    fn arm_halfword_transfer(&mut self, bus: &mut Bus, opcode: u32) -> u32 {
        let p = opcode & (1 << 24) != 0;
        let u = opcode & (1 << 23) != 0;
        let i = opcode & (1 << 22) != 0; // Immediate offset (1) / Register offset (0)
        let w = opcode & (1 << 21) != 0;
        let l = opcode & (1 << 20) != 0;
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rd = ((opcode >> 12) & 0xF) as u8;
        let sh = (opcode >> 5) & 3; // SH bits: 01=H, 10=SB, 11=SH

        let base = self.reg(rn);

        let offset = if i {
            // Immediate: high nibble | low nibble
            ((opcode >> 4) & 0xF0) | (opcode & 0xF)
        } else {
            // Register
            let rm = (opcode & 0xF) as u8;
            self.reg(rm)
        };

        let offset_addr = if u {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };

        let addr = if p { offset_addr } else { base };

        if l {
            let val = match sh {
                0x1 => {
                    // LDRH: unsigned halfword
                    bus.read16(addr & !1) as u32
                }
                0x2 => {
                    // LDRSB: signed byte
                    bus.read8(addr) as i8 as i32 as u32
                }
                0x3 => {
                    // LDRSH: signed halfword
                    if addr & 1 != 0 {
                        // Misaligned LDRSH: reads byte and sign-extends
                        bus.read8(addr) as i8 as i32 as u32
                    } else {
                        bus.read16(addr) as i16 as i32 as u32
                    }
                }
                _ => 0,
            };
            self.set_reg(rd, val);
        } else {
            // STRH
            let val = self.reg(rd);
            bus.write16(addr & !1, val as u16);
        }

        if !p || w {
            if rn != 15 {
                self.regs[rn as usize] = offset_addr;
            }
        }

        if l { 3 } else { 2 }
    }

    // ─── Block Data Transfer (LDM/STM) ───────────────────────────

    fn arm_block_transfer(&mut self, bus: &mut Bus, opcode: u32) -> u32 {
        let p = opcode & (1 << 24) != 0; // Pre-increment
        let u = opcode & (1 << 23) != 0; // Up (ascending)
        let s = opcode & (1 << 22) != 0; // PSR & force user bank
        let w = opcode & (1 << 21) != 0; // Write-back
        let l = opcode & (1 << 20) != 0; // Load
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rlist = (opcode & 0xFFFF) as u16;

        let base = self.reg(rn);
        let reg_count = rlist.count_ones();

        if rlist == 0 {
            // Empty register list: ARM7TDMI quirk
            // Transfers R15 and adds/subtracts 0x40
            if l {
                let val = bus.read32(base);
                self.branch(val);
            } else {
                bus.write32(base, self.reg(15).wrapping_add(4));
            }
            if w {
                self.regs[rn as usize] = if u {
                    base.wrapping_add(0x40)
                } else {
                    base.wrapping_sub(0x40)
                };
            }
            return 3;
        }

        // Calculate start address
        let mut addr = if u {
            if p { base.wrapping_add(4) } else { base }
        } else {
            // Descending: start from lowest address
            let total = reg_count * 4;
            if p {
                base.wrapping_sub(total)
            } else {
                base.wrapping_sub(total).wrapping_add(4)
            }
        };

        let final_addr = if u {
            base.wrapping_add(reg_count * 4)
        } else {
            base.wrapping_sub(reg_count * 4)
        };

        // Transfer registers in order R0-R15
        for i in 0..16u8 {
            if rlist & (1 << i) == 0 {
                continue;
            }

            if l {
                let val = bus.read32(addr & !3);
                if s && (rlist & (1 << 15)) != 0 {
                    // LDM with S bit and R15 in list: restore CPSR from SPSR
                    if i == 15 {
                        let spsr = self.spsr();
                        let new_mode = spsr.mode();
                        self.switch_mode(new_mode);
                        self.cpsr = spsr;
                        self.branch(val & !1);
                    } else {
                        self.regs[i as usize] = val;
                    }
                } else if i == 15 {
                    self.branch(val & !1);
                } else {
                    // S bit without R15: access user-mode registers
                    // (simplified: just use current mode registers for now)
                    self.regs[i as usize] = val;
                }
            } else {
                let val = if i == 15 {
                    self.reg(15).wrapping_add(4)
                } else {
                    self.reg(i)
                };
                bus.write32(addr & !3, val);
            }

            addr = addr.wrapping_add(4);
        }

        if w {
            self.regs[rn as usize] = final_addr;
        }

        if l { reg_count + 2 } else { reg_count + 1 }
    }

    // ─── Branch / Branch with Link ───────────────────────────────

    fn arm_branch(&mut self, opcode: u32) -> u32 {
        let link = opcode & (1 << 24) != 0;
        // 24-bit offset, sign-extended and shifted left 2
        let offset = ((opcode & 0x00FF_FFFF) as i32) << 8 >> 6; // Sign-extend and shift

        if link {
            // BL: save return address in LR
            self.regs[14] = self.regs[15].wrapping_sub(4);
        }

        let target = (self.regs[15] as i32).wrapping_add(offset) as u32;
        self.branch(target);

        3 // 2S + 1N
    }

    // ─── Branch and Exchange (BX) ────────────────────────────────

    fn arm_branch_exchange(&mut self, opcode: u32) -> u32 {
        let rm = (opcode & 0xF) as u8;
        let addr = self.reg(rm);
        self.branch_exchange(addr);
        3
    }

    // ─── SWP / SWPB ──────────────────────────────────────────────

    fn arm_swap(&mut self, bus: &mut Bus, opcode: u32) -> u32 {
        let b = opcode & (1 << 22) != 0;
        let rn = ((opcode >> 16) & 0xF) as u8;
        let rd = ((opcode >> 12) & 0xF) as u8;
        let rm = (opcode & 0xF) as u8;

        let addr = self.reg(rn);

        if b {
            let old = bus.read8(addr) as u32;
            bus.write8(addr, self.reg(rm) as u8);
            self.regs[rd as usize] = old;
        } else {
            let aligned = addr & !3;
            let old = bus.read32(aligned);
            let rotation = (addr & 3) * 8;
            let old_rotated = old.rotate_right(rotation);
            bus.write32(aligned, self.reg(rm));
            self.regs[rd as usize] = old_rotated;
        }

        4 // 1S + 2N + 1I
    }

    // ─── MRS (PSR → Register) ────────────────────────────────────

    fn arm_mrs(&mut self, opcode: u32) -> u32 {
        let spsr = opcode & (1 << 22) != 0;
        let rd = ((opcode >> 12) & 0xF) as u8;

        let psr = if spsr { self.spsr() } else { self.cpsr };
        self.regs[rd as usize] = psr.bits;

        1
    }

    // ─── MSR (Register/Immediate → PSR) ──────────────────────────

    fn arm_msr(&mut self, opcode: u32) -> u32 {
        let i = opcode & (1 << 25) != 0;
        let spsr = opcode & (1 << 22) != 0;

        // Field mask: which parts of PSR to update
        let field_mask = (opcode >> 16) & 0xF;
        let mut mask = 0u32;
        if field_mask & 1 != 0 { mask |= 0x0000_00FF; } // Control (mode, flags)
        if field_mask & 2 != 0 { mask |= 0x0000_FF00; } // Extension
        if field_mask & 4 != 0 { mask |= 0x00FF_0000; } // Status
        if field_mask & 8 != 0 { mask |= 0xFF00_0000; } // Flags (N,Z,C,V)

        // In User mode, can only write flag bits
        if self.cpsr.mode() == super::CpuMode::User {
            mask &= 0xFF00_0000;
        }

        let val = if i {
            let imm = opcode & 0xFF;
            let rotate = ((opcode >> 8) & 0xF) * 2;
            imm.rotate_right(rotate)
        } else {
            let rm = (opcode & 0xF) as u8;
            self.reg(rm)
        };

        if spsr {
            let mut psr = self.spsr();
            psr.bits = (psr.bits & !mask) | (val & mask);
            self.set_spsr(psr);
        } else {
            let old_mode = self.cpsr.mode();
            self.cpsr.bits = (self.cpsr.bits & !mask) | (val & mask);
            let new_mode = self.cpsr.mode();
            if old_mode != new_mode {
                self.switch_mode(new_mode);
            }
        }

        1
    }

    // ─── Software Interrupt ──────────────────────────────────────

    fn arm_swi(&mut self, opcode: u32) -> u32 {
        let comment = (opcode >> 16) & 0xFF; // ARM SWI: function number in bits 23:16
        self.pending_swi = Some(comment as u8);
        3
    }

    // ─── Undefined Instruction ───────────────────────────────────

    fn arm_undefined(&mut self, _opcode: u32) -> u32 {
        log::warn!("ARM undefined instruction: 0x{:08X} at PC=0x{:08X}",
            _opcode, self.regs[15].wrapping_sub(8));
        // TODO: trigger undefined instruction exception
        1
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
    fn test_arm_mov_immediate() {
        let (mut cpu, mut bus) = make_cpu_bus();
        // MOV R0, #42 (condition AL)
        // 1110 00 1 1101 0 0000 0000 0000 00101010
        let opcode: u32 = 0xE3A0_002A;
        cpu.execute_arm(&mut bus, opcode);
        assert_eq!(cpu.regs[0], 42);
    }

    #[test]
    fn test_arm_add() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[1] = 10;
        cpu.regs[2] = 20;
        // ADD R0, R1, R2 (AL condition)
        let opcode: u32 = 0xE081_0002;
        cpu.execute_arm(&mut bus, opcode);
        assert_eq!(cpu.regs[0], 30);
    }

    #[test]
    fn test_arm_sub_with_flags() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[1] = 5;
        cpu.regs[2] = 5;
        // SUBS R0, R1, R2
        let opcode: u32 = 0xE051_0002;
        cpu.execute_arm(&mut bus, opcode);
        assert_eq!(cpu.regs[0], 0);
        assert!(cpu.cpsr.z());
        assert!(cpu.cpsr.c()); // No borrow
    }

    #[test]
    fn test_arm_cmp() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[0] = 10;
        // CMP R0, #10 (AL)
        let opcode: u32 = 0xE350_000A;
        cpu.execute_arm(&mut bus, opcode);
        assert!(cpu.cpsr.z());
    }

    #[test]
    fn test_arm_branch() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[15] = 0x0800_0008; // Pipeline: executing at 0x08000000
        cpu.pipeline_flushed = false;
        // B +0x100 (offset = 0x40 words = 0x100 bytes, but encoding is offset/4-2)
        // Actually: B with offset 0x00_003E (which is +0xF8 bytes from PC)
        // PC is at executing+8 = 0x08000008
        // Target = PC + offset*4 = 0x08000008 + 0xF8 = 0x08000100
        let opcode: u32 = 0xEA00_003E;
        cpu.execute_arm(&mut bus, opcode);
        assert_eq!(cpu.regs[15], 0x0800_0100);
    }

    #[test]
    fn test_arm_str_ldr() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[0] = 0xDEAD_BEEF;
        cpu.regs[1] = 0x0200_0000; // EWRAM
        // STR R0, [R1]
        let opcode_str: u32 = 0xE581_0000;
        cpu.execute_arm(&mut bus, opcode_str);
        // LDR R2, [R1]
        let opcode_ldr: u32 = 0xE591_2000;
        cpu.execute_arm(&mut bus, opcode_ldr);
        assert_eq!(cpu.regs[2], 0xDEAD_BEEF);
    }

    #[test]
    fn test_arm_multiply() {
        let (mut cpu, mut bus) = make_cpu_bus();
        cpu.regs[0] = 7;
        cpu.regs[1] = 6;
        // MUL R2, R0, R1
        let opcode: u32 = 0xE002_0190;
        cpu.execute_arm(&mut bus, opcode);
        assert_eq!(cpu.regs[2], 42);
    }
}
