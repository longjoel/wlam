#!/usr/bin/env python3
"""Copy the original Hover data files beside its recompiled browser bundle."""
import json
import shutil
import sys
from pathlib import Path

source = Path(sys.argv[1])
bundle = Path(sys.argv[2])
files_dir = bundle / "files"
assets = []

def add_file(path: Path) -> None:
    relative = path.relative_to(source)
    destination = files_dir / relative
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        destination.chmod(0o644)
    shutil.copyfile(path, destination)
    logical = "C:\\" + relative.as_posix().replace("/", "\\")
    assets.append({"path": logical, "url": "files/" + relative.as_posix()})

for directory in ("mazes", "sounds"):
    root = source / directory
    if root.is_dir():
        for path in sorted(p for p in root.rglob("*") if p.is_file()):
            add_file(path)

for name in ("hover.hlp", "hover.cnt", "hovrread.txt", "wavemix.inf"):
    path = source / name
    if path.is_file():
        add_file(path)

manifest_path = bundle / "manifest.json"
manifest = json.loads(manifest_path.read_text())
manifest["files"] = assets
manifest["display"] = {"bitsPerPixel": 8}
manifest_path.write_text(json.dumps(manifest, separators=(",", ":")))
print(f"Packaged {len(assets)} Hover data files")
