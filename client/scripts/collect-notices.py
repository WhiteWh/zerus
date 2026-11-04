#!/usr/bin/env python3
"""Collect the installed, locked frontend runtime dependency notices."""
import argparse
import json
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[1]
# Vite emits its module-preload helper into the built web interface.
BUILD_RUNTIME = {"node_modules/vite"}


def collect(client, destination):
    lock = json.loads((client / "package-lock.json").read_text())
    inventory = []
    for relative, package in sorted(lock["packages"].items()):
        if not relative or package.get("link") or package.get("dev") and relative not in BUILD_RUNTIME:
            continue
        path = Path(relative)
        if path.is_absolute() or ".." in path.parts or path.parts[0] != "node_modules":
            raise ValueError("Unexpected frontend dependency path")
        source = client / path
        if not source.exists() and package.get("optional"):
            continue
        info = json.loads((source / "package.json").read_text())
        if info["version"] != package["version"]:
            raise ValueError(f"Frontend dependency differs from the lock: {relative}")
        notices = sorted(p for p in source.iterdir() if p.is_file()
                         and p.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE")))
        if not notices:
            raise ValueError(f"No license text found for {info['name']} {info['version']}")
        folder = destination / (info["name"].replace("/", "-") + "-" + info["version"])
        folder.mkdir(parents=True, exist_ok=True)
        for notice in notices:
            shutil.copyfile(notice, folder / notice.name)
        inventory.append({"name": info["name"], "version": info["version"],
                          "license": info.get("license"), "notices": [p.name for p in notices]})
    if not inventory:
        raise ValueError("The frontend runtime dependency inventory is empty")
    (destination / "inventory.json").write_text(json.dumps({"packages": inventory}, indent=2) + "\n")
    return inventory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    print(f"Collected notices for {len(collect(ROOT, args.destination))} frontend dependencies.")


if __name__ == "__main__":
    main()
