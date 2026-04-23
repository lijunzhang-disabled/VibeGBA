//! Window masking logic.
//!
//! The GBA has three window regions:
//! - **WIN0**: Rectangular region defined by WIN0H (X1, X2) and WIN0V (Y1, Y2)
//! - **WIN1**: Rectangular region defined by WIN1H and WIN1V
//! - **OBJWIN**: Pixels where an OBJ with gfx_mode=2 (OBJ Window) is visible
//!
//! For each pixel, we determine which window region it belongs to (priority: WIN0 > WIN1 > OBJWIN > outside).
//! Each region has a 6-bit control value from WININ/WINOUT that specifies which layers (BG0-3, OBJ)
//! are visible and whether color effects are enabled in that region.
//!
//! If no windows are enabled (DISPCNT bits 13-15 all clear), all layers are visible everywhere.

use crate::bus::io_regs::IoRegisters;
use crate::SCREEN_WIDTH;

/// Per-pixel window flags: which layers are visible and whether effects are enabled.
#[derive(Debug, Clone, Copy)]
pub struct WindowFlags {
    pub bg_enable: [bool; 4],
    pub obj_enable: bool,
    pub effects_enable: bool,
}

impl WindowFlags {
    /// Parse a 6-bit window control value (from WININ/WINOUT).
    fn from_bits(bits: u8) -> Self {
        WindowFlags {
            bg_enable: [
                bits & (1 << 0) != 0,
                bits & (1 << 1) != 0,
                bits & (1 << 2) != 0,
                bits & (1 << 3) != 0,
            ],
            obj_enable: bits & (1 << 4) != 0,
            effects_enable: bits & (1 << 5) != 0,
        }
    }

    /// All layers visible, effects enabled (used when no windows are active).
    pub fn all_enabled() -> Self {
        WindowFlags {
            bg_enable: [true; 4],
            obj_enable: true,
            effects_enable: true,
        }
    }
}

/// Compute window flags for all 240 pixels of a scanline.
/// If no windows are enabled, returns None (meaning all layers visible everywhere).
pub fn compute_window_line(
    line: u16,
    io: &IoRegisters,
    obj_window_line: &[bool; 240],
) -> Option<[WindowFlags; 240]> {
    let dispcnt = io.dispcnt;
    let win0_enabled = dispcnt & (1 << 13) != 0;
    let win1_enabled = dispcnt & (1 << 14) != 0;
    let objwin_enabled = dispcnt & (1 << 15) != 0;

    if !win0_enabled && !win1_enabled && !objwin_enabled {
        return None; // No windows active
    }

    // Parse window control registers
    let win0_flags = WindowFlags::from_bits((io.winin & 0x3F) as u8);
    let win1_flags = WindowFlags::from_bits(((io.winin >> 8) & 0x3F) as u8);
    let outside_flags = WindowFlags::from_bits((io.winout & 0x3F) as u8);
    let objwin_flags = WindowFlags::from_bits(((io.winout >> 8) & 0x3F) as u8);

    // Determine WIN0 vertical range
    let win0_y1 = (io.winv[0] >> 8) as u16;
    let win0_y2 = (io.winv[0] & 0xFF) as u16;
    let win0_in_v = is_in_window_range(line, win0_y1, win0_y2, 160);

    // Determine WIN1 vertical range
    let win1_y1 = (io.winv[1] >> 8) as u16;
    let win1_y2 = (io.winv[1] & 0xFF) as u16;
    let win1_in_v = is_in_window_range(line, win1_y1, win1_y2, 160);

    // WIN0 horizontal range
    let win0_x1 = (io.winh[0] >> 8) as u16;
    let win0_x2 = (io.winh[0] & 0xFF) as u16;

    // WIN1 horizontal range
    let win1_x1 = (io.winh[1] >> 8) as u16;
    let win1_x2 = (io.winh[1] & 0xFF) as u16;

    let mut result = [outside_flags; 240];

    for x in 0..SCREEN_WIDTH {
        let xp = x as u16;

        // Priority: WIN0 > WIN1 > OBJWIN > outside
        if win0_enabled && win0_in_v && is_in_window_range(xp, win0_x1, win0_x2, 240) {
            result[x] = win0_flags;
        } else if win1_enabled && win1_in_v && is_in_window_range(xp, win1_x1, win1_x2, 240) {
            result[x] = win1_flags;
        } else if objwin_enabled && obj_window_line[x] {
            result[x] = objwin_flags;
        }
        // else: stays as outside_flags (default)
    }

    Some(result)
}

/// Check if a coordinate is inside a window range.
/// Window ranges wrap: if Y1 > Y2, the window covers [Y1..max) ∪ [0..Y2).
fn is_in_window_range(coord: u16, start: u16, end: u16, _max: u16) -> bool {
    if start <= end {
        // Normal range: start <= coord < end
        coord >= start && coord < end
    } else {
        // Wrapped range: coord >= start OR coord < end
        coord >= start || coord < end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_flags_from_bits() {
        let flags = WindowFlags::from_bits(0x3F);
        assert!(flags.bg_enable[0]);
        assert!(flags.bg_enable[3]);
        assert!(flags.obj_enable);
        assert!(flags.effects_enable);

        let flags = WindowFlags::from_bits(0x00);
        assert!(!flags.bg_enable[0]);
        assert!(!flags.obj_enable);
        assert!(!flags.effects_enable);
    }

    #[test]
    fn test_no_windows_returns_none() {
        let io = IoRegisters::new();
        let obj_win = [false; 240];
        assert!(compute_window_line(0, &io, &obj_win).is_none());
    }

    #[test]
    fn test_win0_basic() {
        let mut io = IoRegisters::new();
        io.dispcnt = 1 << 13; // WIN0 enabled
        io.winh[0] = (10 << 8) | 50; // X: 10..50
        io.winv[0] = (0 << 8) | 100; // Y: 0..100
        io.winin = 0x3F; // WIN0: all layers + effects
        io.winout = 0x00; // Outside: nothing visible

        let obj_win = [false; 240];
        let result = compute_window_line(5, &io, &obj_win).unwrap();

        // Inside WIN0 (x=20)
        assert!(result[20].bg_enable[0]);
        assert!(result[20].effects_enable);

        // Outside WIN0 (x=5)
        assert!(!result[5].bg_enable[0]);
        assert!(!result[5].effects_enable);
    }

    #[test]
    fn test_window_range_wrap() {
        // Wrapped range: start > end means [start..max) ∪ [0..end)
        assert!(is_in_window_range(250, 200, 50, 256));
        assert!(is_in_window_range(10, 200, 50, 256));
        assert!(!is_in_window_range(100, 200, 50, 256));
    }
}
