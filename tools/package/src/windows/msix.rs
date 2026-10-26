//! The MSIX a Windows app installs from: its files, the manifest naming the
//! app, its publisher and what it may use, and the block map Windows checks
//! each file against, in a zip laid out as the app package format asks —
//! every 64 KiB block of a compressed file deflated on its own, so it can be
//! checked and fetched alone. `signtool` signs it as it does an executable.

use std::path::Path;

use flate2::{Compress, Compression, FlushCompress, Status};
use sha2::{Digest, Sha256};

use crate::Access;
use crate::archive::{self, DEFLATED, Entry, Host, LOCAL_HEADER, Packed, STORED};
use crate::io as at;

/// The logos the manifest names, each `size` pixels square: the Start
/// menu's and taskbar's, the Start tile's, and the Store's.
pub const LOGOS: [(&str, u32); 3] = [
    ("Assets/Square44x44Logo.png", 44),
    ("Assets/Square150x150Logo.png", 150),
    ("Assets/StoreLogo.png", 50),
];

/// The oldest Windows that installs it: Windows 10 1809, the first to
/// install MSIX packages.
const MIN_VERSION: &str = "10.0.17763.0";
/// The newest Windows it is known to run on.
const MAX_VERSION_TESTED: &str = "10.0.26100.0";
/// The size of the blocks the block map hashes.
const BLOCK: usize = 64 * 1024;
/// What a publisher's name says to install an unsigned package: Windows
/// installs one only when it carries this.
const UNSIGNED: &str = "OID.2.25.311729368913984317654407730594956997722=1";

/// The package's identity and what its manifest says of the app.
pub struct Manifest<'a> {
    /// The package name: letters, digits, `.` and `-`.
    pub name: &'a str,
    /// The publisher: the subject of the certificate it is signed with.
    pub publisher: &'a str,
    /// The publisher's name, as the installer shows it.
    pub publisher_name: &'a str,
    /// Major, minor, build and revision.
    pub version: [u16; 4],
    /// The processor: `x64`, `arm64` or `x86`.
    pub architecture: &'a str,
    pub display_name: &'a str,
    /// The executable, at the package's root.
    pub executable: &'a str,
    /// What the app asks to use.
    pub access: &'a [Access],
    /// The URL schemes that open the app.
    pub schemes: &'a [String],
}

/// The processor architecture a package for Rust's `arch` names.
pub fn architecture(arch: &str) -> Option<&'static str> {
    Some(match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" | "i586" | "i686" => "x86",
        _ => return None,
    })
}

/// A subject whose common name is `name`, for a publisher the project does
/// not name.
pub fn subject(name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|c| !matches!(c, ',' | '+' | '=' | '"' | '<' | '>' | '#' | ';'))
        .collect();
    let name = name.trim();
    format!("CN={}", if name.is_empty() { "App" } else { name })
}

/// The publisher of a package that is not signed: `subject`, marked as
/// unsigned.
pub fn unsigned(subject: &str) -> String {
    format!("{subject}, {UNSIGNED}")
}

/// The name `subject` gives its holder: its organization, else its common
/// name.
pub fn holder(subject: &str) -> Option<String> {
    let parts = parts(subject);
    ["O", "CN"].iter().find_map(|key| {
        parts.iter().find_map(|part| {
            let (k, v) = part.split_once('=')?;
            (k.trim() == *key).then(|| v.trim().trim_matches('"').to_owned())
        })
    })
}

