//! Background rendering for tile modes (0, 1, 2) and affine backgrounds.
//!
//! ## Text BG (Modes 0/1)
//! - Tile map: 32x32 tiles per screen block (each entry is 16 bits)
//!   - Bits 0-9: Tile number (0-1023)
//!   - Bit 10: Horizontal flip
//!   - Bit 11: Vertical flip
//!   - Bits 12-15: Palette number (4bpp only)
//! - Screen sizes: 256x256, 512x256, 256x512, 512x512
//! - Character data: 4bpp (16 colors, 16 palettes) or 8bpp (256 colors, 1 palette)
//!
//! ## Affine BG (Modes 1/2)
//! - Tile map: 16x16 to 128x128 tiles per screen (each entry is 8 bits = tile number)
//! - Always 8bpp, no flip, single 256-color palette
//! - Rotation/scaling via PA, PB, PC, PD parameters
//! - Reference point updated per scanline

use crate::bus::io_regs::IoRegisters;
use crate::ppu::PixelInfo;
use crate::SCREEN_WIDTH;

/// Screen block sizes for text mode BGs.
/// Size bits (BGCNT bits 14-15) => (width_tiles, height_tiles)
const TEXT_SCREEN_SIZES: [(u32, u32); 4] = [
    (32, 32),   // 256x256
    (64, 32),   // 512x256
    (32, 64),   // 256x512
    (64, 64),   // 512x512
];

/// Affine screen sizes (BGCNT bits 14-15) => tiles per side
const AFFINE_SCREEN_SIZES: [u32; 4] = [16, 32, 64, 128]; // 128x128 to 1024x1024 pixels

/// Render one scanline of a text-mode background layer.
/// Returns 240 pixel entries (None = transparent).
pub fn render_text_bg_line(
    bg: usize,
    line: u16,
    io: &IoRegisters,
    palette: &[u8],
    vram: &[u8],
    output: &mut [Option<PixelInfo>; 240],
) {
    let bgcnt = io.bgcnt[bg];
    let priority = (bgcnt & 3) as u8;
    let char_base = (((bgcnt >> 2) & 3) as usize) * 0x4000;  // Character base block (16KB units)
    let mosaic = bgcnt & (1 << 6) != 0;
    let bpp8 = bgcnt & (1 << 7) != 0;                         // 0 = 4bpp, 1 = 8bpp
    let screen_base = (((bgcnt >> 8) & 0x1F) as usize) * 0x800; // Screen base block (2KB units)
    let size_idx = ((bgcnt >> 14) & 3) as usize;
    let (screen_w, screen_h) = TEXT_SCREEN_SIZES[size_idx];

    let scroll_x = io.bg_ofs[bg][0] as u32;
    let scroll_y = io.bg_ofs[bg][1] as u32;

    // Apply mosaic to Y coordinate
    let y = if mosaic {
        let mos_h = ((io.mosaic >> 4) & 0xF) as u32 + 1;
        ((line as u32 + scroll_y) / mos_h) * mos_h
    } else {
        line as u32 + scroll_y
    };

    let tile_y = (y / 8) % screen_h;
    let pixel_y = y % 8;

    for screen_x in 0..SCREEN_WIDTH {
        let x = if mosaic {
            let mos_w = (io.mosaic & 0xF) as u32 + 1;
            (((screen_x as u32 + scroll_x) / mos_w) * mos_w) % (screen_w * 8)
        } else {
            (screen_x as u32 + scroll_x) % (screen_w * 8)
        };

        let tile_x = x / 8;
        let pixel_x = x % 8;

        // Determine which screen block this tile is in (for maps > 32x32)
        let screen_block_offset = get_text_screen_block_offset(tile_x, tile_y, screen_w, screen_h);
        let map_addr = screen_base + screen_block_offset
            + ((tile_y % 32) * 32 + (tile_x % 32)) as usize * 2;

        if map_addr + 1 >= vram.len() {
            output[screen_x] = None;
            continue;
        }

        let map_entry = u16::from_le_bytes([vram[map_addr], vram[map_addr + 1]]);
        let tile_num = (map_entry & 0x3FF) as usize;
        let h_flip = map_entry & (1 << 10) != 0;
        let v_flip = map_entry & (1 << 11) != 0;
        let pal_num = ((map_entry >> 12) & 0xF) as usize;

        let px = if h_flip { 7 - pixel_x } else { pixel_x };
        let py = if v_flip { 7 - pixel_y } else { pixel_y };

        let color_index = if bpp8 {
            // 8bpp: 64 bytes per tile
            let tile_addr = char_base + tile_num * 64 + py as usize * 8 + px as usize;
            if tile_addr < vram.len() { vram[tile_addr] as usize } else { 0 }
        } else {
            // 4bpp: 32 bytes per tile, 4 bits per pixel
            let tile_addr = char_base + tile_num * 32 + py as usize * 4 + (px as usize / 2);
            if tile_addr < vram.len() {
                let byte = vram[tile_addr];
                if px % 2 == 0 { (byte & 0x0F) as usize } else { (byte >> 4) as usize }
            } else {
                0
            }
        };

        // Color index 0 = transparent
        if color_index == 0 {
            output[screen_x] = None;
            continue;
        }

        let pal_offset = if bpp8 {
            color_index * 2
        } else {
            (pal_num * 16 + color_index) * 2
        };

        if pal_offset + 1 < palette.len() {
            let color = u16::from_le_bytes([palette[pal_offset], palette[pal_offset + 1]]) & 0x7FFF;
            output[screen_x] = Some(PixelInfo {
                color,
                priority,
                layer: bg as u8,
                semi_transparent: false,
            });
        } else {
            output[screen_x] = None;
        }
    }
}

