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
/// Dump EWRAM (256 KB) to /tmp/ewram-<N>.bin where N is the next free
/// number. Triggered by pressing 'D' in the frontend. Used for diff-
/// based debugging — snapshot at point A and B, find changed bytes.
fn dump_ewram(gba: &Gba) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = format!("/tmp/ewram-{:03}.bin", n);
    match fs::write(&path, gba.bus.ewram_ref()) {
        Ok(()) => eprintln!("EWRAM snapshot written to {}  ({} bytes)",
                            path, gba.bus.ewram_ref().len()),
        Err(e) => eprintln!("Failed to write EWRAM snapshot: {}", e),
    }
}

/// Dump the 32 KB IWRAM (0x03000000..0x03007FFF) to /tmp/iwram-<N>.bin.
/// Triggered by 'I'. Useful for inspecting runtime-loaded code (IRQ
/// handlers, vsync wrappers, etc.) that don't live in ROM.
fn dump_iwram(gba: &Gba) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = format!("/tmp/iwram-{:03}.bin", n);
    match fs::write(&path, gba.bus.iwram_ref()) {
        Ok(()) => eprintln!("IWRAM snapshot written to {}  ({} bytes)",
                            path, gba.bus.iwram_ref().len()),
        Err(e) => eprintln!("Failed to write IWRAM snapshot: {}", e),
    }
}

/// Dump the metatile behavior at the player's current tile (M key).
/// Pokemon Emerald (BPEE US) specific — walks gObjectEvents → gBackupMapLayout
/// → gMapHeader.mapLayout → tileset.metatileAttributes to extract the
/// behavior byte for the tile the player is standing on.
fn dump_metatile_at_player(gba: &Gba) {
    let bus = &gba.bus;
    let p = |a| bus.peek8(a);
    let p32 = |a| bus.peek32(a);

    // gObjectEvents[0].currentCoords (BPEE: 0x02037350, +0x10 = x, +0x12 = y).
    // currentCoords includes MAP_OFFSET (=7); they are direct indices into the
    // bordered map array, no further adjustment needed.
    let px = i16::from_le_bytes([p(0x02037360), p(0x02037361)]);
    let py = i16::from_le_bytes([p(0x02037362), p(0x02037363)]);

    // gBackupMapLayout (BPEE IWRAM 0x03005DC0): { s32 width; s32 height; u16 *map; }
    let width   = p32(0x03005DC0) as i32;
    let height  = p32(0x03005DC4) as i32;
    let map_ptr = p32(0x03005DC8);

    let idx = (py as i32) * width + (px as i32);
    let mt_addr = map_ptr.wrapping_add((idx as u32).wrapping_mul(2));
    let mt_raw  = u16::from_le_bytes([p(mt_addr), p(mt_addr.wrapping_add(1))]);
    let mt_id   = mt_raw & 0x03FF;

    // gMapHeader (BPEE EWRAM 0x02037318): first field is mapLayout pointer.
    let layout_ptr   = p32(0x02037318);
    // struct MapLayout: { width, height, *border, *map, *primaryTileset, *secondaryTileset, ... }
    let prim_tileset = p32(layout_ptr.wrapping_add(0x10));
    let sec_tileset  = p32(layout_ptr.wrapping_add(0x14));

    // struct Tileset: metatileAttributes is at offset 0x10. In original BPEE
    // the attributes are u16 (2 bytes each), NOT u32 — confirmed by raw ROM
    // dump. The low byte is the behavior, the high byte holds terrain/layer
    // flags. NUM_METATILES_IN_PRIMARY = 0x200.
    let (tileset_ptr, local_id) = if mt_id < 0x200 {
        (prim_tileset, mt_id as u32)
    } else {
        (sec_tileset, (mt_id - 0x200) as u32)
    };
    let attrs_ptr  = p32(tileset_ptr.wrapping_add(0x10));
    let attr_addr  = attrs_ptr.wrapping_add(local_id.wrapping_mul(2));
    let attrs_word = u16::from_le_bytes([p(attr_addr), p(attr_addr.wrapping_add(1))]);
    let behavior   = (attrs_word & 0xFF) as u8;

    // gPlayerAvatar (BPEE 0x02037590): flags(0), transitionFlags(1),
    // runningState(2), tileTransitionState(3). The "just-landed-from-warp"
    // state is expected to live in one of these (most likely flags or
    // tileTransitionState) — we log all four so the diff is greppable.
    let pa_flags  = p(0x02037590);
    let pa_trans  = p(0x02037591);
    let pa_run    = p(0x02037592);
    let pa_tile   = p(0x02037593);

    // gObjectEvents[0] first 4 bytes are 32 packed bitfield bits: active,
    // singleMovementActive, triggerGroundEffectsOnMove, triggerGroundEffects-
    // OnStop, disableCoveringGroundEffects, landingJump, heldMovementActive,
    // heldMovementFinished, frozen, … Log the whole word and decode offline.
    let obj_bits  = p32(0x02037350);

    eprintln!(
        "METATILE player=({px},{py}) behavior=0x{behavior:02X} attrs=0x{attrs_word:04X} \
         id=0x{mt_id:03X} layout w={width} h={height} map=0x{map_ptr:08X} idx={idx} raw=0x{mt_raw:04X} \
         layoutPtr=0x{layout_ptr:08X} prim=0x{prim_tileset:08X} sec=0x{sec_tileset:08X} attrsPtr=0x{attrs_ptr:08X} \
         playerAvatar=[flags=0x{pa_flags:02X} trans=0x{pa_trans:02X} run=0x{pa_run:02X} tile=0x{pa_tile:02X}] \
         objBits=0x{obj_bits:08X}"
    );
}

