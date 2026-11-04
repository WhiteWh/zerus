#!/usr/bin/env python3
"""Publish an immutable verified GitHub release, then synchronize its AUR recipes and Homebrew cask."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

from macos_release import CASKS, cask_fields, cask_for, cask_version_key
from release_common import (PACKAGES, REPOSITORY, api, gh, metadata, package_version,
                            recipe_changed, retains_description_update, require, sha256, validate_candidate)

HOST_FINGERPRINT = "SHA256:RFzBCUItH9LZS0cKB5UE6ceAYhBD5C8GeOBip8Z11+4"
# GitHub's published Ed25519 host key fingerprint; packaging/homebrew/known_hosts pins the key.
GITHUB_FINGERPRINT = "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU"
TAP_REMOTE = "git@github.com:ufna/homebrew-zerus.git"


def git(directory, *arguments, env=None):
    return subprocess.check_output(["git", "-C", str(directory), *arguments], env=env, text=True).strip()


def ssh_environment(identity, hosts, expected=HOST_FINGERPRINT, service="AUR"):
    fingerprint = subprocess.check_output(["ssh-keygen", "-lf", str(hosts), "-E", "sha256"], text=True)
    require(len(fingerprint.splitlines()) == 1 and fingerprint.split()[1] == expected, f"Unverified {service} host key; consult the official {service} documentation.")
    require(identity.is_file() and identity.stat().st_mode & 0o077 == 0, f"{service} private key must be a private file (0600).")
    command = ["ssh", "-F", "/dev/null", "-i", str(identity.resolve()), "-o", "IdentitiesOnly=yes",
               "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", "-o", "HostKeyAlgorithms=ssh-ed25519",
               "-o", f"UserKnownHostsFile={hosts.resolve()}", "-o", "ConnectTimeout=10"]
    env = os.environ.copy()
    env["GIT_SSH_COMMAND"] = shlex.join(command)
    return command, env


def prepare_aur(candidate, workspace, ssh, env, packages=PACKAGES):
    # AUR authentication/ownership failures must precede any public GitHub write.
    owned = {name.lstrip("*") for name in subprocess.check_output([*ssh, "aur@aur.archlinux.org", "list-repos"], text=True).split()}
    query = urllib.parse.urlencode([("arg[]", package) for package in packages])
    with urllib.request.urlopen("https://aur.archlinux.org/rpc/v5/info?" + query, timeout=30) as response:
        existing = json.load(response)["results"]
    require(all(item["PackageBase"] in owned for item in existing), "An AUR name belongs to another account; do not overwrite it.")
    plans = []
    for package in packages:
        directory = workspace / package
        subprocess.run(["git", "-c", "init.defaultBranch=master", "clone", "--quiet",
                        f"ssh://aur@aur.archlinux.org/{package}.git", str(directory)], env=env, check=True)
        tracked = git(directory, "ls-files").splitlines()
        require(set(tracked) <= {"PKGBUILD", ".SRCINFO"}, f"Unexpected existing AUR files: {package}; inspect manually.")
        source = candidate / "aur" / package
        recipes = []
        for name in ("PKGBUILD", ".SRCINFO"):
            old = (directory / name).read_text() if (directory / name).exists() else ""
            new = (source / name).read_text()
            recipes.append((old, new))
        vcs = package == "zerus-git"
        if not any(recipe_changed(old, new, vcs=vcs) for old, new in recipes):
            print(f"{package}: recipe already synchronized.")
            continue
        if all(not recipe_changed(old, new, vcs=vcs) or
               retains_description_update(old, new, vcs=vcs) for old, new in recipes):
            print(f"{package}: retaining the refreshed AUR description.")
            continue
        if (directory / ".SRCINFO").exists():
            old = metadata((directory / ".SRCINFO").read_text())
            new = metadata((source / ".SRCINFO").read_text())
            require(old.get("pkgbase") == [package] and old.get("pkgname") == [package], f"Unexpected existing package: {package}")
            require(package_version(new) >= package_version(old), f"Refusing an AUR downgrade: {package}")
            if package != "zerus-git":
                require(package_version(new) > package_version(old), f"Changed stable recipe needs a pkgrel/version bump: {package}")
        for name in ("PKGBUILD", ".SRCINFO"):
            (directory / name).write_bytes((source / name).read_bytes())
        git(directory, "add", "PKGBUILD", ".SRCINFO")
        git(directory, "diff", "--cached", "--check")
        git(directory, "-c", "user.name=Vladimir Alyamkin", "-c", "user.email=ufna@ufna.dev",
            "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "Update reviewed Zerus release recipe")
        # Opening receive-pack also checks AUR email verification/write access.
        # --dry-run changes no refs and runs before any GitHub release write.
        git(directory, "push", "--dry-run", "origin", "HEAD:master", env=env)
        plans.append((package, directory))
    return plans


def prepare_tap(candidate, workspace, env, version, channel="stable", remote=TAP_REMOTE):
    """Stage the channel's reviewed cask in the tap; push only after GitHub and AUR publication."""
    cask = cask_for(channel)
    directory = workspace / "homebrew-zerus"
    subprocess.run(["git", "clone", "--quiet", remote, str(directory)], env=env, check=True)
    casks = {name for name in git(directory, "ls-files").splitlines() if name.startswith(("Casks/", "Formula/"))}
    require(casks <= {"Casks/.gitkeep", *(f"Casks/{name}.rb" for name in CASKS.values())},
            "Unexpected tap contents; inspect the tap manually.")
    new = (candidate / "homebrew" / f"{cask}.rb").read_text()
    path = directory / "Casks" / f"{cask}.rb"
    if path.exists():
        old = path.read_text()
        if old == new:
            print(f"Homebrew {cask} cask already synchronized.")
            return None
        # A published cask version is immutable; changed bytes need a new version.
        require(cask_version_key(version, channel) > cask_version_key(cask_fields(old)["version"], channel),
                "Refusing a Homebrew cask downgrade or a changed cask with the same version.")
    path.parent.mkdir(exist_ok=True)
    path.write_text(new)
    git(directory, "add", f"Casks/{cask}.rb")
    git(directory, "diff", "--cached", "--check")
    git(directory, "-c", "user.name=Vladimir Alyamkin", "-c", "user.email=ufna@ufna.dev",
        "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", f"Update {cask} cask to {version}")
    # Checks write access with the deploy key before any GitHub release write.
    git(directory, "push", "--dry-run", "origin", "HEAD:main", env=env)
    return directory


