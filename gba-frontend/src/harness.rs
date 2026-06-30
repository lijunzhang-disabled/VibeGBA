//! Headless harness: drive the emulator over the Emulator Harness Protocol
//! (EHP) on stdin/stdout, with no SDL window. This is the device-under-test
//! side that the external `emu-agent` (../../emu-agent) talks to.
//!
//! Protocol spec: ../../emu-agent/docs/protocol.md
//!
//! Framing (both directions), all integers little-endian:
//!
//!     u32 total_len | u32 json_len | json bytes | binary blob
//!
//! `total_len` counts everything after itself (4 + json_len + blob_len).
//! Requests are JSON-only; responses may carry a trailing binary blob
//! (framebuffer, audio, save-state, peeked memory). The harness logs to
//! stderr so stdout stays a clean binary channel.

use std::fs;
use std::io::{self, Read, Write};

use gba_core::{Gba, CYCLES_PER_FRAME};
use serde_json::{json, Value};

/// Canonical EHP button bits map 1:1 onto the GBA keypad bits (A=0 .. L=9);
/// bits 10/11 (X/Y) only exist on NDS and are masked off here.
const GBA_BUTTON_MASK: u16 = 0x03FF;

pub fn run(bios: Option<Vec<u8>>, skip_bios: bool) {
    let mut h = Harness::new(bios, skip_bios);
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    loop {
        let (req, blob) = match read_frame(&mut reader) {
            Ok(frame) => frame,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break, // agent closed
            Err(e) => {
                eprintln!("[harness] read error: {e}");
                break;
            }
        };
        let cmd = req.get("cmd").and_then(Value::as_str).unwrap_or("");
        let (resp, out_blob, quit) = h.handle(cmd, &req, blob);
        if let Err(e) = write_frame(&mut writer, &resp, &out_blob) {
            eprintln!("[harness] write error: {e}");
            break;
        }
        if quit {
            break;
        }
    }
}

struct Harness {
    bios: Option<Vec<u8>>,
    skip_bios: bool,
    rom: Option<Vec<u8>>,
    gba: Option<Gba>,
    frame_index: u64,
    buttons: u16,
    audio_accum: Vec<i16>,
    audio_tmp: Vec<i16>,
    /// Cartridge backup (.sav) bytes, re-applied on every (re)build so a loaded
    /// save survives `reset` — a real cartridge save isn't wiped by a console reset.
    save: Option<Vec<u8>>,
}

impl Harness {
    fn new(bios: Option<Vec<u8>>, skip_bios: bool) -> Self {
        Harness {
            bios,
            skip_bios,
            rom: None,
            gba: None,
            frame_index: 0,
            buttons: 0,
            audio_accum: Vec::with_capacity(96_000),
            audio_tmp: vec![0i16; 8192],
            save: None,
        }
    }

    /// (Re)build the Gba from the stored ROM + BIOS.
    fn build(&mut self) -> Result<(), String> {
        let rom = self.rom.clone().ok_or("no ROM loaded")?;
        let mut gba = Gba::new(self.bios.clone(), rom);
        if self.skip_bios {
            gba.cpu = gba_core::arm7tdmi::Cpu::new_skip_bios();
        }
        // Re-apply the cartridge save so it survives reset/rebuild.
        if let Some(save) = &self.save {
            gba.import_save(save);
        }
        self.gba = Some(gba);
        self.frame_index = 0;
        self.buttons = 0;
        self.audio_accum.clear();
        Ok(())
    }

    fn gba_mut(&mut self) -> Result<&mut Gba, String> {
        self.gba.as_mut().ok_or_else(|| "no ROM loaded".to_string())
    }

