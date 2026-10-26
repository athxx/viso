//! `Gles3Backend` — OpenGL ES 3.0 over EGL: the backend of
//! the Embedded-GPU profile, for the SoCs whose drivers are GL only (Mali,
//! Vivante, VideoCore, PowerVR) and for any Linux or Android device when the
//! profile asks for it.
//!
//! EGL and GLES are loaded at run time ([`sys`]); the display and context are
//! created with the first surface, on that window's platform (Wayland, XCB,
//! Android, a KMS scanout's GBM surface, or a pbuffer for a headless
//! surface). A scanout's frames go to the platform's page flips: each swap is
//! reported to its [`Scanout`](viso_handle::Scanout) with the frame's damage.
//!
//! GLES 3.0 has no bind groups, no push constants and no storage buffers, so
//! the RHI is emulated where it differs:
//!
//! - **uniforms**: a draw's inline bytes are packed into one uniform buffer
//!   per draw list and bound by range at block binding 0;
//! - **bind groups**: a group's textures go to texture units `0..`, its
//!   sampler state to a sampler object bound to each of those units;
//! - **instances**: a built-in reads them as per-instance attributes (a
//!   divisor-1 vertex array, re-pointed at the draw's offset); a user program
//!   reads them as `uvec4` words from uniform block binding 1, copied a
//!   [`INSTANCE_CHUNK`] at a time into a uniform ring, with the chunk's first
//!   index in `viso_base_instance`;
//! - **`Bgra8Unorm`** textures are stored `RGBA8` (core GLES has no BGRA
//!   storage): uploads swap red and blue, rendering needs nothing; the sRGB
//!   variants are `SRGB8_ALPHA8`, which GLES encodes into and decodes from;
//! - an 8-bit **window** is created in the sRGB colorspace
//!   (`EGL_KHR_gl_colorspace`), reporting `Bgra8UnormSrgb`, so the default
//!   framebuffer encodes what the pipelines write;
//! - **texture targets** render with clip-space y negated (`viso_target`), so
//!   their rows are stored top-down like every other backend's and a sample
//!   at `v = 0` reads the top; the surface keeps GL's own orientation;
//! - a **turned panel** whose display cannot turn the image
//!   ([`Scanout::turns`](viso_handle::Scanout::turns)) is drawn turned: clip
//!   positions, scissors and damage go onto a framebuffer the panel's shape,
//!   and fragments see the image's own positions.
//!
//! A program compiles on a context sharing this one's objects, so the
//! compiler can run on any thread ([`Gles3Compiler`]); its vertex array is
//! made when it is installed, vertex arrays not being shared.
//!
//! A partial redraw costs the damage: the surface reports
//! `EGL_BUFFER_AGE_EXT`, declares the region a frame draws — its pass's
//! scissors — with `EGL_KHR_partial_update` before the first draw, so a tiler
//! loads only those tiles, and presents with `eglSwapBuffersWithDamage`.

mod sys;

use std::ffi::{CStr, CString, c_void};
use std::sync::{Arc, Mutex};

use viso_handle::{RawWindowHandle, Scanout, ScanoutFormat};

use self::sys::*;
use crate::backend::{
    DrawCommand, DrawList, Frame, Geometry, GpuBackend, IndexFormat, LoadOp, RenderPass,
    RenderTarget,
};
use crate::instance::{AttrFormat, InstanceLayout, LayoutError};
use crate::program::{ProgramAbi, ProgramCode, ProgramCompiler, ProgramDesc};
use crate::resource::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BuiltinShader, Caps,
    CompareFunction, CullMode, DepthState, FilterMode, PipelineDesc, SampledFormats, SamplerDesc,
    ShaderCode, ShaderLang, TextureDesc, TextureFormat,
};
use crate::scene::{self, SceneInstance};
use crate::slots::SlotMap;
use crate::{BindGroupId, BufferId, PipelineId, SamplerId, SurfaceId, TextureId};

/// The bytes of one bound chunk of a program's instances (the uniform block
/// size every GLES 3.0 device supports).
pub const INSTANCE_CHUNK: usize = 16384;

/// The version line and stage define a stage's source opens with.
fn stage_prelude(vertex: bool) -> &'static str {
    if vertex {
        "#version 300 es\n#define VISO_VERTEX 1\n"
    } else {
        "#version 300 es\n"
    }
}

struct GlBuffer {
    name: GLuint,
    size: usize,
}

struct GlTexture {
    name: GLuint,
    width: u32,
    height: u32,
    format: TextureFormat,
    /// The framebuffer drawing into it, made by its first pass.
    fbo: Option<GLuint>,
    /// The framebuffer of passes with a depth attachment, and the depth
    /// texture it was made with.
    depth_fbo: Option<(TextureId, GLuint)>,
}

/// One vertex attribute of a built-in pipeline.
#[derive(Clone, Copy)]
struct Attr {
    location: GLuint,
    lanes: GLint,
    ty: GLenum,
    integer: bool,
    offset: usize,
}

enum Kind {
    /// A built-in: its attributes over one instance (or vertex) of `stride`,
    /// and for the scene the instance attributes after them.
    Builtin {
        attrs: Vec<Attr>,
        stride: usize,
        instance: Vec<Attr>,
    },
    /// A user program.
    Program {
        abi: ProgramAbi,
        base_instance: GLint,
    },
}

struct GlPipeline {
    program: GLuint,
    vao: GLuint,
    blend: BlendMode,
    depth: Option<DepthState>,
    cull: CullMode,
    /// `viso_target`'s location.
    target: GLint,
    kind: Kind,
}

/// What a window surface was made from, kept to resize and release it.
enum Native {
    Wayland(*mut c_void),
    /// The window id whose address EGL's XCB platform was given.
    Xcb {
        _id: Box<u32>,
    },
    Android,
    /// A KMS scanout's GBM surface; each swap is reported to it.
    Scanout(*const dyn Scanout),
    Pbuffer,
}

struct GlSurface {
    egl: EGLSurface,
    native: Native,
    width: u32,
    height: u32,
    format: TextureFormat,
    /// Whether this frame's damage region is declared (or the frame drew
    /// before it could be).
    region_set: bool,
    /// Quarter turns the image is drawn with onto the framebuffer, which is
    /// then the panel's shape (a KMS scanout whose display cannot turn it).
    turns: u8,
}

/// The display and the contexts: the backend's, and the compiler's sharing
/// its objects.
struct Display {
    display: EGLDisplay,
    config: EGLConfig,
    context: EGLContext,
    /// A 1×1 pbuffer to be current on with no window.
    pbuffer: EGLSurface,
    buffer_age: bool,
    partial_update: bool,
    /// Whether a window can be made in the sRGB colorspace.
    srgb_colorspace: bool,
}

/// The compiler's own context, current on one thread at a time.
struct CompileContext {
    libs: Arc<Libs>,
    display: EGLDisplay,
    context: EGLContext,
    pbuffer: EGLSurface,
}

// SAFETY: the EGL handles are opaque tokens EGL accepts from any thread; the
// context is only made current under the compiler's lock.
unsafe impl Send for CompileContext {}

/// The GLES3 backend.
pub struct Gles3Backend {
    libs: Arc<Libs>,
    wayland: Option<WaylandEgl>,
    display: Option<Display>,
    compile: Arc<Mutex<Option<CompileContext>>>,
    buffers: SlotMap<GlBuffer>,
    textures: SlotMap<GlTexture>,
    samplers: SlotMap<GLuint>,
    pipelines: SlotMap<GlPipeline>,
    bind_groups: SlotMap<Vec<Binding>>,
    surfaces: SlotMap<GlSurface>,
    caps: Caps,
    uniform_align: usize,
    /// The draw list's inline uniforms, packed; and their buffer.
    uniform_bytes: Vec<u8>,
    uniform_offsets: Vec<u32>,
    uniform_ring: GlBuffer,
    /// Program instance chunks, a [`INSTANCE_CHUNK`] each.
    instance_ring: GlBuffer,
    /// Red and blue swapped, for a `Bgra8Unorm` upload.
    swizzle: Vec<u8>,
    current: Option<EGLSurface>,
    epoch: u64,
}

impl Gles3Backend {
    /// Loads EGL and GLES; the device opens with the first surface.
    ///
    /// # Panics
    ///
    /// When `libEGL` or `libGLESv2` cannot be loaded.
    pub fn new() -> Self {
        Self::try_new().unwrap_or_else(|why| panic!("no usable GLES 3 driver: {why}"))
    }

    /// Loads EGL and GLES, or says why not.
    pub fn try_new() -> Result<Self, String> {
        let libs = Arc::new(Libs::load()?);
        Ok(Self {
            libs,
            wayland: None,
            display: None,
            compile: Arc::new(Mutex::new(None)),
            buffers: SlotMap::new(),
            textures: SlotMap::new(),
            samplers: SlotMap::new(),
            pipelines: SlotMap::new(),
            bind_groups: SlotMap::new(),
            surfaces: SlotMap::new(),
            caps: Caps {
                max_texture_size: 2048,
                presents_to_display: true,
                compute_dispatch: false,
                bindless_texture_slots: 0,
                indirect_draw: false,
                program_instance_align: 4,
                coverage_paths: false,
                encodes_packed: false,
                // Read from the context once it opens (`init_state`).
                sampled: SampledFormats::empty(),
            },
            uniform_align: 256,
            uniform_bytes: Vec::new(),
            uniform_offsets: Vec::new(),
            uniform_ring: GlBuffer { name: 0, size: 0 },
            instance_ring: GlBuffer { name: 0, size: 0 },
            swizzle: Vec::new(),
            current: None,
            epoch: 0,
        })
    }

    fn gl(&self) -> &Gl {
        &self.libs.gl
    }

    /// The display, opened on the default display for a backend asked for
    /// resources before it has a window.
    fn ensure_display(&mut self) {
        if self.display.is_none() {
            let egl = &self.libs.egl;
            // SAFETY: the default display needs no native handle.
            let display = unsafe { (egl.eglGetDisplay)(EGL_DEFAULT_DISPLAY) };
            self.open(display, None);
        }
    }