def release_for_tag(tag):
    # Inspect the actual HTTP status instead of treating every API error as absence.
    result = subprocess.run(["gh", "api", f"repos/{REPOSITORY}/releases/tags/{tag}"], capture_output=True, text=True)
    if result.returncode == 0:
        return json.loads(result.stdout)
    require("HTTP 404" in result.stderr, "Cannot inspect GitHub release: " + result.stderr.strip())
    # The tag endpoint exposes published releases. An authenticated listing also
    # includes drafts, whose Git tag may not exist yet. Paginate so a retained
    # draft can still be resumed after later releases have been created.
    pages = json.loads(gh("api", "--paginate", "--slurp",
                          f"repos/{REPOSITORY}/releases?per_page=100"))
    matches = [release for page in pages for release in page if release["tag_name"] == tag]
    require(len(matches) <= 1, "Multiple releases use the candidate tag; inspect manually.")
    return matches[0] if matches else None


def wait_for_created_draft(tag):
    # GitHub may briefly omit a newly created draft from both lookup endpoints.
    # Retry reads only: never create another draft or replace candidate assets.
    for delay in (0, 1, 2, 4, 8, 15, 30):
        if delay:
            time.sleep(delay)
        release = release_for_tag(tag)
        if release is not None:
            return release
    raise ValueError("Created draft is still not visible; retry the same candidate after checking GitHub access.")


def verify_tag(tag, commit):
    result = subprocess.run(["gh", "api", f"repos/{REPOSITORY}/git/ref/tags/{tag}"], capture_output=True, text=True)
    if result.returncode:
        require("HTTP 404" in result.stderr, "Cannot inspect release tag: " + result.stderr.strip())
        return False
    target = json.loads(result.stdout)["object"]
    for _ in range(5):
        if target["type"] != "tag":
            break
        target = api(f"git/tags/{target['sha']}")["object"]
    require(target["type"] == "commit" and target["sha"] == commit, "Existing release tag points at a different commit.")
    return True


def verify_assets(release, candidate, hashes, allow_missing=False):
    assets = {asset["name"]: asset for asset in release["assets"]}
    require(len(assets) == len(release["assets"]) and set(assets) <= set(hashes), "Unexpected/duplicate existing release assets.")
    if not allow_missing:
        require(set(assets) == set(hashes), "Published release has missing assets; never repair it by replacing content.")
    for name, asset in assets.items():
        digest = asset.get("digest")
        if digest:
            require(digest == "sha256:" + hashes[name], f"Existing immutable asset differs: {name}")
        else:
            with tempfile.TemporaryDirectory(prefix="zerus-asset-") as temporary:
                gh("release", "download", release["tag_name"], "--repo", REPOSITORY, "--pattern", name, "--dir", temporary)
                require(sha256(Path(temporary) / name) == hashes[name], f"Existing immutable asset differs: {name}")
        require(asset["size"] == (candidate / name).stat().st_size, f"Existing asset size differs: {name}")
    return set(hashes) - set(assets)


