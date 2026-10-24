//! The images a release build converts to the texel format its target samples
//! (`viso_image::convert`), so the device reads them where they lie and never
//! decodes one.
//!
//! Each raster of the package's `assets/` — and each SVG when `images.svg =
//! "raster"` — is converted once per target and settings: the result is kept
//! under the generated sources by the hash of what made it, and copied into
//! the target's overlay directory, which [`OVERLAY_ENV`] names to the build.
//! An asset embeds its converted file from there (`viso_dsl::assets`); a file
//! is rewritten only when its bytes change, so an unchanged asset never
//! rebuilds the crate.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use viso_dsl::assets::{ASSET_DIR, AssetCatalog, AssetFormat, overlay_of};
use viso_image::codec::Effort;
use viso_image::convert::{Plan, Quality, Sampler};
use viso_project::{
    DeviceTarget, Footprint, ImageQuality, Images, Layout, PixelFormat, Profile, Project, Rotation,
    Target,
};
use viso_schema::resource::AssetBytes;

use super::RustPlan;

pub(crate) use viso_dsl::assets::OVERLAY_ENV;

/// The variable naming what the build converted every one of, which the
/// facade then links no decoder for: `raster`, `svg`.
pub(crate) const CONVERTED_ENV: &str = "VISO_IMAGES_CONVERTED";

/// Bumped whenever a converter's output changes for the same input, so a
/// cached result of an older one is never reused.
const CONVERTER_VERSION: u32 = 1;

/// What a build's conversion did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Converted {
    /// Images converted (or found converted from an earlier build).
    pub(crate) images: usize,
    /// Of those, the ones an earlier build had converted.
    pub(crate) cached: usize,
    /// Their source files' bytes.
    pub(crate) source_bytes: u64,
    /// Their converted files' bytes.
    pub(crate) converted_bytes: u64,
    /// Animated images, shipped as written: a texture holds one frame.
    pub(crate) animated: usize,
}

/// How a build for `target` — the board `device` describes, for a device
/// target — converts its images with `settings`, and the scale its SVGs
/// rasterize at when they do; `None` for a target that ships its images as
/// written: a browser's (whose GPU may sample no compressed format) and a
/// headless one's (whose snapshots compare the decoded files).
pub(crate) fn plan(
    target: Target,
    device: Option<&DeviceTarget>,
    settings: Images,
) -> Option<(Plan, Option<f32>)> {
    if !settings.converts() {
        return None;
    }
    let footprint = match (target, device) {
        (Target::Device(_), Some(device)) => device.profile.value,
        (Target::Host, _) => Footprint::Desktop,
        (Target::Ios | Target::Android, _) => Footprint::Mobile,
        _ => return None,
    };
    let sampler = match footprint {
        // Every desktop GPU samples BC.
        Footprint::Desktop => Sampler::Bc,
        // Every Apple GPU since the A8 and almost every Android one samples
        // ASTC; a device that does not decodes it on load.
        Footprint::Mobile => Sampler::Astc,
        // ETC2 is core in OpenGL ES 3 and sampled by the Vulkan boards'.
        Footprint::EmbeddedGpu => Sampler::Etc2,
        Footprint::EmbeddedCpu => Sampler::Cpu {
            rgb565: device
                .and_then(|d| d.display)
                .and_then(|d| d.format)
                .is_some_and(|f| f.value == PixelFormat::Rgb565),
        },
        Footprint::Web => return None,
    };
    let surface = device
        .filter(|_| settings.fits())
        .and_then(|d| d.display)
        .and_then(|display| {
            let (w, h) = display.size?;
            let turned = display
                .rotation
                .is_some_and(|r| matches!(r.value, Rotation::R90 | Rotation::R270));
            Some(if turned {
                [h.value, w.value]
            } else {
                [w.value, h.value]
            })
        });
    let quality = match settings.quality() {
        ImageQuality::Lossless => Quality::Lossless,
        ImageQuality::High => Quality::High,
        ImageQuality::Balanced => Quality::Balanced,
        ImageQuality::Compact => Quality::Compact,
    };
    let svg = settings.rasters_svg().then(|| {
        settings
            .svg_scale
            .map_or(if footprint.is_embedded() { 1.0 } else { 2.0 }, |scale| {
                scale.value.get() as f32
            })
    });
    let plan = Plan {
        sampler,
        quality,
        format: None,
        surface,
        effort: Effort::Thorough,
    };
    Some((plan, svg))
}

