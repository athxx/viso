//! Which backend an Android build draws with, as `cfg(viso_gpu = …)`: the
//! build's `VISO_GPU`, which `viso build` sets from `[target.android] gpu`.
//!
//! - `auto` (the default): Vulkan where the device opens it, else GLES,
//!   both compiled in;
//! - `vulkan` or `gles`: that one alone.
//!
//! The `gles` feature makes GLES the backend as on Linux. A headless or
//! CPU-drawn build, and one without Vulkan, chooses nothing here.

use std::env;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(viso_gpu, values(\"auto\", \"vulkan\", \"gles\"))");
    println!("cargo::rerun-if-env-changed=VISO_GPU");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    let feature = |name: &str| env::var_os(format!("CARGO_FEATURE_{name}")).is_some();
    if feature("HEADLESS") || feature("CPU") {
        return;
    }
    let asked = env::var("VISO_GPU").unwrap_or_default();
    let asked = match asked.trim() {
        "" | "auto" => None,
        "vulkan" => Some("vulkan"),
        "gles" => Some("gles"),
        other => panic!("VISO_GPU is `{other}`: it is `auto`, `vulkan` or `gles`"),
    };
    let vulkan = feature("NATIVE_VULKAN") || feature("VULKAN");
    let api = if feature("GLES") {
        if asked == Some("vulkan") {
            panic!("VISO_GPU is `vulkan`, but the `gles` feature makes GLES the backend");
        }
        "gles"
    } else if vulkan {
        asked.unwrap_or("auto")
    } else {
        return;
    };
    println!("cargo::rustc-cfg=viso_gpu=\"{api}\"");
}
