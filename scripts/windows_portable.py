"""Inspect the Windows portable zip without extracting or executing anything."""
import json
from pathlib import Path
import re
import stat
import zipfile
import zlib

import conpty_pair
from release_common import require

TOP = "zerus-windows-x86_64"
# GitHub release mirror limit for a single asset (update_feed.ASSET_LIMIT).
ZIP_LIMIT = 512 * 1024 * 1024
# The same bound fetch-release-candidate.py applies to a whole candidate.
UNPACKED_LIMIT = 500_000_000
ENTRY_LIMIT = 10_000
MANIFEST_LIMIT = 64 * 1024
# Standard SPDX license and exception identifiers named by the SPDX documents
# of the bundled Qt modules; packaging/windows/licenses holds a <ID>.txt for each.
QT_LICENSE_TEXTS = (
    "AFL-2.1", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "BSD-4-Clause", "BSD-Source-Code", "BSL-1.0", "CC0-1.0", "FTL",
    "GPL-2.0-only", "GPL-2.0-or-later", "GPL-3.0-only", "HPND", "HPND-sell-variant", "IJG", "ISC", "Imlib2",
    "LGPL-2.1-or-later", "LGPL-3.0-only", "LLVM-exception", "Libpng", "Linux-syscall-note", "MIT", "MIT-0", "MIT-open-group", "MPL-2.0",
    "Qt-GPL-exception-1.0", "Unicode-3.0", "Unlicense", "X11", "Zlib", "blessing", "libpng-2.0",
)
# The tested executables, the Qt runtime windeployqt must deploy, the app-local
# MSVC runtime, the pinned ConPTY pair next to hgs.exe (conpty_pair.py) and the
# notices that make the bundle redistributable.
REQUIRED = (
    "hgs.exe", "hgs-tray.exe", "build-info.json", "README.txt", "LICENSE", "THIRD_PARTY_NOTICES.md",
    "Qt6Core.dll", "Qt6Gui.dll", "Qt6Widgets.dll", "Qt6Network.dll", "Qt6Svg.dll", "Qt6Multimedia.dll",
    "platforms/qwindows.dll", "imageformats/qsvg.dll", "iconengines/qsvgicon.dll",
    "multimedia/ffmpegmediaplugin.dll", "multimedia/windowsmediaplugin.dll",
    "avcodec-61.dll", "avformat-61.dll", "avutil-59.dll", "swresample-5.dll", "swscale-8.dll",
    "vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll", "conpty.dll", "OpenConsole.exe",
    "licenses/libvterm-LICENSE", "licenses/md4c-LICENSE", "licenses/qrcodegen-LICENSE", "licenses/conpty-LICENSE.txt",
    "licenses/conpty-NOTICE.md", "licenses/rust/inventory.json",
    *(f"licenses/qt/{name}.txt" for name in QT_LICENSE_TEXTS),
)
# New packages use Tauri and the separately installed Evergreen runtime. Keep
# legacy inspection for immutable published Qt archives; new Tauri packages
# cannot quietly carry an old Qt runtime alongside the replacement executable.
NEXT_REQUIRED = (
    "hgs.exe", "hgs-tray.exe", "zerus-mobile-connector.exe", "build-info.json", "README.txt", "LICENSE", "THIRD_PARTY_NOTICES.md",
    "vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll", "conpty.dll", "OpenConsole.exe",
    "licenses/conpty-LICENSE.txt", "licenses/conpty-NOTICE.md", "licenses/rust/inventory.json",
    "licenses/rust/desktop/inventory.json", "licenses/frontend/inventory.json",
)
NEXT_FORBIDDEN = re.compile(r"(?i)(?:^|/)(?:qt[^/]*\.(?:dll|exe|pak)|qwindows\.dll)$|/licenses/qt/")
# Never shipped: windeployqt copies ICU from the build machine's System32 (a
# forwarder to icu.dll on current Windows; Qt6Core uses the system ICU), the
# Mesa software OpenGL fallback has no notices in the bundle, and Microsoft's
# Direct3D shader compilers (D3DCompiler_47.dll is part of Windows, DXC of the
# Windows SDK) are not redistributed. Qt WebEngine (Chromium) is not part of
# Zerus on Windows: the embedded native UI is an optional Linux plugin.
FORBIDDEN = re.compile(r"(?i)(?:^|/)(?:(?:icu[^/]*|opengl32sw|d3dcompiler[^/]*|dxcompiler|dxil|qt6webengine[^/]*)\.dll"
                       r"|qtwebengineprocess\.exe|qtwebengine[^/]*\.pak|icudtl\.dat|v8_context_snapshot\.bin)$")