/// The `KEY=value` parts of `subject`, split at the commas outside quotes.
fn parts(subject: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (at, c) in subject.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(&subject[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&subject[start..]);
    parts
}

/// Whether `name` is a package name: 3 to 50 letters, digits, `.` and `-`.
pub fn valid_name(name: &str) -> bool {
    (3..=50).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

/// `AppxManifest.xml` for `manifest`: a full-trust desktop app whose tiles
/// are the [`LOGOS`], started with a URL of one of its schemes as its
/// argument.
pub fn manifest(manifest: &Manifest<'_>) -> String {
    let [major, minor, build, revision] = manifest.version;
    let mut extensions = String::new();
    if !manifest.schemes.is_empty() {
        extensions.push_str("\n      <Extensions>");
        for scheme in manifest.schemes {
            extensions.push_str(&format!(
                "\n        <uap3:Extension Category=\"windows.protocol\">\
                 \n          <uap3:Protocol Name=\"{}\" Parameters=\"&quot;%1&quot;\"/>\
                 \n        </uap3:Extension>",
                xml(scheme)
            ));
        }
        extensions.push_str("\n      </Extensions>");
    }
    let mut capabilities = String::new();
    let mut devices = String::new();
    // The foundation's capability comes before the others, as the schema
    // orders them.
    if manifest.access.contains(&Access::Network) {
        capabilities.push_str("    <Capability Name=\"internetClient\"/>\n");
    }
    for access in manifest.access {
        match access {
            Access::Photos => {
                capabilities.push_str("    <uap:Capability Name=\"picturesLibrary\"/>\n")
            }
            Access::Contacts => capabilities.push_str("    <uap:Capability Name=\"contacts\"/>\n"),
            Access::Camera => devices.push_str("    <DeviceCapability Name=\"webcam\"/>\n"),
            Access::Microphone => devices.push_str("    <DeviceCapability Name=\"microphone\"/>\n"),
            Access::Location => devices.push_str("    <DeviceCapability Name=\"location\"/>\n"),
            Access::Bluetooth => devices.push_str("    <DeviceCapability Name=\"bluetooth\"/>\n"),
            // A desktop app shows notifications and asks Windows Hello
            // without a capability.
            Access::Notifications | Access::Biometrics | Access::Network => {}
        }
    }
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
  xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
  xmlns:uap3="http://schemas.microsoft.com/appx/manifest/uap/windows10/3"
  xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
  IgnorableNamespaces="uap uap3 rescap">
  <Identity Name="{name}" Publisher="{publisher}" Version="{major}.{minor}.{build}.{revision}" ProcessorArchitecture="{architecture}"/>
  <Properties>
    <DisplayName>{display}</DisplayName>
    <PublisherDisplayName>{publisher_name}</PublisherDisplayName>
    <Logo>{store}</Logo>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="{MIN_VERSION}" MaxVersionTested="{MAX_VERSION_TESTED}"/>
  </Dependencies>
  <Resources>
    <Resource Language="en-us"/>
  </Resources>
  <Applications>
    <Application Id="App" Executable="{executable}" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements DisplayName="{display}" Description="{display}" BackgroundColor="transparent" Square150x150Logo="{tile}" Square44x44Logo="{small}"/>{extensions}
    </Application>
  </Applications>
  <Capabilities>
{capabilities}    <rescap:Capability Name="runFullTrust"/>
{devices}  </Capabilities>
</Package>
"#,
        name = xml(manifest.name),
        publisher = xml(manifest.publisher),
        architecture = manifest.architecture,
        display = xml(manifest.display_name),
        publisher_name = xml(manifest.publisher_name),
        executable = xml(manifest.executable),
        small = windows_path(LOGOS[0].0),
        tile = windows_path(LOGOS[1].0),
        store = windows_path(LOGOS[2].0),
    )
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn windows_path(path: &str) -> String {
    path.replace('/', "\\")
}

/// Whether a file of `name` is compressed: what is compressed already is
/// stored as it is.
fn compressed(name: &str) -> bool {
    let encoded = ["png", "jpg", "jpeg", "gif", "webp", "avif"];
    !name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| encoded.contains(&ext))
}

/// Its MIME type, by extension.
fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/x-icon",
        Some("svg") => "image/svg+xml",
        Some("exe" | "dll") => "application/x-msdownload",
        _ => "application/octet-stream",
    }
}

/// Writes the package of `files` — `/`-separated paths and contents — and
/// `manifest` as `out`.
pub fn write(files: &[Entry], manifest: &str, out: &Path) -> Result<(), String> {
    let bytes = package(files, manifest).map_err(|e| e.to_string())?;
    at(out, std::fs::write(out, bytes))
}

