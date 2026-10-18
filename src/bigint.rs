//! BigInt (ECMA-262 §6.1.6.2, §21.2): arbitrary-precision integers, a
//! primitive kept in an immutable cell of class `BigInt` (as a symbol is):
//! its sign in `elems_cap` (1: negative), its magnitude's 32-bit limbs, least
//! significant first and none of them a leading zero (zero has none), in
//! `elems` (a buffer of bytes), their count in `elems_len`. Its shape's
//! prototype is `BigInt.prototype`, for its methods.
//!
//! The arithmetic is [`Big`]'s, on magnitudes in `Vec<u32>`: schoolbook
//! multiplication, Knuth's long division, two's complement for the
//! bitwise operators.

use std::cmp::Ordering;

use crate::object::{Class, as_object, object_value};
use crate::value::Value;
use crate::vm::{Error, Result, Vm};

/// An integer: its sign and magnitude (normalized: no leading zero limb,
/// zero not negative).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Big {
    pub neg: bool,
    pub mag: Vec<u32>,
}

fn trim(v: &mut Vec<u32>) {
    while v.last() == Some(&0) {
        v.pop();
    }
}

fn mag_cmp(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

fn mag_add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(a.len() + 1);
    let mut carry = 0u64;
    for i in 0..a.len() {
        let s = a[i] as u64 + *b.get(i).unwrap_or(&0) as u64 + carry;
        out.push(s as u32);
        carry = s >> 32;
    }
    if carry != 0 {
        out.push(carry as u32);
    }
    out
}

/// `a - b`, `a` at least `b`.
fn mag_sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for i in 0..a.len() {
        let mut d = a[i] as i64 - *b.get(i).unwrap_or(&0) as i64 - borrow;
        borrow = 0;
        if d < 0 {
            d += 1 << 32;
            borrow = 1;
        }
        out.push(d as u32);
    }
    trim(&mut out);
    out
}

fn mag_mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &y) in b.iter().enumerate() {
            let t = x as u64 * y as u64 + out[i + j] as u64 + carry;
            out[i + j] = t as u32;
            carry = t >> 32;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let t = out[k] as u64 + carry;
            out[k] = t as u32;
            carry = t >> 32;
            k += 1;
        }
    }
    trim(&mut out);
    out
}

/// `a / d` and `a % d` for a one-limb divisor.
fn mag_divrem_small(a: &[u32], d: u32) -> (Vec<u32>, u32) {
    let mut q = vec![0u32; a.len()];
    let mut r = 0u64;
    for i in (0..a.len()).rev() {
        let cur = (r << 32) | a[i] as u64;
        q[i] = (cur / d as u64) as u32;
        r = cur % d as u64;
    }
    trim(&mut q);
    (q, r as u32)
}

