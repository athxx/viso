//! The OpenGL ES 3.0, EGL 1.5 and `wayland-egl` entry points the backend
//! calls, loaded at run time: a device without GL never links against it.
//!
//! GL functions resolve through `eglGetProcAddress` (EGL 1.5 returns core
//! functions too) and fall back to the GLES library's own symbols.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::{CStr, c_char, c_void};

use libloading::Library;

pub type GLenum = u32;
pub type GLuint = u32;
pub type GLint = i32;
pub type GLsizei = i32;
pub type GLboolean = u8;
pub type GLbitfield = u32;
pub type GLfloat = f32;
pub type GLintptr = isize;
pub type GLsizeiptr = isize;
pub type GLsync = *const c_void;
pub type GLuint64 = u64;

pub type EGLDisplay = *mut c_void;
pub type EGLConfig = *mut c_void;
pub type EGLContext = *mut c_void;
pub type EGLSurface = *mut c_void;
pub type EGLint = i32;
pub type EGLenum = u32;
pub type EGLBoolean = u32;
pub type EGLAttrib = isize;

pub const NO_DISPLAY: EGLDisplay = std::ptr::null_mut();
pub const NO_CONTEXT: EGLContext = std::ptr::null_mut();
pub const NO_SURFACE: EGLSurface = std::ptr::null_mut();

// EGL
pub const EGL_TRUE: EGLBoolean = 1;
pub const EGL_NONE: EGLint = 0x3038;
pub const EGL_SUCCESS: EGLint = 0x3000;
pub const EGL_CONTEXT_LOST: EGLint = 0x300E;
pub const EGL_RED_SIZE: EGLint = 0x3024;
pub const EGL_GREEN_SIZE: EGLint = 0x3023;
pub const EGL_BLUE_SIZE: EGLint = 0x3022;
pub const EGL_ALPHA_SIZE: EGLint = 0x3021;
pub const EGL_DEPTH_SIZE: EGLint = 0x3025;
pub const EGL_STENCIL_SIZE: EGLint = 0x3026;
pub const EGL_SURFACE_TYPE: EGLint = 0x3033;
pub const EGL_WINDOW_BIT: EGLint = 0x0004;
pub const EGL_PBUFFER_BIT: EGLint = 0x0001;
pub const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
pub const EGL_OPENGL_ES3_BIT: EGLint = 0x0040;
pub const EGL_NATIVE_VISUAL_ID: EGLint = 0x302E;
pub const EGL_WIDTH: EGLint = 0x3057;
pub const EGL_GL_COLORSPACE_KHR: EGLint = 0x309D;
pub const EGL_GL_COLORSPACE_SRGB_KHR: EGLint = 0x3089;
pub const EGL_HEIGHT: EGLint = 0x3056;
pub const EGL_OPENGL_ES_API: EGLenum = 0x30A0;
pub const EGL_CONTEXT_MAJOR_VERSION: EGLint = 0x3098;
pub const EGL_CONTEXT_MINOR_VERSION: EGLint = 0x30FB;
pub const EGL_EXTENSIONS: EGLint = 0x3055;
pub const EGL_BUFFER_AGE_EXT: EGLint = 0x313D;
pub const EGL_PLATFORM_WAYLAND_KHR: EGLenum = 0x31D8;
pub const EGL_PLATFORM_XCB_EXT: EGLenum = 0x31DC;
pub const EGL_PLATFORM_XCB_SCREEN_EXT: EGLAttrib = 0x31DE;
pub const EGL_PLATFORM_GBM_KHR: EGLenum = 0x31D7;
pub const EGL_PLATFORM_SURFACELESS_MESA: EGLenum = 0x31DD;
pub const EGL_DRAW: EGLint = 0x3059;
pub const EGL_READ: EGLint = 0x305A;
pub const EGL_DEFAULT_DISPLAY: *mut c_void = std::ptr::null_mut();

