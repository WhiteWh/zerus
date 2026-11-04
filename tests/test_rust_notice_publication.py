"""Rust notice collection preserves Cargo's UTF-8 under a Windows locale."""
import json
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/collect-licenses.py"
TARGET = "x86_64-pc-windows-msvc"


class RustNoticePublication(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="zerus-rust-notices-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        # U+0101 includes the byte 0x81 in UTF-8, which CP1252 cannot decode.
        self.crate = self.root / "author-\u0101" / "example-library"
        self.crate.mkdir(parents=True)
        self.notice = "Copyright Example \u0101\nMIT notice.\n".encode("utf-8")
        (self.crate / "LICENSE").write_bytes(self.notice)
        package = {"id": "example-library@1.2.3", "name": "example-library", "version": "1.2.3",
                   "license": "MIT", "source": "registry+https://example.invalid/index",
                   "repository": "https://example.invalid/library", "description": "Library by \u0101",
                   "manifest_path": str(self.crate / "Cargo.toml")}
        self.metadata = json.dumps({"packages": [package], "resolve": {"nodes": [{"id": package["id"]}]}},
                                   ensure_ascii=False).encode("utf-8")
        self.destination = self.root / "notices"
        self.commands = []

    def windows_output(self, command, **options):
        """Model the runner's text-mode CP1252 default, using real UTF-8 bytes."""
        self.commands.append(command)
        if command[:2] == ["rustc", "-vV"]:
            output = f"rustc 1.99.1\nhost: {TARGET}\n".encode("utf-8")
        elif command[:2] == ["cargo", "metadata"]:
            output = self.metadata
        elif command[:2] == ["cargo", "tree"]:
            output = f"example-app v1.0.0 ({self.root / 'author-ā'})\nexample-library v1.2.3\n".encode("utf-8")
        else:
            self.fail(f"Unexpected subprocess: {command}")
        return output.decode(options.get("encoding") or "cp1252", options.get("errors") or "strict")

    def collect(self, *arguments):
        main = runpy.run_path(str(SCRIPT))["main"]
        with mock.patch.object(sys, "argv", [str(SCRIPT), str(self.destination), *arguments]), \
                mock.patch.object(subprocess, "check_output", side_effect=self.windows_output):
            main()

    def assert_notices(self):
        self.assertEqual((self.destination / "example-library-1.2.3/LICENSE").read_bytes(), self.notice)
        inventory = json.loads((self.destination / "inventory.json").read_bytes())
        self.assertEqual(inventory["targets"], [TARGET])
        self.assertEqual([package["name"] for package in inventory["packages"]], ["example-library"])

    def test_host_metadata_decodes_utf8_and_copies_the_original_notice_bytes(self):
        self.collect()
        self.assert_notices()

    def test_target_tree_decodes_utf8_with_the_shipped_manifest_features(self):
        manifest = str(self.root / "desktop/Cargo.toml")
        self.collect("--manifest-path", manifest, "--manifest-features", f"{manifest}=video", "--target", TARGET)
        self.assert_notices()
        for command in self.commands:
            self.assertIn("--manifest-path", command)
            self.assertIn(manifest, command)
            self.assertEqual(command[command.index("--features") + 1], "video")

    def test_invalid_utf8_metadata_fails_before_writing_notices(self):
        self.metadata = b'{"description":"\xff"}'
        with self.assertRaises(UnicodeDecodeError):
            self.collect()
        self.assertFalse(self.destination.exists())

    def test_unicode_casefold_ships_the_fallback_and_unicode_data_notices(self):
        metadata = json.loads(self.metadata)
        package = metadata["packages"][0]
        package.update(id="unicode-casefold@0.2.0", name="unicode-casefold", version="0.2.0",
                       license="MIT/Apache-2.0")
        metadata["resolve"]["nodes"] = [{"id": package["id"]}]
        self.metadata = json.dumps(metadata, ensure_ascii=False).encode("utf-8")
        (self.crate / "LICENSE").unlink()
        self.collect()
        shipped = self.destination / "unicode-casefold-0.2.0"
        self.assertEqual(sorted(path.name for path in shipped.iterdir()),
                         ["MIT.txt", "NOTICE.txt", "Unicode-DFS-2016.txt"])
        for path in shipped.iterdir():
            self.assertEqual(path.read_bytes(),
                             (ROOT / "packaging/rust-licenses/unicode-casefold" / path.name).read_bytes())
        self.assertIn("Chris Wong", (shipped / "NOTICE.txt").read_text(encoding="utf-8"))
        self.assertIn("1991-2016 Unicode", (shipped / "Unicode-DFS-2016.txt").read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
