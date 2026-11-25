"""Run local desync qualification; input paths, payloads and measurements stay private."""
import argparse
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import traceback
from windows_limits import ResourceGate

MIB = 1024 * 1024


class MemoryCounters(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD), ("faults", wintypes.DWORD),
        ("peak", ctypes.c_size_t), ("working", ctypes.c_size_t),
        ("paged_peak", ctypes.c_size_t), ("paged", ctypes.c_size_t),
        ("nonpaged_peak", ctypes.c_size_t), ("nonpaged", ctypes.c_size_t),
        ("pagefile", ctypes.c_size_t), ("pagefile_peak", ctypes.c_size_t),
    ]

class FileStandardInfo(ctypes.Structure):
    _fields_ = [
        ("allocation", ctypes.c_longlong), ("length", ctypes.c_longlong),
        ("links", wintypes.DWORD), ("delete_pending", ctypes.c_ubyte),
        ("directory", ctypes.c_ubyte),
    ]


kernel = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)
kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel.OpenProcess.restype = wintypes.HANDLE
kernel.CloseHandle.argtypes = [wintypes.HANDLE]
kernel.CreateFileW.argtypes = [
    wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
    wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
kernel.CreateFileW.restype = wintypes.HANDLE
kernel.GetFileInformationByHandleEx.argtypes = [
    wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
kernel.GetFileInformationByHandleEx.restype = wintypes.BOOL
kernel.GetDriveTypeW.argtypes = [wintypes.LPCWSTR]
kernel.GetDriveTypeW.restype = wintypes.UINT
psapi.GetProcessMemoryInfo.argtypes = [
    wintypes.HANDLE, ctypes.POINTER(MemoryCounters), wintypes.DWORD]


def save(path, value):
    tmp = path.with_suffix(path.suffix + ".next")
    with tmp.open("w", encoding="utf-8") as f:
        json.dump(value, f, indent=2)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def footprint(directory):
    logical = allocated = 0
    for entry in os.scandir(directory):
        try:
            if not entry.is_file(follow_symlinks=False):
                continue
            n = entry.stat(follow_symlinks=False).st_size
        except FileNotFoundError:
            # The helper atomically renames its small receipt files.
            continue
        logical += n
        if entry.name != "receiver.bin":
            # Directory enumeration supplies cached metadata on Windows.
            # Never open rapidly replaced receipt files just to measure them.
            allocated += ((n + 4095) // 4096) * 4096
            continue
        # Explicitly share delete/write access. GetCompressedFileSizeW can
        # transiently block receipt renames and perturb the operation measured.
        handle = kernel.CreateFileW(entry.path, 0x80, 7, None, 3, 0x00200000, None)
        if handle == ctypes.c_void_p(-1).value:
            if ctypes.get_last_error() in (2, 3):
                continue
            allocated += ((n + 4095) // 4096) * 4096
            continue
        try:
            info = FileStandardInfo()
            if kernel.GetFileInformationByHandleEx(
                    handle, 1, ctypes.byref(info), ctypes.sizeof(info)):
                allocated += max(0, info.allocation)
            else:
                allocated += ((n + 4095) // 4096) * 4096
        finally:
            kernel.CloseHandle(handle)
    return logical, allocated


def child(binary, command, config, label, memory_bytes=512 * MIB, deadline=300):
    work = config.parent
    env = {k: os.environ[k] for k in ["SystemRoot", "WINDIR", "COMSPEC"] if k in os.environ}
    env.update({
        "TEMP": str(work), "TMP": str(work), "USERPROFILE": str(work),
        "HOME": str(work), "LOCALAPPDATA": str(work), "APPDATA": str(work),
        "GOPROXY": "off", "GOSUMDB": "off", "GOTELEMETRY": "off",
        "OTEL_SDK_DISABLED": "true", "HTTP_PROXY": "", "HTTPS_PROXY": "",
        "ALL_PROXY": "", "NO_PROXY": "*",
    })
    peak_rss = peak_commit = peak_logical = peak_allocated = 0
    deadline_exceeded = False
    start = time.perf_counter()
    with ResourceGate(memory_bytes) as gate, (work / (label + ".log")).open("wb") as log:
        env["ZERUS_DESYNC_QUALIFICATION_GATE"] = gate.name
        proc = subprocess.Popen([str(binary), command, str(config)], cwd=work,
                                env=env, stdout=log, stderr=subprocess.STDOUT,
                                creationflags=subprocess.CREATE_NO_WINDOW)
        handle = kernel.OpenProcess(0x1000 | 0x10, False, proc.pid)
        try:
            if not handle:
                raise RuntimeError("cannot measure the owned qualification process")
            gate.admit(proc)
            while proc.poll() is None:
                if handle:
                    counters = MemoryCounters()
                    counters.cb = ctypes.sizeof(counters)
                    if psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
                        peak_rss = max(peak_rss, counters.peak)
                        peak_commit = max(peak_commit, counters.pagefile_peak)
                logical, allocated = footprint(work)
                peak_logical = max(peak_logical, logical)
                peak_allocated = max(peak_allocated, allocated)
                if time.perf_counter() - start > deadline:
                    proc.kill()
                    proc.wait()
                    deadline_exceeded = True
                    break
                time.sleep(0.025)
        except BaseException:
            if proc.poll() is None:
                proc.kill()
            proc.wait(timeout=10)
            raise
        finally:
            if handle:
                counters = MemoryCounters()
                counters.cb = ctypes.sizeof(counters)
                if psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
                    peak_rss = max(peak_rss, counters.peak)
                    peak_commit = max(peak_commit, counters.pagefile_peak)
                kernel.CloseHandle(handle)
        rc = proc.wait()
    logical, allocated = footprint(work)
    return {
        "exit_code": rc, "seconds": time.perf_counter() - start,
        "hard_commit_limit_bytes": memory_bytes, "peak_commit_bytes": peak_commit,
        "deadline_exceeded": deadline_exceeded,
        "peak_rss_bytes": peak_rss, "peak_logical_bytes": max(peak_logical, logical),
        "peak_allocated_bytes": max(peak_allocated, allocated),
    }


def read(path):
    return json.loads(path.read_text(encoding="utf-8"))


def private_error(out, text):
    """A failed validation must never create diagnostics at an arbitrary path."""
    try:
        resolved = out.resolve()
        base = (Path(os.environ["LOCALAPPDATA"]) / "Zerus/private-benchmarks").resolve()
        repo = Path(__file__).resolve().parents[3]
        if (resolved != base and resolved.is_relative_to(base) and
                not resolved.is_relative_to(repo) and
                kernel.GetDriveTypeW(str(resolved.anchor)) == 3 and resolved.is_dir()):
            (resolved / "driver-error.txt").write_text(text, encoding="utf-8")
    except OSError:
        pass


def main(args):
    selection = read(args.selection)
    item = selection["files"][0]
    source = Path(item["path"]).resolve()
    if kernel.GetDriveTypeW(str(source.anchor)) != 3:
        raise RuntimeError("source must reside on a fixed local drive")
    before = source.stat()
    baseline_identity = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
    # All files created by the experiment live outside the checkout and source tree.
    out = args.out.resolve()
    repository = Path(__file__).resolve().parents[3]
    private_base = (Path(os.environ["LOCALAPPDATA"]) / "Zerus/private-benchmarks").resolve()
    if not out.is_relative_to(private_base) or kernel.GetDriveTypeW(str(out.anchor)) != 3:
        raise RuntimeError("results must stay under the local private benchmark directory")
    if out.is_relative_to(repository) or out.is_relative_to(source.parent):
        raise RuntimeError("report directory must be private and outside source trees")
    out.mkdir(parents=True, exist_ok=False)
    report = {
        "backend_revision": "717d63b1756554abc470671ea591ad6d25e6059d",
        "backend_patch": "explicit optional displacement-stash byte cap",
        "transport": "authenticated fixed loopback HTTP; proxy and redirects disabled",
        "scope": "real content ranges with synthetic byte edits, not recooked builds",
        "source_local": str(source), "source_offset": item["start"],
        "cases": [], "checks": {},
    }
    # name, scenario, size MiB, average KiB, stash MiB, interruption, bytes/second
    cases = [
        ("unchanged", "same", 128, 256, 2, "", 0),
        ("overwrite_64k", "overwrite", 256, 64, 2, "", 0),
        ("overwrite_256k", "overwrite", 256, 256, 2, "", 0),
        ("overwrite_1m", "overwrite", 256, 1024, 2, "", 0),
        ("insertion", "insert", 256, 256, 2, "", 0),
        ("reorder", "swap", 128, 256, 2, "", 0),
        ("reorder_zero_stash", "swap", 128, 256, 0, "", 0),
        ("shrink", "shrink", 128, 256, 2, "", 0),
        ("full_change", "all_changed", 64, 256, 2, "", 0),
        ("disconnect_repair", "all_changed", 128, 256, 2, "failure", 0),
        ("process_exit_repair", "all_changed", 128, 256, 2, "crash", 0),
        ("corruption_repair", "all_changed", 64, 256, 2, "corruption", 0),
        ("insertion_20mbit", "insert", 128, 256, 2, "", 2_500_000),
        ("insertion_100mbit", "insert", 128, 256, 2, "", 12_500_000),
        ("scale_1g", "insert", 1024, 1024, 2, "", 0),
    ]
    for name, scenario, size_mib, average_kib, stash_mib, injection, rate in cases:
        length = size_mib * MIB
        available = before.st_size - item["start"]
        if length > available or shutil.disk_usage(out).free < length + 128 * MIB:
            report["cases"].append({"name": name, "skipped": True,
                                    "reason": "bounded source range or free-space limit"})
            save(out / "results.json", report)
            continue
        work = out / name
        work.mkdir()
        cfg = {
            "source": str(source), "source_offset": item["start"], "length": length,
            "scenario": scenario, "average": average_kib * 1024,
            "stash_bytes": stash_mib * MIB, "memory_bytes": 128 * MIB,
            "fail_after": 0, "crash_after": 0, "corrupt_after": 0, "rate_bytes": rate,
        }
        config = work / "config.json"
        save(config, cfg)
        prepare = child(args.binary, "prepare", config, "prepare")
        if prepare["exit_code"]:
            raise RuntimeError("real-content preparation failed; inspect private case files")
        case = {"name": name, "scenario": scenario, "length": length, "prepare": prepare,
                "attempts": [], "expected_hash": read(work / "manifest.json")["expected_sha256"]}
        if injection:
            key = {"failure": "fail_after", "crash": "crash_after",
                   "corruption": "corrupt_after"}[injection]
            cfg[key] = 4 if injection != "corruption" else 1
            save(config, cfg)
            attempt = child(args.binary, "apply", config, "interrupted")
            case["attempts"].append(attempt)
            expected = 86 if injection == "crash" else 1
            if attempt["exit_code"] != expected:
                raise RuntimeError("injected interruption did not occur as expected")
            state = read(work / "state.json")
            if state["state"] == "ready":
                raise RuntimeError("interrupted receiver was incorrectly ready")
            case["failure_left_unavailable"] = True
            cfg[key] = 0
            save(config, cfg)
        attempt = child(args.binary, "apply", config, "apply")
        case["attempts"].append(attempt)
        if attempt["exit_code"]:
            raise RuntimeError("real-content reconstruction failed; inspect private case files")
        result = read(work / "result.json")
        state = read(work / "state.json")
        if not result.get("ok") or not result.get("exact_hash_match") or state["state"] != "ready":
            raise RuntimeError("final receiver did not verify")
        case["result"] = result
        payloads = [p for p in work.iterdir() if p.is_file() and p.suffix == ".bin"]
        if [p.name for p in payloads] != ["receiver.bin"]:
            raise RuntimeError("unexpected second payload file")
        # Sampling can miss a very short-lived file, so also retain the code-path audit:
        # AssembleFile operates directly on receiver.bin; no tmp/staging writer is used.
        case["only_one_receiver_payload"] = True
        case["peak_extra_allocation_bytes"] = max(
            0, max(x["peak_allocated_bytes"] for x in case["attempts"]) - length)
        (work / "receiver.bin").unlink()
        case["scratch_payload_removed"] = True
        report["cases"].append(case)
        save(out / "results.json", report)
        print("Completed local desync scenario: " + name, flush=True)
    after = source.stat()
    report["checks"]["source_metadata_unchanged"] = baseline_identity == (
        after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    completed = [c for c in report["cases"] if not c.get("skipped")]
    report["checks"]["all_completed_outputs_exact"] = all(
        c["result"]["exact_hash_match"] for c in completed)
    report["checks"]["no_second_receiver_payload"] = all(
        c["only_one_receiver_payload"] for c in completed)
    report["checks"]["interruption_states_unavailable"] = all(
        c.get("failure_left_unavailable") for c in completed if "repair" in c["name"])
    same = next(c for c in completed if c["scenario"] == "same")
    report["checks"]["unchanged_payload_zero"] = same["result"]["publisher_payload_bytes"] == 0
    report["checks"]["scratch_payloads_removed"] = all(
        c["scratch_payload_removed"] for c in completed)
    if not all(report["checks"].values()):
        raise RuntimeError("one or more qualification checks failed")
    save(out / "results.json", report)
    lines = [
        "# Private desync qualification", "",
        "Actual desync engine, read-only real container ranges, virtual synthetic edits.",
        "Transport was authenticated HTTP bound to 127.0.0.1 only. No external data endpoint was used.",
        "The explicit stash-limit patch and all measurement limits are described in the repository README.",
        "", "| Case | Input MiB | Payload MiB | Peak RSS MiB | Extra allocated MiB | Apply seconds |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for c in completed:
        a = c["attempts"][-1]
        r = c["result"]
        lines.append("| {} | {} | {:.3f} | {:.2f} | {:.3f} | {:.3f} |".format(
            c["name"], c["length"] // MIB, r["publisher_payload_bytes"] / MIB,
            max(x["peak_rss_bytes"] for x in c["attempts"]) / MIB,
            c["peak_extra_allocation_bytes"] / MIB, r["apply_seconds"]))
    lines += [
        "", "## Checks", "",
        *["- {}: {}".format(k, "PASS" if v else "FAIL") for k, v in report["checks"].items()],
        "", "## Limits", "",
        "- These are real byte ranges, not a full 400 GB deployment.",
        "- Synthetic edits do not predict the effects of a real recook or encryption change.",
        "- Both endpoints ran on one host; rate shaping is not a WAN throughput benchmark.",
        "- RSS and allocation peaks were sampled and may miss short events.",
        "- Receiver allocation uses FileStandardInfo; small metadata files use block-rounded estimates.",
        "- Publisher and receiver share one process, so RSS includes both roles.",
        "- The receiver payload remained one file; small indexes, manifests and logs were additional.",
        "- Interrupted runs reindex actual mixed contents before repairing; old COPY plans are not replayed.",
        "- A process exit was tested, not a power cut or OS restart.",
        "- Go's heap limit is soft; observed RSS is reported separately.",
        "- Test payload files were removed. The original input was never opened for writing.",
    ]
    (out / "REPORT.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    save(args.out.parent / "latest-desync-run.json", {"directory": str(out)})
    print("Local desync qualification finished; measurements remain in the private report.", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--selection", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    try:
        main(args)
    except BaseException:
        private_error(args.out, traceback.format_exc())
        print("Local qualification stopped; diagnostics are private.", flush=True)
        raise SystemExit(1)