/// `a / b` and `a % b` (Knuth's algorithm D), `b` not zero.
fn mag_divrem(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if mag_cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = mag_divrem_small(a, b[0]);
        let mut r = vec![r];
        trim(&mut r);
        return (q, r);
    }
    // Normalize: the divisor's top limb's top bit set.
    let s = b[b.len() - 1].leading_zeros();
    let shl = |v: &[u32], extra: bool| -> Vec<u32> {
        let mut out = Vec::with_capacity(v.len() + 1);
        let mut carry = 0u32;
        for &x in v {
            out.push(if s == 0 { x } else { (x << s) | carry });
            carry = if s == 0 { 0 } else { x >> (32 - s) };
        }
        if extra {
            out.push(carry);
        }
        out
    };
    let v = shl(b, false);
    let mut u = shl(a, true);
    let n = v.len();
    let m = u.len() - n - 1;
    let mut q = vec![0u32; m + 1];
    let b32 = 1u64 << 32;
    for j in (0..=m).rev() {
        let num = ((u[j + n] as u64) << 32) | u[j + n - 1] as u64;
        let mut qhat = num / v[n - 1] as u64;
        let mut rhat = num % v[n - 1] as u64;
        while qhat >= b32 || qhat * v[n - 2] as u64 > ((rhat << 32) | u[j + n - 2] as u64) {
            qhat -= 1;
            rhat += v[n - 1] as u64;
            if rhat >= b32 {
                break;
            }
        }
        // u[j..j+n+1] -= qhat * v
        let mut borrow = 0i64;
        let mut carry = 0u64;
        for i in 0..n {
            let p = qhat * v[i] as u64 + carry;
            carry = p >> 32;
            let t = u[i + j] as i64 - borrow - (p as u32) as i64;
            u[i + j] = t as u32;
            borrow = if t < 0 { 1 } else { 0 };
        }
        let t = u[j + n] as i64 - borrow - carry as i64;
        u[j + n] = t as u32;
        if t < 0 {
            // Added back: qhat was one too many.
            qhat -= 1;
            let mut c = 0u64;
            for i in 0..n {
                let s2 = u[i + j] as u64 + v[i] as u64 + c;
                u[i + j] = s2 as u32;
                c = s2 >> 32;
            }
            u[j + n] = u[j + n].wrapping_add(c as u32);
        }
        q[j] = qhat as u32;
    }
    trim(&mut q);
    // The remainder, unnormalized.
    let mut r = vec![0u32; n];
    for i in 0..n {
        r[i] = if s == 0 { u[i] } else { (u[i] >> s) | (u[i + 1] << (32 - s)) };
    }
    trim(&mut r);
    (q, r)
}

impl Big {
    pub(crate) fn zero() -> Big {
        Big { neg: false, mag: Vec::new() }
    }

    fn new(neg: bool, mut mag: Vec<u32>) -> Big {
        trim(&mut mag);
        let neg = neg && !mag.is_empty();
        Big { neg, mag }
    }

    pub(crate) fn from_u64(v: u64) -> Big {
        Big::new(false, vec![v as u32, (v >> 32) as u32])
    }

    pub(crate) fn from_i64(v: i64) -> Big {
        let mut b = Big::from_u64(v.unsigned_abs());
        b.neg = v < 0;
        b
    }