// GL
pub const GL_NO_ERROR: GLenum = 0;
pub const GL_TRUE: GLboolean = 1;
pub const GL_FALSE: GLboolean = 0;
pub const GL_TRIANGLES: GLenum = 0x0004;
pub const GL_ONE: GLenum = 1;
pub const GL_ONE_MINUS_SRC_ALPHA: GLenum = 0x0303;
pub const GL_BLEND: GLenum = 0x0BE2;
pub const GL_SCISSOR_TEST: GLenum = 0x0C11;
pub const GL_DEPTH_TEST: GLenum = 0x0B71;
pub const GL_CULL_FACE: GLenum = 0x0B44;
pub const GL_DITHER: GLenum = 0x0BD0;
pub const GL_COLOR_BUFFER_BIT: GLbitfield = 0x4000;
pub const GL_DEPTH_BUFFER_BIT: GLbitfield = 0x0100;
pub const GL_DEPTH_ATTACHMENT: GLenum = 0x8D00;
pub const GL_BACK: GLenum = 0x0405;
pub const GL_FRONT: GLenum = 0x0404;
pub const GL_CW: GLenum = 0x0900;
pub const GL_CCW: GLenum = 0x0901;
pub const GL_ALWAYS: GLenum = 0x0207;
pub const GL_LESS: GLenum = 0x0201;
pub const GL_LEQUAL: GLenum = 0x0203;
pub const GL_GREATER: GLenum = 0x0204;
pub const GL_GEQUAL: GLenum = 0x0206;
pub const GL_UNSIGNED_BYTE: GLenum = 0x1401;
pub const GL_UNSIGNED_SHORT: GLenum = 0x1403;
pub const GL_UNSIGNED_INT: GLenum = 0x1405;
pub const GL_INT: GLenum = 0x1404;
pub const GL_FLOAT: GLenum = 0x1406;
pub const GL_HALF_FLOAT: GLenum = 0x140B;
pub const GL_UNSIGNED_SHORT_5_6_5: GLenum = 0x8363;
pub const GL_UNSIGNED_SHORT_4_4_4_4: GLenum = 0x8033;
pub const GL_UNSIGNED_SHORT_5_5_5_1: GLenum = 0x8034;
pub const GL_RED: GLenum = 0x1903;
pub const GL_RGB: GLenum = 0x1907;
pub const GL_RGBA: GLenum = 0x1908;
pub const GL_DEPTH_COMPONENT: GLenum = 0x1902;
pub const GL_R8: GLenum = 0x8229;
pub const GL_RGBA8: GLenum = 0x8058;
pub const GL_RGB565: GLenum = 0x8D62;
pub const GL_RGBA4: GLenum = 0x8056;
pub const GL_RGB5_A1: GLenum = 0x8057;
pub const GL_COMPRESSED_RGB8_ETC2: GLenum = 0x9274;
pub const GL_COMPRESSED_RGB8_PUNCHTHROUGH_ALPHA1_ETC2: GLenum = 0x9276;
pub const GL_COMPRESSED_RGBA8_ETC2_EAC: GLenum = 0x9278;
pub const GL_COMPRESSED_RGBA_ASTC_4X4_KHR: GLenum = 0x93B0;
pub const GL_COMPRESSED_RGBA_ASTC_6X6_KHR: GLenum = 0x93B4;
pub const GL_COMPRESSED_RGBA_ASTC_8X8_KHR: GLenum = 0x93B7;
pub const GL_COMPRESSED_RGBA_S3TC_DXT1_EXT: GLenum = 0x83F1;
pub const GL_COMPRESSED_RGBA_S3TC_DXT5_EXT: GLenum = 0x83F3;
pub const GL_COMPRESSED_RGBA_BPTC_UNORM_EXT: GLenum = 0x8E8C;
pub const GL_SRGB8_ALPHA8: GLenum = 0x8C43;
pub const GL_COMPRESSED_SRGB8_ETC2: GLenum = 0x9275;
pub const GL_COMPRESSED_SRGB8_PUNCHTHROUGH_ALPHA1_ETC2: GLenum = 0x9277;
pub const GL_COMPRESSED_SRGB8_ALPHA8_ETC2_EAC: GLenum = 0x9279;
pub const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_4X4_KHR: GLenum = 0x93D0;
pub const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6X6_KHR: GLenum = 0x93D4;
pub const GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X8_KHR: GLenum = 0x93D7;
pub const GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT1_EXT: GLenum = 0x8C4D;
pub const GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT5_EXT: GLenum = 0x8C4F;
pub const GL_COMPRESSED_SRGB_ALPHA_BPTC_UNORM_EXT: GLenum = 0x8E8D;
pub const GL_RGBA16F: GLenum = 0x881A;
pub const GL_DEPTH_COMPONENT32F: GLenum = 0x8CAC;
pub const GL_TEXTURE_2D: GLenum = 0x0DE1;
pub const GL_TEXTURE0: GLenum = 0x84C0;
pub const GL_TEXTURE_MIN_FILTER: GLenum = 0x2801;
pub const GL_TEXTURE_MAG_FILTER: GLenum = 0x2800;
pub const GL_TEXTURE_WRAP_S: GLenum = 0x2802;
pub const GL_TEXTURE_WRAP_T: GLenum = 0x2803;
pub const GL_NEAREST: GLint = 0x2600;
pub const GL_LINEAR: GLint = 0x2601;
pub const GL_LINEAR_MIPMAP_LINEAR: GLint = 0x2703;
pub const GL_CLAMP_TO_EDGE: GLint = 0x812F;
pub const GL_REPEAT: GLint = 0x2901;
pub const GL_MIRRORED_REPEAT: GLint = 0x8370;
pub const GL_UNPACK_ALIGNMENT: GLenum = 0x0CF5;
pub const GL_PACK_ALIGNMENT: GLenum = 0x0D05;
pub const GL_ARRAY_BUFFER: GLenum = 0x8892;
pub const GL_ELEMENT_ARRAY_BUFFER: GLenum = 0x8893;
pub const GL_UNIFORM_BUFFER: GLenum = 0x8A11;
pub const GL_COPY_READ_BUFFER: GLenum = 0x8F36;
pub const GL_COPY_WRITE_BUFFER: GLenum = 0x8F37;
pub const GL_DYNAMIC_DRAW: GLenum = 0x88E8;
pub const GL_STREAM_DRAW: GLenum = 0x88E0;
pub const GL_FRAMEBUFFER: GLenum = 0x8D40;
pub const GL_DRAW_FRAMEBUFFER: GLenum = 0x8CA9;
pub const GL_COLOR_ATTACHMENT0: GLenum = 0x8CE0;
pub const GL_COLOR: GLenum = 0x1800;
pub const GL_FRAMEBUFFER_COMPLETE: GLenum = 0x8CD5;
pub const GL_VERTEX_SHADER: GLenum = 0x8B31;
pub const GL_FRAGMENT_SHADER: GLenum = 0x8B30;
pub const GL_COMPILE_STATUS: GLenum = 0x8B81;
pub const GL_LINK_STATUS: GLenum = 0x8B82;
pub const GL_INFO_LOG_LENGTH: GLenum = 0x8B84;
pub const GL_INVALID_INDEX: GLuint = 0xFFFF_FFFF;
pub const GL_MAX_TEXTURE_SIZE: GLenum = 0x0D33;
pub const GL_UNIFORM_BUFFER_OFFSET_ALIGNMENT: GLenum = 0x8A34;
pub const GL_MAX_UNIFORM_BLOCK_SIZE: GLenum = 0x8A30;
pub const GL_EXTENSIONS: GLenum = 0x1F03;
pub const GL_NUM_EXTENSIONS: GLenum = 0x821D;
pub const GL_VERSION: GLenum = 0x1F02;
pub const GL_RENDERER: GLenum = 0x1F01;
pub const GL_SYNC_GPU_COMMANDS_COMPLETE: GLenum = 0x9117;
pub const GL_SYNC_FLUSH_COMMANDS_BIT: GLbitfield = 0x0001;
pub const GL_ALREADY_SIGNALED: GLenum = 0x911A;
pub const GL_CONDITION_SATISFIED: GLenum = 0x911C;
pub const GL_TIMEOUT_EXPIRED: GLenum = 0x911B;
pub const GL_WAIT_FAILED: GLenum = 0x911D;

