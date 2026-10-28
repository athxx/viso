//! Pictures from outside drawn into textures of the backend's own, sRGB as
//! every video texture is: an Android hardware buffer sampled through
//! `samplerExternalOES`, the driver converting its Y′CbCr, and NV12 planes —
//! uploaded, or a dma-buf's imported where they lie as `R8` and `GR88`
//! images — converted with the picture's own matrix.
//!
//! A conversion is one triangle into the texture's framebuffer, which
//! encodes into sRGB what the fragment writes: the gamma-encoded RGB read
//! is decoded first, so the texture holds the picture's own codes. The
//! images made of a buffer are kept while its holds name it, the latest
//! [`KEPT`] at most; each conversion holds the buffer until a fence after
//! it signals.

use std::collections::VecDeque;
use std::sync::Arc;

use viso_handle::{ExternalHold, ExternalHolds, ExternalImage, Nv12};

use super::sys::*;
use super::{build_program, uniform_location};

/// The buffers whose images are kept at most.
const KEPT: usize = 16;

/// The vertex stage both conversions share: one triangle over the whole
/// target, which a target point `p` fills from the source at
/// `origin + p.x * across + p.y * down` in its texture coordinates. Its
/// first row is the picture's top, as every texture target's.
const VERTEX: &str = "
#ifdef VISO_VERTEX
uniform vec4 place_a; // origin.xy, across.xy
uniform vec4 place_b; // down.xy
out vec2 uv;
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    uv = place_a.xy + p.x * place_a.zw + p.y * place_b.xy;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
#else
precision highp float;
in vec2 uv;
out vec4 color;
// The framebuffer encodes what is written: the codes read are decoded.
vec3 decoded(vec3 c) {
    return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(0.04045, c));
}
";

/// A hardware buffer, through the driver's conversion.
const SAMPLED: &str = "
uniform highp samplerExternalOES picture;
void main() {
    color = vec4(decoded(texture(picture, uv).rgb), 1.0);
}
#endif
";

/// NV12 planes: each row of the matrix turns (Y′, Cb, Cr, 1) into one
/// channel of gamma-encoded RGB; chroma's coordinates follow luma's by a
/// scale, for a plane rounded up to whole samples, then an offset, for its
/// siting.
const PLANAR: &str = "
uniform highp sampler2D luma;
uniform highp sampler2D chroma;
uniform vec4 rows_r;
uniform vec4 rows_g;
uniform vec4 rows_b;
uniform vec4 site;
void main() {
    vec4 ycc = vec4(texture(luma, uv).r, texture(chroma, uv * site.xy + site.zw).rg, 1.0);
    vec3 rgb = vec3(dot(ycc, rows_r), dot(ycc, rows_g), dot(ycc, rows_b));
    color = vec4(decoded(clamp(rgb, 0.0, 1.0)), 1.0);
}
#endif
";

/// What the display and context import, read as they open.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Imports {
    /// Android hardware buffers, sampled as external images.
    pub(super) hardware_buffers: bool,
    /// dma-bufs, as images of one plane each.
    pub(super) dma_bufs: bool,
    /// A dma-buf's layout can be named linear (`EGL_EXT_image_dma_buf_import_modifiers`).
    pub(super) linear_modifier: bool,
}

/// A conversion's program and its uniforms.
struct Program {
    name: GLuint,
    place: [GLint; 2],
    rows: [GLint; 3],
    site: GLint,
}

/// The images made of one buffer and the textures over them: the one
/// external image of a hardware buffer, or a dma-buf's luma and chroma.
struct Source {
    holds: Arc<ExternalHolds>,
    /// What else the images were made for: the dma-buf's layout.
    layout: [u64; 6],
    images: [EGLImage; 2],
    textures: [GLuint; 2],
    /// The buffer's size in texels.
    size: (u32, u32),
}

