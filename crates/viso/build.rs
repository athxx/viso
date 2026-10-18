//! Bakes the build's footprint profile in, and keeps the development session
//! out of shipped artifacts.
//!
//! The profile (`VISO_FOOTPRINT`), its budget overrides (`VISO_BUDGET`,
//! `key=size` pairs) and its worker count (`VISO_WORKERS`) come from
//! `viso build`; without them the profile follows the target and the backend
//! features. A profile whose backend features disagree with it fails the
//! build. An embedded profile draws with the app's packaged fonts alone
//! unless `system-fonts` is on (`cfg(viso_no_system_fonts)`); only such a
//! build may leave the glyph rasterizer (`rasterizer`) out.
//!
//! A release build whose images `viso build` converted names what no longer
//! needs decoding on the device: `VISO_IMAGES_CONVERTED` lists `raster` (every
//! PNG and JPEG is converted: no raster decoder links,
//! `cfg(viso_converted_rasters)`) and `svg` (every SVG is rasterized: no SVG
//! parser links, `cfg(viso_converted_svg)`).
//!
//! `hot-reload` links the compiler, the watcher and the dev channel, so a
//! build the CLI marks as a release or shipping artifact (`VISO_PROFILE`)
//! refuses the feature. Cargo's own profile is not the gate — an optimized
//! dev build (the `edit_to_pixels` measurement) is still a dev artifact.

use std::fmt::Write as _;

const BUDGET_KEYS: [&str; 6] = ["heap", "gpu", "faces", "shaping", "glyph_atlas", "images"];

fn feature(name: &str) -> bool {
    std::env::var_os(format!(
        "CARGO_FEATURE_{}",
        name.to_uppercase().replace('-', "_")
    ))
    .is_some()
}

fn fail(message: &str) -> ! {
    println!("cargo::error={message}");
    std::process::exit(0);
}

/// `48MiB`, `512KiB`, `4096` (bytes).
fn size(text: &str) -> Option<u64> {
    let text = text.trim();
    let (digits, unit) = text
        .find(|c: char| !c.is_ascii_digit())
        .map_or((text, ""), |at| text.split_at(at));
    let n: u64 = digits.parse().ok()?;
    let scale = match unit.trim() {
        "" | "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        _ => return None,
    };
    n.checked_mul(scale)
}

