import importlib.util
import json
import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("generate_i18n_catalog", HERE / "generate-i18n-catalog.py")
generator = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(generator)


class GeneratedCatalog(unittest.TestCase):
    def test_checked_in_catalog_is_current(self):
        self.assertEqual(generator.DESTINATION.read_text(encoding="utf-8"), generator.generate())

    def test_disambiguated_sources_keep_their_own_translation(self):
        catalog = json.loads(generator.generate())
        self.assertEqual(catalog["Pause"], "Приостановить")
        self.assertEqual(catalog["media playback\u0004Pause"], "Пауза")
        self.assertEqual(catalog["media playback\u0004Play"], "Воспроизвести")
        self.assertFalse(any("\u0004" in key and not key.split("\u0004")[0] for key in catalog))

    def test_supplement_adds_only_valid_next_sources(self):
        catalog = {"Pause": "Приостановить"}
        merged = generator.supplement(catalog, {"Retry %1": "Повтор в %1", "%n tasks": ["%n задача", "%n задачи", "%n задач"]})
        self.assertEqual(merged["Retry %1"], "Повтор в %1")
        self.assertEqual(merged["Pause"], "Приостановить")
        for entries in ({"Pause": "Пауза"}, {"Retry %1": "Повтор"}, {"Retry %1": "Повтор в %1 %2"},
                        {"%n tasks": ["%n задача", "%n задач"]}, {"Tasks": ["Задача", "Задачи", "Задач"]},
                        {"Retry": " "}, {"Two values": "Один · два"}):
            with self.assertRaises(ValueError):
                generator.supplement(catalog, entries)

    def test_supplement_adds_next_only_disambiguated_sources(self):
        # Next's trc(disambiguation, source) for a source whose shared Qt
        # translation reads differently in one Next use; a key Qt already
        # translates stays Qt's, and every source rule applies to its source.
        catalog = {"Review": "Просмотреть", "media playback\u0004Play": "Воспроизвести"}
        merged = generator.supplement(catalog, {"team stage\u0004Review": "Ревью", "team stage\u0004Retry %1": "Повтор %1"})
        self.assertEqual(merged["team stage\u0004Review"], "Ревью")
        self.assertEqual(merged["Review"], "Просмотреть")
        for entries in ({"media playback\u0004Play": "Играть"}, {"\u0004Review": "Ревью"}, {"team stage\u0004": "Ревью"},
                        {"team\u0004stage\u0004Review": "Ревью"}, {"team stage\u0004Retry %1": "Повтор"},
                        {"team stage\u0004Two": "Один · два"}, {"team stage\u0004%n tasks": ["%n задача", "%n задач"]}):
            with self.assertRaises(ValueError):
                generator.supplement(catalog, entries)

    def test_supplement_keeps_its_entries_out_of_the_qt_catalog(self):
        entries = json.loads(generator.SUPPLEMENT.read_text(encoding="utf-8"))
        catalog = json.loads(generator.generate())
        self.assertTrue(entries)
        self.assertEqual(list(entries), sorted(entries))
        self.assertTrue(all(catalog[source] == value for source, value in entries.items()))

    def test_update_sorts_new_supplement_entries_and_check_never_writes(self):
        with TemporaryDirectory(prefix=".catalog-test-", dir=HERE) as scratch:
            source, destination = Path(scratch) / "supplement.json", Path(scratch) / "catalog.json"
            entries = {"Zerus supplement Zeta": "Зета", "Zerus supplement Alpha": "Альфа"}
            original = json.dumps(entries, ensure_ascii=False, indent=2) + "\n"
            source.write_text(original, encoding="utf-8")
            with patch.object(generator, "SUPPLEMENT", source), patch.object(generator, "DESTINATION", destination), redirect_stdout(StringIO()):
                expected = generator.generate()
                destination.write_text(expected, encoding="utf-8")
                with patch("sys.argv", ["generate-i18n-catalog.py", "--check"]), self.assertRaises(SystemExit):
                    generator.main()
                self.assertEqual(source.read_text(encoding="utf-8"), original)
                with patch("sys.argv", ["generate-i18n-catalog.py"]):
                    generator.main()
                actual = json.loads(source.read_text(encoding="utf-8"))
                self.assertEqual(actual, entries)
                self.assertEqual(list(actual), sorted(entries))
                self.assertEqual(destination.read_text(encoding="utf-8"), expected)
                with patch("sys.argv", ["generate-i18n-catalog.py", "--check"]):
                    generator.main()


if __name__ == "__main__":
    unittest.main()
