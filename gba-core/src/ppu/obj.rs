//! Sprite (OBJ) rendering.
//!
//! OAM contains 128 entries of 8 bytes each (6 bytes attributes + 2 bytes affine padding).
//! Each entry defines a sprite's position, size, tile, palette, priority, and mode.
//!
//! ## OAM Attribute 0 (16 bits)
//! - Bits 0-7: Y coordinate (0-255, wraps)
//! - Bits 8-9: OBJ Mode (0=Normal, 1=Affine, 2=Disabled, 3=Affine double-size)
//! - Bits 10-11: GFX Mode (0=Normal, 1=Semi-transparent, 2=OBJ Window, 3=Prohibited)
//! - Bit 12: Mosaic
//! - Bit 13: Color mode (0=4bpp/16 colors, 1=8bpp/256 colors)
//! - Bits 14-15: Shape (0=Square, 1=Horizontal, 2=Vertical)
//!
//! ## OAM Attribute 1 (16 bits)
//! - Bits 0-8: X coordinate (0-511, signed 9-bit)
//! - For non-affine: Bit 12=H-flip, Bit 13=V-flip
//! - For affine: Bits 9-13 = Affine parameter group (0-31)
//! - Bits 14-15: Size (combined with shape gives actual dimensions)
//!
//! ## OAM Attribute 2 (16 bits)
//! - Bits 0-9: Tile number (base tile in VRAM)
//! - Bits 10-11: Priority (0-3, lower = higher priority)
//! - Bits 12-15: Palette number (4bpp only)

use crate::bus::io_regs::IoRegisters;
use crate::ppu::PixelInfo;
use crate::SCREEN_WIDTH;

/// OBJ sizes indexed by [shape][size]: (width, height) in pixels.
/// Shape: 0=Square, 1=Horizontal, 2=Vertical
/// Size: 0-3
const OBJ_SIZES: [[(u32, u32); 4]; 3] = [
    // Square
    [(8, 8), (16, 16), (32, 32), (64, 64)],
    // Horizontal (wide)
    [(16, 8), (32, 8), (32, 16), (64, 32)],
    // Vertical (tall)
    [(8, 16), (8, 32), (16, 32), (32, 64)],
];

/// Parsed OAM entry.
#[allow(dead_code)]
struct ObjAttr {
    y: i32,
    x: i32,
    mode: u8,      // 0=Normal, 1=Affine, 2=Disabled, 3=Affine double
    gfx_mode: u8,  // 0=Normal, 1=Semi-transparent, 2=OBJ Window
    mosaic: bool,
    bpp8: bool,
    shape: u8,
    size: u8,
    h_flip: bool,
    v_flip: bool,
    affine_param: u8,
    tile_num: u16,
    priority: u8,
    palette: u8,
    width: u32,
    height: u32,
}

fn parse_obj(oam: &[u8], index: usize) -> ObjAttr {
    let base = index * 8;
    let attr0 = u16::from_le_bytes([oam[base], oam[base + 1]]);
    let attr1 = u16::from_le_bytes([oam[base + 2], oam[base + 3]]);
    let attr2 = u16::from_le_bytes([oam[base + 4], oam[base + 5]]);

    let y = (attr0 & 0xFF) as i32;
    let mode = ((attr0 >> 8) & 3) as u8;
    let gfx_mode = ((attr0 >> 10) & 3) as u8;
    let mosaic = attr0 & (1 << 12) != 0;
    let bpp8 = attr0 & (1 << 13) != 0;
    let shape = ((attr0 >> 14) & 3) as u8;

    // X is 9-bit signed
    let x_raw = (attr1 & 0x1FF) as i32;
    let x = if x_raw >= 256 { x_raw - 512 } else { x_raw };

    let is_affine = mode == 1 || mode == 3;
    let h_flip = if !is_affine { attr1 & (1 << 12) != 0 } else { false };
    let v_flip = if !is_affine { attr1 & (1 << 13) != 0 } else { false };
    let affine_param = if is_affine { ((attr1 >> 9) & 0x1F) as u8 } else { 0 };
    let size = ((attr1 >> 14) & 3) as u8;

    let tile_num = attr2 & 0x3FF;
    let priority = ((attr2 >> 10) & 3) as u8;
    let palette = ((attr2 >> 12) & 0xF) as u8;

    let shape_idx = (shape as usize).min(2);
    let size_idx = (size as usize).min(3);
    let (width, height) = OBJ_SIZES[shape_idx][size_idx];

    ObjAttr {
        y, x, mode, gfx_mode, mosaic, bpp8, shape, size,
        h_flip, v_flip, affine_param, tile_num, priority, palette,
        width, height,
    }
}

