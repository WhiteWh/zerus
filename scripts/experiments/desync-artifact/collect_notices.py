"""Collect reviewed notices for the experimental binary; no network access."""
import hashlib
import json
from pathlib import Path
import subprocess

REVISION = "717d63b1756554abc470671ea591ad6d25e6059d"
MAIN_MODULE = "zerus.local/artifact-sync-prototype"
NOTICE_NAMES = {"LICENSE", "LICENCE", "COPYING", "NOTICE", "PATENTS"}


def digest(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def json_stream(text):
    decoder = json.JSONDecoder()
    while text.strip():
        obj, end = decoder.raw_decode(text.lstrip())
        yield obj
        text = text.lstrip()[end:]


def notice_files(root, directories):
    """Root and package-ancestor notices, including nested package overrides."""
    root = root.resolve()
    found = {}
    for directory in [root, *directories]:
        directory = directory.resolve()
        if not directory.is_relative_to(root):
            raise RuntimeError("package outside reviewed source root")
        while True:
            for path in directory.iterdir():
                if path.is_file() and path.name.upper().split(".")[0] in NOTICE_NAMES:
                    found[path.relative_to(root).as_posix()] = path.read_text(encoding="utf-8")
            if directory == root:
                break
            directory = directory.parent
    return dict(sorted(found.items()))


def check_component(name, version, texts, reviewed):
    if name not in reviewed or reviewed[name]["version"] != version:
        raise RuntimeError("unreviewed dependency or toolchain version")
    actual = {path: digest(text) for path, text in texts.items()}
    if actual != reviewed[name]["texts"]:
        raise RuntimeError("missing, changed or additional license text requires review")


def collect(go, work, binary, env, destination):
    here = Path(__file__).resolve().parent
    reviewed = json.loads((here / "reviewed-licenses.json").read_text(encoding="utf-8"))

    def run(*arguments):
        result = subprocess.run([str(go), *arguments], cwd=work, env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                encoding="utf-8", check=False, timeout=120,
                                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        if result.returncode:
            raise RuntimeError("offline license inventory command failed")
        return result.stdout

    build_info = run("version", "-m", str(binary))
    toolchain = build_info.splitlines()[0].rsplit(": ", 1)[-1]
    modules = {}
    for line in build_info.splitlines()[1:]:
        fields = line.strip().split("\t")
        if fields[0] == "dep":
            modules[fields[1]] = fields[2]
    if set(modules) != set(reviewed) - {"Go", "Zerus"}:
        raise RuntimeError("binary dependency set requires license review")
    packages = list(json_stream(run("list", "-mod=readonly", "-deps", "-json", ".")))
    goroot = Path(run("env", "GOROOT").strip())
    components = {"Go": {
        "version": toolchain,
        "texts": notice_files(goroot, [Path(p["Dir"]) for p in packages if p.get("Standard")]),
        "packages": sorted(p["ImportPath"] for p in packages if p.get("Standard")),
    }}
    for name, version in modules.items():
        selected = [p for p in packages if p.get("Module", {}).get("Path") == name]
        if not selected:
            raise RuntimeError("binary dependency absent from package inventory")
        module = selected[0]["Module"]
        if module["Version"] != version:
            raise RuntimeError("binary and package dependency versions disagree")
        replacement = module.get("Replace")
        if replacement and name != "github.com/folbricht/desync":
            raise RuntimeError("unreviewed module replacement")
        root = Path((replacement or module)["Dir"])
        components[name] = {
            "version": version,
            "texts": notice_files(root, [Path(p["Dir"]) for p in selected]),
            "packages": sorted(p["ImportPath"] for p in selected),
        }
        if name == "github.com/folbricht/desync":
            components[name]["revision"] = REVISION
            components[name]["modification"] = (
                "Optional displacement stash limit, synthetic metadata-only planner instrumentation, "
                "and Windows null-seed scratch files with delete-on-close lifetime.")
    repo = here.parents[2]
    components["Zerus"] = {"version": "prototype", "texts": {
        "LICENSE": (repo / "LICENSE").read_text(encoding="utf-8")}, "packages": [MAIN_MODULE]}
    for name, component in components.items():
        check_component(name, component["version"], component["texts"], reviewed)
    desync_text = components["github.com/folbricht/desync"]["texts"]["LICENSE"]
    if desync_text != (repo / "docs/licenses/desync-LICENSE.txt").read_text(encoding="utf-8"):
        raise RuntimeError("repository desync notice differs from reviewed dependency")

    # Publish only after every component passes. Inventory contains public names,
    # relative notice paths and digests; never local source/cache paths.
    destination.mkdir(parents=True, exist_ok=True)
    inventory = []
    summary = ["# Experimental artifact delivery notices", "",
               "Zerus remains MIT licensed. Desync is BSD-3-Clause licensed.", "",
               "Complete upstream documents follow. Composite documents retain notices",
               "for additional source scopes; their presence does not imply those scopes",
               "are linked. In particular, this Windows CGO-disabled build does not use",
               "BoringSSL or desync's optional cloud CLI drivers.", ""]
    for number, (name, component) in enumerate(sorted(components.items())):
        folder = f"{number:02d}"
        record = {k: v for k, v in component.items() if k != "texts"}
        record.update(name=name, license=reviewed[name]["license"], texts={})
        summary += [f"## {name} ({component['version']})", "", reviewed[name]["license"], ""]
        for relative, text in component["texts"].items():
            target = destination / folder / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8", newline="\n")
            record["texts"][f"{folder}/{relative}"] = digest(text)
            summary.append(f"- [{relative}]({folder}/{relative})")
        summary.append("")
        inventory.append(record)
    (destination / "inventory.json").write_text(
        json.dumps(inventory, indent=2) + "\n", encoding="utf-8")
    (destination / "NOTICES.md").write_text("\n".join(summary), encoding="utf-8")
    return len(inventory)