    /// An integral number's value (the caller checks it is one).
    pub(crate) fn from_f64(d: f64) -> Big {
        if d.abs() < 9223372036854775808.0 {
            return Big::from_i64(d as i64);
        }
        let bits = d.to_bits();
        let exp = ((bits >> 52) & 0x7FF) as i64 - 1075;
        let mant = (bits & ((1 << 52) - 1)) | (1 << 52);
        let mut b = Big::from_u64(mant).shl(exp);
        b.neg = d < 0.0;
        b
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    /// The nearest number (ties to even).
    pub(crate) fn to_f64(&self) -> f64 {
        let m = match self.mag.len() {
            0 => 0.0,
            1 => self.mag[0] as f64,
            2 => (self.mag[0] as u64 | (self.mag[1] as u64) << 32) as f64,
            _ => {
                // (Rust's parsing rounds correctly.)
                let s = self.to_string_radix(10);
                return s.parse::<f64>().unwrap_or(f64::NAN);
            }
        };
        if self.neg { -m } else { m }
    }

    pub(crate) fn cmp(&self, o: &Big) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => mag_cmp(&self.mag, &o.mag),
            (true, true) => mag_cmp(&o.mag, &self.mag),
        }
    }

    pub(crate) fn neg(&self) -> Big {
        Big::new(!self.neg, self.mag.clone())
    }

    pub(crate) fn add(&self, o: &Big) -> Big {
        if self.neg == o.neg {
            return Big::new(self.neg, mag_add(&self.mag, &o.mag));
        }
        match mag_cmp(&self.mag, &o.mag) {
            Ordering::Less => Big::new(o.neg, mag_sub(&o.mag, &self.mag)),
            _ => Big::new(self.neg, mag_sub(&self.mag, &o.mag)),
        }
    }

    pub(crate) fn sub(&self, o: &Big) -> Big {
        self.add(&o.neg())
    }

    pub(crate) fn mul(&self, o: &Big) -> Big {
        Big::new(self.neg != o.neg, mag_mul(&self.mag, &o.mag))
    }

    /// Truncating division and remainder (the remainder has the
    /// dividend's sign); `None` dividing by zero.
    pub(crate) fn divrem(&self, o: &Big) -> Option<(Big, Big)> {
        if o.is_zero() {
            return None;
        }
        let (q, r) = mag_divrem(&self.mag, &o.mag);
        Some((Big::new(self.neg != o.neg, q), Big::new(self.neg, r)))
    }

    /// `self ** e`, `e` not negative (the caller checks), up to a size.
    pub(crate) fn pow(&self, mut e: u64) -> Option<Big> {
        let mut base = self.clone();
        let mut out = Big::from_u64(1);
        // (Past 2^30 bits, too large for this engine.)
        if !self.mag.is_empty() && self.mag != [1] && e > (1 << 30) / (32 * self.mag.len() as u64).max(1) {
            return None;
        }
        while e > 0 {
            if e & 1 == 1 {
                out = out.mul(&base);
            }
            e >>= 1;
            if e > 0 {
                base = base.mul(&base);
            }
        }
        Some(out)
    }

    /// `self << n` (`n` may be negative: an arithmetic shift right,
    /// rounding toward negative infinity).
    pub(crate) fn shl(&self, n: i64) -> Big {
        if self.is_zero() || n == 0 {
            return self.clone();
        }
        if n > 0 {
            let (limbs, bits) = ((n / 32) as usize, (n % 32) as u32);
            let mut out = vec![0u32; limbs];
            let mut carry = 0u32;
            for &x in &self.mag {
                out.push(if bits == 0 { x } else { (x << bits) | carry });
                carry = if bits == 0 { 0 } else { x >> (32 - bits) };
            }
            if carry != 0 {
                out.push(carry);
            }
            return Big::new(self.neg, out);
        }
        let n = n.unsigned_abs();
        if self.neg {
            // floor(-m / 2^n) = -((m - 1) >> n) - 1
            let m1 = Big::new(false, mag_sub(&self.mag, &[1]));
            let s = m1.shl(-(n as i64));
            return Big::new(true, mag_add(&s.mag, &[1]));
        }
        let (limbs, bits) = ((n / 32) as usize, (n % 32) as u32);
        if limbs >= self.mag.len() {
            return Big::zero();
        }
        let src = &self.mag[limbs..];
        let mut out = Vec::with_capacity(src.len());
        for i in 0..src.len() {
            let hi = *src.get(i + 1).unwrap_or(&0);
            out.push(if bits == 0 { src[i] } else { (src[i] >> bits) | (hi << (32 - bits)) });
        }
        Big::new(false, out)
    }

    /// Two's complement in `n` limbs (sign-extended).
    fn twos(&self, n: usize) -> Vec<u32> {
        let mut v = self.mag.clone();
        v.resize(n, 0);
        if self.neg {
            for x in v.iter_mut() {
                *x = !*x;
            }
            for x in v.iter_mut() {
                let (s, o) = x.overflowing_add(1);
                *x = s;
                if !o {
                    break;
                }
            }
        }
        v
    }

    fn from_twos(mut v: Vec<u32>) -> Big {
        let neg = v.last().is_some_and(|&x| x >> 31 == 1);
        if neg {
            for x in v.iter_mut() {
                *x = !*x;
            }
            for x in v.iter_mut() {
                let (s, o) = x.overflowing_add(1);
                *x = s;
                if !o {
                    break;
                }
            }
        }
        Big::new(neg, v)
    }

    pub(crate) fn bitop(&self, o: &Big, op: fn(u32, u32) -> u32) -> Big {
        let n = self.mag.len().max(o.mag.len()) + 1;
        let (a, b) = (self.twos(n), o.twos(n));
        Big::from_twos(a.iter().zip(&b).map(|(&x, &y)| op(x, y)).collect())
    }

    /// `~self`: `-self - 1`.
    pub(crate) fn not(&self) -> Big {
        self.neg().sub(&Big::from_u64(1))
    }

    /// BigInt.asUintN(bits): `self` modulo 2^bits.
    pub(crate) fn as_uint_n(&self, bits: u64) -> Big {
        if bits == 0 {
            return Big::zero();
        }
        let limbs = bits.div_ceil(32) as usize;
        let mut v = self.twos(limbs.max(self.mag.len() + 1));
        v.truncate(limbs);
        let rem = bits % 32;
        if rem != 0 {
            v[limbs - 1] &= (1u32 << rem) - 1;
        }
        Big::new(false, v)
    }

    /// BigInt.asIntN(bits): `self` modulo 2^bits, as a signed value.
    pub(crate) fn as_int_n(&self, bits: u64) -> Big {
        if bits == 0 {
            return Big::zero();
        }
        let u = self.as_uint_n(bits);
        // At least 2^(bits-1): minus 2^bits.
        let top = Big::from_u64(1).shl(bits as i64 - 1);
        if u.cmp(&top) != Ordering::Less { u.sub(&Big::from_u64(1).shl(bits as i64)) } else { u }
    }

    pub(crate) fn to_string_radix(&self, radix: u32) -> String {
        if self.is_zero() {
            return "0".into();
        }
        // The largest power of the radix in a limb, and its digits.
        let (mut chunk, mut digits) = (radix, 1);
        while (chunk as u64) * (radix as u64) <= u32::MAX as u64 {
            chunk *= radix;
            digits += 1;
        }
        let mut parts = Vec::new();
        let mut m = self.mag.clone();
        while !m.is_empty() {
            let (q, r) = mag_divrem_small(&m, chunk);
            parts.push(r);
            m = q;
        }
        let mut out = String::new();
        if self.neg {
            out.push('-');
        }
        let to = |mut v: u32, pad: bool, out: &mut String| {
            let mut buf = Vec::new();
            for _ in 0..digits {
                buf.push(std::char::from_digit(v % radix, radix).unwrap());
                v /= radix;
                if v == 0 && !pad {
                    break;
                }
            }
            out.extend(buf.iter().rev());
        };
        let last = parts.len() - 1;
        to(parts[last], false, &mut out);
        for i in (0..last).rev() {
            to(parts[i], true, &mut out);
        }
        out
    }

    /// Digits `s` in `radix` (no sign, no prefix), if all are digits.
    pub(crate) fn parse_digits(s: &str, radix: u32) -> Option<Big> {
        if s.is_empty() {
            return None;
        }
        let mut mag: Vec<u32> = Vec::new();
        for c in s.chars() {
            let d = c.to_digit(radix)?;
            // mag = mag * radix + d
            let mut carry = d as u64;
            for x in mag.iter_mut() {
                let t = *x as u64 * radix as u64 + carry;
                *x = t as u32;
                carry = t >> 32;
            }
            if carry != 0 {
                mag.push(carry as u32);
            }
        }
        Some(Big::new(false, mag))
    }

    /// StringToBigInt: whitespace around an optional sign and decimal
    /// digits, or `0x`/`0o`/`0b` digits; empty is 0n; else `None`.
    pub(crate) fn parse(s: &str) -> Option<Big> {
        let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        if s.is_empty() {
            return Some(Big::zero());
        }
        let lower = s.get(..2).map(str::to_ascii_lowercase);
        let radix = match lower.as_deref() {
            Some("0x") => 16,
            Some("0o") => 8,
            Some("0b") => 2,
            _ => 10,
        };
        if radix != 10 {
            return Big::parse_digits(&s[2..], radix);
        }
        let (neg, digits) = match s.as_bytes()[0] {
            b'-' => (true, &s[1..]),
            b'+' => (false, &s[1..]),
            _ => (false, s),
        };
        let mut b = Big::parse_digits(digits, 10)?;
        b.neg = neg && !b.is_zero();
        Some(b)
    }
}

