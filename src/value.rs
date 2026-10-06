//! JS values, NaN-boxed into 64 bits with JavaScriptCore's pointer-favoring
//! encoding:
//!
//! ```text
//! Pointer   0000:PPPP:PPPP:PPPP   (48-bit pointer, low bit pattern ≠ 0b?1?)
//! Double    0002:****:****:****   (raw bits + 2^49 = DOUBLE_ENCODE_OFFSET)
//!     to    FFFC:****:****:****
//! Int32     FFFE:0000:IIII:IIII   (NUMBER_TAG | u32 payload)
//! ```
//!
//! Immediates live in the low bits of the pointer range, at addresses no
//! allocation can have:
//!
//! ```text
//! empty     0x00   (never a JS value; marks holes / uninitialised slots)
//! null      0x02
//! false     0x06
//! true      0x07
//! undefined 0x0a
//! ```
//!
//! Costs: a pointer is used as-is. An int32 test is one compare, and the
//! payload is just the low 32 bits. Decoding a double is one subtract.
//!
//! Doubles must not be impure NaNs: a NaN with a high payload, plus the
//! offset, would wrap into the pointer range. [`Value::from_f64`]
//! canonicalises NaNs. Hardware arithmetic on canonical inputs only produces
//! the default NaN (`0x7FF8…` on arm64, `0xFFF8…` on x86), and both encode
//! safely.

use std::fmt;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Value(u64);

impl Value {
    /// Set in the top 15 bits of every number; all 15 set means int32.
    pub const NUMBER_TAG: u64 = 0xFFFE_0000_0000_0000;
    pub const DOUBLE_ENCODE_OFFSET: u64 = 1 << 49;
    pub const OTHER_TAG: u64 = 0x2;
    pub const BOOL_TAG: u64 = 0x4;
    pub const UNDEFINED_TAG: u64 = 0x8;
    /// Clear in exactly the cell (heap pointer) values.
    pub const NOT_CELL_MASK: u64 = Self::NUMBER_TAG | Self::OTHER_TAG;

    pub const EMPTY: Value = Value(0);
    pub const NULL: Value = Value(Self::OTHER_TAG);
    pub const FALSE: Value = Value(Self::OTHER_TAG | Self::BOOL_TAG);
    pub const TRUE: Value = Value(Self::OTHER_TAG | Self::BOOL_TAG | 1);
    pub const UNDEFINED: Value = Value(Self::OTHER_TAG | Self::UNDEFINED_TAG);

    /// The canonical NaN, as JSC's `PNaN`.
    const CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;

    #[inline(always)]
    pub const fn from_bits(bits: u64) -> Self {
        Value(bits)
    }

    #[inline(always)]
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    #[inline(always)]
    pub const fn from_i32(i: i32) -> Self {
        Value(Self::NUMBER_TAG | i as u32 as u64)
    }

    /// Box a double as a double, even if it is integral.
    #[inline(always)]
    pub fn from_f64(d: f64) -> Self {
        let bits = if d.is_nan() {
            Self::CANONICAL_NAN
        } else {
            d.to_bits()
        };
        Value(bits.wrapping_add(Self::DOUBLE_ENCODE_OFFSET))
    }

    /// Box the result of float arithmetic on already-boxed numbers, skipping
    /// NaN canonicalisation. Sound because such arithmetic can only yield a
    /// NaN propagated from an input (already canonical) or the hardware
    /// default NaN (`0x7FF8…` / `0xFFF8…`), both of which encode as doubles.
    /// Use [`from_f64`](Self::from_f64) for doubles from anywhere else.
    #[inline(always)]
    pub fn from_f64_arith(d: f64) -> Self {
        Value(d.to_bits().wrapping_add(Self::DOUBLE_ENCODE_OFFSET))
    }

    /// Box a JS number result: int32 if the value is exactly one (not -0),
    /// otherwise a double, as JSC's `jsNumber(double)` does. The interpreter's
    /// number slow paths skip this (like LLInt) and use
    /// [`from_f64_arith`](Self::from_f64_arith); the generic runtime uses it.
    #[inline]
    pub fn number(d: f64) -> Self {
        let i = d as i32;
        if i as f64 == d && !(i == 0 && d.is_sign_negative()) {
            Self::from_i32(i)
        } else {
            Self::from_f64(d)
        }
    }