/// Calculate the screen block offset for text maps > 32x32 tiles.
/// Multi-screen text maps are arranged as:
///   32x32: [0]            64x32: [0][1]
///   32x64: [0]            64x64: [0][1]
///          [1]                   [2][3]
fn get_text_screen_block_offset(tile_x: u32, tile_y: u32, screen_w: u32, _screen_h: u32) -> usize {
    let block_x = tile_x / 32;
    let block_y = tile_y / 32;
    let block_index = match screen_w {
        64 => block_x + block_y * 2,
        32 => block_y,
        _ => 0,
    };
    (block_index as usize) * 0x800
}

/// Render one scanline of an affine (rotation/scaling) background.
pub fn render_affine_bg_line(
    bg: usize, // Must be 2 or 3
    _line: u16,
    io: &IoRegisters,
    palette: &[u8],
    vram: &[u8],
    ref_x: i32,
    ref_y: i32,
    output: &mut [Option<PixelInfo>; 240],
) {
    let bgcnt = io.bgcnt[bg];
    let priority = (bgcnt & 3) as u8;
    let char_base = (((bgcnt >> 2) & 3) as usize) * 0x4000;
    let screen_base = (((bgcnt >> 8) & 0x1F) as usize) * 0x800;
    let wrap = bgcnt & (1 << 13) != 0;
    let size_idx = ((bgcnt >> 14) & 3) as usize;
    let map_size = AFFINE_SCREEN_SIZES[size_idx];
    let pixel_size = map_size * 8; // Total pixel dimension

    // Affine parameters: PA = dx/screen_x, PC = dy/screen_x
    let (pa, _pb, pc, _pd) = get_affine_params(bg, io);

    // Start from the reference point for this line
    let mut tex_x = ref_x;
    let mut tex_y = ref_y;

    for screen_x in 0..SCREEN_WIDTH {
        // Convert from 8.8 fixed point to integer
        let sx = tex_x >> 8;
        let sy = tex_y >> 8;

        let (fx, fy) = if wrap {
            (
                ((sx % pixel_size as i32) + pixel_size as i32) as u32 % pixel_size,
                ((sy % pixel_size as i32) + pixel_size as i32) as u32 % pixel_size,
            )
        } else {
            if sx < 0 || sy < 0 || sx >= pixel_size as i32 || sy >= pixel_size as i32 {
                output[screen_x] = None;
                tex_x += pa as i32;
                tex_y += pc as i32;
                continue;
            }
            (sx as u32, sy as u32)
        };

        let tile_x = fx / 8;
        let tile_y = fy / 8;
        let pixel_x = (fx % 8) as usize;
        let pixel_y = (fy % 8) as usize;

        // Affine maps use 8-bit entries (just tile number)
        let map_addr = screen_base + (tile_y * map_size + tile_x) as usize;
        let tile_num = if map_addr < vram.len() { vram[map_addr] as usize } else { 0 };

        // Always 8bpp for affine BGs
        let tile_addr = char_base + tile_num * 64 + pixel_y * 8 + pixel_x;
        let color_index = if tile_addr < vram.len() { vram[tile_addr] as usize } else { 0 };

        if color_index == 0 {
            output[screen_x] = None;
        } else {
            let pal_offset = color_index * 2;
            if pal_offset + 1 < palette.len() {
                let color = u16::from_le_bytes([palette[pal_offset], palette[pal_offset + 1]]) & 0x7FFF;
                output[screen_x] = Some(PixelInfo {
                    color,
                    priority,
                    layer: bg as u8,
                    semi_transparent: false,
                });
            } else {
                output[screen_x] = None;
            }
        }

        // Advance texture coordinates: PA = dx/pixel, PC = dy/pixel
        tex_x += pa as i32;
        tex_y += pc as i32;
    }
}

