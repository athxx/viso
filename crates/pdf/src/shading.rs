//! Smooth shadings: a ramp sampled into a function, along an axis or a
//! radius, and colored triangle meshes.

use std::io::Write;

use crate::object::{Ref, Writer, num};

/// One shading's geometry, in the space it paints in.
pub(crate) enum Geometry {
    /// From `from` (the ramp's start) to `to` (its end).
    Axial { from: [f32; 2], to: [f32; 2] },
    /// From `center` (the ramp's start) out to `radius` (its end).
    Radial { center: [f32; 2], radius: f32 },
}

/// Writes shading `at` painting `ramp` (equally spaced samples, each of
/// `channels` components in `[0, 1]`) over `geometry`, in RGB for three
/// channels and gray for one, its ends extended.
pub(crate) fn write_ramp(
    w: &mut Writer,
    at: Ref,
    geometry: &Geometry,
    ramp: &[f32],
    channels: usize,
) {
    let samples = ramp.len() / channels;
    let function = w.alloc();
    let bytes: Vec<u8> = ramp
        .iter()
        .map(|&v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect();
    w.stream(
        function,
        |out| {
            let _ = write!(
                out,
                "/FunctionType 0/Domain[0 1]/Size[{samples}]/BitsPerSample 8/Range["
            );
            for _ in 0..channels {
                out.extend_from_slice(b"0 1 ");
            }
            out.push(b']');
        },
        &bytes,
    );
    let space = if channels == 1 {
        "/DeviceGray"
    } else {
        "/DeviceRGB"
    };
    w.obj(at, |out| {
        let mut coords = Vec::new();
        let kind = match *geometry {
            Geometry::Axial { from, to } => {
                for v in [from[0], from[1], to[0], to[1]] {
                    num(&mut coords, v);
                    coords.push(b' ');
                }
                2
            }
            Geometry::Radial { center, radius } => {
                for v in [center[0], center[1], 0.0, center[0], center[1], radius] {
                    num(&mut coords, v);
                    coords.push(b' ');
                }
                3
            }
        };
        let _ = write!(out, "<</ShadingType {kind}/ColorSpace{space}/Coords[");
        out.extend_from_slice(&coords);
        out.extend_from_slice(b"]/Function ");
        function.put(out);
        out.extend_from_slice(b"/Extend[true true]>>");
    });
}

/// Writes shading `at` painting the triangles `triangles` (three vertices
/// each: a position within `bounds` and `channels` components in `[0, 1]`),
/// each blending its corners' colors.
pub(crate) fn write_mesh(
    w: &mut Writer,
    at: Ref,
    bounds: [f32; 4],
    triangles: &[([f32; 2], [f32; 3])],
    channels: usize,
) {
    let [x0, y0, x1, y1] = bounds;
    let (sx, sy) = ((x1 - x0).max(1e-3), (y1 - y0).max(1e-3));
    let mut data = Vec::with_capacity(triangles.len() * (9 + channels));
    for &(pos, color) in triangles {
        data.push(0u8);
        let fx = (((pos[0] - x0) / sx).clamp(0.0, 1.0) * u32::MAX as f32) as u32;
        let fy = (((pos[1] - y0) / sy).clamp(0.0, 1.0) * u32::MAX as f32) as u32;
        data.extend_from_slice(&fx.to_be_bytes());
        data.extend_from_slice(&fy.to_be_bytes());
        for &c in &color[..channels] {
            data.push((c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    let space = if channels == 1 {
        "/DeviceGray"
    } else {
        "/DeviceRGB"
    };
    w.stream(
        at,
        |out| {
            let _ = write!(
                out,
                "/ShadingType 4/ColorSpace{space}/BitsPerCoordinate 32/BitsPerComponent 8/BitsPerFlag 8/Decode["
            );
            for v in [x0, x1, y0, y1] {
                num(out, v);
                out.push(b' ');
            }
            for _ in 0..channels {
                out.extend_from_slice(b"0 1 ");
            }
            out.push(b']');
        },
        &data,
    );
}
