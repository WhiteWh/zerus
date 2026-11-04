#!/usr/bin/env python3
"""Check staged package completeness and its executables using an isolated home."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

CONNECTOR = "usr/bin/zerus-mobile-connector"
CONNECTOR_UNIT = "usr/lib/systemd/user/zerus-mobile-connector.service"
# The connector links rustls, ring and SQLite statically: it needs nothing
# beyond the C runtime that every package depends on.
CONNECTOR_LIBRARIES = {"libc.so.6", "libm.so.6", "libgcc_s.so.1", "ld-linux-x86-64.so.2"}


def needed(path):
    """Shared libraries an ELF file names in its dynamic section."""
    dynamic = subprocess.check_output(["readelf", "--dynamic", "--wide", str(path)], text=True)
    return [line.split("[", 1)[1].split("]", 1)[0] for line in dynamic.splitlines() if "(NEEDED)" in line]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--package-name", default="zerus-git")
    args = parser.parse_args()
    root = args.root.resolve()
    required = ["usr/bin/hgs", "usr/bin/hgs-tray", "usr/bin/hgs_state.py", "usr/bin/zerus-setup",
                "usr/share/applications/hgs-tray.desktop", "usr/lib/systemd/user/hgs-tray.service",
                "usr/lib/systemd/user/hgs-swarm.service", "usr/lib/systemd/user/hgs-recovery.service",
                f"usr/share/licenses/{args.package_name}/LICENSE",
                f"usr/share/licenses/{args.package_name}/codex-LICENSE.txt",
                f"usr/share/licenses/{args.package_name}/rust/inventory.json", "usr/share/doc/zerus/build-info.json",
                CONNECTOR, CONNECTOR_UNIT, "usr/lib/zerus/install-mobile-connector-service.py",
                "usr/lib/zerus/install-linux-desktop-service.py", "usr/lib/zerus/desktop-revoke-hold.py"]
    for name in required:
        if not (root / name).is_file():
            raise SystemExit(f"Missing package file: {name}")
    for path in root.iterdir():
        if path.name not in {"usr", ".PKGINFO", ".BUILDINFO", ".MTREE"}:
            raise SystemExit(f"Unexpected package root: {path.name}")
    desktop_libraries = needed(root / "usr/bin/hgs-tray")
    if any(name.startswith(("libQt", "libKF")) for name in desktop_libraries):
        raise SystemExit("Zerus Next must not link Qt or KDE Frameworks")
    if not {"libwebkit2gtk-4.1.so.0", "libgtk-3.so.0"} <= set(desktop_libraries):
        raise SystemExit("Zerus Next must link WebKitGTK 4.1 and GTK3")
    if (root / "usr/lib/hgs-tray/plugins").exists():
        raise SystemExit("Zerus Next packages must not contain the Qt native UI plugin")
    linked = set(needed(root / CONNECTOR))
    if not linked <= CONNECTOR_LIBRARIES:
        raise SystemExit(f"The mobile connector links unexpected libraries: {sorted(linked - CONNECTOR_LIBRARIES)}")
    # Its black-box test seams exist only with the test-hooks feature.
    if b"ZERUS_CONNECTOR_TEST_" in (root / CONNECTOR).read_bytes():
        raise SystemExit("The packaged mobile connector was built with test hooks")
    unit = (root / CONNECTOR_UNIT).read_text().splitlines()
    if "KillMode=process" not in unit or "ExecStart=/usr/bin/zerus-mobile-connector --config %E/hgs/mobile/connector.json" not in unit:
        raise SystemExit("The mobile connector unit must run the packaged binary with KillMode=process")
    manifest = json.loads((root / "usr/share/doc/zerus/build-info.json").read_text())
    with tempfile.TemporaryDirectory(prefix="zerus-package-test-") as home:
        env = {"PATH": os.defpath, "LANG": "C.UTF-8", "HOME": home,
               "HGS_STATE_DIR": home + "/state", "HGS_CONFIG_DIR": home + "/config",
               "XDG_CONFIG_HOME": home + "/config", "XDG_DATA_HOME": home + "/data",
               "XDG_CACHE_HOME": home + "/cache", "XDG_RUNTIME_DIR": home,
               "TMPDIR": home, "TMUX_TMPDIR": home}
        desktop = manifest.get("desktop_build", {})
        if desktop.get("ZerusGUI") != "tauri":
            raise SystemExit("The package build identity must identify Zerus Next")
        expected = {"hgs": "hgs " + manifest["cli_version"], "hgs-tray": "Zerus " + desktop.get("versionName", manifest["version"]),
                    "zerus-mobile-connector": "zerus-mobile-connector " + manifest["mobile_connector_version"]}
        for name, version in expected.items():
            result = subprocess.run([str(root / "usr/bin" / name), "--version"], env=env,
                                    capture_output=True, text=True, timeout=20, check=True)
            if result.stdout.strip() != version:
                raise SystemExit(f"Wrong {name} version: {result.stdout.strip()}")
        if desktop:
            actual = json.loads(subprocess.check_output([str(root / "usr/bin/hgs-tray"), "--build-info"], env=env, text=True, timeout=20))
            if actual != desktop:
                raise SystemExit("GUI embedded build identity does not match the package manifest")
    subprocess.run(["desktop-file-validate", str(root / "usr/share/applications/hgs-tray.desktop")], check=True)
    inventory = json.loads((root / f"usr/share/licenses/{args.package_name}/rust/inventory.json").read_text())
    if not inventory["packages"]:
        raise SystemExit("Dependency license inventory is empty.")
    print(f"Package layout and executable versions verified; {len(inventory['packages'])} dependency notices included.")


if __name__ == "__main__":
    main()