    /// Initializes `display`, picks a config (of native visual `visual`
    /// when the windows need one), and makes the contexts.
    fn open(&mut self, display: EGLDisplay, visual: Option<EGLint>) {
        let egl = &self.libs.egl;
        assert!(!display.is_null(), "EGL has no display for this window");
        let (mut major, mut minor) = (0, 0);
        // SAFETY: `display` is a display EGL returned; the out-pointers are
        // live locals.
        let ok = unsafe { (egl.eglInitialize)(display, &mut major, &mut minor) };
        assert_eq!(ok, EGL_TRUE, "eglInitialize failed: {:#x}", egl_error(egl));
        // SAFETY: binding the API is a per-thread switch with no pointers.
        unsafe { (egl.eglBindAPI)(EGL_OPENGL_ES_API) };
        let config = choose_config(egl, display, visual);
        // SAFETY: the string EGL returns for a display is NUL-terminated and
        // lives as long as the display.
        let extensions = unsafe { cstr((egl.eglQueryString)(display, EGL_EXTENSIONS)) };
        let has = |name: &str| extensions.split(' ').any(|e| e == name);
        let surfaceless = has("EGL_KHR_surfaceless_context");
        let attribs = [
            EGL_CONTEXT_MAJOR_VERSION,
            3,
            EGL_CONTEXT_MINOR_VERSION,
            0,
            EGL_NONE,
        ];
        // SAFETY: `config` belongs to `display`; the attribute list ends in
        // `EGL_NONE`.
        let context =
            unsafe { (egl.eglCreateContext)(display, config, NO_CONTEXT, attribs.as_ptr()) };
        assert!(
            !context.is_null(),
            "eglCreateContext failed: {:#x}",
            egl_error(egl)
        );
        // SAFETY: as above; the compiler's context shares `context`'s objects.
        let shared = unsafe { (egl.eglCreateContext)(display, config, context, attribs.as_ptr()) };
        // With no window current, a context binds no surface where EGL allows
        // it (GBM has no pbuffers), else a 1×1 pbuffer.
        let pbuffer_attribs = [EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE];
        let idle_surface = || {
            if surfaceless {
                NO_SURFACE
            } else {
                // SAFETY: a 1×1 pbuffer of `config`, attribute list terminated.
                unsafe { (egl.eglCreatePbufferSurface)(display, config, pbuffer_attribs.as_ptr()) }
            }
        };
        let pbuffer = idle_surface();
        let compile_pbuffer = idle_surface();
        self.display = Some(Display {
            display,
            config,
            context,
            pbuffer,
            buffer_age: has("EGL_EXT_buffer_age"),
            partial_update: has("EGL_KHR_partial_update"),
            srgb_colorspace: has("EGL_KHR_gl_colorspace"),
        });
        if !shared.is_null() {
            *self.compile.lock().expect("compile context lock") = Some(CompileContext {
                libs: Arc::clone(&self.libs),
                display,
                context: shared,
                pbuffer: compile_pbuffer,
            });
        }
        self.make_current(pbuffer);
        self.init_state();
    }

    /// Reads the limits and sets the state every pass assumes.
    fn init_state(&mut self) {
        let gl = &self.libs.gl;
        let mut max_texture = 0;
        let mut align = 0;
        // SAFETY: the context is current; each query writes one integer.
        unsafe {
            (gl.glGetIntegerv)(GL_MAX_TEXTURE_SIZE, &mut max_texture);
            (gl.glGetIntegerv)(GL_UNIFORM_BUFFER_OFFSET_ALIGNMENT, &mut align);
            (gl.glDisable)(GL_DITHER);
            (gl.glDisable)(GL_DEPTH_TEST);
            (gl.glDisable)(GL_CULL_FACE);
            (gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 1);
            (gl.glPixelStorei)(GL_PACK_ALIGNMENT, 1);
        }
        self.caps.max_texture_size = max_texture.max(2048) as u32;
        self.caps.sampled = sampled_formats(gl);
        self.uniform_align = (align.max(16) as usize).next_power_of_two();
        self.caps.program_instance_align = 4;
        self.uniform_ring = new_buffer(gl, 0);
        self.instance_ring = new_buffer(gl, 0);
    }

    fn make_current(&mut self, surface: EGLSurface) {
        if self.current == Some(surface) {
            return;
        }
        let d = self.display.as_ref().expect("the display is open");
        let egl = &self.libs.egl;
        // SAFETY: `surface` and the context belong to `d.display`; the
        // context is only current on this thread.
        let ok = unsafe { (egl.eglMakeCurrent)(d.display, surface, surface, d.context) };
        assert_eq!(ok, EGL_TRUE, "eglMakeCurrent failed: {:#x}", egl_error(egl));
        self.current = Some(surface);
    }

    /// Makes the backend's context current, opening it if need be.
    fn ctx(&mut self) {
        self.ensure_display();
        if self.current.is_none() {
            let pbuffer = self.display.as_ref().expect("opened").pbuffer;
            self.make_current(pbuffer);
        }
    }

    fn buffer(&self, id: BufferId) -> &GlBuffer {
        self.buffers
            .get(id.into())
            .expect("buffer handle does not resolve")
    }

    /// Packs every command's inline uniforms into the uniform ring.
    fn upload_uniforms(&mut self, commands: &[DrawCommand]) {
        self.uniform_bytes.clear();
        self.uniform_offsets.clear();
        let align = self.uniform_align;
        for c in commands {
            let bytes = c.uniforms.as_bytes();
            let at = self.uniform_bytes.len().next_multiple_of(align);
            self.uniform_bytes.resize(at, 0);
            self.uniform_bytes.extend_from_slice(bytes);
            // A block reads 16-byte rows; the range covers whole rows.
            self.uniform_bytes
                .resize(at + bytes.len().next_multiple_of(16).max(16), 0);
            self.uniform_offsets.push(at as u32);
        }
        let gl = &self.libs.gl;
        upload_ring(gl, &mut self.uniform_ring, &self.uniform_bytes);
    }

    /// The texture's framebuffer, with `depth` as its depth attachment if
    /// named, made on first use.
    fn texture_fbo(&mut self, id: TextureId, depth: Option<TextureId>) -> (GLuint, u32, u32) {
        let gl = &self.libs.gl;
        let depth_name = depth.map(|d| {
            self.textures
                .get(d.into())
                .expect("texture handle does not resolve")
                .name
        });
        let t = self
            .textures
            .get_mut(id.into())
            .expect("texture handle does not resolve");
        let make = |depth: Option<GLuint>| {
            let mut fbo = 0;
            // SAFETY: the context is current; `t.name` is a live texture of
            // a color-renderable format, `depth` one of a depth format.
            unsafe {
                (gl.glGenFramebuffers)(1, &mut fbo);
                (gl.glBindFramebuffer)(GL_FRAMEBUFFER, fbo);
                (gl.glFramebufferTexture2D)(
                    GL_FRAMEBUFFER,
                    GL_COLOR_ATTACHMENT0,
                    GL_TEXTURE_2D,
                    t.name,
                    0,
                );
                if let Some(depth) = depth {
                    (gl.glFramebufferTexture2D)(
                        GL_FRAMEBUFFER,
                        GL_DEPTH_ATTACHMENT,
                        GL_TEXTURE_2D,
                        depth,
                        0,
                    );
                }
                let status = (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER);
                assert_eq!(
                    status, GL_FRAMEBUFFER_COMPLETE,
                    "a {:?} texture is not renderable here",
                    t.format
                );
            }
            fbo
        };
        let fbo = match (depth, depth_name) {
            (Some(d), Some(name)) => match t.depth_fbo {
                Some((with, fbo)) if with == d => fbo,
                old => {
                    if let Some((_, fbo)) = old {
                        // SAFETY: GL orders the deletion after the commands
                        // already issued against it.
                        unsafe { (gl.glDeleteFramebuffers)(1, &fbo) };
                    }
                    let fbo = make(Some(name));
                    t.depth_fbo = Some((d, fbo));
                    fbo
                }
            },
            _ => *t.fbo.get_or_insert_with(|| make(None)),
        };
        (fbo, t.width, t.height)
    }

    /// Declares the region the frame's first surface pass draws: its
    /// scissors when it loads the old buffer, the whole surface when it
    /// clears.
    fn declare_damage(&mut self, surface: SurfaceId, pass: &RenderPass, commands: &[DrawCommand]) {
        let (Some(set_region), Some(d)) = (self.libs.set_damage_region, &self.display) else {
            return;
        };
        let Some(s) = self.surfaces.get_mut(surface.into()) else {
            return;
        };
        if !d.partial_update || std::mem::replace(&mut s.region_set, true) {
            return;
        }
        const MAX_RECTS: usize = 16;
        let mut rects = [0 as EGLint; 4 * MAX_RECTS];
        let mut n = 0;
        if pass.load == LoadOp::Load {
            for c in commands {
                let Some((x, y, w, h)) = c.scissor else {
                    n = 0;
                    break;
                };
                let (x, y, w, h) = turn_rect(
                    clip((x, y, w, h), [s.width, s.height]),
                    [s.width, s.height],
                    s.turns,
                );
                let gy = framebuffer([s.width, s.height], s.turns)[1].saturating_sub(y + h);
                let rect = [x as EGLint, gy as EGLint, w as EGLint, h as EGLint];
                if (0..n).any(|k| rects[k * 4..k * 4 + 4] == rect) {
                    continue;
                }
                if n == MAX_RECTS {
                    n = 0;
                    break;
                }
                rects[n * 4..n * 4 + 4].copy_from_slice(&rect);
                n += 1;
            }
        }
        // SAFETY: the surface is current and has not been drawn this frame;
        // `n` rects of four integers are live in `rects` (none: the whole
        // surface).
        unsafe { set_region(d.display, s.egl, rects.as_ptr(), n as EGLint) };
    }

