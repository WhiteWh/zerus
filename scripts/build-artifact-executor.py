#!/usr/bin/env python3
"""Build the fork's model-free artifact executor package (Windows x86_64).

WhiteWh fork (zerus-y0cj): the release ships `hgs-executor.exe` beside hgs.exe,
so a released installation needs neither a source checkout nor Go. This script
builds it from pinned inputs only:

- Go 1.27.4 for windows-amd64, checked against its published SHA-256;
- desync at revision 717d63b1756554abc470671ea591ad6d25e6059d, fetched by that
  commit ID (the commit hash fixes the tree);
- the adapter sources in scripts/experiments/desync-artifact, built by its
  build.py, which also collects and checks the reviewed linked-license notices;
- services/artifact-executor/package.py, which verifies the binary, inventory
  and notices again and writes the standalone package.

On a builder the Go modules come from proxy.golang.org, verified by
sum.golang.org, during compilation only. A local build may instead name an
existing toolchain, desync source and module cache; those are only read (the
desync source is copied before build.py patches it).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
ADAPTER = ROOT / "scripts/experiments/desync-artifact"
REVISION = "717d63b1756554abc470671ea591ad6d25e6059d"
DESYNC_REPOSITORY = "https://github.com/folbricht/desync.git"
GO = {"version": "go1.27.4", "url": "https://go.dev/dl/go1.27.4.windows-amd64.zip",
      "sha256": "178ab98950ac69ab25069307dc6ade79f3816e414cce95b4da166a115cde1e7e", "size": 79077744}
PACKAGE = "hgs-executor-windows-x86_64.zip"


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def fetch_go(work):
    archive = work / GO["url"].rsplit("/", 1)[1]
    if not archive.is_file() or digest(archive) != GO["sha256"]:
        archive.unlink(missing_ok=True)
        partial = archive.with_suffix(".partial")
        with urllib.request.urlopen(GO["url"], timeout=120) as answer, partial.open("wb") as target:
            shutil.copyfileobj(answer, target, 1024 * 1024)
        if partial.stat().st_size != GO["size"] or digest(partial) != GO["sha256"]:
            partial.unlink()
            raise SystemExit("Go toolchain archive differs from its pinned SHA-256")
        partial.replace(archive)
    toolchain = work / "toolchain"
    if not (toolchain / "go/bin/go.exe").is_file():
        shutil.rmtree(toolchain, ignore_errors=True)
        with zipfile.ZipFile(archive) as bundle:
            bundle.extractall(toolchain)
    return toolchain / "go/bin/go.exe"


def fetch_desync(work):
    source = work / "source" / f"desync-{REVISION}"
    if not (source / "go.mod").is_file():
        shutil.rmtree(source, ignore_errors=True)
        source.mkdir(parents=True)
        git = ["git", "-c", "core.autocrlf=false", "-C", str(source)]
        subprocess.run(git[:3] + ["init", "-q", str(source)], check=True)
        subprocess.run(git + ["fetch", "-q", "--depth", "1", DESYNC_REPOSITORY, REVISION], check=True, timeout=300)
        subprocess.run(git + ["checkout", "-q", "--detach", REVISION], check=True)
    head = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    if head != REVISION:
        raise SystemExit(f"desync source is {head}, expected {REVISION}")
    return source


def environment(work, go, modules):
    """build.py's private build environment, written before it runs so that a
    local build may read an existing module cache."""
    parent = work.parent
    env = {k: os.environ[k] for k in ["SystemRoot", "WINDIR", "COMSPEC", "PATHEXT"] if k in os.environ}
    env.update({"PATH": str(go.parent) + os.pathsep + str(Path(os.environ.get("SystemRoot", "C:/Windows")) / "System32"),
                "GOCACHE": str(parent / "go-cache"),
                "GOMODCACHE": str(modules or parent / "go-modules")})
    for key, name in [("USERPROFILE", "build-home"), ("HOME", "build-home"), ("LOCALAPPDATA", "build-local"),
                      ("APPDATA", "build-roaming"), ("TEMP", "build-temp"), ("TMP", "build-temp")]:
        (parent / name).mkdir(parents=True, exist_ok=True)
        env[key] = str(parent / name)
    (parent / "build-environment.json").write_text(json.dumps(env, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--work", type=Path, default=ROOT / "target/artifact-executor",
                        help="private build folder [default: target/artifact-executor]")
    parser.add_argument("--output", type=Path, default=ROOT / "artifacts" / PACKAGE)
    parser.add_argument("--go", type=Path, default=os.environ.get("ZERUS_EXECUTOR_GO") or None,
                        help="an existing go.exe of the pinned version [default: download it]")
    parser.add_argument("--desync", type=Path, default=os.environ.get("ZERUS_EXECUTOR_DESYNC") or None,
                        help="an existing desync source at the pinned revision, copied [default: fetch it]")
    parser.add_argument("--modules", type=Path, default=os.environ.get("ZERUS_EXECUTOR_MODULES") or None,
                        help="an existing Go module cache, read offline [default: fetch modules while compiling]")
    parser.add_argument("--test", action="store_true", help="also run the adapter's Go tests")
    args = parser.parse_args()
    if sys.platform != "win32":
        raise SystemExit("The artifact executor builds on Windows only")
    work = args.work.resolve()
    shutil.rmtree(work / "build", ignore_errors=True)
    for stale in ("build-environment.json", "artifact-sync-prototype.exe", "licenses"):
        path = work / stale
        shutil.rmtree(path, ignore_errors=True) if path.is_dir() else path.unlink(missing_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    go = args.go.resolve() if args.go else fetch_go(work)
    version = subprocess.check_output([str(go), "env", "GOVERSION"], text=True,
                                      env={**os.environ, "GOTOOLCHAIN": "local", "GOTELEMETRY": "off"}).strip()
    if version != GO["version"]:
        raise SystemExit(f"Go toolchain is {version}, expected {GO['version']}")
    if args.desync:
        source = work / "source" / f"desync-{REVISION}"
        shutil.rmtree(source, ignore_errors=True)
        shutil.copytree(args.desync.resolve(), source, ignore=shutil.ignore_patterns(".git"))
    else:
        source = fetch_desync(work)
    environment(work / "build", go, args.modules.resolve() if args.modules else None)
    command = [sys.executable, str(ADAPTER / "build.py"), "--source", str(source), "--go", str(go),
               "--work", str(work / "build")]
    if args.test:
        command.append("--test")
    if not args.modules:
        command.append("--fetch-public-dependencies")
    result = subprocess.run(command, cwd=ADAPTER)
    if result.returncode:
        log = work / "prototype-build.log"
        if log.is_file():
            sys.stdout.write(log.read_text(encoding="utf-8", errors="replace")[-8000:])
        raise SystemExit("artifact executor build failed")
    output = args.output.resolve()
    output.unlink(missing_ok=True)
    subprocess.run([sys.executable, str(ROOT / "services/artifact-executor/package.py"),
                    "--binary", str(work / "artifact-sync-prototype.exe"),
                    "--notices", str(work / "licenses/desync-artifact"),
                    "--output", str(output)], check=True)
    attestation = json.loads((work / "licenses/desync-artifact/build.json").read_text(encoding="utf-8"))
    print(f"Wrote {output} (sha256 {digest(output)}): executor source {attestation['source_sha256']}, "
          f"binary {attestation['binary_sha256']}, desync {attestation['desync_revision']}, {version}.")


if __name__ == "__main__":
    main()
