# Bazzite GNOME / Wayland (x86-64)

Download `SnipTory-v017-linux-x64.AppImage` from the v017 release. Keep it in a user-writable folder such as `~/Applications` for in-app updates.

```sh
mkdir -p ~/Applications
mv ~/Downloads/SnipTory-v017-linux-x64.AppImage ~/Applications/
chmod +x ~/Applications/SnipTory-v017-linux-x64.AppImage
APPIMAGE_EXTRACT_AND_RUN=1 ~/Applications/SnipTory-v017-linux-x64.AppImage
```

The extraction mode works without installing FUSE or changing Bazzite's immutable system. The app adds a GNOME launcher on first run. Keep the AppImage at that path; the launcher and optional sign-in entry reference it. Subsequent updates replace this file internally.

Screenshots use the GNOME desktop portal and its native picker. GNOME may request permission. Global shortcuts request Alt+Shift+S (capture), Alt+Shift+V (history) and Alt+Shift+F (full screenshot); approve them in GNOME's dialog. GNOME has final control over shortcut mappings. Settings changes take effect after restarting SnipTory. If your portal does not support global shortcuts, add custom keyboard shortcuts in GNOME Settings using the full AppImage path with `--capture`, `--history`, or `--full`.

Clicking a History card copies its original image to the Wayland clipboard; then paste into the destination app. Original-size viewing, deleting captures, clearing History, GIF hover previews, custom save location, launch at sign-in and update controls use the shared app UI.

This first Linux build does not record GIFs or send automatic paste keystrokes. GNOME controls focus, window activation and always-on-top behavior. There is no tray icon requirement or GNOME extension dependency: close hides History, reopen with the launcher or shortcut, and use Options → Quit SnipTory to exit.

Data defaults to `$XDG_DATA_HOME/sniptory` or `~/.local/share/sniptory`. Captures default to its `captures/screenshots` and `captures/gifs` subfolders. The save location can be changed in Options.

Native Ubuntu CI compiles and tests the Linux frontend and checks the packaged AppImage starts without a desktop. Screenshot selection, clipboard behavior, shortcuts and updates on Bazzite GNOME still need hands-on verification.
