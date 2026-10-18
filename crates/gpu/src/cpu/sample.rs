//! Texture sampling: the bind group a draw names, and a GPU sample of a
//! texture at a normalized coordinate.

use super::fill::{Cx, LINEAR_CLAMP};
use crate::resource::{AddressMode, Binding, FilterMode, SamplerDesc};
use crate::{CpuTexels, CpuTexture, TextureId};

/// The texels a draw samples: a whole texture, or the window of a deferred
/// layer evaluated for one tile ([`super::pass`]). Coordinates are the whole
/// texture's either way; a window holds the texels at `origin..` row by row,
/// `stride` apart, and a fetch outside it reads its nearest edge.
#[derive(Clone, Copy)]
pub(super) struct View<'a> {
    pub width: u32,
    pub height: u32,
    pub texels: CpuTexels<'a>,
    pub origin: [u32; 2],
    /// The window's extent, `[width, height]`.
    pub extent: [u32; 2],
}

impl<'a> View<'a> {
    /// The whole of `texture`.
    pub(super) fn whole(texture: CpuTexture<'a>) -> View<'a> {
        View {
            width: texture.width,
            height: texture.height,
            texels: texture.texels,
            origin: [0, 0],
            extent: [texture.width, texture.height],
        }
    }

    #[inline]
    fn get(&self, x: u32, y: u32) -> [f32; 4] {
        let wx = x.saturating_sub(self.origin[0]).min(self.extent[0] - 1);
        let wy = y.saturating_sub(self.origin[1]).min(self.extent[1] - 1);
        self.texels.get((wy * self.extent[0] + wx) as usize)
    }
}

/// Texture `slot` of the draw's bind group (counting texture bindings only)
/// and the sampler it reads through: the group's last sampler, or linear
/// clamp-to-edge. `None` when the draw binds no such texture. A texture the
/// draw's deferred layer stands in for reads through that layer's window.
pub(super) fn texture_bindings<'a>(cx: &Cx<'a>, slot: usize) -> Option<(View<'a>, SamplerDesc)> {
    let group = cx.res.bind_groups.get(cx.cmd.bind_group?.into())?;
    let mut sampler = LINEAR_CLAMP;
    let mut texture = None;
    let mut seen = 0;
    for binding in group {
        match *binding {
            Binding::Texture(id) => {
                if seen == slot {
                    texture = Some(id);
                }
                seen += 1;
            }
            Binding::Sampler(id) => {
                if let Some(desc) = cx.res.samplers.get(id.into()) {
                    sampler = *desc;
                }
            }
            Binding::Uniform(_) => {}
        }
    }
    let id: TextureId = texture?;
    if let Some((layer, view)) = cx.layer
        && layer == id
    {
        return Some((view, sampler));
    }
    let texture = cx.res.textures.get(id.into())?;
    Some((View::whole(texture.view()), sampler))
}

