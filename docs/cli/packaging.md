# Building and packaging

## `viso build`

```bash
viso build                             # for this machine, dev profile
viso build --release                   # the release profile
viso build ios                         # an iOS Simulator .app
viso build android                     # an APK for the emulator
viso build web-gpu --profile shipping  # the smallest web bundle, precompressed
viso build headless                    # for tests and tooling
```

`viso build` builds and stops: nothing is launched. The output goes to
`dist/<platform>/` with an `artifact.json` beside it:

```text
Built headless (dev) in 4.3 s
  binary     dist/headless/my_app         25.7 MiB
  manifest   dist/headless/artifact.json  341 B
```

`dist/` has one directory per thing you can ship: `macos/`, `windows/` or `linux/`
for this machine, then `headless/`, `ios/`, `android/` and `web/`. Builds are cached
by everything that goes into them; an unchanged build is reused and reported as
`unchanged`.

### Profiles

| Profile | For | `opt_level` | `strip` | Dev runtime |
|---|---|---|---|---|
| `dev` | `viso run`, day-to-day builds | `0` | no | yes |
| `release` | trying a release build | `3` | no | no |
| `shipping` | what you ship; `viso package`'s default | `s` (size) | yes | no |

```bash
viso build --profile release
viso build --release          # the same
```

A profile's settings come from `[profile.<name>]` in `Viso.toml`, over its defaults:

```toml
[profile.release]
source_maps = true

[profile.shipping]
opt_level = 3
```

Run `viso config show --profile shipping` to see what a profile resolves to and why.

## `viso package`

`viso package` builds the app as its platform ships it: optimized, with its name,
version and icon, signed.

```bash
viso package                   # this machine: macOS, Windows or Linux
viso package ios               # an .ipa
viso package android           # an APK and an app bundle
viso package web-gpu           # a site directory and a zip
viso package --dry-run         # say what it would make, build nothing
```

It builds the `shipping` profile by default (`--profile release` is allowed;
`dev` is not — a package never contains the dev runtime).

| Platform | What it writes in `dist/<platform>/` |
|---|---|
| macOS | `<Display Name>.app`, `<name>-<version>-<arch>.dmg` (with an `/Applications` link) |
| Windows | `<name>-<version>-<arch>.zip`, and with an icon `<name>-<version>-<arch>.msix` |
| Linux | `<name>-<version>-<arch>.tar.gz`: the executable, a `.desktop` entry, a 512 px icon |
| iOS | `<name>-<version>.ipa`, for devices, its icon compiled into an asset catalog |
| Android | `<name>-<version>.apk` and `.aab`, one library per ABI |
| Web | `site/` and `<name>-<version>-<target>.zip` |

Archives are written by Viso itself: the same files give the same bytes.

### Describe the app

`viso package` reads `[package]` in `Viso.toml`:

```toml
[package]
name = "notes"
display_name = "Notes"          # the name under the icon; `name` by default
bundle_id = "com.example.notes" # needed on macOS, iOS and Android
version = "1.0.0"               # needed everywhere
build = 42                      # iOS CFBundleVersion, Android versionCode; 1 by default
orientation = "portrait"        # any, portrait or landscape

[package.icons]
source = "assets/icon.png"      # a square PNG of at least 1024 px

[package.permissions]           # what the app asks for, and the reason the system shows
camera = "Scans receipts into notes"
microphone = "Records voice notes"
```

Permissions are `camera`, `microphone`, `location`, `photos`, `contacts`,
`bluetooth`, `notifications`, `biometrics` and `network`. Each becomes what the
platform needs: Info.plist usage strings and entitlements on Apple platforms,
`<uses-permission>` on Android, capabilities in a Windows MSIX. `biometrics` is Face
ID's reason on iOS (`NSFaceIDUsageDescription`) and `USE_BIOMETRIC` on Android;
Touch ID and Windows Hello ask without one.

### What linked packages declare

A package the app links — a plugin such as `viso-push`, or the app itself — can
carry what it needs declared, beside its code. Linking it is then all the app does:

```text
viso-push/
├── android/java/…            Java compiled into the APK
├── android/manifest.xml      joins the Android manifest
├── ios/Info.plist            merges into the iOS Info.plist
├── ios/entitlements.plist    entitlements the iOS app is signed with
├── macos/Info.plist          merges into the macOS Info.plist
├── macos/entitlements.plist  entitlements the macOS app is signed with
└── web/worker.js             runs in the app's service worker
```

`android/manifest.xml` is a `<manifest>` that names only its namespaces. Its
elements (`<uses-permission>`, `<uses-feature>`, `<queries>`, …) join the app's,
and the children of its `<application>` (services, receivers, providers, activities,
`<meta-data>`) join the app's `<application>`; `{package}` reads as the app's id:

```xml
<manifest xmlns:android="http://schemas.android.com/apk/res/android">
    <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
    <application>
        <service
            android:name="dev.viso.background.VisoJobService"
            android:permission="android.permission.BIND_JOB_SERVICE"
            android:exported="false" />
    </application>
</manifest>
```