/// The package's bytes: `files` in name order, the manifest, the block map
/// of both, and the content types of every part.
pub fn package(files: &[Entry], manifest: &str) -> std::io::Result<Vec<u8>> {
    let mut files: Vec<&Entry> = files.iter().collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let manifest = Entry::file("AppxManifest.xml", manifest.as_bytes().to_vec());

    let mut map = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <BlockMap xmlns=\"http://schemas.microsoft.com/appx/2010/blockmap\" \
         HashMethod=\"http://www.w3.org/2001/04/xmlenc#sha256\">\n",
    );
    let mut types = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n",
    );
    let mut defaults: Vec<String> = Vec::new();
    let mut packed = Vec::with_capacity(files.len() + 3);
    for entry in files.into_iter().chain([&manifest]) {
        let compress = compressed(&entry.name);
        let (data, sizes) = if compress {
            blocks(&entry.data)?
        } else {
            (entry.data.clone(), Vec::new())
        };
        map.push_str(&format!(
            "<File Name=\"{}\" Size=\"{}\" LfhSize=\"{}\">\n",
            xml(&windows_path(&entry.name)),
            entry.data.len(),
            LOCAL_HEADER + entry.name.len(),
        ));
        for (n, block) in entry.data.chunks(BLOCK).enumerate() {
            let hash = base64(&Sha256::digest(block));
            match sizes.get(n) {
                Some(size) => map.push_str(&format!("<Block Hash=\"{hash}\" Size=\"{size}\"/>\n")),
                None => map.push_str(&format!("<Block Hash=\"{hash}\"/>\n")),
            }
        }
        map.push_str("</File>\n");
        if entry.name == manifest.name {
            types.push_str(
                "<Override ContentType=\"application/vnd.ms-appx.manifest+xml\" \
                 PartName=\"/AppxManifest.xml\"/>\n",
            );
        } else if let Some((_, ext)) = entry.name.rsplit_once('.')
            && !defaults.iter().any(|d| d == ext)
        {
            types.push_str(&format!(
                "<Default ContentType=\"{}\" Extension=\"{}\"/>\n",
                content_type(&entry.name),
                xml(ext)
            ));
            defaults.push(ext.to_owned());
        }
        packed.push((entry, compress, data));
    }
    map.push_str("</BlockMap>\n");
    types.push_str(
        "<Override ContentType=\"application/vnd.ms-appx.blockmap+xml\" \
         PartName=\"/AppxBlockMap.xml\"/>\n</Types>\n",
    );
    let map = Entry::file("AppxBlockMap.xml", map.into_bytes());
    let types = Entry::file("[Content_Types].xml", types.into_bytes());
    let mut parts: Vec<Packed<'_>> = packed
        .into_iter()
        .map(|(entry, compress, data)| Packed {
            entry,
            method: if compress { DEFLATED } else { STORED },
            data,
        })
        .collect();
    for entry in [&map, &types] {
        let (data, _) = blocks(&entry.data)?;
        parts.push(Packed {
            entry,
            method: DEFLATED,
            data,
        });
    }
    archive::zip_packed(&parts, Host::Dos)
}

/// `data` deflated a block at a time, each block flushed so it inflates on
/// its own, then the stream's end; with the compressed size of each block.
fn blocks(data: &[u8]) -> std::io::Result<(Vec<u8>, Vec<usize>)> {
    let mut z = Compress::new(Compression::best(), false);
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    let mut sizes = Vec::with_capacity(data.len().div_ceil(BLOCK));
    for block in data.chunks(BLOCK) {
        let start = out.len();
        deflate(&mut z, block, FlushCompress::Full, &mut out)?;
        sizes.push(out.len() - start);
    }
    deflate(&mut z, &[], FlushCompress::Finish, &mut out)?;
    Ok((out, sizes))
}

