pub mod alu;
pub mod arm;
pub mod thumb;
pub mod disasm;

use crate::bus::Bus;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Cached lookup of debug env vars. `std::env::var` is too expensive to
/// call once per CPU step (~16M times/sec); cache the result at first
/// access so hot paths just read a bool from a OnceLock.
fn experiment_gate_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("EXPERIMENT_GATE").is_ok())
}

fn instr_trace_ring_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("INSTR_TRACE_RING").is_ok())
}

fn irq_trace_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("IRQ_TRACE").is_ok())
}

fn cycle_profile_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("CYCLE_PROFILE").is_ok())
}

/// FE7-specific probe: prints IF/IE/IME state at three key PCs in FE7's
/// ARM IRQ handler so we can pinpoint *when* a new HBlank IRQ becomes
/// pending during the ack→MSR window.
///
///   0x03003A1C — just before `STRH R0, [R3, #2]` (the ack to REG_IF)
///   0x03003A20 — just after that ack
///   0x03003A30 — just before `MSR CPSR_FC, R3` (which re-enables IRQs)
///
/// If IF is clear at A20 but set at A30, something is re-asserting IF in
/// the ~12 cycles between — pointing at a scheduler issue. If IF stays
/// set at A20, the ack itself isn't reaching write_if correctly.
fn fe7_probe_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("FE7_PROBE").is_ok())
}

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
    /// Debug: count of interrupt-handler entries.
    #[serde(default)]
    pub irq_entries: u64,
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
            irq_entries: 0,
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
        // Snapshot PC for debug tracers (MEM_WATCH etc). Cheap.
        bus.last_pc = if self.cpsr.thumb() {
            self.regs[15].wrapping_sub(4)
        } else {
            self.regs[15].wrapping_sub(8)
        };
        // Refill the pipeline BEFORE checking for IRQs. After a branch
        // (or any instruction that wrote PC), pipeline_flushed is true and
        // regs[15] holds the raw branch target — not the +4/+8 pipeline-
        // ahead value. handle_interrupt() reads regs[15] to compute LR_irq
        // and would store target-4 (THUMB) / target-8 (ARM) instead of the
        // correct target+4 / target+4. Flaky bug: only manifests when an
        // IRQ fires in the narrow window between a branch and its first
        // post-branch instruction. Refilling here makes regs[15] correct.
        if self.pipeline_flushed {
            self.refill_pipeline(bus);
        }

        let gate_irq = experiment_gate_enabled()
            && bus.backup_busy();

        // (FE7 investigation: previously had IntrWait HLE re-halt logic
        // here, but it only affects 3 init SWI 0x05 calls — FE7's main
        // loop doesn't use SWI 0x05 to wait for VBlank, so the audio
        // buffer cascade isn't fixed by gating SWI 0x05. Reverted.
        // See debug/2026-05-24_fe7-hblank-irq-cascade.md.)

        // Check for pending interrupts
        let mut irq_entry_cycles = 0u32;
        if !gate_irq && bus.interrupt.has_pending() && !self.cpsr.irq_disabled() {
            self.irq_entries += 1;
            self.handle_interrupt(bus);
            self.halted = false;
            // ARM7TDMI exception entry overhead: 3 cycles to bank registers,
            // save CPSR to SPSR, switch mode, set PC. The pipeline-refill
            // memory accesses are separately counted via bus.add_mem_cycles.
            irq_entry_cycles = 3;
        }

        if self.halted {
            bus.tick_backup(1);
            return 1; // Idle cycle
        }

        // IRQ entry flushes the pipeline; refill at the vector.
        if self.pipeline_flushed {
            self.refill_pipeline(bus);
        }

        // Clear any wait-state cycles accumulated by the IRQ-entry or
        // pipeline-refill memory reads above (they're conceptually part of
        // this step). All reads/writes from now until the end of step()
        // will be summed into bus.mem_access_cycles. We harvest below.
        let prior_mem_cycles = bus.take_mem_cycles();
        let in_irq = self.cpsr.mode() == CpuMode::Irq;
        let cycles = if self.cpsr.thumb() {
            self.step_thumb(bus)
        } else {
            self.step_arm(bus)
        };
        let mem_cycles = bus.take_mem_cycles();
        let total = cycles + prior_mem_cycles + mem_cycles + irq_entry_cycles;

        // CYCLE_PROFILE: count cycles per mode for diagnostics (env-gated,
        // OnceLock — compiles to a single relaxed load + branch when off).
        if cycle_profile_enabled() {
            crate::cycle_profile_record(in_irq, total);
        }

        bus.tick_backup(total);
        total
    }

    fn step_arm(&mut self, bus: &mut Bus) -> u32 {
        if instr_trace_ring_enabled() {
            let pc = self.regs[15].wrapping_sub(8);
            crate::push_trace_arm(pc, self.pipeline[0],
                self.regs[0], self.regs[1], self.regs[2], self.regs[3],
                self.regs[13], self.regs[14]);
        }
        if fe7_probe_enabled() {
            let pc = self.regs[15].wrapping_sub(8);
            // Probe at FE7's audio mixer entry (IWRAM 0x030031AC). Logs
            // R2 (sample-data ptr), R4 (sample count, loaded a few instr in),
            // and LR (return address — tells us the caller). PC=0x030031B0
            // is just after the initial LDR R7 — at this point we haven't
            // loaded R4 yet but R2 has whatever caller passed.
            if pc == 0x030031AC || pc == 0x030031B8 || pc == 0x030031C0 {
                let _ = bus;
                let vcount = bus.io.vcount;
                let cyc = crate::GLOBAL_CYCLES.load(std::sync::atomic::Ordering::Relaxed);
                let lr = self.regs[14];
                let r4 = self.regs[4];
                let r2 = self.regs[2];
                let r5 = self.regs[5];
                eprintln!(
                    "[FE7] pc={pc:08X} cyc={cyc} \
                     vc={vcount} r2=0x{r2:08X} r4=0x{r4:08X} r5=0x{r5:08X} lr=0x{lr:08X}"
                );
            }
        }
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

        // Ring-buffer trace of last N THUMB instructions. When PC escapes to
        // unmapped memory, we'll dump the buffer.
        if instr_trace_ring_enabled() {
            let pc = self.regs[15].wrapping_sub(4);
            crate::push_trace_thumb(pc, opcode,
                self.regs[0], self.regs[1], self.regs[2], self.regs[3],
                self.regs[13], self.regs[14]);
        }

        // FE7 audio engine main probe: log every entry to 0x0801529C
        // (audio_engine_main) so we can count per-frame invocations.
        // Also dumps the first 32 bytes of the channel array at 0x0202A48C
        // (= 2 candidate channel-list head pointers + adjacent state).
        if fe7_probe_enabled() {
            let pc = self.regs[15].wrapping_sub(4);
            // Main loop split: 0xAEE = BL audio_wrapper, 0xAF2 = BL other_call.
            // audio_cost = cyc(0xAF2) - cyc(0xAEE); other_cost = cyc(next 0xAEE) - cyc(0xAF2).
            if pc == 0x08000AEE || pc == 0x08000AF2 {
                use std::sync::atomic::{AtomicU64, Ordering};
                let cyc = crate::GLOBAL_CYCLES.load(Ordering::Relaxed);
                static AEE_CYC: AtomicU64 = AtomicU64::new(0);
                static AF2_CYC: AtomicU64 = AtomicU64::new(0);
                static AUDIO_SUM: AtomicU64 = AtomicU64::new(0);
                static OTHER_SUM: AtomicU64 = AtomicU64::new(0);
                static CNT: AtomicU64 = AtomicU64::new(0);
                static AEE_INSTR: AtomicU64 = AtomicU64::new(0);
                static AUDIO_INSTR_SUM: AtomicU64 = AtomicU64::new(0);
                if pc == 0x08000AF2 {
                    let aee = AEE_CYC.load(Ordering::Relaxed);
                    if aee > 0 {
                        AUDIO_SUM.fetch_add(cyc - aee, Ordering::Relaxed);
                        let ic = crate::INSTR_COUNT.load(Ordering::Relaxed);
                        let aee_ic = AEE_INSTR.load(Ordering::Relaxed);
                        if aee_ic > 0 { AUDIO_INSTR_SUM.fetch_add(ic - aee_ic, Ordering::Relaxed); }
                    }
                    AF2_CYC.store(cyc, Ordering::Relaxed);
                } else {
                    let af2 = AF2_CYC.load(Ordering::Relaxed);
                    if af2 > 0 {
                        OTHER_SUM.fetch_add(cyc - af2, Ordering::Relaxed);
                        let n = CNT.fetch_add(1, Ordering::Relaxed) + 1;
                        if n % 100 == 0 {
                            let a = AUDIO_SUM.load(Ordering::Relaxed);
                            let o = OTHER_SUM.load(Ordering::Relaxed);
                            let buf_ptr = bus.peek32(0x0300_2F34);
                            let ai = AUDIO_INSTR_SUM.load(Ordering::Relaxed);
                            let cpi = if ai > 0 { a as f64 / ai as f64 } else { 0.0 };
                            eprintln!(
                                "[LOOP] n={n} audio_avg={} audio_instr={} cyc_per_instr={cpi:.2} other_avg={} ptr=0x{buf_ptr:08X}",
                                a / n, ai / n, o / n
                            );
                        }
                    }
                    AEE_CYC.store(cyc, Ordering::Relaxed);
                    AEE_INSTR.store(crate::INSTR_COUNT.load(Ordering::Relaxed), Ordering::Relaxed);
                }
            }
            if pc == 0x0801529C {
                let cyc = crate::GLOBAL_CYCLES.load(std::sync::atomic::Ordering::Relaxed);
                let vcount = bus.io.vcount;
                let lr = self.regs[14];
                // Dump first 32 bytes of channel array at 0x0202A48C
                let mut chan_dump = String::new();
                for off in (0..32).step_by(4) {
                    let v = bus.peek32(0x0202A48C + off);
                    chan_dump.push_str(&format!(" {v:08X}"));
                }
                eprintln!(
                    "[AUDIO_MAIN] cyc={cyc} vc={vcount} lr=0x{lr:08X} chan_array=[{chan_dump} ]"
                );
            }
        }
        let cycles = self.execute_thumb(bus, opcode);

        if !self.pipeline_flushed {
            self.advance_thumb_pipeline(bus);
        }

        cycles
    }

    #[inline]
    fn advance_arm_pipeline(&mut self, bus: &mut Bus) {
        self.pipeline[0] = self.pipeline[1];
        self.pipeline[1] = bus.fetch32(self.regs[15]);
        self.regs[15] = self.regs[15].wrapping_add(4);
    }

    #[inline]
    fn advance_thumb_pipeline(&mut self, bus: &mut Bus) {
        self.pipeline[0] = self.pipeline[1];
        self.pipeline[1] = bus.fetch16(self.regs[15]) as u32;
        self.regs[15] = self.regs[15].wrapping_add(2);
    }

    fn refill_pipeline(&mut self, bus: &mut Bus) {
        // Pipeline refill follows a branch / mode switch / IRQ entry, so
        // the first fetch is non-sequential. The second (advance) fetch
        // is naturally sequential because it's contiguous.
        bus.break_sequential();
        if self.cpsr.thumb() {
            let pc = self.regs[15] & !1;
            // Update bus.last_pc to the actual fetch PC so the BIOS-read
            // protection check (which uses last_pc to gate "PC in BIOS" vs
            // "PC outside BIOS" behavior) sees the correct value when this
            // refill targets a different region than the previous step
            // (e.g. an IRQ entry jumping from ROM to BIOS 0x18).
            bus.last_pc = pc;
            self.pipeline[0] = bus.fetch16(pc) as u32;
            self.pipeline[1] = bus.fetch16(pc + 2) as u32;
            self.regs[15] = pc + 4;
        } else {
            let pc = self.regs[15] & !3;
            bus.last_pc = pc;
            self.pipeline[0] = bus.fetch32(pc);
            self.pipeline[1] = bus.fetch32(pc + 4);
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
    fn handle_interrupt(&mut self, bus: &mut Bus) {
        // (FE7 investigation: previously updated BIOS_IF mirror at
        // 0x03007FF8 here to support proper SWI 0x04/0x05 wait. Reverted
        // because FE7's main loop doesn't use SWI 0x05 for VBlank-sync
        // and the change didn't address the actual cascade. See debug
        // doc for details.)

        let return_addr = if self.cpsr.thumb() {
            self.regs[15] // PC is already ahead by 4 in THUMB
        } else {
            self.regs[15].wrapping_sub(4) // PC is ahead by 8, return to PC-4
        };

        // Save CPSR to SPSR_irq
        let saved_cpsr = self.cpsr;
        if irq_trace_enabled() {
            eprintln!("[IRQ] enter cpsr=0x{:08X} mode={:?} thumb={} ret=0x{:08X} ie=0x{:04X} ir=0x{:04X} ime={}",
                saved_cpsr.bits, saved_cpsr.mode(), saved_cpsr.thumb(), return_addr,
                bus.interrupt.ie, bus.interrupt.ir, bus.interrupt.ime);
        }
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

    /// Read a register as seen by User/System mode, regardless of current mode.
    /// Used by LDM/STM with S bit (when R15 is not in the rlist — the "^" suffix).
    pub fn read_user_reg(&self, r: u8) -> u32 {
        let r = r as usize & 0xF;
        let mode = self.cpsr.mode();
        match r {
            0..=7 | 15 => self.regs[r],
            8..=12 => {
                if mode == CpuMode::Fiq {
                    self.banked.usr_r8_r12[r - 8]
                } else {
                    self.regs[r]
                }
            }
            13 => {
                if mode.bank_index() == 0 {
                    self.regs[13]
                } else {
                    self.banked.sp[0]
                }
            }
            14 => {
                if mode.bank_index() == 0 {
                    self.regs[14]
                } else {
                    self.banked.lr[0]
                }
            }
            _ => unreachable!(),
        }
    }

    /// Write a register as seen by User/System mode, regardless of current mode.
    /// Used by LDM with S bit loading into user-mode registers.
    pub fn write_user_reg(&mut self, r: u8, val: u32) {
        let r = r as usize & 0xF;
        let mode = self.cpsr.mode();
        match r {
            0..=7 => self.regs[r] = val,
            15 => self.regs[r] = val,
            8..=12 => {
                if mode == CpuMode::Fiq {
                    self.banked.usr_r8_r12[r - 8] = val;
                } else {
                    self.regs[r] = val;
                }
            }
            13 => {
                if mode.bank_index() == 0 {
                    self.regs[13] = val;
                } else {
                    self.banked.sp[0] = val;
                }
            }
            14 => {
                if mode.bank_index() == 0 {
                    self.regs[14] = val;
                } else {
                    self.banked.lr[0] = val;
                }
            }
            _ => unreachable!(),
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
                if irq_trace_enabled() {
                    eprintln!("[IRQ] return val=0x{:08X} spsr=0x{:08X} new_mode={:?} thumb={} from_mode={:?}",
                        val, spsr.bits, new_mode, spsr.thumb(), self.cpsr.mode());
                }
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

    /// The full Pokémon scenario: handler is in IRQ mode with pushed
    /// state, switches to System with IRQ ENABLED, a NESTED IRQ fires
    /// while in System mode. The nested IRQ must not corrupt the
    /// outer handler's SP_irq.
    #[test]
    fn test_sp_irq_preserved_with_nested_irq() {
        use crate::bus::Bus;
        let mut cpu = Cpu::new();
        cpu.cpsr = Psr::new(CpuMode::Irq);
        cpu.cpsr.bits |= 1 << 7;
        cpu.regs[13] = 0x03007F74;
        cpu.banked.sp[CpuMode::System.bank_index()] = 0x03007E20;
        let mut bus = Bus::new(None, vec![0u8; 0x100]);

        // Step A: outer handler MSR to System (with IRQs enabled).
        cpu.regs[3] = 0x4000_001F;
        cpu.execute_arm(&mut bus, 0xE129_F003);
        assert_eq!(cpu.cpsr.mode(), CpuMode::System);

        // Step B: nested IRQ fires. handle_interrupt simulates that.
        cpu.handle_interrupt(&mut bus);
        assert_eq!(cpu.cpsr.mode(), CpuMode::Irq);
        // We're in IRQ mode now with SP_irq loaded from banked.sp[Irq],
        // which should be 0x03007F74 (the post-push value from outer
        // handler).
        assert_eq!(cpu.regs[13], 0x03007F74,
            "nested IRQ entry: SP_irq should be outer handler's pushed value");

        // Pretend nested handler does some pushing.
        cpu.regs[13] -= 24; // nested BIOS push (6 regs)

        // Nested handler returns: switch_mode back to System (matches
        // SUBS PC,LR,#4 with SPSR_irq.mode() = System).
        // Manually do what set_reg_with_flags would do for an exception
        // return: switch to the SPSR's mode, then assign cpsr=spsr.
        let saved_spsr = cpu.spsr();
        cpu.switch_mode(saved_spsr.mode());
        cpu.cpsr = saved_spsr;
        assert_eq!(cpu.cpsr.mode(), CpuMode::System);

        // Step C: outer handler MSR back to IRQ.
        cpu.regs[3] = 0x4000_0092;
        cpu.execute_arm(&mut bus, 0xE129_F003);
        assert_eq!(cpu.cpsr.mode(), CpuMode::Irq);
        // SP_irq should match the value we set after step B (0x03007F5C).
        assert_eq!(cpu.regs[13], 0x03007F5C,
            "after nested IRQ + outer MSR back: SP_irq wrong");
    }

    /// Same round-trip but driven by actual MSR instructions executed
    /// through `arm_msr`, matching the path Pokémon's IRQ handler takes.
    #[test]
    fn test_sp_irq_preserved_through_msr_round_trip() {
        use crate::bus::Bus;
        let mut cpu = Cpu::new();
        cpu.cpsr = Psr::new(CpuMode::Irq);
        cpu.cpsr.bits |= 1 << 7; // IRQ disabled (in IRQ handler)
        cpu.regs[13] = 0x03007F74;
        cpu.banked.sp[CpuMode::System.bank_index()] = 0x03007E20;

        // Tiny ROM is enough — arm_msr doesn't touch the bus.
        let mut bus = Bus::new(None, vec![0u8; 0x100]);

        // Encode `MSR CPSR_fc, R3` (E129F003).
        // Then set R3 to System-mode CPSR (0x4000001F: System, IRQ enabled).
        cpu.regs[3] = 0x4000_001F;
        cpu.execute_arm(&mut bus, 0xE129_F003);
        assert_eq!(cpu.cpsr.mode(), CpuMode::System);
        assert_eq!(cpu.regs[13], 0x03007E20);

        // Pretend we did some work in System mode — push something.
        cpu.regs[13] = 0x03007D00;

        // Now MSR back to IRQ mode (R3 = 0x40000092: IRQ, IRQ disabled).
        cpu.regs[3] = 0x4000_0092;
        cpu.execute_arm(&mut bus, 0xE129_F003);
        assert_eq!(cpu.cpsr.mode(), CpuMode::Irq);
        assert_eq!(cpu.regs[13], 0x03007F74,
            "SP_irq corrupted across IRQ→System→IRQ via MSR");
    }

    /// Regression: Pokémon Emerald in-game save hangs because its IRQ
    /// handler does:
    ///   1. (IRQ mode) push regs → SP_irq goes down
    ///   2. MSR back to System → switch_mode saves SP_irq, loads SP_sys
    ///   3. (System mode) work, SP_sys may move
    ///   4. MSR to IRQ → switch_mode saves SP_sys, loads SP_irq
    ///   5. (IRQ mode) pop regs → SP_irq must equal value from step 1
    ///
    /// SP_irq must be preserved across the System-mode round-trip.
    #[test]
    fn test_sp_irq_preserved_across_round_trip() {
        let mut cpu = Cpu::new();
        cpu.cpsr = Psr::new(CpuMode::Irq);
        // Initial IRQ SP after some pushes.
        cpu.regs[13] = 0x03007F74;
        // Initial System SP value to load when we switch.
        cpu.banked.sp[CpuMode::System.bank_index()] = 0x03007E20;

        // Step: IRQ → System
        cpu.switch_mode(CpuMode::System);
        assert_eq!(cpu.cpsr.mode(), CpuMode::System);
        assert_eq!(cpu.regs[13], 0x03007E20);

        // System mode work — SP changes
        cpu.regs[13] = 0x03007D00;

        // Step: System → IRQ
        cpu.switch_mode(CpuMode::Irq);
        assert_eq!(cpu.cpsr.mode(), CpuMode::Irq);
        // KEY ASSERTION: SP_irq must be the post-push value from step 1,
        // not anything else.
        assert_eq!(cpu.regs[13], 0x03007F74,
            "SP_irq corrupted across IRQ→System→IRQ round trip");
    }
}