    fn encode_pass(
        &mut self,
        pass: &RenderPass,
        commands: &[DrawCommand],
        first: usize,
        slot: &mut usize,
    ) {
        let (fbo, width, height, flip, turns) = match pass.target {
            RenderTarget::Surface(_) if pass.depth.is_some() => {
                panic!("a pass with a depth attachment draws into a texture")
            }
            RenderTarget::Surface(frame) => {
                let s = self
                    .surfaces
                    .get(frame.surface.into())
                    .expect("surface handle does not resolve");
                let (egl, w, h, turns) = (s.egl, s.width, s.height, s.turns);
                self.make_current(egl);
                self.declare_damage(frame.surface, pass, commands);
                (0, w, h, 1.0, turns)
            }
            RenderTarget::Texture(id) => {
                let (fbo, w, h) = self.texture_fbo(id, pass.depth.map(|d| d.texture));
                (fbo, w, h, -1.0, 0)
            }
        };
        let gl = &self.libs.gl;
        // SAFETY: the context is current; `fbo` is the default framebuffer
        // or a complete one made above.
        unsafe {
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, fbo);
            let [fw, fh] = framebuffer([width, height], turns);
            (gl.glViewport)(0, 0, fw as GLsizei, fh as GLsizei);
            (gl.glDisable)(GL_SCISSOR_TEST);
            if let LoadOp::Clear(c) = pass.load {
                (gl.glClearColor)(c[0], c[1], c[2], c[3]);
                (gl.glClear)(GL_COLOR_BUFFER_BIT);
            }
            if let Some(d) = pass.depth {
                // The clear honours the depth mask.
                (gl.glDepthMask)(GL_TRUE);
                (gl.glClearDepthf)(d.clear);
                (gl.glClear)(GL_DEPTH_BUFFER_BIT);
            }
            (gl.glEnable)(GL_SCISSOR_TEST);
        }
        let mut bound: Option<PipelineId> = None;
        for (k, c) in commands.iter().enumerate() {
            self.draw(c, first + k, [width, height], flip, turns, &mut bound, slot);
        }
        // SAFETY: the context is current and `fbo` bound; the depth is
        // transient, so its contents are invalidated rather than stored, and
        // the 2D passes after it run with no depth test or culling.
        unsafe {
            if pass.depth.is_some() {
                let attachment = [GL_DEPTH_ATTACHMENT];
                (gl.glInvalidateFramebuffer)(GL_FRAMEBUFFER, 1, attachment.as_ptr());
                (gl.glDisable)(GL_DEPTH_TEST);
                (gl.glDisable)(GL_CULL_FACE);
            }
            if fbo != 0 {
                (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            }
        }
    }

    /// Draws `c`, the list's command `index`; a program's instance chunks
    /// take the ring's slots from `slot` on.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        c: &DrawCommand,
        index: usize,
        [width, height]: [u32; 2],
        flip: f32,
        turns: u8,
        bound: &mut Option<PipelineId>,
        slot: &mut usize,
    ) {
        let gl = &self.libs.gl;
        let p = self
            .pipelines
            .get(c.pipeline.into())
            .expect("pipeline handle does not resolve");
        let image = [width, height];
        let scissor = c.scissor.map_or((0, 0, width, height), |r| clip(r, image));
        let (sx, sy, sw, sh) = turn_rect(scissor, image, turns);
        let [fw, fh] = framebuffer(image, turns);
        // A surface counts rows from the bottom; a texture target, drawn
        // flipped, from the top.
        let gy = if flip > 0.0 { fh - sy - sh } else { sy };
        let bindings: &[Binding] = c
            .bind_group
            .and_then(|bg| self.bind_groups.get(bg.into()))
            .map_or(&[], |b| &b[..]);
        // SAFETY: the context is current and every name below is a live
        // object of it: the pipeline's program and vertex array, the bind
        // group's textures and sampler, the command's buffers. The renderer
        // keeps instance, vertex and index ranges inside their buffers.
        unsafe {
            (gl.glScissor)(sx as GLint, gy as GLint, sw as GLsizei, sh as GLsizei);
            if *bound != Some(c.pipeline) {
                (gl.glUseProgram)(p.program);
                (gl.glBindVertexArray)(p.vao);
                match p.blend {
                    BlendMode::Replace => (gl.glDisable)(GL_BLEND),
                    BlendMode::PremultipliedOver => {
                        (gl.glEnable)(GL_BLEND);
                        (gl.glBlendFunc)(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
                    }
                }
                if p.target >= 0 {
                    (gl.glUniform4f)(p.target, flip, fh as f32, f32::from(turns), fw as f32);
                }
                match p.depth {
                    Some(d) => {
                        (gl.glEnable)(GL_DEPTH_TEST);
                        (gl.glDepthFunc)(depth_func(d.compare));
                        (gl.glDepthMask)(if d.write { GL_TRUE } else { GL_FALSE });
                    }
                    None => (gl.glDisable)(GL_DEPTH_TEST),
                }
                match p.cull {
                    CullMode::None => (gl.glDisable)(GL_CULL_FACE),
                    CullMode::Back => {
                        (gl.glEnable)(GL_CULL_FACE);
                        (gl.glCullFace)(GL_BACK);
                        // A texture target is drawn flipped, which turns
                        // clip space's counter-clockwise front faces around.
                        (gl.glFrontFace)(if flip > 0.0 { GL_CCW } else { GL_CW });
                    }
                }
                *bound = Some(c.pipeline);
            }
            // Textures to units in order; the group's last sampler on each.
            let sampler = bindings.iter().rev().find_map(|b| match b {
                Binding::Sampler(s) => self.samplers.get((*s).into()).copied(),
                _ => None,
            });
            let mut unit = 0;
            let mut samplers = bindings.iter().filter_map(|b| match b {
                Binding::Sampler(s) => self.samplers.get((*s).into()).copied(),
                _ => None,
            });
            let program = matches!(p.kind, Kind::Program { .. });
            let scene = matches!(&p.kind, Kind::Builtin { instance, .. } if !instance.is_empty());
            let mut scene_samplers = [None; scene::SAMPLER_SLOTS];
            if scene {
                for (slot, s) in scene_samplers
                    .iter_mut()
                    .zip(bindings.iter().filter_map(|b| match b {
                        Binding::Sampler(s) => Some(self.samplers.get((*s).into()).copied()),
                        _ => None,
                    }))
                {
                    *slot = s;
                }
            }
            for b in bindings {
                if let Binding::Texture(t) = b {
                    let name = self.textures.get((*t).into()).map_or(0, |t| t.name);
                    (gl.glActiveTexture)(GL_TEXTURE0 + unit);
                    (gl.glBindTexture)(GL_TEXTURE_2D, name);
                    // A program pairs texture `i` with sampler `i`; a scene
                    // program each texture slot with its own sampler slot.
                    let s = if program {
                        samplers.next().or(sampler)
                    } else if scene {
                        scene::TEXTURE_SAMPLERS
                            .get(unit as usize)
                            .and_then(|&j| scene_samplers[j as usize])
                            .or(sampler)
                    } else {
                        sampler
                    };
                    (gl.glBindSampler)(unit, s.unwrap_or(0));
                    unit += 1;
                }
            }
            let inline = c.uniforms.as_bytes();
            if !inline.is_empty() {
                let at = self.uniform_offsets[index] as GLintptr;
                let size = inline.len().next_multiple_of(16) as GLsizeiptr;
                (gl.glBindBufferRange)(GL_UNIFORM_BUFFER, 0, self.uniform_ring.name, at, size);
            }
            match &p.kind {
                Kind::Builtin {
                    attrs,
                    stride,
                    instance,
                } => {
                    let (buffer, base) = match c.geometry {
                        Geometry::Generated { .. } => {
                            (self.buffer(c.instance_buffer).name, c.instance_offset)
                        }
                        Geometry::IndexedMesh { vertex_buffer, .. }
                        | Geometry::InstancedMesh { vertex_buffer, .. } => {
                            (self.buffer(vertex_buffer).name, 0)
                        }
                    };
                    point_attrs(gl, buffer, base, attrs, *stride);
                    if !instance.is_empty() {
                        point_attrs(
                            gl,
                            self.buffer(c.instance_buffer).name,
                            c.instance_offset,
                            instance,
                            SceneInstance::SIZE,
                        );
                        // Uniform buffer `k` of the group at binding `k`.
                        let blocks = bindings.iter().filter_map(|b| match b {
                            Binding::Uniform(u) => Some(*u),
                            _ => None,
                        });
                        for (k, u) in blocks.take(scene::UNIFORM_SLOTS).enumerate() {
                            let u = self.buffer(u);
                            (gl.glBindBufferRange)(
                                GL_UNIFORM_BUFFER,
                                k as GLuint,
                                u.name,
                                0,
                                u.size as GLsizeiptr,
                            );
                        }
                    }
                    match c.geometry {
                        Geometry::Generated { count } => {
                            (gl.glDrawArraysInstanced)(GL_TRIANGLES, 0, 6, count as GLsizei);
                        }
                        Geometry::IndexedMesh {
                            index_buffer,
                            index_format,
                            index_offset,
                            index_count,
                            ..
                        } => {
                            let ty = match index_format {
                                IndexFormat::U16 => GL_UNSIGNED_SHORT,
                                IndexFormat::U32 => GL_UNSIGNED_INT,
                            };
                            (gl.glBindBuffer)(
                                GL_ELEMENT_ARRAY_BUFFER,
                                self.buffer(index_buffer).name,
                            );
                            let at = (index_offset as usize * index_format.size()) as *const c_void;
                            (gl.glDrawElements)(GL_TRIANGLES, index_count as GLsizei, ty, at);
                        }
                        Geometry::InstancedMesh {
                            index_buffer,
                            index_format,
                            index_offset,
                            index_count,
                            count,
                            ..
                        } => {
                            let ty = match index_format {
                                IndexFormat::U16 => GL_UNSIGNED_SHORT,
                                IndexFormat::U32 => GL_UNSIGNED_INT,
                            };
                            (gl.glBindBuffer)(
                                GL_ELEMENT_ARRAY_BUFFER,
                                self.buffer(index_buffer).name,
                            );
                            let at = (index_offset as usize * index_format.size()) as *const c_void;
                            (gl.glDrawElementsInstanced)(
                                GL_TRIANGLES,
                                index_count as GLsizei,
                                ty,
                                at,
                                count as GLsizei,
                            );
                        }
                    }
                }
                Kind::Program { abi, base_instance } => {
                    let Geometry::Generated { count } = c.geometry else {
                        panic!("a program draws generated quads");
                    };
                    if inline.is_empty()
                        && abi.uniform_size > 0
                        && let Some(Binding::Uniform(u)) =
                            bindings.iter().find(|b| matches!(b, Binding::Uniform(_)))
                    {
                        let u = self.buffer(*u);
                        let size = (abi.uniform_size as usize).next_multiple_of(16).min(u.size);
                        (gl.glBindBufferRange)(GL_UNIFORM_BUFFER, 0, u.name, 0, size as GLsizeiptr);
                    }
                    let stride = abi.instance_stride as usize;
                    if stride == 0 {
                        (gl.glDrawArraysInstanced)(GL_TRIANGLES, 0, 6, count as GLsizei);
                        return;
                    }
                    let per_chunk = (INSTANCE_CHUNK / stride).max(1);
                    let source = self.buffer(c.instance_buffer).name;
                    let mut start = 0usize;
                    while start < count as usize {
                        let n = per_chunk.min(count as usize - start);
                        let at = *slot * INSTANCE_CHUNK.next_multiple_of(self.uniform_align);
                        (gl.glBindBuffer)(GL_COPY_READ_BUFFER, source);
                        (gl.glBindBuffer)(GL_COPY_WRITE_BUFFER, self.instance_ring.name);
                        (gl.glCopyBufferSubData)(
                            GL_COPY_READ_BUFFER,
                            GL_COPY_WRITE_BUFFER,
                            (c.instance_offset + start * stride) as GLintptr,
                            at as GLintptr,
                            (n * stride) as GLsizeiptr,
                        );
                        (gl.glBindBufferRange)(
                            GL_UNIFORM_BUFFER,
                            1,
                            self.instance_ring.name,
                            at as GLintptr,
                            INSTANCE_CHUNK as GLsizeiptr,
                        );
                        if *base_instance >= 0 {
                            (gl.glUniform1ui)(*base_instance, start as GLuint);
                        }
                        (gl.glDrawArraysInstanced)(GL_TRIANGLES, 0, 6, n as GLsizei);
                        start += n;
                        *slot += 1;
                    }
                }
            }
        }
    }

    /// Room in the instance ring for every program chunk of `list`, each in
    /// a slot of its own so no copy waits on an earlier draw's read; the old
    /// storage is orphaned so none waits on the last frame's either.
    fn reserve_instance_chunks(&mut self, list: &DrawList<'_>) {
        let mut chunks = 0usize;
        for c in list.commands {
            let Some(GlPipeline {
                kind: Kind::Program { abi, .. },
                ..
            }) = self.pipelines.get(c.pipeline.into())
            else {
                continue;
            };
            let stride = abi.instance_stride as usize;
            if let (Geometry::Generated { count }, true) = (c.geometry, stride > 0) {
                chunks += (count as usize).div_ceil((INSTANCE_CHUNK / stride).max(1));
            }
        }
        if chunks == 0 {
            return;
        }
        let need = chunks * INSTANCE_CHUNK.next_multiple_of(self.uniform_align);
        self.instance_ring.size = self.instance_ring.size.max(need.next_power_of_two());
        let gl = &self.libs.gl;
        // SAFETY: the context is current; the ring is this context's buffer,
        // given fresh storage with no data.
        unsafe {
            (gl.glBindBuffer)(GL_UNIFORM_BUFFER, self.instance_ring.name);
            (gl.glBufferData)(
                GL_UNIFORM_BUFFER,
                self.instance_ring.size as GLsizeiptr,
                std::ptr::null(),
                GL_STREAM_DRAW,
            );
        }
    }
}

