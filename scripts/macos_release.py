"""Homebrew cask rendering and macOS candidate metadata; no build or publication side effects."""
import hashlib
import json
from pathlib import Path
import re
import shutil

ROOT = Path(__file__).resolve().parents[1]
# Every channel installs the same Zerus.app; the casks conflict with each other.
CASKS = {"stable": "zerus", "nightly": "zerus@nightly", "dev": "zerus@dev"}
MINIMUM_SYSTEM = "14.0"
# Both existing Qt and Next feed parsers require this semantic-version field.
# In a Next candidate it is a compatibility marker, not a bundled dependency.
LEGACY_QT_VERSION = "6.11.2"
STABLE = re.compile(r"\d+\.\d+\.\d+")
NIGHTLY = re.compile(r"\d+\.\d+\.\d+\.r\d+\.g[0-9a-f]{7}\.n(\d+)")
DEV = re.compile(r"\d+\.\d+\.\d+-dev\.([1-9]\d*)")
NIGHTLY_TAG = re.compile(r"nightly-[1-9]\d*")
DIGEST = re.compile(r"[0-9a-f]{64}")
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def _require(condition, message):
    if not condition:
        raise ValueError(message)


def macos_asset(version):
    return f"zerus-{version}-macos-arm64.zip"


def macos_dmg(version):
    return f"zerus-{version}-macos-arm64.dmg"


def macos_assets(macos):
    """Asset names from already validated metadata, including legacy ZIP-only releases."""
    return [macos["asset"], *([macos["dmg"]["asset"]] if "dmg" in macos else [])]


def cask_for(channel):
    _require(channel in CASKS, "Unknown Homebrew channel.")
    return CASKS[channel]


def _channel_version(version, channel):
    cask_for(channel)
    pattern = {"stable": STABLE, "nightly": NIGHTLY, "dev": DEV}[channel]
    _require(isinstance(version, str) and pattern.fullmatch(version), f"Cask version must be a {channel} version.")


def _render_cask(text, version, digest, channel, tag):
    _channel_version(version, channel)
    _require(DIGEST.fullmatch(digest), "Cask digest must be a lowercase SHA-256.")
    if channel == "nightly":
        # Nightly prereleases are immutable; the tag holds the workflow run ID, not the version.
        _require(isinstance(tag, str) and NIGHTLY_TAG.fullmatch(tag), "Nightly cask needs its nightly release tag.")
        text = text.replace("@RELEASE_TAG@", tag)
    elif channel == "dev":
        _require(isinstance(tag, str) and re.fullmatch(r"dev-desktop-[1-9]\d*", tag),
                 "Dev cask needs its immutable mirror build directory.")
        text = text.replace("@BUILD_DIRECTORY@", tag)
    return text.replace("@VERSION@", version).replace("@SHA256@", digest)


def render_cask(version, digest, channel="stable", tag=None):
    text = (ROOT / "packaging/homebrew" / f"{cask_for(channel)}.rb.in").read_text()
    return _render_cask(text, version, digest, channel, tag)


def cask_fields(text):
    fields = {}
    for key in ("version", "sha256"):
        values = re.findall(rf'(?m)^  {key} "([^"]*)"$', text)
        _require(len(values) == 1, f"Cask must declare exactly one {key}.")
        fields[key] = values[0]
    return fields


def require_template_cask(text, version, digest, channel="stable", tag=None):
    _require(text == render_cask(version, digest, channel, tag), "Homebrew cask differs from the reviewed template.")


def reviewed_cask_templates(version, digest, channel="stable", tag=None):
    """Expected bytes for validating existing casks, never for creating new ones."""
    templates = [render_cask(version, digest, channel, tag)]
    if channel in ("stable", "nightly"):
        previous = (ROOT / "packaging/homebrew/previous" / f"{cask_for(channel)}.rb.in").read_text()
        templates.append(_render_cask(previous, version, digest, channel, tag))
    return tuple(templates)


def require_sealed_cask(text, version, digest, channel="stable", tag=None):
    """Retain the exact pre-Next cask when retrying a sealed stable/nightly candidate.

    New candidates always render the current template. Old ones must not be
    rebuilt or rewritten just because the reviewed template changed.
    """
    _require(text in reviewed_cask_templates(version, digest, channel, tag),
             "Homebrew cask differs from the reviewed template.")


def cask_version_key(version, channel="stable"):
    try:
        _channel_version(version, channel)
    except ValueError:
        raise ValueError("Unexpected cask version; inspect the tap manually.") from None
    if channel in ("nightly", "dev"):
        # Build numbers of the Nightly workflow only grow; the product version may not.
        return (int((DEV if channel == "dev" else NIGHTLY).fullmatch(version).group(1)),)
    return tuple(int(part) for part in version.split("."))


