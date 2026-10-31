"""Normalize public Mac artifact names and feeds to integer release labels."""
import hashlib
import json
import pathlib
import sys
import zipfile

folder = pathlib.Path(sys.argv[1])
tag, arch = sys.argv[2:4]
renamed = {}
for path in list(folder.iterdir()):
    name = None
    if path.suffix == ".nupkg":
        kind = "delta" if "delta" in path.name.lower() else "full"
        name = f"SnipTory-{tag}-macos-{arch}-{kind}.nupkg"
    elif path.suffix == ".zip":
        name = f"SnipTory-{tag}-macos-{arch}.zip"
    elif path.name == "RELEASES":
        name = f"RELEASES-osx-{arch}"
    if name and name != path.name:
        renamed[path.name] = name
        path.rename(path.with_name(name))

def replace(value):
    if isinstance(value, str):
        return renamed.get(value, value)
    if isinstance(value, list):
        return [replace(item) for item in value]
    if isinstance(value, dict):
        return {key: replace(item) for key, item in value.items()}
    return value

for path in folder.glob("*.json"):
    path.write_text(json.dumps(replace(json.loads(path.read_text(encoding="utf-8-sig"))), indent=2), encoding="utf-8")
for path in folder.glob("RELEASES*"):
    text = path.read_text(encoding="utf-8-sig")
    for old, new in renamed.items():
        text = text.replace(old, new)
    path.write_text(text, encoding="utf-8")
portable = folder / f"SnipTory-{tag}-macos-{arch}.zip"
with zipfile.ZipFile(portable) as archive:
    names = archive.namelist()
    assert any(name.endswith("Contents/MacOS/sniptory") for name in names), "Missing executable"
    assert any(name.endswith("Contents/MacOS/UpdateMac") for name in names), "Missing update helper"
checksums = []
for path in sorted(folder.iterdir()):
    if path.is_file() and not path.name.startswith("SHA256SUMS"):
        checksums.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}")
(folder / f"SHA256SUMS-macos-{arch}.txt").write_text("\n".join(checksums) + "\n", encoding="utf-8")
print(f"Packaged {tag} for macOS {arch}")
