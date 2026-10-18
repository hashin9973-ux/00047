import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from audit_windhawk_mod import IDENTIFIER, SUPPORTED, UNSUPPORTED, audit_paths, audit_text, mask_cpp_noncode


class WindhawkAuditTests(unittest.TestCase):
    def test_classifies_supported_unsupported_unknown_and_lifecycle(self):
        source = r'''
        // Wh_Disasm(address, &result) is only an example in a comment.
        const wchar_t* example = L"Wh_SetFunctionHook(target, hook, &original)";
        int value = Wh_GetIntSetting(L"style[%d]", 2);
        Wh_SetFunctionHook(target, hook, &original);
        Wh_ModInit();
        Wh_MadeUpThing();
        '''
        findings = {item["name"]: item for item in audit_text("mod.cpp", source)}
        self.assertEqual(findings["Wh_GetIntSetting"]["category"], "supported-with-limitations")
        self.assertEqual(findings["Wh_SetFunctionHook"]["category"], "unsupported")
        self.assertEqual(findings["Wh_ModInit"]["category"], "adapter-available-lifecycle")
        self.assertEqual(findings["Wh_MadeUpThing"]["category"], "unknown-api")
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
        header = root / "sdk" / "native-mod" / "windhawk_nonhook_compat.h"
        code = mask_cpp_noncode(header.read_text(encoding="utf-8"))
        identifiers = set(IDENTIFIER.findall(code))
        missing = set(SUPPORTED) - identifiers
        self.assertFalse(missing, f"SDK header missing advertised supported APIs: {sorted(missing)}")
        stubs = set(UNSUPPORTED) & identifiers
        self.assertFalse(stubs, f"Unsupported APIs must not be declared as compatibility stubs: {sorted(stubs)}")

    def test_settings_changed_callback_has_opt_in_adapter(self):
        report = audit_text("mod.cpp", "void Wh_ModSettingsChanged() {}")
        self.assertEqual(report[0]["category"], "adapter-available-lifecycle")


if __name__ == "__main__":
    unittest.main()