/// How a conversion reads its picture.
pub(super) enum Read {
    /// An external image: the region of it the picture is, as an offset and
    /// a scale in its texture coordinates, and the quarter turns clockwise
    /// that show it upright.
    Sampled {
        texture: GLuint,
        region: [f32; 4],
        turns: u8,
    },
    /// Luma and chroma textures, the matrix's rows and chroma's siting.
    Planar {
        textures: [GLuint; 2],
        rows: [[f32; 4]; 3],
        site: [f32; 4],
    },
}

/// The conversions' programs, the sources kept and the holds taken.
#[derive(Default)]
pub(super) struct Converter {
    /// Each program once asked for: `None` inside where it does not build.
    sampled: Option<Option<Program>>,
    planar: Option<Option<Program>>,
    /// The latest used last.
    sources: Vec<Source>,
    /// The holds conversions took, with the fence after each, oldest first.
    held: VecDeque<(GLsync, ExternalHold)>,
}

impl Converter {
    /// Whether the program `read` needs builds, building it the first time.
    pub(super) fn ready(&mut self, gl: &Gl, sampled: bool) -> bool {
        let (slot, body, label, units): (_, _, _, &[&str]) = if sampled {
            (&mut self.sampled, SAMPLED, "external picture", &["picture"])
        } else {
            (&mut self.planar, PLANAR, "NV12 planes", &["luma", "chroma"])
        };
        slot.get_or_insert_with(|| {
            let extension = if sampled {
                "#extension GL_OES_EGL_image_external_essl3 : require\n"
            } else {
                ""
            };
            let source = format!("{extension}{VERTEX}{body}");
            match build_program(gl, &source, label, &[], units) {
                Ok(name) => Some(Program {
                    name,
                    place: [
                        uniform_location(gl, name, "place_a"),
                        uniform_location(gl, name, "place_b"),
                    ],
                    rows: [
                        uniform_location(gl, name, "rows_r"),
                        uniform_location(gl, name, "rows_g"),
                        uniform_location(gl, name, "rows_b"),
                    ],
                    site: uniform_location(gl, name, "site"),
                }),
                Err(why) => {
                    eprintln!("viso: GLES converts no external pictures: {why}");
                    None
                }
            }
        })
        .is_some()
    }

    /// The index of the source made of the buffer `holds` names with
    /// `layout`, moved to the latest.
    fn source_of(&mut self, holds: *const ExternalHolds, layout: [u64; 6]) -> Option<usize> {
        let at = self
            .sources
            .iter()
            .position(|s| std::ptr::eq(Arc::as_ptr(&s.holds), holds) && s.layout == layout)?;
        let source = self.sources.remove(at);
        self.sources.push(source);
        Some(self.sources.len() - 1)
    }

    /// Keeps `source` as the latest, letting go of those whose buffers are
    /// gone and of the oldest past [`KEPT`]. GL keeps an image's memory for
    /// the draws already issued that read it.
    fn keep(&mut self, libs: &Libs, display: EGLDisplay, source: Source) -> usize {
        let gone = |s: &Source| Arc::strong_count(&s.holds) == 1;
        let mut k = 0;
        while k < self.sources.len() {
            if gone(&self.sources[k]) || self.sources.len() >= KEPT && k == 0 {
                let s = self.sources.remove(k);
                destroy(libs, display, &s);
            } else {
                k += 1;
            }
        }
        self.sources.push(source);
        self.sources.len() - 1
    }

