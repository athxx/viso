//! `packaged_fonts!` over a real font directory: every sfnt file is embedded
//! with the metadata the manifest keeps, and non-font files are ignored.

use viso::fonts::{FontSlant, PackagedFonts};

static FONTS: PackagedFonts = viso::packaged_fonts!("../text/tests/fixtures");

#[test]
fn every_face_is_packaged_with_its_metadata() {
    let faces = FONTS.faces();
    let files: Vec<_> = faces.iter().map(|face| face.file).collect();
    assert_eq!(files, ["ColrFixture.ttf", "DejaVuSans-subset.ttf"]);
    for (index, face) in faces.iter().enumerate() {
        assert_eq!(face.file_index, index as u32);
        assert_eq!(face.face_index, 0);
        assert_eq!((face.weight, face.width), (400, 5));
        assert_eq!(face.slant, FontSlant::Normal);
        assert!(!face.mono);
        assert!(face.scripts.contains(&"Latn"), "{:?}", face.scripts);
    }
    assert_eq!(faces[0].family, "Viso COLR Fixture");
    assert_eq!(faces[1].family, "DejaVu Sans");
    assert!(faces[0].color, "the COLR fixture");
    assert!(!faces[1].color);
    assert_eq!(
        faces[1].bytes,
        include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")
    );
}

#[test]
fn the_manifest_binds_ui_and_emoji() {
    let manifest = FONTS.manifest();
    assert_eq!(manifest.entries().len(), 2);
    assert_eq!(
        manifest.family_for_role(viso_text::FontRole::Ui),
        Some("DejaVu Sans")
    );
    assert_eq!(
        manifest.family_for_role(viso_text::FontRole::Emoji),
        Some("Viso COLR Fixture")
    );
}
