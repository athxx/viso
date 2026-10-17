# Mobile: iOS and Android

During development a mobile app runs on a **simulator or emulator** that Viso
manages. There is no signing, provisioning or physical device in the dev loop, and
you never have to run `sdkmanager`, `avdmanager` or `simctl` yourself. Shipping to
devices and stores is [`viso package`](packaging.md).

```bash
viso run ios
viso run android
```

## Android

### Set up the SDK

```bash
viso android list                  # the supported API levels, what is installed, the default
viso android use 36                # install API 36's tested set and make it the default
viso android doctor                # check the SDK, emulator, acceleration and profiles
```

```text
API   ANDROID   STATUS       SYSTEM IMAGE            DEFAULT
36    16        installed    google_apis/arm64-v8a   *
35    15        available    google_apis/arm64-v8a
34    14        available    google_apis/arm64-v8a
```

`viso android use` downloads a fixed, tested set of components for that API level —
platform tools, build tools, the NDK, the emulator, the platform and a system image
for your machine's architecture — each checked against its published size and hash.
It asks you to accept the Android SDK licenses first; in scripts, pass
`--accept-licenses`. With `--offline` it only switches to a level already installed.

When `ANDROID_HOME` or `ANDROID_SDK_ROOT` names an SDK you already have, Viso uses
that one instead of its own; `viso android doctor` says which it found and where.

### Emulator profiles

A profile is a named virtual device:

```bash
viso android emulator create pixel                       # a phone on the default API level
viso android emulator create tab --preset tablet --api 35
viso android emulator list
viso android emulator start pixel --no-window            # boot it without a window
viso android emulator stop pixel
viso android emulator delete tab
```

Profile IDs are lowercase letters, digits and `-`. The ABI and system image follow
from your machine and the API level; the GPU backend is always Vulkan.

### `adb`

```bash
viso android adb devices
viso android adb --device pixel shell getprop
viso android adb --device pixel logcat
viso android adb -- shell settings list global   # everything after -- goes to adb as is
```

`viso android adb` runs the SDK's own `adb`, aimed at a profile's running emulator.

## iOS

`viso ios` works on macOS with Xcode installed and its license accepted.

```bash
viso ios list                      # the Simulator runtimes, installed and available
viso ios use 26.3                  # make an installed runtime the default
viso ios use 27 --download         # have Xcode download it first
viso ios doctor                    # check Xcode, its license, the runtime and the profiles
```

Runtimes are installed by Xcode itself; Viso orchestrates it and records your
default.

### Simulator profiles

```bash
viso ios simulator create iphone
viso ios simulator create ipad --preset tablet
viso ios simulator list
viso ios simulator start iphone
viso ios simulator stop iphone
viso ios simulator delete ipad
```

## Running on a profile

```bash
viso run ios --device iphone
viso run android --device pixel
```

A profile that is not booted is booted for you while the app builds. Without
`--device`, `viso run` uses, in order:

1. the project's local default, in `.viso/local.toml`;
2. your machine's default, from `~/.viso/<platform>/config.toml`;
3. the only profile there is.

With several and no default, an interactive terminal lists them and asks; with none,
it offers to create a standard `phone` profile. Under `--json`, off a terminal and in
CI it never asks: it fails with `DEVICE_AMBIGUOUS` or `DEVICE_MISSING` and the
command to run, for example `viso run android --device pixel`.

`.viso/local.toml` is per machine — keep it out of version control:

```toml
[ios]
device = "iphone"

[android]
device = "pixel"
```

Everything else `viso run` does is the same as on the desktop: hot reload of `.vs`
edits, warm restarts for Rust changes, the dev overlay for errors, `viso inspect`
and `viso profile` against the session (pass `--target ios` or `--target android`).
The app's output appears in the terminal; for the system log, use
`viso android adb --device pixel logcat`.

Ctrl-C ends the session but leaves the simulator or emulator running, so the next
`viso run` starts in seconds.

## Where the state lives

```text
~/.viso/android/      the SDK Viso installed, its emulator profiles, your default
~/.viso/ios/          the simulator profiles, your default
.viso/local.toml      this project's defaults on this machine
```

Set `VISO_HOME` to move `~/.viso`. None of this goes in `Viso.toml`.
