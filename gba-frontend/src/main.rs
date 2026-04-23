mod video;
mod audio;
mod input;

use clap::Parser;
use gba_core::Gba;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "gba-emu", about = "GBA Emulator")]
struct Args {
    /// Path to the GBA ROM file
    rom: String,

    /// Path to the BIOS file (optional, uses HLE if not provided)
    #[arg(short, long)]
    bios: Option<String>,

    /// Skip BIOS boot animation
    #[arg(long, default_value_t = true)]
    skip_bios: bool,

    /// Window scale factor
    #[arg(short, long, default_value_t = 3)]
    scale: u32,

    /// Disable audio
    #[arg(long)]
    no_audio: bool,

    /// Test mode: render a fixed color pattern instead of running the ROM
    /// (sanity-check that SDL2 rendering works end-to-end).
    #[arg(long)]
    test_pattern: bool,

    /// Even simpler: skip ALL texture code, just clear the window to solid red.
    /// If this also shows wrong colors, the bug is at the SDL2 canvas layer.
    #[arg(long)]
    test_solid_red: bool,
}

/// Derive the .sav file path from the ROM path.
fn sav_path(rom_path: &str) -> PathBuf {
    let p = Path::new(rom_path);
    p.with_extension("sav")
}

/// Derive the save state file path from the ROM path.
fn state_path(rom_path: &str) -> PathBuf {
    let p = Path::new(rom_path);
    p.with_extension("state")
}

/// Load .sav file if it exists.
fn load_sav(gba: &mut Gba, path: &Path) {
    if path.exists() {
        match fs::read(path) {
            Ok(data) => {
                gba.import_save(&data);
                eprintln!("Loaded save from {}", path.display());
            }
            Err(e) => eprintln!("Failed to load save: {}", e),
        }
    }
}

/// Save .sav file.
fn save_sav(gba: &Gba, path: &Path) {
    if let Some(data) = gba.export_save() {
        match fs::write(path, &data) {
            Ok(()) => eprintln!("Saved to {}", path.display()),
            Err(e) => eprintln!("Failed to write save: {}", e),
        }
    }
}

/// Save state (F5): serialize + zstd compress.
fn save_state(gba: &Gba, path: &Path) {
    match gba.save_state() {
        Ok(data) => {
            match zstd::encode_all(data.as_slice(), 3) {
                Ok(compressed) => {
                    match fs::write(path, &compressed) {
                        Ok(()) => eprintln!("Save state written to {}", path.display()),
                        Err(e) => eprintln!("Failed to write save state: {}", e),
                    }
                }
                Err(e) => eprintln!("Failed to compress save state: {}", e),
            }
        }
        Err(e) => eprintln!("Failed to serialize state: {}", e),
    }
}

/// Load state (F8): zstd decompress + deserialize.
fn load_state(gba: &mut Gba, path: &Path) {
    if !path.exists() {
        eprintln!("No save state found at {}", path.display());
        return;
    }
    match fs::read(path) {
        Ok(compressed) => {
            match zstd::decode_all(compressed.as_slice()) {
                Ok(data) => {
                    match gba.load_state(&data) {
                        Ok(()) => eprintln!("Loaded save state from {}", path.display()),
                        Err(e) => eprintln!("Failed to deserialize state: {}", e),
                    }
                }
                Err(e) => eprintln!("Failed to decompress save state: {}", e),
            }
        }
        Err(e) => eprintln!("Failed to read save state: {}", e),
    }
}

fn main() {
    env_logger::init();
    let args = Args::parse();

    // Load ROM
    let rom = fs::read(&args.rom).unwrap_or_else(|e| {
        eprintln!("Failed to read ROM '{}': {}", args.rom, e);
        std::process::exit(1);
    });

    // Load BIOS
    let bios = args.bios.map(|path| {
        fs::read(&path).unwrap_or_else(|e| {
            eprintln!("Failed to read BIOS '{}': {}", path, e);
            std::process::exit(1);
        })
    });

    // Create GBA instance
    let mut gba = Gba::new(bios, rom);

    if args.skip_bios {
        gba.cpu = gba_core::arm7tdmi::Cpu::new_skip_bios();
    }

    // Load .sav file
    let sav = sav_path(&args.rom);
    load_sav(&mut gba, &sav);

    let state = state_path(&args.rom);

    // Initialize SDL2
    let sdl_context = sdl2::init().expect("Failed to initialize SDL2");
    let mut display = video::Display::new(&sdl_context, args.scale);
    let mut event_pump = sdl_context.event_pump().expect("Failed to get event pump");

    // Initialize audio
    let audio_state = if !args.no_audio {
        audio::init_audio(&sdl_context)
    } else {
        None
    };

    let frame_duration = Duration::from_nanos(16_742_706); // ~59.737 Hz
    let mut audio_tmp = vec![0i16; 4096];
    let mut test_buf = vec![0u16; 240 * 160];

    // Main emulation loop
    'running: loop {
        let frame_start = Instant::now();

        // Handle input
        for event in event_pump.poll_iter() {
            match event {
                sdl2::event::Event::Quit { .. } => break 'running,
                sdl2::event::Event::KeyDown {
                    keycode: Some(key), ..
                } => match key {
                    sdl2::keyboard::Keycode::Escape => break 'running,
                    // Save state: ] (right bracket) — "save forward"
                    sdl2::keyboard::Keycode::RightBracket => save_state(&gba, &state),
                    // Load state: [ (left bracket) — "restore back"
                    sdl2::keyboard::Keycode::LeftBracket => load_state(&mut gba, &state),
                    _ => {}
                },
                _ => {}
            }
        }

        // Update keypad from keyboard state
        let keyboard = event_pump.keyboard_state();
        let keys = input::read_keyboard(&keyboard);
        gba.set_keypad_state(keys);

        // Solid red test — bypasses texture code entirely
        if args.test_solid_red {
            display.clear_to_red();
            std::thread::sleep(frame_duration);
            continue;
        }

        // Run one frame (or build a test pattern if --test-pattern)
        let framebuffer: &[u16] = if args.test_pattern {
            // 4 vertical stripes: red | green | blue | white
            for y in 0..160 {
                for x in 0..240 {
                    let color = match x / 60 {
                        0 => 0x001F, // red
                        1 => 0x03E0, // green
                        2 => 0x7C00, // blue
                        _ => 0x7FFF, // white
                    };
                    test_buf[y * 240 + x] = color;
                }
            }
            &test_buf
        } else {
            gba.run_frame()
        };

        // Display
        display.render(framebuffer);

        // Pump audio samples to SDL2
        if let Some((ref audio_buf, ref _device)) = audio_state {
            let n = gba.drain_audio(&mut audio_tmp);
            if n > 0 {
                audio_buf.push_samples(&audio_tmp[..n]);
            }
        }

        // Frame timing
        let elapsed = frame_start.elapsed();
        if elapsed < frame_duration {
            std::thread::sleep(frame_duration - elapsed);
        }
    }

    // Save .sav on exit
    save_sav(&gba, &sav);
}
