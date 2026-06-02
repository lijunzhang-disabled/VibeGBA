use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DmaTiming {
    Immediate = 0,
    VBlank = 1,
    HBlank = 2,
    Special = 3, // FIFO for DMA1/2, Video Capture for DMA3
}

/// Address control modes for source/destination.
#[derive(Debug, Clone, Copy)]
pub enum AddrControl {
    Increment = 0,
    Decrement = 1,
    Fixed = 2,
    IncrementReload = 3, // Increment, but reload on repeat (dest only)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmaChannel {
    /// Source address register (written by CPU)
    pub sad: u32,
    /// Destination address register (written by CPU)
    pub dad: u32,
    /// Word count register (written by CPU)
    pub count: u16,
    /// Control register
    pub control: u16,
    /// Internal source address (latched on enable)
    pub internal_sad: u32,
    /// Internal destination address (latched on enable)
    pub internal_dad: u32,
    /// Internal word count (latched on enable)
    pub internal_count: u32,
    /// Whether this channel is currently active (enabled + timing matched)
    pub active: bool,
    /// Diagnostic: cumulative count of run_dma calls. Useful for tracking
    /// whether FIFO DMAs fire at the expected rate.
    #[serde(default, skip)]
    pub run_count: u64,
    /// Scheduler timestamp at which `internal_sad` was last latched (i.e.,
    /// the most recent 0→1 transition of the enable bit). Used by the
    /// VBlank FIFO-DMA re-anchor to distinguish games that drive their
    /// own DMA refresh (like velipso's timer-IRQ buffer swap) from games
    /// that don't (like Pokémon Emerald). See [[fifo-dma-vblank]].
    #[serde(default)]
    pub last_latch_cycle: u64,
}

