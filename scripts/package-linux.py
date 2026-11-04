#!/usr/bin/env python3
"""Stage a system package without running installers or touching user data."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def install(source, destination, mode=0o644):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(mode)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destdir", type=Path, required=True)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--gui", type=Path, required=True)
    # The Rust mobile connector (services/mobile/connector); its user unit is
    # packaged but never enabled.
    parser.add_argument("--connector", type=Path, required=True)
    parser.add_argument("--licenses", type=Path, required=True)
    parser.add_argument("--package-name", default="zerus-git")
    args = parser.parse_args()
    destination = args.destdir.resolve()
    if destination == Path("/"):
        parser.error("stage into a package directory, never the running system")
    for name, source in (("hgs", args.cli), ("hgs-tray", args.gui), ("zerus-mobile-connector", args.connector)):
        if not source.is_file():
            parser.error(f"build output missing: {source}")
        install(source, destination / "usr/bin" / name, 0o755)
    for installed, source in ((destination / "usr/bin/hgs_state.py", ROOT / "hgs_state.py"),
                              (destination / "usr/bin/zerus-setup", ROOT / "packaging/linux/zerus-setup"),
                              # zerus-setup --mobile-connector runs it.
                              (destination / "usr/lib/zerus/install-mobile-connector-service.py",
                               ROOT / "scripts/install-mobile-connector-service.py"),
                              (destination / "usr/lib/zerus/install-linux-desktop-service.py",
                               ROOT / "scripts/install-linux-desktop-service.py"),
                              (destination / "usr/lib/zerus/desktop-revoke-hold.py",
                               ROOT / "scripts/desktop-revoke-hold.py")):
        install(source, installed, 0o755)
        # Arch owns /usr/bin/python. Make the runtime dependency visible to
        # package tools while retaining portable source shebangs elsewhere.
        text = installed.read_text()
        installed.write_text("#!/usr/bin/python\n" + text.split("\n", 1)[1])
    for unit in (ROOT / "packaging/linux").glob("*.service"):
        install(unit, destination / "usr/lib/systemd/user" / unit.name)
    install(ROOT / "packaging/linux/hgs-tray.desktop", destination / "usr/share/applications/hgs-tray.desktop")
    for size in (16, 24, 32, 48, 64, 128, 256, 512, 1024):
        install(ROOT / f"tray/resources/icons/hgs-zerus-{size}.png",
                destination / f"usr/share/icons/hicolor/{size}x{size}/apps/hgs-zerus.png")
    install(ROOT / "tray/resources/icons/hgs-zerus-symbolic.svg",
            destination / "usr/share/icons/hicolor/scalable/apps/hgs-zerus-swarm-symbolic.svg")
    documentation = destination / "usr/share/doc/zerus"
    for name in ("README.md", "CONTRIBUTING.md", "LICENSE", "THIRD_PARTY_NOTICES.md", "tmux.conf"):
        install(ROOT / name, documentation / name)
    shutil.copytree(ROOT / "docs", documentation / "docs", dirs_exist_ok=True)
    if args.package_name not in {"zerus", "zerus-git", "zerus-ade-bin", "zerus-ade-nightly-bin"}:
        parser.error("unsupported package name")
    licenses = destination / "usr/share/licenses" / args.package_name
    install(ROOT / "LICENSE", licenses / "LICENSE")
    # hgs embeds the Codex greeting and idle-logo data (THIRD_PARTY_NOTICES.md).
    install(ROOT / "docs/licenses/codex-LICENSE.txt", licenses / "codex-LICENSE.txt")
    if not (args.licenses / "inventory.json").is_file():
        parser.error("collect dependency license texts first")
    shutil.copytree(args.licenses, licenses / "rust", dirs_exist_ok=True)
    cli_version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    connector_version = tomllib.loads((ROOT / "services/mobile/connector/Cargo.toml").read_text())["package"]["version"]
    manifest = {"product": "Zerus", "version": (ROOT / "VERSION").read_text().strip(),
                "cli_version": cli_version, "mobile_connector_version": connector_version,
                "platform": "Arch Linux", "native_agents_bundled": False}
    if (ROOT / ".git").exists():
        manifest["source_commit"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    with tempfile.TemporaryDirectory(prefix="zerus-build-identity-") as home:
        env = {"PATH": os.defpath, "LANG": "C.UTF-8", "HOME": home,
               "XDG_CONFIG_HOME": home + "/config", "XDG_DATA_HOME": home + "/data",
               "XDG_CACHE_HOME": home + "/cache", "XDG_RUNTIME_DIR": home,
               "TMPDIR": home, "TMUX_TMPDIR": home}
        identity = json.loads(subprocess.check_output([str(args.gui.resolve()), "--build-info"], env=env, text=True, timeout=20))
    if identity.get("ZerusGUI") != "tauri":
        parser.error("Linux packages must ship the Zerus Next switch binary")
    if identity.get("version") != manifest["version"] or (manifest.get("source_commit") and identity.get("sourceCommit") != manifest["source_commit"]):
        parser.error("GUI build identity does not match the packaged source/version")
    manifest["desktop_build"] = identity
    install_info = documentation / "build-info.json"
    install_info.write_text(json.dumps(manifest, indent=2) + "\n")
    print("Staged CLI, Zerus Next, mobile connector, user service definitions, icons, documentation and dependency notices.")


if __name__ == "__main__":
    main()