// ---- The cells ----

/// Whether `v` is a BigInt.
#[inline]
pub(crate) fn is_bigint(v: Value) -> bool {
    v.is_cell() && unsafe { (*as_object(v)).class } == Class::BigInt
}

/// BigInt `v`'s value.
pub(crate) fn big_of(v: Value) -> Big {
    unsafe {
        let o = as_object(v);
        let n = (*o).elems_len as usize;
        let limbs = if n == 0 { &[][..] } else { std::slice::from_raw_parts((*o).elems.cast::<u32>(), n) };
        Big { neg: (*o).elems_cap == 1, mag: limbs.to_vec() }
    }
}

/// A new BigInt of `b`.
pub(crate) fn new_bigint(vm: &mut Vm, b: Big) -> Value {
    let proto = if vm.bigint_prototype.is_null() { Value::NULL } else { object_value(vm.bigint_prototype) };
    let o = vm.new_object_with(Class::BigInt, proto, 0);
    unsafe {
        (*o).elems_cap = b.neg as u32;
        if !b.mag.is_empty() {
            let p = vm.heap.alloc_bytes(4 * b.mag.len().next_multiple_of(2)).cast::<u32>();
            std::ptr::copy_nonoverlapping(b.mag.as_ptr(), p, b.mag.len());
            (*o).elems = p.cast();
        }
        (*o).elems_len = b.mag.len() as u32;
    }
    vm.prevent_extensions(o);
    object_value(o)
}