/// Converts the images of `project` for a `profile` build of `target` (the
/// board `device` describes, for a device target) into its overlay under
/// `layout`, and names the overlay to the build in `plan`'s environment;
/// `None` for a debug build or a target that ships its images as written.
///
/// What no longer needs a decoder is named too ([`CONVERTED_ENV`]): SVG when
/// SVGs rasterize, and on a board — whose app shows no image the build did
/// not see — every raster format, unless an animated image ships as written.
///
/// # Errors
///
/// An asset that cannot be read or does not decode, or a file that cannot be
/// written.
pub(crate) fn add_converted_images(
    plan: &mut RustPlan,
    target: Target,
    device: Option<&DeviceTarget>,
    profile: Profile,
    project: &Project,
    layout: &Layout,
) -> Result<Option<Converted>, String> {
    if profile == Profile::Dev {
        return Ok(None);
    }
    let settings = device.map_or(project.manifest.images, |d| {
        d.images.or(project.manifest.images)
    });
    let Some((conversion, svg)) = self::plan(target, device, settings) else {
        return Ok(None);
    };
    let dir = project.root.join(ASSET_DIR);
    let catalog =
        AssetCatalog::scan(&dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    let root = layout.generated().join("images");
    let name = match target {
        Target::Device(name) => name,
        other => other.as_str(),
    };
    let overlay = root.join(name);
    let converted = convert_catalog(&catalog, &conversion, svg, &root.join("cache"), &overlay)?;
    if converted.images > 0 {
        let overlay = overlay.canonicalize().unwrap_or(overlay);
        plan.env
            .push((OVERLAY_ENV.to_owned(), overlay.into_os_string()));
    }
    let board = device.is_some_and(|d| d.profile.value.is_embedded()) && converted.animated == 0;
    let kinds: Vec<&str> = [(board, "raster"), (svg.is_some(), "svg")]
        .into_iter()
        .filter_map(|(on, kind)| on.then_some(kind))
        .collect();
    if !kinds.is_empty() {
        plan.env
            .push((CONVERTED_ENV.to_owned(), kinds.join(",").into()));
    }
    Ok(Some(converted))
}

/// Converts every image of `catalog` as `conversion` says (each SVG at `svg`
/// times its size, or none), each result kept in `cache` by the hash of what
/// made it and written into `overlay`, from which every other file is
/// removed. An animated image is left out, so it embeds as written.
fn convert_catalog(
    catalog: &AssetCatalog,
    conversion: &Plan,
    svg: Option<f32>,
    cache: &Path,
    overlay: &Path,
) -> Result<Converted, String> {
    let into = overlay_of(overlay, catalog.dir());
    let mut converted = Converted::default();
    let mut written = BTreeSet::new();
    for (path, format) in catalog.files() {
        if format == AssetFormat::Svg && svg.is_none() {
            continue;
        }
        let source = catalog.dir().join(path);
        let bytes: Arc<[u8]> = fs::read(&source)
            .map_err(|e| format!("cannot read `{}`: {e}", source.display()))?
            .into();
        if format != AssetFormat::Svg && viso_image::is_animated(&AssetBytes::Shared(bytes.clone()))
        {
            converted.animated += 1;
            continue;
        }
        let key = cache_key(
            conversion,
            svg.filter(|_| format == AssetFormat::Svg),
            &bytes,
        );
        let cached = cache.join(&key);
        let out = match fs::read(&cached) {
            Ok(out) => {
                converted.cached += 1;
                out
            }
            Err(_) => {
                let image = conversion
                    .convert_asset(&bytes, svg)
                    .map_err(|e| format!("`assets/{path}` does not convert: {e}"))?;
                let out = viso_image::texture::write(&image);
                write_if_changed(&cached, &out)?;
                out
            }
        };
        converted.images += 1;
        converted.source_bytes += bytes.len() as u64;
        converted.converted_bytes += out.len() as u64;
        let file = into.join(path);
        write_if_changed(&file, &out)?;
        written.insert(file);
    }
    remove_others(overlay, &written)
        .map_err(|e| format!("cannot clean `{}`: {e}", overlay.display()))?;
    Ok(converted)
}

/// The cache name of `bytes` converted as `conversion` and `svg` say.
fn cache_key(conversion: &Plan, svg: Option<f32>, bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(CONVERTER_VERSION.to_le_bytes());
    hash.update(format!("{conversion:?} {svg:?}").as_bytes());
    hash.update(bytes);
    hash.finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Writes `bytes` to `path` unless it holds them already.
fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create `{}`: {e}", parent.display()))?;
    }
    fs::write(path, bytes).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

/// Removes every file under `dir` but `keep`.
fn remove_others(dir: &Path, keep: &BTreeSet<PathBuf>) -> std::io::Result<()> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let entries = match fs::read_dir(&at) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if !keep.contains(&path) {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_project::Manifest;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viso-images-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn device(toml: &str) -> DeviceTarget {
        let manifest = Manifest::parse("Viso.toml", toml).ok().expect("parses");
        manifest.targets.devices[0].clone()
    }

    #[test]
    fn each_target_samples_its_family() {
        let none = Images::default();
        let sampler = |target, device| plan(target, device, none).map(|(p, _)| p.sampler);
        assert_eq!(sampler(Target::Host, None), Some(Sampler::Bc));
        assert_eq!(sampler(Target::Ios, None), Some(Sampler::Astc));
        assert_eq!(sampler(Target::Android, None), Some(Sampler::Astc));
        assert_eq!(sampler(Target::WebGpu, None), None);
        assert_eq!(sampler(Target::Headless, None), None);
        let board = device("[package]\nname = \"a\"\n[target.board]\nprofile = \"embedded-gpu\"\n");
        assert_eq!(
            sampler(Target::Device("board"), Some(&board)),
            Some(Sampler::Etc2)
        );
        let panel = device(
            "[package]\nname = \"a\"\n[target.panel]\nprofile = \"embedded-cpu\"\n\
             display = { width = 800, height = 480, format = \"rgb565\", rotation = 90 }\n\
             images = { quality = \"compact\", svg = \"raster\" }\n",
        );
        let (p, svg) = plan(Target::Device("panel"), Some(&panel), panel.images).expect("converts");
        assert_eq!(p.sampler, Sampler::Cpu { rgb565: true });
        assert_eq!(p.surface, Some([480, 800]), "turned a quarter");
        assert_eq!(p.quality, Quality::Compact);
        assert_eq!(svg, Some(1.0));
    }

    #[test]
    fn a_catalog_converts_once_and_its_overlay_holds_only_its_stills() {
        let dir = scratch("catalog");
        let assets = dir.join("assets");
        fs::create_dir_all(assets.join("icons")).unwrap();
        fs::write(
            assets.join("photo.jpg"),
            include_bytes!("../../../../crates/image/src/fixtures/two_tone.jpg"),
        )
        .unwrap();
        fs::write(
            assets.join("icons/dot.svg"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\">\
             <circle cx=\"4\" cy=\"4\" r=\"3\" fill=\"#0a0\"/></svg>",
        )
        .unwrap();
        fs::write(
            assets.join("spinner.gif"),
            include_bytes!("../../../../crates/image/tests/fixtures/anim.gif"),
        )
        .unwrap();
        let catalog = AssetCatalog::scan(&assets).unwrap();
        let conversion = Plan {
            sampler: Sampler::Cpu { rgb565: true },
            quality: Quality::High,
            format: None,
            surface: None,
            effort: Effort::Fast,
        };
        let (cache, overlay) = (dir.join("cache"), dir.join("overlay"));
        let first = convert_catalog(&catalog, &conversion, Some(2.0), &cache, &overlay).unwrap();
        assert_eq!((first.images, first.cached, first.animated), (2, 0, 1));
        let into = overlay_of(&overlay, &assets);
        // A texture holds one frame: the animation embeds as written.
        assert!(!into.join("spinner.gif").exists());
        let svg = fs::read(into.join("icons/dot.svg")).unwrap();
        assert!(viso_image::texture::is_texture(&svg));
        let image = viso_image::decode_raster(&svg).unwrap();
        assert_eq!(
            (image.width, image.height, image.natural),
            (16, 16, [8.0, 8.0])
        );

        // Again: every result from the cache. Vector SVGs: the rasterized one
        // leaves the overlay, so the source embeds.
        let second = convert_catalog(&catalog, &conversion, None, &cache, &overlay).unwrap();
        assert_eq!((second.images, second.cached), (1, 1));
        assert!(!into.join("icons/dot.svg").exists());
        assert!(into.join("photo.jpg").is_file());
        let _ = fs::remove_dir_all(&dir);
    }
}
