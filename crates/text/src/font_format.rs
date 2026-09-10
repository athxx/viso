//! Font container format detection and face enumeration for sfnt containers:
//! single TrueType / OpenType faces and TTC/OTC collections.
//!
//! The framework's packaged/external font input is sfnt only (TTF/OTF/TTC/OTC);
//! it does not decode or decompress WOFF2. A caller holding a compressed web
//! font decompresses it to sfnt before supplying the bytes here, or injects
//! already-decoded sfnt through the external font-provider seam (see ADR 0028
//! and ADR 0026).
//!
//! Given owned bytes, this identifies the container from its sfnt magic and
//! reports how many faces it holds, so the resolver can register each as a
//! [`crate::FontFaceId`]. It does not parse tables, shape, or rasterize — that
//! is the shaper's and rasterizer's job once a face index is chosen.

/// A detected sfnt container format.
///
/// The four accepted on-disk shapes collapse to two container kinds: a single
/// face (TTF/OTF, any of the three single-face magics) and a collection
/// wrapper (TTC/OTC) that holds one or more faces sharing a byte blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFormat {
    /// A single sfnt face: TrueType (`0x00010000` or `true`) or OpenType/CFF
    /// (`OTTO`). Exactly one face at index 0.
    Sfnt,
    /// An sfnt collection (`ttcf`, i.e. TTC/OTC) holding one or more faces that
    /// share the same underlying byte blob, addressed by face index.
    Collection,
}

/// The sfnt magic for a single TrueType face (`0x00010000`).
const MAGIC_TRUETYPE: u32 = 0x0001_0000;
/// The sfnt magic for an Apple legacy TrueType face (`true`).
const MAGIC_TRUE: u32 = 0x7472_7565;
/// The sfnt magic for an OpenType/CFF face (`OTTO`).
const MAGIC_OPENTYPE: u32 = 0x4F54_544F;
/// The collection magic shared by TTC and OTC (`ttcf`).
const MAGIC_COLLECTION: u32 = 0x7474_6366;

/// Read the leading 32-bit big-endian sfnt tag, if the bytes are long enough.
///
/// The container kind is decided entirely by the first four bytes; nothing
/// beyond them is touched here.
fn leading_tag(bytes: &[u8]) -> Option<u32> {
    let head: [u8; 4] = bytes.get(0..4)?.try_into().ok()?;
    Some(u32::from_be_bytes(head))
}

/// Detect the sfnt container format of owned font bytes.
///
/// Returns `None` for a non-sfnt container — including WOFF/WOFF2, which the
/// framework never decodes (ADR 0028). The caller decompresses such a container
/// to sfnt before offering it here. `None` is a routing decision, not a parse
/// error: the bytes may still be a valid font the caller must unwrap first.
pub fn detect(bytes: &[u8]) -> Option<FontFormat> {
    match leading_tag(bytes)? {
        MAGIC_TRUETYPE | MAGIC_TRUE | MAGIC_OPENTYPE => Some(FontFormat::Sfnt),
        MAGIC_COLLECTION => Some(FontFormat::Collection),
        _ => None,
    }
}