impl Default for Gles3Backend {
    fn default() -> Self {
        Self::new()
    }
}

/// The last EGL error.
fn egl_error(egl: &Egl) -> EGLint {
    // SAFETY: a thread-local query with no arguments.
    unsafe { (egl.eglGetError)() }
}

/// # Safety
///
/// `p` is null or a NUL-terminated string.
unsafe fn cstr(p: *const std::ffi::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: the caller's contract.
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

/// `r` cut to an image of `w`×`h`.
fn clip((x, y, rw, rh): (u32, u32, u32, u32), [w, h]: [u32; 2]) -> (u32, u32, u32, u32) {
    let x = x.min(w);
    let y = y.min(h);
    (x, y, rw.min(w - x), rh.min(h - y))
}

/// The framebuffer an image of `w`×`h` is drawn onto with `turns`.
fn framebuffer([w, h]: [u32; 2], turns: u8) -> [u32; 2] {
    if turns & 1 == 1 { [h, w] } else { [w, h] }
}

/// An image rect (top-left origin) where it lands on the framebuffer the
/// image is drawn onto turned counter-clockwise `turns` quarter turns
/// (top-left origin too).
fn turn_rect(
    (x, y, rw, rh): (u32, u32, u32, u32),
    [w, h]: [u32; 2],
    turns: u8,
) -> (u32, u32, u32, u32) {
    match turns & 3 {
        1 => (y, w - x - rw, rh, rw),
        2 => (w - x - rw, h - y - rh, rw, rh),
        3 => (h - y - rh, x, rh, rw),
        _ => (x, y, rw, rh),
    }
}

/// The DRM fourcc of a scanout format, a GBM config's native visual.
fn fourcc(format: ScanoutFormat) -> EGLint {
    let code = |s: &[u8; 4]| u32::from_le_bytes(*s) as EGLint;
    match format {
        ScanoutFormat::Xrgb8888 => code(b"XR24"),
        ScanoutFormat::Rgb565 => code(b"RG16"),
    }
}

/// An ES 3 config: for windows of native visual `visual` when given (a GBM
/// surface's format), else for windows and pbuffers — 8-bit RGBA when the
/// display has it, else RGB565 (a panel's own format).
fn choose_config(egl: &Egl, display: EGLDisplay, visual: Option<EGLint>) -> EGLConfig {
    if let Some(visual) = visual {
        let attribs = [
            EGL_RENDERABLE_TYPE,
            EGL_OPENGL_ES3_BIT,
            EGL_SURFACE_TYPE,
            EGL_WINDOW_BIT,
            EGL_NONE,
        ];
        let mut configs = [std::ptr::null_mut(); 64];
        let mut count = 0;
        // SAFETY: the attribute list ends in `EGL_NONE`; at most 64 configs
        // are written into the live array.
        unsafe {
            (egl.eglChooseConfig)(
                display,
                attribs.as_ptr(),
                configs.as_mut_ptr(),
                64,
                &mut count,
            )
        };
        let found = configs[..count.max(0) as usize].iter().copied().find(|&c| {
            let mut id = 0;
            // SAFETY: `c` is one of the display's configs; one integer is
            // written.
            unsafe { (egl.eglGetConfigAttrib)(display, c, EGL_NATIVE_VISUAL_ID, &mut id) };
            id == visual
        });
        return found.unwrap_or_else(|| panic!("EGL has no ES 3 config of visual {visual:#x}"));
    }
    for (r, g, b, a) in [(8, 8, 8, 8), (8, 8, 8, 0), (5, 6, 5, 0)] {
        let attribs = [
            EGL_RENDERABLE_TYPE,
            EGL_OPENGL_ES3_BIT,
            EGL_SURFACE_TYPE,
            EGL_WINDOW_BIT | EGL_PBUFFER_BIT,
            EGL_RED_SIZE,
            r,
            EGL_GREEN_SIZE,
            g,
            EGL_BLUE_SIZE,
            b,
            EGL_ALPHA_SIZE,
            a,
            EGL_NONE,
        ];
        let mut config = std::ptr::null_mut();
        let mut count = 0;
        // SAFETY: the attribute list ends in `EGL_NONE`; one config is
        // written at most.
        let ok =
            unsafe { (egl.eglChooseConfig)(display, attribs.as_ptr(), &mut config, 1, &mut count) };
        if ok == EGL_TRUE && count > 0 {
            return config;
        }
    }
    panic!("EGL offers no OpenGL ES 3 config: {:#x}", egl_error(egl));
}

fn config_format(egl: &Egl, display: EGLDisplay, config: EGLConfig) -> TextureFormat {
    let mut green = 0;
    let mut alpha = 0;
    // SAFETY: `config` belongs to `display`; each query writes one integer.
    unsafe {
        (egl.eglGetConfigAttrib)(display, config, EGL_GREEN_SIZE, &mut green);
        (egl.eglGetConfigAttrib)(display, config, EGL_ALPHA_SIZE, &mut alpha);
    }
    if green == 6 && alpha == 0 {
        TextureFormat::Rgb565Unorm
    } else {
        TextureFormat::Bgra8Unorm
    }
}

fn new_buffer(gl: &Gl, size: usize) -> GlBuffer {
    let mut name = 0;
    // SAFETY: the context is current; one buffer name is written.
    unsafe {
        (gl.glGenBuffers)(1, &mut name);
        if size > 0 {
            (gl.glBindBuffer)(GL_COPY_WRITE_BUFFER, name);
            (gl.glBufferData)(
                GL_COPY_WRITE_BUFFER,
                size as GLsizeiptr,
                std::ptr::null(),
                GL_DYNAMIC_DRAW,
            );
        }
    }
    GlBuffer { name, size }
}

/// Replaces `ring`'s contents with `bytes`, growing it when they do not fit
/// and orphaning the old storage either way, so a draw still reading the
/// last list's uniforms never waits.
fn upload_ring(gl: &Gl, ring: &mut GlBuffer, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    if bytes.len() > ring.size {
        ring.size = bytes.len().next_power_of_two();
    }
    // SAFETY: the context is current; `ring.name` is its buffer, sized to
    // hold `bytes`, which the call reads from a live slice.
    unsafe {
        (gl.glBindBuffer)(GL_UNIFORM_BUFFER, ring.name);
        (gl.glBufferData)(
            GL_UNIFORM_BUFFER,
            ring.size as GLsizeiptr,
            std::ptr::null(),
            GL_STREAM_DRAW,
        );
        (gl.glBufferSubData)(
            GL_UNIFORM_BUFFER,
            0,
            bytes.len() as GLsizeiptr,
            bytes.as_ptr().cast(),
        );
    }
}

/// A shader stage of `source`, or the driver's log.
fn compile_stage(gl: &Gl, stage: GLenum, source: &str, label: &str) -> Result<GLuint, String> {
    let vertex = stage == GL_VERTEX_SHADER;
    let text = CString::new(format!("{}{source}", stage_prelude(vertex)))
        .map_err(|_| format!("{label}: the source holds a NUL"))?;
    // SAFETY: the context is current; the source pointer is a live
    // NUL-terminated string for the call; the log buffer is as long as the
    // length the driver reports.
    unsafe {
        let shader = (gl.glCreateShader)(stage);
        let ptr = text.as_ptr();
        (gl.glShaderSource)(shader, 1, &ptr, std::ptr::null());
        (gl.glCompileShader)(shader);
        let mut ok = 0;
        (gl.glGetShaderiv)(shader, GL_COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut len = 0;
            (gl.glGetShaderiv)(shader, GL_INFO_LOG_LENGTH, &mut len);
            let mut log = vec![0u8; len.max(1) as usize];
            (gl.glGetShaderInfoLog)(shader, len, std::ptr::null_mut(), log.as_mut_ptr().cast());
            (gl.glDeleteShader)(shader);
            let which = if vertex { "vertex" } else { "fragment" };
            return Err(format!(
                "{label} ({which}): {}",
                String::from_utf8_lossy(&log).trim_end_matches('\0')
            ));
        }
        Ok(shader)
    }
}

/// A linked program of `source`'s two stages, its blocks and samplers bound
/// to their slots.
fn build_program(
    gl: &Gl,
    source: &str,
    label: &str,
    blocks: &[&str],
    units: &[&str],
) -> Result<GLuint, String> {
    let vs = compile_stage(gl, GL_VERTEX_SHADER, source, label)?;
    let fs = compile_stage(gl, GL_FRAGMENT_SHADER, source, label).inspect_err(|_| {
        // SAFETY: `vs` is a shader of the current context.
        unsafe { (gl.glDeleteShader)(vs) };
    })?;
    // SAFETY: the context is current; `vs` and `fs` are its shaders, deleted
    // once linked (the program keeps them); names are NUL-terminated.
    unsafe {
        let program = (gl.glCreateProgram)();
        (gl.glAttachShader)(program, vs);
        (gl.glAttachShader)(program, fs);
        (gl.glLinkProgram)(program);
        (gl.glDeleteShader)(vs);
        (gl.glDeleteShader)(fs);
        let mut ok = 0;
        (gl.glGetProgramiv)(program, GL_LINK_STATUS, &mut ok);
        if ok == 0 {
            let mut len = 0;
            (gl.glGetProgramiv)(program, GL_INFO_LOG_LENGTH, &mut len);
            let mut log = vec![0u8; len.max(1) as usize];
            (gl.glGetProgramInfoLog)(program, len, std::ptr::null_mut(), log.as_mut_ptr().cast());
            (gl.glDeleteProgram)(program);
            return Err(format!(
                "{label} (link): {}",
                String::from_utf8_lossy(&log).trim_end_matches('\0')
            ));
        }
        for (binding, name) in blocks.iter().enumerate() {
            let name = CString::new(*name).expect("a block name");
            let index = (gl.glGetUniformBlockIndex)(program, name.as_ptr());
            if index != GL_INVALID_INDEX {
                (gl.glUniformBlockBinding)(program, index, binding as GLuint);
            }
        }
        (gl.glUseProgram)(program);
        for (unit, name) in units.iter().enumerate() {
            let name = CString::new(*name).expect("a sampler name");
            let at = (gl.glGetUniformLocation)(program, name.as_ptr());
            if at >= 0 {
                (gl.glUniform1i)(at, unit as GLint);
            }
        }
        (gl.glUseProgram)(0);
        Ok(program)
    }
}

fn uniform_location(gl: &Gl, program: GLuint, name: &str) -> GLint {
    let name = CString::new(name).expect("a uniform name");
    // SAFETY: the context is current; `program` is linked; the name is
    // NUL-terminated.
    unsafe { (gl.glGetUniformLocation)(program, name.as_ptr()) }
}

/// The GL storage of `format`: internal format, upload format and type. A
/// block-compressed format uploads through `glCompressedTexSubImage2D`, which
/// takes the internal format alone, so its other two are 0.
fn texture_format(format: TextureFormat) -> (GLenum, GLenum, GLenum) {
    match format {
        TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Data => {
            (GL_RGBA8, GL_RGBA, GL_UNSIGNED_BYTE)
        }
        TextureFormat::R8Unorm => (GL_R8, GL_RED, GL_UNSIGNED_BYTE),
        // GL's packed types put the first component in the high bits, as Viso
        // stores them.
        TextureFormat::Rgb565Unorm => (GL_RGB565, GL_RGB, GL_UNSIGNED_SHORT_5_6_5),
        TextureFormat::Rgba4444Unorm => (GL_RGBA4, GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4),
        TextureFormat::Rgba5551Unorm => (GL_RGB5_A1, GL_RGBA, GL_UNSIGNED_SHORT_5_5_5_1),
        TextureFormat::Rgba16Float => (GL_RGBA16F, GL_RGBA, GL_HALF_FLOAT),
        TextureFormat::Depth32Float => (GL_DEPTH_COMPONENT32F, GL_DEPTH_COMPONENT, GL_FLOAT),
        TextureFormat::Bc1RgbaUnorm => (GL_COMPRESSED_RGBA_S3TC_DXT1_EXT, 0, 0),
        TextureFormat::Bc3RgbaUnorm => (GL_COMPRESSED_RGBA_S3TC_DXT5_EXT, 0, 0),
        TextureFormat::Bc7RgbaUnorm => (GL_COMPRESSED_RGBA_BPTC_UNORM_EXT, 0, 0),
        TextureFormat::Etc2Rgb8Unorm => (GL_COMPRESSED_RGB8_ETC2, 0, 0),
        TextureFormat::Etc2Rgb8A1Unorm => (GL_COMPRESSED_RGB8_PUNCHTHROUGH_ALPHA1_ETC2, 0, 0),
        TextureFormat::Etc2Rgba8Unorm => (GL_COMPRESSED_RGBA8_ETC2_EAC, 0, 0),
        TextureFormat::Astc4x4Unorm => (GL_COMPRESSED_RGBA_ASTC_4X4_KHR, 0, 0),
        TextureFormat::Astc6x6Unorm => (GL_COMPRESSED_RGBA_ASTC_6X6_KHR, 0, 0),
        TextureFormat::Astc8x8Unorm => (GL_COMPRESSED_RGBA_ASTC_8X8_KHR, 0, 0),
        TextureFormat::Bgra8UnormSrgb | TextureFormat::Rgba8UnormSrgb => {
            (GL_SRGB8_ALPHA8, GL_RGBA, GL_UNSIGNED_BYTE)
        }
        TextureFormat::Bc1RgbaUnormSrgb => (GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT1_EXT, 0, 0),
        TextureFormat::Bc3RgbaUnormSrgb => (GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT5_EXT, 0, 0),
        TextureFormat::Bc7RgbaUnormSrgb => (GL_COMPRESSED_SRGB_ALPHA_BPTC_UNORM_EXT, 0, 0),
        TextureFormat::Etc2Rgb8UnormSrgb => (GL_COMPRESSED_SRGB8_ETC2, 0, 0),
        TextureFormat::Etc2Rgb8A1UnormSrgb => (GL_COMPRESSED_SRGB8_PUNCHTHROUGH_ALPHA1_ETC2, 0, 0),
        TextureFormat::Etc2Rgba8UnormSrgb => (GL_COMPRESSED_SRGB8_ALPHA8_ETC2_EAC, 0, 0),
        TextureFormat::Astc4x4UnormSrgb => (GL_COMPRESSED_SRGB8_ALPHA8_ASTC_4X4_KHR, 0, 0),
        TextureFormat::Astc6x6UnormSrgb => (GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6X6_KHR, 0, 0),
        TextureFormat::Astc8x8UnormSrgb => (GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8X8_KHR, 0, 0),
    }
}

/// The formats beyond the baseline the current context samples: ETC2 is core
/// in OpenGL ES 3.0; ASTC and BC come with their extensions (BC needs S3TC
/// and its sRGB variants for BC1/BC3 and BPTC for BC7). None of the packed
/// 16-bit ones: they have no sRGB variant to decode a sample.
fn sampled_formats(gl: &Gl) -> SampledFormats {
    let mut count = 0;
    // SAFETY: the context is current; the query writes one integer.
    unsafe { (gl.glGetIntegerv)(GL_NUM_EXTENSIONS, &mut count) };
    let names: Vec<String> = (0..count.max(0) as GLuint)
        .filter_map(|i| {
            // SAFETY: the context is current and `i` is below the extension
            // count; GL returns a NUL-terminated string it owns, or null.
            let name = unsafe { (gl.glGetStringi)(GL_EXTENSIONS, i) };
            (!name.is_null()).then(|| {
                // SAFETY: non-null, NUL-terminated, alive for the context.
                unsafe { CStr::from_ptr(name.cast()) }
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect();
    let has = |name: &str| names.iter().any(|n| n == name);
    let mut sampled = SampledFormats::ETC2;
    if has("GL_KHR_texture_compression_astc_ldr") || has("GL_OES_texture_compression_astc") {
        sampled |= SampledFormats::ASTC;
    }
    if has("GL_EXT_texture_compression_s3tc")
        && has("GL_EXT_texture_compression_s3tc_srgb")
        && has("GL_EXT_texture_compression_bptc")
    {
        sampled |= SampledFormats::BC;
    }
    sampled
}

/// An attribute's lane count, GL type and whether it is integer.
fn attr_format(format: AttrFormat) -> (GLint, GLenum, bool) {
    match format {
        AttrFormat::Float1 => (1, GL_FLOAT, false),
        AttrFormat::Float2 => (2, GL_FLOAT, false),
        AttrFormat::Float3 => (3, GL_FLOAT, false),
        AttrFormat::Float4 => (4, GL_FLOAT, false),
        AttrFormat::Uint1 => (1, GL_UNSIGNED_INT, true),
        AttrFormat::Uint2 => (2, GL_UNSIGNED_INT, true),
        AttrFormat::Uint4 => (4, GL_UNSIGNED_INT, true),
    }
}

/// Whether `builtin` reads a vertex buffer rather than instances.
fn per_vertex(builtin: BuiltinShader) -> bool {
    matches!(
        builtin,
        BuiltinShader::Path | BuiltinShader::Mesh | BuiltinShader::Scene
    )
}

fn depth_func(f: CompareFunction) -> GLenum {
    match f {
        CompareFunction::Always => GL_ALWAYS,
        CompareFunction::Less => GL_LESS,
        CompareFunction::LessEqual => GL_LEQUAL,
        CompareFunction::Greater => GL_GREATER,
        CompareFunction::GreaterEqual => GL_GEQUAL,
    }
}

/// Points `attrs` at `buffer` from byte `base` on, `stride` apart.
///
/// # Safety
///
/// The context is current, `buffer` is a live buffer and the pipeline's
/// vertex array is bound.
unsafe fn point_attrs(gl: &Gl, buffer: GLuint, base: usize, attrs: &[Attr], stride: usize) {
    // SAFETY: as the caller vouches; the pointers are offsets into `buffer`.
    unsafe {
        (gl.glBindBuffer)(GL_ARRAY_BUFFER, buffer);
        for a in attrs {
            let at = (base + a.offset) as *const c_void;
            if a.integer {
                (gl.glVertexAttribIPointer)(a.location, a.lanes, a.ty, stride as GLsizei, at);
            } else {
                (gl.glVertexAttribPointer)(
                    a.location,
                    a.lanes,
                    a.ty,
                    GL_FALSE,
                    stride as GLsizei,
                    at,
                );
            }
        }
    }
}

impl GpuBackend for Gles3Backend {
    const SHADER_LANG: ShaderLang = ShaderLang::Glsl;

    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId {
        self.ctx();
        // Whole 16-byte rows, so a uniform block bound over it never reads
        // past its end.
        let buffer = new_buffer(self.gl(), desc.size.next_multiple_of(16).max(16));
        self.buffers.insert(buffer).into()
    }

    fn create_texture(&mut self, desc: &TextureDesc) -> TextureId {
        self.ctx();
        let gl = self.gl();
        let (internal, _, _) = texture_format(desc.format);
        let mut name = 0;
        // SAFETY: the context is current; one immutable-storage level of a
        // size the renderer keeps within `max_texture_size`.
        unsafe {
            (gl.glGenTextures)(1, &mut name);
            (gl.glBindTexture)(GL_TEXTURE_2D, name);
            (gl.glTexStorage2D)(
                GL_TEXTURE_2D,
                1,
                internal,
                desc.width.max(1) as GLsizei,
                desc.height.max(1) as GLsizei,
            );
        }
        self.textures
            .insert(GlTexture {
                name,
                width: desc.width,
                height: desc.height,
                format: desc.format,
                fbo: None,
                depth_fbo: None,
            })
            .into()
    }

    fn create_texture_mipped(&mut self, desc: &TextureDesc, levels: &[&[u8]]) -> TextureId {
        if desc.format.is_compressed() || desc.format.linear() == TextureFormat::Bgra8Unorm {
            let texture = self.create_texture(desc);
            if let Some(base) = levels.first() {
                self.write_texture(texture, 0, 0, desc.width, desc.height, base);
            }
            return texture;
        }
        self.ctx();
        let gl = self.gl();
        let (internal, upload, ty) = texture_format(desc.format);
        let (width, height) = (desc.width.max(1), desc.height.max(1));
        let mut name = 0;
        // SAFETY: the context is current; immutable storage of the chain's
        // levels, each written whole from bytes that hold it.
        unsafe {
            (gl.glGenTextures)(1, &mut name);
            (gl.glBindTexture)(GL_TEXTURE_2D, name);
            (gl.glTexStorage2D)(
                GL_TEXTURE_2D,
                levels.len().max(1) as GLsizei,
                internal,
                width as GLsizei,
                height as GLsizei,
            );
            let (mut w, mut h) = (width, height);
            for (k, bytes) in levels.iter().enumerate() {
                if k > 0 {
                    (w, h) = ((w / 2).max(1), (h / 2).max(1));
                }
                if bytes.len() < desc.format.data_size(w, h) {
                    break;
                }
                (gl.glTexSubImage2D)(
                    GL_TEXTURE_2D,
                    k as GLint,
                    0,
                    0,
                    w as GLsizei,
                    h as GLsizei,
                    upload,
                    ty,
                    bytes.as_ptr().cast(),
                );
            }
        }
        self.textures
            .insert(GlTexture {
                name,
                width: desc.width,
                height: desc.height,
                format: desc.format,
                fbo: None,
                depth_fbo: None,
            })
            .into()
    }

    fn create_sampler(&mut self, desc: &SamplerDesc) -> SamplerId {
        self.ctx();
        let gl = self.gl();
        let (min, mag) = match desc.filter {
            FilterMode::Nearest => (GL_NEAREST, GL_NEAREST),
            FilterMode::Linear => (GL_LINEAR, GL_LINEAR),
            // A texture of one immutable level is complete with it alone.
            FilterMode::MipmapLinear => (GL_LINEAR_MIPMAP_LINEAR, GL_LINEAR),
        };
        let wrap = match desc.address {
            AddressMode::ClampToEdge => GL_CLAMP_TO_EDGE,
            AddressMode::Repeat => GL_REPEAT,
            AddressMode::Mirror => GL_MIRRORED_REPEAT,
        };
        let mut name = 0;
        // SAFETY: the context is current; one sampler name is written and
        // configured.
        unsafe {
            (gl.glGenSamplers)(1, &mut name);
            (gl.glSamplerParameteri)(name, GL_TEXTURE_MIN_FILTER, min);
            (gl.glSamplerParameteri)(name, GL_TEXTURE_MAG_FILTER, mag);
            (gl.glSamplerParameteri)(name, GL_TEXTURE_WRAP_S, wrap);
            (gl.glSamplerParameteri)(name, GL_TEXTURE_WRAP_T, wrap);
        }
        self.samplers.insert(name).into()
    }

    fn create_pipeline(
        &mut self,
        desc: &PipelineDesc,
        layout: &InstanceLayout,
    ) -> Result<PipelineId, LayoutError> {
        layout.validate_against(&desc.instance_schema)?;
        self.ctx();
        let gl = self.gl();
        let ShaderCode::Glsl(source) = desc.code else {
            panic!(
                "{}: the GLES backend runs GLSL, not {:?}",
                desc.label, desc.code
            );
        };
        // A scene program's blocks and textures take their slots' bindings
        // and units.
        const SCENE_BLOCKS: [&str; scene::UNIFORM_SLOTS] = ["ViewBlock", "viso_b1", "viso_b2"];
        const SCENE_TEXTURES: [&str; scene::TEXTURE_SLOTS] = [
            "viso_t0", "viso_t1", "viso_t2", "viso_t3", "viso_t4", "viso_t5", "viso_t6", "viso_t7",
        ];
        let (blocks, textures): (&[&str], &[&str]) = if desc.builtin == BuiltinShader::Scene {
            (&SCENE_BLOCKS, &SCENE_TEXTURES)
        } else {
            (&["ViewBlock"], &["tex", "dst_tex"])
        };
        let program = build_program(gl, source, desc.label, blocks, textures)
            .unwrap_or_else(|e| panic!("a built-in shader does not build: {e}"));
        let divisor = u32::from(!per_vertex(desc.builtin));
        let attrs: Vec<Attr> = desc
            .instance_schema
            .attributes
            .iter()
            .enumerate()
            .map(|(location, attr)| {
                let field = layout
                    .fields
                    .iter()
                    .find(|f| f.name == attr.name)
                    .expect("validated against the schema");
                let (lanes, ty, integer) = attr_format(field.format);
                Attr {
                    location: location as GLuint,
                    lanes,
                    ty,
                    integer,
                    offset: field.offset,
                }
            })
            .collect();
        // A scene draw reads its instances at the locations after the vertex
        // attributes, one per instance.
        let mut instance = Vec::new();
        if desc.builtin == BuiltinShader::Scene {
            let mut offset = 0;
            for a in scene::INSTANCE_SCHEMA.attributes {
                let (lanes, ty, integer) = attr_format(a.format);
                instance.push(Attr {
                    location: (attrs.len() + instance.len()) as GLuint,
                    lanes,
                    ty,
                    integer,
                    offset,
                });
                offset += a.format.size();
            }
        }
        let mut vao = 0;
        // SAFETY: the context is current; the vertex array records which
        // attributes are enabled and their step, the pointers come per draw.
        unsafe {
            (gl.glGenVertexArrays)(1, &mut vao);
            (gl.glBindVertexArray)(vao);
            for a in &attrs {
                (gl.glEnableVertexAttribArray)(a.location);
                (gl.glVertexAttribDivisor)(a.location, divisor);
            }
            for a in &instance {
                (gl.glEnableVertexAttribArray)(a.location);
                (gl.glVertexAttribDivisor)(a.location, 1);
            }
            (gl.glBindVertexArray)(0);
        }
        let target = uniform_location(gl, program, "viso_target");
        Ok(self
            .pipelines
            .insert(GlPipeline {
                program,
                vao,
                blend: desc.blend,
                depth: desc.depth,
                cull: desc.cull,
                target,
                kind: Kind::Builtin {
                    attrs,
                    stride: layout.stride,
                    instance,
                },
            })
            .into())
    }

    type Compiler = Gles3Compiler;

    fn program_compiler(&self) -> Gles3Compiler {
        Gles3Compiler {
            context: Arc::clone(&self.compile),
        }
    }

    fn install_program(&mut self, program: Gles3Program) -> PipelineId {
        self.ctx();
        let gl = self.gl();
        let mut vao = 0;
        // SAFETY: the context is current; an empty vertex array (vertex
        // arrays are not shared with the compiler's context).
        unsafe { (gl.glGenVertexArrays)(1, &mut vao) };
        let target = uniform_location(gl, program.program, "viso_target");
        let base_instance = uniform_location(gl, program.program, "viso_base_instance");
        let (blend, abi) = (program.blend, program.abi);
        self.pipelines
            .insert(GlPipeline {
                program: program.take(),
                vao,
                blend,
                depth: None,
                cull: CullMode::None,
                target,
                kind: Kind::Program { abi, base_instance },
            })
            .into()
    }

    fn create_bind_group(&mut self, desc: &BindGroupDesc) -> BindGroupId {
        self.bind_groups.insert(desc.bindings.clone()).into()
    }

    fn destroy_buffer(&mut self, id: BufferId) {
        if let Some(b) = self.buffers.remove(id.into()) {
            // SAFETY: GL keeps a buffer a queued draw reads alive past its
            // deletion; the name is this context's.
            unsafe { (self.libs.gl.glDeleteBuffers)(1, &b.name) };
        }
    }

    fn destroy_texture(&mut self, id: TextureId) {
        if let Some(t) = self.textures.remove(id.into()) {
            // SAFETY: as for buffers; the framebuffer is deleted with it.
            unsafe {
                (self.libs.gl.glDeleteTextures)(1, &t.name);
                for fbo in t.fbo.into_iter().chain(t.depth_fbo.map(|(_, fbo)| fbo)) {
                    (self.libs.gl.glDeleteFramebuffers)(1, &fbo);
                }
            }
        }
    }

    fn destroy_sampler(&mut self, id: SamplerId) {
        if let Some(s) = self.samplers.remove(id.into()) {
            // SAFETY: as for buffers.
            unsafe { (self.libs.gl.glDeleteSamplers)(1, &s) };
        }
    }

    fn destroy_pipeline(&mut self, id: PipelineId) {
        if let Some(p) = self.pipelines.remove(id.into()) {
            // SAFETY: as for buffers.
            unsafe {
                (self.libs.gl.glDeleteProgram)(p.program);
                (self.libs.gl.glDeleteVertexArrays)(1, &p.vao);
            }
        }
    }

    fn destroy_bind_group(&mut self, id: BindGroupId) {
        self.bind_groups.remove(id.into());
    }

    fn write_buffer(&mut self, id: BufferId, offset: usize, bytes: &[u8]) {
        self.ctx();
        let b = self.buffer(id);
        assert!(
            offset + bytes.len() <= b.size,
            "a write past the buffer's end"
        );
        // SAFETY: the context is current; the range lies inside the buffer
        // and `bytes` is live for the call.
        unsafe {
            (self.libs.gl.glBindBuffer)(GL_COPY_WRITE_BUFFER, b.name);
            (self.libs.gl.glBufferSubData)(
                GL_COPY_WRITE_BUFFER,
                offset as GLintptr,
                bytes.len() as GLsizeiptr,
                bytes.as_ptr().cast(),
            );
        }
    }

    fn write_texture(&mut self, id: TextureId, x: u32, y: u32, w: u32, h: u32, bytes: &[u8]) {
        self.ctx();
        let t = self
            .textures
            .get(id.into())
            .expect("texture handle does not resolve");
        let (name, format) = (t.name, t.format);
        let data = if format.linear() == TextureFormat::Bgra8Unorm {
            self.swizzle.clear();
            self.swizzle.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[2], p[1], p[0], p[3]]),
            );
            &self.swizzle[..]
        } else {
            bytes
        };
        let (internal, upload, ty) = texture_format(format);
        let gl = &self.libs.gl;
        if format.is_compressed() {
            let size = format.data_size(w, h);
            assert!(
                data.len() >= size,
                "write_texture: {} bytes < a {w}x{h} region of {format:?}",
                data.len()
            );
            // SAFETY: the context is current; the block-aligned region lies
            // inside the texture and `data` holds its `size` bytes of blocks.
            unsafe {
                (gl.glBindTexture)(GL_TEXTURE_2D, name);
                (gl.glCompressedTexSubImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    x as GLint,
                    y as GLint,
                    w as GLsizei,
                    h as GLsizei,
                    internal,
                    size as GLsizei,
                    data.as_ptr().cast(),
                );
            }
            return;
        }
        // SAFETY: the context is current; the region lies inside the
        // texture and `data` holds `w × h` texels of its format.
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, name);
            (gl.glTexSubImage2D)(
                GL_TEXTURE_2D,
                0,
                x as GLint,
                y as GLint,
                w as GLsizei,
                h as GLsizei,
                upload,
                ty,
                data.as_ptr().cast(),
            );
        }
    }

    fn create_surface(&mut self, raw: RawWindowHandle, width: u32, height: u32) -> SurfaceId {
        let egl = &self.libs.egl;
        if self.display.is_none() {
            // SAFETY: each native handle is the live connection the platform
            // layer created the window on; the attribute lists end in
            // `EGL_NONE`.
            let display = unsafe {
                match raw {
                    RawWindowHandle::Wayland { display, .. } => (egl.eglGetPlatformDisplay)(
                        EGL_PLATFORM_WAYLAND_KHR,
                        display,
                        std::ptr::null(),
                    ),
                    RawWindowHandle::Xcb { connection, .. } => {
                        let attribs = [EGL_PLATFORM_XCB_SCREEN_EXT, 0, EGL_NONE as EGLAttrib];
                        (egl.eglGetPlatformDisplay)(
                            EGL_PLATFORM_XCB_EXT,
                            connection,
                            attribs.as_ptr(),
                        )
                    }
                    RawWindowHandle::Scanout { scanout } => {
                        let gbm = (*scanout)
                            .gbm()
                            .expect("a GPU draws a scanout through its GBM surface");
                        (egl.eglGetPlatformDisplay)(
                            EGL_PLATFORM_GBM_KHR,
                            gbm.device,
                            std::ptr::null(),
                        )
                    }
                    _ => (egl.eglGetDisplay)(EGL_DEFAULT_DISPLAY),
                }
            };
            let visual = match raw {
                // SAFETY: the platform keeps the scanout alive with its window.
                RawWindowHandle::Scanout { scanout } => {
                    Some(fourcc(unsafe { (*scanout).format() }))
                }
                _ => None,
            };
            self.open(display, visual);
        }
        let d = self.display.as_ref().expect("opened");
        let egl = &self.libs.egl;
        let (w, h) = (width.max(1) as i32, height.max(1) as i32);
        let (native, window): (Native, *mut c_void) = match raw {
            RawWindowHandle::Wayland { surface, .. } => {
                let wayland = self.wayland.get_or_insert_with(|| {
                    WaylandEgl::load().unwrap_or_else(|e| panic!("no wayland-egl: {e}"))
                });
                // SAFETY: `surface` is the toplevel's live `wl_surface`.
                let window = unsafe { (wayland.create)(surface, w, h) };
                (Native::Wayland(window), window)
            }
            RawWindowHandle::Xcb { window, .. } => {
                // EGL's XCB platform takes a pointer to the window id.
                let mut id = Box::new(window);
                let ptr: *mut u32 = &mut *id;
                (Native::Xcb { _id: id }, ptr.cast())
            }
            RawWindowHandle::AndroidNdk { a_native_window } => (Native::Android, a_native_window),
            RawWindowHandle::Scanout { scanout } => {
                // SAFETY: the platform keeps the scanout alive with its window.
                let gbm = unsafe { (*scanout).gbm() }.expect("checked when the display opened");
                (Native::Scanout(scanout), gbm.surface)
            }
            _ => (Native::Pbuffer, std::ptr::null_mut()),
        };
        let linear = match native {
            // SAFETY: the platform keeps the scanout alive with its window.
            Native::Scanout(scanout) => match unsafe { (*scanout).format() } {
                ScanoutFormat::Xrgb8888 => TextureFormat::Bgra8Unorm,
                ScanoutFormat::Rgb565 => TextureFormat::Rgb565Unorm,
            },
            _ => config_format(egl, d.display, d.config),
        };
        // An 8-bit window encodes in the sRGB colorspace where EGL offers it.
        let format = match linear.srgb() {
            Some(srgb) if d.srgb_colorspace => srgb,
            _ => linear,
        };
        let colorspace = if format.is_srgb() {
            [EGL_GL_COLORSPACE_KHR, EGL_GL_COLORSPACE_SRGB_KHR]
        } else {
            [EGL_NONE, EGL_NONE]
        };
        let egl_surface = if matches!(native, Native::Pbuffer) {
            let attribs = [
                EGL_WIDTH,
                w,
                EGL_HEIGHT,
                h,
                colorspace[0],
                colorspace[1],
                EGL_NONE,
            ];
            // SAFETY: a pbuffer of the display's config; attributes
            // terminated.
            unsafe { (egl.eglCreatePbufferSurface)(d.display, d.config, attribs.as_ptr()) }
        } else {
            let attribs = [
                colorspace[0] as EGLAttrib,
                colorspace[1] as EGLAttrib,
                EGL_NONE as EGLAttrib,
            ];
            // SAFETY: `window` is the platform's native window for this
            // display (a `wl_egl_window`, an `xcb_window_t*`, an
            // `ANativeWindow`, a `gbm_surface`), alive until the surface is
            // destroyed; the attribute list is terminated.
            unsafe {
                (egl.eglCreatePlatformWindowSurface)(d.display, d.config, window, attribs.as_ptr())
            }
        };
        assert!(
            !egl_surface.is_null(),
            "eglCreate*Surface failed: {:#x}",
            egl_error(egl)
        );
        let turns = match native {
            // SAFETY: the platform keeps the scanout alive with its window.
            Native::Scanout(scanout) => (unsafe { (*scanout).turns() }) & 3,
            _ => 0,
        };
        let display = d.display;
        self.make_current(egl_surface);
        // SAFETY: presenting on vblank; the surface is current.
        unsafe { (self.libs.egl.eglSwapInterval)(display, 1) };
        self.surfaces
            .insert(GlSurface {
                egl: egl_surface,
                native,
                width,
                height,
                format,
                region_set: false,
                turns,
            })
            .into()
    }

    fn resize_surface(&mut self, id: SurfaceId, width: u32, height: u32) {
        let s = self
            .surfaces
            .get_mut(id.into())
            .expect("surface handle does not resolve");
        s.width = width;
        s.height = height;
        if let (Native::Wayland(window), Some(wayland)) = (&s.native, &self.wayland) {
            // SAFETY: `window` is the live `wl_egl_window` this surface was
            // made on.
            unsafe { (wayland.resize)(*window, width.max(1) as i32, height.max(1) as i32, 0, 0) };
        }
    }

    fn destroy_surface(&mut self, id: SurfaceId) {
        let Some(s) = self.surfaces.remove(id.into()) else {
            return;
        };
        let d = self
            .display
            .as_ref()
            .expect("a surface implies the display");
        let egl = &self.libs.egl;
        if self.current == Some(s.egl) {
            // SAFETY: back to the pbuffer before the window surface goes.
            unsafe { (egl.eglMakeCurrent)(d.display, d.pbuffer, d.pbuffer, d.context) };
            self.current = Some(d.pbuffer);
        }
        // SAFETY: the surface is no longer current; the native window
        // outlived it and goes after it.
        unsafe {
            (egl.eglDestroySurface)(d.display, s.egl);
            if let (Native::Wayland(window), Some(wayland)) = (&s.native, &self.wayland) {
                (wayland.destroy)(*window);
            }
        }
    }

    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame> {
        let s = self.surfaces.get_mut(surface.into())?;
        s.region_set = false;
        let egl = s.egl;
        self.make_current(egl);
        self.epoch += 1;
        Some(Frame {
            surface,
            drawable: 0,
        })
    }

    fn encode(&mut self, list: &DrawList<'_>) {
        self.ctx();
        self.upload_uniforms(list.commands);
        self.reserve_instance_chunks(list);
        let mut slot = 0;
        for pass in list.passes {
            let range = pass.command_range();
            let first = range.start;
            self.encode_pass(pass, &list.commands[range], first, &mut slot);
        }
        // SAFETY: unbinding the program and vertex array, with the context
        // current.
        unsafe {
            (self.libs.gl.glUseProgram)(0);
            (self.libs.gl.glBindVertexArray)(0);
        }
    }

    fn present(&mut self, frame: Frame) {
        self.present_with_damage(frame, &[]);
    }

    fn present_with_damage(&mut self, frame: Frame, damage: &[(u32, u32, u32, u32)]) {
        let Some(egl_surface) = self.surfaces.get(frame.surface.into()).map(|s| s.egl) else {
            return;
        };
        self.make_current(egl_surface);
        let s = self
            .surfaces
            .get(frame.surface.into())
            .expect("resolved above");
        let d = self
            .display
            .as_ref()
            .expect("a surface implies the display");
        let egl = &self.libs.egl;
        let ok = match self.libs.swap_with_damage {
            Some(swap) if !damage.is_empty() => {
                // EGL counts damage rows from the bottom.
                let mut rects = [0 as EGLint; 4 * 8];
                let n = damage.len().min(8);
                for (k, &(x, y, w, h)) in damage[..n].iter().enumerate() {
                    let (x, y, w, h) = turn_rect(
                        clip((x, y, w, h), [s.width, s.height]),
                        [s.width, s.height],
                        s.turns,
                    );
                    let gy = framebuffer([s.width, s.height], s.turns)[1].saturating_sub(y + h);
                    rects[k * 4..k * 4 + 4].copy_from_slice(&[
                        x as EGLint,
                        gy as EGLint,
                        w as EGLint,
                        h as EGLint,
                    ]);
                }
                let n = if damage.len() > 8 { 0 } else { n };
                // SAFETY: `n` rects of four integers each are live in `rects`.
                unsafe { swap(d.display, s.egl, rects.as_ptr(), n as EGLint) }
            }
            // SAFETY: the surface belongs to the display and is current.
            _ => unsafe { (egl.eglSwapBuffers)(d.display, s.egl) },
        };
        if ok != EGL_TRUE && egl_error(egl) == EGL_CONTEXT_LOST {
            panic!("the GLES context was lost");
        }
        if let (true, Native::Scanout(scanout)) = (ok == EGL_TRUE, &s.native) {
            // SAFETY: the platform keeps the scanout alive with its window;
            // the swap just posted the frame to its GBM surface.
            unsafe { (**scanout).presented(damage) };
        }
    }

    fn frame_age(&self, frame: Frame) -> u32 {
        let (Some(s), Some(d)) = (self.surfaces.get(frame.surface.into()), &self.display) else {
            return 0;
        };
        if !d.buffer_age {
            return 0;
        }
        let mut age = 0;
        // SAFETY: the surface belongs to the display; one integer is written.
        unsafe { (self.libs.egl.eglQuerySurface)(d.display, s.egl, EGL_BUFFER_AGE_EXT, &mut age) };
        age.max(0) as u32
    }

    fn device_lost(&mut self, surface: SurfaceId) {
        // The swap chain is the driver's; nothing is held between frames.
        let _ = surface;
    }

    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn frame_epoch(&self) -> u64 {
        self.epoch
    }

    fn surface_format(&self, surface: SurfaceId) -> TextureFormat {
        self.surfaces
            .get(surface.into())
            .expect("surface handle does not resolve")
            .format
    }
}

