//! Rust's accesses to typed arrays' and buffers' bytes, which a
//! `SharedArrayBuffer`'s other agents (`agent`) may read and write at the
//! same time: each one a relaxed atomic access, so that no access races
//! in Rust's memory model (a plain access racing any other access, atomic
//! or not, is undefined behavior), as ECMAScript's memory model has them
//! (unordered, each element read or written whole when aligned). On the
//! processors this runs on, a relaxed atomic load or store is a plain
//! one. Compiled code is not Rust: its accesses are the machine's.
//!
//! An access to an element whose address is not aligned to its size (a
//! `DataView`'s) is byte by byte. `Atomics` uses its own sequentially
//! consistent operations (`shared::apply`).

use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64};

/// The `N` bytes at `p` (1, 2, 4 or 8), in native order, as an integer.
///
/// # Safety
/// `p` must point to `N` readable bytes.
#[inline(always)]
pub(crate) unsafe fn load<const N: usize>(p: *const u8) -> u64 {
    unsafe {
        let p = p.cast_mut();
        if (p as usize).is_multiple_of(N) {
            return match N {
                1 => AtomicU8::from_ptr(p).load(Relaxed) as u64,
                2 => AtomicU16::from_ptr(p.cast()).load(Relaxed) as u64,
                4 => AtomicU32::from_ptr(p.cast()).load(Relaxed) as u64,
                _ => AtomicU64::from_ptr(p.cast()).load(Relaxed),
            };
        }
        let mut b = [0u8; 8];
        for (k, b) in b.iter_mut().enumerate().take(N) {
            *b = AtomicU8::from_ptr(p.add(k)).load(Relaxed);
        }
        let at = if cfg!(target_endian = "big") { 8 - N } else { 0 };
        let mut v = [0u8; 8];
        v[at..at + N].copy_from_slice(&b[..N]);
        u64::from_ne_bytes(v)
    }
}

/// The low `N` bytes of `v` stored at `p`, in native order.
///
/// # Safety
/// `p` must point to `N` writable bytes.
#[inline(always)]
pub(crate) unsafe fn store<const N: usize>(p: *mut u8, v: u64) {
    unsafe {
        if (p as usize).is_multiple_of(N) {
            match N {
                1 => AtomicU8::from_ptr(p).store(v as u8, Relaxed),
                2 => AtomicU16::from_ptr(p.cast()).store(v as u16, Relaxed),
                4 => AtomicU32::from_ptr(p.cast()).store(v as u32, Relaxed),
                _ => AtomicU64::from_ptr(p.cast()).store(v, Relaxed),
            }
            return;
        }
        let at = if cfg!(target_endian = "big") { 8 - N } else { 0 };
        let b = v.to_ne_bytes();
        for k in 0..N {
            AtomicU8::from_ptr(p.add(k)).store(b[at + k], Relaxed);
        }
    }
}

/// `memmove` of `n` bytes from `src` to `dst`, either of which may be
/// shared: eight bytes at a time where both are as aligned, else one.
///
/// # Safety
/// `src` must point to `n` readable bytes and `dst` to `n` writable ones.
pub(crate) unsafe fn copy(src: *const u8, dst: *mut u8, n: usize) {
    let byte = |k: usize| unsafe { store::<1>(dst.add(k), load::<1>(src.add(k))) };
    let word = |k: usize| unsafe { store::<8>(dst.add(k), load::<8>(src.add(k))) };
    let words = (src as usize).wrapping_sub(dst as usize).is_multiple_of(8);
    // The bytes before `dst` is aligned, and the aligned end of the words.
    let head = if words { ((dst as usize).wrapping_neg() % 8).min(n) } else { n };
    let end = head + (n - head) / 8 * 8;
    if (dst as usize) <= (src as usize) || (dst as usize) >= (src as usize).wrapping_add(n) {
        (0..head).for_each(byte);
        (head..end).step_by(8).for_each(word);
        (end..n).for_each(byte);
    } else {
        // Overlapping, `dst` after `src`: last to first.
        (end..n).rev().for_each(byte);
        (head..end).step_by(8).rev().for_each(word);
        (0..head).rev().for_each(byte);
    }
}

/// [`copy`] where either side may be shared, else `memmove`.
///
/// # Safety
/// As [`copy`].
pub(crate) unsafe fn copy_if(shared: bool, src: *const u8, dst: *mut u8, n: usize) {
    unsafe {
        if shared {
            copy(src, dst, n)
        } else {
            std::ptr::copy(src, dst, n)
        }
    }
}

#[cfg(test)]
mod tests {
    /// [`super::copy`] as `memmove`, at every alignment and overlap.
    #[test]
    fn copy_is_memmove() {
        for n in [0, 1, 7, 8, 9, 23, 64] {
            for src in 0..17 {
                for dst in 0..17 {
                    let init: Vec<u8> = (0..100).map(|k| k as u8).collect();
                    let (mut a, mut b) = (init.clone(), init.clone());
                    unsafe {
                        std::ptr::copy(a.as_ptr().add(src), a.as_mut_ptr().add(dst), n);
                        super::copy(b.as_ptr().add(src), b.as_mut_ptr().add(dst), n);
                    }
                    assert_eq!(a, b, "n {n} src {src} dst {dst}");
                }
            }
        }
    }

    #[test]
    fn unaligned_load_and_store() {
        let mut b = [0u8; 16];
        unsafe {
            super::store::<4>(b.as_mut_ptr().add(1), 0x0102_0304);
            assert_eq!(super::load::<4>(b.as_ptr().add(1)), 0x0102_0304);
            assert_eq!(super::load::<2>(b.as_ptr().add(3)) as u16, u16::from_ne_bytes([b[3], b[4]]));
            super::store::<8>(b.as_mut_ptr().add(5), u64::MAX - 1);
            assert_eq!(super::load::<8>(b.as_ptr().add(5)), u64::MAX - 1);
        }
    }
}
