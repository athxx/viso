//! Runtime lookup of the system libraries the Linux / BSD and Android
//! resolvers call, so a binary starts on systems that lack them.

use std::ffi::{CStr, c_void};

/// Open the first of `names` that loads. The handle is never closed, so every
/// symbol resolved from it stays valid for the process lifetime.
pub fn open(names: &[&CStr]) -> Option<*mut c_void> {
    names.iter().find_map(|name| {
        // SAFETY: `name` is NUL-terminated; `dlopen` has no other precondition.
        let library = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW) };
        (!library.is_null()).then_some(library)
    })
}

/// The address of `name` in `library` as a function pointer of type `F`.
///
/// # Safety
/// `library` must be a handle from [`open`], and `F` must be the C
/// function-pointer type the library declares for `name`.
pub unsafe fn symbol<F: Copy>(library: *mut c_void, name: &CStr) -> Option<F> {
    assert_eq!(size_of::<F>(), size_of::<*mut c_void>());
    // SAFETY: `library` is a live handle and `name` is NUL-terminated.
    let address = unsafe { libc::dlsym(library, name.as_ptr()) };
    // SAFETY: the caller guarantees `F` is the symbol's function-pointer
    // type, which has the size of the non-null address it is read from.
    (!address.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, F>(&address) })
}