/// The GLES program compiler: builds on a context sharing the backend's
/// objects, so it runs on any thread.
#[derive(Clone)]
pub struct Gles3Compiler {
    context: Arc<Mutex<Option<CompileContext>>>,
}

/// A linked user program, deleted if it is dropped uninstalled.
pub struct Gles3Program {
    program: GLuint,
    blend: BlendMode,
    abi: ProgramAbi,
    owner: Option<Arc<Mutex<Option<CompileContext>>>>,
}

impl Gles3Program {
    /// The program, now the backend's to delete.
    fn take(mut self) -> GLuint {
        self.owner = None;
        self.program
    }
}

impl Drop for Gles3Program {
    fn drop(&mut self) {
        let Some(owner) = self.owner.take() else {
            return;
        };
        let guard = owner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.as_ref() {
            c.with(|gl| {
                // SAFETY: the program is a name of the shared namespace,
                // never drawn with.
                unsafe { (gl.glDeleteProgram)(self.program) }
            });
        }
    }
}

impl CompileContext {
    /// Runs `f` with this context current on the calling thread, then puts
    /// back whatever was current there (the backend's own context, when the
    /// compiler runs on the render thread).
    fn with<R>(&self, f: impl FnOnce(&Gl) -> R) -> R {
        let egl = &self.libs.egl;
        // SAFETY: thread-local queries with no arguments.
        let (display, context, draw, read) = unsafe {
            (
                (egl.eglGetCurrentDisplay)(),
                (egl.eglGetCurrentContext)(),
                (egl.eglGetCurrentSurface)(EGL_DRAW),
                (egl.eglGetCurrentSurface)(EGL_READ),
            )
        };
        // SAFETY: the context is only made current under the compiler's lock,
        // so no other thread holds it; it is released before returning.
        unsafe { (egl.eglMakeCurrent)(self.display, self.pbuffer, self.pbuffer, self.context) };
        let out = f(&self.libs.gl);
        // SAFETY: the program is finished before another context uses it; the
        // binding put back is the one this thread had, still alive.
        unsafe {
            (self.libs.gl.glFinish)();
            if context.is_null() {
                (egl.eglMakeCurrent)(self.display, NO_SURFACE, NO_SURFACE, NO_CONTEXT);
            } else {
                (egl.eglMakeCurrent)(display, draw, read, context);
            }
        }
        out
    }
}

