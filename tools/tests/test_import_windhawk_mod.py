import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from import_windhawk_mod import ImportError, import_project

SOURCE = r'''// ==WindhawkMod==
// @id example-mod
// @name Example Mod
// @description A test source
// @version 2.1
// @include explorer.exe
// @compilerOptions -std=c++20 -lcomctl32 -DTEST_FLAG=1
// ==/WindhawkMod==
// ==WindhawkModSettings==
/*
- Enabled: true
  $name: Enabled
- Mode: prefixed
  $name: Display mode
  $options:
    - prefixed: Prefix
    - unchanged: Original
- Opacity: 0.85
  $float: true
  $min: 0
  $max: 1
  $showIf: {Enabled: true}
*/
// ==/WindhawkModSettings==
#include <windhawk_api.h>
BOOL Wh_ModInit() { return TRUE; }
void Wh_ModSettingsChanged() {}
// ==WindhawkModReadme==
/* Example readme */
// ==/WindhawkModReadme==
'''


class WindhawkImportTests(unittest.TestCase):
    def source(self, root: Path, value: str = SOURCE) -> Path:
        path = root / "mod.cpp"
        path.write_text(value, encoding="utf-8")
        return path

    def test_import_manifest_settings_wrapper_and_build_options(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            project, warnings = import_project(self.source(root), root / "out")
            manifest = json.loads((project / "manifest.json").read_text())
            self.assertEqual(manifest["id"], "example-mod")
            self.assertEqual(manifest["targets"], ["explorer.exe"])
            self.assertEqual(manifest["module"], "bin/example_mod.dll")
            self.assertEqual(
                [(x["id"], x["type"], x["default"]) for x in manifest["settings"]],
                [("Enabled", "boolean", True), ("Mode", "select", "prefixed"), ("Opacity", "string", "0.85")],
            )
            self.assertEqual(manifest["settings"][1]["options"], [
                {"value": "prefixed", "label": "Prefix"},
                {"value": "unchanged", "label": "Original"},
            ])
            self.assertIn("WMOD_DEFINE_WINDHAWK_LIFECYCLE_WITH_SETTINGS_CHANGED", (project / "windhawk_entry.cpp").read_text())
            cmake = (project / "CMakeLists.txt").read_text()
            self.assertIn("cxx_std_20", cmake)
            self.assertIn('"comctl32"', cmake)
            self.assertIn('"TEST_FLAG=1"', cmake)
            self.assertIn("not translated", " ".join(warnings))
            self.assertTrue(json.loads((project / "windhawk-import.json").read_text())["settingsBlock"])
            self.assertTrue((project / "WINDHAWK_README.md").exists())

    def test_copies_local_quoted_dependencies(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "nested").mkdir()
            (root / "nested" / "helper.h").write_text("#pragma once\n", encoding="utf-8")
            source = self.source(root, SOURCE.replace("#include <windhawk_api.h>", '#include "nested/helper.h"\n#include <windhawk_api.h>'))
            project, _ = import_project(source, root / "out")
            self.assertTrue((project / "nested" / "helper.h").is_file())

    def test_rejects_wildcard_and_excluded_targets(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaisesRegex(ImportError, "exact allow-listed"):
                import_project(self.source(root, SOURCE.replace("@include explorer.exe", "@include explorer*.exe")), root / "out1")
            with self.assertRaisesRegex(ImportError, "@exclude"):
                import_project(self.source(root, SOURCE.replace("@include explorer.exe", "@include explorer.exe\n// @exclude explorer.exe")), root / "out2")

    def test_rejects_invalid_init_signature(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaisesRegex(ImportError, "must define BOOL Wh_ModInit"):
                import_project(self.source(root, SOURCE.replace("BOOL Wh_ModInit()", "void Wh_ModInit()")), root / "out")

    def test_refuses_to_overwrite_destination(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "out" / "example-mod").mkdir(parents=True)
            with self.assertRaisesRegex(ImportError, "refusing to overwrite"):
                import_project(self.source(root), root / "out")


if __name__ == "__main__":
    unittest.main()
