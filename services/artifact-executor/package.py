"""Package the exact offline-built Windows helper and its reviewed notices."""
import argparse
import hashlib
import json
from pathlib import Path
import zipfile

REVISION="717d63b1756554abc470671ea591ad6d25e6059d"
EXAMPLES=("identity-init.json","executor.json","profile.json","performance-policy.json","scenario/scenario.json","scenario/run.ps1")

def digest(path):
    with path.open("rb") as stream:return hashlib.file_digest(stream,"sha256").hexdigest()

def package(binary,notices,output,examples_root=None):
    binary,notices,output=binary.resolve(),notices.resolve(),output.resolve()
    manifest=json.loads((notices/"build.json").read_text(encoding="utf-8"))
    if manifest.get("desync_revision")!=REVISION or digest(binary)!=manifest.get("binary_sha256") or digest(notices/"inventory.json")!=manifest.get("inventory_sha256"):
        raise ValueError("binary or linked-license inventory differs from its offline build")
    files={"hgs-executor.exe":binary,"licenses/build.json":notices/"build.json","licenses/inventory.json":notices/"inventory.json","licenses/NOTICES.md":notices/"NOTICES.md"}
    for component in json.loads((notices/"inventory.json").read_text(encoding="utf-8")):
        for relative,expected in component["texts"].items():
            path=(notices/relative).resolve()
            if not path.is_relative_to(notices) or path.is_symlink() or digest(path)!=expected:raise ValueError("reviewed notice changed")
            files["licenses/"+relative]=path
    here=Path(__file__).resolve().parent
    files["README.md"]=here/"README.md"
    examples_root=(examples_root or here/"examples").resolve()
    for relative in EXAMPLES:
        path=examples_root/relative
        if path.is_symlink() or not path.resolve().is_relative_to(examples_root) or not path.is_file():raise ValueError("reviewed example missing or redirected")
        files["examples/"+relative]=path
    output.parent.mkdir(parents=True,exist_ok=True)
    if output.exists():raise ValueError("package output must be new")
    with zipfile.ZipFile(output,"x",compression=zipfile.ZIP_DEFLATED,compresslevel=6) as archive:
        for name,path in sorted(files.items()):
            entry=zipfile.ZipInfo("hgs-executor-windows-x86_64/"+name,date_time=(2026,11,25,0,0,0))
            entry.compress_type=zipfile.ZIP_DEFLATED
            archive.writestr(entry,path.read_bytes())
    print("Standalone executor package and reviewed notices verified.")

if __name__=="__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,required=True);parser.add_argument("--notices",type=Path,required=True);parser.add_argument("--output",type=Path,required=True)
    args=parser.parse_args();package(args.binary,args.notices,args.output)