# File names of the ConPTY pair: only the pinned files next to hgs.exe may use them.
CONPTY_NAMES = {name.casefold() for name in conpty_pair.NAMES}
WINDOWS_INVALID = re.compile(r'[\x00-\x1f\x7f<>:"|?*\\]')
WINDOWS_RESERVED = {"CON", "PRN", "AUX", "NUL", *(f"COM{n}" for n in range(1, 10)), *(f"LPT{n}" for n in range(1, 10))}
VERSION = re.compile(r"\d+\.\d+\.\d+")


def asset_name(version):
    """The release asset of the tested zip: renamed for its stable or nightly version, never rebuilt."""
    return f"zerus-{version}-windows-x86_64.zip"


def safe_name(name):
    """A relative file path that extracts identically and safely on Windows."""
    require(isinstance(name, str) and name.startswith(TOP + "/") and not WINDOWS_INVALID.search(name),
            f"Unsafe Windows portable entry: {name!r}")
    for part in name.split("/"):
        require(part not in {"", ".", ".."} and not part.endswith((".", " ")) and
                part.split(".", 1)[0].upper() not in WINDOWS_RESERVED,
                f"Unsafe Windows portable entry: {name!r}")
    return name


def pinned_entry(archive, name, pinned):
    """Whether the bundle's `name` holds the pinned file: a bounded read in memory."""
    entry = archive.getinfo(f"{TOP}/{name}")
    if entry.file_size != pinned["size"]:
        return False
    try:
        with archive.open(entry) as stream:
            data = stream.read(pinned["size"] + 1)
    except (zipfile.BadZipFile, zlib.error, EOFError):
        return False
    return conpty_pair.matches(pinned, data)


def inspect(path, required=None):
    """Return build-info.json after checking every entry, the expected layout and
    the pinned ConPTY pair."""
    path = Path(path)
    require(path.is_file() and not path.is_symlink(), "Windows portable bundle is not a regular file.")
    require(0 < path.stat().st_size <= ZIP_LIMIT, "Windows portable bundle exceeds the release asset limit.")
    try:
        archive = zipfile.ZipFile(path)
    except zipfile.BadZipFile:
        raise ValueError("Windows portable bundle is not a valid zip archive.") from None
    with archive:
        entries = archive.infolist()
        require(0 < len(entries) <= ENTRY_LIMIT, "Unexpected Windows portable entry count.")
        names, folded, total = set(), set(), 0
        for entry in entries:
            # ZipInfo normalizes Windows separators (and truncates NUL) while
            # reading. Validate the original stored spelling before that change
            # can make an ambiguous archive look safe on the current OS.
            require(entry.orig_filename == entry.filename,
                    f"Windows portable entry changes under ZIP decoding: {entry.orig_filename!r}")
            name = safe_name(entry.filename)
            require(not FORBIDDEN.search(name), f"Windows portable bundle must not contain {name}")
            # conpty.dll would run an OpenConsole.exe from a subfolder when none sat next to it.
            require(name.rpartition("/")[2].casefold() not in CONPTY_NAMES or name.count("/") == 1,
                    f"Windows portable ConPTY files belong next to hgs.exe: {name}")
            # Unix file type bits, when present, must say "regular file" (no symlinks).
            require(not entry.is_dir() and stat.S_IFMT(entry.external_attr >> 16) in {0, stat.S_IFREG},
                    f"Windows portable entries must be regular files: {name}")
            require(not entry.flag_bits & 0x1 and entry.compress_type in {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED},
                    f"Unsupported Windows portable entry encoding: {name}")
            # NTFS is case-insensitive: two spellings would overwrite each other.
            require(name.casefold() not in folded, f"Duplicate Windows portable entry: {name}")
            names.add(name)
            folded.add(name.casefold())
            total += entry.file_size
        require(total <= UNPACKED_LIMIT, "Windows portable bundle unpacks beyond its size limit.")
        # `hgs session-host install` stages the pair from the folder of hgs.exe into
        # the host image, which loads conpty.dll and runs OpenConsole.exe (spec D2,
        # D6): a different binary under either name fails.
        pair = {name: f"{TOP}/{name}" in names and pinned_entry(archive, name, pinned)
                for name, pinned in conpty_pair.PIN["files"].items()}
        require(f"{TOP}/build-info.json" in names, "Windows portable bundle lacks: build-info.json")
        info = archive.getinfo(f"{TOP}/build-info.json")
        require(info.file_size <= MANIFEST_LIMIT, "Windows portable build-info.json is too large.")
        with archive.open(info) as stream:
            raw = stream.read(MANIFEST_LIMIT + 1)
    require(len(raw) <= MANIFEST_LIMIT, "Windows portable build-info.json is too large.")
    try:
        manifest = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        raise ValueError("Windows portable build-info.json is not valid JSON.") from None
    require(isinstance(manifest, dict), "Windows portable build-info.json is not an object.")
    require(manifest.get("product") == "Zerus" and manifest.get("platform") == "Windows" and
            manifest.get("architecture") == "x86_64" and manifest.get("native_agents_bundled") is False,
            "Unexpected Windows portable product/platform.")
    if "mobile_connector_bundled" in manifest:
        require(manifest["mobile_connector_bundled"] is True and
                f"{TOP}/zerus-mobile-connector.exe" in names,
                "Windows portable mobile connector declaration requires its executable.")
    desktop = manifest.get("desktop")
    require(desktop in (None, "tauri"), "Unsupported Windows portable desktop.")
    if desktop == "tauri":
        require(manifest.get("webview2_runtime") == "evergreen" and "qt_version" not in manifest,
                "Tauri portable bundles require WebView2 Evergreen, without Qt.")
        require(not any(NEXT_FORBIDDEN.search(name) for name in names),
                "Tauri portable bundle must not contain Qt deployment files.")
    wanted = required if required is not None else (NEXT_REQUIRED if desktop == "tauri" else REQUIRED)
    missing = sorted(item for item in wanted if f"{TOP}/{item}" not in names)
    require(not missing, "Windows portable bundle lacks: " + ", ".join(missing))
    for name, matches in pair.items():
        require(matches, f"Windows portable {name} is not the pinned {conpty_pair.PACKAGE} "
                f"{conpty_pair.PIN['version']} build.")
    for key in (("version", "cli_version") if desktop == "tauri" else ("version", "cli_version", "qt_version")):
        require(isinstance(manifest.get(key), str) and VERSION.fullmatch(manifest[key]),
                f"Invalid Windows portable {key}.")
    require(isinstance(manifest.get("source_commit"), str) and re.fullmatch(r"[0-9a-f]{40}", manifest["source_commit"]),
            "Windows portable bundle lacks its exact source commit.")
    return manifest