impl ProgramCompiler for Gles3Compiler {
    type Program = Gles3Program;

    fn compile(&self, desc: &ProgramDesc<'_>) -> Result<Gles3Program, String> {
        desc.check_abi()?;
        let ProgramCode::Glsl(source) = desc.code else {
            return Err(format!(
                "{}: the GLES backend runs GLSL, not {:?}",
                desc.label,
                desc.code.lang()
            ));
        };
        let guard = self
            .context
            .lock()
            .map_err(|_| "the compile context is poisoned")?;
        let context = guard
            .as_ref()
            .ok_or_else(|| format!("{}: no GLES context yet (no surface)", desc.label))?;
        let units: Vec<String> = (0..desc.abi.textures).map(|i| format!("t{i}")).collect();
        let units: Vec<&str> = units.iter().map(String::as_str).collect();
        let program = context.with(|gl| {
            build_program(
                gl,
                source,
                desc.label,
                &["ProgramUniforms", "ProgramInstances"],
                &units,
            )
        })?;
        Ok(Gles3Program {
            program,
            blend: desc.blend,
            abi: desc.abi,
            owner: Some(Arc::clone(&self.context)),
        })
    }
}

// SAFETY: a program name is a token of the shared namespace, usable from
// the backend's thread once compiled.
unsafe impl Send for Gles3Program {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turned_rects_cover_their_turned_pixels() {
        // The clip-space turn `viso_place` makes, in pixels: a quarter turn
        // counter-clockwise takes image pixel (x, y) to framebuffer pixel
        // (y, w - 1 - x), both counted from the top-left.
        let image = [40, 20];
        let pixel = |x: u32, y: u32, turns: u8| match turns {
            1 => (y, 40 - 1 - x),
            2 => (40 - 1 - x, 20 - 1 - y),
            3 => (20 - 1 - y, x),
            _ => (x, y),
        };
        for turns in 0..4 {
            let r = turn_rect((10, 15, 3, 2), image, turns);
            let [fw, fh] = framebuffer(image, turns);
            assert!(
                r.0 + r.2 <= fw && r.1 + r.3 <= fh,
                "{turns}: inside the framebuffer"
            );
            assert_eq!(r.2 * r.3, 6);
            for (x, y) in [(10, 15), (12, 16), (10, 16), (12, 15)] {
                let (px, py) = pixel(x, y, turns);
                assert!(
                    px >= r.0 && px < r.0 + r.2 && py >= r.1 && py < r.1 + r.3,
                    "{turns}: ({x}, {y})"
                );
            }
        }
        assert_eq!(clip((30, 15, 20, 20), image), (30, 15, 10, 5));
    }
}