/// A new BigInt of 64 bits `bits` (two's complement if `signed`), for
/// `BigInt64Array`s and `DataView`s.
pub(crate) fn from_bits(bits: u64, signed: bool) -> Value {
    let b = if signed { Big::from_i64(bits as i64) } else { Big::from_u64(bits) };
    new_bigint(vm(), b)
}

/// BigInt `v`'s low 64 bits (two's complement), or 0 if not a BigInt.
pub(crate) fn to_bits(v: Value) -> u64 {
    if !is_bigint(v) {
        return 0;
    }
    let u = big_of(v).as_uint_n(64);
    u.mag.first().copied().unwrap_or(0) as u64 | (*u.mag.get(1).unwrap_or(&0) as u64) << 32
}

/// Whether BigInts `a` and `b` are the same value.
pub(crate) fn equal(a: Value, b: Value) -> bool {
    big_of(a) == big_of(b)
}

/// A hash of BigInt `v`'s value (for a Map's keys).
pub(crate) fn hash(v: Value) -> u32 {
    let b = big_of(v);
    let mut h: u32 = if b.neg { 0x9e37_79b9 } else { 0 };
    for x in b.mag {
        h = h.rotate_left(5) ^ x;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The string of BigInt `v` (decimal).
pub(crate) fn to_string(v: Value) -> String {
    big_of(v).to_string_radix(10)
}

/// An operator's error, reported as a conversion's (see
/// `interp::to_primitive_hint`).
fn throw(e: Error) {
    let vm = vm();
    vm.conv_error.get_or_insert(e);
    vm.conv_thrown = true;
}

/// Whether the operators can't run here: in compiled code's runtime calls
/// (`native_top` null), which don't know the instruction (`AddSmi`'s and
/// `Add`'s share one) and where allocating isn't safe. There it sets
/// `Vm::conv_redo`, for the interpreter to redo the instruction (whose
/// slow paths redo it once more with `native_top` set).
pub(crate) fn redo() -> bool {
    let vm = vm();
    if vm.native_top.is_null() {
        vm.conv_redo = true;
        return true;
    }
    false
}

fn mix_error() {
    throw(Error::type_error("Cannot mix BigInt and other types, use explicit conversions"));
}

/// The VM, to allocate the result.
fn vm() -> &'static mut Vm {
    unsafe { &mut *crate::interp::current_vm() }
}

/// What a binary operator does where either operand is a BigInt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    And,
    Or,
    Xor,
    Shl,
    Sar,
    Shr,
}

