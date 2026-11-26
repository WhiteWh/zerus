"""Build the pinned experimental helper. Offline by default; no content arguments."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
from collect_notices import collect

REVISION = "717d63b1756554abc470671ea591ad6d25e6059d"


def patch_null_seed_lifetime(source):
    path = source / "nullseed.go"
    text = path.read_text(encoding="utf-8")
    if "var zerusCreateNullSeedTemp = os.CreateTemp" in text:
        return
    marker = 'os.CreateTemp(filepath.Dir(dstFile), ".tmp-block")'
    close = "return os.Remove(s.blockfile.Name())"
    if text.count(marker) != 1 or text.count(close) != 1:
        raise RuntimeError("unexpected upstream null-seed lifetime")
    text = text.replace("type nullChunkSeed struct {",
                        "var zerusCreateNullSeedTemp = os.CreateTemp\n\ntype nullChunkSeed struct {", 1)
    text = text.replace(marker, 'zerusCreateNullSeedTemp(filepath.Dir(dstFile), ".tmp-block")', 1)
    text = text.replace(close, "err := os.Remove(s.blockfile.Name())\n"
                       "\t\tif os.IsNotExist(err) { return nil }; return err", 1)
    path.write_text(text, encoding="utf-8")


def patch_stash_limit(source):
    path = source / "assemble.go"
    text = path.read_text(encoding="utf-8")
    if "StashLimitBytes *int64" in text:
        return
    text, count = re.subn(
        r"(\bInvalidSeedAction\s+InvalidSeedAction\s*\n)",
        r"\1\tStashLimitBytes *int64\n", text, count=1)
    if count != 1 or text.count("planWithStashLimit(stashMemoryLimit()),") != 1:
        raise RuntimeError("unexpected upstream assembly API")
    marker = "\t// Create the plan and validate the seed indexes."
    if text.count(marker) != 1:
        raise RuntimeError("unexpected upstream planner")
    addition = (
        "\t// Zerus experiment: cap displacement data independently of the Go heap.\n"
        "\tstashLimit := stashMemoryLimit()\n"
        "\tif options.StashLimitBytes != nil {\n"
        "\t\tif *options.StashLimitBytes < 0 { return nil, errors.New(\"invalid stash limit\") }\n"
        "\t\tstashLimit = min(stashLimit, *options.StashLimitBytes)\n"
        "\t}\n"
    )
    text = text.replace(marker, addition + marker, 1)
    text = text.replace("planWithStashLimit(stashMemoryLimit()),",
                        "planWithStashLimit(stashLimit),", 1)
    path.write_text(text, encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--go", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--test", action="store_true")
    parser.add_argument("--fetch-public-dependencies", action="store_true",
                        help="allow module downloads during compilation, before any data tests")
    args = parser.parse_args()
    source, work = args.source.resolve(), args.work.resolve()
    if not (source / "go.mod").is_file():
        raise RuntimeError("public dependency source is missing")
    work.mkdir(parents=True, exist_ok=True)
    env_file = work.parent / "build-environment.json"
    if not env_file.is_file():
        env = {k: os.environ[k] for k in
               ["SystemRoot", "WINDIR", "COMSPEC", "PATHEXT"] if k in os.environ}
        env.update({
            "PATH": str(args.go.parent) + os.pathsep +
                    str(Path(os.environ.get("SystemRoot", "C:/Windows")) / "System32"),
            "GOCACHE": str(work.parent / "go-cache"),
            "GOMODCACHE": str(work.parent / "go-modules"),
        })
        for key, name in [("USERPROFILE", "build-home"), ("HOME", "build-home"),
                          ("LOCALAPPDATA", "build-local"), ("APPDATA", "build-roaming"),
                          ("TEMP", "build-temp"), ("TMP", "build-temp")]:
            folder = work.parent / name
            folder.mkdir(exist_ok=True)
            env[key] = str(folder)
    else:
        env = json.loads(env_file.read_text(encoding="utf-8"))
    # All dependency fetching is finished before a test can read private data.
    env.update(GOPROXY="off", GOSUMDB="off", GOTOOLCHAIN="local",
               GOTELEMETRY="off", GOENV="off", GOWORK="off", CGO_ENABLED="0",
               GOOS="windows", GOARCH="amd64", GOEXPERIMENT="", GOFLAGS="")
    patch_stash_limit(source)
    patch_null_seed_lifetime(source)
    here = Path(__file__).resolve().parent
    source_hash=hashlib.sha256()
    for path in sorted([*here.glob("*.go"),*here.glob("*.go.txt"),here/"build.py",here/"collect_notices.py",here/"reviewed-licenses.json"]):
        source_hash.update(path.name.encode()+b"\0"+path.read_bytes()+b"\0")
    source_identity=source_hash.hexdigest()
    shutil.copyfile(here / "planner_probe.go.txt", source / "zerus_memory_probe.go")
    shutil.copyfile(here / "nullseed_windows.go.txt", source / "zerus_nullseed_windows.go")
    files = [p for p in here.glob("*.go") if args.test or not p.name.endswith("_test.go")]
    for path in files:
        shutil.copyfile(path, work / path.name)
    if args.test:
        # zerus-un7n: the host access contract's shared fixtures (also read by
        # src/host_access) and the mobile protocol's rejected-key vectors.
        repo = here.parents[2]
        shared = {"host-access-v1": sorted((repo / "contracts/host-access-v1/fixtures").glob("*.json")),
                  "mobile-vectors": [repo / "services/mobile/crypto/tests/vectors/rejected-keys-v1.json"]}
        for folder, fixtures in shared.items():
            (work / "testdata" / folder).mkdir(parents=True, exist_ok=True)
            for fixture in fixtures:
                shutil.copyfile(fixture, work / "testdata" / folder / fixture.name)
    # The binary records this path in its build information: a relative one
    # keeps local and builder binaries of the same source identical and keeps
    # the build machine's folders out of a published executable.
    try:
        replacement = Path(os.path.relpath(source, work)).as_posix()
        if not replacement.startswith("."):
            replacement = "./" + replacement
    except ValueError:  # another drive
        replacement = source.as_posix()
    (work / "go.mod").write_text(
        "module zerus.local/artifact-sync-prototype\n\ngo 1.26.0\n\n"
        "require github.com/folbricht/desync v0.0.0\n\n"
        # zerus-un7n: host access person-key point decoding (BSD-3-Clause).
        "require filippo.io/edwards25519 v1.2.0\n\n"
        "replace github.com/folbricht/desync => " + json.dumps(replacement) + "\n",
        encoding="utf-8")
    log_path = work.parent / "prototype-build.log"
    with log_path.open("w", encoding="utf-8") as log:
        commands = [
            [str(args.go.parent / "gofmt.exe"), "-w"] + [p.name for p in files],
            [str(args.go), "build", "-mod=mod", "-trimpath", "-buildvcs=false",
             "-ldflags", "-X main.executorSourceIdentity="+source_identity,
             "-o", str(work.parent / "artifact-sync-prototype.exe"), "."],
        ]
        if args.test:
            commands.append([str(args.go), "test", "-mod=mod", "-count=1", "./..."])
        for command in commands:
            command_env = dict(env)
            if args.fetch_public_dependencies and "build" in command:
                command_env.update(GOPROXY="https://proxy.golang.org",
                                   GOSUMDB="sum.golang.org")
            subprocess.run(command, cwd=work, env=command_env, stdout=log,
                           stderr=subprocess.STDOUT, check=True, timeout=300,
                           creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    collect(args.go, work, work.parent / "artifact-sync-prototype.exe", env,
            work.parent / "licenses/desync-artifact")
    binary=work.parent/"artifact-sync-prototype.exe"
    inventory=work.parent/"licenses/desync-artifact/inventory.json"
    with binary.open("rb") as stream:binary_digest=hashlib.file_digest(stream,"sha256").hexdigest()
    attestation={"protocol":1,"desync_revision":REVISION,"source_sha256":source_identity,
                 "binary_sha256":binary_digest,
                 "inventory_sha256":hashlib.sha256(inventory.read_bytes()).hexdigest()}
    (work.parent/"licenses/desync-artifact/build.json").write_text(json.dumps(attestation,indent=2)+"\n",encoding="utf-8")
    print("Helper build, requested checks and offline license inventory passed.")


if __name__ == "__main__":
    main()
