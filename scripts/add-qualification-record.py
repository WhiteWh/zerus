#!/usr/bin/env python3
"""Place an all-pass AppContainer qualification record beside hgs-executor.exe.

Trusted packaging step of the fork release (services/artifact-executor/README.md,
"Activation"): the record produced by `hgs-executor appcontainer-qualify` on the
disposable runner for this exact release goes next to the executor in the
portable zip and in the standalone executor zip. The executor still decides
activation itself; this script only refuses a record that cannot activate.
"""
import argparse
import hashlib
import json
import sys
import zipfile
from pathlib import PurePosixPath

SIDECAR = "appcontainer-qualification.json"
CASES = [f"Q{n}" for n in range(11)]


def fail(message):
    print(f"add-qualification-record: {message}", file=sys.stderr)
    sys.exit(1)


def executor_entry(archive):
    names = [n for n in archive.namelist() if PurePosixPath(n).name == "hgs-executor.exe" and n.count("/") == 1]
    if len(names) != 1:
        fail(f"{archive.filename}: expected one top-level-folder hgs-executor.exe, found {len(names)}")
    return names[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--record", required=True)
    parser.add_argument("zips", nargs="+")
    args = parser.parse_args()
    raw = open(args.record, "rb").read()
    record = json.loads(raw.decode("utf-8-sig"))
    cases = record.get("cases") or []
    if record.get("evidence") != "distributable" or record.get("service_admin") is not False:
        fail("record is not distributable evidence from a non-administrator account")
    if [c.get("case") for c in cases] != CASES or any(c.get("verdict") != "pass" for c in cases):
        fail("record is not an all-pass Q0-Q10 record")
    for path in args.zips:
        with zipfile.ZipFile(path) as archive:
            entry = executor_entry(archive)
            if hashlib.sha256(archive.read(entry)).hexdigest() != record.get("binary_sha256"):
                fail(f"{path}: the record names another executor binary")
            target = str(PurePosixPath(entry).parent / SIDECAR)
            if target in archive.namelist():
                fail(f"{path}: already carries a qualification record")
            info = archive.getinfo(entry)
        with zipfile.ZipFile(path, "a", compression=zipfile.ZIP_DEFLATED) as archive:
            added = zipfile.ZipInfo(target, date_time=info.date_time)
            added.compress_type = zipfile.ZIP_DEFLATED
            added.external_attr = 0o644 << 16
            archive.writestr(added, raw)
        print(f"{path}: added {target} (binary {record['binary_sha256'][:12]}, source {record.get('source_identity', '')[:12]})")


if __name__ == "__main__":
    main()