/// Read affine parameters (PA, PB, PC, PD) for a given parameter group.
/// Affine params are stored at OAM offsets: group*32 + 6, +14, +22, +30
fn read_affine_params(oam: &[u8], group: u8) -> (i16, i16, i16, i16) {
    let base = group as usize * 32;
    let pa = i16::from_le_bytes([oam[base + 6], oam[base + 7]]);
    let pb = i16::from_le_bytes([oam[base + 14], oam[base + 15]]);
    let pc = i16::from_le_bytes([oam[base + 22], oam[base + 23]]);
    let pd = i16::from_le_bytes([oam[base + 30], oam[base + 31]]);
    (pa, pb, pc, pd)
}

/// Render all visible sprites on a scanline.
/// OBJ VRAM starts at 0x06010000 (offset 0x10000 in VRAM).
pub fn render_obj_line(
    line: u16,
    io: &IoRegisters,
    palette: &[u8], // Full 1KB palette (OBJ palette starts at offset 0x200)
    vram: &[u8],
    oam: &[u8],
    output: &mut [Option<PixelInfo>; 240],
) {
    let mapping_1d = io.dispcnt & (1 << 6) != 0;
    // OBJ tile data starts at VRAM offset 0x10000
    let obj_vram_base: usize = 0x10000;
    // OBJ palette starts at palette offset 0x200
    let obj_pal_base: usize = 0x200;

    // Scan all 128 OBJs (lower index = higher priority for same-priority OBJs)
    for i in 0..128 {
        let obj = parse_obj(oam, i);

        // Skip disabled sprites
        if obj.mode == 2 {
            continue;
        }

        // Skip OBJ Window sprites for now (Phase 5)
        if obj.gfx_mode == 2 {
            continue;
        }

        let is_affine = obj.mode == 1 || obj.mode == 3;
        let double_size = obj.mode == 3;

        // Calculate the bounding box (double-size affine sprites have 2x dimensions)
        let (bound_w, bound_h) = if double_size {
            (obj.width * 2, obj.height * 2)
        } else {
            (obj.width, obj.height)
        };

        // Check if this sprite is on the current scanline
        // Y wraps: a Y of 160-255 means the sprite starts at -(256-Y) relative to screen top
        let obj_y = if obj.y >= 160 && obj.y < 256 { obj.y - 256 } else { obj.y };

        let local_y = line as i32 - obj_y;
        if local_y < 0 || local_y >= bound_h as i32 {
            continue;
        }

        // Render each pixel of this sprite on this scanline
        for lx in 0..bound_w as i32 {
            let screen_x = obj.x + lx;
            if screen_x < 0 || screen_x >= SCREEN_WIDTH as i32 {
                continue;
            }
            let sx = screen_x as usize;

            // Don't overwrite higher-priority OBJ pixels
            if let Some(existing) = &output[sx] {
                if existing.layer == 4 && existing.priority <= obj.priority {
                    continue;
                }
            }

            // Map screen-relative (lx, local_y) to texture coordinates
            let (tex_x, tex_y) = if is_affine {
                let (pa, pb, pc, pd) = read_affine_params(oam, obj.affine_param);
                let half_w = obj.width as i32 / 2;
                let half_h = obj.height as i32 / 2;
                // Transform from bounding box coords to texture coords
                let cx = lx - bound_w as i32 / 2;
                let cy = local_y - bound_h as i32 / 2;
                let tx = ((pa as i32 * cx + pb as i32 * cy) >> 8) + half_w;
                let ty = ((pc as i32 * cx + pd as i32 * cy) >> 8) + half_h;
                if tx < 0 || ty < 0 || tx >= obj.width as i32 || ty >= obj.height as i32 {
                    continue;
                }
                (tx as u32, ty as u32)
            } else {
                let tx = if obj.h_flip { obj.width - 1 - lx as u32 } else { lx as u32 };
                let ty = if obj.v_flip { obj.height - 1 - local_y as u32 } else { local_y as u32 };
                (tx, ty)
            };

            // Fetch the pixel from tile data
            let tile_x = tex_x / 8;
            let tile_y = tex_y / 8;
            let pixel_x = (tex_x % 8) as usize;
            let pixel_y = (tex_y % 8) as usize;

            let tile_offset = if mapping_1d {
                // 1D mapping: tiles are sequential
                let base_tile = obj.tile_num as u32;
                let tile_idx = if obj.bpp8 {
                    // 8bpp: each tile is 64 bytes (2 tile slots)
                    base_tile + tile_y * (obj.width / 8) * 2 + tile_x * 2
                } else {
                    base_tile + tile_y * (obj.width / 8) + tile_x
                };
                tile_idx as usize
            } else {
                // 2D mapping: tiles in a 32-tile-wide grid
                let base_tile = obj.tile_num as u32;
                let tile_idx = if obj.bpp8 {
                    base_tile + tile_y * 32 + tile_x * 2
                } else {
                    base_tile + tile_y * 32 + tile_x
                };
                tile_idx as usize
            };

            let color_index = if obj.bpp8 {
                let addr = obj_vram_base + tile_offset * 32 + pixel_y * 8 + pixel_x;
                if addr < vram.len() { vram[addr] as usize } else { 0 }
            } else {
                let addr = obj_vram_base + tile_offset * 32 + pixel_y * 4 + pixel_x / 2;
                if addr < vram.len() {
                    let byte = vram[addr];
                    if pixel_x % 2 == 0 { (byte & 0x0F) as usize } else { (byte >> 4) as usize }
                } else {
                    0
                }
            };

            if color_index == 0 {
                continue; // Transparent
            }

            let pal_offset = if obj.bpp8 {
                obj_pal_base + color_index * 2
            } else {
                obj_pal_base + (obj.palette as usize * 16 + color_index) * 2
            };

            if pal_offset + 1 < palette.len() {
                let color = u16::from_le_bytes([palette[pal_offset], palette[pal_offset + 1]]) & 0x7FFF;
                output[sx] = Some(PixelInfo {
                    color,
                    priority: obj.priority,
                    layer: 4,
                    semi_transparent: obj.gfx_mode == 1,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_obj_sizes() {
        assert_eq!(OBJ_SIZES[0][0], (8, 8));
        assert_eq!(OBJ_SIZES[0][3], (64, 64));
        assert_eq!(OBJ_SIZES[1][0], (16, 8));
        assert_eq!(OBJ_SIZES[2][3], (32, 64));
    }

    #[test]
    fn test_parse_obj_disabled() {
        let mut oam = vec![0u8; 0x400];
        // Set mode = 2 (disabled) in attr0 bits 8-9
        oam[1] = 0x02; // bits 8-9 = 10 = mode 2

        let obj = parse_obj(&oam, 0);
        assert_eq!(obj.mode, 2);
    }

    #[test]
    fn test_render_simple_sprite() {
        let mut io = IoRegisters::new();
        io.dispcnt = 0x1040; // OBJ enable, 1D mapping

        let mut palette = vec![0u8; 0x400];
        // OBJ palette index 1 in palette 0 = green (0x03E0)
        let pal_addr = 0x200 + 2; // index 1
        palette[pal_addr] = 0xE0;
        palette[pal_addr + 1] = 0x03;

        let mut vram = vec![0u8; 0x18000];
        // Tile 0 at OBJ VRAM (0x10000): 4bpp, pixel 0 = index 1
        let tile_addr = 0x10000;
        vram[tile_addr] = 0x01;

        let mut oam = vec![0u8; 0x400];
        // OBJ 0: Y=0, mode=normal, 4bpp, square shape
        oam[0] = 0;   // Y=0
        oam[1] = 0;   // mode=0, gfx=0, 4bpp, square
        // X=0, size=0 (8x8)
        oam[2] = 0;
        oam[3] = 0;
        // Tile 0, priority 0, palette 0
        oam[4] = 0;
        oam[5] = 0;

        let mut output = [None; 240];
        render_obj_line(0, &io, &palette, &vram, &oam, &mut output);

        assert!(output[0].is_some());
        assert_eq!(output[0].unwrap().color, 0x03E0);
        assert_eq!(output[0].unwrap().layer, 4);
    }
}
