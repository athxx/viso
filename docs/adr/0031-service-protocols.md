# ADR 0031 — Service protocols: one registry, one-shot replies, one implementation per OS

- Status: Accepted
- Date: 2026-09-26

## Context

`Viso_Architecture.md` §49 routes low-frequency OS capabilities through service
protocols that are mockable for headless tests. `viso-services` was a marker trait
with no protocol. The first set to land: file open/save dialogs, share, notifications,
permissions, secure storage and haptics.

Three facts shape the design:

- Most of these calls wait on the user or the OS. AppKit, UIKit, WinRT, the XDG
  portal, Android activity results and the browser all answer through callbacks, and
  some of those callbacks run off the UI thread.
- Viso has no UI task protocol yet (§25 `cx.spawn`), so a service cannot assume an
  executor.
- Several OS implementations hold main-thread objects: panels, view controllers,
  generators.

## Decision

### 1. Protocols are small object-safe traits in `viso-services`

The traits are `FileDialogs`, `Share`, `Notifications`, `Permissions`, `SecureStorage`
and `Haptics`.

- Their inputs are plain owned data: `OpenOptions`, `ShareItem`, `Notification`, and
  so on.
- Enums that will grow (`ShareItem`, `Permission`, `Haptic`, `ServiceError`) are
  `#[non_exhaustive]`.
- Opened files are read in full (`PickedFile { name, path: Option<PathBuf>, contents }`).
  iOS, Android and the Web grant the contents rather than a path, so an eager read is
  the only portable shape.

### 2. Asynchronous calls return a `Reply<T>`

- A call returns at once with a `Reply<T>`, and hands the paired `Completer<T>` to the
  OS callback.
- `Reply` is a `Future<Output = Result<T, ServiceError>>`, and `try_take()` polls it
  without an executor.
- The completer is `Send`: it may complete from any thread, and it wakes the stored
  waker.
- A completer dropped without an answer yields `Cancelled`.

The shared slot is an `Arc<Mutex<…>>`. This is a cold path, touched once per call and
never per frame.

`ServiceError` has four cases:

- `Unsupported`: no such capability on this OS.
- `Denied`: the permission is refused.
- `Cancelled`: the user dismissed the request, or it was abandoned.
- `Failed(String)`: the OS reported an error.

Haptics are fire-and-forget and return a plain `Result`.

### 3. `Services` is a registry with a fixed field per protocol

- Each field is an `Rc<dyn Protocol>`. There is no string-keyed lookup.
- `Services::system(app)` picks this OS's implementations. `app` scopes secure storage
  and names the notification sender.
- `Services::unsupported()` answers every call with `Unsupported`.
- `with_files`, `with_share` and the other `with_*` methods replace one protocol.
- `Mock` is a recording, scripted implementation of all six protocols. Its clones share
  state, so a test keeps a clone to inspect the calls.

The registry is `!Send` on purpose. Services are called on the UI thread, and that is
what lets an implementation hold main-thread objects.

### 4. Implementations

| | macOS | iOS | Windows | Linux | Android | Web |
|---|---|---|---|---|---|---|
| Files | `NSOpenPanel` / `NSSavePanel` | `UIDocumentPickerViewController` | `IFileOpenDialog` / `IFileSaveDialog` | XDG portal `FileChooser` | `ACTION_OPEN_DOCUMENT` / `ACTION_CREATE_DOCUMENT` | `<input type=file>` / `<a download>` |
| Share | `NSSharingServicePicker` | `UIActivityViewController` | `DataTransferManager` | Unsupported | `ACTION_SEND` chooser | `navigator.share` |
| Notifications | `UNUserNotificationCenter` | `UNUserNotificationCenter` | tray balloon (`Shell_NotifyIcon`) | `org.freedesktop.Notifications` | `NotificationManager` + channel | `Notification` |
| Permissions | UN authorization | UN authorization | always granted | always granted | `POST_NOTIFICATIONS` (API 33+) | `Notification.permission` |
| Secure storage | Keychain | Keychain | Credential Manager | Secret Service | AES-GCM under an `AndroidKeyStore` key, in `SharedPreferences` | Unsupported |
| Haptics | `NSHapticFeedbackManager` | `UIFeedbackGenerator` | Unsupported | Unsupported | `View.performHapticFeedback` | `navigator.vibrate` |

Notes on the table:

- Every other target gets `Services::unsupported()`.
- On macOS, `UNUserNotificationCenter` requires a bundled app. A bare binary answers
  `Unsupported` rather than letting the framework raise.
- Android answers a save with `Ok(None)`: the document is a content URI, not a path.
  Its share chooser reports no cancellation, and its pickers take no title.
- The Web shows the file chooser, share sheet and permission prompt only during
  transient user activation, so these calls belong in an input handler. A save is a
  download and answers `Ok(None)`. Browsers that show notifications only from a
  service worker (Chrome on Android) answer `Unsupported`.

### 5. Android reaches the activity through `viso-platform`

Android services need the live activity and the results it receives: activity results
and permission results. Only the platform's `VisoActivity` receives these, so the
platform exposes two narrow hooks and nothing more:

- `viso_platform::backend::android::with_activity(f)` runs `f` with the attached
  `JNIEnv` and a local reference to the current activity, or answers `None` when there
  is none.
- `VisoActivity.addResults(Results)` registers a Java listener that receives every
  `onActivityResult` and `onRequestPermissionsResult`.

Everything else lives in services. The APK carries a `dev.viso.services.VisoServices`
Java class. Services load it through the activity's class loader, register its native
callbacks, and call its static methods. Each asynchronous call parks its `Completer`
under a token and passes the token to Java. Java answers through a native callback
with the token, a status (ok, cancelled, denied, failed, unsupported) and the value.
The parked completers sit in a mutex-guarded map: a cold path, reached from Java
threads.

This adds the edge `viso-services → viso-platform`, on Android only. It follows the
one-way DAG (§3.5): services sit above platform, and platform knows nothing of
services.

### 6. The facade integration waits for the UI task protocol

Until `cx.spawn` exists, apps hold a `Services` value and poll replies with `try_take`
during a frame, or await them in their own executor. Wiring the registry into the
contexts (`cx.services()`, §49) is left as a follow-up.

## Consequences

- App code has no `cfg(target_os)` branch for these capabilities, and every call is
  exercised headlessly through `Mock`.
- Each additional protocol (camera, location, …) is a new trait, a field, and a row in
  the table.
- Eager file reads make very large files expensive. A streaming variant can be added
  when a real need shows up.