    /// Draws the picture `read` reads into framebuffer `fbo` of `width` ×
    /// `height`, holding `holds` until the GPU has.
    pub(super) fn draw(
        &mut self,
        gl: &Gl,
        fbo: GLuint,
        (width, height): (u32, u32),
        read: &Read,
        holds: Option<&Arc<ExternalHolds>>,
    ) {
        let program = match read {
            Read::Sampled { .. } => self.sampled.as_ref(),
            Read::Planar { .. } => self.planar.as_ref(),
        };
        let Some(Some(p)) = program else {
            return;
        };
        let (place, textures, target) = match *read {
            Read::Sampled {
                texture,
                region,
                turns,
            } => (place(region, turns), [texture, 0], GL_TEXTURE_EXTERNAL_OES),
            Read::Planar { textures, .. } => {
                (place([0.0, 0.0, 1.0, 1.0], 0), textures, GL_TEXTURE_2D)
            }
        };
        // SAFETY: the context is current; `fbo` is the complete framebuffer
        // of a live texture, the program and textures are live objects of
        // the context; the draw reads no attributes.
        unsafe {
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, fbo);
            (gl.glViewport)(0, 0, width as GLsizei, height as GLsizei);
            (gl.glDisable)(GL_SCISSOR_TEST);
            (gl.glDisable)(GL_BLEND);
            (gl.glDisable)(GL_DEPTH_TEST);
            (gl.glDisable)(GL_CULL_FACE);
            (gl.glUseProgram)(p.name);
            (gl.glBindVertexArray)(0);
            (gl.glUniform4f)(p.place[0], place[0], place[1], place[2], place[3]);
            (gl.glUniform4f)(p.place[1], place[4], place[5], 0.0, 0.0);
            if let Read::Planar { rows, site, .. } = read {
                for (at, r) in p.rows.iter().zip(rows) {
                    (gl.glUniform4f)(*at, r[0], r[1], r[2], r[3]);
                }
                (gl.glUniform4f)(p.site, site[0], site[1], site[2], site[3]);
            }
            for (unit, texture) in textures.iter().enumerate() {
                if *texture != 0 {
                    (gl.glActiveTexture)(GL_TEXTURE0 + unit as GLenum);
                    (gl.glBindTexture)(target, *texture);
                    (gl.glBindSampler)(unit as GLuint, 0);
                }
            }
            (gl.glDrawArraysInstanced)(GL_TRIANGLES, 0, 3, 1);
            for (unit, texture) in textures.iter().enumerate().rev() {
                if *texture != 0 {
                    (gl.glActiveTexture)(GL_TEXTURE0 + unit as GLenum);
                    (gl.glBindTexture)(target, 0);
                }
            }
            (gl.glUseProgram)(0);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
        }
        if let Some(holds) = holds {
            // SAFETY: the context is current; a fence after the draw.
            let fence = unsafe { (gl.glFenceSync)(GL_SYNC_GPU_COMMANDS_COMPLETE, 0) };
            self.held.push_back((fence, holds.hold()));
        }
    }

    /// Lets go of the holds whose conversions the GPU has drawn.
    pub(super) fn release(&mut self, gl: &Gl) {
        while let Some(&(fence, _)) = self.held.front() {
            // SAFETY: the context is current; `fence` is a live sync object
            // of it, polled without waiting.
            let status = unsafe { (gl.glClientWaitSync)(fence, 0, 0) };
            if status != GL_ALREADY_SIGNALED && status != GL_CONDITION_SATISFIED {
                break;
            }
            // SAFETY: as above; the fence is not used again.
            unsafe { (gl.glDeleteSync)(fence) };
            self.held.pop_front();
        }
    }
}

/// Deletes a source's textures and images.
fn destroy(libs: &Libs, display: EGLDisplay, s: &Source) {
    // SAFETY: the context is current; the textures and images are this
    // source's own, not used after; GL keeps their memory for the draws
    // already issued.
    unsafe {
        for t in s.textures.iter().filter(|t| **t != 0) {
            (libs.gl.glDeleteTextures)(1, t);
        }
        if let Some(images) = &libs.images {
            for image in s.images.iter().filter(|i| !i.is_null()) {
                (images.destroy)(display, *image);
            }
        }
    }
}

/// A texture of `target` over `image`, sampled linearly and clamped; `None`
/// where GL does not take the image.
///
/// # Safety
///
/// The context is current; `image` is a live image of the display.
unsafe fn texture_of(libs: &Libs, target: GLenum, image: EGLImage) -> Option<GLuint> {
    let gl = &libs.gl;
    let images = libs.images.as_ref()?;
    let mut name = 0;
    // SAFETY: the caller's contract; the texture is a new one of `target`,
    // its errors read so as not to be mistaken for later ones.
    unsafe {
        while (gl.glGetError)() != GL_NO_ERROR {}
        (gl.glGenTextures)(1, &mut name);
        (gl.glBindTexture)(target, name);
        (images.target_texture)(target, image);
        for (param, value) in [
            (GL_TEXTURE_MIN_FILTER, GL_LINEAR),
            (GL_TEXTURE_MAG_FILTER, GL_LINEAR),
            (GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE),
            (GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE),
        ] {
            (gl.glTexParameteri)(target, param, value);
        }
        (gl.glBindTexture)(target, 0);
        if (gl.glGetError)() != GL_NO_ERROR {
            (gl.glDeleteTextures)(1, &name);
            return None;
        }
    }
    Some(name)
}

