#!/usr/bin/env python3
"""Write the Tauri configuration of the Mac switch bundle.

tauri.switch.conf.json with Zerus' version from VERSION and every path made
absolute, so `tauri build --config OUTPUT` reads the same files wherever
OUTPUT lives (docs/macos-distribution.md#tauri-client-switch).

    switch-config.py OUTPUT
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
TAURI = ROOT / "client/src-tauri"


def absolute(path):
    return str((TAURI / path).resolve())


def config(version, info_plist=None):
    data = json.loads((TAURI / "tauri.switch.conf.json").read_text())
    data["version"] = version
    bundle = data["bundle"]
    bundle["resources"] = {absolute(source): target for source, target in bundle["resources"].items()}
    mac = bundle["macOS"]
    mac["infoPlist"] = str(info_plist) if info_plist else absolute(mac["infoPlist"])
    mac["files"] = {target: absolute(source) for target, source in mac["files"].items()}
    return data


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--release", default=os.environ.get("ZERUS_RELEASE_VERSION", ""))
    args = parser.parse_args(argv[1:])
    version = (ROOT / "VERSION").read_text().strip()
    dev_base = (ROOT / "DEV_VERSION").read_text().strip()
    number = os.environ.get("ZERUS_DEV_BUILD_NUMBER", "")
    if number and not re.fullmatch(r"[1-9]\d*", number):
        parser.error("ZERUS_DEV_BUILD_NUMBER must be a positive build number")
    release = args.release or (f"{dev_base}-dev.{number}" if number else version)
    if not (re.fullmatch(re.escape(version) + r"(?:\.r\d+\.g[0-9a-f]{7}\.n\d+)?", release)
            or re.fullmatch(re.escape(dev_base) + r"-dev\.[1-9]\d*", release)):
        parser.error("release must match the stable, nightly or numbered Dev source")
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    # Generate outside the source tree (normally target/) so numbered builds
    # keep their clean source identity and a replaced bundle can offer Restart.
    info = plistlib.loads((TAURI / "Info.switch.plist").read_bytes())
    info["ZerusReleaseVersion"] = release
    info_path = output.with_suffix(".plist")
    info_path.write_bytes(plistlib.dumps(info))
    output.write_text(json.dumps(config(version, info_path), indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
