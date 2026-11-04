"""Shared release validation; no build or publication side effects."""
import gzip
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tarfile

from macos_release import cask_for, macos_assets, require_sealed_cask, validate_macos_info

REPOSITORY = "ufna/zerus"
PACKAGES = ("zerus", "zerus-ade-bin", "zerus-git")
BINARY_LIBRARIES = ("webkit2gtk-4.1", "gtk3", "gdk-pixbuf2", "glib2", "cairo", "libsoup3",
                   "alsa-lib", "gstreamer", "gst-plugins-base-libs",
                   "libappindicator", "dbus", "libgcc", "libstdc++", "glibc")
# Previously published Qt packages remain immutable and retryable.
QT_BINARY_LIBRARIES = ("qt6-base", "qt6-multimedia", "qt6-svg", "kstatusnotifieritem",
                       "kwindowsystem", "kidletime", "libgcc", "libstdc++", "glibc")
# Candidates sealed before Qt WebEngine became optional (zerus-abnk, 2026-10-13)
# required it, and their recipes lacked its optional-dependency line. Retrying
# their publication must keep working unchanged.
LEGACY_BINARY_LIBRARIES = ("qt6-base", "qt6-webengine", "qt6-multimedia", "qt6-svg", "kstatusnotifieritem",
                           "kwindowsystem", "kidletime", "libgcc", "libstdc++", "glibc")
