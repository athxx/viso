//! The Android component sets Viso is tested with: for each supported API
//! level, its platform and Google APIs system images, beside the
//! platform-tools, build-tools, NDK and emulator every level shares. Each
//! archive is pinned by the size and SHA-1 the Android repository publishes
//! for it, so a download is the archive this table names or nothing.

use viso_package::android::Abi;

/// Where the SDK packages are published.
const REPOSITORY: &str = "https://dl.google.com/android/repository/";
/// Where the Google APIs system images are published.
const GOOGLE_APIS: &str = "https://dl.google.com/android/repository/sys-img/google_apis/";

/// A host the SDK publishes tools for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Host {
    MacArm,
    MacX64,
    LinuxX64,
    WindowsX64,
}

impl Host {
    /// This machine, when the SDK has tools for it.
    pub(crate) fn current() -> Option<Host> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => Some(Host::MacArm),
            ("macos", "x86_64") => Some(Host::MacX64),
            ("linux", "x86_64") => Some(Host::LinuxX64),
            ("windows", "x86_64") => Some(Host::WindowsX64),
            _ => None,
        }
    }
}

/// The hosts an archive is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hosts {
    /// Every host: a platform or a system image.
    Any,
    /// Both Macs.
    Mac,
    One(Host),
}

impl Hosts {
    fn include(self, host: Host) -> bool {
        match self {
            Hosts::Any => true,
            Hosts::Mac => matches!(host, Host::MacArm | Host::MacX64),
            Hosts::One(one) => one == host,
        }
    }
}

/// One archive of a component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Archive {
    pub hosts: Hosts,
    /// Its file name in the component's repository.
    pub file: &'static str,
    pub size: u64,
    pub sha1: &'static str,
}

/// An SDK package, as the repository names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Component {
    /// Its path: `build-tools;36.0.0`; the directory it installs to is the
    /// path with `/` for `;`.
    pub path: &'static str,
    pub revision: &'static str,
    /// What it is, for a reader.
    pub title: &'static str,
    /// The repository its archives are under.
    pub repository: &'static str,
    /// The license its archives are under.
    pub license: License,
    /// The paths under the archive's top directory that are kept, each a
    /// file or a directory; empty keeps everything.
    pub keep: &'static [&'static str],
    pub archives: &'static [Archive],
}

impl Component {
    /// Its archive for `host`.
    pub(crate) fn archive(&self, host: Host) -> Option<&'static Archive> {
        self.archives.iter().find(|a| a.hosts.include(host))
    }

    /// Its directory under the SDK root.
    pub(crate) fn dir(&self) -> String {
        self.path.replace(';', "/")
    }

    /// The URL of `archive`.
    pub(crate) fn url(&self, archive: &Archive) -> String {
        format!("{}{}", self.repository, archive.file)
    }

    /// Whether the archive's path `relative` (under its top directory) is
    /// kept.
    pub(crate) fn keeps(&self, relative: &str) -> bool {
        self.keep.is_empty()
            || self.keep.iter().any(|kept| {
                relative == *kept
                    || relative
                        .strip_prefix(kept)
                        .is_some_and(|rest| rest.starts_with('/') || kept.ends_with('/'))
            })
    }
}

/// A license an archive is published under, and the manifest that holds its
/// text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct License {
    pub id: &'static str,
    pub manifest: &'static str,
}

pub(crate) const SDK_LICENSE: License = License {
    id: "android-sdk-license",
    manifest: "https://dl.google.com/android/repository/repository2-3.xml",
};

/// The license of the Arm system images.
pub(crate) const ARM_LICENSE: License = License {
    id: "android-sdk-arm-dbt-license",
    manifest: "https://dl.google.com/android/repository/sys-img/google_apis/sys-img2-3.xml",
};

/// A supported API level and what it adds to the shared components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Api {
    pub level: u32,
    /// The Android release it is.
    pub android: &'static str,
    pub platform: Component,
    pub arm64: Component,
    pub x86_64: Component,
}

impl Api {
    /// Its system image for `abi`.
    pub(crate) fn image(&self, abi: Abi) -> &Component {
        match abi {
            Abi::Arm64 => &self.arm64,
            Abi::X86_64 => &self.x86_64,
        }
    }

    /// Every component its development set needs on a host running `abi`.
    pub(crate) fn components(&self, abi: Abi) -> Vec<&Component> {
        SHARED
            .iter()
            .chain([&self.platform, self.image(abi)])
            .collect()
    }

    /// `google_apis/arm64-v8a`.
    pub(crate) fn image_name(&self, abi: Abi) -> String {
        format!("google_apis/{}", abi.as_str())
    }
}

/// The level `viso android use` picks with none chosen yet.
pub(crate) const DEFAULT_API: u32 = 36;

