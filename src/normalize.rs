//! Unicode normalization (UAX #15), for `String.prototype.normalize`: a
//! string's code points fully decomposed (canonically, or by compatibility
//! too), combining marks put in canonical order, then for NFC and NFKC
//! composed again. The tables are generated from the Unicode Character
//! Database (`unicode_norm`); Hangul syllables are decomposed and composed
//! arithmetically. Lone surrogates pass through, as starters of no
//! decomposition.

use crate::unicode_norm::{CCC, COMPOSE, DATA, DECOMP};

/// A normalization form.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

impl Form {
    fn compat(self) -> bool {
        matches!(self, Form::Nfkc | Form::Nfkd)
    }

    fn composes(self) -> bool {
        matches!(self, Form::Nfc | Form::Nfkc)
    }
}

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

/// `c`'s canonical combining class.
fn ccc(c: u32) -> u8 {
    // (Nothing below U+0300 combines.)
    if c < 0x300 {
        return 0;
    }
    match CCC.binary_search_by(|&(a, b, _)| if b < c { std::cmp::Ordering::Less } else if a > c { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal }) {
        Ok(i) => CCC[i].2,
        Err(_) => 0,
    }
}

/// Append `c` fully decomposed to `out`.
fn decompose(c: u32, compat: bool, out: &mut Vec<u32>) {
    if (S_BASE..S_BASE + S_COUNT).contains(&c) {
        let s = c - S_BASE;
        out.push(L_BASE + s / N_COUNT);
        out.push(V_BASE + s % N_COUNT / T_COUNT);
        if s % T_COUNT != 0 {
            out.push(T_BASE + s % T_COUNT);
        }
        return;
    }
    if c >= 0xA0
        && let Ok(i) = DECOMP.binary_search_by_key(&c, |e| e.0)
        && (compat || !DECOMP[i].3)
    {
        let (_, at, n, _) = DECOMP[i];
        for &d in &DATA[at as usize..at as usize + n as usize] {
            decompose(d, compat, out);
        }
        return;
    }
    out.push(c);
}

/// The canonical composite of `a` and `b`, if any.
fn compose(a: u32, b: u32) -> Option<u32> {
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        return Some(S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&a) && (a - S_BASE) % T_COUNT == 0 && (T_BASE + 1..T_BASE + T_COUNT).contains(&b) {
        return Some(a + (b - T_BASE));
    }
    COMPOSE.binary_search_by_key(&(a, b), |e| (e.0, e.1)).ok().map(|i| COMPOSE[i].2)
}

/// Whether `units` is in form `form` for sure, without looking further:
/// ASCII is in every form, and nothing below U+0300 changes in NFC.
fn quick(units: &[u16], form: Form) -> bool {
    let limit = if form == Form::Nfc { 0x300 } else { 0x80 };
    units.iter().all(|&u| u < limit)
}

/// `units` normalized to `form`, or `None` if it is already.
pub(crate) fn normalize(units: &[u16], form: Form) -> Option<Vec<u16>> {
    if quick(units, form) {
        return None;
    }
    // Decompose, a code point (or lone surrogate) at a time.
    let mut cps = Vec::with_capacity(units.len());
    for c in char::decode_utf16(units.iter().copied()) {
        let c = c.map_or_else(|e| e.unpaired_surrogate() as u32, |c| c as u32);
        decompose(c, form.compat(), &mut cps);
    }
    // Canonical order: each run of marks sorted by class, stably.
    let mut i = 0;
    while i < cps.len() {
        if ccc(cps[i]) == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < cps.len() && ccc(cps[i]) != 0 {
            i += 1;
        }
        cps[start..i].sort_by_key(|&c| ccc(c));
    }
    if form.composes() {
        // Each mark (or starter) joins the last starter if nothing between
        // blocks it: a mark of the same or a higher class, or (for a
        // starter) anything.
        let mut out: Vec<u32> = Vec::with_capacity(cps.len());
        let mut starter: Option<usize> = None;
        let mut last_ccc: i32 = -1;
        for &c in &cps {
            let cc = ccc(c) as i32;
            if let Some(s) = starter {
                let blocked = last_ccc != -1 && (last_ccc >= cc || last_ccc == 0);
                if !blocked && let Some(k) = compose(out[s], c) {
                    out[s] = k;
                    continue;
                }
            }
            if cc == 0 {
                starter = Some(out.len());
                last_ccc = -1;
            } else {
                last_ccc = cc;
            }
            out.push(c);
        }
        cps = out;
    }
    let mut out = Vec::with_capacity(cps.len());
    for c in cps {
        if c >= 0x10000 {
            let c = c - 0x10000;
            out.push(0xD800 + (c >> 10) as u16);
            out.push(0xDC00 + (c & 0x3FF) as u16);
        } else {
            out.push(c as u16);
        }
    }
    (out != units).then_some(out)
}
