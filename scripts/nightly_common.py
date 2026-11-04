"""Validate immutable nightly candidates without executing downloaded recipes."""
import json
from pathlib import Path
import re
import tarfile

from macos_release import macos_assets, require_sealed_cask, validate_macos_info
from release_common import (BINARY_LIBRARIES, QT_BINARY_LIBRARIES, LEGACY_BINARY_LIBRARIES,
                            OPTIONAL_WEBENGINE, binary_library_names, metadata,
                            retains_description_update, require, sha256)
import windows_portable

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "zerus-ade-nightly-bin"
PACKAGES = (PACKAGE,)


def windows_asset(version):
    return windows_portable.asset_name(version)


def nightly_version(product, count, commit, number):
    require(re.fullmatch(r"\d+\.\d+\.\d+", product or ""), "Invalid nightly product version.")
    require(re.fullmatch(r"[0-9a-f]{40}", commit or ""), "Invalid nightly source commit.")
    require(type(count) is int and count > 0 and type(number) is int and number > 0,
            "Invalid nightly revision/build number.")
    return f"{product}.r{count}.g{commit[:7]}.n{number}"


def render_recipe(info, binary_hash):
    floors = info["binary_dependencies"]
    names = binary_library_names(floors)
    values = {"@VERSION@": info["version"], "@PRODUCT_VERSION@": info["product_version"],
              "@RELEASE_TAG@": info["release_tag"], "@BINARY_SHA256@": binary_hash,
              "@BINARY_LIBRARY_DEPENDS@": " ".join(f"'{name}>={floors[name]}'" for name in names)}
    text = (ROOT / "packaging/aur" / PACKAGE / "PKGBUILD.in").read_text()
    if names != BINARY_LIBRARIES:
        # Retry the original immutable Qt recipe, without adopting Next codecs.
        text = text.replace("'gst-plugins-base' 'gst-plugins-good' 'gst-plugins-bad' 'gst-plugins-ugly' 'gst-libav'\n         ", "")
        if names == QT_BINARY_LIBRARIES:
            text = text.replace("optdepends=('konsole: external KDE terminal integration'\n",
                                "optdepends=('konsole: external KDE terminal integration'\n" + OPTIONAL_WEBENGINE)
    for key, value in values.items():
        text = text.replace(key, value)
    return text


