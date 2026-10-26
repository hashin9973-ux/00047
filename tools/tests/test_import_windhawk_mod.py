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
  $hideIf: {Enabled: false}
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
            self.assertEqual(manifest["settings"][1]["hideIf"], {"Enabled": [False]})
            self.assertEqual(manifest["settings"][2]["showIf"], {"Enabled": [True]})
            self.assertIn("WMOD_DEFINE_WINDHAWK_LIFECYCLE_WITH_SETTINGS_CHANGED", (project / "windhawk_entry.cpp").read_text())
            cmake = (project / "CMakeLists.txt").read_text()
            self.assertIn("cxx_std_20", cmake)
            self.assertIn('"comctl32"', cmake)
            self.assertIn('"TEST_FLAG=1"', cmake)
            self.assertIn("range limits are not represented", " ".join(warnings))
            self.assertTrue(json.loads((project / "windhawk-import.json").read_text())["settingsBlock"])
            self.assertTrue((project / "WINDHAWK_README.md").exists())

    def test_import_report_includes_supported_and_unknown_api_findings(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source_text = SOURCE.replace(
                "BOOL Wh_ModInit() { return TRUE; }",
                'BOOL Wh_ModInit() { Wh_Log(L"starting"); Wh_SetFunctionHook(nullptr, nullptr, nullptr); return TRUE; }',
            ) + "\nvoid Wh_MadeUpThing();\n"
            source = self.source(root, source_text)
            project, warnings = import_project(source, root / "out")
            report = json.loads((project / "windhawk-import.json").read_text(encoding="utf-8"))
            compatibility = report["apiCompatibility"]
            findings = {item["name"]: item for item in compatibility["findings"]}
            self.assertEqual(findings["Wh_Log"]["category"], "supported-with-limitations")
            self.assertEqual(findings["Wh_SetFunctionHook"]["category"], "supported-with-limitations")
            self.assertEqual(findings["Wh_MadeUpThing"]["category"], "unknown-api")
            self.assertTrue(compatibility["requiresManualReview"])
            self.assertTrue(any("Wh_MadeUpThing" in warning for warning in warnings))

    def test_indented_visibility_condition_maps_are_imported(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source_text = SOURCE.replace(
                "$showIf: {Enabled: true}",
                "$showIf:\\n    #! Enabled: true",
            )
            project, _ = import_project(self.source(root, source_text), root / "out")
            manifest = json.loads((project / "manifest.json").read_text(encoding="utf-8"))
            opacity = next(item for item in manifest["settings"] if item["id"] == "Opacity")
            self.assertEqual(opacity["showIf"], {"Enabled": [True]})

    def test_rejects_unknown_visibility_reference_and_cycles(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            unknown = SOURCE.replace("$showIf: {Enabled: true}", "$showIf: {Missing: true}")
            with self.assertRaisesRegex(ImportError, "references unknown setting"):
                import_project(self.source(root, unknown), root / "out1")
            cycle = SOURCE.replace("$showIf: {Enabled: true}", "$showIf: {Mode: prefixed}")
            cycle = cycle.replace("$hideIf: {Enabled: false}", "$hideIf: {Opacity: 0.85}")
            with self.assertRaisesRegex(ImportError, "cycle"):
                import_project(self.source(root, cycle), root / "out2")

    def test_nonstandard_source_filename_is_normalized_for_wrapper):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "windhawk-import-smoke.cpp"
            source.write_text(SOURCE, encoding="utf-8")
            project, _ = import_project(source, root / "out")
            self.assertTrue((project / "mod.cpp").is_file())
            self.assertIn('#include "mod.cpp"', (project / "windhawk_entry.cpp").read_text(encoding="utf-8"))

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
            with self.assertRaisesRegex(ImportError, "Wh_ModInit is present but its signature is not supported"):
                import_project(self.source(root, SOURCE.replace("BOOL Wh_ModInit()", "void Wh_ModInit()")), root / "out")

    def test_refuses_to_overwrite_destination(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "out" / "example-mod").mkdir(parents=True)
            with self.assertRaisesRegex(ImportError, "refusing to overwrite"):
                import_project(self.source(root), root / "out")


if __name__ == "__main__":
    unittest.main()
