//! What a Windows executable carries besides its code: the icon Explorer,
//! the taskbar and its windows show, and the version its properties list,
//! as resources the linker puts in it. `link.exe` and `lld-link` take them
//! as a `.res` file; the GNU linker as a COFF object holding the same tree
//! in its `.rsrc` section. Both are the same bytes for the same inputs.

/// The sizes the icon is drawn at: each size the shell asks for at 100 %
/// to 400 % scale.
pub const ICON_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// One image of the icon: `size` pixels square, `rgba` its pixels row by
/// row from the top, and the same pixels as a PNG.
pub struct IconImage {
    pub size: u32,
    pub rgba: Vec<u8>,
    pub png: Vec<u8>,
}

/// What the executable's version resource says.
pub struct Version<'a> {
    /// Major, minor, patch and build.
    pub numbers: [u16; 4],
    /// The version as the project writes it.
    pub text: &'a str,
    /// The app's name, as the user sees it: Task Manager lists the
    /// executable by it.
    pub product: &'a str,
    /// Who makes it, when the project names its publisher.
    pub company: Option<&'a str>,
    /// Whether it is a debug build.
    pub debug: bool,
}

/// The processor a COFF object is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Machine {
    X86,
    X64,
    Arm64,
}

impl Machine {
    /// The machine of a Rust target's architecture, such as `x86_64`.
    pub fn of(arch: &str) -> Option<Machine> {
        Some(match arch {
            "x86" | "i586" | "i686" => Machine::X86,
            "x86_64" => Machine::X64,
            "aarch64" => Machine::Arm64,
            _ => return None,
        })
    }

    fn id(self) -> u16 {
        match self {
            Machine::X86 => 0x014c,
            Machine::X64 => 0x8664,
            Machine::Arm64 => 0xaa64,
        }
    }

    /// The relocation to an address relative to the image's base.
    fn image_relative(self) -> u16 {
        match self {
            Machine::X86 => 7,
            Machine::X64 => 3,
            Machine::Arm64 => 2,
        }
    }
}

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
/// US English, which every resource is filed under.
const LANGUAGE: u16 = 0x0409;
/// The images at least this large are kept as PNGs; smaller ones as
/// bitmaps, which every part of the shell reads.
const PNG_FROM: u32 = 256;

/// One resource: its type, its id and its data.
struct Resource {
    kind: u16,
    id: u16,
    data: Vec<u8>,
}

impl Resource {
    /// How it is loaded, as resource compilers mark it: moveable and
    /// discardable, the group and version pure as well.
    fn memory_flags(&self) -> u16 {
        match self.kind {
            RT_ICON => 0x1010,
            RT_GROUP_ICON => 0x1030,
            _ => 0x0030,
        }
    }
}

