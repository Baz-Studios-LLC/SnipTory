"""Collect the active host's dependency licenses for a native Mac bundle."""
import json
import pathlib
import subprocess
import sys

host = next(line.split(": ", 1)[1] for line in subprocess.check_output(["rustc", "-vV"], text=True).splitlines() if line.startswith("host: "))
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", host], text=True))
notices = ["SnipTory third-party notices", "SnipTory source is MIT. Dependencies retain their respective licenses.", "Slint components are used under the Slint Royalty-free License. Attribution appears on the repository and release download pages."]
for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    if not package.get("source"):
        continue
    notices.extend(["\n" + "=" * 60, f'{package["name"]} {package["version"]}', f'License: {package.get("license")}', f'Source: {package.get("repository")}'])
    for path in sorted(pathlib.Path(package["manifest_path"]).parent.iterdir()):
        if path.is_file() and path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT")):
            notices.append(path.read_text(encoding="utf-8", errors="replace"))
pathlib.Path(sys.argv[1]).write_text("\n".join(notices) + "\n", encoding="utf-8")
