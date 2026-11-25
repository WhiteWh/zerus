"""Package custody tests use only generated public stand-ins."""
import hashlib
import json
from pathlib import Path
import tempfile
import shutil
import unittest
import zipfile
from package import package,REVISION,digest,EXAMPLES

class PackageTests(unittest.TestCase):
    def fixture(self):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup);root=Path(temporary.name)
        binary=root/"synthetic.exe";binary.write_bytes(b"synthetic executable stand-in")
        notices=root/"notices";notices.mkdir();(notices/"LICENSE").write_bytes(b"synthetic notice")
        (notices/"NOTICES.md").write_text("synthetic notices",encoding="utf-8")
        (notices/"inventory.json").write_text(json.dumps([{"texts":{"LICENSE":digest(notices/"LICENSE")}}]),encoding="utf-8")
        (notices/"build.json").write_text(json.dumps({"desync_revision":REVISION,"binary_sha256":digest(binary),"inventory_sha256":digest(notices/"inventory.json")}),encoding="utf-8")
        return binary,notices,root/"package.zip"

    def test_package_has_notices_and_no_private_runtime(self):
        binary,notices,output=self.fixture();package(binary,notices,output)
        with zipfile.ZipFile(output) as archive:
            names=archive.namelist();self.assertIn("hgs-executor-windows-x86_64/licenses/LICENSE",names)
            self.assertFalse(any(name.endswith(".key") or "/state/" in name for name in names))
        with self.assertRaises(ValueError):package(binary,notices,output)

    def test_changed_binary_or_notice_refuses_publication(self):
        for target in ("binary","notice"):
            binary,notices,output=self.fixture()
            (binary if target=="binary" else notices/"LICENSE").write_bytes(b"changed synthetic bytes")
            with self.assertRaises(ValueError):package(binary,notices,output)
            self.assertFalse(output.exists())

    def test_extra_runtime_identity_is_never_packaged(self):
        binary,notices,output=self.fixture();examples=output.parent/"examples";examples.mkdir()
        for relative in EXAMPLES:
            target=examples/relative;target.parent.mkdir(parents=True,exist_ok=True)
            shutil.copyfile(Path(__file__).parent/"examples"/relative,target)
        (examples/"identity").mkdir();(examples/"identity/identity.key").write_bytes(b"synthetic credential stand-in")
        (examples/"runtime.json").write_text("synthetic runtime data",encoding="utf-8")
        package(binary,notices,output,examples_root=examples)
        with zipfile.ZipFile(output) as archive:
            self.assertFalse(any(name.endswith(".key") or name.endswith("runtime.json") for name in archive.namelist()))

if __name__=="__main__":unittest.main()
