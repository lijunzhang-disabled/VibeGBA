// ARM + THUMB disassembler.
// Phase 8: Convert instructions to readable strings for the debugger.

/// Disassemble an ARM instruction at the given address.
pub fn disasm_arm(_opcode: u32, _addr: u32) -> String {
    // TODO: Phase 8
    format!("??? (0x{:08X})", _opcode)
}

/// Disassemble a THUMB instruction at the given address.
pub fn disasm_thumb(_opcode: u16, _addr: u32) -> String {
    // TODO: Phase 8
    format!("??? (0x{:04X})", _opcode)
}
