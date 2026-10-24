//! 4:2:0 Y′CbCr pictures to RGBA rows, for a player whose pictures come as
//! planes.

/// One plane: its bytes from the picture's top-left sample, the bytes
/// between rows, and between samples of a row (1 planar, 2 interleaved).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Plane<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) row_stride: usize,
    pub(crate) pixel_stride: usize,
}

/// The matrix and range a picture was encoded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Matrix {
    /// BT.601, studio range: standard definition.
    Bt601,
    /// BT.709, studio range: high definition.
    Bt709,
}

impl Matrix {
    /// The matrix a picture `height` rows tall is taken to use when it does
    /// not say.
    pub(crate) fn for_height(height: u32) -> Matrix {
        if height >= 720 {
            Matrix::Bt709
        } else {
            Matrix::Bt601
        }
    }

    /// `[y, v→r, u→g, v→g, u→b]`, in 1/65536.
    fn coefficients(self) -> [i32; 5] {
        match self {
            Matrix::Bt601 => [76309, 104597, 25675, 53279, 132201],
            Matrix::Bt709 => [76309, 117489, 13975, 34925, 138438],
        }
    }
}

/// Writes the `width` × `height` picture in `y`, `u` and `v` (chroma at half
/// resolution both ways) into `out` as opaque RGBA rows `width * 4` bytes
/// long. Returns `false`, writing nothing, when a plane is too short for the
/// picture.
pub(crate) fn to_rgba(
    width: usize,
    height: usize,
    y: Plane<'_>,
    u: Plane<'_>,
    v: Plane<'_>,
    matrix: Matrix,
    out: &mut Vec<u8>,
) -> bool {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let fits = |p: &Plane<'_>, w: usize, h: usize| {
        h == 0 || w == 0 || p.bytes.len() > (h - 1) * p.row_stride + (w - 1) * p.pixel_stride
    };
    if !fits(&y, width, height) || !fits(&u, cw, ch) || !fits(&v, cw, ch) {
        return false;
    }
    let [ky, kvr, kug, kvg, kub] = matrix.coefficients();
    out.clear();
    out.resize(width * height * 4, 0);
    for (row, line) in out.chunks_exact_mut(width * 4).enumerate() {
        let ys = &y.bytes[row * y.row_stride..];
        let us = &u.bytes[(row / 2) * u.row_stride..];
        let vs = &v.bytes[(row / 2) * v.row_stride..];
        for (col, texel) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let l = (i32::from(ys[col * y.pixel_stride]) - 16) * ky;
            let cb = i32::from(us[(col / 2) * u.pixel_stride]) - 128;
            let cr = i32::from(vs[(col / 2) * v.pixel_stride]) - 128;
            let channel = |x: i32| ((x + 32768) >> 16).clamp(0, 255) as u8;
            texel[0] = channel(l + kvr * cr);
            texel[1] = channel(l - kug * cb - kvg * cr);
            texel[2] = channel(l + kub * cb);
            texel[3] = 255;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn studio_range_planes_become_full_range_rgba_planar_or_interleaved() {
        // Studio white and black, sharing neutral chroma.
        let y = [235, 16, 81, 41];
        let (u, v) = ([128u8], [128u8]);
        let plane = |bytes, row_stride, pixel_stride| Plane {
            bytes,
            row_stride,
            pixel_stride,
        };
        let mut out = Vec::new();
        assert!(to_rgba(
            2,
            1,
            plane(&y[..2], 2, 1),
            plane(&u, 1, 1),
            plane(&v, 1, 1),
            Matrix::Bt601,
            &mut out
        ));
        assert_eq!(out, [255, 255, 255, 255, 0, 0, 0, 255]);

        // Red (Y 81, Cb 90, Cr 240) through interleaved chroma, Cb then Cr.
        let uv = [90u8, 240];
        assert!(to_rgba(
            1,
            1,
            plane(&[81], 1, 1),
            plane(&uv[..], 2, 2),
            plane(&uv[1..], 2, 2),
            Matrix::Bt601,
            &mut out
        ));
        let [r, g, b, a] = out[..] else { panic!() };
        assert!(r >= 250 && g <= 5 && b <= 5 && a == 255, "{out:?}");

        // A plane too short for the picture writes nothing.
        assert!(!to_rgba(
            2,
            2,
            plane(&y[..3], 2, 1),
            plane(&u, 1, 1),
            plane(&v, 1, 1),
            Matrix::Bt709,
            &mut out
        ));
        assert_eq!(Matrix::for_height(1080), Matrix::Bt709);
        assert_eq!(Matrix::for_height(480), Matrix::Bt601);
    }
}
