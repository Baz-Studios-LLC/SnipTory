# macOS first test build

Download the ZIP matching your Mac from the v014 release:

- `SnipTory-v014-macos-arm64.zip`: Apple Silicon (M-series).
- `SnipTory-v014-macos-x64.zip`: Intel.

Extract it and move **SnipTory.app** into a writable Applications folder
(for example `~/Applications`), then open it. macOS 13 or later is required.

This build is ad-hoc signed, not Developer ID signed or notarized. If macOS blocks
it, use the app-specific **Open Anyway** control in System Settings → Privacy &
Security after trying to open it. Do not disable Gatekeeper globally. A successful
CI build is not confirmation of the app's runtime behavior on your Mac.

## Permissions and first checks

1. Open SnipTory. Confirm History appears and the menu bar icon remains after X.
2. Use **Option+Shift+S** or Screenshot from the right-click menu. Allow SnipTory
   in System Settings → Privacy & Security → Screen Recording (the label may
   include System Audio on newer macOS). Restart the app if macOS requests it.
3. Make a small selection. Check its dimensions/proportions, clipboard image,
   and immediate appearance in History. Escape should cancel selection.
4. Use **Option+Shift+V** for History and **Option+Shift+F** for a full screenshot
   of the primary display. Left-click the menu bar icon also opens History.
5. Focus your chat app, then click a History card. Automatic paste requires
   SnipTory in Privacy & Security → Accessibility. Otherwise use ⌘V manually.
   Keep History open and test several attachments in succession.
6. Test the eye/trash hover actions, deletion confirmation, Clear history,
   settings persistence, shortcut conflicts, and restart.

All image data stays in `~/Library/Application Support/SnipTory` outside the app
bundle. The update helper and architecture-specific feed are packaged with the
app; an actual version-to-version update still needs testing. Signing with a
stable Developer ID is future work, so permission prompts may change between
these ad-hoc-signed test builds.

## Known first-build scope

- Screenshot selection uses the native macOS selector. The custom blue/red
  selection overlay is not ported yet.
- Full screenshot currently captures the primary display.
- New GIF recording is not available on macOS yet. The action/shortcut is hidden;
  GIF preview and file clipboard support remain available for existing entries.
- Paste-back depends on destination focus and Accessibility permission. The app
  does not claim the destination accepted an attachment merely because a
  keystroke was sent.
- The menu bar context menu uses native macOS styling.

Please start with capture, History, and clipboard testing. These are the first
on-device checks after compiling on actual Mac CI runners.

## Release maintenance

The Package and release workflow builds Windows and both Mac architectures, then
publishes their assets and update feeds together. Do not publish a latest release
containing only Windows assets: Mac clients also use the latest release's feed
directory. Increment VERSION and add release notes before running that workflow;
published version tags and binaries must not be replaced.
