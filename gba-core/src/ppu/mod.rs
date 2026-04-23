pub mod bg;
pub mod obj;
pub mod window;
pub mod effects;

use crate::bus::io_regs::IoRegisters;
use crate::SCREEN_WIDTH;
use effects::{BlendMode, alpha_blend, brightness_decrease, brightness_increase, is_first_target, is_second_target};
use window::WindowFlags;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PixelInfo {
    pub color: u16,
    pub priority: u8,
    /// Which layer produced this pixel (0-3 for BG, 4 for OBJ, 5 for backdrop)
    pub layer: u8,
    /// Whether this OBJ pixel is semi-transparent (gfx_mode=1)
    pub semi_transparent: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Ppu {
    /// Internal affine reference point registers for BG2/BG3.
    pub bg2_ref_x: i32,
    pub bg2_ref_y: i32,
    pub bg3_ref_x: i32,
    pub bg3_ref_y: i32,
}

impl Ppu {
    pub fn new() -> Self {
        Ppu {
            bg2_ref_x: 0,
            bg2_ref_y: 0,
            bg3_ref_x: 0,
            bg3_ref_y: 0,
        }
    }

    /// Render one scanline into the framebuffer.
    pub fn render_scanline(
        &mut self,
        line: u16,
        io: &IoRegisters,
        palette: &[u8],
        vram: &[u8],
        oam: &[u8],
        framebuffer: &mut [u16],
    ) {
        let dispcnt = io.dispcnt;
        let mode = dispcnt & 0x07;
        let forced_blank = dispcnt & (1 << 7) != 0;
        let row_start = line as usize * SCREEN_WIDTH;

        if forced_blank {
            for x in 0..SCREEN_WIDTH {
                framebuffer[row_start + x] = 0x7FFF;
            }
            return;
        }

        match mode {
            0 => self.render_tile_mode(line, io, palette, vram, oam, framebuffer, &[0, 1, 2, 3], &[]),
            1 => self.render_tile_mode(line, io, palette, vram, oam, framebuffer, &[0, 1], &[2]),
            2 => self.render_tile_mode(line, io, palette, vram, oam, framebuffer, &[], &[2, 3]),
            3 => self.render_mode3(line, vram, framebuffer),
            4 => self.render_mode4(line, io, palette, vram, framebuffer),
            5 => self.render_mode5(line, io, vram, framebuffer),
            _ => {
                for x in 0..SCREEN_WIDTH {
                    framebuffer[row_start + x] = 0;
                }
            }
        }

        // Update affine reference points for next scanline
        if mode == 1 || mode == 2 {
            self.advance_affine_refs(io);
        }
    }

    /// Render a tile-based mode (0, 1, or 2) with full compositing, windows, and effects.
    fn render_tile_mode(
        &mut self,
        line: u16,
        io: &IoRegisters,
        palette: &[u8],
        vram: &[u8],
        oam: &[u8],
        framebuffer: &mut [u16],
        text_bgs: &[usize],
        affine_bgs: &[usize],
    ) {
        let dispcnt = io.dispcnt;
        let row_start = line as usize * SCREEN_WIDTH;

        let backdrop = if palette.len() >= 2 {
            u16::from_le_bytes([palette[0], palette[1]]) & 0x7FFF
        } else {
            0
        };

        // Render each enabled BG layer
        let mut bg_lines: [Option<[Option<PixelInfo>; 240]>; 4] = [None, None, None, None];

        for &bgi in text_bgs {
            if dispcnt & (1 << (8 + bgi)) != 0 {
                let mut line_buf = [None; 240];
                bg::render_text_bg_line(bgi, line, io, palette, vram, &mut line_buf);
                bg_lines[bgi] = Some(line_buf);
            }
        }

        for &bgi in affine_bgs {
            if dispcnt & (1 << (8 + bgi)) != 0 {
                let mut line_buf = [None; 240];
                let (ref_x, ref_y) = if bgi == 2 {
                    (self.bg2_ref_x, self.bg2_ref_y)
                } else {
                    (self.bg3_ref_x, self.bg3_ref_y)
                };
                bg::render_affine_bg_line(bgi, line, io, palette, vram, ref_x, ref_y, &mut line_buf);
                bg_lines[bgi] = Some(line_buf);
            }
        }

        // Render OBJ layer + OBJ window mask
        let mut obj_line: [Option<PixelInfo>; 240] = [None; 240];
        let obj_window_mask = [false; 240];
        let obj_enabled = dispcnt & (1 << 12) != 0;
        if obj_enabled {
            obj::render_obj_line(line, io, palette, vram, oam, &mut obj_line);
            // TODO: build obj_window_mask from gfx_mode=2 sprites
        }

        // Compute window flags
        let window_line = window::compute_window_line(line, io, &obj_window_mask);

        // Blending parameters
        let bldcnt = io.bldcnt;
        let blend_mode = BlendMode::from_bldcnt(bldcnt);
        let eva = (io.bldalpha & 0x1F).min(16);
        let evb = ((io.bldalpha >> 8) & 0x1F).min(16);
        let evy = (io.bldy & 0x1F).min(16);

        // Composite each pixel
        for x in 0..SCREEN_WIDTH {
            let win_flags = match &window_line {
                Some(wl) => wl[x],
                None => WindowFlags::all_enabled(),
            };

            framebuffer[row_start + x] = self.composite_pixel_with_effects(
                x, &bg_lines, &obj_line, backdrop,
                &win_flags, bldcnt, blend_mode, eva, evb, evy,
            );
        }
    }

    /// Composite a single pixel with window masking and color effects.
    fn composite_pixel_with_effects(
        &self,
        x: usize,
        bg_lines: &[Option<[Option<PixelInfo>; 240]>; 4],
        obj_line: &[Option<PixelInfo>; 240],
        backdrop: u16,
        win_flags: &WindowFlags,
        bldcnt: u16,
        blend_mode: BlendMode,
        eva: u16,
        evb: u16,
        evy: u16,
    ) -> u16 {
        // Build a sorted list of opaque pixels at this X, filtered by window visibility.
        // We need the top two for alpha blending.
        // Sorted by: priority ASC, then OBJ before BG at same priority, then BG index ASC.

        let mut top: Option<PixelInfo> = None;
        let mut second: Option<PixelInfo> = None;

        // Helper: try to insert a pixel into top/second
        let mut try_insert = |px: PixelInfo| {
            let dominated = match &top {
                None => false,
                Some(t) => {
                    if px.priority < t.priority {
                        false // px has higher priority
                    } else if px.priority == t.priority {
                        if px.layer == 4 && t.layer != 4 {
                            false // OBJ beats BG at same priority
                        } else if px.layer != 4 && t.layer == 4 {
                            true // BG loses to OBJ at same priority
                        } else {
                            px.layer >= t.layer // Lower layer index wins
                        }
                    } else {
                        true
                    }
                }
            };

            if !dominated {
                second = top;
                top = Some(px);
            } else if second.is_none() || second.map_or(false, |s| {
                px.priority < s.priority || (px.priority == s.priority && px.layer < s.layer)
            }) {
                second = Some(px);
            }
        };

        // Add OBJ pixel if visible in window
        if win_flags.obj_enable {
            if let Some(obj_px) = &obj_line[x] {
                try_insert(*obj_px);
            }
        }

        // Add BG pixels if visible in window
        for bgi in 0..4usize {
            if !win_flags.bg_enable[bgi] {
                continue;
            }
            if let Some(ref line_buf) = bg_lines[bgi] {
                if let Some(px) = &line_buf[x] {
                    try_insert(*px);
                }
            }
        }

        // Get top pixel color (or backdrop)
        let (top_color, top_layer, is_semi_transparent) = match &top {
            Some(px) => (px.color, px.layer, px.semi_transparent),
            None => (backdrop, 5, false), // 5 = backdrop layer
        };

        // Get second pixel color (or backdrop)
        let (second_color, second_layer) = match &second {
            Some(px) => (px.color, px.layer),
            None => (backdrop, 5),
        };

        // Apply color effects if enabled in this window region
        if !win_flags.effects_enable {
            return top_color;
        }

        // Semi-transparent OBJs always alpha-blend (regardless of BLDCNT 1st target flags)
        if is_semi_transparent && is_second_target(bldcnt, second_layer) {
            return alpha_blend(top_color, second_color, eva, evb);
        }

        match blend_mode {
            BlendMode::None => top_color,
            BlendMode::Alpha => {
                if is_first_target(bldcnt, top_layer) && is_second_target(bldcnt, second_layer) {
                    alpha_blend(top_color, second_color, eva, evb)
                } else {
                    top_color
                }
            }
            BlendMode::BrightnessIncrease => {
                if is_first_target(bldcnt, top_layer) {
                    brightness_increase(top_color, evy)
                } else {
                    top_color
                }
            }
            BlendMode::BrightnessDecrease => {
                if is_first_target(bldcnt, top_layer) {
                    brightness_decrease(top_color, evy)
                } else {
                    top_color
                }
            }
        }
    }

    fn advance_affine_refs(&mut self, io: &IoRegisters) {
        let (_, pb2, _, pd2) = bg::get_affine_params(2, io);
        self.bg2_ref_x += pb2 as i32;
        self.bg2_ref_y += pd2 as i32;

        let (_, pb3, _, pd3) = bg::get_affine_params(3, io);
        self.bg3_ref_x += pb3 as i32;
        self.bg3_ref_y += pd3 as i32;
    }

    pub fn on_vblank(&mut self, io: &IoRegisters) {
        self.bg2_ref_x = io.bg2x_latch;
        self.bg2_ref_y = io.bg2y_latch;
        self.bg3_ref_x = io.bg3x_latch;
        self.bg3_ref_y = io.bg3y_latch;
    }

    // ─── Bitmap modes ─────────────────────────────────────────────

    fn render_mode3(&self, line: u16, vram: &[u8], framebuffer: &mut [u16]) {
        let row_start = line as usize * SCREEN_WIDTH;
        let vram_row = line as usize * SCREEN_WIDTH * 2;
        for x in 0..SCREEN_WIDTH {
            let offset = vram_row + x * 2;
            if offset + 1 < vram.len() {
                framebuffer[row_start + x] = u16::from_le_bytes([vram[offset], vram[offset + 1]]) & 0x7FFF;
            }
        }
    }

    fn render_mode4(&self, line: u16, io: &IoRegisters, palette: &[u8], vram: &[u8], framebuffer: &mut [u16]) {
        let frame_base = if io.dispcnt & (1 << 4) != 0 { 0xA000 } else { 0 };
        let row_start = line as usize * SCREEN_WIDTH;
        let vram_row = frame_base + line as usize * SCREEN_WIDTH;
        for x in 0..SCREEN_WIDTH {
            let idx = vram[vram_row + x] as usize;
            let color = if idx == 0 {
                u16::from_le_bytes([palette[0], palette[1]])
            } else {
                let po = idx * 2;
                u16::from_le_bytes([palette[po], palette[po + 1]])
            };
            framebuffer[row_start + x] = color & 0x7FFF;
        }
    }

    fn render_mode5(&self, line: u16, io: &IoRegisters, vram: &[u8], framebuffer: &mut [u16]) {
        let frame_base = if io.dispcnt & (1 << 4) != 0 { 0xA000 } else { 0 };
        let row_start = line as usize * SCREEN_WIDTH;
        if line >= 128 {
            for x in 0..SCREEN_WIDTH { framebuffer[row_start + x] = 0; }
            return;
        }
        for x in 0..SCREEN_WIDTH {
            if x < 160 {
                let offset = frame_base + (line as usize * 160 + x) * 2;
                if offset + 1 < vram.len() {
                    framebuffer[row_start + x] = u16::from_le_bytes([vram[offset], vram[offset + 1]]) & 0x7FFF;
                }
            } else {
                framebuffer[row_start + x] = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_composite_backdrop_only() {
        let ppu = Ppu::new();
        let bg_lines: [Option<[Option<PixelInfo>; 240]>; 4] = [None, None, None, None];
        let obj_line = [None; 240];
        let win = WindowFlags::all_enabled();
        let color = ppu.composite_pixel_with_effects(0, &bg_lines, &obj_line, 0x7C00, &win, 0, BlendMode::None, 0, 0, 0);
        assert_eq!(color, 0x7C00);
    }

    #[test]
    fn test_composite_bg_over_backdrop() {
        let ppu = Ppu::new();
        let mut bg0 = [None; 240];
        bg0[0] = Some(PixelInfo { color: 0x001F, priority: 0, layer: 0, semi_transparent: false });
        let bg_lines = [Some(bg0), None, None, None];
        let obj_line = [None; 240];
        let win = WindowFlags::all_enabled();
        let color = ppu.composite_pixel_with_effects(0, &bg_lines, &obj_line, 0x7C00, &win, 0, BlendMode::None, 0, 0, 0);
        assert_eq!(color, 0x001F);
    }

    #[test]
    fn test_composite_obj_over_bg_same_priority() {
        let ppu = Ppu::new();
        let mut bg0 = [None; 240];
        bg0[0] = Some(PixelInfo { color: 0x001F, priority: 0, layer: 0, semi_transparent: false });
        let bg_lines = [Some(bg0), None, None, None];
        let mut obj_line = [None; 240];
        obj_line[0] = Some(PixelInfo { color: 0x03E0, priority: 0, layer: 4, semi_transparent: false });
        let win = WindowFlags::all_enabled();
        let color = ppu.composite_pixel_with_effects(0, &bg_lines, &obj_line, 0, &win, 0, BlendMode::None, 0, 0, 0);
        assert_eq!(color, 0x03E0); // OBJ wins
    }

    #[test]
    fn test_alpha_blend_bg_layers() {
        let ppu = Ppu::new();
        let mut bg0 = [None; 240];
        bg0[0] = Some(PixelInfo { color: 0x001F, priority: 0, layer: 0, semi_transparent: false }); // Red
        let mut bg1 = [None; 240];
        bg1[0] = Some(PixelInfo { color: 0x7C00, priority: 1, layer: 1, semi_transparent: false }); // Blue
        let bg_lines = [Some(bg0), Some(bg1), None, None];
        let obj_line = [None; 240];
        let win = WindowFlags::all_enabled();

        // BLDCNT: alpha mode, 1st target=BG0, 2nd target=BG1
        let bldcnt: u16 = (1 << 6) | (1 << 0) | (1 << 9); // Alpha | BG0 1st | BG1 2nd
        let color = ppu.composite_pixel_with_effects(0, &bg_lines, &obj_line, 0, &win, bldcnt, BlendMode::Alpha, 8, 8, 0);

        // 50/50 blend: R=(31*8)/16=15, B=(31*8)/16=15
        let r = color & 0x1F;
        let b = (color >> 10) & 0x1F;
        assert_eq!(r, 15);
        assert_eq!(b, 15);
    }

    #[test]
    fn test_brightness_increase_on_first_target() {
        let ppu = Ppu::new();
        let mut bg0 = [None; 240];
        bg0[0] = Some(PixelInfo { color: 0x0000, priority: 0, layer: 0, semi_transparent: false }); // Black
        let bg_lines = [Some(bg0), None, None, None];
        let obj_line = [None; 240];
        let win = WindowFlags::all_enabled();

        // BLDCNT: brightness increase, 1st target=BG0
        let bldcnt: u16 = (2 << 6) | (1 << 0); // BrightnessIncrease | BG0
        let color = ppu.composite_pixel_with_effects(0, &bg_lines, &obj_line, 0, &win, bldcnt, BlendMode::BrightnessIncrease, 0, 0, 16);
        assert_eq!(color, 0x7FFF); // Full brightness = white
    }

    #[test]
    fn test_window_hides_layer() {
        let ppu = Ppu::new();
        let mut bg0 = [None; 240];
        bg0[0] = Some(PixelInfo { color: 0x001F, priority: 0, layer: 0, semi_transparent: false });
        let bg_lines = [Some(bg0), None, None, None];
        let obj_line = [None; 240];

        // Window that hides BG0
        let win = WindowFlags {
            bg_enable: [false, true, true, true],
            obj_enable: true,
            effects_enable: true,
        };

        let color = ppu.composite_pixel_with_effects(0, &bg_lines, &obj_line, 0x7C00, &win, 0, BlendMode::None, 0, 0, 0);
        assert_eq!(color, 0x7C00); // Backdrop shows through since BG0 is hidden
    }

    #[test]
    fn test_semi_transparent_obj_always_blends() {
        let ppu = Ppu::new();
        let mut bg0 = [None; 240];
        bg0[0] = Some(PixelInfo { color: 0x7C00, priority: 1, layer: 0, semi_transparent: false }); // Blue BG
        let bg_lines = [Some(bg0), None, None, None];
        let mut obj_line = [None; 240];
        obj_line[0] = Some(PixelInfo { color: 0x001F, priority: 0, layer: 4, semi_transparent: true }); // Red OBJ, semi-transparent
        let win = WindowFlags::all_enabled();

        // BLDCNT: no 1st target for OBJ, but BG0 is 2nd target
        // Semi-transparent OBJ should still blend
        let bldcnt: u16 = 1 << 8; // Only BG0 as 2nd target, no 1st target
        let color = ppu.composite_pixel_with_effects(0, &bg_lines, &obj_line, 0, &win, bldcnt, BlendMode::Alpha, 8, 8, 0);

        // Should blend: semi-transparent ignores 1st target flag
        let r = color & 0x1F;
        let b = (color >> 10) & 0x1F;
        assert_eq!(r, 15); // 50/50 of red
        assert_eq!(b, 15); // 50/50 of blue
    }
}