/// What `AHardwareBuffer_describe` tells of a buffer.
#[cfg(target_os = "android")]
#[repr(C)]
#[derive(Default)]
struct AHardwareBufferDesc {
    width: u32,
    height: u32,
    layers: u32,
    format: u32,
    usage: u64,
    stride: u32,
    rfu0: u32,
    rfu1: u64,
}

#[cfg(target_os = "android")]
#[link(name = "nativewindow")]
unsafe extern "C" {
    fn AHardwareBuffer_describe(buffer: *const std::ffi::c_void, desc: *mut AHardwareBufferDesc);
}

impl Converter {
    /// How to read `image` — its images made if they are new — or `None`
    /// where it cannot be imported; with the holds to take for reading it.
    pub(super) fn read(
        &mut self,
        libs: &Libs,
        display: EGLDisplay,
        imports: Imports,
        image: &ExternalImage,
    ) -> Option<(Read, Arc<ExternalHolds>)> {
        match *image {
            ExternalImage::AndroidHardwareBuffer {
                buffer,
                holds,
                x,
                y,
                width,
                height,
                turns,
            } if imports.hardware_buffers => {
                let at = match self.source_of(holds, [0; 6]) {
                    Some(at) => at,
                    None => {
                        // SAFETY: the producer keeps the buffer and its
                        // holds alive while it hands the image over.
                        let made = unsafe { hardware_buffer(libs, display, buffer, holds)? };
                        self.keep(libs, display, made)
                    }
                };
                let source = &self.sources[at];
                let (bw, bh) = (source.size.0.max(1) as f32, source.size.1.max(1) as f32);
                let read = Read::Sampled {
                    texture: source.textures[0],
                    region: [
                        x as f32 / bw,
                        y as f32 / bh,
                        width as f32 / bw,
                        height as f32 / bh,
                    ],
                    turns,
                };
                Some((read, Arc::clone(&source.holds)))
            }
            ExternalImage::DmaBuf {
                fd,
                len,
                offsets,
                strides,
                holds,
                matrix,
                full_range,
                left_chroma,
                width,
                height,
            } if imports.dma_bufs => {
                let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
                let fits = |offset: u64, stride: u32, row: u64, rows: u64| {
                    stride > 0
                        && u64::from(stride) >= row
                        && offset <= i32::MAX as u64
                        && stride <= i32::MAX as u32
                        && offset
                            .checked_add(u64::from(stride) * (rows.max(1) - 1) + row)
                            .is_some_and(|end| end <= len)
                };
                if width == 0
                    || height == 0
                    || !fits(offsets[0], strides[0], u64::from(width), u64::from(height))
                    || !fits(offsets[1], strides[1], u64::from(cw) * 2, u64::from(ch))
                {
                    return None;
                }
                let layout = [
                    offsets[0],
                    offsets[1],
                    u64::from(strides[0]),
                    u64::from(strides[1]),
                    u64::from(width),
                    u64::from(height),
                ];
                let at = match self.source_of(holds, layout) {
                    Some(at) => at,
                    None => {
                        let plane = |fourcc, w: u32, h: u32, k: usize| Plane {
                            fourcc,
                            width: w,
                            height: h,
                            offset: offsets[k],
                            pitch: strides[k],
                        };
                        // SAFETY: the producer keeps the dma-buf and its
                        // holds alive while it hands the image over.
                        let made = unsafe {
                            dma_buf(
                                libs,
                                display,
                                imports.linear_modifier,
                                fd,
                                [
                                    plane(DRM_FORMAT_R8, width, height, 0),
                                    plane(DRM_FORMAT_GR88, cw, ch, 1),
                                ],
                                holds,
                                layout,
                            )?
                        };
                        self.keep(libs, display, made)
                    }
                };
                let source = &self.sources[at];
                let picture = Nv12 {
                    width,
                    height,
                    y: &[],
                    y_stride: 0,
                    uv: &[],
                    uv_stride: 0,
                    matrix,
                    full_range,
                    left_chroma,
                };
                let read = Read::Planar {
                    textures: source.textures,
                    rows: picture.matrix.rows(full_range),
                    site: site(&picture),
                };
                Some((read, Arc::clone(&source.holds)))
            }
            _ => None,
        }
    }
}