/// `a op b` for primitives, one a BigInt: a BigInt, or (an error
/// reported as a conversion's, see `throw`; or to be redone, see
/// [`redo`]) `undefined`. Another type mixed in is an error.
pub(crate) fn binary(op: BinOp, a: Value, b: Value) -> Value {
    if redo() {
        return Value::UNDEFINED;
    }
    if !is_bigint(a) || !is_bigint(b) {
        mix_error();
        return Value::UNDEFINED;
    }
    let (x, y) = (big_of(a), big_of(b));
    let r = match op {
        BinOp::Add => x.add(&y),
        BinOp::Sub => x.sub(&y),
        BinOp::Mul => x.mul(&y),
        BinOp::Div | BinOp::Mod => match x.divrem(&y) {
            Some((q, r)) => {
                if op == BinOp::Div {
                    q
                } else {
                    r
                }
            }
            None => {
                throw(Error::range_error("Division by zero"));
                return Value::UNDEFINED;
            }
        },
        BinOp::Exp => {
            if y.neg {
                throw(Error::range_error("Exponent must be non-negative"));
                return Value::UNDEFINED;
            }
            let e = if y.mag.len() > 2 { u64::MAX } else { y.mag.first().copied().unwrap_or(0) as u64 | (*y.mag.get(1).unwrap_or(&0) as u64) << 32 };
            match x.pow(e) {
                Some(r) => r,
                None => {
                    throw(Error::range_error("Maximum BigInt size exceeded"));
                    return Value::UNDEFINED;
                }
            }
        }
        BinOp::And => x.bitop(&y, |p, q| p & q),
        BinOp::Or => x.bitop(&y, |p, q| p | q),
        BinOp::Xor => x.bitop(&y, |p, q| p ^ q),
        BinOp::Shl | BinOp::Sar => {
            let n = if y.mag.len() > 1 { i64::MAX / 2 } else { y.mag.first().copied().unwrap_or(0) as i64 };
            let n = if y.neg { -n } else { n };
            let n = if op == BinOp::Sar { -n } else { n };
            if n > (1 << 30) {
                throw(Error::range_error("Maximum BigInt size exceeded"));
                return Value::UNDEFINED;
            }
            x.shl(n)
        }
        BinOp::Shr => {
            throw(Error::type_error("BigInts have no unsigned right shift, use >> instead"));
            return Value::UNDEFINED;
        }
    };
    new_bigint(vm(), r)
}

/// `-v` for a BigInt (`undefined` to be redone, see [`redo`]).
pub(crate) fn negate(v: Value) -> Value {
    if redo() {
        return Value::UNDEFINED;
    }
    new_bigint(vm(), big_of(v).neg())
}

/// `~v` for a BigInt.
pub(crate) fn bit_not(v: Value) -> Value {
    if redo() {
        return Value::UNDEFINED;
    }
    new_bigint(vm(), big_of(v).not())
}

/// `v + d` for a BigInt (`++`, `--`).
pub(crate) fn inc(v: Value, d: i64) -> Value {
    if redo() {
        return Value::UNDEFINED;
    }
    new_bigint(vm(), big_of(v).add(&Big::from_i64(d)))
}

/// A BigInt compared with a number, mathematically (`None`: NaN).
pub(crate) fn cmp_number(b: &Big, d: f64) -> Option<Ordering> {
    if d.is_nan() {
        return None;
    }
    if d.is_infinite() {
        return Some(if d > 0.0 { Ordering::Less } else { Ordering::Greater });
    }
    let t = d.trunc();
    match b.cmp(&Big::from_f64(t)) {
        Ordering::Equal if d != t => Some(if d > t { Ordering::Less } else { Ordering::Greater }),
        o => Some(o),
    }
}

/// `a < b` (or `<=`) where either primitive is a BigInt: the other a
/// BigInt, a number or a string (which, not a BigInt's digits, compares
/// false).
pub(crate) fn less(a: Value, b: Value, le: bool) -> bool {
    let as_num = |v: Value| -> Option<std::result::Result<Big, f64>> {
        if is_bigint(v) {
            return Some(Ok(big_of(v)));
        }
        if let Some(u) = crate::vm::string_units(v) {
            return Big::parse(&String::from_utf16_lossy(u)).map(Ok);
        }
        Some(Err(crate::interp::to_number(v)))
    };
    let (Some(x), Some(y)) = (as_num(a), as_num(b)) else { return false };
    let o = match (x, y) {
        (Ok(x), Ok(y)) => Some(x.cmp(&y)),
        (Ok(x), Err(d)) => cmp_number(&x, d),
        (Err(d), Ok(y)) => cmp_number(&y, d).map(Ordering::reverse),
        (Err(p), Err(q)) => p.partial_cmp(&q),
    };
    match o {
        Some(Ordering::Less) => true,
        Some(Ordering::Equal) => le,
        _ => false,
    }
}