/// The number of faces an sfnt container holds, or `None` if the bytes are not
/// a container the framework accepts.
///
/// A single face reports `1`; a collection reports the count from its `ttcf`
/// header. This is the enumeration the resolver drives to register every face
/// in a collection under its own [`crate::FontFaceId`] and face index. Reading
/// the collection count reuses `ttf-parser`, which owns the sfnt parsing this
/// subsystem prefers not to reimplement (section 3.7).
pub fn face_count(bytes: &[u8]) -> Option<u32> {
    match detect(bytes)? {
        FontFormat::Sfnt => Some(1),
        // A `ttcf` header whose count cannot be read is a malformed collection,
        // not a single face: report nothing rather than guess a face.
        FontFormat::Collection => ttf_parser::fonts_in_collection(bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real single TrueType face: the shared DejaVu subset used across the
    /// text tests. Its leading tag is `0x00010000`.
    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

    /// Build a minimal well-formed `ttcf` header wrapping `face_count` copies of
    /// `sfnt`, so `fonts_in_collection` reports `face_count`. The offset table
    /// points at the appended sfnt blobs; each face is the same real face, which
    /// is all the enumeration path needs.
    fn synth_collection(sfnt: &[u8], face_count: u32) -> Vec<u8> {
        let header_len = 12 + 4 * face_count as usize; // tag+version+count + offsets.
        let mut out = Vec::new();
        out.extend_from_slice(b"ttcf");
        out.extend_from_slice(&0x0002_0000u32.to_be_bytes()); // version 2.0.
        out.extend_from_slice(&face_count.to_be_bytes());
        // Every face points at the single appended sfnt blob right after the
        // offset table — a shared blob is legal and keeps the fixture small.
        let blob_offset = header_len as u32;
        for _ in 0..face_count {
            out.extend_from_slice(&blob_offset.to_be_bytes());
        }
        out.extend_from_slice(sfnt);
        out
    }

    #[test]
    fn detects_truetype_single_face() {
        // `0x00010000` — the DejaVu subset itself.
        assert_eq!(detect(DEJAVU), Some(FontFormat::Sfnt));
        assert_eq!(face_count(DEJAVU), Some(1));
    }

    #[test]
    fn detects_apple_true_magic_as_single_face() {
        let bytes = [0x74, 0x72, 0x75, 0x65]; // "true".
        assert_eq!(detect(&bytes), Some(FontFormat::Sfnt));
        assert_eq!(face_count(&bytes), Some(1));
    }

    #[test]
    fn detects_opentype_cff_magic_as_single_face() {
        let bytes = *b"OTTO";
        assert_eq!(detect(&bytes), Some(FontFormat::Sfnt));
        assert_eq!(face_count(&bytes), Some(1));
    }

    #[test]
    fn detects_collection_and_counts_its_faces() {
        let ttc = synth_collection(DEJAVU, 3);
        assert_eq!(detect(&ttc), Some(FontFormat::Collection));
        assert_eq!(face_count(&ttc), Some(3));
    }

    #[test]
    fn single_face_collection_wrapper_still_counts_one() {
        // A `ttcf` wrapper holding one face is a Collection by container kind,
        // distinct from a bare single face, and reports exactly one face.
        let ttc = synth_collection(DEJAVU, 1);
        assert_eq!(detect(&ttc), Some(FontFormat::Collection));
        assert_eq!(face_count(&ttc), Some(1));
    }

    #[test]
    fn woff2_and_foreign_containers_are_not_detected() {
        // `wOF2` — the framework never decodes WOFF2 (ADR 0028); it routes as a
        // non-sfnt container the caller must decompress first.
        assert_eq!(detect(b"wOF2\x00\x01\x00\x00"), None);
        // `wOFF` — WOFF1, likewise foreign.
        assert_eq!(detect(b"wOFF\x00\x01\x00\x00"), None);
        // Arbitrary non-font bytes.
        assert_eq!(detect(b"\x89PNG"), None);
        assert_eq!(face_count(b"wOF2\x00\x01\x00\x00"), None);
    }

    #[test]
    fn too_short_input_is_not_detected() {
        assert_eq!(detect(&[]), None);
        assert_eq!(detect(&[0x00, 0x01, 0x00]), None); // three bytes, no tag.
        assert_eq!(face_count(&[0x00, 0x01, 0x00]), None);
    }

    #[test]
    fn malformed_collection_header_reports_no_faces() {
        // Correct `ttcf` magic but truncated before the face count: detect sees
        // a Collection, but the count cannot be read, so no face is reported.
        let truncated = b"ttcf\x00\x02\x00\x00"; // tag + version, no count.
        assert_eq!(detect(truncated), Some(FontFormat::Collection));
        assert_eq!(face_count(truncated), None);
    }
}
