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

/// Append `.bak-N` to a save path. e.g. `Pokemon.sav` → `Pokemon.sav.bak-1`.
fn backup_path(path: &Path, n: u32) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(format!(".bak-{}", n));
    PathBuf::from(s)
}

/// Rotate `slots` backup files: bak-{slots-1} → bak-{slots} (oldest dropped),
/// …, bak-1 → bak-2, current `.sav` → `.bak-1`. Missing files are silently
/// skipped — first save just creates `.bak-1`, second creates `.bak-2`, etc.
fn rotate_sav_backups(path: &Path, slots: u32) {
    if slots == 0 {
        return;
    }
    // Drop the oldest slot (it's about to be overwritten by slot-1's data).
    let _ = fs::remove_file(backup_path(path, slots));
    // Shift slots: bak-(N-1) → bak-N for N down to 2.
    for n in (1..slots).rev() {
        let src = backup_path(path, n);
        let dst = backup_path(path, n + 1);
        let _ = fs::rename(&src, &dst);
    }
    // Move current .sav → .bak-1 (if it exists).
    if path.exists() {
        let bak1 = backup_path(path, 1);
        let _ = fs::rename(path, &bak1);
    }
}

/// Save `.sav` file. If the new save is different from what's already on
/// disk, rotate the backup files (.bak-1 .. .bak-N) before writing. This
/// keeps a 5-deep history of meaningful save changes; pure open-and-close
/// cycles with no in-game save are no-ops and don't fill the slots.
fn save_sav(gba: &Gba, path: &Path) {
    const BACKUP_SLOTS: u32 = 5;
    if let Some(data) = gba.export_save() {
        // Skip the rotate-and-write entirely if the file already matches.
        // This is what protects backup slots from being clobbered when the
        // user just opens the emulator and quits without playing.
        if let Ok(existing) = fs::read(path) {
            if existing == data {
                return;
            }
        }
        rotate_sav_backups(path, BACKUP_SLOTS);
        match fs::write(path, &data) {
            Ok(()) => eprintln!("Saved to {} (rotated 1 backup, kept up to {})",
                path.display(), BACKUP_SLOTS),
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

    // WAV_DUMP=path.wav captures every output sample into a 48 kHz stereo
    // 16-bit WAV file. Used for offline spectrum analysis (Audacity, etc.)
    // when chasing audio bugs.
    let mut wav_dump = std::env::var("WAV_DUMP").ok().and_then(|p| {
        match audio::WavWriter::create(&p) {
            Ok(w) => { eprintln!("WAV_DUMP: writing samples to {}", p); Some(w) }
            Err(e) => { eprintln!("WAV_DUMP: failed to open {}: {}", p, e); None }
        }
    });

    // PC-hot-loop sampler: when DUMP_PC=1, every 120 frames (~2s) print
    // the most-frequent PC and IRQ-disabled state from the last frame.
    // Useful for finding where the CPU is stuck during in-game save.
    let dump_pc = std::env::var("DUMP_PC").is_ok();
    let mut frame_count: u64 = 0;

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

        // Run one frame split into chunks so we can pump audio more often
        // (every ~4 ms of emulated time instead of every ~16 ms). This makes
        // buffer-level fluctuations much smaller and eliminates periodic gaps.
        const CHUNKS_PER_FRAME: u64 = 4;
        const CHUNK_CYCLES: u64 = gba_core::CYCLES_PER_FRAME / CHUNKS_PER_FRAME;

        if args.test_pattern {
            // 4 vertical stripes: red | green | blue | white
            for y in 0..160 {
                for x in 0..240 {
                    let color = match x / 60 {
                        0 => 0x001F, 1 => 0x03E0, 2 => 0x7C00, _ => 0x7FFF,
                    };
                    test_buf[y * 240 + x] = color;
                }
            }
            display.render(&test_buf);
        } else {
            for _ in 0..CHUNKS_PER_FRAME {
                gba.run_cycles(CHUNK_CYCLES);
                // Pump audio after each chunk
                if let Some((ref audio_buf, ref _device)) = audio_state {
                    let n = gba.drain_audio(&mut audio_tmp);
                    if n > 0 {
                        audio_buf.push_samples(&audio_tmp[..n]);
                        if let Some(w) = wav_dump.as_mut() {
                            w.append(&audio_tmp[..n]);
                        }
                    }
                }
            }
            display.render(gba.framebuffer());

            if dump_pc {
                frame_count += 1;
                // Sample more aggressively (every 30 frames = 0.5s) and ALSO
                // every frame once we detect PC is in unmapped memory.
                let thumb = gba.cpu.cpsr.thumb();
                let pc = if gba.cpu.pipeline_flushed {
                    gba.cpu.regs[15]
                } else if thumb {
                    gba.cpu.regs[15].wrapping_sub(4)
                } else {
                    gba.cpu.regs[15].wrapping_sub(8)
                };
                let pc_in_valid = matches!(pc >> 24, 0x00 | 0x02 | 0x03 | 0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D);
                if std::env::var("INSTR_TRACE_RING").is_ok() && gba_core::trace_is_frozen() {
                    static DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if !DUMPED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        eprintln!("=== ESCAPE DETECTED: PC=0x{:08X} mode={:?} thumb={} sp=0x{:08X} lr=0x{:08X} ===",
                            pc, gba.cpu.cpsr.mode(), thumb, gba.cpu.regs[13], gba.cpu.regs[14]);
                        gba_core::dump_trace_ring();
                        eprintln!("=== END TRACE — exiting ===");
                        break 'running;
                    }
                }
                let want_dump = frame_count % 30 == 0 || (!pc_in_valid && frame_count % 5 == 0);
                if want_dump {
                    let dispstat = gba.bus.io.dispstat;
                    let dispcnt = gba.bus.io.dispcnt;
                    let irq_handler_ptr = gba.bus.read32(0x0300_7FFC);
                    eprintln!(
                        "[PC] f={} pc=0x{:08X} {} mode={:?} halt={} dispcnt=0x{:04X} dispstat=0x{:04X} ie=0x{:04X} ir=0x{:04X} usr_irq=0x{:08X} irqs={} vbl={} | r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X} r4=0x{:08X} r5=0x{:08X} r6=0x{:08X} r7=0x{:08X} sp=0x{:08X} lr=0x{:08X}",
                        frame_count, pc,
                        if thumb {"T"} else {"A"},
                        gba.cpu.cpsr.mode(),
                        gba.cpu.halted,
                        dispcnt,
                        dispstat,
                        gba.bus.interrupt.ie,
                        gba.bus.interrupt.ir,
                        irq_handler_ptr,
                        gba.cpu.irq_entries,
                        gba.vblank_irqs_raised,
                        gba.cpu.regs[0], gba.cpu.regs[1], gba.cpu.regs[2], gba.cpu.regs[3],
                        gba.cpu.regs[4], gba.cpu.regs[5], gba.cpu.regs[6], gba.cpu.regs[7],
                        gba.cpu.regs[13], gba.cpu.regs[14],
                    );
                    // One-time dump of the user IRQ handler at [0x03007FFC].
                    // Useful when triaging a "boots but stays black" game —
                    // tells you whether the handler was installed and what
                    // it does on entry.
                    static DUMPED_IRQ: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !DUMPED_IRQ.swap(true, std::sync::atomic::Ordering::Relaxed)
                        && irq_handler_ptr != 0
                    {
                        let base = irq_handler_ptr & !1;
                        eprint!("[IRQ_CODE] @0x{:08X}:", base);
                        for i in 0..16 {
                            let w = gba.bus.read32(base + i * 4);
                            eprint!(" {:08X}", w);
                        }
                        eprintln!();
                    }
                }
            }
        }

        // Pacing: audio-synced if audio enabled, else wall-clock 60 Hz.
        if let Some((ref audio_buf, ref _device)) = audio_state {
            // Wait until the buffer drains to target level. SDL2 is the master
            // clock — we only run the emulator as fast as the audio callback
            // consumes samples. This keeps latency bounded and prevents the
            // burst-production artifacts that cause "up and down" noise.
            while audio_buf.level() > audio::BUFFER_HIGH {
                std::thread::sleep(Duration::from_millis(1));
                if audio_buf.level() <= audio::BUFFER_TARGET { break; }
            }
        } else {
            let elapsed = frame_start.elapsed();
            if elapsed < frame_duration {
                std::thread::sleep(frame_duration - elapsed);
            }
        }
    }

    // Save .sav on exit
    save_sav(&gba, &sav);
}
