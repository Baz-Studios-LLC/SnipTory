#!/usr/bin/env bash
# Read-only diagnostics for the macOS / Bazzite port. Does not capture the screen
# or request permissions, install packages, or change desktop configuration.
set -u
printf 'SnipTory platform diagnostics\nOS: %s\nArchitecture: %s\n' "$(uname -s)" "$(uname -m)"
if command -v rustc >/dev/null 2>&1; then rustc --version; else printf 'Rust: not installed\n'; fi
if command -v cargo >/dev/null 2>&1; then cargo --version; fi
case "$(uname -s)" in
  Darwin)
    sw_vers
    if command -v xcode-select >/dev/null 2>&1; then xcode-select -p 2>/dev/null || true; fi
    printf 'Screen Recording / Accessibility permissions: verify interactively when the native app is ready.\n'
    ;;
  Linux)
    if [ -r /etc/os-release ]; then
      while IFS= read -r line; do
        case "$line" in NAME=*|VERSION=*|VARIANT=*|PRETTY_NAME=*) printf '%s\n' "$line";; esac
      done < /etc/os-release
    fi
    printf 'Desktop: %s\nSession: %s\n' "${XDG_CURRENT_DESKTOP:-unknown}" "${XDG_SESSION_TYPE:-unknown}"
    for tool in flatpak pipewire rustc cc busctl; do
      if command -v "$tool" >/dev/null 2>&1; then printf '%s: available\n' "$tool"; else printf '%s: unavailable\n' "$tool"; fi
    done
    if command -v busctl >/dev/null 2>&1; then
      for portal in Screenshot ScreenCast GlobalShortcuts Clipboard; do
        printf '%s portal version: ' "$portal"
        busctl --user --timeout=5 get-property org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop "org.freedesktop.portal.$portal" version 2>/dev/null || printf 'not exposed by this session\n'
      done
    fi
    ;;
  *) printf 'This platform is not a current desktop target.\n';;
esac
