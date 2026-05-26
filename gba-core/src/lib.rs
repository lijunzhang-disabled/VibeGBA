pub mod arm7tdmi;
pub mod bios;
pub mod bus;
pub mod ppu;
pub mod apu;
pub mod backup;
pub mod dma;
pub mod timer;
pub mod interrupt;
pub mod keypad;
pub mod rtc;
pub mod scheduler;

// ─── Instruction-trace ring buffer (debug only) ─────────────────────
//
// When INSTR_TRACE_RING=1, every CPU step pushes a record into a fixed-size
// ring. When the CPU escapes (PC goes outside valid memory), the frontend
// can call dump_trace_ring() to print the last N instructions executed.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

// Global cycle clock — updated by the step loop before each cpu.step().
// Lets diagnostic probes inside the CPU sample wall-clock cycles without
// having to thread the scheduler through. Zero cost when probes don't read.
pub static GLOBAL_CYCLES: AtomicU64 = AtomicU64::new(0);

// Cycle profiler — tracks cycles spent in user mode vs IRQ mode per
// frame. Diagnostic for finding where the FE7 ~3× cycle-counting gap
// is hiding. Enabled with CYCLE_PROFILE=1.
pub static PROFILE_USER_CYCLES: AtomicU64 = AtomicU64::new(0);
pub static PROFILE_IRQ_CYCLES: AtomicU64 = AtomicU64::new(0);
pub static PROFILE_FRAMES: AtomicU64 = AtomicU64::new(0);
pub static PROFILE_HALT_CYCLES: AtomicU64 = AtomicU64::new(0);

pub fn cycle_profile_record(in_irq: bool, cycles: u32) {
    if in_irq {
        PROFILE_IRQ_CYCLES.fetch_add(cycles as u64, Ordering::Relaxed);
    } else {
        PROFILE_USER_CYCLES.fetch_add(cycles as u64, Ordering::Relaxed);
    }
}

pub fn cycle_profile_record_halt(cycles: u64) {
    PROFILE_HALT_CYCLES.fetch_add(cycles, Ordering::Relaxed);
}

pub fn cycle_profile_report() {
    let frames = PROFILE_FRAMES.load(Ordering::Relaxed);
    let user = PROFILE_USER_CYCLES.load(Ordering::Relaxed);
    let irq = PROFILE_IRQ_CYCLES.load(Ordering::Relaxed);
    let halt = PROFILE_HALT_CYCLES.load(Ordering::Relaxed);
    if frames > 0 {
        let total = user + irq + halt;
        eprintln!(
            "[PROFILE] frames={frames} user={user} ({:.1}/f) irq={irq} ({:.1}/f) halt={halt} ({:.1}/f) total={total} ({:.1}/f, expected 280896)",
            user as f64 / frames as f64,
            irq as f64 / frames as f64,
            halt as f64 / frames as f64,
            total as f64 / frames as f64,
        );
    }
}

#[derive(Clone, Copy, Default)]
pub struct TraceEntry {
    pub pc: u32,
    pub op: u32,
    pub thumb: bool,
    pub r0: u32, pub r1: u32, pub r2: u32, pub r3: u32,
    pub sp: u32, pub lr: u32,
}

const TRACE_RING_SIZE: usize = 32768;
static TRACE_RING: Mutex<Option<Box<[TraceEntry; TRACE_RING_SIZE]>>> = Mutex::new(None);
static TRACE_HEAD: Mutex<usize> = Mutex::new(0);
static TRACE_FROZEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn pc_in_valid_code(pc: u32) -> bool {
    // PCs valid for instruction execution:
    //   BIOS:   0x00000000..0x00003FFF
    //   EWRAM:  0x02000000..0x0203FFFF (256 KB)
    //   IWRAM:  0x03000000..0x03007FFF (32 KB)
    //   ROM:    0x08000000..0x0DFFFFFF (with mirrors 0x09..0x0D)
    // NOT valid: VRAM (0x06xxxxxx) — VRAM is data, not code.
    // Treating VRAM as valid masks runaway-PC bugs where PC escapes into
    // VRAM mirror space (e.g., 0x06FD7000) and the CPU executes garbage.
    match pc >> 24 {
        0x00 => pc < 0x0000_4000,
        0x02 => pc < 0x0204_0000,
        0x03 => pc < 0x0300_8000,
        0x08..=0x0D => true,
        _ => false,
    }
}

/// Optional "freeze trace ring when PC first reaches this address" env var.
/// Set TRACE_FREEZE_PC=0xADDR (hex) to capture the 256 instructions BEFORE
/// the CPU reaches a known-bad PC — used for diagnosing hangs where the
/// stuck-in-loop pattern means a normal trace ring just shows the loop
/// repeating endlessly, with no path-into-the-loop visible.
fn trace_freeze_at_pc() -> Option<u32> {
    use std::sync::OnceLock;
    static V: OnceLock<Option<u32>> = OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("TRACE_FREEZE_PC").ok().and_then(|s| {
            u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()
        })
    })
}

