# SnipTory

**Capture. Recall. Paste.** A native Windows 11 visual memory utility from Baz Studios. Built with Rust 2024, Slint, Win32, and SQLite. Screenshots and clipboard contents remain local.

[Download the installer](https://github.com/Baz-Studios-LLC/SnipTory/releases/latest) · [MIT license](LICENSE)

## Install and update

Install **SnipTory-v002-Setup.exe** once. The app installs for your Windows user, creates a Start menu shortcut, and can be uninstalled through Windows Settings. The blue-and-white app icon appears on the taskbar; the tray glyph is white on transparent.

Open **Options** using the gear in History or the matching tray menu. Options include launch at sign-in, automatic update checks, copy-only versus paste-back behavior, opening the capture folder, restarting the app, and checking for updates. History and Options share one always-on-top window.

Installed builds check the public GitHub release feed after launch and every six hours when automatic checks are enabled. Checking and downloading run in a background worker. When an update is available, choose **Download**, then **Restart & update**. Queued saves finish first, and Velopack replaces the app and relaunches silently without another installation wizard. Checks can be disabled; manual checks remain available. No GitHub login or embedded credential is needed. Uninstalled development executables explain that installation is required for updates.

Release names, tags, in-app labels, and download filenames use **v001, v002, v003**, with no decimals. `VERSION` is the source of the public release number. Cargo and installer package metadata use the numeric format required by those tools; it is not displayed as the app's release label.

Application binaries install under `%LOCALAPPDATA%\BazStudios.SnipTory\`. Capture data is separate under `%LOCALAPPDATA%\Sniptory\`, so updates and app uninstallation do not remove your history. The Windows runtime is statically linked; no .NET runtime is required to run the application. Current binaries are unsigned.

## Development

Requires stable Rust (1.92+), the `x86_64-pc-windows-msvc` target, Visual Studio C++ Build Tools, and the Windows SDK.

```powershell
cargo run
cargo test
cargo build --release
```

The gallery opens on launch. Closing it keeps the tray utility running. Exit through the tray menu.

| Action | Shortcut |
| --- | --- |
| Capture a region | Alt+Shift+S |
| Open history | Alt+Shift+V |
| Cancel selection / hide history | Escape |
| Select a gallery item | Arrow keys |
| Paste selected item | Enter |

Drag a rectangle to capture. The image is copied to the clipboard and saved as PNG. Open history and click a capture to copy it, restore the preceding window, and request paste. Clipboard contention is retried. If focus restoration or Windows input injection is blocked, the image stays on the clipboard for manual paste.

## Visual direction

The design standard is modern, polished, and restrained. A charcoal shell, reference navy **#162F55**, brighter blue focus accents, Segoe UI Variable typography, rounded preview cards, and thin borders create a compact utility rather than a conventional application dashboard. Lavender is excluded from the palette.

## Architecture

- `src/main.rs`: Slint gallery, callbacks, background storage worker.
- `src/native.rs`: blocking Win32 message loop, tray, hotkeys, single instance, foreground-window tracking, physical-pixel selection overlay.
- `src/capture.rs`: checked GDI capture and region cropping.
- `src/clipboard.rs`: real CF_DIB image data, clipboard retries, synthetic paste.
- `src/history.rs`: SQLite metadata, atomic PNG and thumbnail writes.
- `src/settings.rs`: persisted preferences and this app's per-user startup entry.
- `src/updates.rs`: installer-managed release checks, downloads, and restart coordination.
- `ui/history.slint`, `ui/options.slint`, `ui/tray.slint`: gallery, integrated Options, and matching tray menu.

The desktop is frozen **before** the selector is displayed. Selection crops that immutable image, so selector borders never appear in output. Slint runs on the main thread; native integration and storage each have a blocking worker. No idle polling timer is used. A temporary 30 ms timer waits for physical modifiers to be released before paste, with a two-second timeout.

Per-monitor-v2 DPI awareness comes from the executable manifest. Selection uses virtual-desktop physical pixels, including negative monitor origins. Gallery placement is constrained to the cursor monitor's work area.

Storage lives in `%LOCALAPPDATA%\Sniptory\`: `captures/screenshots/`, `thumbnails/`, and `sniptory.db`. SQLite uses WAL and full synchronous writes. Missing or unreadable history assets are skipped; thumbnails can be regenerated in memory from originals. Files are written to a temporary file, flushed, then renamed before metadata is inserted.

## Scope and limitations

Phase 1 supports screenshots, a persistent gallery, tray control, global hotkeys, paste-back, saved options, and installer-managed updates. GIF recording remains future work.

GDI capture prioritizes small dependencies and simple region selection. Protected video/windows, secure desktops, exclusive fullscreen content, and HDR color fidelity are not guaranteed. Windows may return black protected content without an explicit error. A virtual desktop larger than a 512 MiB raw frame is rejected. Selection temporarily uses additional memory for its frozen and dimmed images; those buffers are freed on completion or cancellation.

Windows can prevent focus changes and synthetic input, especially into elevated applications. Successful `SendInput` delivery does not prove a target application accepted the image; arbitrary applications do not provide a universal paste acknowledgement. Hotkey conflicts are reported through tray feedback, and tray actions remain available. The tray icon is recreated after Explorer restarts.

## Validation so far

The app builds and launches on Windows. Automated checks cover reversed/clamped region cropping, PNG and thumbnail persistence across database reopening, and integer release labels. An earlier manual native test passed an actual desktop-pixel roundtrip through the Windows CF_DIB clipboard. The initial gallery was visually inspected and selection cancellation was exercised. Full drag-to-capture, paste-back, current UI changes, installation, and the first version-to-version update are left to the user, as requested. Mixed-DPI hardware, protected content, and elevated paste targets have not been exercised.

Next: user feedback on Phase 1's capture/paste flow and visual design, followed by reliability fixes and then GIF recording.

## Release packaging

Build on Windows with the Visual Studio C++ toolchain and Windows SDK. Install .NET SDK 8 and the pinned packaging tool (build-time only):

```powershell
dotnet tool install vpk --version 1.2.161 --tool-path .tools/vpk
./scripts/Package.ps1
```

This builds the release executable, generates third-party notices, and packages the installer, portable ZIP, full update package, feed, and SHA-256 checksums under `dist/v002/`. Only the staged application files are packaged; captures and development files are excluded. Package and release filenames are normalized to the public integer release sequence, and the update feed is rewritten to match.

For the next release, advance `VERSION`, update Cargo's tool-required version metadata to match its integer value, and add the corresponding file under `release-notes/`. Commit, then run the **Package and release** GitHub Actions workflow. Publish a new release rather than replacing binaries under an existing tag.

## License and attribution

SnipTory's source is **MIT**, copyright Baz Studios LLC. Dependencies retain their own licenses. Slint is used under its Royalty-free Desktop, Mobile, and Web Applications License, with attribution on this public repository and the release download pages. The installer includes third-party notices and the Slint license.

[![Made with Slint](assets/MadeWithSlint.png)](https://slint.dev)