fn main() {
    for var in [
        "VISO_PROFILE",
        "VISO_FOOTPRINT",
        "VISO_BUDGET",
        "VISO_WORKERS",
        "VISO_IMAGES_CONVERTED",
    ] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    let profile = std::env::var("VISO_PROFILE").unwrap_or_default();
    if feature("hot-reload") && matches!(profile.as_str(), "release" | "shipping") {
        fail(&format!(
            "the `hot-reload` feature is development-only and cannot be part of a `{profile}` \
             artifact; build it without `--features viso/hot-reload`"
        ));
    }

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let footprint = match std::env::var("VISO_FOOTPRINT")
        .ok()
        .filter(|f| !f.is_empty())
    {
        Some(name) => name,
        None if feature("cpu") => "embedded-cpu".to_owned(),
        None if feature("gles") && feature("kms") => "embedded-gpu".to_owned(),
        None if os == "ios" || os == "android" => "mobile".to_owned(),
        None if arch == "wasm32" => "web".to_owned(),
        None => "desktop".to_owned(),
    };
    let variant = match footprint.as_str() {
        "desktop" => "Desktop",
        "mobile" => "Mobile",
        "web" => "Web",
        "embedded-gpu" => "EmbeddedGpu",
        "embedded-cpu" => "EmbeddedCpu",
        other => fail(&format!(
            "`{other}` is not a footprint profile; expected desktop, mobile, web, embedded-gpu or embedded-cpu"
        )),
    };
    let headless = feature("headless");
    match footprint.as_str() {
        "embedded-cpu" if !(feature("cpu") || headless) => fail(
            "the embedded-cpu profile draws on the CPU: build it with `viso/cpu` (and `viso/kms` for a display)",
        ),
        "embedded-cpu" if feature("gles") || feature("vulkan") => {
            fail("the embedded-cpu profile has no GPU backend: drop `viso/gles` / `viso/vulkan`")
        }
        "embedded-gpu" if !headless && !(feature("gles") || os == "linux" || os == "android") => {
            fail(
                "the embedded-gpu profile draws through GLES 3 or Vulkan on Linux: build it with `viso/gles`",
            )
        }
        "embedded-gpu" if feature("cpu") => {
            fail("the embedded-gpu profile draws on the GPU: drop `viso/cpu`")
        }
        _ => {}
    }

    // The platform's fonts back the app's own unless the build is headless (what
    // it draws depends on the project alone) or embedded without
    // `system-fonts`.
    println!("cargo::rustc-check-cfg=cfg(viso_no_system_fonts)");
    let system_fonts =
        !(headless || (footprint.starts_with("embedded") && !feature("system-fonts")));
    if !system_fonts {
        println!("cargo::rustc-cfg=viso_no_system_fonts");
    }
    // A platform font is never baked, so a build that draws one rasterizes.
    // Only the packaged fonts can draw without the rasterizer, and
    // `packaged_fonts!` holds the build to having baked every glyph.
    if system_fonts && !feature("rasterizer") {
        fail(
            "this build draws the platform's fonts, which takes the glyph rasterizer: enable \
             `viso/rasterizer` (an app depending on `viso` with `default-features = false` lists \
             it among its own default features); only an embedded build without `system-fonts` \
             draws from baked glyphs alone",
        );
    }

    println!("cargo::rustc-check-cfg=cfg(viso_converted_rasters, viso_converted_svg)");
    let converted = std::env::var("VISO_IMAGES_CONVERTED").unwrap_or_default();
    for kind in converted
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        match kind {
            "raster" => println!("cargo::rustc-cfg=viso_converted_rasters"),
            "svg" => println!("cargo::rustc-cfg=viso_converted_svg"),
            other => fail(&format!(
                "`{other}` in VISO_IMAGES_CONVERTED is not `raster` or `svg`"
            )),
        }
    }

    let mut budget: [Option<u64>; 6] = [None; 6];
    if let Ok(spec) = std::env::var("VISO_BUDGET") {
        for pair in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((key, value)) = pair.split_once('=') else {
                fail(&format!("`{pair}` in VISO_BUDGET is not `key=size`"));
            };
            let Some(slot) = BUDGET_KEYS.iter().position(|k| *k == key.trim()) else {
                fail(&format!(
                    "`{}` is not a budget; expected one of {}",
                    key.trim(),
                    BUDGET_KEYS.join(", ")
                ));
            };
            let Some(bytes) = size(value) else {
                fail(&format!(
                    "`{value}` is not a size (`48MiB`, `512KiB`, bytes)"
                ));
            };
            budget[slot] = Some(bytes);
        }
    }
    let workers = match std::env::var("VISO_WORKERS").ok().filter(|w| !w.is_empty()) {
        Some(w) => match w.parse::<usize>() {
            Ok(n) => format!("Some({n})"),
            Err(_) => fail(&format!("VISO_WORKERS `{w}` is not a thread count")),
        },
        None => "None".to_owned(),
    };

    let mut out = String::new();
    let _ = writeln!(
        out,
        "const BUILD_FOOTPRINT: Footprint = Footprint::{variant};"
    );
    let slots: Vec<String> = budget
        .iter()
        .map(|b| b.map_or("None".to_owned(), |v| format!("Some({v})")))
        .collect();
    let _ = writeln!(
        out,
        "const BUILD_BUDGET: [Option<u64>; 6] = [{}];",
        slots.join(", ")
    );
    let _ = writeln!(out, "const BUILD_WORKERS: Option<usize> = {workers};");
    let dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    std::fs::write(std::path::Path::new(&dir).join("footprint.rs"), out).expect("writes OUT_DIR");
}
