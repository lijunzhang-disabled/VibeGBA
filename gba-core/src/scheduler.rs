use serde::{Deserialize, Serialize};
use std::collections::BinaryHeap;
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventKind {
    HBlank,
    HBlankEnd,
    TimerOverflow(u8),
    DmaComplete(u8),
    ApuSample,
    ApuFrameSequencer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
pub struct Event {
    pub fire_time: u64,
    pub kind: EventKind,
}

// BinaryHeap is a max-heap, so we reverse the ordering to get a min-heap.
impl Ord for Event {
    fn cmp(&self, other: &Self) -> Ordering {
        other.fire_time.cmp(&self.fire_time)
    }
}

impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Serialize, Deserialize)]
pub struct Scheduler {
    timestamp: u64,
    events: BinaryHeap<Event>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            timestamp: 0,
            events: BinaryHeap::new(),
        }
    }

    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    pub fn add_cycles(&mut self, cycles: u64) {
        self.timestamp += cycles;
    }

    pub fn advance_to(&mut self, time: u64) {
        self.timestamp = time;
    }

    pub fn schedule(&mut self, event: Event) {
        self.events.push(event);
    }

    /// Peek at the earliest event time without removing it.
    pub fn peek_time(&self) -> Option<u64> {
        self.events.peek().map(|e| e.fire_time)
    }

    /// Pop the next event if its fire time <= current timestamp.
    pub fn pop_if_ready(&mut self) -> Option<Event> {
        if let Some(event) = self.events.peek() {
            if event.fire_time <= self.timestamp {
                return self.events.pop();
            }
        }
        None
    }

    /// Remove all events of a given kind.
    pub fn cancel(&mut self, kind: EventKind) {
        let remaining: Vec<Event> = self.events.drain().filter(|e| e.kind != kind).collect();
        self.events.extend(remaining);
    }
}
