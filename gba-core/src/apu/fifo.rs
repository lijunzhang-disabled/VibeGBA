//! DMA FIFO sound channels A and B.
//!
//! Each channel has a 32-byte (8-sample x 4-byte) FIFO buffer.
//! Samples are 8-bit signed, played at the rate of Timer 0 or Timer 1.
//! When the FIFO has ≤16 bytes remaining, it requests a DMA refill
//! (4 x 32-bit = 16 bytes transferred per DMA request).

use serde::{Deserialize, Serialize};

const FIFO_CAPACITY: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FifoChannel {
    /// Circular buffer of signed 8-bit samples
    buffer: [i8; FIFO_CAPACITY],
    /// Read position
    read_pos: usize,
    /// Write position
    write_pos: usize,
    /// Number of samples in buffer
    count: usize,
    /// Current sample being played (latched on timer tick)
    pub current_sample: i8,
    /// Which timer drives this channel (0 or 1)
    pub timer_select: u8,
    /// Enable left/right output
    pub enable_left: bool,
    pub enable_right: bool,
    /// Volume: false = 50%, true = 100%
    pub volume_full: bool,
}

impl FifoChannel {
    pub fn new() -> Self {
        FifoChannel {
            buffer: [0; FIFO_CAPACITY],
            read_pos: 0,
            write_pos: 0,
            count: 0,
            current_sample: 0,
            timer_select: 0,
            enable_left: false,
            enable_right: false,
            volume_full: false,
        }
    }

    /// Reset the FIFO buffer (called on channel enable or specific writes).
    pub fn reset(&mut self) {
        self.read_pos = 0;
        self.write_pos = 0;
        self.count = 0;
        self.current_sample = 0;
    }

    /// Write a 32-bit value (4 samples) into the FIFO.
    /// Called by DMA or CPU writes to FIFO_A/FIFO_B register.
    pub fn write32(&mut self, value: u32) {
        let bytes = value.to_le_bytes();
        for &byte in &bytes {
            if self.count < FIFO_CAPACITY {
                self.buffer[self.write_pos] = byte as i8;
                self.write_pos = (self.write_pos + 1) % FIFO_CAPACITY;
                self.count += 1;
            }
        }
    }

    /// Pop one sample from the FIFO (called on timer overflow).
    /// Returns true if FIFO needs a DMA refill (count <= 16).
    pub fn pop_sample(&mut self) -> bool {
        if self.count > 0 {
            self.current_sample = self.buffer[self.read_pos];
            self.read_pos = (self.read_pos + 1) % FIFO_CAPACITY;
            self.count -= 1;
        }
        // Request DMA refill when half empty
        self.count <= 16
    }

    /// Get the current output sample scaled to the mixer range.
    /// Returns a value in roughly -128..127 range.
    pub fn output(&self) -> i16 {
        let sample = self.current_sample as i16;
        if self.volume_full { sample } else { sample / 2 }
    }

    pub fn len(&self) -> usize {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fifo_write_and_pop() {
        let mut fifo = FifoChannel::new();
        fifo.volume_full = true;

        // Write 4 samples
        fifo.write32(0x01020304); // LE: bytes 04, 03, 02, 01
        assert_eq!(fifo.len(), 4);

        // Pop samples
        fifo.pop_sample();
        assert_eq!(fifo.current_sample, 0x04);
        fifo.pop_sample();
        assert_eq!(fifo.current_sample, 0x03);
        fifo.pop_sample();
        assert_eq!(fifo.current_sample, 0x02);
        fifo.pop_sample();
        assert_eq!(fifo.current_sample, 0x01);
        assert_eq!(fifo.len(), 0);
    }

    #[test]
    fn test_fifo_reset() {
        let mut fifo = FifoChannel::new();
        fifo.write32(0xDEADBEEF);
        assert_eq!(fifo.len(), 4);
        fifo.reset();
        assert_eq!(fifo.len(), 0);
    }

    #[test]
    fn test_fifo_refill_request() {
        let mut fifo = FifoChannel::new();
        // Fill with 32 bytes (8 writes of 4 bytes)
        for i in 0..8 {
            fifo.write32(i);
        }
        assert_eq!(fifo.len(), 32);

        // Pop until half empty — should not request refill until count <= 16
        for _ in 0..15 {
            let needs_refill = fifo.pop_sample();
            assert!(!needs_refill);
        }
        // Next pop brings count to 16 → request refill
        let needs_refill = fifo.pop_sample();
        assert!(needs_refill);
    }
}
