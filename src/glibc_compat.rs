//! Lets the binary link on glibc < 2.38 (Ubuntu 22.04, Debian 12).
//!
//! ort's prebuilt static ONNX Runtime was compiled against glibc 2.38+, where C23 headers
//! redirect `strtol`/`strtoll`/`strtoull` to `__isoc23_*` variants. The only difference is
//! that the C23 versions also accept a `0b` prefix when `base` is 0 or 2, which ONNX Runtime
//! never relies on, so forwarding to the classic functions is safe. On newer glibc these
//! definitions simply take precedence over libc's identical-in-practice ones.

use std::ffi::{c_char, c_int, c_long, c_longlong, c_ulonglong};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __isoc23_strtol(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> c_long {
    unsafe { libc::strtol(s, end, base) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __isoc23_strtoll(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> c_longlong {
    unsafe { libc::strtoll(s, end, base) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __isoc23_strtoull(
    s: *const c_char,
    end: *mut *mut c_char,
    base: c_int,
) -> c_ulonglong {
    unsafe { libc::strtoull(s, end, base) }
}
