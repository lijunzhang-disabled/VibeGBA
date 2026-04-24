pub mod alu;
pub mod arm;
pub mod thumb;
pub mod disasm;

use crate::bus::Bus;
use serde::{Deserialize, Serialize};

/// ARM7TDMI CPU modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CpuMode {
    User = 0x10,
    Fiq = 0x11,
    Irq = 0x12,
    Supervisor = 0x13,
    Abort = 0x17,
    Undefined = 0x1B,
    System = 0x1F,
}

impl CpuMode {
    pub fn from_bits(bits: u32) -> Self {
        match bits & 0x1F {
            0x10 => CpuMode::User,
            0x11 => CpuMode::Fiq,
            0x12 => CpuMode::Irq,
            0x13 => CpuMode::Supervisor,
            0x17 => CpuMode::Abort,
            0x1B => CpuMode::Undefined,
            0x1F => CpuMode::System,
            _ => CpuMode::User, // Invalid mode: default to User
        }
    }

    /// Index for banked register storage (0-4).
    pub fn bank_index(self) -> usize {
        match self {
            CpuMode::User | CpuMode::System => 0,
            CpuMode::Fiq => 1,
            CpuMode::Irq => 2,
            CpuMode::Supervisor => 3,
            CpuMode::Abort => 4,
            CpuMode::Undefined => 5,
        }
    }

    pub fn has_spsr(self) -> bool {
        !matches!(self, CpuMode::User | CpuMode::System)
    }
}

/// Program Status Register (CPSR / SPSR).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Psr {
    pub bits: u32,
}

impl Psr {
    pub fn new(mode: CpuMode) -> Self {
        Psr {
            bits: mode as u32 | (1 << 7) | (1 << 6), // IRQ and FIQ disabled
        }
    }

    // Flag getters
    #[inline] pub fn n(self) -> bool { self.bits >> 31 != 0 }
    #[inline] pub fn z(self) -> bool { (self.bits >> 30) & 1 != 0 }
    #[inline] pub fn c(self) -> bool { (self.bits >> 29) & 1 != 0 }
    #[inline] pub fn v(self) -> bool { (self.bits >> 28) & 1 != 0 }
    #[inline] pub fn irq_disabled(self) -> bool { (self.bits >> 7) & 1 != 0 }
    #[inline] pub fn fiq_disabled(self) -> bool { (self.bits >> 6) & 1 != 0 }
    #[inline] pub fn thumb(self) -> bool { (self.bits >> 5) & 1 != 0 }
    #[inline] pub fn mode(self) -> CpuMode { CpuMode::from_bits(self.bits) }

    // Flag setters
    #[inline] pub fn set_n(&mut self, v: bool) { self.bits = (self.bits & !(1 << 31)) | ((v as u32) << 31); }
    #[inline] pub fn set_z(&mut self, v: bool) { self.bits = (self.bits & !(1 << 30)) | ((v as u32) << 30); }
    #[inline] pub fn set_c(&mut self, v: bool) { self.bits = (self.bits & !(1 << 29)) | ((v as u32) << 29); }
    #[inline] pub fn set_v(&mut self, v: bool) { self.bits = (self.bits & !(1 << 28)) | ((v as u32) << 28); }

    /// Set N and Z flags from a result value.
    #[inline]
    pub fn set_nz(&mut self, result: u32) {
        self.set_n(result >> 31 != 0);
        self.set_z(result == 0);
    }

    pub fn set_thumb(&mut self, v: bool) {
        self.bits = (self.bits & !(1 << 5)) | ((v as u32) << 5);
    }
}

/// Banked registers for mode switching.
/// FIQ banks R8-R14 and SPSR, other modes bank R13-R14 and SPSR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankedRegisters {
    /// R13 (SP) and R14 (LR) for each mode (indexed by CpuMode::bank_index)
    /// Index 0 = User/System, 1 = FIQ, 2 = IRQ, 3 = SVC, 4 = ABT, 5 = UND
    pub(crate) sp: [u32; 6],
    pub(crate) lr: [u32; 6],
    /// FIQ has its own R8-R12
    fiq_r8_r12: [u32; 5],
    /// User/System R8-R12 (saved when switching to FIQ)
    usr_r8_r12: [u32; 5],
    /// SPSR for each privileged mode (FIQ, IRQ, SVC, ABT, UND)
    spsr: [Psr; 5],
}