OPTIONAL_WEBENGINE = "            'qt6-webengine: embedded DeepSeek Native UI'\n"
# The newest stable release sealed without the tested Windows portable zip. Every
# later stable candidate must carry it: portable Windows apps announce those
# releases as Windows updates (lastStableWithoutWindows in tray/src/UpdateController.cpp).
LAST_STABLE_WITHOUT_WINDOWS = "0.42.2"
MAINTAINER = "# Maintainer: Vladimir Alyamkin <ufna@ufna.dev>"
DESCRIPTION_UPDATES = (
    ("An agent development environment for persistent local and remote tmux sessions",
     "Agent development environment for AI coding agents across your machines"),
    ("Nightly agent development environment for persistent local and remote tmux sessions",
     "Agent development environment for AI coding agents across machines (nightly)"),
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def gh(*arguments):
    return subprocess.check_output(["gh", *arguments], text=True).strip()


def api(endpoint):
    return json.loads(gh("api", f"repos/{REPOSITORY}/{endpoint}"))


def metadata(text):
    result = {}
    for line in text.splitlines():
        if " = " in line:
            key, value = line.strip().split(" = ", 1)
            result.setdefault(key, []).append(value)
    return result


def binary_library_names(floors):
    """The ordered build libraries of a sealed candidate's floors: current or legacy."""
    for names in (BINARY_LIBRARIES, QT_BINARY_LIBRARIES, LEGACY_BINARY_LIBRARIES):
        if set(floors) == set(names):
            return names
    raise ValueError("Missing binary build-library provenance.")


def binary_dependencies(buildinfo):
    installed = metadata(buildinfo).get("installed", [])
    records = []
    for identifier in installed:
        if identifier.endswith(("-x86_64", "-any")):
            identifier = identifier.rsplit("-", 1)[0]
        # Package names contain hyphens; Arch versions and pkgrel do not.
        # Match the whole name rather than including similarly named backends.
        record = identifier.rsplit("-", 2)
        if len(record) == 3:
            records.append(record)
    versions = {}
    for package in BINARY_LIBRARIES:
        matches = [(version, release) for name, version, release in records if name == package]
        require(len(matches) == 1, f"Missing/ambiguous binary build dependency: {package}")
        version, release = matches[0]
        require(re.fullmatch(r"\d+(?:\.\d+)*", release), "Unexpected dependency package release/architecture.")
        require(re.fullmatch(r"[0-9][0-9A-Za-z.+:~_-]*", version), "Unexpected binary library version.")
        versions[package] = version
    return versions


def finish_candidate(directory, packages=PACKAGES):
    """Seal generated AUR metadata and all flat release assets after makepkg checks."""
    info = json.loads((directory / "release-info.json").read_text())
    info.update(schema=1, aur_packages=list(packages))
    if "macos" in info:
        # Schema 2 only adds the signed macOS asset and its cask; Arch fields stay as in schema 1.
        info.update(schema=2, homebrew_casks=[cask_for(info.get("channel", "stable"))])
    (directory / "release-info.json").write_text(json.dumps(info, indent=2) + "\n")
    archive = directory / f"zerus-{info['version']}-aur.tar.gz"
    with archive.open("wb") as output, gzip.GzipFile(filename="", fileobj=output, mode="wb", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as bundled:
            for package in packages:
                for name in ("PKGBUILD", ".SRCINFO"):
                    path = directory / "aur" / package / name
                    member = bundled.gettarinfo(str(path), arcname=f"aur/{package}/{name}")
                    member.uid = member.gid = 0
                    member.uname = member.gname = "root"
                    member.mtime = 0
                    with path.open("rb") as content:
                        bundled.addfile(member, content)
    # Containers are sealed too; validation accepts only declared platform assets.
    assets = sorted([*directory.glob("*.tar.gz"), *directory.glob("*.pkg.tar.zst"), *directory.glob("*.zip"),
                     *directory.glob("*.dmg"), *directory.glob("*.exe"), directory / "release-info.json"])
    (directory / "SHA256SUMS").write_text("".join(f"{sha256(path)}  {path.name}\n" for path in assets))


def validate_candidate(directory, version, commit=None):
    import windows_portable  # It imports this module, so only once this one is loaded.
    require(re.fullmatch(r"\d+\.\d+\.\d+", version), "Use a stable semantic version without v.")
    info = json.loads((directory / "release-info.json").read_text())
    schema = info.get("schema")
    require(schema in (1, 2) and info.get("version") == version, "Candidate schema/version mismatch; regenerate old candidates.")
    require(schema == 2 or ("macos" not in info and "homebrew_casks" not in info), "Schema 1 candidates carry no macOS asset.")
    macos = validate_macos_info(info, version) if schema == 2 else None
    require(info.get("architecture") == "x86_64" and info.get("distribution") == "Arch Linux", "Only Arch x86_64 is supported.")
    require(info.get("publication") == "candidate" and info.get("native_agents_bundled") is False, "Unexpected candidate manifest.")
    require(info.get("aur_packages") == list(PACKAGES), "Unexpected AUR package family.")
    require(re.fullmatch(r"[0-9a-f]{40}", info.get("source_commit", "")), "Missing exact source commit.")
    require(commit is None or info["source_commit"] == commit, "Candidate belongs to another commit.")
    sums = {}
    for line in (directory / "SHA256SUMS").read_text().splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.+-]+)", line)
        require(match is not None, "Invalid checksum entry.")
        digest, name = match.groups()
        require(name not in sums and name not in {".", "..", "SHA256SUMS"}, "Duplicate/unsafe checksum name.")
        path = directory / name
        require(path.is_file() and not path.is_symlink() and sha256(path) == digest, f"Checksum mismatch: {name}")
        sums[name] = digest
    required = {f"zerus-{version}-{suffix}.tar.gz" for suffix in ("source", "arch-x86_64", "aur")} | {"release-info.json"}
    if macos:
        required.update(macos_assets(macos))
    # Stable candidates sealed before the Windows zip joined them (0.42.2 and
    # earlier) have none; retrying their publication must keep working. Later ones
    # must carry it, and a manifest that names the zip requires exactly that file.
    windows = info.get("windows_portable")
    later = tuple(map(int, version.split("."))) > tuple(map(int, LAST_STABLE_WITHOUT_WINDOWS.split(".")))
    require(windows is not None or not later,
            f"Stable candidates after {LAST_STABLE_WITHOUT_WINDOWS} must carry the tested Windows zip.")
    if windows is not None:
        require(windows == windows_portable.asset_name(version), "Invalid Windows stable asset.")
        required.add(windows)
    installer = info.get("windows_installer")
    if installer is not None:
        import windows_installer_release
        required.add(windows_installer_release.validate_metadata(installer, version, windows, sums))
    package_files = {name for name in sums if name.endswith(".pkg.tar.zst")}
    patterns = [re.fullmatch(rf"(zerus|zerus-ade-bin)-{re.escape(version)}-\d+-x86_64\.pkg\.tar\.zst", name) for name in package_files]
    require(len(package_files) == 2 and all(patterns) and {match[1] for match in patterns if match} == {"zerus", "zerus-ade-bin"}, "Expected checked stable source and binary x86_64 pacman packages.")
    require(set(sums) == required | package_files, "Unexpected/missing release assets.")
    directories = {"aur", "homebrew"} if macos else {"aur"}
    flat_files = {path.name for path in directory.iterdir() if path.name not in directories}
    require(flat_files == set(sums) | {"SHA256SUMS"}, "Unsealed candidate files.")
    if windows is not None:
        # Bounded reads of the zip directory, build-info.json and the pinned ConPTY
        # pair; nothing is extracted or run.
        windows_portable.require_build(directory / windows, info["source_commit"], version)
    if macos:
        casks = sorted(path.name for path in (directory / "homebrew").iterdir())
        cask = directory / "homebrew/zerus.rb"
        require(casks == ["zerus.rb"] and cask.is_file() and not cask.is_symlink(), "Unexpected Homebrew cask files.")
        require_sealed_cask(cask.read_text(), version, sums[macos["asset"]])
    expected_members = {f"aur/{package}/{name}" for package in PACKAGES for name in ("PKGBUILD", ".SRCINFO")}
    with tarfile.open(directory / f"zerus-{version}-aur.tar.gz", "r:gz") as archive:
        members = archive.getmembers()
        require(len(members) == len(expected_members) and {member.name for member in members} == expected_members, "Unexpected AUR archive contents.")
        for member in members:
            require(member.isfile() and member.size < 100_000, "Unsafe AUR archive entry.")
            path = directory / member.name
            require(path.is_file() and not path.is_symlink() and path.read_bytes() == archive.extractfile(member).read(), "AUR recipes do not match sealed archive.")
    source_hash = sums[f"zerus-{version}-source.tar.gz"]
    binary_hash = sums[f"zerus-{version}-arch-x86_64.tar.gz"]
    for package in PACKAGES:
        recipe = (directory / "aur" / package / "PKGBUILD").read_text()
        data = metadata((directory / "aur" / package / ".SRCINFO").read_text())
        require(recipe.startswith(MAINTAINER + "\n"), f"Unapproved maintainer: {package}")
        require(data.get("pkgbase") == [package] and data.get("pkgname") == [package] and data.get("arch") == ["x86_64"], f"Wrong package metadata: {package}")
        if package == "zerus-git":
            require(data.get("source") == ["zerus::git+https://github.com/ufna/zerus.git#branch=main"] and data.get("sha256sums") == ["SKIP"], "VCS package must track public main.")
        else:
            suffix, digest = ("source", source_hash) if package == "zerus" else ("arch-x86_64", binary_hash)
            url = f"https://github.com/{REPOSITORY}/releases/download/v{version}/zerus-{version}-{suffix}.tar.gz"
            require(data.get("pkgver") == [version] and data.get("source") == [url] and data.get("sha256sums") == [digest], f"Wrong stable source/hash: {package}")
            if package == "zerus-ade-bin":
                floors = info.get("binary_dependencies", {})
                binary_library_names(floors)
                require(all(f"{name}>={value}" in data.get("depends", []) for name, value in floors.items()), "Binary dependencies omit build-library version floors.")
    return info, sums


