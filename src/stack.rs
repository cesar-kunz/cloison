//! The native stack left on this thread, for code that recurses over its
//! input (the front end over a program's nesting): past [`RESERVE`] from
//! the stack's end it fails instead of overflowing.

use std::cell::Cell;

/// Stack kept free below the deepest check: more than the frames between
/// two checks use.
pub(crate) const RESERVE: usize = 256 << 10;

thread_local! {
    /// The lowest address of this thread's stack, once found (0 before).
    static END: Cell<usize> = const { Cell::new(0) };
}

/// The lowest address of this thread's stack.
#[cold]
fn find_end() -> usize {
    unsafe {
        #[cfg(target_os = "macos")]
        let end = {
            let t = libc::pthread_self();
            libc::pthread_get_stackaddr_np(t) as usize - libc::pthread_get_stacksize_np(t)
        };
        #[cfg(not(target_os = "macos"))]
        let end = {
            let mut attr: libc::pthread_attr_t = std::mem::zeroed();
            let mut addr = std::ptr::null_mut();
            let mut size = 0;
            // (Unknown: 1, never low.)
            if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) == 0 {
                libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
                libc::pthread_attr_destroy(&mut attr);
                (addr as usize).max(1)
            } else {
                1
            }
        };
        END.with(|e| e.set(end));
        end
    }
}

/// The lowest address of this thread's stack (1 if unknown).
pub(crate) fn end() -> usize {
    match END.with(Cell::get) {
        0 => find_end(),
        end => end,
    }
}

/// Whether less than [`RESERVE`] of this thread's stack is left.
#[inline]
pub(crate) fn low() -> bool {
    let here = 0u8;
    let mut end = END.with(Cell::get);
    if end == 0 {
        end = find_end();
    }
    (&raw const here as usize) < end + RESERVE
}