fn ensure_ring() {
    let mut g = TRACE_RING.lock().unwrap();
    if g.is_none() {
        *g = Some(Box::new([TraceEntry::default(); TRACE_RING_SIZE]));
    }
}

pub fn push_trace_thumb(pc: u32, op: u16, r0: u32, r1: u32, r2: u32, r3: u32, sp: u32, lr: u32) {
    if TRACE_FROZEN.load(std::sync::atomic::Ordering::Relaxed) { return; }
    ensure_ring();
    let should_freeze = !pc_in_valid_code(pc) || trace_freeze_at_pc() == Some(pc);
    if should_freeze {
        TRACE_FROZEN.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let mut head = TRACE_HEAD.lock().unwrap();
    let mut ring = TRACE_RING.lock().unwrap();
    if let Some(r) = ring.as_mut() {
        r[*head] = TraceEntry { pc, op: op as u32, thumb: true, r0, r1, r2, r3, sp, lr };
        *head = (*head + 1) % TRACE_RING_SIZE;
    }
    drop(ring);
    drop(head);
    if should_freeze {
        eprintln!("=== TRACE FREEZE: pc=0x{pc:08X} thumb op=0x{op:04X} ===");
        dump_trace_ring();
    }
}

pub fn push_trace_arm(pc: u32, op: u32, r0: u32, r1: u32, r2: u32, r3: u32, sp: u32, lr: u32) {
    if TRACE_FROZEN.load(std::sync::atomic::Ordering::Relaxed) { return; }
    ensure_ring();
    let should_freeze = !pc_in_valid_code(pc) || trace_freeze_at_pc() == Some(pc);
    if should_freeze {
        TRACE_FROZEN.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let mut head = TRACE_HEAD.lock().unwrap();
    let mut ring = TRACE_RING.lock().unwrap();
    if let Some(r) = ring.as_mut() {
        r[*head] = TraceEntry { pc, op, thumb: false, r0, r1, r2, r3, sp, lr };
        *head = (*head + 1) % TRACE_RING_SIZE;
    }
    drop(ring);
    drop(head);
    if should_freeze {
        eprintln!("=== TRACE FREEZE: pc=0x{pc:08X} arm op=0x{op:08X} ===");
        dump_trace_ring();
    }
}

pub fn trace_is_frozen() -> bool {
    TRACE_FROZEN.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn dump_trace_ring() {
    let head = *TRACE_HEAD.lock().unwrap();
    let ring = TRACE_RING.lock().unwrap();
    if let Some(r) = ring.as_ref() {
        eprintln!("=== Last {} CPU instructions (oldest first): ===", TRACE_RING_SIZE);
        for i in 0..TRACE_RING_SIZE {
            let idx = (head + i) % TRACE_RING_SIZE;
            let e = r[idx];
            if e.pc == 0 && e.op == 0 { continue; }
            eprintln!(
                "  PC=0x{:08X} {} op=0x{:08X} r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} sp=0x{:08X} lr=0x{:08X}",
                e.pc, if e.thumb {"T"} else {"A"}, e.op,
                e.r0, e.r1, e.r2, e.r3, e.sp, e.lr,
            );
        }
    }
}

use arm7tdmi::Cpu;
use bus::Bus;
use dma::DmaTiming;
use scheduler::{Event, EventKind, Scheduler};
use serde::{Deserialize, Serialize};

/// GBA timing constants
pub const CPU_CLOCK_HZ: u32 = 16_777_216; // 16.78 MHz
pub const CYCLES_PER_DOT: u32 = 4;
pub const DOTS_PER_LINE: u32 = 308; // 240 visible + 68 HBlank
pub const CYCLES_PER_LINE: u32 = DOTS_PER_LINE * CYCLES_PER_DOT; // 1232
pub const VISIBLE_LINES: u16 = 160;
pub const VBLANK_LINES: u16 = 68;
pub const LINES_PER_FRAME: u16 = VISIBLE_LINES + VBLANK_LINES; // 228
pub const CYCLES_PER_FRAME: u64 = CYCLES_PER_LINE as u64 * LINES_PER_FRAME as u64; // 280896
pub const SCREEN_WIDTH: usize = 240;
pub const SCREEN_HEIGHT: usize = 160;

/// Visible pixel portion of a scanline in cycles
pub const HDRAW_CYCLES: u32 = 240 * CYCLES_PER_DOT; // 960
/// HBlank portion of a scanline in cycles
pub const HBLANK_CYCLES: u32 = 68 * CYCLES_PER_DOT; // 272

#[derive(Serialize, Deserialize)]
pub struct Gba {
    pub cpu: Cpu,
    pub bus: Bus,
    pub scheduler: Scheduler,
    /// 240x160 framebuffer, 15-bit RGB (xBBBBBGGGGGRRRRR)
    frame_buffer: Vec<u16>,
    /// Debug: total VBlank entries (line == 160 transitions) observed.
    pub vblank_entries: u64,
    /// Debug: total VBlank IRQ requests raised to the interrupt controller.
    pub vblank_irqs_raised: u64,
}

impl Gba {
    /// Create a new GBA instance with optional BIOS and a ROM.
    pub fn new(bios: Option<Vec<u8>>, rom: Vec<u8>) -> Self {
        let mut scheduler = Scheduler::new();
        // Schedule the first HBlank event
        scheduler.schedule(Event {
            fire_time: HDRAW_CYCLES as u64,
            kind: EventKind::HBlank,
        });

        let bus = Bus::new(bios, rom);
        let cpu = Cpu::new();

        Gba {
            cpu,
            bus,
            scheduler,
            frame_buffer: vec![0u16; SCREEN_WIDTH * SCREEN_HEIGHT],
            vblank_entries: 0,
            vblank_irqs_raised: 0,
        }
    }

    /// Run the emulator for one full frame (~280896 cycles).
    /// Returns a reference to the 240x160 framebuffer.
    pub fn run_frame(&mut self) -> &[u16] {
        self.run_cycles(CYCLES_PER_FRAME);
        &self.frame_buffer
    }

    /// Run the emulator for `cycles` CPU cycles. Used by audio-synced frontends
    /// that want to pump audio samples at a finer granularity than a full frame.
    pub fn run_cycles(&mut self, cycles: u64) {
        let target_time = self.scheduler.timestamp() + cycles;

        while self.scheduler.timestamp() < target_time {
            let next_event_time = self.scheduler.peek_time().unwrap_or(target_time);
            let step_target = next_event_time.min(target_time);

            // Step CPU until next event or chunk end
            while self.scheduler.timestamp() < step_target {
                if self.cpu.halted {
                    // CPU halted, but APU and timers must keep ticking on
                    // real hardware. Sub-step to each upcoming FIFO sample-
                    // pop (timer overflow), ticking APU with the CURRENT
                    // FIFO value over each sub-span, then popping. This
                    // keeps audio cycle-accurate during long halts (e.g.,
                    // SRTOG's VBlankIntrWait which halts ~99 % of frame).
                    let now = self.scheduler.timestamp();
                    let total_gap = (step_target - now) as u32;
                    let mut remaining = total_gap;
                    static HALT_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    let trace = *HALT_TRACE.get_or_init(|| std::env::var("HALT_TRACE").is_ok());
                    if trace {
                        eprintln!("[HALT] enter gap={} now={} step_target={} T0_cnt=0x{:04X} T0_ctl=0x{:04X}",
                            total_gap, now, step_target,
                            self.bus.timers.timers[0].counter,
                            self.bus.timers.timers[0].control);
                    }
                    while remaining > 0 {
                        let to_next = self.bus.timers.cycles_to_next_fifo_overflow();
                        // Cap to remaining; also cap to a safety max in case
                        // no timer is enabled (then to_next = u32::MAX).
                        let chunk = remaining.min(to_next).max(1).min(1024);
                        // APU sees old FIFO state for `chunk` cycles
                        self.bus.apu.tick(chunk);
                        // Now advance scheduler + timers (may pop FIFO once
                        // if we hit an overflow boundary)
                        self.scheduler.add_cycles(chunk as u64);
                        self.tick_timers(chunk);
                        cycle_profile_record_halt(chunk as u64);
                        remaining -= chunk;
                    }
                    if trace {
                        eprintln!("[HALT] exit  T0_cnt=0x{:04X}",
                            self.bus.timers.timers[0].counter);
                    }
                    break;
                }
                GLOBAL_CYCLES.store(self.scheduler.timestamp(), Ordering::Relaxed);
                let cycles = self.cpu.step(&mut self.bus) as u64;
                self.scheduler.add_cycles(cycles);

                // Tick timers and audio
                self.tick_timers(cycles as u32);
                self.bus.apu.tick(cycles as u32);

                // Handle pending SWI
                if let Some(swi_num) = self.cpu.pending_swi.take() {
                    self.handle_swi(swi_num);
                }

                // Handle halt request (from HALTCNT write or SWI Halt)
                if self.bus.halt_requested {
                    self.bus.halt_requested = false;
                    self.cpu.halted = true;
                }
            }

            // Dispatch pending events
            while let Some(event) = self.scheduler.pop_if_ready() {
                self.handle_event(event);
            }

            // Wake a halted CPU when any enabled IRQ becomes pending.
            // Real ARM7TDMI: halt-wake is gated only by (IE & IF) != 0;
            // IME and CPSR.I gate IRQ *delivery* but not halt *exit*.
            // Without this, games that use SWI IntrWait / VBlankIntrWait
            // freeze on the first halt — step() is the only place that
            // clears `halted`, but the inner loop above skips step() while
            // halted (it fast-forwards the scheduler instead).
            if self.cpu.halted
                && (self.bus.interrupt.ie & self.bus.interrupt.ir) != 0
            {
                self.cpu.halted = false;
            }
        }
    }

    fn handle_event(&mut self, event: Event) {
        let current_time = self.scheduler.timestamp();

        match event.kind {
            EventKind::HBlank => {
                let line = self.bus.io.vcount;

                // Set HBlank flag in DISPSTAT
                self.bus.io.dispstat |= 0x0002;

                if line < VISIBLE_LINES {
                    // Render this scanline
                    self.bus.ppu.render_scanline(
                        line,
                        &self.bus.io,
                        &self.bus.palette,
                        &self.bus.vram,
                        &self.bus.oam,
                        &mut self.frame_buffer,
                    );
                }

                // Fire HBlank IRQ if enabled
                if self.bus.io.dispstat & 0x0010 != 0 {
                    static DISABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    let disabled = *DISABLE.get_or_init(|| {
                        std::env::var("DISABLE_HBLANK_IRQ").is_ok()
                    });
                    if !disabled {
                        self.bus.interrupt.request_irq(interrupt::Irq::HBlank);
                    }
                    static HBLANK_IRQ_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    let trace = *HBLANK_IRQ_TRACE.get_or_init(|| {
                        std::env::var("HBLANK_IRQ_TRACE").is_ok()
                    });
                    if trace {
                        eprintln!(
                            "[HB-IRQ] vc={} time={} ir=0x{:04X} ie=0x{:04X}",
                            line, current_time,
                            self.bus.interrupt.ir, self.bus.interrupt.ie,
                        );
                    }
                }

                // Trigger HBlank DMA — but ONLY for visible scanlines
                // (0..159). Real GBA does not fire HBlank DMA during
                // VBlank lines (160..227). Without this gate, games like
                // Pokémon Emerald that stream a 160-entry per-scanline
                // table via HBlank DMA (e.g. the cave-flash circle on
                // WIN0H) have their source pointer drift ~68 entries per
                // frame during VBlank, so by the time visible line 0
                // renders, the WIN0H value is already the table's middle
                // entries — the circle equator ends up at the top of the
                // screen instead of the middle.
                if line < VISIBLE_LINES {
                    self.run_dma_for_timing(dma::DmaTiming::HBlank);
                }

                // Schedule end of HBlank (start of next scanline)
                self.scheduler.schedule(Event {
                    fire_time: current_time + HBLANK_CYCLES as u64,
                    kind: EventKind::HBlankEnd,
                });
            }
            EventKind::HBlankEnd => {
                // Clear HBlank flag
                self.bus.io.dispstat &= !0x0002;

                // Advance to next scanline
                self.bus.io.vcount = (self.bus.io.vcount + 1) % LINES_PER_FRAME;
                let line = self.bus.io.vcount;

                // Check VCount match
                let lyc = (self.bus.io.dispstat >> 8) as u16;
                if line == lyc {
                    self.bus.io.dispstat |= 0x0004; // VCount match flag
                    if self.bus.io.dispstat & 0x0020 != 0 {
                        self.bus.interrupt.request_irq(interrupt::Irq::VCountMatch);
                    }
                } else {
                    self.bus.io.dispstat &= !0x0004;
                }

                if line == VISIBLE_LINES {
                    // VBlank begins
                    self.bus.io.dispstat |= 0x0001;
                    self.vblank_entries += 1;
                    // CYCLE_PROFILE: emit per-frame report
                    if std::env::var("CYCLE_PROFILE").is_ok() {
                        let f = PROFILE_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
                        if f % 60 == 0 {
                            cycle_profile_report();
                        }
                    }
                    if self.bus.io.dispstat & 0x0008 != 0 {
                        self.bus.interrupt.request_irq(interrupt::Irq::VBlank);
                        self.vblank_irqs_raised += 1;
                    }
                    // Trigger VBlank DMA
                    self.run_dma_for_timing(dma::DmaTiming::VBlank);
                    // Re-anchor FIFO DMA source on every VBlank. M4A-based
                    // games (Pokémon, etc.) write fresh samples to a fixed
                    // buffer each vblank and expect DMA to keep reading
                    // from the start of that buffer rather than drifting
                    // forward indefinitely. On real hardware this reset
                    // is typically done by the BIOS sound driver or the
                    // game's own in-ROM IRQ handler; we guarantee it here
                    // so that games which assume the behaviour but don't
                    // explicitly call SWI 0x1D still work.
                    // See debug/2026-04-25_pokemon-audio-dma-reanchor.md
                    let dma_audio_trace = std::env::var("DMA_AUDIO_TRACE").is_ok();
                    if dma_audio_trace {
                        let fa = &self.bus.apu.fifo_a;
                        eprintln!(
                            "[FIFO_A] count={} push={} pop={} refill_req={} dropped={}",
                            fa.count, fa.push_count, fa.pop_count,
                            fa.refill_request_count, fa.push_dropped_count
                        );
                        for ch in 0..4 {
                            let c = &self.bus.dma.channels[ch];
                            if c.active {
                                eprintln!(
                                    "[DMA{}] active timing={:?} ctl=0x{:04X} \
                                     sad=0x{:08X} dad=0x{:08X} count=0x{:04X} \
                                     run_count={}",
                                    ch, c.timing(), c.control,
                                    c.sad & 0x07FF_FFFF, c.dad,
                                    c.count, c.run_count
                                );
                            }
                        }
                    }
                    for ch in [1usize, 2] {
                        let c = &mut self.bus.dma.channels[ch];
                        if c.active && matches!(c.timing(), dma::DmaTiming::Special) {
                            if dma_audio_trace {
                                let advance = c.internal_sad
                                    .wrapping_sub(c.sad & 0x07FF_FFFF);
                                eprintln!(
                                    "[DMA{}] vbl: sad=0x{:08X} internal_sad=0x{:08X} \
                                     advanced={} bytes → re-anchor",
                                    ch, c.sad & 0x07FF_FFFF, c.internal_sad,
                                    advance
                                );
                            }
                            c.internal_sad = c.sad & 0x07FF_FFFF;
                        }
                    }
                } else if line == 0 {
                    // VBlank ends, new frame starts
                    self.bus.io.dispstat &= !0x0001;
                }

                // Schedule next HBlank
                self.scheduler.schedule(Event {
                    fire_time: self.scheduler.timestamp() + HDRAW_CYCLES as u64,
                    kind: EventKind::HBlank,
                });
            }
            EventKind::TimerOverflow(_id) => {
                // TODO: Phase 4
            }
            EventKind::DmaComplete(_ch) => {
                // TODO: Phase 4
            }
            EventKind::ApuSample => {
                // TODO: Phase 6
            }
            EventKind::ApuFrameSequencer => {
                // TODO: Phase 6
            }
        }
    }

    /// Tick timers by the given number of CPU cycles.
    fn tick_timers(&mut self, cycles: u32) {
        let result = self.bus.timers.tick(cycles);

        // Fire timer IRQs
        const TIMER_IRQS: [interrupt::Irq; 4] = [
            interrupt::Irq::Timer0,
            interrupt::Irq::Timer1,
            interrupt::Irq::Timer2,
            interrupt::Irq::Timer3,
        ];
        for i in 0..4 {
            if result.irqs[i] {
                self.bus.interrupt.request_irq(TIMER_IRQS[i]);
            }
        }

        // Timer 0/1 overflow: advance FIFO samples and trigger DMA refill.
        //
        // CRITICAL: only fire the DMA channel whose destination address
        // matches the FIFO that requested refill. Without this filter, an
        // empty FIFO_B (which can stay empty if a game uses only FIFO_A)
        // requests refill on every timer overflow → run_dma_for_timing
        // would fire ALL Special-timed channels including DMA1, causing
        // DMA1 to over-push samples to FIFO_A (which is already full).
        // SRTOG hits this — observed DMA1 firing 352 ×/frame instead of
        // the expected 22 ×/frame for its 21024 Hz sample rate.
        const FIFO_A_ADDR: u32 = 0x0400_00A0;
        const FIFO_B_ADDR: u32 = 0x0400_00A4;

        if result.timer0_overflow {
            let (fifo_a_refill, fifo_b_refill) = self.bus.apu.on_timer_overflow(0);
            if fifo_a_refill { self.run_dma_for_fifo(FIFO_A_ADDR); }
            if fifo_b_refill { self.run_dma_for_fifo(FIFO_B_ADDR); }
        }
        if result.timer1_overflow {
            let (fifo_a_refill, fifo_b_refill) = self.bus.apu.on_timer_overflow(1);
            if fifo_a_refill { self.run_dma_for_fifo(FIFO_A_ADDR); }
            if fifo_b_refill { self.run_dma_for_fifo(FIFO_B_ADDR); }
        }
    }

    /// Run DMA channels in Special timing whose destination is the given
    /// FIFO register. Used by FIFO refill triggering so that a low FIFO_A
    /// only fires the DMA writing to FIFO_A, not arbitrary other Special
    /// channels.
    fn run_dma_for_fifo(&mut self, fifo_addr: u32) {
        for ch_id in 0..4 {
            let c = &self.bus.dma.channels[ch_id];
            if c.enabled() && c.active
                && c.timing() == DmaTiming::Special
                && (c.dad & 0x07FF_FFFF) == (fifo_addr & 0x07FF_FFFF)
            {
                let (_cycles, irq) = self.bus.run_dma(ch_id);
                if irq {
                    let irq_type = match ch_id {
                        0 => interrupt::Irq::Dma0,
                        1 => interrupt::Irq::Dma1,
                        2 => interrupt::Irq::Dma2,
                        3 => interrupt::Irq::Dma3,
                        _ => continue,
                    };
                    static DMA_IRQ_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    let trace = *DMA_IRQ_TRACE.get_or_init(|| std::env::var("DMA_IRQ_TRACE").is_ok());
                    if trace {
                        let cyc = GLOBAL_CYCLES.load(Ordering::Relaxed);
                        eprintln!("[DMA-IRQ] ch={ch_id} timing=FIFO cyc={cyc}");
                    }
                    self.bus.interrupt.request_irq(irq_type);
                }
            }
        }
    }

    /// Run all DMA channels that match a given timing trigger.
    fn run_dma_for_timing(&mut self, timing: DmaTiming) {
        let channels = self.bus.dma.channels_for_timing(timing);
        for ch_id in channels {
            let (_cycles, irq) = self.bus.run_dma(ch_id);
            if irq {
                let irq_type = match ch_id {
                    0 => interrupt::Irq::Dma0,
                    1 => interrupt::Irq::Dma1,
                    2 => interrupt::Irq::Dma2,
                    3 => interrupt::Irq::Dma3,
                    _ => continue,
                };
                static DMA_IRQ_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                let trace = *DMA_IRQ_TRACE.get_or_init(|| std::env::var("DMA_IRQ_TRACE").is_ok());
                if trace {
                    let cyc = GLOBAL_CYCLES.load(Ordering::Relaxed);
                    eprintln!("[DMA-IRQ] ch={ch_id} timing={timing:?} cyc={cyc}");
                }
                self.bus.interrupt.request_irq(irq_type);
            }
        }
    }

    /// Handle a SWI (software interrupt).
    /// If no BIOS is loaded, use HLE. Otherwise, jump to the BIOS SWI vector.
    fn handle_swi(&mut self, swi_num: u8) {
        if self.bus.has_bios {
            // Real BIOS: trigger the SWI exception normally
            self.cpu.software_interrupt(swi_num as u32);
        } else {
            // HLE: handle the SWI in Rust
            bios::handle_swi(&mut self.cpu, &mut self.bus, swi_num);
        }
    }

    /// Set the keypad state (active-low: 0 = pressed, 1 = released).
    pub fn set_keypad_state(&mut self, keys: u16) {
        self.bus.keypad.set_keys(keys);
    }

    /// Get a reference to the framebuffer.
    pub fn framebuffer(&self) -> &[u16] {
        &self.frame_buffer
    }

    /// Drain audio samples (interleaved stereo i16) into the output buffer.
    /// Returns the number of samples written.
    pub fn drain_audio(&mut self, out: &mut [i16]) -> usize {
        self.bus.apu.drain_samples(out)
    }

    /// Serialize the entire emulator state to bytes (for save states).
    pub fn save_state(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    /// Deserialize and restore emulator state from bytes.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), bincode::Error> {
        let state: Gba = bincode::deserialize(data)?;
        *self = state;
        Ok(())
    }

    /// Export raw save data (.sav) from the backup media.
    pub fn export_save(&self) -> Option<Vec<u8>> {
        self.bus.backup.to_raw()
    }

    /// Import raw save data (.sav) into the backup media.
    pub fn import_save(&mut self, data: &[u8]) {
        match &mut self.bus.backup {
            backup::BackupMedia::None => {}
            backup::BackupMedia::Sram(s) => {
                let len = data.len().min(s.data.len());
                s.data[..len].copy_from_slice(&data[..len]);
            }
            backup::BackupMedia::Flash(f) => {
                let len = data.len().min(f.data.len());
                f.data[..len].copy_from_slice(&data[..len]);
            }
            backup::BackupMedia::Eeprom(e) => {
                let len = data.len().min(e.data.len());
                e.data[..len].copy_from_slice(&data[..len]);
            }
        }
    }

    /// Step the CPU for N instructions (for testing).
    pub fn step_n(&mut self, n: usize) {
        for _ in 0..n {
            let cycles = self.cpu.step(&mut self.bus) as u64;
            self.scheduler.add_cycles(cycles);
            self.tick_timers(cycles as u32);
            if let Some(swi_num) = self.cpu.pending_swi.take() {
                self.handle_swi(swi_num);
            }
            if self.bus.halt_requested {
                self.bus.halt_requested = false;
                self.cpu.halted = true;
            }
        }
    }

    /// Step one instruction AND process any scheduler events that fire.
    /// Used by tracing diagnostics that need the game to progress normally
    /// (with HBlank/VBlank/DMA/timer events) while logging each instruction.
    pub fn step_one(&mut self) {
        if !self.cpu.halted {
            let cycles = self.cpu.step(&mut self.bus) as u64;
            self.scheduler.add_cycles(cycles);
            self.tick_timers(cycles as u32);
            if let Some(swi_num) = self.cpu.pending_swi.take() {
                self.handle_swi(swi_num);
            }
            if self.bus.halt_requested {
                self.bus.halt_requested = false;
                self.cpu.halted = true;
            }
        } else {
            self.scheduler.add_cycles(1);
        }
        // Dispatch any events that have fired
        while let Some(event) = self.scheduler.pop_if_ready() {
            self.handle_event(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal ARM ROM that sets up Mode 3 and writes a pixel.
    /// This simulates what a real GBA program does on startup.
    fn make_mode3_test_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x100];

        // ARM instructions at 0x08000000:
        // Set up Mode 3 display: MOV R0, #0x0403 (DISPCNT = BG2 enable + Mode 3)
        // Note: we need to write 0x0403 to 0x04000000
        // Step 1: MOV R0, #0x04000000 (I/O base)
        // Step 2: MOV R1, #0x0403 (Mode 3 + BG2 enable)
        // Step 3: STR R1, [R0] (write DISPCNT)
        // Step 4: MOV R0, #0x06000000 (VRAM base)
        // Step 5: MOV R1, #0x001F (red in 15-bit: 0x001F = max red)
        // Step 6: STRH R1, [R0] (write pixel at 0,0)
        // Step 7: B step7 (infinite loop)

        let instructions: &[u32] = &[
            // MOV R0, #0x04000000 (0x04 ROR 8, rotate_field=4)
            0xE3A0_0404,
            // MOV R1, #0x400 (0x04 ROR 24, rotate_field=0xC)
            0xE3A0_1C04,
            // ADD R1, R1, #3 -> R1 = 0x403
            0xE281_1003,
            // STRH R1, [R0, #0] — write DISPCNT (16-bit)
            0xE1C0_10B0,
            // MOV R0, #0x06000000 (0x06 ROR 8, rotate_field=4)
            0xE3A0_0406,
            // MOV R1, #0x1F (red in 15-bit)
            0xE3A0_101F,
            // STRH R1, [R0, #0] — pixel (0,0)
            0xE1C0_10B0,
            // MOV R1, #0x7C00 (blue: 0x1F ROR 22, rotate_field=0xB)
            0xE3A0_1B1F,
            // STRH R1, [R0, #2] — pixel (1,0)
            // Halfword transfer: offset = hi_nibble[11:8] | lo_nibble[3:0] = 0|2 = 2
            0xE1C0_10B2,
            // B . (infinite loop)
            0xEAFF_FFFE,
        ];

        for (i, &inst) in instructions.iter().enumerate() {
            let offset = i * 4;
            rom[offset..offset + 4].copy_from_slice(&inst.to_le_bytes());
        }

        rom
    }

    #[test]
    fn test_mode3_pixel_write() {
        let rom = make_mode3_test_rom();
        let mut gba = Gba::new(None, rom);

        // Skip BIOS — start at ROM entry
        gba.cpu = arm7tdmi::Cpu::new_skip_bios();

        // Run enough instructions to execute the setup code
        gba.step_n(20);

        // Verify DISPCNT was written
        assert_eq!(gba.bus.io.dispcnt, 0x0403, "DISPCNT should be Mode 3 + BG2 enable");

        // Verify VRAM has the pixel data
        let pixel0 = u16::from_le_bytes([gba.bus.vram[0], gba.bus.vram[1]]);
        assert_eq!(pixel0, 0x001F, "Pixel (0,0) should be red (0x001F)");

        let pixel1 = u16::from_le_bytes([gba.bus.vram[2], gba.bus.vram[3]]);
        assert_eq!(pixel1, 0x7C00, "Pixel (1,0) should be blue (0x7C00)");
    }

    #[test]
    fn test_mode3_renders_to_framebuffer() {
        let rom = make_mode3_test_rom();
        let mut gba = Gba::new(None, rom);
        gba.cpu = arm7tdmi::Cpu::new_skip_bios();

        // Run one full frame
        let fb = gba.run_frame();

        // Pixel (0,0) should be red
        assert_eq!(fb[0], 0x001F, "Framebuffer pixel (0,0) should be red");
        // Pixel (1,0) should be blue
        assert_eq!(fb[1], 0x7C00, "Framebuffer pixel (1,0) should be blue");
    }

    #[test]
    fn test_vblank_interrupt() {
        let rom = vec![0u8; 256]; // Empty ROM — CPU will loop on undefined
        let mut gba = Gba::new(None, rom);
        gba.cpu = arm7tdmi::Cpu::new_skip_bios();

        // Enable VBlank IRQ in DISPSTAT
        gba.bus.io.dispstat = 0x0008; // VBlank IRQ enable

        // Enable VBlank in IE
        gba.bus.interrupt.write_ie(0x0001); // VBlank

        // Run a frame
        gba.run_frame();

        // After a frame, VCOUNT should have cycled through 0-227
        // and VBlank IRQ should have been requested
        assert!(gba.bus.interrupt.read_if() & 1 != 0, "VBlank IRQ should be pending");
    }

    /// Regression test: pipeline advance used to happen BEFORE execute, making
    /// branch targets off by 4. Every commercial ROM starts with:
    ///   B +0x200          ; 0x08000000
    ///   ...padding...
    ///   MOV R0, #0x12     ; 0x08000208 — FIQ mode constant
    ///   MSR CPSR_fc, R0   ; 0x0800020C
    /// If the pipeline bug were present, R0 would end up 0x1F instead of 0x12.
    #[test]
    fn test_branch_then_mov_pipeline() {
        // Build a ROM with:
        //   0x08000000: B +0x200 (target = 0x08000208)
        //   0x08000208: MOV R0, #0x12
        //   0x0800020C: B . (infinite loop)
        let mut rom = vec![0u8; 0x300];

        // B +0x200: offset field = (target - (PC+8)) / 4 = (0x208 - 0x8) / 4 = 0x80
        // Opcode: 0xEA000080
        let b_instr: u32 = 0xEA00_0080;
        rom[0..4].copy_from_slice(&b_instr.to_le_bytes());

        // MOV R0, #0x12 at 0x208
        let mov_instr: u32 = 0xE3A0_0012;
        rom[0x208..0x20C].copy_from_slice(&mov_instr.to_le_bytes());

        // B . at 0x20C (branch to self): offset = -2 → field = 0xFFFFFE
        let loop_instr: u32 = 0xEAFF_FFFE;
        rom[0x20C..0x210].copy_from_slice(&loop_instr.to_le_bytes());

        let mut gba = Gba::new(None, rom);
        gba.cpu = arm7tdmi::Cpu::new_skip_bios();

        // Step enough to execute: B (step 0) + MOV (step 1)
        gba.step_n(3);

        // After the MOV executed, R0 should be 0x12 — NOT 0x1F or 0x16 (0x20C word) or other.
        assert_eq!(gba.cpu.regs[0], 0x12,
            "R0 should be 0x12 (MOV R0, #0x12 @ 0x08000208); got 0x{:X}. \
             If this is wrong, pipeline ordering is broken.", gba.cpu.regs[0]);

        // PC should be inside ROM (around 0x0800020C — the B . loop)
        assert!(gba.cpu.regs[15] >= 0x0800_0000 && gba.cpu.regs[15] < 0x0900_0000,
            "PC should stay in ROM; got 0x{:08X}", gba.cpu.regs[15]);
    }

    /// MSR CPSR was being misdecoded as MRS (both share bits[27:20]=0x10 after
    /// masking bit 21 away). This caused MSR to write CPSR into a register
    /// instead of updating CPSR. Every ROM's startup does MSR to set mode.
    #[test]
    fn test_msr_not_decoded_as_mrs() {
        // MOV R0, #0x12       ; load FIQ mode constant
        // MSR CPSR_fc, R0     ; switch to FIQ
        // B .                  ; loop
        let mut rom = vec![0u8; 0x100];
        let mov: u32 = 0xE3A0_0012;
        let msr: u32 = 0xE129_F000;
        let loop_: u32 = 0xEAFF_FFFE;
        rom[0..4].copy_from_slice(&mov.to_le_bytes());
        rom[4..8].copy_from_slice(&msr.to_le_bytes());
        rom[8..12].copy_from_slice(&loop_.to_le_bytes());

        let mut gba = Gba::new(None, rom);
        gba.cpu = arm7tdmi::Cpu::new_skip_bios();
        gba.step_n(2);

        // After MSR, mode should be FIQ (0x12)
        assert_eq!(gba.cpu.cpsr.mode() as u32, 0x12,
            "MSR should have switched to FIQ mode (0x12); got mode=0x{:X}",
            gba.cpu.cpsr.mode() as u32);

        // R0 should still be 0x12 (MSR doesn't modify R0)
        assert_eq!(gba.cpu.regs[0], 0x12,
            "R0 should still be 0x12; got 0x{:X}", gba.cpu.regs[0]);

        // PC should NOT be 0x1F — that was the MRS-writes-to-PC bug symptom
        assert_ne!(gba.cpu.regs[15], 0x1F,
            "PC should not equal CPSR value (indicates MSR decoded as MRS with Rd=PC)");
    }

    /// Pipeline invariant: during ARM execute, regs[15] must equal
    /// the executing instruction's address + 8 (per ARM7TDMI spec).
    #[test]
    fn test_arm_pc_read_during_execute() {
        // MOV R0, PC at 0x08000000
        // Expected: R0 = 0x08000008 (address of MOV + 8)
        let mut rom = vec![0u8; 0x100];
        let mov_pc: u32 = 0xE1A0_000F; // MOV R0, R15
        rom[0..4].copy_from_slice(&mov_pc.to_le_bytes());
        // Follow with B . at 0x04 (offset = -2 field)
        let loop_instr: u32 = 0xEAFF_FFFE;
        rom[4..8].copy_from_slice(&loop_instr.to_le_bytes());

        let mut gba = Gba::new(None, rom);
        gba.cpu = arm7tdmi::Cpu::new_skip_bios();
        gba.step_n(1);

        assert_eq!(gba.cpu.regs[0], 0x0800_0008,
            "R0 should read PC as 0x08000008 (instruction_addr + 8); got 0x{:08X}",
            gba.cpu.regs[0]);
    }
}