fn deflate(
    z: &mut Compress,
    mut input: &[u8],
    flush: FlushCompress,
    out: &mut Vec<u8>,
) -> std::io::Result<()> {
    loop {
        out.reserve(input.len() + 1024);
        let before = z.total_in();
        let status = z
            .compress_vec(input, out, flush)
            .map_err(std::io::Error::other)?;
        input = &input[(z.total_in() - before) as usize..];
        let room = out.len() < out.capacity();
        let done = match flush {
            FlushCompress::Finish => status == Status::StreamEnd,
            _ => input.is_empty() && room,
        };
        if done {
            return Ok(());
        }
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use flate2::read::DeflateDecoder;

    use super::*;

    fn sample() -> Manifest<'static> {
        Manifest {
            name: "com.example.app",
            publisher: "CN=Example & Co",
            publisher_name: "Example",
            version: [1, 2, 3, 0],
            architecture: "x64",
            display_name: "My <App>",
            executable: "my-app.exe",
            access: &[Access::Camera, Access::Photos, Access::Notifications],
            schemes: &[],
        }
    }

    #[test]
    fn the_manifest_names_the_app_its_publisher_and_what_it_uses() {
        let text = manifest(&sample());
        assert!(text.contains(
            "<Identity Name=\"com.example.app\" Publisher=\"CN=Example &amp; Co\" \
             Version=\"1.2.3.0\" ProcessorArchitecture=\"x64\"/>"
        ));
        assert!(text.contains("<DisplayName>My &lt;App&gt;</DisplayName>"));
        assert!(text.contains("Executable=\"my-app.exe\""));
        assert!(text.contains("Square44x44Logo=\"Assets\\Square44x44Logo.png\""));
        assert!(text.contains("<Logo>Assets\\StoreLogo.png</Logo>"));
        // Capabilities, then device capabilities, as the schema orders them.
        let pictures = text.find("picturesLibrary").unwrap();
        let trust = text.find("runFullTrust").unwrap();
        let webcam = text.find("webcam").unwrap();
        assert!(pictures < trust && trust < webcam);
        let networked = Manifest {
            access: &[Access::Photos, Access::Network],
            ..sample()
        };
        let text = manifest(&networked);
        let internet = text.find("<Capability Name=\"internetClient\"/>").unwrap();
        assert!(internet < text.find("picturesLibrary").unwrap());
        assert!(!text.contains("<Extensions>"));

        let schemes = ["notes".to_owned(), "x-notes".to_owned()];
        let linked = Manifest {
            schemes: &schemes,
            ..sample()
        };
        let text = manifest(&linked);
        assert!(text.contains(
            "Square44x44Logo=\"Assets\\Square44x44Logo.png\"/>
      <Extensions>
        <uap3:Extension Category=\"windows.protocol\">
          <uap3:Protocol Name=\"notes\" Parameters=\"&quot;%1&quot;\"/>
        </uap3:Extension>
        <uap3:Extension Category=\"windows.protocol\">
          <uap3:Protocol Name=\"x-notes\" Parameters=\"&quot;%1&quot;\"/>
        </uap3:Extension>
      </Extensions>
    </Application>"
        ));
    }

    #[test]
    fn an_unsigned_publisher_is_marked_and_a_name_checked() {
        assert_eq!(
            unsigned(&subject("My, App")),
            "CN=My App, OID.2.25.311729368913984317654407730594956997722=1"
        );
        assert_eq!(
            holder("CN=Jo, O=\"Example, Ltd\", C=US").as_deref(),
            Some("Example, Ltd")
        );
        assert_eq!(holder("CN=Jo").as_deref(), Some("Jo"));
        assert!(valid_name("com.example.app"));
        assert!(!valid_name("ab"));
        assert!(!valid_name("my_app"));
    }

    #[test]
    fn base64_pads_its_last_group() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }

    #[test]
    fn every_block_inflates_alone_and_matches_its_hash() {
        // Three blocks of noise and text, the last short.
        let mut exe = Vec::new();
        let mut x: u32 = 1;
        while exe.len() < 2 * BLOCK + 1000 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            exe.push((x >> 16) as u8);
            exe.extend_from_slice(b"text text ");
        }
        let files = vec![
            Entry::file("my-app.exe", exe.clone()),
            Entry::file("Assets/StoreLogo.png", b"\x89PNG logo".to_vec()),
        ];
        let bytes = package(&files, &manifest(&sample())).unwrap();
        assert_eq!(bytes, package(&files, &manifest(&sample())).unwrap());

        let entries = archive::unzip(&bytes).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Assets/StoreLogo.png",
                "my-app.exe",
                "AppxManifest.xml",
                "AppxBlockMap.xml",
                "[Content_Types].xml"
            ]
        );
        assert_eq!(entries[1].data, exe);
        assert!(entries[0].stored && !entries[1].stored);

        let map = String::from_utf8(entries[3].data.clone()).unwrap();
        assert!(map.contains("<File Name=\"Assets\\StoreLogo.png\" Size=\"9\" LfhSize=\"50\">"));
        let exe_map = map.split("<File Name=\"my-app.exe\"").nth(1).unwrap();
        let exe_map = &exe_map[..exe_map.find("</File>").unwrap()];
        let sizes: Vec<usize> = exe_map
            .split("<Block ")
            .skip(1)
            .map(|b| {
                let s = &b[b.find("Size=\"").unwrap() + 6..];
                s[..s.find('"').unwrap()].parse().unwrap()
            })
            .collect();
        assert_eq!(sizes.len(), 3);

        // The deflated data of the executable, from its local header.
        let at = bytes.windows(10).position(|w| w == b"my-app.exe").unwrap() + 10;
        let mut offset = at;
        for (n, size) in sizes.iter().enumerate() {
            let mut block = Vec::new();
            DeflateDecoder::new(&bytes[offset..offset + size])
                .read_to_end(&mut block)
                .ok();
            let expected = &exe[n * BLOCK..((n + 1) * BLOCK).min(exe.len())];
            assert_eq!(block, expected, "block {n} inflates alone");
            let hash = base64(&Sha256::digest(expected));
            assert!(exe_map.contains(&hash));
            offset += size;
        }

        let types = String::from_utf8(entries[4].data.clone()).unwrap();
        assert!(types.contains("<Default ContentType=\"image/png\" Extension=\"png\"/>"));
        assert!(types.contains("Extension=\"exe\""));
        assert!(types.contains("PartName=\"/AppxManifest.xml\""));
        assert!(types.contains("PartName=\"/AppxBlockMap.xml\""));
    }
}