/// `a == b` where `a` is a BigInt and `b` a primitive.
pub(crate) fn loose_equal(a: Value, b: Value) -> bool {
    let x = big_of(a);
    if is_bigint(b) {
        return x == big_of(b);
    }
    if let Some(u) = crate::vm::string_units(b) {
        return Big::parse(&String::from_utf16_lossy(u)).is_some_and(|y| y == x);
    }
    if let Some(d) = b.as_number() {
        return cmp_number(&x, d) == Some(Ordering::Equal);
    }
    if b.is_bool() {
        return x == Big::from_u64((b == Value::TRUE) as u64);
    }
    false
}

// ---- The builtins ----

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let any = crate::jit::Ty::Any;
    let op = object_value(vm.object_prototype);
    let proto = vm.new_object(op);
    vm.bigint_prototype = proto;
    let c = vm.define_native(g, "BigInt", bigint_fn, 1, any, true);
    crate::builtins::link_constructor(vm, c, proto);
    vm.define_natives(
        c,
        &[("asIntN", bigint_as_int_n as crate::vm::NativeFn, 2), ("asUintN", bigint_as_uint_n, 2)],
    );
    vm.define_natives(
        proto,
        &[
            ("toString", bigint_to_string as crate::vm::NativeFn, 0),
            ("toLocaleString", bigint_to_locale_string, 0),
            ("valueOf", bigint_value_of, 0),
        ],
    );
    crate::symbol::to_string_tag(vm, proto, "BigInt");
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// ToBigInt(v): a BigInt, a boolean, or a string of a BigInt's digits
/// (the primitive of an object).
pub(crate) fn to_bigint(vm: &mut Vm, v: Value) -> Result<Big> {
    let p = crate::interp::to_primitive(v);
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    if is_bigint(p) {
        return Ok(big_of(p));
    }
    if p.is_bool() {
        return Ok(Big::from_u64((p == Value::TRUE) as u64));
    }
    if let Some(u) = crate::vm::string_units(p) {
        return Big::parse(&String::from_utf16_lossy(u))
            .ok_or_else(|| Error::new_syntax(format!("Cannot convert {} to a BigInt", String::from_utf16_lossy(u))));
    }
    Err(Error::type_error(format!("Cannot convert {} to a BigInt", crate::vm::display(p))))
}

/// `BigInt(v)`: a number (an integer) or what ToBigInt takes; not a
/// constructor.
fn bigint_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if vm.is_construct() {
        return Err(Error::type_error("BigInt is not a constructor"));
    }
    let p = crate::interp::to_primitive(arg(a, 0));
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    if let Some(d) = p.as_number() {
        if d.fract() != 0.0 || !d.is_finite() {
            return Err(Error::range_error(format!(
                "The number {} cannot be converted to a BigInt because it is not an integer",
                crate::vm::display(p)
            )));
        }
        return Ok(new_bigint(vm, Big::from_f64(d)));
    }
    let b = to_bigint(vm, p)?;
    Ok(new_bigint(vm, b))
}

/// The bit count argument of `asIntN`/`asUintN` (ToIndex).
fn bits_arg(vm: &mut Vm, v: Value) -> Result<u64> {
    let d = crate::interp::to_number(v);
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    let d = if d.is_nan() { 0.0 } else { d.trunc() };
    if !(0.0..=9007199254740991.0).contains(&d) {
        return Err(Error::range_error("Invalid value: not (convertible to) a safe integer"));
    }
    Ok(d as u64)
}