def publish_github(candidate, info, sums, nightly=False):
    version, commit = info["version"], info["source_commit"]
    tag = info["release_tag"] if nightly else "v" + version
    hashes = {**sums, "SHA256SUMS": sha256(candidate / "SHA256SUMS")}
    verify_tag(tag, commit)
    release = release_for_tag(tag)
    if release is None:
        cask = "zerus@nightly" if nightly else "zerus"
        macos = (f"On Apple Silicon Macs, install with `brew install --cask ufna/zerus/{cask}` "
                 f"and run `zerus-setup` once. The Homebrew tap updates after GitHub and AUR publication.\n\n"
                 if info.get("macos") else "")
        if info.get("macos", {}).get("dmg"):
            image = info["macos"]["dmg"]["asset"]
            macos += (f"Or [download the signed macOS disk image]"
                      f"(https://github.com/{REPOSITORY}/releases/download/{tag}/{image}), "
                      f"open it and drag Zerus into Applications. macOS 14 or later and Apple Silicon required.\n\n")
        notes = (f"Arch Linux x86_64{' and Apple Silicon macOS' if macos else ''} ADE bundle.\n\n"
                 f"Install a stable binary with `yay -S zerus-ade-bin`, stable sources with `yay -S zerus`, "
                 f"or upstream main with `yay -S zerus-git`. Run `zerus-setup` once as your normal user. "
                 f"AUR updates follow GitHub publication; consult the publication workflow if an update is pending.\n\n"
                 f"{macos}"
                 f"Native agents are installed separately. Services and autostart are opt-in.\n\n"
                 f"Source commit: `{commit}`. Verify downloads using `SHA256SUMS`.\n")
        if nightly:
            notes = (f"Automatically verified Arch Linux x86_64 nightly from upstream main.\n\n"
                     f"Install with `yay -S zerus-ade-nightly-bin`. This replaces another Zerus package flavor. "
                     f"Run `zerus-setup` once as your normal user; services remain opt-in.\n\n"
                     f"{macos}"
                     f"Product version: `{info['product_version']}`. Source commit: `{commit}`. "
                     f"Build run: https://github.com/{REPOSITORY}/actions/runs/{info['workflow_run_id']}.\n\n"
                     f"Native agents are installed separately. Verify downloads using `SHA256SUMS`.\n")
        # Stable and nightly candidates carry the tested zip; older candidates have none.
        if info.get("windows_portable"):
            notes += (f"\nWindows x86_64: `{info['windows_portable']}` is a portable control console for "
                      f"remote Linux and macOS machines over OpenSSH; enabling local sessions in Machines "
                      f"also runs agents on the Windows computer itself. Extract it and run `hgs-tray.exe`. "
                      + ("It is unsigned and has no automatic updates.\n" if info.get("windows_installer") else
                       "It is unsigned and has no installer or automatic updates.\n"))
        if info.get("windows_installer"):
            notes += (f"Windows per-user setup: `{info['windows_installer']['asset']}` installs the same tested payload. "
                      "WebView2 Evergreen is required. UI login startup is opt-in; local agent hosting remains separate. "
                      "Verify its SHA256SUMS entry before running the unsigned installer.\n")
        title = f"Zerus {'nightly ' if nightly else ''}{version}"
        flags = ["--prerelease", "--latest=false"] if nightly else []
        with tempfile.TemporaryDirectory(prefix="zerus-notes-") as temporary:
            path = Path(temporary) / "notes.md"
            path.write_text(notes)
            try:
                gh("release", "create", tag, "--repo", REPOSITORY, "--draft", "--target", commit,
                   "--title", title, "--notes-file", str(path), *flags)
            except subprocess.CalledProcessError as error:
                # GitHub once refused the job token here (HTTP 403, zerus-ktg3) although
                # it held contents: write. Nothing public exists yet: the owner creates
                # the draft, and this same candidate then resumes it.
                print(notes, file=sys.stderr)
                raise ValueError(
                    f"GitHub refused to create the {tag} draft (exit {error.returncode}). Check that no "
                    f"{tag} release exists, then create the draft as the owner with the notes printed "
                    f"above saved as notes.md: gh release create {tag} --repo {REPOSITORY} --draft "
                    f"--target {commit} --title '{title}' --notes-file notes.md {' '.join(flags)}".rstrip()
                    + ". Rerun this publisher for the same candidate: it verifies the draft's target "
                    "and uploads only the verified candidate assets.") from error
        release = wait_for_created_draft(tag)
    if release["draft"]:
        require(release["target_commitish"] == commit, "Existing draft targets another commit or a moving branch.")
    else:
        require(verify_tag(tag, commit), "Published release tag is missing.")
    missing = verify_assets(release, candidate, hashes, allow_missing=release["draft"])
    for name in sorted(missing):
        gh("release", "upload", tag, str(candidate / name), "--repo", REPOSITORY)
    if release["draft"]:
        release = release_for_tag(tag)
        verify_assets(release, candidate, hashes)
        verify_tag(tag, commit)  # Existing draft/tag must still name the verified commit.
        gh("release", "edit", tag, "--repo", REPOSITORY, "--draft=false",
           *(["--prerelease", "--latest=false"] if nightly else ["--latest"]))
    require(verify_tag(tag, commit), "Published release tag is missing.")
    # AUR recipes must resolve without a GitHub login or token.
    for name, expected in hashes.items():
        url = f"https://github.com/{REPOSITORY}/releases/download/{tag}/{name}"
        with urllib.request.urlopen(url, timeout=60) as response:
            require(response.geturl().startswith("https://"), "Release download redirected outside HTTPS.")
            digest = hashlib.file_digest(response, "sha256").hexdigest()
        require(digest == expected, f"Anonymous release checksum mismatch: {name}")
    print(f"GitHub {tag}: all immutable assets verified anonymously.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--version")
    parser.add_argument("--nightly", action="store_true")
    parser.add_argument("--ssh-key", type=Path, required=True)
    parser.add_argument("--known-hosts", type=Path, default=Path("packaging/aur/known_hosts"))
    parser.add_argument("--tap-ssh-key", type=Path, help="deploy key for the Homebrew tap (macOS candidates)")
    parser.add_argument("--tap-known-hosts", type=Path, default=Path("packaging/homebrew/known_hosts"))
    args = parser.parse_args()
    if args.nightly:
        from nightly_common import PACKAGES as nightly_packages, validate_nightly
        info, sums = validate_nightly(args.candidate)
        packages = nightly_packages
    else:
        require(args.version is not None, "Stable publication requires --version.")
        info, sums = validate_candidate(args.candidate, args.version)
        packages = PACKAGES
    channel = "nightly" if args.nightly else "stable"
    macos = bool(info.get("macos"))
    require(not macos or args.tap_ssh_key is not None, "The macOS candidate needs the Homebrew tap deploy key.")
    ssh, env = ssh_environment(args.ssh_key, args.known_hosts)
    tap_env = ssh_environment(args.tap_ssh_key, args.tap_known_hosts, GITHUB_FINGERPRINT, "GitHub")[1] if macos else None
    with tempfile.TemporaryDirectory(prefix="zerus-aur-") as temporary:
        if args.nightly:
            plans = prepare_aur(args.candidate, Path(temporary), ssh, env, packages=packages)
        else:
            plans = prepare_aur(args.candidate, Path(temporary), ssh, env)
        tap = prepare_tap(args.candidate, Path(temporary), tap_env, info["version"], channel) if macos else None
        if args.nightly:
            publish_github(args.candidate, info, sums, nightly=True)
        else:
            publish_github(args.candidate, info, sums)
        for package, directory in plans:
            try:
                git(directory, "push", "origin", "HEAD:master", env=env)
            except subprocess.CalledProcessError:
                raise SystemExit(f"GitHub assets remain published; {package} AUR update failed. Rerun publication with this same candidate; do not rebuild or replace assets.")
            print(f"{package}: AUR update published.")
        if tap:
            try:
                git(tap, "push", "origin", "HEAD:main", env=tap_env)
            except subprocess.CalledProcessError:
                raise SystemExit("GitHub assets and AUR recipes remain published; the Homebrew tap update failed. Rerun publication with this same candidate; do not rebuild or replace assets.")
            print(f"Homebrew {cask_for(channel)} cask published to ufna/homebrew-zerus.")
    print(f"Zerus {info['version']}: GitHub, AUR{' and Homebrew' if macos else ''} synchronized.")


if __name__ == "__main__":
    main()