/// A source of an Android hardware buffer: an external image of it.
///
/// # Safety
///
/// The context is current; `buffer` is a live `AHardwareBuffer` and `holds`
/// a live `Arc`'s.
#[cfg(target_os = "android")]
unsafe fn hardware_buffer(
    libs: &Libs,
    display: EGLDisplay,
    buffer: *mut std::ffi::c_void,
    holds: *const ExternalHolds,
) -> Option<Source> {
    let images = libs.images.as_ref()?;
    let client_buffer = images.native_client_buffer?;
    let mut desc = AHardwareBufferDesc::default();
    let attribs = [EGL_IMAGE_PRESERVED_KHR, EGL_TRUE as EGLint, EGL_NONE];
    // SAFETY: the caller's contract; the image takes its own reference on
    // the buffer, the attribute list ends in `EGL_NONE`.
    let image = unsafe {
        AHardwareBuffer_describe(buffer.cast_const(), &mut desc);
        (images.create)(
            display,
            NO_CONTEXT,
            EGL_NATIVE_BUFFER_ANDROID,
            client_buffer(buffer.cast_const()),
            attribs.as_ptr(),
        )
    };
    if image.is_null() {
        return None;
    }
    // SAFETY: the caller's contract; the image was just made.
    let Some(texture) = (unsafe { texture_of(libs, GL_TEXTURE_EXTERNAL_OES, image) }) else {
        // SAFETY: the image is not used again.
        unsafe { (images.destroy)(display, image) };
        return None;
    };
    Some(Source {
        // SAFETY: the caller's contract.
        holds: unsafe { ExternalHolds::retain(holds) },
        layout: [0; 6],
        images: [image, NO_IMAGE],
        textures: [texture, 0],
        size: (desc.width, desc.height),
    })
}

#[cfg(not(target_os = "android"))]
unsafe fn hardware_buffer(
    _: &Libs,
    _: EGLDisplay,
    _: *mut std::ffi::c_void,
    _: *const ExternalHolds,
) -> Option<Source> {
    None
}

/// One plane of a dma-buf, as an image of its own.
struct Plane {
    fourcc: EGLint,
    width: u32,
    height: u32,
    offset: u64,
    pitch: u32,
}