/// Dump game var values for the BPEE Emerald investigation. Triggered by 'V'.
///
/// BPEE's GetVarPointer at 0x0809D648 computes:
///     addr = *gSaveBlock1Ptr + (var_id << 1) + 0xFFFF939C
/// Rearranging, vars[N - VARS_START] lives at:
///     sb1Ptr + 0x139C + (N - 0x4000) * 2
/// i.e. vars[0] = sb1Ptr + 0x139C. (Not 0x1490 — master pokeemerald has
/// re-laid-out SaveBlock1 since BPEE shipped.)
fn dump_vars(gba: &Gba) {
    let bus = &gba.bus;
    let sb1 = bus.peek32(0x03005D8C);
    if sb1 == 0 {
        eprintln!("VARS gSaveBlock1Ptr is null — no map loaded yet?");
        return;
    }
    let p16 = |a: u32| u16::from_le_bytes([bus.peek8(a), bus.peek8(a.wrapping_add(1))]);
    let base = sb1.wrapping_add(0x139C); // vars[0]
    eprintln!("VARS sb1=0x{sb1:08X} vars_base=0x{base:08X}");
    for off in 0x20..0x28 {
        let var_id = 0x4000 + off;
        let addr   = base.wrapping_add((off as u32) * 2);
        let val    = p16(addr);
        let marker = if var_id == 0x4022 { " <-- Map 2 on-frame trigger" } else { "" };
        eprintln!("  var 0x{var_id:04X} @ 0x{addr:08X} = 0x{val:04X}{marker}");
    }
}

