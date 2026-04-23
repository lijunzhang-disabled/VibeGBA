//! Color special effects: alpha blending, brightness increase/decrease.
//!
//! ## BLDCNT (0x04000050) — Blend Control
//! - Bits 0-5: 1st target (BG0, BG1, BG2, BG3, OBJ, Backdrop)
//! - Bits 6-7: Effect mode (0=None, 1=Alpha, 2=Brighten, 3=Darken)
//! - Bits 8-13: 2nd target (BG0, BG1, BG2, BG3, OBJ, Backdrop)
//!
//! ## BLDALPHA (0x04000052) — Alpha Coefficients
//! - Bits 0-4: EVA (1st target coefficient, 0-16)
//! - Bits 8-12: EVB (2nd target coefficient, 0-16)
//!
//! ## BLDY (0x04000054) — Brightness Coefficient
//! - Bits 0-4: EVY (0-16, clamped to 16)
//!
//! ## Rules
//! - Alpha blending only applies when the top pixel is a 1st target AND the pixel
//!   below it is a 2nd target.
//! - Semi-transparent OBJs (gfx_mode=1) always use alpha blending, regardless of
//!   BLDCNT 1st target flags — but still need a 2nd target below them.
//! - Brightness effects apply to all 1st-target pixels when there's no qualifying
//!   2nd target for alpha blending.
//! - Windows can disable effects per-region.

/// Blend mode from BLDCNT bits 6-7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    None = 0,
    Alpha = 1,
    BrightnessIncrease = 2,
    BrightnessDecrease = 3,
}

impl BlendMode {
    pub fn from_bldcnt(bldcnt: u16) -> Self {
        match (bldcnt >> 6) & 3 {
            0 => BlendMode::None,
            1 => BlendMode::Alpha,
            2 => BlendMode::BrightnessIncrease,
            3 => BlendMode::BrightnessDecrease,
            _ => unreachable!(),
        }
    }
}

/// Check if a layer is a 1st target in BLDCNT.
/// layer: 0-3 = BG0-3, 4 = OBJ, 5 = Backdrop
pub fn is_first_target(bldcnt: u16, layer: u8) -> bool {
    if layer > 5 { return false; }
    bldcnt & (1 << layer) != 0
}

/// Check if a layer is a 2nd target in BLDCNT.
pub fn is_second_target(bldcnt: u16, layer: u8) -> bool {
    if layer > 5 { return false; }
    bldcnt & (1 << (8 + layer)) != 0
}

/// Perform alpha blending between two 15-bit colors.
/// EVA and EVB are coefficients (0-16), clamped internally.
/// Result = min(31, (color1 * EVA + color2 * EVB) / 16) per component.
pub fn alpha_blend(color1: u16, color2: u16, eva: u16, evb: u16) -> u16 {
    let eva = eva.min(16);
    let evb = evb.min(16);

    let r1 = (color1 & 0x1F) as u32;
    let g1 = ((color1 >> 5) & 0x1F) as u32;
    let b1 = ((color1 >> 10) & 0x1F) as u32;

    let r2 = (color2 & 0x1F) as u32;
    let g2 = ((color2 >> 5) & 0x1F) as u32;
    let b2 = ((color2 >> 10) & 0x1F) as u32;

    let r = ((r1 * eva as u32 + r2 * evb as u32) / 16).min(31);
    let g = ((g1 * eva as u32 + g2 * evb as u32) / 16).min(31);
    let b = ((b1 * eva as u32 + b2 * evb as u32) / 16).min(31);

    (r as u16) | ((g as u16) << 5) | ((b as u16) << 10)
}

/// Increase brightness of a 15-bit color toward white.
/// EVY coefficient (0-16): result = color + (31 - color) * EVY / 16.
pub fn brightness_increase(color: u16, evy: u16) -> u16 {
    let evy = evy.min(16);

    let r = (color & 0x1F) as u32;
    let g = ((color >> 5) & 0x1F) as u32;
    let b = ((color >> 10) & 0x1F) as u32;

    let r = r + ((31 - r) * evy as u32) / 16;
    let g = g + ((31 - g) * evy as u32) / 16;
    let b = b + ((31 - b) * evy as u32) / 16;

    (r as u16) | ((g as u16) << 5) | ((b as u16) << 10)
}

/// Decrease brightness of a 15-bit color toward black.
/// EVY coefficient (0-16): result = color - color * EVY / 16.
pub fn brightness_decrease(color: u16, evy: u16) -> u16 {
    let evy = evy.min(16);

    let r = (color & 0x1F) as u32;
    let g = ((color >> 5) & 0x1F) as u32;
    let b = ((color >> 10) & 0x1F) as u32;

    let r = r - (r * evy as u32) / 16;
    let g = g - (g * evy as u32) / 16;
    let b = b - (b * evy as u32) / 16;

    (r as u16) | ((g as u16) << 5) | ((b as u16) << 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alpha_blend_equal() {
        // 50/50 blend of red and blue
        let red = 0x001F;   // R=31, G=0, B=0
        let blue = 0x7C00;  // R=0, G=0, B=31
        let result = alpha_blend(red, blue, 8, 8);
        // Each component: (31*8 + 0*8)/16 = 15, (0+0)/16 = 0, (0*8+31*8)/16 = 15
        let r = result & 0x1F;
        let b = (result >> 10) & 0x1F;
        assert_eq!(r, 15);
        assert_eq!(b, 15);
    }

    #[test]
    fn test_alpha_blend_full_first() {
        let red = 0x001F;
        let blue = 0x7C00;
        let result = alpha_blend(red, blue, 16, 0);
        assert_eq!(result, red); // 100% first target
    }

    #[test]
    fn test_alpha_blend_clamped() {
        // Both at max: (31*16 + 31*16)/16 = 62 → clamped to 31
        let white = 0x7FFF;
        let result = alpha_blend(white, white, 16, 16);
        assert_eq!(result, 0x7FFF); // Stays white (clamped)
    }

    #[test]
    fn test_brightness_increase() {
        let black = 0x0000;
        // Full brightness increase: 0 + (31-0)*16/16 = 31 → white
        let result = brightness_increase(black, 16);
        assert_eq!(result, 0x7FFF);

        // Half increase of red=16: 16 + (31-16)*8/16 = 16+7 = 23
        let color = 16; // R=16
        let result = brightness_increase(color, 8);
        assert_eq!(result & 0x1F, 23);
    }

    #[test]
    fn test_brightness_decrease() {
        let white = 0x7FFF;
        // Full decrease: 31 - 31*16/16 = 0 → black
        let result = brightness_decrease(white, 16);
        assert_eq!(result, 0x0000);

        // Half decrease of red=20: 20 - 20*8/16 = 20-10 = 10
        let color = 20;
        let result = brightness_decrease(color, 8);
        assert_eq!(result & 0x1F, 10);
    }

    #[test]
    fn test_target_flags() {
        // BG0 + OBJ as 1st target: bits 0 and 4
        let bldcnt: u16 = 0x0011;
        assert!(is_first_target(bldcnt, 0)); // BG0
        assert!(is_first_target(bldcnt, 4)); // OBJ
        assert!(!is_first_target(bldcnt, 1)); // BG1 not set

        // 2nd target bits start at bit 8
        let bldcnt2: u16 = 0x0200; // BG1 as 2nd target (bit 9)
        assert!(is_second_target(bldcnt2, 1));
        assert!(!is_second_target(bldcnt2, 0));

        // Backdrop as 1st target: bit 5
        let bldcnt3: u16 = 0x0020;
        assert!(is_first_target(bldcnt3, 5));
    }
}
