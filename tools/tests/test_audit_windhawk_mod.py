import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from audit_windhawk_mod import IDENTIFIER, SUPPORTED, UNSUPPORTED, audit_paths, audit_text, mask_cpp_noncode, main, to_sarif


class WindhawkAuditTests(unittest.TestCase):

    def test_sarif_export_preserves_api_locations_and_severity(self):
        report = {
            "files_scanned": 1,
            "findings": [
                {
                    "name": "Wh_SetFunctionHook",
                    "category": "unsupported",
                    "reason": "Hook engine is unavailable.",
                    "locations": [{"file": "mod.cpp", "line": 17}],
                },
                {
                    "name": "Wh_GetIntSetting",
                    "category": "supported-with-limitations",
                    "reason": "Settings helper.",
                    "locations": [{"file": "mod.cpp", "line": 22}],
                },
            ],
            "summary": {"unsupported": 1, "supported-with-limitations": 1},
            "note": "Static audit only.",
        }
        sarif = to_sarif(report)
        self.assertEqual(sarif["version"], "2.1.0")
        results = sarif["runs"][0]["results"]
        self.assertEqual(len(results), 2)
        hook = next(item for item in results if "Wh_SetFunctionHook" in item["message"]["text"])
        self.assertEqual(hook["level"], "error")
        self.assertEqual(hook["locations"][0]["physicalLocation"]["region"]["startLine"], 17)
        supported = next(item for item in results if "Wh_GetIntSetting" in item["message"]["text"])
        self.assertEqual(supported["level"], "note")
        self.assertEqual(sarif["runs"][0]["properties"]["filesScanned"], 1)



    def test_sarif_uses_repo_relative_uri_for_in_tree_sources(self):
        report = {
            "files_scanned": 1,
            "findings": [{
                "name": "Wh_Log",
                "category": "supported-with-limitations",
                "reason": "Logging helper.",
                "locations": [{
                    "file": str(Path.cwd() / "tools" / "example.cpp"),
                    "line": 3,
                }],
            }],
            "summary": {"supported-with-limitations": 1},
            "note": "Static audit only.",
        }
        sarif = to_sarif(report)
        uri = sarif["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"]
        self.assertEqual(uri, "tools/example.cpp")

    def test_sarif_cli_emits_parseable_sarif(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "mod.cpp"
            source.write_text("Wh_SetFunctionHook(target, hook, &original);", encoding="utf-8")
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                result = main([str(source), "--sarif"])
            self.assertEqual(result, 0)
            parsed = json.loads(output.getvalue())
            self.assertEqual(parsed["version"], "2.1.0")
            self.assertEqual(parsed["runs"][0]["results"][0]["level"], "error")
            uri = parsed["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"]
            self.assertTrue(uri.startswith("file:"), uri)

    def test_classifies_supported_unsupported_unknown_and_lifecycle(self):
        source = r'''
        // Wh_Disasm(address, &result) is only an example in a comment.
        const wchar_t* example = L"Wh_SetFunctionHook(target, hook, &original)";
        int value = Wh_GetIntSetting(L"style[%d]", 2);
        Wh_SetFunctionHook(target, hook, &original);
        HANDLE symbols = Wh_FindFirstSymbol(module, nullptr, &symbolInfo);
        Wh_FindCloseSymbol(symbols);
        Wh_ModInit();
        Wh_MadeUpThing();
        '''
        findings = {item["name"]: item for item in audit_text("mod.cpp", source)}
        self.assertEqual(findings["Wh_GetIntSetting"]["category"], "supported-with-limitations")
        self.assertEqual(findings["Wh_SetFunctionHook"]["category"], "unsupported")
        self.assertEqual(findings["Wh_ModInit"]["category"], "adapter-available-lifecycle")
        self.assertEqual(findings["Wh_MadeUpThing"]["category"], "unknown-api")
        self.assertEqual(findings["Wh_FindFirstSymbol"]["category"], "supported-with-limitations")
        self.assertNotIn("Wh_Disasm", findings)

    def test_preserves_line_numbers_while_masking_comments_and_literals(self):
        masked = mask_cpp_noncode('Wh_Log(L"not Wh_Disasm");\n/* Wh_SetFunctionHook(); */\nWh_Disasm(p, &r);')
        self.assertIn("Wh_Log", masked)
        self.assertNotIn("Wh_SetFunctionHook", masked)
        findings = audit_text("sample.cpp", masked)
        disasm = next(item for item in findings if item["name"] == "Wh_Disasm")
        self.assertEqual(disasm["locations"][0]["line"], 3)

    def test_directory_scan_ignores_build_output_and_sdk_compat_header(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "src").mkdir()
            (root / "build").mkdir()
            (root / "sdk").mkdir()
            (root / "src" / "mod.cpp").write_text("Wh_GetIntSetting(L\"x\");", encoding="utf-8")
            (root / "build" / "generated.cpp").write_text("Wh_SetFunctionHook(a,b,c);", encoding="utf-8")
            (root / "sdk" / "windhawk_nonhook_compat.h").write_text(
                "inline int Wh_GetIntSetting(PCWSTR x);", encoding="utf-8"
            )
            report = audit_paths([root])
            self.assertEqual(report["files_scanned"], 1)
            names = {item["name"] for item in report["findings"]}
            self.assertEqual(names, {"Wh_GetIntSetting"})

    def test_finds_lifecycle_callbacks_without_call_syntax(self):
        report = audit_text("mod.cpp", "BOOL Wh_ModInit() { return TRUE; }")
        self.assertEqual(report[0]["category"], "adapter-available-lifecycle")

    def test_supported_matrix_matches_sdk_header(self):
        root = Path(__file__).resolve().parents[2]
        headers = [
            root / "sdk" / "native-mod" / "windhawk_nonhook_compat.h",
            root / "sdk" / "native-mod" / "windhawk_symbol_compat.h",
        ]
        code = "\\n".join(mask_cpp_noncode(header.read_text(encoding="utf-8")) for header in headers)
        identifiers = set(IDENTIFIER.findall(code))
        missing = set(SUPPORTED) - identifiers
        self.assertFalse(missing, f"SDK header missing advertised supported APIs: {sorted(missing)}")
        stubs = set(UNSUPPORTED) & identifiers
        self.assertFalse(stubs, f"Unsupported APIs must not be declared as compatibility stubs: {sorted(stubs)}")

    def test_settings_changed_callback_has_opt_in_adapter(self):
        report = audit_text("mod.cpp", "void Wh_ModSettingsChanged() {}")
        self.assertEqual(report[0]["category"], "adapter-available-lifecycle")

    def test_settings_changed_reload_variant_is_blocked(self):
        report = audit_text("mod.cpp", "BOOL Wh_ModSettingsChanged(BOOL* bReload) { *bReload = TRUE; return TRUE; }")
        finding = next(item for item in report if item["name"] == "Wh_ModSettingsChanged")
        self.assertEqual(finding["category"], "unsupported")
        self.assertIn("reload/unload protocol is not implemented", finding["reason"])


if __name__ == "__main__":
    unittest.main()