/// Walk Map 2's script table (gMapHeader.mapScripts, BPEE +0x08 = 0x02037320),
/// printing every top-level entry. For ON_FRAME_TABLE (type=2) and
/// ON_WARP_INTO_MAP_TABLE (type=4), also dumps the sub-table of
/// {var, value, script_ptr} triples — these are how the map auto-runs
/// scripts when game variables match. Triggered by pressing 'S' in-game.
fn dump_map_scripts(gba: &Gba) {
    let bus = &gba.bus;
    let p   = |a| bus.peek8(a);
    let p16 = |a: u32| u16::from_le_bytes([bus.peek8(a), bus.peek8(a.wrapping_add(1))]);
    // bus.peek32 force-aligns to 4 bytes; map-script entries are 5 bytes each
    // (1 type + 4 ptr) so subsequent ptr fields are unaligned. Build the u32
    // from four byte reads instead.
    let p32u = |a: u32| u32::from_le_bytes([
        bus.peek8(a), bus.peek8(a.wrapping_add(1)),
        bus.peek8(a.wrapping_add(2)), bus.peek8(a.wrapping_add(3)),
    ]);
    let p32 = |a| bus.peek32(a);

    let ms_ptr = p32(0x02037320); // gMapHeader.mapScripts
    eprintln!("MAPSCRIPTS table=0x{ms_ptr:08X}");

    let mut e = ms_ptr;
    for _ in 0..16 {  // safety bound
        let ty = p(e);
        if ty == 0 {
            eprintln!("  end");
            break;
        }
        // Each top-level entry is 1 byte (type) + 4 bytes (script/table ptr) = 5 bytes
        let sub_ptr = p32u(e.wrapping_add(1));
        let label = match ty {
            1 => "ON_LOAD",
            2 => "ON_FRAME_TABLE",
            3 => "ON_TRANSITION",
            4 => "ON_WARP_INTO_MAP_TABLE",
            5 => "ON_RESUME",
            6 => "ON_DIVE_WARP",
            7 => "ON_RETURN_TO_FIELD",
            _ => "?",
        };
        eprintln!("  type=0x{ty:02X} ({label}) ptr=0x{sub_ptr:08X}");

        if ty == 2 || ty == 4 {
            // Sub-table of {u16 var, u16 value, u32 script_ptr} = 8 bytes per entry.
            let mut t = sub_ptr;
            for _ in 0..32 {
                let var = p16(t);
                if var == 0 {
                    eprintln!("    end");
                    break;
                }
                let val    = p16(t.wrapping_add(2));
                let script = p32u(t.wrapping_add(4));
                eprintln!("    var=0x{var:04X} value=0x{val:04X} script=0x{script:08X}");
                t = t.wrapping_add(8);
            }
        }
        e = e.wrapping_add(5);
    }
}

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

    // TILE_TRACE=1 polls player tile coords each frame and dumps the metatile
    // behavior whenever they change. Use for catching short-lived tiles (e.g.
    // landing-then-falling-again in Granite Cave) where pressing M by hand is
    // racy. BPEE Emerald specific.
    let tile_trace = std::env::var("TILE_TRACE").is_ok();
    let mut last_player_xy: (i16, i16) = (i16::MIN, i16::MIN);

    // PERF_TRACE=1: measure real FPS + host busy-time per frame.
    let perf_trace = std::env::var("PERF_TRACE").is_ok();
    let mut perf_window_start = Instant::now();
    let mut perf_busy_ns: u64 = 0;
    let mut perf_frames: u64 = 0;

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
                    // EWRAM snapshot: D — dumps EWRAM to /tmp/ewram-<N>.bin
                    // for diff-based diagnostics (Pokémon map bug, etc).
                    sdl2::keyboard::Keycode::D => dump_ewram(&gba),
                    // IWRAM snapshot: I — dumps IWRAM to /tmp/iwram-<N>.bin
                    // for inspecting runtime-loaded code (IRQ handlers, etc).
                    sdl2::keyboard::Keycode::I => dump_iwram(&gba),
                    // Trace ring on-demand dump: R. Prints the last 256
                    // instructions executed (requires INSTR_TRACE_RING=1).
                    // Useful for "stuck but PC is in valid memory" hangs
                    // where the auto-dump-on-escape never fires.
                    sdl2::keyboard::Keycode::R => {
                        eprintln!("[TRACE_RING manual dump]");
                        gba_core::dump_trace_ring();
                    },
                    // Metatile probe: M — dumps metatile behavior at player tile
                    // (BPEE Emerald specific; for the Granite Cave hole-vs-ladder bug).
                    sdl2::keyboard::Keycode::M => dump_metatile_at_player(&gba),
                    // Map script table: S — walks gMapHeader.mapScripts and dumps
                    // ON_FRAME_TABLE / ON_WARP_INTO_MAP_TABLE var/value/script triples.
                    sdl2::keyboard::Keycode::S => dump_map_scripts(&gba),
                    // Vars probe: V — dumps the values of game vars around 0x4022
                    // (the var that gates the auto-warp on Map 2).
                    sdl2::keyboard::Keycode::V => dump_vars(&gba),
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

            if tile_trace {
                let px = i16::from_le_bytes([gba.bus.peek8(0x02037360), gba.bus.peek8(0x02037361)]);
                let py = i16::from_le_bytes([gba.bus.peek8(0x02037362), gba.bus.peek8(0x02037363)]);
                if (px, py) != last_player_xy {
                    dump_metatile_at_player(&gba);
                    last_player_xy = (px, py);
                }
            }

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
                    // Timer state snapshot — when triaging hangs that look
                    // like "halted waiting for a Timer IRQ", we need to see
                    // whether each timer is enabled, what its counter is,
                    // and whether its IRQ enable bit is set. control fields:
                    //   bit 7 = enable, bit 6 = IRQ enable, bits 0-1 = prescaler.
                    eprintln!(
                        "[TIM] T0 ctl=0x{:04X} cnt=0x{:04X} rld=0x{:04X} | T1 ctl=0x{:04X} cnt=0x{:04X} rld=0x{:04X} | T2 ctl=0x{:04X} T3 ctl=0x{:04X}",
                        gba.bus.timers.timers[0].control, gba.bus.timers.timers[0].counter, gba.bus.timers.timers[0].reload,
                        gba.bus.timers.timers[1].control, gba.bus.timers.timers[1].counter, gba.bus.timers.timers[1].reload,
                        gba.bus.timers.timers[2].control,
                        gba.bus.timers.timers[3].control,
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

        // PERF_TRACE=1: report effective FPS, host busy-time per frame, and
        // audio buffer level every ~1s. Distinguishes "free-running (pacing
        // broken)" from "can't keep up (emulation too slow)" from "fine".
        if perf_trace {
            let busy = frame_start.elapsed();
            perf_busy_ns += busy.as_nanos() as u64;
            perf_frames += 1;
            if perf_window_start.elapsed() >= Duration::from_secs(1) {
                let wall = perf_window_start.elapsed();
                let fps = perf_frames as f64 / wall.as_secs_f64();
                let busy_ms = (perf_busy_ns as f64 / perf_frames as f64) / 1.0e6;
                let lvl = audio_state.as_ref().map(|(b, _)| b.level()).unwrap_or(0);
                eprintln!(
                    "[PERF] fps={fps:.1} busy={busy_ms:.2}ms/frame audio_level={lvl} (target={}, high={})",
                    audio::BUFFER_TARGET, audio::BUFFER_HIGH
                );
                perf_frames = 0;
                perf_busy_ns = 0;
                perf_window_start = Instant::now();
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
