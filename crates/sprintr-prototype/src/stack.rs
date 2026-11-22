// SPDX-License-Identifier: Apache-2.0

//! Producer-side capture of raw caller addresses.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

const NATIVE_CAPTURE: bool = cfg!(any(
    target_os = "macos",
    all(target_os = "linux", target_env = "gnu")
));
/// Default caller depth: ten on macOS and glibc Linux, zero on other targets.
pub const DEFAULT_STACK_DEPTH: usize = if NATIVE_CAPTURE { 10 } else { 0 };
/// Capacity of the stack-local capture buffer used by the macros.
pub const MAX_STACK_DEPTH: usize = 64;
static STACK_LIMIT: AtomicUsize = AtomicUsize::new(DEFAULT_STACK_DEPTH);

/// Set the process-wide limit on caller PCs. Zero skips native unwinding.
pub fn set_stack_limit(limit: usize) -> io::Result<()> {
    if limit > MAX_STACK_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("stack limit {limit} exceeds {MAX_STACK_DEPTH}"),
        ));
    }
    if limit != 0 && !NATIVE_CAPTURE {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native caller capture is available on macOS and glibc Linux",
        ));
    }
    STACK_LIMIT.store(limit, Ordering::Relaxed);
    Ok(())
}

/// Called directly by the logging macro, before entering the logger.
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
#[doc(hidden)]
#[inline(never)]
pub fn capture_stack(pcs: &mut [u64; MAX_STACK_DEPTH]) -> io::Result<&[u64]> {
    let limit = STACK_LIMIT.load(Ordering::Relaxed);
    if limit == 0 {
        return Ok(&[]);
    }
    // Both libc implementations start in their caller (this non-inlined
    // function). The next PC is the logging callsite, our static frame zero.
    const SKIP: usize = 2;
    let mut frames = [std::ptr::null_mut(); MAX_STACK_DEPTH + SKIP];
    let frames = &mut frames[..limit + SKIP];
    // SAFETY: libc writes at most the supplied number of pointer slots into
    // this invocation's buffer. We only read the addresses, never their targets.
    #[allow(unsafe_code)]
    let count = unsafe { libc::backtrace(frames.as_mut_ptr(), frames.len() as libc::c_int) };
    let callers = frames
        .get(SKIP..count as usize)
        .ok_or_else(|| io::Error::other("unwinder could not locate the logging callsite"))?;
    let mut len = 0;
    for &pc in callers {
        if pc.is_null() {
            break;
        }
        pcs[len] = pc.addr() as u64;
        len += 1;
    }
    Ok(&pcs[..len])
}

#[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
#[doc(hidden)]
pub fn capture_stack(_: &mut [u64; MAX_STACK_DEPTH]) -> io::Result<&[u64]> {
    Ok(&[])
}