/// A source of a dma-buf's two planes, an image each, laid out linearly.
///
/// # Safety
///
/// The context is current; `fd` is a live dma-buf holding both planes and
/// `holds` a live `Arc`'s.
unsafe fn dma_buf(
    libs: &Libs,
    display: EGLDisplay,
    linear_modifier: bool,
    fd: i32,
    planes: [Plane; 2],
    holds: *const ExternalHolds,
    layout: [u64; 6],
) -> Option<Source> {
    let images = libs.images.as_ref()?;
    let mut made = [NO_IMAGE; 2];
    let mut textures = [0; 2];
    for (k, p) in planes.iter().enumerate() {
        let mut attribs = vec![
            EGL_WIDTH,
            p.width as EGLint,
            EGL_HEIGHT,
            p.height as EGLint,
            EGL_LINUX_DRM_FOURCC_EXT,
            p.fourcc,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            fd,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            p.offset as EGLint,
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            p.pitch as EGLint,
        ];
        if linear_modifier {
            // `DRM_FORMAT_MOD_LINEAR` is zero.
            attribs.extend([
                EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
                0,
                EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
                0,
            ]);
        }
        attribs.push(EGL_NONE);
        // SAFETY: the caller's contract; EGL references the dma-buf's memory
        // rather than the descriptor, which stays the producer's; the
        // attribute list ends in `EGL_NONE`.
        let image = unsafe {
            (images.create)(
                display,
                NO_CONTEXT,
                EGL_LINUX_DMA_BUF_EXT,
                std::ptr::null_mut(),
                attribs.as_ptr(),
            )
        };
        // SAFETY: the caller's contract; the image was just made.
        let texture = (!image.is_null())
            .then(|| unsafe { texture_of(libs, GL_TEXTURE_2D, image) })
            .flatten();
        made[k] = image;
        match texture {
            Some(t) => textures[k] = t,
            None => {
                let failed = Source {
                    // SAFETY: the caller's contract.
                    holds: unsafe { ExternalHolds::retain(holds) },
                    layout,
                    images: made,
                    textures,
                    size: (0, 0),
                };
                destroy(libs, display, &failed);
                return None;
            }
        }
    }
    Some(Source {
        // SAFETY: the caller's contract.
        holds: unsafe { ExternalHolds::retain(holds) },
        layout,
        images: made,
        textures,
        size: (planes[0].width, planes[0].height),
    })
}

/// How chroma's texture coordinates follow luma's for `picture`: a scale,
/// for a plane rounded up to whole samples, then an offset, for a
/// left-sited sample, which lies on its left luma sample, a quarter of a
/// sample before where its texel's center falls.
pub(super) fn site(picture: &Nv12<'_>) -> [f32; 4] {
    let (cw, ch) = (picture.width.div_ceil(2), picture.height.div_ceil(2));
    let ox = if picture.left_chroma {
        0.25 / cw as f32
    } else {
        0.0
    };
    [
        picture.width as f32 / (2 * cw) as f32,
        picture.height as f32 / (2 * ch) as f32,
        ox,
        0.0,
    ]
}

/// Where a target point at the top left reads its source, and the steps
/// across and down it take there, for the picture's `region` — its offset
/// and scale in the source's texture coordinates — turned `turns` quarter
/// turns clockwise: origin, across, down.
fn place(region: [f32; 4], turns: u8) -> [f32; 6] {
    let [x, y, w, h] = region;
    // In the region's own coordinates, 0 to 1 each way.
    let (origin, across, down) = match turns % 4 {
        0 => ([0.0, 0.0], [1.0, 0.0], [0.0, 1.0]),
        // Turned a quarter clockwise, the target's top row is the source's
        // left column read upward.
        1 => ([0.0, 1.0], [0.0, -1.0], [1.0, 0.0]),
        2 => ([1.0, 1.0], [-1.0, 0.0], [0.0, -1.0]),
        _ => ([1.0, 0.0], [0.0, 1.0], [-1.0, 0.0]),
    };
    [
        x + origin[0] * w,
        y + origin[1] * h,
        across[0] * w,
        across[1] * h,
        down[0] * w,
        down[1] * h,
    ]
}

#[cfg(test)]
mod tests {
    use super::place;

    /// Turned a quarter clockwise, the source's bottom-left corner shows at
    /// the top left and its top-left at the top right.
    #[test]
    fn a_turned_picture_reads_its_region_from_the_turned_corner() {
        let read = |p: [f32; 6], q: [f32; 2]| {
            [
                p[0] + q[0] * p[2] + q[1] * p[4],
                p[1] + q[0] * p[3] + q[1] * p[5],
            ]
        };
        let region = [0.25, 0.5, 0.5, 0.25];
        let corners = |turns| {
            let p = place(region, turns);
            [read(p, [0.0, 0.0]), read(p, [1.0, 0.0])]
        };
        assert_eq!(corners(0), [[0.25, 0.5], [0.75, 0.5]]);
        assert_eq!(corners(1), [[0.25, 0.75], [0.25, 0.5]]);
        assert_eq!(corners(2), [[0.75, 0.75], [0.25, 0.75]]);
        assert_eq!(corners(4), corners(0));
    }
}