def require_build(path, commit, version):
    """build-info.json of the zip, which must be the build of `commit` at product `version`.

    Release assembly and publication call this: like `inspect`, it never extracts
    or runs anything.
    """
    manifest = inspect(path)
    require(manifest["source_commit"] == commit and manifest["version"] == version,
            "Windows portable bundle belongs to another commit/version.")
    return manifest


SPDX_LICENSE_FIELDS = {"PackageLicenseConcluded", "PackageLicenseDeclared", "PackageLicenseInfoFromFiles",
                       "LicenseConcluded", "LicenseInfoInFile"}
SPDX_KEYWORDS = {"AND", "OR", "WITH", "NOASSERTION", "NONE"}


def spdx_tags(text):
    """(tag, value) pairs of an SPDX tag-value document; <text> values may span lines."""
    lines = iter(text.splitlines())
    for line in lines:
        tag, separator, value = line.partition(":")
        if not separator or not re.fullmatch(r"[A-Za-z]+", tag):
            continue
        value = value.strip()
        if value.startswith("<text>"):
            parts = [value[len("<text>"):]]
            while "</text>" not in parts[-1]:
                following = next(lines, None)
                require(following is not None, f"Unterminated <text> value for {tag} in an SPDX document.")
                parts.append(following)
            parts[-1] = parts[-1][:parts[-1].index("</text>")]
            value = "\n".join(parts)
        yield tag, value


def sbom_licenses(text):
    """License and exception identifiers an SPDX document names, and the LicenseRef
    identifiers it defines with a non-empty extracted text."""
    named, defined, license_id = set(), set(), None
    for tag, value in spdx_tags(text):
        if tag in SPDX_LICENSE_FIELDS:
            named.update(token for token in re.split(r"[\s()]+", value) if token and token not in SPDX_KEYWORDS)
        elif tag == "LicenseID":
            license_id = value
        elif tag == "ExtractedText":
            if license_id and value.strip():
                defined.add(license_id)
            license_id = None
    return named, defined


def missing_license_texts(documents, texts):
    """'document: identifier' for each license the documents name without a text:
    a standard identifier needs <ID>.txt among texts, a LicenseRef its extracted
    text in the same document (or, through a DocumentRef, in another one)."""
    parsed = {name: sbom_licenses(text) for name, text in documents.items()}
    everywhere = set().union(*(defined for _, defined in parsed.values()))
    missing = []
    for name, (named, defined) in sorted(parsed.items()):
        for identifier in sorted(named):
            reference = identifier.rpartition(":")[2]
            if reference.startswith("LicenseRef-"):
                known = reference in (everywhere if identifier.startswith("DocumentRef-") else defined)
            else:
                known = identifier in texts
            if not known:
                missing.append(f"{name}: {identifier}")
    return missing