A plist fragment is a `<plist>` holding one `<dict>`, merged key by key: arrays are
united (each item once), dictionaries merge, and any other value must agree with
what is there. The app's own files come last, so it adds what only it knows — the
ids of its background tasks:

```xml
<!-- the app's ios/Info.plist -->
<plist version="1.0">
<dict>
    <key>BGTaskSchedulerPermittedIdentifiers</key>
    <array>
        <string>com.example.notes.sync</string>
    </array>
</dict>
</plist>
```

What needs a reason the user reads comes from `[package.permissions]` instead.
The same declaration made twice is made once; a permission asked for by two
packages is asked for at its widest (`android:maxSdkVersion` dropped when one asks
on every level). Two different declarations of one component, or a fragment changing
a key Viso writes from `Viso.toml`, is an error naming both files:

```text
error[C0014]: `UIRequiresFullScreen` is true in `viso-x`'s `ios/Info.plist` but false in what Viso writes from `Viso.toml`
  --> viso-x/ios/Info.plist:5:2
 = note: arrays and dictionaries merge, other values must agree: give the key in one place
```

A fragment that does not read is `C0013`. Both are checked before anything is
built, by `viso build`, `viso run` and `viso package`.

Entitlements: a simulator app has them, and the associated domains of
`[package.links]`, linked into its executable (`__TEXT,__entitlements` and
`__TEXT,__ents_der`), where the simulator reads them; its ad-hoc signature carries
none, since the Mac it runs on refuses to start one that claims what only a
provisioning profile allows. A device app's provisioning profile must allow each key a fragment asks for — `viso package`
fails naming the package and the capability to enable — and its own value is the one
signed, so `aps-environment` is the profile's `development` or `production`. A macOS
package embeds no provisioning profile, so the keys only one allows
(`com.apple.developer.*`) are left out of its signature with a warning naming the
package.

When a package the app links carries `web/worker.js`, the web bundle has a service
worker: `sw.js`, each package's script in turn, which the page registers before it
loads the app. The worker keeps its name, so a browser updates it in place. Without
one, a bundle has no worker.

A change in any of these files, or in the Java of any package the app links, makes
the next `viso build` package the app again.

### Links that open the app

```toml
[package.links]
schemes = ["notes"]             # notes://… opens the app
hosts = ["notes.example.com"]   # https://notes.example.com/… opens it on iOS and Android
```

A scheme is lowercase, without `://`, 2 to 39 characters, and not one the system or
the browser owns (`http`, `https`, `file`, `mailto`, …). A host is a lowercase DNS
name, `*.` taking every subdomain, with no scheme, port or path. Each becomes what the
platform reads:

| Platform | Schemes | Hosts |
|---|---|---|
| macOS, iOS | `CFBundleURLTypes` in Info.plist | iOS: the `com.apple.developer.associated-domains` entitlement (`applinks:<host>`), which the provisioning profile must allow; macOS: not declared (it needs a profile a Developer ID app does not carry) |
| Android | a `VIEW`/`BROWSABLE` intent filter per scheme | an `autoVerify` intent filter for `https` and the hosts |
| Windows | a `windows.protocol` extension in the MSIX, the URL passed as the argument | — |
| Linux | `MimeType=x-scheme-handler/<scheme>;` in the `.desktop` entry, `Exec=… %u` | — |

The site of each host must vouch for the app: `/.well-known/apple-app-site-association`
for iOS, `/.well-known/assetlinks.json` for Android. Development builds (`viso run`,
`viso build`) declare the same links, so a simulator or emulator opens them too, and
every build names the schemes to the app (`VISO_URL_SCHEMES`), which takes a URL of
one of them from its arguments on Windows and Linux.

The app receives each link in `Application::on_open_url`, with the launch window's
handler context:

```rust
fn on_open_url(&mut self, cx: &mut viso::ui::EventCx<'_>, url: &str) {
    if let Some(id) = url.strip_prefix("notes://open/") {
        // write state, spawn a task, open a window
    }
}
```

It is called for the link that started the app and for every one that arrives while
it runs, in order; one that arrives before the window is built waits for it. On
Windows and Linux the OS starts the program again for each link, so a start with a
link first hands it to the instance already running (a message-only window on
Windows, a socket in `$XDG_RUNTIME_DIR` on Linux) and ends. A Windows build not
installed from an MSIX registers itself under `HKCU\Software\Classes\<scheme>` when
it starts, so it opens its links too. On the web the page URL is the link when it
has a fragment, at start and on each `hashchange`.

Every icon size is rendered from the one source, scaled in linear light. It is all
checked before anything is built, each problem at its line in `Viso.toml`:

```text
error[C0006]: `package.version` is required to package
 = note: add `version = "1.0.0"` under `[package]`

error[C0006]: `package.bundle_id` is required to package for macOS
 = note: add `bundle_id = "com.example.app"` under `[package]`

warning[C0006]: no `[package.icons] source`: the app shows the system's default icon
 = note: add `[package.icons]` with `source = "assets/icon.png"`, a square PNG of at least 1024 pixels
```

