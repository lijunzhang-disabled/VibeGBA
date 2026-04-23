/// Barrel shifter shift types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftType {
    Lsl = 0, // Logical Shift Left
    Lsr = 1, // Logical Shift Right
    Asr = 2, // Arithmetic Shift Right
    Ror = 3, // Rotate Right
}

impl ShiftType {
    pub fn from_u8(val: u8) -> Self {
        match val & 3 {
            0 => ShiftType::Lsl,
            1 => ShiftType::Lsr,
            2 => ShiftType::Asr,
            3 => ShiftType::Ror,
            _ => unreachable!(),
        }
    }
}

/// Perform a barrel shift, returning (result, carry_out).
///
/// `carry_in` is the current C flag (used for shift-by-0 cases).
/// `immediate` indicates whether this is an immediate shift amount (affects shift-by-0 behavior).
#[inline]
pub fn barrel_shift(value: u32, shift_type: ShiftType, amount: u8, carry_in: bool, immediate: bool) -> (u32, bool) {
    match shift_type {
        ShiftType::Lsl => shift_lsl(value, amount, carry_in),
        ShiftType::Lsr => shift_lsr(value, amount, carry_in, immediate),
        ShiftType::Asr => shift_asr(value, amount, carry_in, immediate),
        ShiftType::Ror => shift_ror(value, amount, carry_in, immediate),
    }
}

fn shift_lsl(value: u32, amount: u8, carry_in: bool) -> (u32, bool) {
    match amount {
        0 => (value, carry_in),
        1..=31 => {
            let carry = (value >> (32 - amount)) & 1 != 0;
            (value << amount, carry)
        }
        32 => (0, value & 1 != 0),
        _ => (0, false), // amount > 32
    }
}

fn shift_lsr(value: u32, amount: u8, carry_in: bool, immediate: bool) -> (u32, bool) {
    match amount {
        0 => {
            if immediate {
                // LSR #0 encodes LSR #32
                (0, value >> 31 != 0)
            } else {
                (value, carry_in)
            }
        }
        1..=31 => {
            let carry = (value >> (amount - 1)) & 1 != 0;
            (value >> amount, carry)
        }
        32 => (0, value >> 31 != 0),
        _ => (0, false),
    }
}

fn shift_asr(value: u32, amount: u8, carry_in: bool, immediate: bool) -> (u32, bool) {
    match amount {
        0 => {
            if immediate {
                // ASR #0 encodes ASR #32
                let carry = (value as i32) < 0;
                let result = if carry { 0xFFFF_FFFF } else { 0 };
                (result, carry)
            } else {
                (value, carry_in)
            }
        }
        1..=31 => {
            let carry = ((value as i32) >> (amount - 1)) & 1 != 0;
            ((value as i32 >> amount) as u32, carry)
        }
        _ => {
            // >= 32: result is all sign bits
            let carry = (value as i32) < 0;
            let result = if carry { 0xFFFF_FFFF } else { 0 };
            (result, carry)
        }
    }
}

fn shift_ror(value: u32, amount: u8, carry_in: bool, immediate: bool) -> (u32, bool) {
    match amount {
        0 => {
            if immediate {
                // ROR #0 encodes RRX (Rotate Right Extended through carry)
                let result = (carry_in as u32) << 31 | (value >> 1);
                let carry = value & 1 != 0;
                (result, carry)
            } else {
                (value, carry_in)
            }
        }
        _ => {
            let amount = amount & 31;
            if amount == 0 {
                // Rotate by 32 (or 0 in register case): result unchanged, carry = bit 31
                (value, value >> 31 != 0)
            } else {
                let result = value.rotate_right(amount as u32);
                let carry = result >> 31 != 0;
                (result, carry)
            }
        }
    }
}

/// ALU operation codes for ARM data processing instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluOp {
    And = 0x0,
    Eor = 0x1,
    Sub = 0x2,
    Rsb = 0x3,
    Add = 0x4,
    Adc = 0x5,
    Sbc = 0x6,
    Rsc = 0x7,
    Tst = 0x8,
    Teq = 0x9,
    Cmp = 0xA,
    Cmn = 0xB,
    Orr = 0xC,
    Mov = 0xD,
    Bic = 0xE,
    Mvn = 0xF,
}

impl AluOp {
    pub fn from_u8(val: u8) -> Self {
        match val & 0xF {
            0x0 => AluOp::And,
            0x1 => AluOp::Eor,
            0x2 => AluOp::Sub,
            0x3 => AluOp::Rsb,
            0x4 => AluOp::Add,
            0x5 => AluOp::Adc,
            0x6 => AluOp::Sbc,
            0x7 => AluOp::Rsc,
            0x8 => AluOp::Tst,
            0x9 => AluOp::Teq,
            0xA => AluOp::Cmp,
            0xB => AluOp::Cmn,
            0xC => AluOp::Orr,
            0xD => AluOp::Mov,
            0xE => AluOp::Bic,
            0xF => AluOp::Mvn,
            _ => unreachable!(),
        }
    }