def validate_nightly(directory, commit=None, run_id=None, run_number=None):
    info = json.loads((directory / "release-info.json").read_text())
    schema = info.get("schema")
    require(schema in (1, 2) and info.get("channel") == "nightly" and
            info.get("publication") == "candidate", "Not a sealed nightly candidate.")
    require(schema == 2 or ("macos" not in info and "homebrew_casks" not in info),
            "Schema 1 nightlies carry no macOS asset.")
    require(info.get("architecture") == "x86_64" and info.get("distribution") == "Arch Linux" and
            info.get("native_agents_bundled") is False, "Unexpected nightly platform/runtime.")
    require(info.get("aur_packages") == list(PACKAGES), "Unexpected nightly package family.")
    expected_version = nightly_version(info.get("product_version"), info.get("revision_count"),
                                       info.get("source_commit"), info.get("workflow_run_number"))
    require(info.get("version") == expected_version, "Nightly version/provenance mismatch.")
    identifier = info.get("workflow_run_id")
    require(type(identifier) is int and identifier > 0 and
            info.get("release_tag") == f"nightly-{identifier}", "Invalid immutable nightly tag.")
    require(commit is None or info["source_commit"] == commit, "Nightly belongs to another commit.")
    require(run_id is None or identifier == run_id, "Nightly belongs to another workflow run.")
    require(run_number is None or info["workflow_run_number"] == run_number, "Nightly build number mismatch.")
    floors = info.get("binary_dependencies", {})
    require(isinstance(floors, dict), "Invalid nightly binary library provenance.")
    binary_library_names(floors)
    require(all(re.fullmatch(r"[0-9][0-9A-Za-z.+:~_-]*", str(value)) for value in floors.values()),
            "Invalid nightly binary library provenance.")
    version = info["version"]
    # Schema 2 adds the notarized Apple Silicon bundle and the zerus@nightly cask.
    macos = validate_macos_info(info, version, "nightly") if schema == 2 else None
    expected_files = {f"zerus-{version}-arch-x86_64.tar.gz", f"zerus-{version}-aur.tar.gz",
                      f"{PACKAGE}-{version}-1-x86_64.pkg.tar.zst", "release-info.json"}
    # Nightlies built before the Windows job have no portable zip; retrying their
    # publication must keep working. A manifest that names the zip requires it.
    windows = info.get("windows_portable")
    if windows is not None:
        require(windows == windows_asset(version), "Invalid Windows nightly asset.")
        expected_files.add(windows)
    installer = info.get("windows_installer")
    if installer is not None:
        import windows_installer_release
        require(isinstance(installer, dict) and installer.get("asset") == windows_installer_release.installer_name(version),
                "Invalid Windows nightly installer asset.")
        expected_files.add(installer["asset"])
    if macos:
        expected_files.update(macos_assets(macos))
    sums = {}
    for line in (directory / "SHA256SUMS").read_text().splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.+-]+)", line)
        require(match is not None, "Invalid nightly checksum entry.")
        digest, name = match.groups()
        require(name in expected_files and name not in sums, "Unexpected/duplicate nightly asset.")
        path = directory / name
        require(path.is_file() and not path.is_symlink() and sha256(path) == digest,
                f"Nightly checksum mismatch: {name}")
        sums[name] = digest
    directories = {"aur", "homebrew"} if macos else {"aur"}
    require(set(sums) == expected_files and
            {path.name for path in directory.iterdir()} == expected_files | directories | {"SHA256SUMS"},
            "Unsealed/missing nightly assets.")
    if installer is not None:
        windows_installer_release.validate_metadata(installer, version, windows, sums)
    if windows is not None:
        # Bounded reads of the zip directory, build-info.json and the pinned ConPTY
        # pair; nothing is extracted or run.
        windows_portable.require_build(directory / windows, info["source_commit"], info["product_version"])
    if macos:
        cask = directory / "homebrew/zerus@nightly.rb"
        require(sorted(path.name for path in (directory / "homebrew").iterdir()) == [cask.name] and
                cask.is_file() and not cask.is_symlink(), "Unexpected Homebrew cask files.")
        require_sealed_cask(cask.read_text(), version, sums[macos["asset"]], "nightly", info["release_tag"])
    members_expected = {f"aur/{PACKAGE}/{name}" for name in ("PKGBUILD", ".SRCINFO")}
    with tarfile.open(directory / f"zerus-{version}-aur.tar.gz", "r:gz") as archive:
        members = archive.getmembers()
        require(len(members) == 2 and {member.name for member in members} == members_expected,
                "Unexpected nightly AUR archive.")
        for member in members:
            require(member.isfile() and member.size < 100_000, "Unsafe nightly recipe entry.")
            path = directory / member.name
            require(path.is_file() and not path.is_symlink() and
                    path.read_bytes() == archive.extractfile(member).read(), "Unsealed nightly recipe.")
    binary_hash = sums[f"zerus-{version}-arch-x86_64.tar.gz"]
    recipe = directory / "aur" / PACKAGE
    actual_recipe = (recipe / "PKGBUILD").read_text()
    trusted_recipe = render_recipe(info, binary_hash)
    require(actual_recipe == trusted_recipe or retains_description_update(trusted_recipe, actual_recipe),
            "Nightly recipe differs from the trusted binary template.")
    data = metadata((recipe / ".SRCINFO").read_text())
    url = f"https://github.com/ufna/zerus/releases/download/{info['release_tag']}/zerus-{version}-arch-x86_64.tar.gz"
    require(data.get("pkgbase") == [PACKAGE] and data.get("pkgname") == [PACKAGE] and
            data.get("pkgver") == [version] and data.get("pkgrel") == ["1"] and
            data.get("arch") == ["x86_64"], "Wrong nightly package metadata.")
    require(data.get("source") == [url] and data.get("sha256sums") == [binary_hash],
            "Nightly recipe must use its immutable URL and checksum.")
    dependencies = {f"{name}>={value}" for name, value in floors.items()} | {
        "tmux>=3.7", "openssh", "python", "curl", "procps-ng", "hicolor-icon-theme", "bash", "tar"}
    require(set(data.get("depends", [])) == dependencies,
            "Nightly dependencies omit or change runtime/build-library requirements.")
    require(set(data.get("conflicts", [])) == {"zerus", "zerus-git", "zerus-ade-bin"} and
            set(data.get("provides", [])) == {f"zerus={version}", "hgs", "hgs-tray"} and
            not data.get("makedepends"), "Unexpected nightly conflicts/provides/build dependencies.")
    return info, sums