macro_rules! table {
    ($(#[$doc:meta])* $name:ident { $($f:ident: fn($($p:ty),*) $(-> $r:ty)?;)* }) => {
        $(#[$doc])*
        pub struct $name {
            $(pub $f: unsafe extern "C" fn($($p),*) $(-> $r)?,)*
        }

        impl $name {
            /// Every entry point through `resolve`, or the first one missing.
            ///
            /// # Safety
            ///
            /// `resolve` must return, for each name, null or the address of
            /// that function with this signature.
            unsafe fn load(resolve: &mut dyn FnMut(&CStr) -> *const c_void) -> Result<$name, String> {
                Ok($name {
                    $($f: {
                        let name = concat!(stringify!($f), "\0");
                        let name = CStr::from_bytes_with_nul(name.as_bytes()).expect("a C name");
                        let ptr = resolve(name);
                        if ptr.is_null() {
                            return Err(format!("missing `{}`", stringify!($f)));
                        }
                        // SAFETY: the caller vouches that a non-null address
                        // is this function with this signature.
                        unsafe { std::mem::transmute::<*const c_void, unsafe extern "C" fn($($p),*) $(-> $r)?>(ptr) }
                    },)*
                })
            }
        }
    };
}

table! {
    /// The EGL 1.5 entry points.
    Egl {
        eglGetProcAddress: fn(*const c_char) -> *const c_void;
        eglGetError: fn() -> EGLint;
        eglGetDisplay: fn(*mut c_void) -> EGLDisplay;
        eglGetPlatformDisplay: fn(EGLenum, *mut c_void, *const EGLAttrib) -> EGLDisplay;
        eglInitialize: fn(EGLDisplay, *mut EGLint, *mut EGLint) -> EGLBoolean;
        eglTerminate: fn(EGLDisplay) -> EGLBoolean;
        eglQueryString: fn(EGLDisplay, EGLint) -> *const c_char;
        eglBindAPI: fn(EGLenum) -> EGLBoolean;
        eglChooseConfig: fn(EGLDisplay, *const EGLint, *mut EGLConfig, EGLint, *mut EGLint) -> EGLBoolean;
        eglGetConfigAttrib: fn(EGLDisplay, EGLConfig, EGLint, *mut EGLint) -> EGLBoolean;
        eglCreateContext: fn(EGLDisplay, EGLConfig, EGLContext, *const EGLint) -> EGLContext;
        eglDestroyContext: fn(EGLDisplay, EGLContext) -> EGLBoolean;
        eglCreatePlatformWindowSurface: fn(EGLDisplay, EGLConfig, *mut c_void, *const EGLAttrib) -> EGLSurface;
        eglCreatePbufferSurface: fn(EGLDisplay, EGLConfig, *const EGLint) -> EGLSurface;
        eglDestroySurface: fn(EGLDisplay, EGLSurface) -> EGLBoolean;
        eglMakeCurrent: fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> EGLBoolean;
        eglSwapBuffers: fn(EGLDisplay, EGLSurface) -> EGLBoolean;
        eglSwapInterval: fn(EGLDisplay, EGLint) -> EGLBoolean;
        eglQuerySurface: fn(EGLDisplay, EGLSurface, EGLint, *mut EGLint) -> EGLBoolean;
        eglGetCurrentDisplay: fn() -> EGLDisplay;
        eglGetCurrentContext: fn() -> EGLContext;
        eglGetCurrentSurface: fn(EGLint) -> EGLSurface;
    }
}

table! {
    /// The OpenGL ES 3.0 entry points.
    Gl {
        glGetError: fn() -> GLenum;
        glGetIntegerv: fn(GLenum, *mut GLint);
        glGetString: fn(GLenum) -> *const u8;
        glGetStringi: fn(GLenum, GLuint) -> *const u8;
        glEnable: fn(GLenum);
        glDisable: fn(GLenum);
        glBlendFunc: fn(GLenum, GLenum);
        glViewport: fn(GLint, GLint, GLsizei, GLsizei);
        glScissor: fn(GLint, GLint, GLsizei, GLsizei);
        glClearColor: fn(GLfloat, GLfloat, GLfloat, GLfloat);
        glClear: fn(GLbitfield);
        glClearDepthf: fn(GLfloat);
        glDepthFunc: fn(GLenum);
        glDepthMask: fn(GLboolean);
        glCullFace: fn(GLenum);
        glFrontFace: fn(GLenum);
        glFlush: fn();
        glFinish: fn();
        glPixelStorei: fn(GLenum, GLint);
        glReadPixels: fn(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, *mut c_void);
        glGenBuffers: fn(GLsizei, *mut GLuint);
        glDeleteBuffers: fn(GLsizei, *const GLuint);
        glBindBuffer: fn(GLenum, GLuint);
        glBufferData: fn(GLenum, GLsizeiptr, *const c_void, GLenum);
        glBufferSubData: fn(GLenum, GLintptr, GLsizeiptr, *const c_void);
        glBindBufferRange: fn(GLenum, GLuint, GLuint, GLintptr, GLsizeiptr);
        glCopyBufferSubData: fn(GLenum, GLenum, GLintptr, GLintptr, GLsizeiptr);
        glGenTextures: fn(GLsizei, *mut GLuint);
        glDeleteTextures: fn(GLsizei, *const GLuint);
        glBindTexture: fn(GLenum, GLuint);
        glActiveTexture: fn(GLenum);
        glTexStorage2D: fn(GLenum, GLsizei, GLenum, GLsizei, GLsizei);
        glTexSubImage2D: fn(GLenum, GLint, GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, *const c_void);
        glCompressedTexSubImage2D: fn(GLenum, GLint, GLint, GLint, GLsizei, GLsizei, GLenum, GLsizei, *const c_void);
        glTexParameteri: fn(GLenum, GLenum, GLint);
        glGenSamplers: fn(GLsizei, *mut GLuint);
        glDeleteSamplers: fn(GLsizei, *const GLuint);
        glBindSampler: fn(GLuint, GLuint);
        glSamplerParameteri: fn(GLuint, GLenum, GLint);
        glGenFramebuffers: fn(GLsizei, *mut GLuint);
        glDeleteFramebuffers: fn(GLsizei, *const GLuint);
        glBindFramebuffer: fn(GLenum, GLuint);
        glFramebufferTexture2D: fn(GLenum, GLenum, GLenum, GLuint, GLint);
        glCheckFramebufferStatus: fn(GLenum) -> GLenum;
        glInvalidateFramebuffer: fn(GLenum, GLsizei, *const GLenum);
        glCreateShader: fn(GLenum) -> GLuint;
        glShaderSource: fn(GLuint, GLsizei, *const *const c_char, *const GLint);
        glCompileShader: fn(GLuint);
        glGetShaderiv: fn(GLuint, GLenum, *mut GLint);
        glGetShaderInfoLog: fn(GLuint, GLsizei, *mut GLsizei, *mut c_char);
        glDeleteShader: fn(GLuint);
        glCreateProgram: fn() -> GLuint;
        glAttachShader: fn(GLuint, GLuint);
        glLinkProgram: fn(GLuint);
        glGetProgramiv: fn(GLuint, GLenum, *mut GLint);
        glGetProgramInfoLog: fn(GLuint, GLsizei, *mut GLsizei, *mut c_char);
        glDeleteProgram: fn(GLuint);
        glUseProgram: fn(GLuint);
        glGetUniformLocation: fn(GLuint, *const c_char) -> GLint;
        glGetUniformBlockIndex: fn(GLuint, *const c_char) -> GLuint;
        glUniformBlockBinding: fn(GLuint, GLuint, GLuint);
        glUniform1i: fn(GLint, GLint);
        glUniform1ui: fn(GLint, GLuint);
        glUniform4f: fn(GLint, GLfloat, GLfloat, GLfloat, GLfloat);
        glGenVertexArrays: fn(GLsizei, *mut GLuint);
        glDeleteVertexArrays: fn(GLsizei, *const GLuint);
        glBindVertexArray: fn(GLuint);
        glEnableVertexAttribArray: fn(GLuint);
        glVertexAttribPointer: fn(GLuint, GLint, GLenum, GLboolean, GLsizei, *const c_void);
        glVertexAttribIPointer: fn(GLuint, GLint, GLenum, GLsizei, *const c_void);
        glVertexAttribDivisor: fn(GLuint, GLuint);
        glDrawArraysInstanced: fn(GLenum, GLint, GLsizei, GLsizei);
        glDrawElements: fn(GLenum, GLsizei, GLenum, *const c_void);
        glDrawElementsInstanced: fn(GLenum, GLsizei, GLenum, *const c_void, GLsizei);
        glFenceSync: fn(GLenum, GLbitfield) -> GLsync;
        glClientWaitSync: fn(GLsync, GLbitfield, GLuint64) -> GLenum;
        glDeleteSync: fn(GLsync);
    }
}

/// `eglSwapBuffersWithDamageKHR` / `EXT`: present with the damaged rects.
pub type SwapWithDamage =
    unsafe extern "C" fn(EGLDisplay, EGLSurface, *const EGLint, EGLint) -> EGLBoolean;

/// `eglSetDamageRegionKHR`: the region this frame will draw, so a tiler
/// loads only those tiles of the old buffer.
pub type SetDamageRegion =
    unsafe extern "C" fn(EGLDisplay, EGLSurface, *const EGLint, EGLint) -> EGLBoolean;

/// `wl_egl_window_*`, the Wayland side of an EGL window.
pub struct WaylandEgl {
    pub create: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void,
    pub resize: unsafe extern "C" fn(*mut c_void, i32, i32, i32, i32),
    pub destroy: unsafe extern "C" fn(*mut c_void),
    _lib: Library,
}

/// The loaded libraries and their entry points.
pub struct Libs {
    pub egl: Egl,
    pub gl: Gl,
    pub swap_with_damage: Option<SwapWithDamage>,
    pub set_damage_region: Option<SetDamageRegion>,
    _egl: Library,
    _gles: Library,
}

/// Opens the first of `names` that loads.
fn open(names: &[&str]) -> Result<Library, String> {
    let mut last = String::new();
    for name in names {
        // SAFETY: loading a system graphics library runs its initializers,
        // the documented way to reach EGL and GLES.
        match unsafe { Library::new(name) } {
            Ok(lib) => return Ok(lib),
            Err(e) => last = format!("{name}: {e}"),
        }
    }
    Err(last)
}

/// The address of `name` in `lib`, or null.
fn symbol(lib: &Library, name: &CStr) -> *const c_void {
    // SAFETY: the symbol is only read as an address; its type is the
    // caller's to know.
    unsafe { lib.get::<*const c_void>(name.to_bytes_with_nul()) }.map_or(std::ptr::null(), |s| *s)
}

impl Libs {
    /// Loads EGL and GLES 3.
    pub fn load() -> Result<Libs, String> {
        let egl_lib = open(&["libEGL.so.1", "libEGL.so"])?;
        let gles_lib = open(&["libGLESv2.so.2", "libGLESv2.so"])?;
        // SAFETY: each name is looked up in libEGL, whose exports have the
        // EGL 1.5 signatures the table declares.
        let egl = unsafe { Egl::load(&mut |name| symbol(&egl_lib, name)) }?;
        let proc_address = egl.eglGetProcAddress;
        // SAFETY: `eglGetProcAddress` returns a GL function of the queried
        // name or null; the GLES library's own export is the same function.
        let gl = unsafe {
            Gl::load(&mut |name| {
                let ptr = symbol(&gles_lib, name);
                if ptr.is_null() {
                    proc_address(name.as_ptr())
                } else {
                    ptr
                }
            })
        }?;
        let swap_with_damage = [
            c"eglSwapBuffersWithDamageKHR",
            c"eglSwapBuffersWithDamageEXT",
        ]
        .iter()
        .map(|name| {
            // SAFETY: a lookup by name; null when the extension is absent.
            unsafe { proc_address(name.as_ptr()) }
        })
        .find(|p| !p.is_null())
        .map(|p| {
            // SAFETY: both extensions define this signature.
            unsafe { std::mem::transmute::<*const c_void, SwapWithDamage>(p) }
        });
        // SAFETY: a lookup by name; null when the extension is absent.
        let region = unsafe { proc_address(c"eglSetDamageRegionKHR".as_ptr()) };
        let set_damage_region = (!region.is_null()).then(|| {
            // SAFETY: `EGL_KHR_partial_update` defines this signature.
            unsafe { std::mem::transmute::<*const c_void, SetDamageRegion>(region) }
        });
        Ok(Libs {
            egl,
            gl,
            swap_with_damage,
            set_damage_region,
            _egl: egl_lib,
            _gles: gles_lib,
        })
    }
}

impl WaylandEgl {
    /// Loads `libwayland-egl`.
    pub fn load() -> Result<WaylandEgl, String> {
        let lib = open(&["libwayland-egl.so.1", "libwayland-egl.so"])?;
        let get = |name: &CStr| {
            let p = symbol(&lib, name);
            if p.is_null() {
                Err(format!("missing `{}`", name.to_string_lossy()))
            } else {
                Ok(p)
            }
        };
        let (create, resize, destroy) = (
            get(c"wl_egl_window_create")?,
            get(c"wl_egl_window_resize")?,
            get(c"wl_egl_window_destroy")?,
        );
        // SAFETY: libwayland-egl exports these three with these signatures.
        unsafe {
            Ok(WaylandEgl {
                create: std::mem::transmute::<
                    *const c_void,
                    unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void,
                >(create),
                resize: std::mem::transmute::<
                    *const c_void,
                    unsafe extern "C" fn(*mut c_void, i32, i32, i32, i32),
                >(resize),
                destroy: std::mem::transmute::<*const c_void, unsafe extern "C" fn(*mut c_void)>(
                    destroy,
                ),
                _lib: lib,
            })
        }
    }
}
