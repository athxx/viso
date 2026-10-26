//! Geometry a model's file leaves out, built at load: flat normals, tangents,
//! and triangle lists from strips and fans.

/// `v` scaled to unit length; a zero vector becomes `+y`.
pub(crate) fn unit(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-20 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 1.0, 0.0]
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// A triangle strip's triangles, each wound as its first is.
pub(crate) fn strip(indices: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(indices.len().saturating_sub(2) * 3);
    for i in 2..indices.len() {
        let (a, b, c) = (indices[i - 2], indices[i - 1], indices[i]);
        if i % 2 == 0 {
            out.extend([a, b, c]);
        } else {
            out.extend([b, a, c]);
        }
    }
    out
}

/// A triangle fan's triangles.
pub(crate) fn fan(indices: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(indices.len().saturating_sub(2) * 3);
    for i in 2..indices.len() {
        out.extend([indices[0], indices[i - 1], indices[i]]);
    }
    out
}

/// A triangle list unshared: the vertex each corner reads, in corner order.
pub(crate) struct Flat<'a> {
    corners: &'a [u32],
}

impl<'a> Flat<'a> {
    pub(crate) fn of(indices: &'a [u32]) -> Flat<'a> {
        Flat { corners: indices }
    }

    /// `attr` read once per corner; an empty attribute stays empty.
    pub(crate) fn gather<T: Copy>(&self, attr: &[T]) -> Vec<T> {
        if attr.is_empty() {
            return Vec::new();
        }
        self.corners.iter().map(|&i| attr[i as usize]).collect()
    }
}

/// The face normal of each triangle of an unshared list, on its corners.
pub(crate) fn flat_normals(positions: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(positions.len());
    for tri in positions.as_chunks::<3>().0 {
        let n = unit(cross(sub(tri[1], tri[0]), sub(tri[2], tri[0])));
        out.extend([n; 3]);
    }
    out
}

/// Per-vertex tangents along `+u`, from each triangle's position and
/// texture-coordinate gradients summed at its corners, made perpendicular
/// to the normal. `w` is the handedness of the bitangent `cross(n, t) · w`,
/// which points along `-v`: up the image, as a glTF normal map's green does.
pub(crate) fn tangents(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    uvs: &[[f32; 2]],
    indices: &[u32],
) -> Vec<[f32; 4]> {
    let n = positions.len();
    let mut tan = vec![[0.0f32; 3]; n];
    let mut bit = vec![[0.0f32; 3]; n];
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let (e1, e2) = (
            sub(positions[b], positions[a]),
            sub(positions[c], positions[a]),
        );
        let (du1, dv1) = (uvs[b][0] - uvs[a][0], uvs[b][1] - uvs[a][1]);
        let (du2, dv2) = (uvs[c][0] - uvs[a][0], uvs[c][1] - uvs[a][1]);
        let det = du1 * dv2 - du2 * dv1;
        if det.abs() < 1e-20 {
            continue;
        }
        let r = 1.0 / det;
        let t: [f32; 3] = core::array::from_fn(|i| (e1[i] * dv2 - e2[i] * dv1) * r);
        let s: [f32; 3] = core::array::from_fn(|i| (e2[i] * du1 - e1[i] * du2) * r);
        for &v in &[a, b, c] {
            for i in 0..3 {
                tan[v][i] += t[i];
                bit[v][i] += s[i];
            }
        }
    }
    (0..n)
        .map(|v| {
            let nrm = normals[v];
            let t = tan[v];
            // Gram-Schmidt against the normal.
            let k = dot(nrm, t);
            let mut o = [t[0] - nrm[0] * k, t[1] - nrm[1] * k, t[2] - nrm[2] * k];
            if dot(o, o) < 1e-20 {
                // No gradient here: any axis across the normal.
                let axis = if nrm[0].abs() < 0.9 {
                    [1.0, 0.0, 0.0]
                } else {
                    [0.0, 1.0, 0.0]
                };
                o = cross(axis, nrm);
            }
            let o = unit(o);
            let w = if dot(cross(nrm, o), bit[v]) < 0.0 {
                1.0
            } else {
                -1.0
            };
            [o[0], o[1], o[2], w]
        })
        .collect()
}
