//! `libgbm`, loaded at run time: the buffer allocator an EGL driver renders
//! a scanout's frames into. A CPU-drawn display never loads it.

use std::ffi::{CStr, c_int, c_void};
use std::os::fd::RawFd;

const BO_USE_SCANOUT: u32 = 1 << 0;
const BO_USE_RENDERING: u32 = 1 << 2;

/// `union gbm_bo_handle`, returned by value (in memory on 32-bit Arm, in a
/// register elsewhere: the union type keeps the C ABI right).
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code, reason = "the C layout's other members")]
union BoHandle {
    ptr: *mut c_void,
    s32: i32,
    u32: u32,
    s64: i64,
    u64: u64,
}

/// An open shared library, closed on drop.
pub(super) struct Library(*mut c_void);

impl Library {
    /// The first of `names` that loads.
    pub fn open(names: &[&CStr]) -> Option<Library> {
        names.iter().find_map(|name| {
            // SAFETY: loading a system library by name runs its initializers,
            // the documented way to reach it.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            // Made only for a handle: a null one dropped would be closed.
            (!handle.is_null()).then(|| Library(handle))
        })
    }

    /// The address of `name`, or null.
    pub fn symbol(&self, name: &CStr) -> *mut c_void {
        // SAFETY: a lookup in a live handle; the address is only used as the
        // type its caller knows it to have.
        unsafe { libc::dlsym(self.0, name.as_ptr()) }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the handle `dlopen` returned, closed once; nothing resolved
        // from it outlives the struct holding both.
        unsafe { libc::dlclose(self.0) };
    }
}

/// Resolves each named symbol of `lib` as the field's function type, or
/// returns `None` from the enclosing function when one is missing.
macro_rules! resolve {
    ($lib:expr, $($f:ident: $t:ty),* $(,)?) => {
        ($({
            let ptr = $lib.symbol(
                CStr::from_bytes_with_nul(concat!(stringify!($f), "\0").as_bytes()).expect("a C name"),
            );
            if ptr.is_null() {
                return None;
            }
            // SAFETY: libgbm exports this symbol with this signature
            // (`gbm.h`).
            unsafe { std::mem::transmute::<*mut c_void, $t>(ptr) }
        },)*)
    };
}

/// The `libgbm` entry points the scanout uses.
pub(super) struct Gbm {
    create_device: unsafe extern "C" fn(c_int) -> *mut c_void,
    surface_create: unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32) -> *mut c_void,
    surface_destroy: unsafe extern "C" fn(*mut c_void),
    lock_front_buffer: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    release_buffer: unsafe extern "C" fn(*mut c_void, *mut c_void),
    bo_get_handle: unsafe extern "C" fn(*mut c_void) -> BoHandle,
    bo_get_stride: unsafe extern "C" fn(*mut c_void) -> u32,
    _lib: Library,
}

impl Gbm {
    pub fn load() -> Option<Gbm> {
        let lib = Library::open(&[c"libgbm.so.1", c"libgbm.so"])?;
        type CreateDevice = unsafe extern "C" fn(c_int) -> *mut c_void;
        type Destroy = unsafe extern "C" fn(*mut c_void);
        type SurfaceCreate = unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32) -> *mut c_void;
        type Lock = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
        type Release = unsafe extern "C" fn(*mut c_void, *mut c_void);
        type Handle = unsafe extern "C" fn(*mut c_void) -> BoHandle;
        type Stride = unsafe extern "C" fn(*mut c_void) -> u32;
        let (
            gbm_create_device,
            gbm_surface_create,
            gbm_surface_destroy,
            gbm_surface_lock_front_buffer,
            gbm_surface_release_buffer,
            gbm_bo_get_handle,
            gbm_bo_get_stride,
        ) = resolve!(lib,
            gbm_create_device: CreateDevice,
            gbm_surface_create: SurfaceCreate,
            gbm_surface_destroy: Destroy,
            gbm_surface_lock_front_buffer: Lock,
            gbm_surface_release_buffer: Release,
            gbm_bo_get_handle: Handle,
            gbm_bo_get_stride: Stride,
        );
        Some(Gbm {
            create_device: gbm_create_device,
            surface_create: gbm_surface_create,
            surface_destroy: gbm_surface_destroy,
            lock_front_buffer: gbm_surface_lock_front_buffer,
            release_buffer: gbm_surface_release_buffer,
            bo_get_handle: gbm_bo_get_handle,
            bo_get_stride: gbm_bo_get_stride,
            _lib: lib,
        })
    }

    /// A GBM device over the card `fd` (which must outlive it).
    pub fn create_device(&self, fd: RawFd) -> *mut c_void {
        // SAFETY: `fd` is an open DRM card.
        unsafe { (self.create_device)(fd) }
    }

    /// A scanout-capable render surface of `width`×`height` in `format`.
    ///
    /// # Safety
    ///
    /// `device` is a live GBM device.
    pub unsafe fn create_surface(
        &self,
        device: *mut c_void,
        width: u32,
        height: u32,
        format: u32,
    ) -> *mut c_void {
        // SAFETY: the caller's contract.
        unsafe {
            (self.surface_create)(
                device,
                width,
                height,
                format,
                BO_USE_SCANOUT | BO_USE_RENDERING,
            )
        }
    }

    /// # Safety
    ///
    /// `surface` is live, no EGL surface is made on it any more, and no
    /// buffer of it is still locked.
    pub unsafe fn destroy_surface(&self, surface: *mut c_void) {
        // SAFETY: the caller's contract.
        unsafe { (self.surface_destroy)(surface) }
    }

    /// The buffer the last swap finished, locked until
    /// [`release`](Self::release); null when there is none.
    ///
    /// # Safety
    ///
    /// `surface` is live.
    pub unsafe fn lock_front(&self, surface: *mut c_void) -> *mut c_void {
        // SAFETY: the caller's contract.
        unsafe { (self.lock_front_buffer)(surface) }
    }

    /// # Safety
    ///
    /// `bo` is a buffer of `surface` locked by [`lock_front`](Self::lock_front).
    pub unsafe fn release(&self, surface: *mut c_void, bo: *mut c_void) {
        // SAFETY: the caller's contract.
        unsafe { (self.release_buffer)(surface, bo) }
    }

    /// A locked buffer's GEM handle and row pitch.
    ///
    /// # Safety
    ///
    /// `bo` is a live buffer object.
    pub unsafe fn handle_and_stride(&self, bo: *mut c_void) -> (u32, u32) {
        // SAFETY: the caller's contract; a GEM handle is the union's `u32`.
        unsafe { ((self.bo_get_handle)(bo).u32, (self.bo_get_stride)(bo)) }
    }
}