### Signing

```bash
viso package --signing auto        # the default
viso package --signing required    # only with a real identity
viso package --signing off
```

`auto` signs with the identity you configured or one it finds; short of one, it
signs as far as the platform allows — ad hoc on macOS, the debug keystore on Android,
not at all on iOS and Windows — and warns `PACKAGE_UNSIGNED`. `required` fails with
`PACKAGE_SIGNING` (exit 6) instead. An identity you named that cannot be found is an
error under `auto` too: Viso never quietly signs with something else.

| Platform | Identity | `auto` without one |
|---|---|---|
| macOS | `[package.macos] identity`, else the keychain's first `Developer ID Application:`; hardened runtime, entitlements from the permissions and the linked packages' `macos/entitlements.plist`, timestamped; the `.dmg` is signed too | ad hoc (runs on this Mac only) |
| iOS | `[package.ios] identity` (else Apple Distribution, then Development, from the keychain) and `provisioning_profile`, whose entitlements it is signed with | not signed |
| Android | `[package.android] keystore` and `key_alias`; passwords from the environment | the debug keystore |
| Windows | `[package.windows] identity`, a certificate in the system store, with `signtool` | not signed |
| Linux, web | — | — |

```toml
[package.macos]
identity = "Developer ID Application: Example (ABCDE12345)"

[package.ios]
provisioning_profile = "signing/notes.mobileprovision"
team_id = "ABCDE12345"

[package.android]
keystore = "signing/release.jks"
key_alias = "upload"
abis = ["arm64-v8a", "armeabi-v7a"]   # arm64-v8a, armeabi-v7a, x86_64, x86

[package.windows]
identity = "Example Ltd"                           # subject name or SHA-1 thumbprint
timestamp_url = "http://timestamp.example.com"
publisher = "CN=Example Ltd, O=Example Ltd, C=US"  # the certificate's subject
```

**Secrets never go in `Viso.toml` or on the command line.** Keys stay in the
keychain or certificate store. The Android keystore's password is read from
`VISO_ANDROID_KEYSTORE_PASSWORD` (and `VISO_ANDROID_KEY_PASSWORD` when the key's
differs) by the signing tools themselves:

```bash
export VISO_ANDROID_KEYSTORE_PASSWORD=…   # from your CI's secret store
viso package android --signing required
```

Logs, `--verbose` output and `artifact.json` name identities, never secrets.

### Windows

On Windows the executable carries its icon (16 to 256 px) and its version — the
numbers Explorer and installers compare, and the names Task Manager and the file's
properties show — linked in when it is built. Windows numbers versions with four
numbers of at most 65535 each, so `version` and `build` must fit.

With an icon, `viso package` also writes an MSIX: the executable, its tiles drawn
from the icon, and a manifest naming the app, its publisher and the capabilities its
permissions need. The MSIX's name is `bundle_id`, or `name` when there is none. A
signed MSIX needs `publisher` set to the signing certificate's exact subject;
Windows refuses to install one whose publisher and signature differ. An unsigned
MSIX installs on a development machine with:

```powershell
Add-AppxPackage -AllowUnsigned .\notes-1.0.0-x86_64.msix
```

### Check before you build

```bash
viso package android --dry-run
```

prints the metadata, the signing identity chosen, the steps and the files it would
write — and validates them, so a missing identity or file fails here rather than
after a long build:

```text
warning[PACKAGE_UNSIGNED]: the package is signed with the debug keystore: a store refuses it
 = note: name the release keystore in `[package.android] keystore` and `key_alias`

Package android (shipping)
  id           com.example.myapp
  name         My App
  version      1.0.0 (3)
  orientation  any
  permissions  camera
  signing      signed with the debug keystore
  steps        1. compile for arm64-v8a
               2. pack and align my_app-1.0.0.apk
               3. lay out the app bundle my_app-1.0.0.aab
               4. sign both with the debug keystore
  output       dist/android/my_app-1.0.0.apk
               dist/android/my_app-1.0.0.aab
```

With `--json` it is one `result` event with the same fields.

### `artifact.json`

Every build and package writes a manifest beside its output:

```json
{"project":"notes","version":"1.0.0","build":42,"target":"macos","profile":"shipping",
 "build_id":"…","toolchain":"rustc 1.99.1 (…)",
 "signing":{"policy":"auto","status":"signed","identity":"Developer ID Application: Example (ABCDE12345)"},
 "artifacts":[{"kind":"app-bundle","path":"Notes.app","size":…,"hash":"sha256:…"},
              {"kind":"dmg","path":"notes-1.0.0-aarch64.dmg","size":…,"hash":"sha256:…"}]}
```

`signing.status` is `none`, `ad-hoc`, `debug-keystore` or `signed`. Publish it with
your release so anyone can check what they downloaded.
