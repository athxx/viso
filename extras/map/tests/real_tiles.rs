//! Decodes a directory of real tiles named `z-x-y.pbf`, when
//! `VISO_MAP_TILES` names one; otherwise does nothing.

use viso_map::mvt;

#[test]
fn real_tiles_decode() {
    let Ok(dir) = std::env::var("VISO_MAP_TILES") else {
        return;
    };
    let mut tiles = 0;
    let mut features = 0;
    let start = std::time::Instant::now();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "pbf") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let tile = mvt::decode(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for layer in &tile.layers {
            for f in &layer.features {
                f.geometry()
                    .unwrap_or_else(|e| panic!("{} {}: {e}", path.display(), layer.name));
                features += 1;
            }
        }
        tiles += 1;
    }
    eprintln!(
        "{tiles} tiles, {features} features in {:?}",
        start.elapsed()
    );
}

/// Triangulates every polygon of the real tiles and checks the triangles
/// cover each polygon's area.
#[test]
fn real_polygons_triangulate_to_their_area() {
    use viso_map::mvt::{GeomType, ring_area};
    use viso_map::tess::earcut::Earcut;
    let Ok(dir) = std::env::var("VISO_MAP_TILES") else {
        return;
    };
    let mut earcut = Earcut::default();
    let (mut polygons, mut off) = (0, 0);
    let mut worst = 0.0f64;
    let start = std::time::Instant::now();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "pbf") {
            continue;
        }
        let tile = mvt::decode(&std::fs::read(&path).unwrap()).unwrap();
        for layer in &tile.layers {
            for f in layer
                .features
                .iter()
                .filter(|f| f.kind == GeomType::Polygon)
            {
                let g = f.geometry().unwrap();
                // Split into polygons at each exterior ring.
                let mut groups: Vec<Vec<&[[i32; 2]]>> = Vec::new();
                for ring in g.parts() {
                    if ring_area(ring) > 0.0 || groups.is_empty() {
                        groups.push(vec![ring]);
                    } else {
                        groups.last_mut().unwrap().push(ring);
                    }
                }
                for rings in groups {
                    let mut pts: Vec<[f32; 2]> = Vec::new();
                    let mut holes = Vec::new();
                    let mut want = 0.0;
                    for (k, r) in rings.iter().enumerate() {
                        if k > 0 {
                            holes.push(pts.len() as u32);
                        }
                        want += ring_area(r) / 2.0;
                        pts.extend(r.iter().map(|p| [p[0] as f32, p[1] as f32]));
                    }
                    let mut tris = Vec::new();
                    earcut.triangulate(&pts, &holes, 0, &mut tris);
                    let got: f64 = tris
                        .chunks(3)
                        .map(|t| {
                            let (a, b, c) =
                                (pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]);
                            ((b[0] - a[0]) as f64 * (c[1] - a[1]) as f64
                                - (c[0] - a[0]) as f64 * (b[1] - a[1]) as f64)
                                .abs()
                                / 2.0
                        })
                        .sum();
                    polygons += 1;
                    let err = (got - want).abs() / want.abs().max(1.0);
                    if err > 0.01 {
                        off += 1;
                    }
                    worst = worst.max(err);
                }
            }
        }
    }
    eprintln!(
        "{polygons} polygons, {off} off by over 1%, worst {worst:.4}, in {:?}",
        start.elapsed()
    );
    assert!(off * 1000 <= polygons, "{off} of {polygons}");
}