fn bigint_as_int_n(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let bits = bits_arg(vm, arg(a, 0))?;
    let b = to_bigint(vm, arg(a, 1))?;
    if bits > 1 << 30 {
        return Ok(new_bigint(vm, b));
    }
    Ok(new_bigint(vm, b.as_int_n(bits)))
}

fn bigint_as_uint_n(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let bits = bits_arg(vm, arg(a, 0))?;
    let b = to_bigint(vm, arg(a, 1))?;
    if bits > 1 << 30 {
        if b.neg {
            return Err(Error::range_error("Maximum BigInt size exceeded"));
        }
        return Ok(new_bigint(vm, b));
    }
    Ok(new_bigint(vm, b.as_uint_n(bits)))
}

/// `this` as a BigInt (a BigInt, or its wrapper).
fn this_bigint(this: Value, what: &str) -> Result<Big> {
    let v = crate::vm::wrapped(this).unwrap_or(this);
    if is_bigint(v) {
        return Ok(big_of(v));
    }
    Err(Error::type_error(format!("BigInt.prototype.{what} requires that 'this' be a BigInt")))
}

fn bigint_to_string(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let b = this_bigint(this, "toString")?;
    let r = arg(a, 0);
    let radix = if r.is_undefined() {
        10.0
    } else {
        let d = crate::interp::to_number(r);
        if vm.conv_failed() {
            return Err(vm.take_conv_error());
        }
        if d.is_nan() { 0.0 } else { d.trunc() }
    };
    if !(2.0..=36.0).contains(&radix) {
        return Err(Error::range_error("toString() radix must be between 2 and 36"));
    }
    let s = b.to_string_radix(radix as u32);
    Ok(crate::builtins::str_value(vm, &s))
}

fn bigint_to_locale_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let b = this_bigint(this, "toLocaleString")?;
    let s = b.to_string_radix(10);
    Ok(crate::builtins::str_value(vm, &s))
}

fn bigint_value_of(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    this_bigint(this, "valueOf")?;
    Ok(crate::vm::wrapped(this).unwrap_or(this))
}

/// A BigInt literal's property key (`{1n: …}`): its decimal digits.
pub(crate) fn literal_key(digits: &str) -> String {
    Big::parse(digits).map_or_else(String::new, |b| b.to_string_radix(10))
}

/// A BigInt literal's value: `[digits]` (with a `0x`/`0o`/`0b` prefix).
pub(crate) fn literal(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = String::from_utf16_lossy(crate::vm::string_units(a[0]).unwrap_or(&[]));
    let b = Big::parse(&s).ok_or_else(|| Error::new_syntax(format!("Invalid BigInt literal {s}")))?;
    Ok(new_bigint(vm, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Big {
        Big::parse(s).unwrap()
    }

    #[test]
    fn arithmetic() {
        let x = b("123456789012345678901234567890");
        let y = b("-987654321098765432109876543210");
        assert_eq!(x.add(&y).to_string_radix(10), "-864197532086419753208641975320");
        assert_eq!(x.mul(&y).to_string_radix(10), "-121932631137021795226185032733622923332237463801111263526900");
        let (q, r) = y.divrem(&x).unwrap();
        assert_eq!((q.to_string_radix(10).as_str(), r.to_string_radix(10).as_str()), ("-8", "-9000000000900000000090"));
        assert_eq!(b("2").pow(100).unwrap().to_string_radix(16), "10000000000000000000000000");
        assert_eq!(b("-5").shl(-1).to_string_radix(10), "-3");
        assert_eq!(b("-1").as_uint_n(64).to_string_radix(10), "18446744073709551615");
        assert_eq!(b("255").as_int_n(8).to_string_radix(10), "-1");
        assert_eq!(b("-6").bitop(&b("3"), |p, q| p & q).to_string_radix(10), "2");
        assert_eq!(b("0xff").to_string_radix(2), "11111111");
        assert_eq!(Big::from_f64(1e21).to_string_radix(10), "1000000000000000000000");
        assert_eq!(b("9007199254740993").to_f64(), 9007199254740992.0);
    }
}
