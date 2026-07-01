//! End-to-end EEPROM test: drive DMA3 through the real bus to write a value
//! into the EEPROM chip and read it back, exercising bus routing, the serial
//! protocol, read-advance, and DMA-length width detection.

use gba_core::Gba;

const EEPROM_DST: u32 = 0x0DFF_FF00; // canonical EEPROM DMA port
const SRC_BUF: u32 = 0x0200_0000; // EWRAM: command/data bit stream
const OUT_BUF: u32 = 0x0200_1000; // EWRAM: read-back bit stream

// DMA3 registers.
const DMA3SAD: u32 = 0x0400_00D4;
const DMA3DAD: u32 = 0x0400_00D8;
const DMA3CNT_L: u32 = 0x0400_00DC;
const DMA3CNT_H: u32 = 0x0400_00DE;

/// Enable | immediate | 16-bit | dest-fixed | src-increment.
const DMA_WRITE_CTRL: u16 = 0x8000 | (2 << 5);
/// Enable | immediate | 16-bit | dest-increment | src-fixed.
const DMA_READ_CTRL: u16 = 0x8000 | (2 << 7);

fn make_eeprom_rom() -> Vec<u8> {
    // 1 MB ROM (≤16 MB → whole 0x0D region is EEPROM) carrying the signature
    // that `detect_backup_type` scans for.
    let mut rom = vec![0u8; 1024 * 1024];
    rom[0x100..0x100 + 11].copy_from_slice(b"EEPROM_V124");
    rom
}

/// Stage `count` serial bits (MSB first) of `val` into EWRAM at `SRC_BUF`,
/// one bit per halfword (in bit 0), then DMA them to the EEPROM port.
fn dma_to_eeprom(gba: &mut Gba, bits: &[u8]) {
    for (i, &b) in bits.iter().enumerate() {
        gba.bus.write16(SRC_BUF + (i as u32) * 2, b as u16);
    }
    gba.bus.write32(DMA3SAD, SRC_BUF);
    gba.bus.write32(DMA3DAD, EEPROM_DST);
    gba.bus.write16(DMA3CNT_L, bits.len() as u16);
    gba.bus.write16(DMA3CNT_H, DMA_WRITE_CTRL); // triggers immediate DMA
}

/// DMA `count` bits back from the EEPROM port into `OUT_BUF`, return them.
fn dma_from_eeprom(gba: &mut Gba, count: u32) -> Vec<u8> {
    gba.bus.write32(DMA3SAD, EEPROM_DST);
    gba.bus.write32(DMA3DAD, OUT_BUF);
    gba.bus.write16(DMA3CNT_L, count as u16);
    gba.bus.write16(DMA3CNT_H, DMA_READ_CTRL);
    (0..count)
        .map(|i| (gba.bus.read16(OUT_BUF + i * 2) & 1) as u8)
        .collect()
}

fn bits_msb(val: u64, n: u32) -> Vec<u8> {
    (0..n).rev().map(|i| ((val >> i) & 1) as u8).collect()
}

/// Build a 14-bit-address write command: 10 + addr(14) + data(64) + dummy(1).
fn write_cmd(addr: u16, data: u64) -> Vec<u8> {
    let mut v = vec![1, 0];
    v.extend(bits_msb(addr as u64, 14));
    v.extend(bits_msb(data, 64));
    v.push(0);
    v // 81 units
}

/// Build a 14-bit-address read command: 11 + addr(14) + dummy(1).
fn read_cmd(addr: u16) -> Vec<u8> {
    let mut v = vec![1, 1];
    v.extend(bits_msb(addr as u64, 14));
    v.push(0);
    v // 17 units
}

fn bits_to_u64(bits: &[u8]) -> u64 {
    bits.iter().fold(0u64, |acc, &b| (acc << 1) | b as u64)
}

#[test]
fn eeprom_dma_roundtrip_8k() {
    let mut gba = Gba::new(None, make_eeprom_rom());

    let addr = 0x123u16; // a 14-bit block index
    let data = 0x0011_2233_4455_6677u64;

    // Write, then read back.
    dma_to_eeprom(&mut gba, &write_cmd(addr, data)); // 81 units → detects 14-bit
    dma_to_eeprom(&mut gba, &read_cmd(addr)); // 17 units → sets up read
    let out = dma_from_eeprom(&mut gba, 68); // 4 dummy + 64 data

    assert_eq!(&out[..4], &[0, 0, 0, 0], "leading dummy bits should be 0");
    assert_eq!(bits_to_u64(&out[4..]), data, "read-back must match written data");

    // A distinct second block must be independent.
    let data2 = 0xDEAD_BEEF_CAFE_F00Du64;
    dma_to_eeprom(&mut gba, &write_cmd(0x001, data2));
    dma_to_eeprom(&mut gba, &read_cmd(0x001));
    let out2 = dma_from_eeprom(&mut gba, 68);
    assert_eq!(bits_to_u64(&out2[4..]), data2);

    // Original block must be untouched by the second write.
    dma_to_eeprom(&mut gba, &read_cmd(addr));
    let out3 = dma_from_eeprom(&mut gba, 68);
    assert_eq!(bits_to_u64(&out3[4..]), data, "block 0x123 must survive");

    // The written save must survive an export/import cycle.
    let saved = gba.export_save().expect("eeprom has save data");
    let mut gba2 = Gba::new(None, make_eeprom_rom());
    gba2.import_save(&saved);
    dma_to_eeprom(&mut gba2, &read_cmd(addr));
    let out4 = dma_from_eeprom(&mut gba2, 68);
    assert_eq!(bits_to_u64(&out4[4..]), data, "save must persist across reload");
}