/// The supported level `level`.
pub(crate) fn api(level: u32) -> Option<&'static Api> {
    APIS.iter().find(|api| api.level == level)
}

const fn sdk(
    path: &'static str,
    revision: &'static str,
    title: &'static str,
    keep: &'static [&'static str],
    archives: &'static [Archive],
) -> Component {
    Component {
        path,
        revision,
        title,
        repository: REPOSITORY,
        license: SDK_LICENSE,
        keep,
        archives,
    }
}

const fn archive(hosts: Hosts, file: &'static str, size: u64, sha1: &'static str) -> Archive {
    Archive {
        hosts,
        file,
        size,
        sha1,
    }
}

const fn image(
    path: &'static str,
    revision: &'static str,
    abi: Abi,
    archives: &'static [Archive],
) -> Component {
    Component {
        path,
        revision,
        title: "system image",
        repository: GOOGLE_APIS,
        license: match abi {
            Abi::Arm64 => ARM_LICENSE,
            Abi::X86_64 => SDK_LICENSE,
        },
        keep: &[],
        archives,
    }
}

use Host::{LinuxX64, MacArm, MacX64, WindowsX64};

/// What every API level shares.
pub(crate) const SHARED: [Component; 4] = [
    sdk(
        "platform-tools",
        "37.0.1",
        "platform-tools (adb)",
        &[],
        &[
            archive(
                Hosts::Mac,
                "platform-tools_r37.0.1-darwin.zip",
                16_110_554,
                "6ae73f4de6452dc57e62ec02b68eed92a4c21661",
            ),
            archive(
                Hosts::One(LinuxX64),
                "platform-tools_r37.0.1-linux.zip",
                9_054_187,
                "477254aa5f903c15cf51001717bdf347fb6b53e0",
            ),
            archive(
                Hosts::One(WindowsX64),
                "platform-tools_r37.0.1-win.zip",
                8_044_989,
                "e03e78b1d80b396f1c3358e31251cb31740e1110",
            ),
        ],
    ),
    sdk(
        "build-tools;36.0.0",
        "36.0.0",
        "build-tools",
        &[],
        &[
            archive(
                Hosts::Mac,
                "build-tools_r36_macosx.zip",
                79_121_749,
                "199ae0047ee61e842f8ee0c6d3918e44fb9a1f83",
            ),
            archive(
                Hosts::One(LinuxX64),
                "build-tools_r36_linux.zip",
                63_737_259,
                "b0b6376977657e8ad9b969bacf4093601da2c6fb",
            ),
            archive(
                Hosts::One(WindowsX64),
                "build-tools_r36_windows.zip",
                58_699_878,
                "f16ccffd34de8790dede813a6c7d8e2c11a27b50",
            ),
        ],
    ),
    // Only the LLVM toolchain a Rust build links with: its compilers, linker,
    // archiver and sysroot.
    sdk(
        "ndk;28.2.13676358",
        "28.2.13676358",
        "NDK",
        &["source.properties", "toolchains/llvm/prebuilt"],
        &[
            archive(
                Hosts::Mac,
                "android-ndk-r28c-darwin.zip",
                952_495_160,
                "fc20a6bf15a30fb3428c9b60a7308793a362dc6d",
            ),
            archive(
                Hosts::One(LinuxX64),
                "android-ndk-r28c-linux.zip",
                722_261_334,
                "a7b54a5de87fecd125a17d54f73c446199e72a64",
            ),
            archive(
                Hosts::One(WindowsX64),
                "android-ndk-r28c-windows.zip",
                748_118_221,
                "086bba43ff2f5eb0e387b15c8278bb4e0d89ba1d",
            ),
        ],
    ),
    sdk(
        "emulator",
        "37.2.12",
        "emulator",
        &[],
        &[
            archive(
                Hosts::One(MacArm),
                "emulator-darwin_aarch64-16428233.zip",
                416_112_708,
                "3af4fe44ce82b3d88ae5678a53735f27ad729c15",
            ),
            archive(
                Hosts::One(MacX64),
                "emulator-darwin_x64-16428233.zip",
                488_731_099,
                "0c08fc22f41f2e63976f5503d593f821ff11e30b",
            ),
            archive(
                Hosts::One(LinuxX64),
                "emulator-linux_x64-16428233.zip",
                349_654_171,
                "cd7362ea55dfb86a418958138dc396e74165dd01",
            ),
            archive(
                Hosts::One(WindowsX64),
                "emulator-windows_x64-16428233.zip",
                455_342_868,
                "488ed747e82de7e9bb5247becd1ac043c7e5e85d",
            ),
        ],
    ),
];

/// The `android.jar` an APK compiles and links against.
const PLATFORM_KEEP: &[&str] = &["source.properties", "android.jar"];