impl DmaChannel {
    pub fn new() -> Self {
        DmaChannel {
            sad: 0,
            dad: 0,
            count: 0,
            control: 0,
            internal_sad: 0,
            internal_dad: 0,
            internal_count: 0,
            active: false,
            run_count: 0,
            last_latch_cycle: 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.control & (1 << 15) != 0
    }

    pub fn timing(&self) -> DmaTiming {
        match (self.control >> 12) & 3 {
            0 => DmaTiming::Immediate,
            1 => DmaTiming::VBlank,
            2 => DmaTiming::HBlank,
            3 => DmaTiming::Special,
            _ => unreachable!(),
        }
    }

    pub fn word_size_32(&self) -> bool {
        self.control & (1 << 10) != 0
    }

    pub fn repeat(&self) -> bool {
        self.control & (1 << 9) != 0
    }

    pub fn irq_enabled(&self) -> bool {
        self.control & (1 << 14) != 0
    }

    pub fn dest_control(&self) -> AddrControl {
        match (self.control >> 5) & 3 {
            0 => AddrControl::Increment,
            1 => AddrControl::Decrement,
            2 => AddrControl::Fixed,
            3 => AddrControl::IncrementReload,
            _ => unreachable!(),
        }
    }

    pub fn src_control(&self) -> AddrControl {
        match (self.control >> 7) & 3 {
            0 => AddrControl::Increment,
            1 => AddrControl::Decrement,
            2 => AddrControl::Fixed,
            3 => AddrControl::Increment, // 3 is prohibited for source, treat as increment
            _ => unreachable!(),
        }
    }

    /// Latch internal registers from the written values.
    /// Called when the enable bit transitions from 0 to 1.
    fn latch(&mut self, channel_id: usize) {
        // Source address masks: DMA0-2 = 27 bits, DMA3 = 28 bits
        self.internal_sad = match channel_id {
            0..=2 => self.sad & 0x07FF_FFFF,
            3 => self.sad & 0x0FFF_FFFF,
            _ => self.sad,
        };

        // Destination address masks: DMA0-2 = 27 bits, DMA3 = 28 bits
        self.internal_dad = match channel_id {
            0..=2 => self.dad & 0x07FF_FFFF,
            3 => self.dad & 0x0FFF_FFFF,
            _ => self.dad,
        };

        // Word count: DMA0-2 = 14 bits (max 0x4000), DMA3 = 16 bits (max 0x10000)
        let raw_count = self.count as u32;
        self.internal_count = if raw_count == 0 {
            match channel_id {
                0..=2 => 0x4000,
                3 => 0x10000,
                _ => 0x10000,
            }
        } else {
            raw_count
        };
    }

    /// Reload destination and count for repeat transfers.
    pub(crate) fn reload_for_repeat(&mut self, channel_id: usize) {
        // Reload destination if dest_control is IncrementReload
        if let AddrControl::IncrementReload = self.dest_control() {
            self.internal_dad = match channel_id {
                0..=2 => self.dad & 0x07FF_FFFF,
                3 => self.dad & 0x0FFF_FFFF,
                _ => self.dad,
            };
        }

        // Always reload count
        let raw_count = self.count as u32;
        self.internal_count = if raw_count == 0 {
            match channel_id {
                0..=2 => 0x4000,
                3 => 0x10000,
                _ => 0x10000,
            }
        } else {
            raw_count
        };
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmaController {
    pub channels: [DmaChannel; 4],
}

impl DmaController {
    pub fn new() -> Self {
        DmaController {
            channels: [
                DmaChannel::new(),
                DmaChannel::new(),
                DmaChannel::new(),
                DmaChannel::new(),
            ],
        }
    }

    /// Write to a DMA channel's control register.
    /// Handles the enable-bit 0→1 transition (latch addresses + fire immediate).
    /// `now` is the scheduler timestamp at the write — recorded on each
    /// 0→1 latch and consulted by the VBlank FIFO re-anchor.
    /// Returns Some(channel_id) if an immediate DMA should run now.
    pub fn write_control(&mut self, channel_id: usize, value: u16, now: u64) -> Option<usize> {
        let old_enabled = self.channels[channel_id].enabled();
        self.channels[channel_id].control = value;
        let new_enabled = self.channels[channel_id].enabled();

        if !old_enabled && new_enabled {
            // Enable bit went 0→1: latch addresses
            self.channels[channel_id].latch(channel_id);
            self.channels[channel_id].active = true;
            self.channels[channel_id].last_latch_cycle = now;

            if self.channels[channel_id].timing() == DmaTiming::Immediate {
                return Some(channel_id);
            }
        } else if !new_enabled {
            self.channels[channel_id].active = false;
        }

        None
    }

    /// Check which channels should fire for a given timing trigger.
    /// Returns channel IDs in priority order (0 first).
    pub fn channels_for_timing(&self, timing: DmaTiming) -> Vec<usize> {
        let mut result = Vec::new();
        for i in 0..4 {
            if self.channels[i].enabled() && self.channels[i].active
                && self.channels[i].timing() == timing
            {
                result.push(i);
            }
        }
        result
    }
}

/// Execute a DMA transfer for a single channel.
/// Reads from `memory` via closures to avoid borrow issues.
/// Returns the number of cycles consumed (approximate).
pub struct DmaTransferResult {
    pub cycles: u32,
    pub irq: bool,
    pub channel_disabled: bool,
}

/// Perform one DMA transfer. This is called from the Bus context where
/// we have access to memory. We pass in read/write functions to avoid
/// needing &mut Bus in two places.
pub fn execute_dma_transfer(channel: &mut DmaChannel, channel_id: usize, memory: &mut DmaMemory) -> DmaTransferResult {
    let word_size = if channel.word_size_32() { 4u32 } else { 2u32 };
    let count = channel.internal_count;

    let src_step = match channel.src_control() {
        AddrControl::Increment | AddrControl::IncrementReload => word_size as i32,
        AddrControl::Decrement => -(word_size as i32),
        AddrControl::Fixed => 0,
    };

    let dst_step = match channel.dest_control() {
        AddrControl::Increment | AddrControl::IncrementReload => word_size as i32,
        AddrControl::Decrement => -(word_size as i32),
        AddrControl::Fixed => 0,
    };

    // Special case: FIFO mode for DMA1/2 (timing=Special)
    let is_fifo = (channel_id == 1 || channel_id == 2)
        && channel.timing() == DmaTiming::Special;

    if is_fifo {
        // FIFO DMA: transfer 4 words (16 bytes) to the FIFO address
        // Destination is fixed (FIFO register), source increments
        for _ in 0..4 {
            let val = memory.read32(channel.internal_sad);
            memory.write32(channel.internal_dad, val);
            channel.internal_sad = channel.internal_sad.wrapping_add(4);
        }
        // FIFO DMA always repeats, never disables
        return DmaTransferResult {
            cycles: 4,
            irq: channel.irq_enabled(),
            channel_disabled: false,
        };
    }

    // Normal transfer
    for _ in 0..count {
        if word_size == 4 {
            let val = memory.read32(channel.internal_sad & !3);
            memory.write32(channel.internal_dad & !3, val);
        } else {
            let val = memory.read16(channel.internal_sad & !1);
            memory.write16(channel.internal_dad & !1, val);
        }

        channel.internal_sad = (channel.internal_sad as i32).wrapping_add(src_step) as u32;
        channel.internal_dad = (channel.internal_dad as i32).wrapping_add(dst_step) as u32;
    }

    let irq = channel.irq_enabled();

    // After transfer: check repeat
    let channel_disabled = if channel.repeat() && channel.timing() != DmaTiming::Immediate {
        // Repeat: reload count (and dest if IncrementReload), stay enabled
        channel.reload_for_repeat(channel_id);
        false
    } else {
        // One-shot or immediate: disable the channel
        channel.control &= !(1 << 15);
        channel.active = false;
        true
    };

    DmaTransferResult {
        cycles: count,
        irq,
        channel_disabled,
    }
}

/// Trait-like struct for DMA memory access, avoiding circular Bus borrows.
pub struct DmaMemory<'a> {
    // We store function pointers/closures for memory access
    pub read16_fn: &'a dyn Fn(u32) -> u16,
    pub read32_fn: &'a dyn Fn(u32) -> u32,
    pub write16_fn: &'a mut dyn FnMut(u32, u16),
    pub write32_fn: &'a mut dyn FnMut(u32, u32),
}

impl<'a> DmaMemory<'a> {
    pub fn read16(&self, addr: u32) -> u16 {
        (self.read16_fn)(addr)
    }
    pub fn read32(&self, addr: u32) -> u32 {
        (self.read32_fn)(addr)
    }
    pub fn write16(&mut self, addr: u32, val: u16) {
        (self.write16_fn)(addr, val)
    }
    pub fn write32(&mut self, addr: u32, val: u32) {
        (self.write32_fn)(addr, val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dma_channel_latch() {
        let mut dma = DmaController::new();
        // Use DMA3 which has 28-bit address range (can access ROM)
        dma.channels[3].sad = 0x0800_0000;
        dma.channels[3].dad = 0x0600_0000;
        dma.channels[3].count = 100;

        // Enable with immediate timing
        let result = dma.write_control(3, 1 << 15, 0); // Enable, immediate
        assert_eq!(result, Some(3));
        assert_eq!(dma.channels[3].internal_sad, 0x0800_0000);
        assert_eq!(dma.channels[3].internal_dad, 0x0600_0000);
        assert_eq!(dma.channels[3].internal_count, 100);
    }

    #[test]
    fn test_dma_count_zero_means_max() {
        let mut dma = DmaController::new();
        dma.channels[0].count = 0;
        let _ = dma.write_control(0, 1 << 15, 0);
        assert_eq!(dma.channels[0].internal_count, 0x4000); // DMA0 max

        dma.channels[3].count = 0;
        let _ = dma.write_control(3, 1 << 15, 0);
        assert_eq!(dma.channels[3].internal_count, 0x10000); // DMA3 max
    }

    #[test]
    fn test_dma_timing_detection() {
        let mut ch = DmaChannel::new();
        ch.control = (1 << 15) | (1 << 12); // Enable, VBlank timing
        assert_eq!(ch.timing(), DmaTiming::VBlank);

        ch.control = (1 << 15) | (2 << 12); // Enable, HBlank timing
        assert_eq!(ch.timing(), DmaTiming::HBlank);
    }

    #[test]
    fn test_dma_channels_for_timing() {
        let mut dma = DmaController::new();
        // Channel 0: VBlank
        dma.channels[0].sad = 0x0800_0000;
        dma.channels[0].dad = 0x0600_0000;
        dma.channels[0].count = 10;
        dma.write_control(0, (1 << 15) | (1 << 12), 0); // VBlank

        // Channel 2: HBlank
        dma.channels[2].sad = 0x0800_0000;
        dma.channels[2].dad = 0x0600_0000;
        dma.channels[2].count = 10;
        dma.write_control(2, (1 << 15) | (2 << 12), 0); // HBlank

        assert_eq!(dma.channels_for_timing(DmaTiming::VBlank), vec![0]);
        assert_eq!(dma.channels_for_timing(DmaTiming::HBlank), vec![2]);
        assert!(dma.channels_for_timing(DmaTiming::Immediate).is_empty());
    }
}