/// `texture` at `(u, v)` through `sampler`, with texel centers at `+0.5`.
/// There is no mip chain, so trilinear filtering samples the base level
/// bilinearly.
pub(super) fn sample(texture: &View<'_>, sampler: &SamplerDesc, u: f32, v: f32) -> [f32; 4] {
    let (tw, th) = (texture.width, texture.height);
    if tw == 0 || th == 0 {
        return [0.0; 4];
    }
    let tx = u * tw as f32 - 0.5;
    let ty = v * th as f32 - 0.5;
    // An axis coordinate folded into `[0, n)` as a period-`2n` triangle wave.
    let mirror = |i: i32, n: i32| {
        let m = i.rem_euclid(2 * n);
        if m < n { m } else { 2 * n - 1 - m }
    };
    let fetch = |ix: i32, iy: i32| {
        let (cx, cy) = match sampler.address {
            AddressMode::ClampToEdge => (ix.clamp(0, tw as i32 - 1), iy.clamp(0, th as i32 - 1)),
            AddressMode::Repeat => (ix.rem_euclid(tw as i32), iy.rem_euclid(th as i32)),
            AddressMode::Mirror => (mirror(ix, tw as i32), mirror(iy, th as i32)),
        };
        texture.get(cx as u32, cy as u32)
    };
    match sampler.filter {
        FilterMode::Nearest => fetch(tx.round() as i32, ty.round() as i32),
        FilterMode::Linear | FilterMode::MipmapLinear => {
            let (x0, y0) = (tx.floor(), ty.floor());
            let (fx, fy) = (tx - x0, ty - y0);
            let (x0, y0) = (x0 as i32, y0 as i32);
            let c00 = fetch(x0, y0);
            let c10 = fetch(x0 + 1, y0);
            let c01 = fetch(x0, y0 + 1);
            let c11 = fetch(x0 + 1, y0 + 1);
            std::array::from_fn(|c| {
                let top = c00[c] * (1.0 - fx) + c10[c] * fx;
                let bottom = c01[c] * (1.0 - fx) + c11[c] * fx;
                top * (1.0 - fy) + bottom * fy
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×1 texture: texel 0 red, texel 1 green, both opaque.
    const TEXELS: [[f32; 4]; 2] = [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]];

    fn two_texel() -> View<'static> {
        View::whole(CpuTexture {
            width: 2,
            height: 1,
            texels: CpuTexels::Float(&TEXELS),
        })
    }

    fn nearest(address: AddressMode) -> SamplerDesc {
        SamplerDesc {
            filter: FilterMode::Nearest,
            address,
        }
    }

    #[test]
    fn mirror_reflects_across_the_edge() {
        let (tex, samp) = (two_texel(), nearest(AddressMode::Mirror));
        assert_eq!(sample(&tex, &samp, 0.25, 0.5), TEXELS[0]);
        assert_eq!(sample(&tex, &samp, 0.75, 0.5), TEXELS[1]);
        // The second period is mirrored: green near the seam, red at its end.
        assert_eq!(sample(&tex, &samp, 1.25, 0.5), TEXELS[1]);
        assert_eq!(sample(&tex, &samp, 1.75, 0.5), TEXELS[0]);
    }

    #[test]
    fn mirror_differs_from_repeat_in_the_reflected_period() {
        let tex = two_texel();
        let repeat = sample(&tex, &nearest(AddressMode::Repeat), 1.75, 0.5);
        let mirror = sample(&tex, &nearest(AddressMode::Mirror), 1.75, 0.5);
        assert_eq!(repeat, TEXELS[1]);
        assert_eq!(mirror, TEXELS[0]);
    }

    #[test]
    fn trilinear_samples_the_base_level_bilinearly() {
        let tex = two_texel();
        let base = SamplerDesc {
            filter: FilterMode::Linear,
            address: AddressMode::ClampToEdge,
        };
        let mip = SamplerDesc {
            filter: FilterMode::MipmapLinear,
            ..base
        };
        for u in [0.0, 0.3, 0.5, 0.8, 1.0] {
            assert_eq!(
                sample(&tex, &base, u, 0.5),
                sample(&tex, &mip, u, 0.5),
                "u = {u}"
            );
        }
    }

    #[test]
    fn a_window_samples_as_the_whole_texture_does_inside_it() {
        // A 4×3 ramp, and its window over texels x 1..3, y 1..3.
        let whole: Vec<[f32; 4]> = (0..12).map(|i| [i as f32 / 11.0, 0.0, 0.0, 1.0]).collect();
        let window: Vec<[f32; 4]> = [5, 6, 9, 10].iter().map(|&i| whole[i]).collect();
        let full = View::whole(CpuTexture {
            width: 4,
            height: 3,
            texels: CpuTexels::Float(&whole),
        });
        let part = View {
            width: 4,
            height: 3,
            texels: CpuTexels::Float(&window),
            origin: [1, 1],
            extent: [2, 2],
        };
        let linear = SamplerDesc {
            filter: FilterMode::Linear,
            address: AddressMode::ClampToEdge,
        };
        for (u, v) in [(0.375, 0.5), (0.5, 0.6), (0.6, 0.7)] {
            assert_eq!(
                sample(&full, &linear, u, v),
                sample(&part, &linear, u, v),
                "({u}, {v})"
            );
        }
    }

    #[test]
    fn color_texels_read_as_uploaded_in_either_byte_order() {
        let rgba = [[128u8, 0, 0, 128]];
        let bgra = [[0u8, 0, 128, 128]];
        let a = CpuTexels::Rgba8 {
            texels: &rgba,
            bgra: false,
        };
        let b = CpuTexels::Rgba8 {
            texels: &bgra,
            bgra: true,
        };
        assert_eq!(a.get(0), [128.0 / 255.0, 0.0, 0.0, 128.0 / 255.0]);
        assert_eq!(a.get(0), b.get(0));
    }
}
