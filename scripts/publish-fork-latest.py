#!/usr/bin/env python3
"""Refresh only the mutable fork prerelease `latest` with a tested Windows ZIP."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from windows_portable import inspect

TAG = "latest"
ASSET = "zerus-windows-x86_64.zip"
BACKUP = ASSET + ".previous"
REPOSITORIES = {"WhiteWh/zerus", "Phoenix-/zerus"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    with path.open("rb") as stream:
        return "sha256:" + hashlib.file_digest(stream, "sha256").hexdigest()


class GitHub:
    def __init__(self, repository):
        require(repository in REPOSITORIES, "Only the explicitly supported forks may publish Latest.")
        self.repository, self.release_id = repository, None

    def api(self, method, route, data=None, missing=False):
        args = ["gh", "api", f"repos/{self.repository}/{route}", "--method", method]
        if data is not None:
            args += ["--input", "-"]
        result = subprocess.run(args, input=None if data is None else json.dumps(data),
                                text=True, capture_output=True, check=False)
        if result.returncode:
            if missing and "HTTP 404" in result.stderr:
                return None
            raise RuntimeError(result.stderr.strip() or "GitHub request failed.")
        return json.loads(result.stdout) if result.stdout.strip() else None

    def release(self):
        if self.release_id is not None:
            return self.api("GET", f"releases/{self.release_id}")
        release = self.api("GET", f"releases/tags/{TAG}", missing=True)
        if release is None:
            # The tag endpoint may omit drafts. Read one bounded page; a duplicate
            # create is refused by GitHub, never repaired by removing a release.
            matches = [r for r in self.api("GET", "releases?per_page=100") if r["tag_name"] == TAG]
            require(len(matches) <= 1, "Ambiguous Latest drafts; inspect before publishing.")
            release = matches[0] if matches else None
        if release is not None:
            self.release_id = release["id"]
        return release

    def create(self, commit, notes):
        release = self.api("POST", "releases", {
            "tag_name": TAG, "target_commitish": commit, "name": "Latest",
            "body": notes, "draft": True, "prerelease": True, "make_latest": "false",
        })
        self.release_id = release["id"]
        return release

    def upload(self, path, name):
        with tempfile.TemporaryDirectory(prefix="zerus-latest-upload-") as temporary:
            target = Path(temporary) / name
            shutil.copyfile(path, target)
            subprocess.run(["gh", "release", "upload", TAG, str(target), "--repo", self.repository], check=True)

    def rename(self, asset_id, name):
        return self.api("PATCH", f"releases/assets/{asset_id}", {"name": name})

    def delete(self, asset_id):
        self.api("DELETE", f"releases/assets/{asset_id}")

    def move_tag(self, commit):
        ref = self.api("GET", f"git/ref/tags/{TAG}", missing=True)
        if ref is None:
            ref = self.api("POST", "git/refs", {"ref": f"refs/tags/{TAG}", "sha": commit})
        else:
            require(ref["object"]["type"] == "commit", "Latest must use a lightweight commit tag.")
            if ref["object"]["sha"] != commit:
                ref = self.api("PATCH", f"git/refs/tags/{TAG}", {"sha": commit, "force": True})
        require(ref["object"]["type"] == "commit" and ref["object"]["sha"] == commit,
                "Latest tag does not identify the checked ZIP's source.")

    def finish(self, commit, notes):
        return self.api("PATCH", f"releases/{self.release_id}", {
            "target_commitish": commit, "name": "Latest", "body": notes,
            "draft": False, "prerelease": True, "make_latest": "false",
        })


def assets(release):
    require(release["tag_name"] == TAG and not release.get("immutable", False),
            "Only the mutable Latest release may be refreshed.")
    require(release["draft"] or release["prerelease"], "Refusing to replace a stable release.")
    rows = release["assets"]
    result = {row["name"]: row for row in rows}
    require(len(result) == len(rows), "Duplicate Latest assets.")
    require(all(name in (ASSET, BACKUP) or re.fullmatch(re.escape(ASSET) + r"\.[a-f0-9]{16}\.upload", name)
                for name in result), "Latest contains unrecognized assets; inspect before replacing them.")
    return result


def verify(asset, path, expected):
    require(asset["state"] == "uploaded" and asset["size"] == path.stat().st_size
            and (asset.get("digest") or "").lower() == expected,
            "Uploaded Latest asset size or SHA-256 differs from the checked ZIP.")


def publish(client, archive, commit, run_id):
    require(re.fullmatch(r"[a-f0-9]{40}", commit) and re.fullmatch(r"[1-9][0-9]*", run_id),
            "Use the exact source SHA and successful workflow run ID.")
    require(inspect(archive)["source_commit"] == commit, "ZIP belongs to another source commit.")
    expected = digest(archive)
    staged = ASSET + "." + expected.removeprefix("sha256:")[:16] + ".upload"
    notes = (f"Rolling Windows build of `whitewh/main` at `{commit}`.\n\n"
             f"[Build and test run](https://github.com/{client.repository}/actions/runs/{run_id}). "
             "Download the ZIP and install it manually. Automatic updates are disabled.\n\n"
             f"SHA-256: `{expected.removeprefix('sha256:')}`\n")
    release = client.release()
    if release is None:
        release = client.create(commit, notes)
    current = assets(release)
    existing = current.get(ASSET)
    if existing is None or (existing.get("digest") or "").lower() != expected:
        if staged not in current:
            client.upload(archive, staged)
            current = assets(client.release())
        verify(current[staged], archive, expected)
        try:
            if ASSET in current:
                if BACKUP in current:
                    client.delete(current[BACKUP]["id"])
                client.rename(current[ASSET]["id"], BACKUP)
            client.rename(current[staged]["id"], ASSET)
        except Exception:
            # Reconcile an uncertain rename before restoring anything. Never
            # replace an already-promoted canonical asset with the old backup.
            observed = assets(client.release())
            if ASSET not in observed and BACKUP in observed:
                client.rename(observed[BACKUP]["id"], ASSET)
            raise
    verify(assets(client.release())[ASSET], archive, expected)
    client.move_tag(commit)
    result = client.finish(commit, notes)
    require(result["prerelease"] and not result["draft"] and result["target_commitish"] == commit,
            "Latest was not published as a prerelease of the checked source.")
    verified = assets(result)
    verify(verified[ASSET], archive, expected)
    for name, row in verified.items():
        if name != ASSET:
            client.delete(row["id"])
    return result["html_url"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--zip", type=Path, required=True)
    args = parser.parse_args()
    print(publish(GitHub(args.repository), args.zip, args.commit, args.run_id))


if __name__ == "__main__":
    main()