/// The executable's resources, by type and then id: each image of `icon`,
/// the group that lists them, and `version`.
fn resources(icon: &[IconImage], version: &Version<'_>) -> Vec<Resource> {
    let mut out = Vec::new();
    let mut group = Vec::with_capacity(6 + 14 * icon.len());
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&(icon.len() as u16).to_le_bytes());
    for (at, image) in icon.iter().enumerate() {
        let id = at as u16 + 1;
        let data = if image.size >= PNG_FROM {
            image.png.clone()
        } else {
            bitmap(image)
        };
        // 256 is written as 0.
        let side = if image.size >= 256 {
            0
        } else {
            image.size as u8
        };
        group.extend_from_slice(&[side, side, 0, 0]);
        group.extend_from_slice(&1u16.to_le_bytes());
        group.extend_from_slice(&32u16.to_le_bytes());
        group.extend_from_slice(&(data.len() as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
        out.push(Resource {
            kind: RT_ICON,
            id,
            data,
        });
    }
    if !icon.is_empty() {
        out.push(Resource {
            kind: RT_GROUP_ICON,
            id: 1,
            data: group,
        });
    }
    out.push(Resource {
        kind: RT_VERSION,
        id: 1,
        data: version_info(version),
    });
    out
}

/// `image` as an icon's bitmap: 32-bit BGRA rows from the bottom, then the
/// mask of its fully transparent pixels.
fn bitmap(image: &IconImage) -> Vec<u8> {
    let side = image.size as usize;
    let mask_row = side.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(40 + side * side * 4 + mask_row * side);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(side as i32).to_le_bytes());
    // The color rows and the mask's, stacked.
    out.extend_from_slice(&(2 * side as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&((side * side * 4 + mask_row * side) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    let rows: Vec<&[u8]> = image.rgba.chunks_exact(side * 4).collect();
    for row in rows.iter().rev() {
        for px in row.as_chunks::<4>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    for row in rows.iter().rev() {
        let mut mask = vec![0u8; mask_row];
        for (x, px) in row.as_chunks::<4>().0.iter().enumerate() {
            if px[3] == 0 {
                mask[x / 8] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&mask);
    }
    out
}

/// A version resource node: its key, its value — `text` ones counted in
/// UTF-16 units, others in bytes — and its children, each on a 4-byte
/// boundary.
fn node(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0; 6];
    for unit in key.encode_utf16().chain([0]) {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    align(&mut out, 4);
    out.extend_from_slice(value);
    for child in children {
        align(&mut out, 4);
        out.extend_from_slice(child);
    }
    let length = out.len() as u16;
    let value_length = if text { value.len() / 2 } else { value.len() } as u16;
    out[0..2].copy_from_slice(&length.to_le_bytes());
    out[2..4].copy_from_slice(&value_length.to_le_bytes());
    out[4..6].copy_from_slice(&u16::from(text).to_le_bytes());
    out
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// The `VS_VERSIONINFO` of `version`: the numbers Windows compares, and the
/// names and version its file properties show, in US English.
fn version_info(version: &Version<'_>) -> Vec<u8> {
    let [major, minor, patch, build] = version.numbers.map(u32::from);
    let mut fixed = Vec::with_capacity(52);
    for word in [
        0xfeef_04bd,
        0x0001_0000,
        (major << 16) | minor,
        (patch << 16) | build,
        (major << 16) | minor,
        (patch << 16) | build,
        // The flags that are set: only whether it is a debug build.
        0x3f,
        u32::from(version.debug),
        // Windows NT, an application.
        0x0004_0004,
        1,
        0,
        0,
        0,
    ] {
        fixed.extend_from_slice(&word.to_le_bytes());
    }
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", version.company),
        ("FileDescription", Some(version.product)),
        ("FileVersion", Some(version.text)),
        ("ProductName", Some(version.product)),
        ("ProductVersion", Some(version.text)),
    ]
    .iter()
    .filter_map(|(key, value)| Some(node(key, &utf16((*value)?), true, &[])))
    .collect();
    // US English in Unicode.
    let table = node("040904B0", &[], true, &strings);
    let file_info = node("StringFileInfo", &[], true, &[table]);
    let mut translation = Vec::new();
    translation.extend_from_slice(&LANGUAGE.to_le_bytes());
    translation.extend_from_slice(&1200u16.to_le_bytes());
    let var = node("Translation", &translation, false, &[]);
    let var_info = node("VarFileInfo", &[], true, &[var]);
    node("VS_VERSION_INFO", &fixed, false, &[file_info, var_info])
}

fn align(out: &mut Vec<u8>, to: usize) {
    out.resize(out.len().next_multiple_of(to), 0);
}

/// The resources of `icon` and `version` as a `.res` file, for `link.exe`
/// and `lld-link`.
pub fn res(icon: &[IconImage], version: &Version<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    // The empty resource every `.res` file starts with.
    header(&mut out, 0, 0, 0, 0, 0);
    for resource in resources(icon, version) {
        header(
            &mut out,
            resource.data.len() as u32,
            resource.kind,
            resource.id,
            resource.memory_flags(),
            LANGUAGE,
        );
        out.extend_from_slice(&resource.data);
        align(&mut out, 4);
    }
    out
}

fn header(out: &mut Vec<u8>, size: u32, kind: u16, id: u16, flags: u16, language: u16) {
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes());
    // The type and the name, each an id.
    for value in [0xffff, kind, 0xffff, id] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&language.to_le_bytes());
    out.extend_from_slice(&[0; 8]);
}

/// The resources of `icon` and `version` as a COFF object for `machine`,
/// for the GNU linker: one `.rsrc` section holding the resource tree — the
/// types, each one's ids, each id's language — then each resource's data,
/// which the tree points at through a relocation.
pub fn object(icon: &[IconImage], version: &Version<'_>, machine: Machine) -> Vec<u8> {
    let resources = resources(icon, version);
    let mut kinds: Vec<u16> = resources.iter().map(|r| r.kind).collect();
    kinds.dedup();
    let directory = |entries: usize| 16 + 8 * entries;
    let ids = |kind: u16| resources.iter().filter(|r| r.kind == kind).count();
    let root = directory(kinds.len());
    let types: usize = kinds.iter().map(|&kind| directory(ids(kind))).sum();
    let languages = directory(1) * resources.len();
    let entries = root + types + languages;
    let mut data_at = entries + 16 * resources.len();

    let mut section = Vec::new();
    let table = |section: &mut Vec<u8>, count: usize| {
        section.extend_from_slice(&[0; 14]);
        section.extend_from_slice(&(count as u16).to_le_bytes());
    };
    let entry = |section: &mut Vec<u8>, id: u16, at: usize, subdirectory: bool| {
        section.extend_from_slice(&u32::from(id).to_le_bytes());
        let flag = if subdirectory { 0x8000_0000 } else { 0 };
        section.extend_from_slice(&(at as u32 | flag).to_le_bytes());
    };
    table(&mut section, kinds.len());
    let mut at = root;
    for &kind in &kinds {
        entry(&mut section, kind, at, true);
        at += directory(ids(kind));
    }
    let mut language_at = root + types;
    for &kind in &kinds {
        table(&mut section, ids(kind));
        for resource in resources.iter().filter(|r| r.kind == kind) {
            entry(&mut section, resource.id, language_at, true);
            language_at += directory(1);
        }
    }
    for (n, _) in resources.iter().enumerate() {
        table(&mut section, 1);
        entry(&mut section, LANGUAGE, entries + 16 * n, false);
    }
    let mut relocations = Vec::new();
    for resource in &resources {
        relocations.push(section.len() as u32);
        // Its offset in the section, which the relocation makes its address.
        section.extend_from_slice(&(data_at as u32).to_le_bytes());
        section.extend_from_slice(&(resource.data.len() as u32).to_le_bytes());
        section.extend_from_slice(&[0; 8]);
        data_at = (data_at + resource.data.len()).next_multiple_of(8);
    }
    for resource in &resources {
        section.extend_from_slice(&resource.data);
        align(&mut section, 8);
    }

    const HEADERS: usize = 20 + 40;
    let symbols = HEADERS + section.len() + 10 * relocations.len();
    let mut out = Vec::with_capacity(symbols + 2 * 18 + 4);
    out.extend_from_slice(&machine.id().to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    // No time, so the same inputs give the same object.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(symbols as u32).to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);

    out.extend_from_slice(b".rsrc\0\0\0");
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&(section.len() as u32).to_le_bytes());
    out.extend_from_slice(&(HEADERS as u32).to_le_bytes());
    out.extend_from_slice(&((HEADERS + section.len()) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(relocations.len() as u16).to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    // Initialized data, aligned to 4 bytes, read and written.
    out.extend_from_slice(&0xc030_0040u32.to_le_bytes());

    out.extend_from_slice(&section);
    for at in &relocations {
        out.extend_from_slice(&at.to_le_bytes());
        // Against the section's own symbol.
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&machine.image_relative().to_le_bytes());
    }

    // The section's symbol and its definition.
    out.extend_from_slice(b".rsrc\0\0\0");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    // Static, with one auxiliary record.
    out.extend_from_slice(&[3, 1]);
    out.extend_from_slice(&(section.len() as u32).to_le_bytes());
    out.extend_from_slice(&(relocations.len() as u16).to_le_bytes());
    out.extend_from_slice(&[0; 12]);
    // An empty string table.
    out.extend_from_slice(&4u32.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version() -> Version<'static> {
        Version {
            numbers: [1, 2, 3, 42],
            text: "1.2.3",
            product: "My App",
            company: Some("Example Ltd"),
            debug: false,
        }
    }

    fn icon() -> Vec<IconImage> {
        [16, 256]
            .into_iter()
            .map(|size| {
                let mut rgba = vec![255; (size * size * 4) as usize];
                // The top left pixel is transparent.
                rgba[3] = 0;
                IconImage {
                    size,
                    rgba,
                    png: format!("png {size}").into_bytes(),
                }
            })
            .collect()
    }

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// Each resource of a `.res` file: its type, id, language and data.
    fn read_res(bytes: &[u8]) -> Vec<(u16, u16, u16, Vec<u8>)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let size = u32_at(bytes, at) as usize;
            let header = u32_at(bytes, at + 4) as usize;
            assert_eq!(header, 32);
            assert_eq!(u16_at(bytes, at + 8), 0xffff);
            assert_eq!(u16_at(bytes, at + 12), 0xffff);
            let data = bytes[at + header..at + header + size].to_vec();
            out.push((
                u16_at(bytes, at + 10),
                u16_at(bytes, at + 14),
                u16_at(bytes, at + 22),
                data,
            ));
            at = (at + header + size).next_multiple_of(4);
        }
        out
    }

    #[test]
    fn a_res_file_holds_each_icon_image_their_group_and_the_version() {
        let bytes = res(&icon(), &version());
        let read = read_res(&bytes);
        assert_eq!(read[0], (0, 0, 0, Vec::new()));
        let kinds: Vec<(u16, u16)> = read[1..].iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(kinds, [(3, 1), (3, 2), (14, 1), (16, 1)]);
        assert!(read[1..].iter().all(|r| r.2 == 0x0409));

        // The small image is a bitmap twice as tall as wide, with a mask
        // marking the transparent pixel; the large one is the PNG.
        let small = &read[1].3;
        assert_eq!(u32_at(small, 0), 40);
        assert_eq!(u32_at(small, 4), 16);
        assert_eq!(u32_at(small, 8), 32);
        assert_eq!(u16_at(small, 14), 32);
        assert_eq!(small.len(), 40 + 16 * 16 * 4 + 4 * 16);
        // The top row is the last of the colors and of the mask.
        let mask = 40 + 16 * 16 * 4;
        assert_eq!(small[mask + 15 * 4], 0x80);
        assert_eq!(small[mask], 0);
        assert_eq!(small[40 + 15 * 64 + 3], 0);
        assert_eq!(read[2].3, b"png 256");

        let group = &read[3].3;
        assert_eq!(u16_at(group, 2), 1);
        assert_eq!(u16_at(group, 4), 2);
        assert_eq!(&group[6..8], &[16, 16]);
        assert_eq!(u32_at(group, 6 + 8) as usize, small.len());
        assert_eq!(u16_at(group, 6 + 12), 1);
        assert_eq!(&group[20..22], &[0, 0]);
        assert_eq!(u16_at(group, 20 + 12), 2);

        assert_eq!(bytes, res(&icon(), &version()));
    }

    #[test]
    fn the_version_resource_carries_the_numbers_and_the_names() {
        let info = version_info(&version());
        assert_eq!(usize::from(u16_at(&info, 0)), info.len());
        assert_eq!(u16_at(&info, 2), 52);
        let key = utf16("VS_VERSION_INFO");
        assert_eq!(&info[6..6 + key.len()], key.as_slice());
        // The fixed part, after the key and its padding.
        let fixed = (6 + key.len()).next_multiple_of(4);
        assert_eq!(u32_at(&info, fixed), 0xfeef_04bd);
        assert_eq!(u32_at(&info, fixed + 8), (1 << 16) | 2);
        assert_eq!(u32_at(&info, fixed + 12), (3 << 16) | 42);
        let text = String::from_utf16_lossy(
            &info
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_le_bytes(c))
                .collect::<Vec<_>>(),
        );
        for part in [
            "StringFileInfo",
            "040904B0",
            "ProductName\0\0My App\0",
            "CompanyName\0\0Example Ltd\0",
            "FileVersion\0\0\x31.2.3\0",
            "VarFileInfo",
            "Translation",
        ] {
            let found = text.contains(part) || text.contains(&part.replace("\0\0", "\0"));
            assert!(found, "{part:?} in the version resource");
        }
    }

    #[test]
    fn the_object_points_its_tree_at_each_resource() {
        let object = object(&icon(), &version(), Machine::X64);
        assert_eq!(u16_at(&object, 0), 0x8664);
        assert_eq!(u16_at(&object, 2), 1);
        assert_eq!(&object[20..28], b".rsrc\0\0\0");
        let size = u32_at(&object, 20 + 16) as usize;
        let start = u32_at(&object, 20 + 20) as usize;
        let relocations = u32_at(&object, 20 + 24) as usize;
        let count = usize::from(u16_at(&object, 20 + 32));
        assert_eq!(count, 4);
        let section = &object[start..start + size];
        let symbols = u32_at(&object, 8) as usize;
        assert_eq!(symbols, relocations + 10 * count);
        assert_eq!(&object[symbols..symbols + 8], b".rsrc\0\0\0");

        // Root → type → id → language → data, for each resource.
        let resources = resources(&icon(), &version());
        let root_count = usize::from(u16_at(section, 14));
        assert_eq!(root_count, 3);
        let mut found = Vec::new();
        for t in 0..root_count {
            let kind = u32_at(section, 16 + 8 * t) as u16;
            let types = (u32_at(section, 16 + 8 * t + 4) & 0x7fff_ffff) as usize;
            for i in 0..usize::from(u16_at(section, types + 14)) {
                let id = u32_at(section, types + 16 + 8 * i) as u16;
                let ids = (u32_at(section, types + 16 + 8 * i + 4) & 0x7fff_ffff) as usize;
                assert_eq!(u32_at(section, ids + 16), 0x0409);
                let data_entry = u32_at(section, ids + 20) as usize;
                assert_eq!(data_entry & 0x8000_0000, 0);
                // A relocation at the entry, of an image-relative address.
                let relocated = (0..count).any(|r| {
                    let at = relocations + 10 * r;
                    u32_at(&object, at) as usize == data_entry && u16_at(&object, at + 8) == 3
                });
                assert!(relocated);
                let offset = u32_at(section, data_entry) as usize;
                let len = u32_at(section, data_entry + 4) as usize;
                found.push((kind, id, section[offset..offset + len].to_vec()));
            }
        }
        let expected: Vec<_> = resources
            .into_iter()
            .map(|r| (r.kind, r.id, r.data))
            .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn without_an_icon_only_the_version_is_linked_in() {
        let read = read_res(&res(&[], &version()));
        assert_eq!(read.len(), 2);
        assert_eq!((read[1].0, read[1].1), (16, 1));
    }
}