impl BankedRegisters {
    pub fn new() -> Self {
        BankedRegisters {
            sp: [0; 6],
            lr: [0; 6],
            fiq_r8_r12: [0; 5],
            usr_r8_r12: [0; 5],
            spsr: [Psr { bits: 0 }; 5],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cpu {
    /// General purpose registers R0-R15.
    /// R13 = SP, R14 = LR, R15 = PC.
    pub regs: [u32; 16],
    /// Current Program Status Register.
    pub cpsr: Psr,
    /// Banked registers for mode switching.
    pub(crate) banked: BankedRegisters,
    /// Pipeline: two prefetched opcodes.
    pipeline: [u32; 2],
    /// Whether the pipeline needs to be refilled (after branch).
    pub pipeline_flushed: bool,
    /// CPU is halted (waiting for interrupt).
    pub halted: bool,
    /// Pending SWI comment (set by SWI instruction, consumed by Gba::step).
    pub(crate) pending_swi: Option<u8>,
}

impl Cpu {
    pub fn new() -> Self {
        let mut cpu = Cpu {
            regs: [0; 16],
            cpsr: Psr::new(CpuMode::Supervisor),
            banked: BankedRegisters::new(),
            pipeline: [0; 2],
            pipeline_flushed: true,
            halted: false,
            pending_swi: None,
        };
        // ARM7TDMI starts in ARM mode, Supervisor, at address 0x00000000
        // The BIOS will set up the stack pointers and jump to the ROM
        cpu.regs[15] = 0x0000_0000;
        // If no BIOS, start at ROM entry point
        // (caller should set PC to 0x08000000 if skipping BIOS)
        cpu
    }

    /// Create CPU with initial state for skipping BIOS.
    pub fn new_skip_bios() -> Self {
        let mut cpu = Cpu::new();
        cpu.cpsr = Psr::new(CpuMode::System);
        cpu.cpsr.bits &= !(1 << 7); // Enable IRQ
        cpu.cpsr.bits &= !(1 << 6); // Enable FIQ
        cpu.regs[13] = 0x0300_7F00; // SP for System/User mode
        cpu.banked.sp[CpuMode::Irq.bank_index()] = 0x0300_7FA0;
        cpu.banked.sp[CpuMode::Supervisor.bank_index()] = 0x0300_7FE0;
        cpu.regs[15] = 0x0800_0000; // ROM entry point
        cpu.pipeline_flushed = true;
        cpu
    }

    /// Execute one instruction. Returns the number of cycles consumed.
    pub fn step(&mut self, bus: &mut Bus) -> u32 {
        // Check for pending interrupts
        if bus.interrupt.has_pending() && !self.cpsr.irq_disabled() {
            self.handle_interrupt(bus);
            self.halted = false;
        }

        if self.halted {
            return 1; // Idle cycle
        }

        // Refill pipeline if needed (after branch or init)
        if self.pipeline_flushed {
            self.refill_pipeline(bus);
        }

        if self.cpsr.thumb() {
            self.step_thumb(bus)
        } else {
            self.step_arm(bus)
        }
    }

    fn step_arm(&mut self, bus: &mut Bus) -> u32 {
        // Invariant: at start of step, regs[15] = executing_instruction_address + 8.
        // This matches the ARM7TDMI spec where PC reads during execution return PC+8.
        let opcode = self.pipeline[0];

        // Check condition
        if !self.check_condition(opcode >> 28) {
            // Condition failed: still advance pipeline to skip this instruction
            self.advance_arm_pipeline(bus);
            return 1; // 1S cycle
        }

        // Execute with regs[15] at its correct "PC = X+8" value
        let cycles = self.execute_arm(bus, opcode);

        // Advance pipeline only if the instruction didn't flush it (branch/SWI/etc)
        if !self.pipeline_flushed {
            self.advance_arm_pipeline(bus);
        }

        cycles
    }

    fn step_thumb(&mut self, bus: &mut Bus) -> u32 {
        // Invariant: at start of step, regs[15] = executing_instruction_address + 4.
        let opcode = self.pipeline[0] as u16;

        let cycles = self.execute_thumb(bus, opcode);

        if !self.pipeline_flushed {
            self.advance_thumb_pipeline(bus);
        }

        cycles
    }

    #[inline]
    fn advance_arm_pipeline(&mut self, bus: &mut Bus) {
        self.pipeline[0] = self.pipeline[1];
        self.pipeline[1] = bus.read32(self.regs[15]);
        self.regs[15] = self.regs[15].wrapping_add(4);
    }

    #[inline]
    fn advance_thumb_pipeline(&mut self, bus: &mut Bus) {
        self.pipeline[0] = self.pipeline[1];
        self.pipeline[1] = bus.read16(self.regs[15]) as u32;
        self.regs[15] = self.regs[15].wrapping_add(2);
    }

    fn refill_pipeline(&mut self, bus: &mut Bus) {
        if self.cpsr.thumb() {
            let pc = self.regs[15] & !1;
            self.pipeline[0] = bus.read16(pc) as u32;
            self.pipeline[1] = bus.read16(pc + 2) as u32;
            self.regs[15] = pc + 4;
        } else {
            let pc = self.regs[15] & !3;
            self.pipeline[0] = bus.read32(pc);
            self.pipeline[1] = bus.read32(pc + 4);
            self.regs[15] = pc + 8;
        }
        self.pipeline_flushed = false;
    }

    /// Check ARM condition code (bits 31:28).
    fn check_condition(&self, cond: u32) -> bool {
        match cond & 0xF {
            0x0 => self.cpsr.z(),                                    // EQ
            0x1 => !self.cpsr.z(),                                   // NE
            0x2 => self.cpsr.c(),                                    // CS/HS
            0x3 => !self.cpsr.c(),                                   // CC/LO
            0x4 => self.cpsr.n(),                                    // MI
            0x5 => !self.cpsr.n(),                                   // PL
            0x6 => self.cpsr.v(),                                    // VS
            0x7 => !self.cpsr.v(),                                   // VC
            0x8 => self.cpsr.c() && !self.cpsr.z(),                  // HI
            0x9 => !self.cpsr.c() || self.cpsr.z(),                  // LS
            0xA => self.cpsr.n() == self.cpsr.v(),                   // GE
            0xB => self.cpsr.n() != self.cpsr.v(),                   // LT
            0xC => !self.cpsr.z() && (self.cpsr.n() == self.cpsr.v()), // GT
            0xD => self.cpsr.z() || (self.cpsr.n() != self.cpsr.v()), // LE
            0xE => true,                                              // AL (Always)
            0xF => true,                                              // Unconditional (ARMv5+, treat as AL on ARM7TDMI)
            _ => unreachable!(),
        }
    }

    /// Switch CPU mode, banking/restoring registers as needed.
    pub fn switch_mode(&mut self, new_mode: CpuMode) {
        let old_mode = self.cpsr.mode();
        if old_mode == new_mode {
            return;
        }

        // Bank current SP and LR
        let old_bank = old_mode.bank_index();
        self.banked.sp[old_bank] = self.regs[13];
        self.banked.lr[old_bank] = self.regs[14];

        // Bank FIQ R8-R12 if switching from/to FIQ
        if old_mode == CpuMode::Fiq {
            self.banked.fiq_r8_r12.copy_from_slice(&self.regs[8..13]);
            self.regs[8..13].copy_from_slice(&self.banked.usr_r8_r12);
        } else if new_mode == CpuMode::Fiq {
            self.banked.usr_r8_r12.copy_from_slice(&self.regs[8..13]);
            self.regs[8..13].copy_from_slice(&self.banked.fiq_r8_r12);
        }

        // Restore new mode's SP and LR
        let new_bank = new_mode.bank_index();
        self.regs[13] = self.banked.sp[new_bank];
        self.regs[14] = self.banked.lr[new_bank];

        // Update mode bits in CPSR
        self.cpsr.bits = (self.cpsr.bits & !0x1F) | (new_mode as u32);
    }

    /// Get the SPSR for the current mode.
    pub fn spsr(&self) -> Psr {
        let mode = self.cpsr.mode();
        if mode.has_spsr() {
            let index = match mode {
                CpuMode::Fiq => 0,
                CpuMode::Irq => 1,
                CpuMode::Supervisor => 2,
                CpuMode::Abort => 3,
                CpuMode::Undefined => 4,
                _ => return self.cpsr,
            };
            self.banked.spsr[index]
        } else {
            self.cpsr // User/System don't have SPSR
        }
    }

    /// Set the SPSR for the current mode.
    pub fn set_spsr(&mut self, psr: Psr) {
        let mode = self.cpsr.mode();
        if mode.has_spsr() {
            let index = match mode {
                CpuMode::Fiq => 0,
                CpuMode::Irq => 1,
                CpuMode::Supervisor => 2,
                CpuMode::Abort => 3,
                CpuMode::Undefined => 4,
                _ => return,
            };
            self.banked.spsr[index] = psr;
        }
    }

    /// Handle an IRQ interrupt.
    fn handle_interrupt(&mut self, _bus: &mut Bus) {
        let return_addr = if self.cpsr.thumb() {
            self.regs[15] // PC is already ahead by 4 in THUMB
        } else {
            self.regs[15].wrapping_sub(4) // PC is ahead by 8, return to PC-4
        };

        // Save CPSR to SPSR_irq
        let saved_cpsr = self.cpsr;
        self.switch_mode(CpuMode::Irq);
        self.set_spsr(saved_cpsr);

        // Set return address in LR_irq
        self.regs[14] = return_addr;

        // Enter ARM mode, disable IRQ
        self.cpsr.set_thumb(false);
        self.cpsr.bits |= 1 << 7; // Disable IRQ

        // Jump to IRQ vector
        self.regs[15] = 0x0000_0018;
        self.pipeline_flushed = true;
    }

    /// Software interrupt (SWI).
    pub fn software_interrupt(&mut self, _comment: u32) {
        let return_addr = if self.cpsr.thumb() {
            self.regs[15].wrapping_sub(2)
        } else {
            self.regs[15].wrapping_sub(4)
        };

        let saved_cpsr = self.cpsr;
        self.switch_mode(CpuMode::Supervisor);
        self.set_spsr(saved_cpsr);

        self.regs[14] = return_addr;
        self.cpsr.set_thumb(false);
        self.cpsr.bits |= 1 << 7; // Disable IRQ

        self.regs[15] = 0x0000_0008;
        self.pipeline_flushed = true;
    }

    /// Branch: set PC and flush pipeline.
    #[inline]
    pub fn branch(&mut self, addr: u32) {
        self.regs[15] = addr;
        self.pipeline_flushed = true;
    }

    /// Branch and exchange (BX): switch ARM/THUMB based on bit 0.
    #[inline]
    pub fn branch_exchange(&mut self, addr: u32) {
        self.cpsr.set_thumb(addr & 1 != 0);
        self.regs[15] = addr & !1;
        self.pipeline_flushed = true;
    }

    // ─── Register read helpers (PC+8 for ARM, PC+4 for THUMB) ───

    /// Read a register value. If reading PC (R15), returns the current
    /// instruction address + 8 (ARM) or + 4 (THUMB), which is the value
    /// already in self.regs[15] due to pipeline advancement.
    #[inline]
    pub fn reg(&self, r: u8) -> u32 {
        self.regs[r as usize & 0xF]
    }

    /// Write to a register. If writing PC, trigger a branch.
    #[inline]
    pub fn set_reg(&mut self, r: u8, val: u32) {
        let r = r as usize & 0xF;
        if r == 15 {
            self.branch(val & !1);
        } else {
            self.regs[r] = val;
        }
    }

    /// Write to a register without triggering a branch (used in data processing
    /// when S bit is set and Rd=R15 to restore CPSR from SPSR).
    pub fn set_reg_with_flags(&mut self, r: u8, val: u32, s: bool) {
        let r_idx = r as usize & 0xF;
        if r_idx == 15 {
            if s {
                // Rd=R15 with S bit: restore CPSR from SPSR, then branch
                let spsr = self.spsr();
                let new_mode = spsr.mode();
                self.switch_mode(new_mode);
                self.cpsr = spsr;
            }
            self.branch(val & !1);
        } else {
            self.regs[r_idx] = val;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_psr_flags() {
        let mut psr = Psr { bits: 0 };
        psr.set_n(true);
        assert!(psr.n());
        psr.set_z(true);
        assert!(psr.z());
        psr.set_c(true);
        assert!(psr.c());
        psr.set_v(true);
        assert!(psr.v());
        psr.set_nz(0);
        assert!(!psr.n());
        assert!(psr.z());
        psr.set_nz(0x80000000);
        assert!(psr.n());
        assert!(!psr.z());
    }

    #[test]
    fn test_condition_codes() {
        let cpu = Cpu::new();
        // AL should always pass
        assert!(cpu.check_condition(0xE));
    }

    #[test]
    fn test_mode_switching() {
        let mut cpu = Cpu::new();
        cpu.cpsr = Psr::new(CpuMode::System);
        cpu.regs[13] = 0x1234;
        cpu.regs[14] = 0x5678;
        cpu.switch_mode(CpuMode::Irq);
        assert_eq!(cpu.cpsr.mode(), CpuMode::Irq);
        // IRQ has its own SP/LR (initially 0)
        assert_eq!(cpu.regs[13], 0);
        assert_eq!(cpu.regs[14], 0);
        // Switch back
        cpu.switch_mode(CpuMode::System);
        assert_eq!(cpu.regs[13], 0x1234);
        assert_eq!(cpu.regs[14], 0x5678);
    }
}
