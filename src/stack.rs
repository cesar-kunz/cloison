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
                #[cfg(target_env = "musl")]
                let addr = main_thread_end(addr as usize + size).map_or(addr as usize, |e| e.min(addr as usize));
                (addr as usize).max(1)
            } else {
                1
            }
        };
        END.with(|e| e.set(end));
        end
    }
}

/// On the main thread, the lowest address its stack, ending at `top`, may
/// grow to: musl's `pthread_getattr_np` gives the main thread's stack as
/// far as it is mapped so far (at the start, less than [`RESERVE`]: every
/// check failed), glibc's as far as `RLIMIT_STACK` lets it grow, which
/// the kernel keeps free below its top. (No limit: 8 MiB, Linux's usual.)
/// None on another thread, whose stack is as it was made.
#[cfg(target_env = "musl")]
fn main_thread_end(top: usize) -> Option<usize> {
    unsafe {
        if libc::getpid() as libc::c_long != libc::syscall(libc::SYS_gettid) {
            return None;
        }
        let mut l = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        let limit = match libc::getrlimit(libc::RLIMIT_STACK, &mut l) {
            0 if l.rlim_cur != libc::RLIM_INFINITY => l.rlim_cur as usize,
            _ => 8 << 20,
        };
        Some(top.saturating_sub(limit))
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