    #[inline(always)]
    pub const fn from_bool(b: bool) -> Self {
        if b { Self::TRUE } else { Self::FALSE }
    }

    #[inline(always)]
    pub const fn is_int32(self) -> bool {
        self.0 >= Self::NUMBER_TAG
    }

    /// Whether `a` and `b` are both int32, with one `and` and one compare.
    /// The AND has all of `NUMBER_TAG`'s bits set only if both operands do.
    #[inline(always)]
    pub const fn both_int32(a: Value, b: Value) -> bool {
        a.0 & b.0 >= Self::NUMBER_TAG
    }

    #[inline(always)]
    pub const fn is_number(self) -> bool {
        self.0 & Self::NUMBER_TAG != 0
    }

    #[inline(always)]
    pub const fn is_double(self) -> bool {
        self.is_number() && !self.is_int32()
    }

    #[inline(always)]
    pub const fn is_cell(self) -> bool {
        self.0 & Self::NOT_CELL_MASK == 0 && self.0 != 0
    }

    #[inline(always)]
    pub const fn is_undefined(self) -> bool {
        self.0 == Self::UNDEFINED.0
    }

    #[inline(always)]
    pub const fn is_null(self) -> bool {
        self.0 == Self::NULL.0
    }

    #[inline(always)]
    pub const fn is_bool(self) -> bool {
        self.0 & !1 == Self::FALSE.0
    }

    /// The int32 payload. Only meaningful if [`is_int32`](Self::is_int32).
    #[inline(always)]
    pub const fn as_i32_unchecked(self) -> i32 {
        self.0 as u32 as i32
    }

    /// The double payload. Only meaningful if [`is_double`](Self::is_double).
    #[inline(always)]
    pub fn as_f64_unchecked(self) -> f64 {
        f64::from_bits(self.0.wrapping_sub(Self::DOUBLE_ENCODE_OFFSET))
    }

    /// Any number as `f64`, or `None` if not a number.
    #[inline]
    pub fn as_number(self) -> Option<f64> {
        if self.is_int32() {
            Some(self.as_i32_unchecked() as f64)
        } else if self.is_number() {
            Some(self.as_f64_unchecked())
        } else {
            None
        }
    }

    /// ECMAScript ToNumber, for the primitives representable so far.
    pub fn to_number(self) -> f64 {
        if let Some(d) = self.as_number() {
            d
        } else if self.is_undefined() {
            f64::NAN
        } else if self.is_null() || self == Self::FALSE {
            0.0
        } else if self == Self::TRUE {
            1.0
        } else {
            todo!("ToNumber on heap values")
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_int32() {
            write!(f, "{}", self.as_i32_unchecked())
        } else if self.is_number() {
            write!(f, "{:?}", self.as_f64_unchecked())
        } else if *self == Self::EMPTY {
            f.write_str("<empty>")
        } else if self.is_undefined() {
            f.write_str("undefined")
        } else if self.is_null() {
            f.write_str("null")
        } else if self.is_bool() {
            write!(f, "{}", *self == Self::TRUE)
        } else {
            write!(f, "<cell {:#x}>", self.0)
        }
    }
}

/// [`Value::NUMBER_TAG`], kept in a register for the whole run.
///
/// x86 instructions can't take a 64-bit immediate. With the tag as a
/// constant, every int32 test becomes `shr` + `cmp`, and every int32 box a
/// `movabs`. JSC pins it in a register for the same reason. It's passed as a
/// handler argument, created once per run by [`NumberTag::new`], so LLVM
/// can't constant-fold it back in. Shared by the interpreter
/// (`src/interp.rs`) and the JIT stencils (`stencils/stencils.rs`).
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct NumberTag(u64);

impl Default for NumberTag {
    fn default() -> Self {
        Self::new()
    }
}

impl NumberTag {
    /// The tag, laundered through `black_box` so that code taking it as an
    /// argument can't see it is constant.
    #[inline(always)]
    pub fn new() -> Self {
        NumberTag(std::hint::black_box(Value::NUMBER_TAG))
    }