    /// Dispatch one command. Returns (response_json, response_blob, should_quit).
    fn handle(&mut self, cmd: &str, req: &Value, blob: Vec<u8>) -> (Value, Vec<u8>, bool) {
        match cmd {
            "hello" => (self.hello(), vec![], false),
            "bye" => (json!({"ok": true}), vec![], true),
            "load_rom" => wrap(self.load_rom(req)),
            "load_bios" => wrap(self.load_bios(req)),
            "load_save" => wrap(self.load_save(req)),
            "reset" => wrap(self.build()),
            "set_input" => wrap(self.set_input(req)),
            "step" => return self.step(req),
            "get_video" => return self.get_video(),
            "get_audio" => return self.get_audio(),
            "save_state" => return self.save_state(),
            "load_state" => wrap(self.load_state(blob)),
            "peek" => return self.peek(req),
            other => (
                json!({"ok": false, "error": format!("unknown cmd {other:?}")}),
                vec![],
                false,
            ),
        }
    }

    fn hello(&self) -> Value {
        json!({
            "ok": true,
            "engine": "gba",
            "version": env!("CARGO_PKG_VERSION"),
            "screens": [{"w": 240, "h": 160, "fmt": "BGR555"}],
            "audio": {"rate": gba_core::apu::OUTPUT_SAMPLE_RATE, "channels": 2, "fmt": "s16le"},
            "buttons": ["A","B","Select","Start","Right","Left","Up","Down","R","L"],
            "has_touch": false,
            "has_extkeys": false,
            "peek": true,
        })
    }

    fn load_rom(&mut self, req: &Value) -> Result<(), String> {
        let path = req.get("path").and_then(Value::as_str).ok_or("missing path")?;
        let data = fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        self.rom = Some(data);
        self.build()
    }

    fn load_bios(&mut self, req: &Value) -> Result<(), String> {
        let path = req.get("path").and_then(Value::as_str).ok_or("missing path")?;
        let data = fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        self.bios = Some(data);
        // Rebuild only if a ROM is already loaded, so BIOS takes effect.
        if self.rom.is_some() {
            self.build()?;
        }
        Ok(())
    }

    fn load_save(&mut self, req: &Value) -> Result<(), String> {
        let path = req.get("path").and_then(Value::as_str).ok_or("missing path")?;
        let data = fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        self.gba_mut()?.import_save(&data);
        self.save = Some(data); // persist so it survives a later reset
        Ok(())
    }

    fn set_input(&mut self, req: &Value) -> Result<(), String> {
        let buttons = req.get("buttons").and_then(Value::as_u64).unwrap_or(0) as u16;
        self.buttons = buttons & GBA_BUTTON_MASK;
        // touch / extkeys are not applicable to GBA; ignored by design.
        Ok(())
    }

    fn step(&mut self, req: &Value) -> (Value, Vec<u8>, bool) {
        let frames = req.get("frames").and_then(Value::as_u64).unwrap_or(1).max(1);
        let buttons = self.buttons;
        let gba = match self.gba.as_mut() {
            Some(g) => g,
            None => return (json!({"ok": false, "error": "no ROM loaded"}), vec![], false),
        };
        for _ in 0..frames {
            gba.set_keypad_state(buttons);
            gba.run_cycles(CYCLES_PER_FRAME);
            if std::env::var("PC_SAMPLE").is_ok() {
                eprintln!("[PC] frame={} pc=0x{:08X} halted={} thumb={} ie=0x{:04X} ir=0x{:04X}",
                    self.frame_index, gba.cpu.regs[15], gba.cpu.halted,
                    gba.cpu.cpsr.thumb(), gba.bus.interrupt.ie, gba.bus.interrupt.ir);
            }
            // Drain audio every frame so the APU's bounded buffer never
            // overflows across a multi-frame step.
            loop {
                let n = gba.drain_audio(&mut self.audio_tmp);
                self.audio_accum.extend_from_slice(&self.audio_tmp[..n]);
                if n < self.audio_tmp.len() {
                    break;
                }
            }
        }
        self.frame_index += frames;
        (json!({"ok": true, "frame_index": self.frame_index}), vec![], false)
    }

