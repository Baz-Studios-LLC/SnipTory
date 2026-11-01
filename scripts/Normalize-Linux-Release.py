"""Normalize Linux public names without altering package identity or checksums."""
import hashlib,json,pathlib,sys
folder=pathlib.Path(sys.argv[1]);tag=sys.argv[2];renamed={}
for path in list(folder.iterdir()):
    name=None
    if path.suffix==".nupkg":name=f"SnipTory-{tag}-linux-x64-{'delta' if 'delta' in path.name.lower() else 'full'}.nupkg"
    elif path.suffix.lower()==".appimage":name=f"SnipTory-{tag}-linux-x64.AppImage"
    elif path.name=="RELEASES":name="RELEASES-linux-x64"
    if name and name!=path.name:renamed[path.name]=name;path.rename(path.with_name(name))
def replace(value):
    if isinstance(value,str):return renamed.get(value,value)
    if isinstance(value,list):return [replace(item) for item in value]
    if isinstance(value,dict):return {key:replace(item) for key,item in value.items()}
    return value
for path in folder.glob("*.json"):path.write_text(json.dumps(replace(json.loads(path.read_text(encoding="utf-8-sig"))),indent=2),encoding="utf-8")
for path in folder.glob("RELEASES*"):
    text=path.read_text(encoding="utf-8-sig")
    for old,new in renamed.items():text=text.replace(old,new)
    path.write_text(text,encoding="utf-8")
assert (folder/f"SnipTory-{tag}-linux-x64.AppImage").is_file()
(folder/"SHA256SUMS-linux-x64.txt").write_text("".join(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n" for path in sorted(folder.iterdir()) if path.is_file() and not path.name.startswith("SHA256SUMS")),encoding="utf-8")