    #[inline(always)]
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    #[inline(always)]
    pub fn is_int32(self, v: Value) -> bool {
        v.to_bits() >= self.0
    }

    /// See [`Value::both_int32`].
    #[inline(always)]
    pub fn both_int32(self, a: Value, b: Value) -> bool {
        a.to_bits() & b.to_bits() >= self.0
    }

    #[inline(always)]
    pub fn box_i32(self, i: i32) -> Value {
        Value::from_bits(self.0 | i as u32 as u64)
    }

    /// Box an `I32`-state accumulator. Its high bits are 0 or the tag, so
    /// this is one `or` with no truncation.
    #[inline(always)]
    pub fn rebox(self, acc: Value) -> Value {
        Value::from_bits(self.0 | acc.to_bits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_round_trip() {
        for i in [0, 1, -1, i32::MIN, i32::MAX, 42] {
            let v = Value::from_i32(i);
            assert!(v.is_int32() && v.is_number() && !v.is_double() && !v.is_cell());
            assert_eq!(v.as_i32_unchecked(), i);
        }
    }

    #[test]
    fn doubles_round_trip() {
        for d in [
            0.5,
            -0.0,
            1e300,
            -1e-300,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MIN_POSITIVE,
        ] {
            let v = Value::from_f64(d);
            assert!(v.is_double() && !v.is_int32() && !v.is_cell(), "{d}");
            assert_eq!(v.as_f64_unchecked().to_bits(), d.to_bits());
        }
    }

    #[test]
    fn impure_nans_are_canonicalised() {
        for bits in [
            0x7FF8_0000_0000_0000,
            0xFFF8_0000_0000_0000,
            0xFFFF_FFFF_FFFF_FFFF,
            0x7FF0_0000_0000_0001,
        ] {
            let v = Value::from_f64(f64::from_bits(bits));
            assert!(v.is_double(), "{bits:#x} -> {:#x}", v.to_bits());
            assert!(v.as_f64_unchecked().is_nan());
        }
    }

    #[test]
    fn hardware_default_nan_encodes_as_double() {
        let nan = std::hint::black_box(f64::INFINITY) - std::hint::black_box(f64::INFINITY);
        let v = Value::from_bits(nan.to_bits().wrapping_add(Value::DOUBLE_ENCODE_OFFSET));
        assert!(v.is_double());
    }

    #[test]
    fn number_normalises_to_int32() {
        assert!(Value::number(3.0).is_int32());
        assert!(Value::number(-2147483648.0).is_int32());
        assert!(Value::number(-0.0).is_double());
        assert!(Value::number(2147483648.0).is_double());
        assert!(Value::number(0.5).is_double());
        assert!(Value::number(f64::NAN).is_double());
    }

    #[test]
    fn both_int32() {
        let (i, d) = (Value::from_i32(-1), Value::from_f64(-1.5));
        assert!(Value::both_int32(i, Value::from_i32(7)));
        assert!(!Value::both_int32(i, d));
        assert!(!Value::both_int32(d, i));
        assert!(!Value::both_int32(i, Value::UNDEFINED));
        // The most int-like double encoding (NaN-space negative) still fails.
        let neg_nan = Value::from_bits(0xFFFC_0000_0000_0000);
        assert!(!Value::both_int32(neg_nan, i));
    }

    #[test]
    fn immediates() {
        for v in [
            Value::NULL,
            Value::TRUE,
            Value::FALSE,
            Value::UNDEFINED,
            Value::EMPTY,
        ] {
            assert!(!v.is_number() && !v.is_cell(), "{v:?}");
        }
        assert!(Value::TRUE.is_bool() && Value::FALSE.is_bool() && !Value::NULL.is_bool());
        assert!(Value::from_bits(0x1000).is_cell());
    }

    #[test]
    fn to_number() {
        assert!(Value::UNDEFINED.to_number().is_nan());
        assert_eq!(Value::NULL.to_number(), 0.0);
        assert_eq!(Value::TRUE.to_number(), 1.0);
        assert_eq!(Value::from_i32(-3).to_number(), -3.0);
    }
}
