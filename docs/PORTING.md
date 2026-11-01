# macOS and Bazzite preparation

The established application is Windows-based. Shared code is extracted into a
portable Rust library, with CI for Linux, Apple Silicon, and Intel Macs. macOS frontend is compiled and packaged for both architectures; see
[Mac setup and test scope](MACOS.md). The first Linux frontend targets **Bazzite GNOME / Wayland** in v017; see [Linux setup and test scope](LINUX.md). Native CI does not establish desktop feature parity.

## What is ready to reuse

- SQLite history, safe deletion/clearing, PNG writes, preview cache repair.
- Aspect-preserving screenshot and animated-frame thumbnail generation.
- Preferences, shortcut validation/defaults, and completed-recording metadata.
- Shared Slint UI source compilation on each CI host.
- Persistent data path resolution: Windows keeps `%LOCALAPPDATA%\Sniptory`,
  macOS uses `~/Library/Application Support/SnipTory`, and Linux uses
  `$XDG_DATA_HOME/sniptory` (absolute paths only), falling back to
  `~/.local/share/sniptory`. Settings remain alongside history in this data
  directory for now. No existing data is moved.

Windows-specific dependencies are target-gated. The Windows binary consumes the
same library that the other hosts test. `src/recording.rs` still contains the
Windows frame capture and streaming encoder; completed-recording metadata is
portable in `src/media.rs`. Extract the frame-source/encoder boundary when adding
the first non-Windows recording backend rather than copying the encoder.

## First commands on either machine

Install Rust and a native C compiler if needed (bundled SQLite requires one).
On a Mac the compiler comes from the Xcode command-line tools. From this checkout:

```sh
bash scripts/Platform-Info.sh
cargo test --locked --lib
```

The diagnostic script reports architecture, desktop/session, tool availability,
and Linux portal interface versions without taking a screenshot or changing
settings. `cargo run` selects the host frontend. Use `scripts/Package-Mac.sh` or `scripts/Package-Linux.sh` on their native hosts; `scripts/Package.ps1` remains Windows-specific.

## Native implementation order

1. Launch the shared Slint gallery with an OS-specific application entry point,
   persistent data path, correct icon, window behavior, and single-instance handling.
2. Capture a still image, save it through shared History, and update the gallery.
3. Add native clipboard image/file representations, then test attachments in the
   user's actual chat apps. Add paste-back only where the desktop permits it.
4. Add menu bar/tray, global shortcuts, capture selection and multi-monitor/DPI handling.
5. Connect a native recording frame source to the streaming GIF encoder. Check
   live selection, cursor handling, stop-bar exclusion, and bounded memory.
6. Add platform-specific startup and packaging/update support before publishing
   installers. Preserve integer public version labels and data outside binaries.

The Win32 app currently mixes native message-loop commands and update types into
`src/main.rs`. Keep its behavior stable while adding new frontends; do not fill
other platforms with success-returning placeholder capture/clipboard APIs.

## macOS decisions and machine checks

- Use Apple's [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)
  for the capture/recording backend, after checking the target macOS version.
- Implement native pasteboard image and file representations; verify GIFs remain
  animated when attached in destination apps.
- Request Screen Recording permission in the actual app bundle. Verify the
  permissions needed for paste-back separately; do not treat capture permission
  as permission to send keystrokes.
- Keep a menu bar utility that can show History and retain the established
  design. Check displays, scale factors, Spaces/full-screen behavior, and the
  app's ability to keep History visible.
- Choose signed/notarized .app distribution and its update mechanism once signing
  credentials and a real bundle are available. No signing secrets enter the repo.
- Test Apple Silicon and Intel shared code in CI. Test the actual UI on the Mac
  before claiming either architecture is a supported desktop release.

## Bazzite / Wayland decisions and machine checks

The current user target is Bazzite GNOME on Wayland (updated from the earlier KDE target). See [Bazzite's variants](https://docs.bazzite.gg/General/FAQ/).

- Start with the [Screenshot portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Screenshot.html).
- Use [ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
  and its PipeWire streams for recording. Verify portal/backend versions,
  source selection, cursor modes, consent, and restore-token behavior on-device.
- Use [GlobalShortcuts](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html)
  where the session exposes it. The desktop may present a configuration dialog
  and owns the accepted binding; preferred shortcuts are requests, not guarantees.
- Do not implement Wayland capture or global shortcuts by assuming X11 APIs work
  under XWayland. Do not promise arbitrary window positioning, always-on-top, or
  synthetic Ctrl+V until tested against the compositor's supported mechanisms.
- Inspect Clipboard/RemoteDesktop support as needed. Image copy and paste-back
  are different capabilities; keep explicit user paste available if injection
  is unavailable.
- Prefer a Flatpak distribution investigation for Bazzite's desktop workflow,
  with permission scopes limited to actual requirements. Flatpak updates should
  integrate with the OS delivery mechanism rather than replacing files inside
  a read-only installation. No Flatpak manifest is published as runnable yet.

## Release acceptance on each machine

Verify capture permissions (including refusal), multi-monitor selection, live GIF
selection, original clipboard formats, persistent history, hover playback,
viewer actions, configurable shortcut conflicts, tray/menu behavior, restarting,
and an actual version-to-version update with data preservation. Shared library CI
is useful preparation, not evidence that these desktop integrations work.
