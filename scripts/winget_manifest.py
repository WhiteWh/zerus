"""Windows Package Manager manifests for a published stable portable zip; no side effects."""
import re

from release_common import LAST_STABLE_WITHOUT_WINDOWS, REPOSITORY, require
import windows_portable

IDENTIFIER = "ufna.Zerus"
MANIFEST_VERSION = "1.12.0"
# winget-pkgs stores manifests under manifests/<lowercase first letter>/<publisher>/<name>/<version>.
PACKAGE_PATH = "manifests/u/ufna/Zerus"
# Windows 10 version 1903, the oldest release docs/installation.md supports.
MINIMUM_OS = "10.0.18362.0"
SHORT_DESCRIPTION = "Agent development environment for AI coding agents across your machines"
DESCRIPTION = (
    "Zerus is a desktop app and the hgs CLI for persistent AI coding agent sessions.\n"
    "On Windows it is a control console for remote Linux and macOS machines, reached\n"
    "through the Windows OpenSSH client, and it can also run agents on the Windows\n"
    "computer itself in its own ConPTY session host."
)
TAGS = ("ai", "agents", "claude-code", "codex", "coding-agent", "ssh", "terminal")
SCHEMA = "# yaml-language-server: $schema=https://aka.ms/winget-manifest.{}.{}.schema.json\n"
HEADER = "# Created by scripts/publish-winget.py in https://github.com/" + REPOSITORY + "\n"


def require_version(version):
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version or "") is not None, "winget publishes stable versions only.")
    later = tuple(map(int, version.split("."))) > tuple(map(int, LAST_STABLE_WITHOUT_WINDOWS.split(".")))
    require(later, f"Stable {version} has no Windows zip; releases after {LAST_STABLE_WITHOUT_WINDOWS} carry one.")


def installer_url(version):
    return f"https://github.com/{REPOSITORY}/releases/download/v{version}/{windows_portable.asset_name(version)}"


def manifests(version, sha256, release_date):
    """The three manifest files of one version, keyed by their winget-pkgs path."""
    require_version(version)
    require(re.fullmatch(r"[0-9a-f]{64}", sha256 or "") is not None, "Invalid Windows zip SHA-256.")
    require(re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", release_date or "") is not None, "Invalid release date.")
    common = f"PackageIdentifier: {IDENTIFIER}\nPackageVersion: {version}\n"
    files = {
        f"{IDENTIFIER}.yaml": (
            SCHEMA.format("version", MANIFEST_VERSION) + "\n" + common
            + "DefaultLocale: en-US\nManifestType: version\n"),
        f"{IDENTIFIER}.installer.yaml": (
            SCHEMA.format("installer", MANIFEST_VERSION) + "\n" + common
            + "Platform:\n- Windows.Desktop\n"
            + f"MinimumOSVersion: {MINIMUM_OS}\n"
            + "InstallerType: zip\nNestedInstallerType: portable\nNestedInstallerFiles:\n"
            + f"- RelativeFilePath: {windows_portable.TOP}/hgs-tray.exe\n"
            # The staged CLI, connector and app-local runtime stay with the desktop:
            # winget puts that folder on PATH instead of linking the executable alone.
            + "ArchiveBinariesDependOnPath: true\n"
            + "Commands:\n- hgs\n- hgs-tray\n"
            + "Dependencies:\n  PackageDependencies:\n  - PackageIdentifier: Microsoft.EdgeWebView2Runtime\n"
            + f"ReleaseDate: {release_date}\n"
            + "Installers:\n- Architecture: x64\n"
            + f"  InstallerUrl: {installer_url(version)}\n"
            + f"  InstallerSha256: {sha256.upper()}\n"
            + "ManifestType: installer\n"),
        f"{IDENTIFIER}.locale.en-US.yaml": (
            SCHEMA.format("defaultLocale", MANIFEST_VERSION) + "\n" + common
            + "PackageLocale: en-US\n"
            + "Publisher: Zerus contributors\n"
            + f"PublisherUrl: https://github.com/{REPOSITORY}\n"
            + f"PublisherSupportUrl: https://github.com/{REPOSITORY}/issues\n"
            + "PackageName: Zerus\n"
            + "PackageUrl: https://zerus.dev\n"
            + "License: MIT\n"
            + f"LicenseUrl: https://github.com/{REPOSITORY}/blob/main/LICENSE\n"
            + "Copyright: Copyright (c) 2026 Zerus contributors\n"
            + f"ShortDescription: {SHORT_DESCRIPTION}\n"
            + "Description: |-\n" + "".join(f"  {line}\n" for line in DESCRIPTION.splitlines())
            + "Moniker: zerus\n"
            + "Tags:\n" + "".join(f"- {tag}\n" for tag in TAGS)
            + f"ReleaseNotesUrl: https://github.com/{REPOSITORY}/releases/tag/v{version}\n"
            + "Documentations:\n- DocumentLabel: Installation\n"
            + f"  DocumentUrl: https://github.com/{REPOSITORY}/blob/main/docs/installation.md#windows\n"
            + "ManifestType: defaultLocale\n"),
    }
    return {f"{PACKAGE_PATH}/{version}/{name}": HEADER + text + f"ManifestVersion: {MANIFEST_VERSION}\n"
            for name, text in files.items()}