/// The supported API levels, newest first.
pub(crate) const APIS: [Api; 3] = [
    Api {
        level: 36,
        android: "16",
        platform: sdk(
            "platforms;android-36",
            "2",
            "platform",
            PLATFORM_KEEP,
            &[archive(
                Hosts::Any,
                "platform-36_r02.zip",
                65_878_410,
                "2c1a80dd4d9f7d0e6dd336ec603d9b5c55a6f576",
            )],
        ),
        arm64: image(
            "system-images;android-36;google_apis;arm64-v8a",
            "7",
            Abi::Arm64,
            &[archive(
                Hosts::Any,
                "arm64-v8a-36_r07.zip",
                1_872_691_175,
                "5a99183b6d924da606260e45fd41a3eb8eca6eb7",
            )],
        ),
        x86_64: image(
            "system-images;android-36;google_apis;x86_64",
            "7",
            Abi::X86_64,
            &[archive(
                Hosts::Any,
                "x86_64-36_r07.zip",
                1_895_447_397,
                "c6bf44bdcd885bb902b4ba752d111a073ad7a817",
            )],
        ),
    },
    Api {
        level: 35,
        android: "15",
        platform: sdk(
            "platforms;android-35",
            "2",
            "platform",
            PLATFORM_KEEP,
            &[archive(
                Hosts::Any,
                "platform-35_r02.zip",
                64_273_788,
                "0bb560a90a7a2cbd0dd8348224d518b638fe7949",
            )],
        ),
        arm64: image(
            "system-images;android-35;google_apis;arm64-v8a",
            "9",
            Abi::Arm64,
            &[archive(
                Hosts::Any,
                "arm64-v8a-35_r09.zip",
                1_778_933_980,
                "16f5bceca236b2737008977c4aaf826e46a8de7d",
            )],
        ),
        x86_64: image(
            "system-images;android-35;google_apis;x86_64",
            "9",
            Abi::X86_64,
            &[archive(
                Hosts::Any,
                "x86_64-35_r09.zip",
                1_738_815_903,
                "0103e6dab21290c4b9d16550a3ce99476f884eef",
            )],
        ),
    },
    Api {
        level: 34,
        android: "14",
        platform: sdk(
            "platforms;android-34",
            "3",
            "platform",
            PLATFORM_KEEP,
            &[archive(
                Hosts::Any,
                "platform-34-ext7_r03.zip",
                63_180_081,
                "1f2e9478d6a7601425ceaa553311dc43191f103d",
            )],
        ),
        arm64: image(
            "system-images;android-34;google_apis;arm64-v8a",
            "14",
            Abi::Arm64,
            &[archive(
                Hosts::Any,
                "arm64-v8a-34_r14.zip",
                1_610_393_229,
                "2fe8b46d419a3400e30f31b0152b241b50c8b99f",
            )],
        ),
        x86_64: image(
            "system-images;android-34;google_apis;x86_64",
            "14",
            Abi::X86_64,
            &[archive(
                Hosts::Any,
                "x86_64-34_r14.zip",
                1_563_721_130,
                "e0f6c9a0691aa27bd597d0deb1bcfdc943ac8ca7",
            )],
        ),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_host_has_an_archive_of_every_component() {
        for host in [MacArm, MacX64, LinuxX64, WindowsX64] {
            for api in &APIS {
                for abi in [Abi::Arm64, Abi::X86_64] {
                    for component in api.components(abi) {
                        let archive = component
                            .archive(host)
                            .unwrap_or_else(|| panic!("{} on {host:?}", component.path));
                        assert_eq!(archive.sha1.len(), 40, "{}", archive.file);
                        assert!(archive.sha1.bytes().all(|b| b.is_ascii_hexdigit()));
                    }
                }
            }
        }
        assert!(api(DEFAULT_API).is_some());
        assert!(APIS.windows(2).all(|w| w[0].level > w[1].level));
    }

    #[test]
    fn a_component_installs_under_its_path_keeping_what_a_build_needs() {
        let ndk = &SHARED[2];
        assert_eq!(ndk.dir(), "ndk/28.2.13676358");
        assert!(ndk.keeps("toolchains/llvm/prebuilt/darwin-x86_64/bin/clang"));
        assert!(ndk.keeps("source.properties"));
        assert!(!ndk.keeps("sources/cxx-stl/llvm-libc++/include/vector"));
        assert!(!ndk.keeps("toolchains/llvm/prebuilt-extra"));
        assert!(SHARED[0].keeps("anything"));
        let image = APIS[0].image(Abi::Arm64);
        assert_eq!(
            image.dir(),
            "system-images/android-36/google_apis/arm64-v8a"
        );
        assert_eq!(image.license, ARM_LICENSE);
        assert_eq!(
            image.url(&image.archives[0]),
            "https://dl.google.com/android/repository/sys-img/google_apis/arm64-v8a-36_r07.zip"
        );
    }
}
