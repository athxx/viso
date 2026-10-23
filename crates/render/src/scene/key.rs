//! What a retained entry draws, as a damage key: a hash of every
//! field that decides its pixels, and, for the kinds whose bounds the walk
//! does not compute (glyph runs, paths, meshes), the world-space extent it
//! covers.

use std::hash::{Hash, Hasher};

use crate::damage::Key;
use crate::primitive::Rect;

use super::Scene;
use super::store::StoreRef;

/// A [`Hasher`] over a [`Key`], for the fields that are `Hash`.
struct KeyHasher(Key);

impl Hasher for KeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0 = self.0.bytes(bytes);
    }

    fn write_u32(&mut self, v: u32) {
        self.0 = self.0.word(u64::from(v));
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = self.0.word(v);
    }

    fn write_usize(&mut self, v: usize) {
        self.0 = self.0.word(v as u64);
    }

    fn finish(&self) -> u64 {
        self.0.finish()
    }
}

fn hashed(key: Key, value: &impl Hash) -> Key {
    let mut h = KeyHasher(key);
    value.hash(&mut h);
    h.0
}

/// The bytes of `values`, which are plain GPU data.
pub(crate) fn pod_bytes<T: viso_gpu::GpuPod>(values: &[T]) -> &[u8] {
    // SAFETY: a `GpuPod` is `#[repr(C)]` plain old data whose bytes are its
    // device-buffer representation; the view reads initialized memory only and
    // lives as long as the borrow of `values`.
    unsafe { core::slice::from_raw_parts(values.as_ptr().cast::<u8>(), size_of_val(values)) }
}

fn pod<T: viso_gpu::GpuPod>(key: Key, value: &T) -> Key {
    key.bytes(pod_bytes(std::slice::from_ref(value)))
}

impl Scene {
    /// The damage key of the entry `store` names, and its world extent when
    /// its recorded bounds do not hold one.
    pub(crate) fn entry_key(&self, store: StoreRef) -> (u64, Option<Rect>) {
        let key = Key::new(store_kind(store));
        let mut extent = None;
        let key = match store {
            StoreRef::Quad(id) => self.quads.get(id).map(|e| pod(key, &e.instance)),
            StoreRef::AnalyticRRect(id) => {
                self.analytic_rrects.get(id).map(|e| pod(key, &e.instance))
            }
            StoreRef::AnalyticEllipse(id) => self
                .analytic_ellipses
                .get(id)
                .map(|e| pod(key, &e.instance)),
            StoreRef::AnalyticCapsule(id) => self
                .analytic_capsules
                .get(id)
                .map(|e| pod(key, &e.instance)),
            StoreRef::AnalyticLine(id) => {
                self.analytic_lines.get(id).map(|e| pod(key, &e.instance))
            }
            StoreRef::AnalyticShadow(id) => {
                self.analytic_shadows.get(id).map(|e| pod(key, &e.instance))
            }
            StoreRef::Gradient(id) => self
                .gradients
                .get(id)
                .map(|e| hashed(pod(key, &e.instance), &e.texture)),
            StoreRef::Image(id) => self.images.get(id).map(|e| {
                hashed(hashed(pod(key, &e.instance), &e.texture), &e.sampler).word(e.revision)
            }),
            StoreRef::Shader(id) => self.shaders.get(id).map(|e| {
                let key = key.f32s(&[e.rect_pos[0], e.rect_pos[1], e.rect_size[0], e.rect_size[1]]);
                hashed(hashed(key.bytes(&e.params), &e.program), &e.textures)
            }),
            StoreRef::GlyphRun(run) => self.glyph_runs.run(run).map(|e| {
                let glyphs = self.glyph_runs.glyphs(e);
                extent = glyphs
                    .iter()
                    .map(|g| {
                        Rect::new(g.rect_pos[0], g.rect_pos[1], g.rect_size[0], g.rect_size[1])
                    })
                    .reduce(Rect::union);
                hashed(key.bytes(pod_bytes(glyphs)), &e.atlas).word(e.lane as u64)
            }),
            StoreRef::Path(id) => self.paths.get(id).map(|e| {
                let b = e.geometry.bounds;
                let [ox, oy] = e.xform.offset;
                let s = e.xform.scale;
                extent = Some(Rect::new(b.x * s + ox, b.y * s + oy, b.w * s, b.h * s));
                let mut key = hashed(key, &e.geom_key).f32s(&[ox, oy, s, e.paint.opacity]);
                for color in [e.paint.fill, e.paint.stroke_color] {
                    key = match color {
                        Some(c) => key.f32s(&[c.r, c.g, c.b, c.a]),
                        None => key.word(u64::MAX),
                    };
                }
                key
            }),
            StoreRef::Mesh(id) => self.meshes.get(id).map(|e| {
                extent = e
                    .vertices
                    .iter()
                    .map(|v| Rect::new(v.pos[0], v.pos[1], 0.0, 0.0))
                    .reduce(Rect::union);
                let key = key.bytes(pod_bytes(&e.vertices));
                e.indices.iter().fold(key, |k, &i| k.word(u64::from(i)))
            }),
            StoreRef::Composite { pass, slot } => self
                .composites
                .get(slot as usize)
                .map(|instance| pod(key, instance).word(u64::from(pass))),
            StoreRef::BackdropComposite { capture, slot } => self
                .backdrops
                .get(slot as usize)
                .map(|&(rect, opacity)| key.word(u64::from(capture)).rect(rect).f32s(&[opacity])),
            StoreRef::MaterialComposite { capture, material } => {
                Some(key.word(u64::from(capture)).word(u64::from(material)))
            }
        };
        (key.map_or(0, Key::finish), extent)
    }
}

/// A distinct seed per store kind, so equal bytes in two kinds key apart.
fn store_kind(store: StoreRef) -> u64 {
    match store {
        StoreRef::Quad(_) => 1,
        StoreRef::AnalyticRRect(_) => 2,
        StoreRef::AnalyticEllipse(_) => 3,
        StoreRef::AnalyticCapsule(_) => 4,
        StoreRef::AnalyticLine(_) => 5,
        StoreRef::Image(_) => 6,
        StoreRef::Gradient(_) => 7,
        StoreRef::AnalyticShadow(_) => 8,
        StoreRef::Shader(_) => 9,
        StoreRef::GlyphRun(_) => 10,
        StoreRef::Path(_) => 11,
        StoreRef::Mesh(_) => 12,
        StoreRef::Composite { .. } => 13,
        StoreRef::BackdropComposite { .. } => 14,
        StoreRef::MaterialComposite { .. } => 15,
    }
}
