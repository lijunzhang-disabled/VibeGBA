use gba_core::{SCREEN_WIDTH, SCREEN_HEIGHT};
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::render::{Canvas, TextureCreator};
use sdl2::video::{Window, WindowContext};

pub struct Display {
    canvas: Canvas<Window>,
    texture_creator: TextureCreator<WindowContext>,
    /// Reusable byte buffer for ARGB8888 pixels (4 bytes per pixel).
    pixel_buffer: Vec<u8>,
}

impl Display {
    pub fn new(sdl: &sdl2::Sdl, scale: u32) -> Self {
        let video = sdl.video().expect("Failed to initialize SDL2 video");

        let window = video
            .window(
                "GBA Emulator",
                SCREEN_WIDTH as u32 * scale,
                SCREEN_HEIGHT as u32 * scale,
            )
            .position_centered()
            .build()
            .expect("Failed to create window");

        // Use software renderer to avoid Metal/OpenGL backend issues.
        // For a 240x160 emulator scaled to 720x480, perf is plenty.
        let mut canvas = window
            .into_canvas()
            .software()
            .build()
            .expect("Failed to create canvas");

        // Explicit black background — avoids platform defaults.
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();
        canvas.present();

        let texture_creator = canvas.texture_creator();

        // Log the renderer info so we know what backend is active.
        let info = canvas.info();
        eprintln!("SDL2 renderer: {} (flags: 0x{:X})", info.name, info.flags);

        Display {
            canvas,
            texture_creator,
            pixel_buffer: vec![0u8; SCREEN_WIDTH * SCREEN_HEIGHT * 4],
        }
    }

    /// Diagnostic: clear the entire window to solid red, no texture, no copy.
    pub fn clear_to_red(&mut self) {
        self.canvas.set_draw_color(Color::RGB(255, 0, 0));
        self.canvas.clear();
        self.canvas.present();
    }

    /// Render a 240x160 framebuffer (15-bit GBA color) to the window.
    pub fn render(&mut self, framebuffer: &[u16]) {
        // Convert 15-bit GBA color to ARGB8888 in the pre-allocated byte buffer.
        // ARGB8888 in SDL2 native byte order on little-endian = bytes [B, G, R, A].
        for i in 0..(SCREEN_WIDTH * SCREEN_HEIGHT) {
            let color = framebuffer[i];
            let r = ((color & 0x1F) as u8) << 3;
            let g = (((color >> 5) & 0x1F) as u8) << 3;
            let b = (((color >> 10) & 0x1F) as u8) << 3;
            let off = i * 4;
            self.pixel_buffer[off] = b;
            self.pixel_buffer[off + 1] = g;
            self.pixel_buffer[off + 2] = r;
            self.pixel_buffer[off + 3] = 0xFF;
        }

        // Recreate the texture each frame (cheap, avoids lifetime gymnastics).
        let mut texture = self
            .texture_creator
            .create_texture_streaming(
                PixelFormatEnum::ARGB8888,
                SCREEN_WIDTH as u32,
                SCREEN_HEIGHT as u32,
            )
            .expect("Failed to create texture");

        texture
            .update(None, &self.pixel_buffer, SCREEN_WIDTH * 4)
            .expect("Failed to update texture");

        self.canvas.set_draw_color(Color::RGB(0, 0, 0));
        self.canvas.clear();
        self.canvas
            .copy(&texture, None, None)
            .expect("Failed to copy texture");
        self.canvas.present();
    }
}