def validate_macos_info(info, version, channel="stable"):
    """Check the provenance recorded by the signing job for a schema 2 candidate."""
    macos = info.get("macos")
    fields = {"asset", "qt_version", "team_id", "notarization_id", "minimum_system_version"}
    _require(isinstance(macos, dict) and set(macos) in (fields, fields | {"dmg"}), "Missing macOS provenance.")
    _require(macos["asset"] == macos_asset(version), "Unexpected macOS provenance: asset name.")
    _require(isinstance(macos["qt_version"], str) and STABLE.fullmatch(macos["qt_version"]),
             "Unexpected macOS provenance: Qt version.")
    _require(isinstance(macos["team_id"], str) and re.fullmatch(r"[A-Z0-9]{10}", macos["team_id"]),
             "Unexpected macOS provenance: Team ID.")
    _require(isinstance(macos["notarization_id"], str) and
             UUID.fullmatch(macos["notarization_id"]),
             "Unexpected macOS provenance: notarization ID.")
    if "dmg" in macos:
        dmg = macos["dmg"]
        _require(isinstance(dmg, dict) and set(dmg) == {"asset", "notarization_id"} and
                 dmg["asset"] == macos_dmg(version) and isinstance(dmg["notarization_id"], str) and
                 UUID.fullmatch(dmg["notarization_id"]), "Unexpected macOS disk image provenance.")
    _require(macos["minimum_system_version"] == MINIMUM_SYSTEM, "Unexpected macOS provenance: minimum system version.")
    _require(info.get("homebrew_casks") == [cask_for(channel)], "Unexpected Homebrew cask family.")
    return macos


def catalog_macos(metadata, version, channel, prefix, seals):
    """An additive catalog field; legacy clients keep their existing artifact list."""
    validate_macos_info({"macos": metadata, "homebrew_casks": [cask_for(channel)]}, version, channel)
    downloads = []
    for name in macos_assets(metadata):
        seal = seals[name]
        downloads.append({"name": name, "url": prefix + name, "size": seal["size"], "sha256": seal["sha256"]})
    result = dict(metadata, downloads=downloads)
    validate_catalog_macos(result, version, channel, prefix)
    return result


def validate_catalog_macos(value, version, channel, prefix):
    """Validate names, signing provenance and exact download origins before using URLs."""
    _channel_version(version, channel)
    _require(isinstance(value, dict), "Invalid macOS catalog metadata.")
    metadata = {key: item for key, item in value.items() if key != "downloads"}
    validate_macos_info({"macos": metadata, "homebrew_casks": [cask_for(channel)]}, version, channel)
    rows = value.get("downloads")
    names = macos_assets(metadata)
    _require(isinstance(rows, list) and len(rows) == len(names), "Missing macOS catalog downloads.")
    for name, row in zip(names, rows):
        _require(isinstance(row, dict) and set(row) == {"name", "url", "size", "sha256"}
                 and row["name"] == name and row["url"] == prefix + name
                 and type(row["size"]) is int and 0 < row["size"] <= 512 * 1024**2
                 and isinstance(row["sha256"], str) and DIGEST.fullmatch(row["sha256"]),
                 "Invalid macOS catalog download seal.")
    return rows


def add_macos(output, source, version, info, channel="stable"):
    """Copy the signed, notarized macOS asset into a candidate and render its cask.

    Everything is checked before the candidate changes, so a rejected artifact
    leaves no partial macOS files behind.
    """
    manifest = source / "macos-info.json"
    _require(manifest.is_file() and not manifest.is_symlink(), "Missing regular macOS metadata.")
    metadata = json.loads(manifest.read_text())
    cask = cask_for(channel)
    validate_macos_info(dict(info, macos=metadata, homebrew_casks=[cask]), version, channel)
    expected = {*macos_assets(metadata), "macos-info.json"}
    entries = list(source.iterdir())
    _require({path.name for path in entries} == expected and
             all(path.is_file() and not path.is_symlink() for path in entries),
             "Unexpected signed macOS artifact contents.")
    tag = info.get("release_tag") if channel == "nightly" else None
    render_cask(version, "0" * 64, channel, tag)
    archive = output / macos_asset(version)
    for name in macos_assets(metadata):
        shutil.copyfile(source / name, output / name)
    with archive.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    (output / "homebrew").mkdir()
    (output / "homebrew" / f"{cask}.rb").write_text(render_cask(version, digest, channel, tag))
    info["macos"] = metadata
    return metadata