/// Get the affine parameters (PA, PC) for stepping across a scanline.
/// PA = dx per screen pixel in X, PC = dy per screen pixel in X.
/// PB = dx per scanline in Y, PD = dy per scanline in Y.
/// For rendering a line: ref_x += PA per pixel, ref_y += PC per pixel.
/// At end of scanline: ref_x += PB, ref_y += PD (for the next line).
pub fn get_affine_params(bg: usize, io: &IoRegisters) -> (i16, i16, i16, i16) {
    if bg == 2 {
        (
            io.bg2_affine[0] as i16, // PA
            io.bg2_affine[1] as i16, // PB
            io.bg2_affine[2] as i16, // PC
            io.bg2_affine[3] as i16, // PD
        )
    } else {
        (
            io.bg3_affine[0] as i16,
            io.bg3_affine[1] as i16,
            io.bg3_affine[2] as i16,
            io.bg3_affine[3] as i16,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_bg_basic() {
        let mut io = IoRegisters::new();
        // BG0: priority 0, char base 0, screen base 0, 4bpp, 256x256
        io.bgcnt[0] = 0x0000;

        let mut palette = vec![0u8; 0x400];
        // Set palette entry 1 in palette 0 to red (0x001F)
        palette[2] = 0x1F;
        palette[3] = 0x00;

        let mut vram = vec![0u8; 0x18000];
        // Tile map at screen base 0: tile 1 at position (0,0)
        vram[0] = 1; // Tile number = 1
        vram[1] = 0; // No flip, palette 0

        // Tile 1 character data at char base 0: 4bpp, first pixel = palette index 1
        // 4bpp: 32 bytes per tile, 4 bytes per row
        // Byte 0 of row 0: low nibble = pixel 0, high nibble = pixel 1
        let tile_offset = 1 * 32; // Tile 1
        vram[tile_offset] = 0x01; // pixel 0 = index 1, pixel 1 = index 0

        let mut output = [None; 240];
        render_text_bg_line(0, 0, &io, &palette, &vram, &mut output);

        // Pixel 0 should be red
        assert!(output[0].is_some());
        assert_eq!(output[0].unwrap().color, 0x001F);
        // Pixel 1 should be transparent (index 0)
        assert!(output[1].is_none());
    }

    #[test]
    fn test_text_bg_hflip() {
        let mut io = IoRegisters::new();
        io.bgcnt[0] = 0x0000; // 4bpp

        let mut palette = vec![0u8; 0x400];
        palette[2] = 0x1F; // index 1 = red

        let mut vram = vec![0u8; 0x18000];
        // Tile map: tile 1, H-flip
        vram[0] = 1;
        vram[1] = 0x04; // H-flip bit (bit 10 of map entry = bit 2 of byte 1)

        // Tile 1: first row, pixel 0 = index 1 (rest = 0)
        let tile_offset = 1 * 32;
        vram[tile_offset] = 0x01;

        let mut output = [None; 240];
        render_text_bg_line(0, 0, &io, &palette, &vram, &mut output);

        // With H-flip, pixel 0 should now be at x=7
        assert!(output[0].is_none()); // Was index 1, flipped to x=7
        assert!(output[7].is_some()); // Now pixel 0 (index 1) is here
        assert_eq!(output[7].unwrap().color, 0x001F);
    }

    #[test]
    fn test_text_bg_scrolling() {
        let mut io = IoRegisters::new();
        io.bgcnt[0] = 0x0000;
        io.bg_ofs[0][0] = 4; // Scroll X by 4 pixels

        let mut palette = vec![0u8; 0x400];
        palette[2] = 0x1F;

        let mut vram = vec![0u8; 0x18000];
        // Tile 1 at (0,0)
        vram[0] = 1;
        vram[1] = 0;
        let tile_offset = 1 * 32;
        vram[tile_offset] = 0x01; // pixel 0 = index 1

        let mut output = [None; 240];
        render_text_bg_line(0, 0, &io, &palette, &vram, &mut output);

        // With scroll X=4, tile pixel 0 is now at screen pixel (240-4)=236
        // Actually: screen_x=0 maps to tile (0+4)/8=0, pixel (0+4)%8=4
        // So pixel 0 of tile (index 1) is at screen_x that maps to x=0
        // screen_x + scroll_x = 0+4=4, tile pixel 4 of tile 0 -> transparent
        assert!(output[0].is_none());
    }
}
