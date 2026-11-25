"""Isolated memory scaling qualification. All measurements stay in local private reports."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import traceback

from qualify import MIB, child, kernel, private_error, read, save


def source_identity(source):
    info = source.stat()
    return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns


def resource_refusal(log):
    text = log.read_text(encoding="utf-8", errors="replace").lower()
    return any(word in text for word in ("out of memory", "cannot allocate memory",
               "paging file is too small", "page file is too small", "virtualalloc"))


def main(args):
    private_base = (Path(os.environ["LOCALAPPDATA"]) / "Zerus/private-benchmarks").resolve()
    out = args.out.resolve()
    repository = Path(__file__).resolve().parents[3]
    if (not out.is_relative_to(private_base) or out == private_base or
            out.is_relative_to(repository) or kernel.GetDriveTypeW(str(out.anchor)) != 3):
        raise RuntimeError("output must be a new local private qualification directory")
    source_item = read(args.selection)["files"][0]
    source = Path(source_item["path"]).resolve()
    if kernel.GetDriveTypeW(str(source.anchor)) != 3 or out.is_relative_to(source.parent):
        raise RuntimeError("source must be on a separate fixed local source tree")
    identity = source_identity(source)
    out.mkdir(parents=True, exist_ok=False)
    report = {"scope": "real read-only ranges plus synthetic metadata; not full-build qualification",
              "backend": "desync", "backend_revision": "717d63b1756554abc470671ea591ad6d25e6059d",
              "helper_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "completed": False, "source_local": str(source), "cases": [], "checks": {}}
    save(out / "results.json", report)
    # Stress whole-container metadata without allocating a logical/physical
    # payload of that size. Fixed synthetic chunks isolate planner memory.
    metadata = [("metadata_baseline", 32, 1024, "rotate")]
    metadata += [(f"metadata_rotate_{average}k", 400, average, "rotate")
                 for average in (4096, 1024, 256, 64)]
    metadata += [("metadata_reverse_4m", 400, 4096, "reverse"),
                 ("metadata_reverse_1m", 400, 1024, "reverse"),
                 ("metadata_full_change", 400, 1024, "full_changed"),
                 ("metadata_same", 400, 1024, "same")]
    for name, gib, kib, scenario in metadata:
        work = out / name
        work.mkdir()
        config = work / "config.json"
        cfg = {"probe_chunks": math.ceil(gib * 1024**3 / (kib * 1024)),
               "average": kib * 1024, "scenario": scenario,
               "stash_bytes": 2 * MIB, "memory_bytes": args.hard_memory_mib * MIB * 3 // 4}
        save(config, cfg)
        measurement = child(args.binary, "metadata", config, "metadata",
                            args.hard_memory_mib * MIB, min(args.deadline, 90))
        status = "passed"
        if measurement["exit_code"]:
            if measurement["deadline_exceeded"]:
                status = "time_budget_refused"
            elif resource_refusal(work / "metadata.log"):
                status = "resource_refused"
            else:
                raise RuntimeError("metadata probe failed for an unexpected reason")
        else:
            if not read(work / "metadata-result.json").get("ok"):
                raise RuntimeError("metadata planner did not finish")
        report["cases"].append({"name": name, "kind": "synthetic_metadata", "status": status,
                                "measurement": measurement,
                                "progress": read(work / "metadata-progress.json")
                                if (work / "metadata-progress.json").exists() else {}})
        save(out / "results.json", report)
        print(name + ": " + status, flush=True)

    real = [("real_baseline", 1024, 256), ("real_growth", 4096, 256)]
    real += [(f"real_large_{average}k", args.real_mib, average) for average in (64, 256, 1024, 4096)]
    for name, size_mib, kib in real:
        length = size_mib * MIB
        if (length > identity[2] - source_item["start"] or
                shutil.disk_usage(out).free < length + 256 * MIB):
            report["cases"].append({"name": name, "kind": "real_range", "status": "skipped_bounds"})
            save(out / "results.json", report)
            print(name + ": skipped_bounds", flush=True)
            continue
        work = out / name
        work.mkdir()
        config = work / "config.json"
        save(config, {"source": str(source), "source_offset": source_item["start"],
                      "length": length, "scenario": "insert", "average": kib * 1024,
                      "stash_bytes": 2 * MIB, "memory_bytes": args.hard_memory_mib * MIB * 3 // 4})
        record = {"name": name, "kind": "real_range"}
        record["prepare"] = child(args.binary, "prepare", config, "prepare",
                                  args.hard_memory_mib * MIB, args.deadline)
        if record["prepare"]["exit_code"]:
            raise RuntimeError("large-range preparation failed")
        record["apply"] = child(args.binary, "apply", config, "apply",
                                args.hard_memory_mib * MIB, args.deadline)
        if record["apply"]["exit_code"]:
            raise RuntimeError("large-range application failed")
        result, state = read(work / "result.json"), read(work / "state.json")
        if not (result.get("ok") and result.get("exact_hash_match") and state.get("state") == "ready"):
            raise RuntimeError("large-range verification failed")
        record.update(status="passed", result=result, manifest=read(work / "manifest.json"))
        # Only remove this run's exact owned payload, never the publisher.
        payload = work / "receiver.bin"
        if payload.resolve().parent != work.resolve() or payload.samefile(source):
            raise RuntimeError("unsafe scratch cleanup refused")
        payload.unlink()
        report["cases"].append(record)
        save(out / "results.json", report)
        print(name + ": passed", flush=True)
    report["checks"]["source_identity_size_mtime_unchanged"] = source_identity(source) == identity
    report["checks"]["no_receiver_payloads_retained"] = not any(out.glob("*/receiver.bin"))
    if not all(report["checks"].values()):
        raise RuntimeError("source or storage postcondition failed")
    report["completed"] = True
    save(out / "results.json", report)
    lines = ["# Private local desync scaling report", "",
             report["scope"], "",
             "Metadata cases plan synthetic fixed-size chunk IDs without payload I/O.",
             "Real cases reconstruct read-only container ranges with synthetic insertion.",
             "Both endpoints share a process. Values do not establish a real 400 GiB update.", "",
             "| Case | Status | Peak RSS MiB | Peak commit MiB | Seconds |", "|---|---|---:|---:|---:|"]
    for case in report["cases"]:
        measures = [case["measurement"]] if "measurement" in case else [
            case[key] for key in ("prepare", "apply") if key in case]
        rss = max((m["peak_rss_bytes"] for m in measures), default=0)/MIB
        commit = max((m["peak_commit_bytes"] for m in measures), default=0)/MIB
        seconds = sum(m["seconds"] for m in measures)
        lines.append(f"| {case['name']} | {case['status']} | {rss:.2f} | {commit:.2f} | {seconds:.3f} |")
    lines += ["", "Resource refusals and deadline refusals are failed admission configurations.",
              "They must not be presented as successful reconstruction.",
              "Use complete JSON and phase snapshots for analysis; all values remain private.", ""]
    (out / "REPORT.md").write_text("\n".join(lines), encoding="utf-8")
    save(out.parent / "DESYNC-SCALE-RESULTS.json", report)
    (out.parent / "DESYNC-SCALE-REPORT.md").write_text("\n".join(lines), encoding="utf-8")
    save(out.parent / "latest-desync-scale-run.json", {"directory": str(out)})
    print("Local desync scaling qualification completed; measurements remain private.", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--selection", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--real-mib", type=int, default=8192)
    parser.add_argument("--hard-memory-mib", type=int, default=512)
    parser.add_argument("--deadline", type=int, default=600)
    args = parser.parse_args()
    if not (1 <= args.real_mib <= 32768 and 128 <= args.hard_memory_mib <= 2048 and
            30 <= args.deadline <= 1200):
        raise SystemExit("invalid qualification bounds")
    try:
        main(args)
    except BaseException:
        private_error(args.out, traceback.format_exc())
        print("Scaling qualification stopped; inspect the private local report.", flush=True)
        raise SystemExit(1)