def recipe_changed(old, new, vcs=False):
    if vcs:
        # AUR forbids commits that merely bump a VCS pkgver. Preserve its last
        # published seed when the recipe/build/dependencies are otherwise identical.
        pattern = r"(?m)^(?:pkgver=|[ \t]*pkgver = |[ \t]*provides = zerus=).*$"
        old = re.sub(pattern, "", old)
        new = re.sub(pattern, "", new)
    return old != new


def retains_description_update(old, new, vcs=False):
    """Keep an approved AUR description when retrying an older sealed recipe."""
    for previous, current in DESCRIPTION_UPDATES:
        for prefix, suffix in (("pkgdesc='", "'"), ("\tpkgdesc = ", "")):
            previous_line = prefix + previous + suffix + "\n"
            current_line = prefix + current + suffix + "\n"
            if current_line in old.splitlines(keepends=True) and previous_line in new.splitlines(keepends=True):
                refreshed = "".join(current_line if line == previous_line else line
                                    for line in new.splitlines(keepends=True))
                return not recipe_changed(old, refreshed, vcs=vcs)
    return False


def package_version(data):
    version = data["pkgver"][0]
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:\.r(\d+)\.g[0-9a-f]+(?:\.n(\d+))?)?", version)
    require(match is not None, "Unexpected existing package version; inspect manually.")
    return tuple(int(part or 0) for part in match.groups()) + (int(data["pkgrel"][0]),)
