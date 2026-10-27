//! The wire format byte for byte: what each write lays down, pinned so a
//! change of layout is seen, and the same on every target. CI runs these on
//! a 32-bit target too, where `usize` is narrower than the counts and
//! lengths on the wire.

use viso_ende::{
    Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag, SectionWriter, Sections,
    content_hash,
};

/// One of every scalar write, after a protocol tag.
fn scalars() -> Vec<u8> {
    let mut enc = Encoder::new();
    ProtocolTag::current().encode(&mut enc);
    enc.write_bool(true);
    enc.write_u8(0xab);
    enc.write_i8(-2);
    enc.write_u16(0x1234);
    enc.write_u32(0xdead_beef);
    enc.write_u64(0x0102_0304_0506_0708);
    enc.write_i16(-2);
    enc.write_i32(-1);
    enc.write_i64(i64::MIN);
    enc.write_f32(1.5);
    enc.write_f64(-0.25);
    enc.write_varint(300);
    enc.write_varint(u64::MAX);
    enc.write_varint_signed(-3);
    enc.write_str("é");
    enc.write_bytes(&[]);
    enc.into_bytes()
}

#[rustfmt::skip]
const SCALARS: &[u8] = &[
    b'V', b'E', 0x01, 0x00,
    0x01,
    0xab,
    0xfe,
    0x34, 0x12,
    0xef, 0xbe, 0xad, 0xde,
    0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
    0xfe, 0xff,
    0xff, 0xff, 0xff, 0xff,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80,
    0x00, 0x00, 0xc0, 0x3f,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xd0, 0xbf,
    0xac, 0x02,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
    0x05,
    0x02, 0xc3, 0xa9,
    0x00,
];

#[test]
fn every_scalar_lays_down_its_pinned_bytes_and_reads_back() {
    assert_eq!(scalars(), SCALARS);
    let mut dec = Decoder::new(SCALARS);
    assert_eq!(ProtocolTag::decode(&mut dec), Ok(ProtocolTag::current()));
    assert_eq!(dec.read_bool(), Ok(true));
    assert_eq!(dec.read_u8(), Ok(0xab));
    assert_eq!(dec.read_i8(), Ok(-2));
    assert_eq!(dec.read_u16(), Ok(0x1234));
    assert_eq!(dec.read_u32(), Ok(0xdead_beef));
    assert_eq!(dec.read_u64(), Ok(0x0102_0304_0506_0708));
    assert_eq!(dec.read_i16(), Ok(-2));
    assert_eq!(dec.read_i32(), Ok(-1));
    assert_eq!(dec.read_i64(), Ok(i64::MIN));
    assert_eq!(dec.read_f32(), Ok(1.5));
    assert_eq!(dec.read_f64(), Ok(-0.25));
    assert_eq!(dec.read_varint(), Ok(300));
    assert_eq!(dec.read_varint(), Ok(u64::MAX));
    assert_eq!(dec.read_varint_signed(), Ok(-3));
    assert_eq!(dec.read_str(), Ok("é"));
    assert_eq!(dec.read_bytes(), Ok(&[][..]));
    assert_eq!(dec.finish(), Ok(()));
}

#[rustfmt::skip]
const SECTIONS: &[u8] = &[
    0x02, 0x00, 0x00, 0x00,
    0x20, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x28, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x11,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x33, 0x22,
];

#[test]
fn a_section_table_lays_its_sections_on_pinned_aligned_offsets() {
    let mut enc = Encoder::new();
    let mut table = SectionWriter::new(&mut enc, 2);
    table.section(&mut enc, |enc| {
        enc.write_u8(0x11);
        1
    });
    table.section(&mut enc, |enc| {
        enc.write_u16(0x2233);
        1
    });
    table.finish();
    assert_eq!(enc.as_bytes(), SECTIONS);

    let mut dec = Decoder::new(SECTIONS);
    let sections = Sections::read(&mut dec, 2).expect("the table reads");
    assert_eq!(sections.len(), 2);
    let (first, second) = (sections.get(0), sections.get(1));
    assert_eq!(
        (first.offset, first.bytes, first.items),
        (32, &[0x11][..], 1)
    );
    assert_eq!(
        (second.offset, second.bytes, second.items),
        (40, &[0x33, 0x22][..], 1)
    );
}

#[test]
fn the_content_hash_is_pinned() {
    assert_eq!(content_hash(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(content_hash(b"viso"), 0xfe76_dc00_c6e1_6152);
}

/// A length or count past `u32::MAX` is an error, whatever `usize` is: a
/// 32-bit reader must not truncate it to a small one.
#[test]
fn a_length_past_u32_fails_rather_than_wrapping() {
    let mut enc = Encoder::new();
    enc.write_varint(u64::from(u32::MAX) + 2);
    enc.write_u8(0);
    let bytes = enc.into_bytes();
    assert!(Decoder::new(&bytes).read_bytes().is_err());
    assert!(Decoder::new(&bytes).read_count(usize::MAX).is_err());
    let mut dec = Decoder::new(&bytes);
    assert!(dec.read_list(usize::MAX, |d| d.read_u8()).is_err());
}

#[test]
fn a_foreign_preamble_is_malformed_at_its_offset() {
    let mut enc = Encoder::new();
    enc.write_u8(7);
    enc.write_raw(b"XY\x01\x00");
    let bytes = enc.into_bytes();
    let mut dec = Decoder::new(&bytes);
    dec.read_u8().expect("the lead byte");
    assert_eq!(
        ProtocolTag::decode(&mut dec),
        Err(DecodeError::Malformed { offset: 1 })
    );
}