    fn get_video(&mut self) -> (Value, Vec<u8>, bool) {
        let gba = match self.gba.as_ref() {
            Some(g) => g,
            None => return (json!({"ok": false, "error": "no ROM loaded"}), vec![], false),
        };
        let fb = gba.framebuffer(); // &[u16], 240x160 BGR555
        let mut blob = Vec::with_capacity(fb.len() * 2);
        for &px in fb {
            blob.extend_from_slice(&px.to_le_bytes());
        }
        let hdr = json!({
            "ok": true,
            "screens": [{
                "index": 0, "w": 240, "h": 160, "fmt": "BGR555",
                "offset": 0, "len": blob.len(),
            }],
        });
        (hdr, blob, false)
    }

    fn get_audio(&mut self) -> (Value, Vec<u8>, bool) {
        let nsamples = self.audio_accum.len() / 2;
        let mut blob = Vec::with_capacity(self.audio_accum.len() * 2);
        for &s in &self.audio_accum {
            blob.extend_from_slice(&s.to_le_bytes());
        }
        self.audio_accum.clear();
        let hdr = json!({
            "ok": true,
            "rate": gba_core::apu::OUTPUT_SAMPLE_RATE,
            "channels": 2,
            "fmt": "s16le",
            "nsamples": nsamples,
        });
        (hdr, blob, false)
    }

    fn save_state(&mut self) -> (Value, Vec<u8>, bool) {
        match self.gba.as_ref().map(|g| g.save_state()) {
            Some(Ok(data)) => (json!({"ok": true}), data, false),
            Some(Err(e)) => (json!({"ok": false, "error": format!("save_state: {e}")}), vec![], false),
            None => (json!({"ok": false, "error": "no ROM loaded"}), vec![], false),
        }
    }

    fn load_state(&mut self, blob: Vec<u8>) -> Result<(), String> {
        self.gba_mut()?
            .load_state(&blob)
            .map_err(|e| format!("load_state: {e}"))
    }

    fn peek(&mut self, req: &Value) -> (Value, Vec<u8>, bool) {
        let addr = req.get("addr").and_then(Value::as_u64).unwrap_or(0) as u32;
        let len = req.get("len").and_then(Value::as_u64).unwrap_or(0) as usize;
        let gba = match self.gba.as_ref() {
            Some(g) => g,
            None => return (json!({"ok": false, "error": "no ROM loaded"}), vec![], false),
        };
        let mut blob = Vec::with_capacity(len);
        for i in 0..len as u32 {
            blob.push(gba.bus.peek8(addr.wrapping_add(i)));
        }
        (json!({"ok": true}), blob, false)
    }
}

/// Turn a `Result<(), String>` into a plain (ok/err json, no blob, no quit).
fn wrap(r: Result<(), String>) -> (Value, Vec<u8>, bool) {
    match r {
        Ok(()) => (json!({"ok": true}), vec![], false),
        Err(e) => (json!({"ok": false, "error": e}), vec![], false),
    }
}

// ── EHP framing ────────────────────────────────────────────────────────────

fn read_frame(r: &mut impl Read) -> io::Result<(Value, Vec<u8>)> {
    let mut hdr = [0u8; 8];
    r.read_exact(&mut hdr)?;
    let total_len = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
    let json_len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
    if total_len < 4 + json_len {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame lengths"));
    }
    let mut json_bytes = vec![0u8; json_len];
    r.read_exact(&mut json_bytes)?;
    let mut blob = vec![0u8; total_len - 4 - json_len];
    r.read_exact(&mut blob)?;
    let v: Value = serde_json::from_slice(&json_bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((v, blob))
}

fn write_frame(w: &mut impl Write, header: &Value, blob: &[u8]) -> io::Result<()> {
    let json_bytes = serde_json::to_vec(header)?;
    let total_len = (4 + json_bytes.len() + blob.len()) as u32;
    w.write_all(&total_len.to_le_bytes())?;
    w.write_all(&(json_bytes.len() as u32).to_le_bytes())?;
    w.write_all(&json_bytes)?;
    w.write_all(blob)?;
    w.flush()
}
