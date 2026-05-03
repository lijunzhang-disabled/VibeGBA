use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

fn timer_trace_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("TIMER_TRACE").is_ok())
}

/// Prescaler dividers: F/1, F/64, F/256, F/1024
const PRESCALER_DIVIDERS: [u32; 4] = [1, 64, 256, 1024];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timer {
    /// Counter/Reload value (written value is reload, read value is counter)
    pub reload: u16,
    /// Current counter value
    pub counter: u16,
    /// Control register (TMCNT_H)
    pub control: u16,
    /// Internal fractional cycle accumulator for prescaler
    pub(crate) prescaler_counter: u32,
}

impl Timer {
    pub fn new() -> Self {
        Timer {
            reload: 0,
            counter: 0,
            control: 0,
            prescaler_counter: 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.control & (1 << 7) != 0
    }

    pub fn cascade(&self) -> bool {
        self.control & (1 << 2) != 0
    }

    pub fn irq_enabled(&self) -> bool {
        self.control & (1 << 6) != 0
    }

    pub fn prescaler(&self) -> u32 {
        PRESCALER_DIVIDERS[(self.control & 3) as usize]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timers {
    pub timers: [Timer; 4],
}

/// Result of ticking timers: which timer IRQs should fire.
pub struct TimerTickResult {
    pub irqs: [bool; 4],
    /// Timer 0 or 1 overflowed — may need to trigger FIFO DMA
    pub timer0_overflow: bool,
    pub timer1_overflow: bool,
}

impl Timers {
    pub fn new() -> Self {
        Timers {
            timers: [Timer::new(), Timer::new(), Timer::new(), Timer::new()],
        }
    }

    /// Read timer counter value.
    pub fn read_counter(&self, id: usize) -> u16 {
        self.timers[id].counter
    }

    /// Write timer reload value.
    pub fn write_reload(&mut self, id: usize, value: u16) {
        self.timers[id].reload = value;
    }

    /// Write timer control. Handles start bit transition (reload on 0->1).
    pub fn write_control(&mut self, id: usize, value: u16) {
        let old_enabled = self.timers[id].enabled();
        self.timers[id].control = value;
        let new_enabled = self.timers[id].enabled();

        // Reload counter when start bit goes from 0 to 1
        if !old_enabled && new_enabled {
            self.timers[id].counter = self.timers[id].reload;
            self.timers[id].prescaler_counter = 0;
            if timer_trace_enabled() {
                let reload = self.timers[id].reload as u32;
                let prescaler = self.timers[id].prescaler();
                // Cycles per overflow = (0x10000 - reload) * prescaler.
                // Sample rate = CPU_CLOCK / cycles_per_overflow.
                let cycles_per = (0x10000u32 - reload) * prescaler;
                let rate_hz = if cycles_per > 0 { 16_777_216 / cycles_per } else { 0 };
                eprintln!(
                    "[TIMER] T{} start: reload=0x{:04X} prescaler={} → cycles/overflow={} → {} Hz",
                    id, reload, prescaler, cycles_per, rate_hz
                );
            }
        }
    }

    /// Tick all timers by the given number of CPU cycles.
    /// Returns which timers overflowed (for IRQ and FIFO triggering).
    pub fn tick(&mut self, cycles: u32) -> TimerTickResult {
        let mut result = TimerTickResult {
            irqs: [false; 4],
            timer0_overflow: false,
            timer1_overflow: false,
        };

        // Process timers 0-3 in order (cascade flows upward)
        let mut prev_overflow = false;

        for i in 0..4 {
            if !self.timers[i].enabled() {
                prev_overflow = false;
                continue;
            }

            let overflows = if self.timers[i].cascade() && i > 0 {
                // Cascade mode: increment by number of times previous timer overflowed
                if prev_overflow {
                    self.increment_timer(i, 1)
                } else {
                    0
                }
            } else {
                // Normal mode: tick by prescaled cycles
                let prescaler = self.timers[i].prescaler();
                self.timers[i].prescaler_counter += cycles;
                let ticks = self.timers[i].prescaler_counter / prescaler;
                self.timers[i].prescaler_counter %= prescaler;

                if ticks > 0 {
                    self.increment_timer(i, ticks)
                } else {
                    0
                }
            };

            prev_overflow = overflows > 0;

            if prev_overflow {
                if self.timers[i].irq_enabled() {
                    result.irqs[i] = true;
                }
                if i == 0 {
                    result.timer0_overflow = true;
                }
                if i == 1 {
                    result.timer1_overflow = true;
                }
            }
        }

        result
    }

    /// Cycles until the next overflow of T0 or T1 (whichever is sooner).
    /// Returns u32::MAX if neither timer is enabled.
    /// Used by halt-period sub-stepping so we can place FIFO sample-pops
    /// at exactly the right cycle within an idle span.
    pub fn cycles_to_next_fifo_overflow(&self) -> u32 {
        let mut best = u32::MAX;
        for i in 0..2usize {
            let t = &self.timers[i];
            if !t.enabled() || (t.cascade() && i > 0) { continue; }
            let prescaler = t.prescaler();
            // Cycles until next timer increment
            let to_next_tick = prescaler - t.prescaler_counter;
            // Plus ticks until counter wraps to reload (i.e., 0x10000)
            let ticks_to_overflow = (0x10000u32 - t.counter as u32).saturating_sub(1);
            let cycles = to_next_tick + ticks_to_overflow.saturating_mul(prescaler);
            if cycles < best { best = cycles; }
        }
        best
    }

    /// Increment a timer's counter by `ticks`. Returns the number of overflows.
    fn increment_timer(&mut self, id: usize, ticks: u32) -> u32 {
        let counter = self.timers[id].counter as u32;
        let reload = self.timers[id].reload as u32;
        let max = 0x10000u32; // 16-bit counter wraps at 0x10000

        let total = counter + ticks;

        if total >= max {
            // Calculate how many times we overflow
            let range = max - reload; // ticks per overflow cycle
            if range == 0 {
                // Reload == 0xFFFF: overflows every tick
                self.timers[id].counter = reload as u16;
                return ticks;
            }
            let remaining = total - max;
            let extra_overflows = remaining / range;
            let final_counter = reload + (remaining % range);
            self.timers[id].counter = final_counter as u16;
            1 + extra_overflows
        } else {
            self.timers[id].counter = total as u16;
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timer_basic_tick() {
        let mut timers = Timers::new();
        // Timer 0: reload=0xFFF0, prescaler=1, enabled
        timers.write_reload(0, 0xFFF0);
        timers.write_control(0, 1 << 7); // Enable

        assert_eq!(timers.timers[0].counter, 0xFFF0);

        // Tick 10 cycles — counter should advance to 0xFFFA
        let result = timers.tick(10);
        assert_eq!(timers.timers[0].counter, 0xFFFA);
        assert!(!result.irqs[0]); // No overflow yet
    }

    #[test]
    fn test_timer_overflow() {
        let mut timers = Timers::new();
        timers.write_reload(0, 0xFFF0);
        timers.write_control(0, (1 << 7) | (1 << 6)); // Enable + IRQ

        // Tick 20 cycles — should overflow (0xFFF0 + 20 = 0x10004 → reload + 4)
        let result = timers.tick(20);
        assert_eq!(timers.timers[0].counter, 0xFFF4); // Reloaded + 4
        assert!(result.irqs[0]);
        assert!(result.timer0_overflow);
    }

    #[test]
    fn test_timer_prescaler() {
        let mut timers = Timers::new();
        timers.write_reload(0, 0);
        timers.write_control(0, (1 << 7) | 1); // Enable, prescaler=64

        // 63 cycles: not enough for a tick
        timers.tick(63);
        assert_eq!(timers.timers[0].counter, 0);

        // 1 more cycle (total 64): one tick
        timers.tick(1);
        assert_eq!(timers.timers[0].counter, 1);
    }

    #[test]
    fn test_timer_cascade() {
        let mut timers = Timers::new();
        // Timer 0: will overflow quickly
        timers.write_reload(0, 0xFFFF);
        timers.write_control(0, 1 << 7); // Enable, prescaler=1

        // Timer 1: cascade from timer 0
        timers.write_reload(1, 0);
        timers.write_control(1, (1 << 7) | (1 << 2)); // Enable + cascade

        assert_eq!(timers.timers[1].counter, 0);

        // Tick 1: timer 0 overflows (0xFFFF + 1 = 0x10000)
        let result = timers.tick(1);
        assert!(result.timer0_overflow);
        // Timer 1 should have incremented by cascade
        assert_eq!(timers.timers[1].counter, 1);
    }

    #[test]
    fn test_timer_reload_on_enable() {
        let mut timers = Timers::new();
        timers.write_reload(0, 0x1234);

        // Counter starts at 0
        assert_eq!(timers.timers[0].counter, 0);

        // Enable: counter reloads
        timers.write_control(0, 1 << 7);
        assert_eq!(timers.timers[0].counter, 0x1234);
    }
}
