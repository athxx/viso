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