    /// Whether this op is a "test" op (TST, TEQ, CMP, CMN) that doesn't write to Rd.
    pub fn is_test(self) -> bool {
        matches!(self, AluOp::Tst | AluOp::Teq | AluOp::Cmp | AluOp::Cmn)
    }

    /// Whether this op is a "logical" op (results use shifter carry, not ALU carry).
    pub fn is_logical(self) -> bool {
        matches!(
            self,
            AluOp::And | AluOp::Eor | AluOp::Tst | AluOp::Teq
                | AluOp::Orr | AluOp::Mov | AluOp::Bic | AluOp::Mvn
        )
    }
}

/// Perform addition with carry, returning (result, carry, overflow).
#[inline]
pub fn add_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    let result = (a as u64) + (b as u64) + (carry_in as u64);
    let result32 = result as u32;
    let carry = result > 0xFFFF_FFFF;
    let overflow = ((a ^ result32) & (b ^ result32)) >> 31 != 0;
    (result32, carry, overflow)
}

/// Perform subtraction (a - b), returning (result, carry/borrow, overflow).
/// Note: ARM carry flag for SUB is inverted (1 = no borrow).
#[inline]
pub fn sub_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    // a - b - !carry = a + !b + carry
    add_with_carry(a, !b, carry_in)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lsl() {
        assert_eq!(barrel_shift(0x80000000, ShiftType::Lsl, 1, false, false), (0, true));
        assert_eq!(barrel_shift(1, ShiftType::Lsl, 31, false, false), (0x80000000, false));
        assert_eq!(barrel_shift(0xFF, ShiftType::Lsl, 0, true, false), (0xFF, true));
        assert_eq!(barrel_shift(0xFF, ShiftType::Lsl, 0, false, false), (0xFF, false));
    }

    #[test]
    fn test_lsr() {
        assert_eq!(barrel_shift(1, ShiftType::Lsr, 1, false, false), (0, true));
        assert_eq!(barrel_shift(0x80000000, ShiftType::Lsr, 31, false, false), (1, false));
        // LSR #0 as immediate = LSR #32
        assert_eq!(barrel_shift(0x80000000, ShiftType::Lsr, 0, false, true), (0, true));
    }

    #[test]
    fn test_asr() {
        assert_eq!(barrel_shift(0x80000000, ShiftType::Asr, 1, false, false), (0xC0000000, false));
        assert_eq!(barrel_shift(0x80000000, ShiftType::Asr, 31, false, false), (0xFFFFFFFF, false));
        // ASR #0 as immediate = ASR #32
        assert_eq!(barrel_shift(0x80000000, ShiftType::Asr, 0, false, true), (0xFFFFFFFF, true));
        assert_eq!(barrel_shift(0x7FFFFFFF, ShiftType::Asr, 0, false, true), (0, false));
    }

    #[test]
    fn test_ror() {
        assert_eq!(barrel_shift(1, ShiftType::Ror, 1, false, false), (0x80000000, true));
        assert_eq!(barrel_shift(0x80000000, ShiftType::Ror, 1, false, false), (0x40000000, false));
    }

    #[test]
    fn test_rrx() {
        // ROR #0 as immediate = RRX
        assert_eq!(barrel_shift(1, ShiftType::Ror, 0, true, true), (0x80000000, true));
        assert_eq!(barrel_shift(1, ShiftType::Ror, 0, false, true), (0, true));
        assert_eq!(barrel_shift(0, ShiftType::Ror, 0, true, true), (0x80000000, false));
    }

    #[test]
    fn test_add_with_carry() {
        assert_eq!(add_with_carry(0xFFFFFFFF, 1, false), (0, true, false));
        assert_eq!(add_with_carry(0x7FFFFFFF, 1, false), (0x80000000, false, true));
        assert_eq!(add_with_carry(0x80000000, 0x80000000, false), (0, true, true));
    }

    #[test]
    fn test_sub_with_carry() {
        // SUB: a - b = a + !b + 1 (carry_in=true for plain subtraction)
        assert_eq!(sub_with_carry(5, 3, true), (2, true, false));
        assert_eq!(sub_with_carry(3, 5, true), (0xFFFFFFFE, false, false));
        assert_eq!(sub_with_carry(0, 1, true), (0xFFFFFFFF, false, false));
    }
}
