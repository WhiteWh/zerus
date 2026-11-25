"""License gate checks using synthetic public fixtures only."""
import tempfile
import unittest
from pathlib import Path

from collect_notices import check_component, digest, json_stream, notice_files


class LicenseGateTests(unittest.TestCase):
    def test_changed_missing_and_additional_texts_require_review(self):
        reviewed = {"example": {"version": "v1", "texts": {"LICENSE": digest("BSD")}}}
        check_component("example", "v1", {"LICENSE": "BSD"}, reviewed)
        for texts in [{}, {"LICENSE": "changed"}, {"LICENSE": "BSD", "NOTICE": "extra"}]:
            with self.subTest(texts=texts), self.assertRaises(RuntimeError):
                check_component("example", "v1", texts, reviewed)
        for name, version in [("unknown", "v1"), ("example", "v2")]:
            with self.subTest(name=name, version=version), self.assertRaises(RuntimeError):
                check_component(name, version, {"LICENSE": "BSD"}, reviewed)

    def test_nested_notices_are_collected_without_unselected_siblings(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "LICENSE").write_text("BSD", encoding="utf-8")
            nested = root / "internal/selected"
            nested.mkdir(parents=True)
            (nested / "LICENSE.txt").write_text("MIT", encoding="utf-8")
            sibling = root / "unused"
            sibling.mkdir()
            (sibling / "COPYING").write_text("unused", encoding="utf-8")
            self.assertEqual(notice_files(root, [nested]),
                             {"LICENSE": "BSD", "internal/selected/LICENSE.txt": "MIT"})
            with self.assertRaises(RuntimeError):
                notice_files(nested, [root])

    def test_go_json_stream_handles_unicode_and_multiple_records(self):
        self.assertEqual(list(json_stream(' \n{"name":"©"}\n {"second":true}\n')),
                         [{"name": "©"}, {"second": True}])


if __name__ == "__main__":
    unittest.main()
