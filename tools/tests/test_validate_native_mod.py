import json
import os
import struct
import sys
import tempfile
import unittest
import zipfile

sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
from package_native_mod import build_package
from new_native_mod import create_project
from validate_native_mod import ValidationError, inspect_pe_exports, validate_package

EXPORTS = ["WmodGetApiVersion", "WmodInitializeV1", "WmodShutdownV1", "WmodIsInitializedV1", "WmodCanUnloadV1"]


def make_test_pe(exports=None, machine=0x8664):
    """Create a tiny PE32+ fixture with real export directory tables."""
    names = list(EXPORTS if exports is None else exports)
    data = bytearray(0x400)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 0x84, machine, 1, 0, 0, 0, 240, 0x2022)

    optional = 0x98
    struct.pack_into("<H", data, optional, 0x20B)
    struct.pack_into("<I", data, optional + 60, 0x200)
    struct.pack_into("<I", data, optional + 108, 16)
    struct.pack_into("<II", data, optional + 112, 0x1000, 0x100)

    section = optional + 240
    data[section:section + 8] = b".rdata\0\0"
    struct.pack_into("<IIII", data, section + 8, 0x200, 0x1000, 0x200, 0x200)

    function_count = len(names)
    struct.pack_into(
        "<IIHHIIIIIII", data, 0x200,
        0, 0, 0, 0, 0x1040, 1,
        function_count, len(names), 0x1050, 0x1070, 0x1090,
    )
    for index, name in enumerate(names):
        struct.pack_into("<I", data, 0x250 + index * 4, 0x1110 + index * 0x10)
        name_rva = 0x10A0 + index * 0x20
        struct.pack_into("<I", data, 0x270 + index * 4, name_rva)
        struct.pack_into("<H", data, 0x290 + index * 2, index)
        raw_name_offset = 0x2A0 + index * 0x20
        encoded = name.encode("ascii") + b"\0"
        data[raw_name_offset:raw_name_offset + len(encoded)] = encoded
    return bytes(data)


def base_manifest():
    return {
        "manifestVersion": 1,
        "id": "com.example.test",
        "name": "Test Native Mod",
        "version": "1.0.0",
        "description": "test package",
        "architecture": "x64",
        "module": "bin/test.dll",
        "targets": ["explorer.exe"],
    }


def make_package(entries):
    temp = tempfile.NamedTemporaryFile(suffix=".zip", delete=False)
    temp.close()
    with zipfile.ZipFile(temp.name, "w") as archive:
        for name, payload in entries:
            archive.writestr(name, payload)
    return temp.name


class NativeModValidatorTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.paths = []

    def tearDown(self):
        self.temp_dir.cleanup()
        for path in self.paths:
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass

    def package(self, entries):
        path = make_package(entries)
        self.paths.append(path)
        return path

    def test_accepts_valid_x64_package(self):
        path = self.package([
            ("manifest.json", json.dumps(base_manifest())),
            ("bin/test.dll", make_test_pe()),
        ])
        result = validate_package(path)
        self.assertEqual(result["id"], "com.example.test")
        self.assertEqual(set(result["exports"]), set(EXPORTS))

    def test_pe_parser_reads_required_exports(self):
        machine, names = inspect_pe_exports(make_test_pe())
        self.assertEqual(machine, 0x8664)
        self.assertEqual(names, set(EXPORTS))

    def test_rejects_missing_manifest(self):
        path = self.package([("readme.txt", "hello")])
        with self.assertRaisesRegex(ValidationError, "manifest.json"):
            validate_package(path)

    def test_rejects_traversal_path(self):
        path = self.package([("../outside.txt", "bad"), ("manifest.json", "{}")])
        with self.assertRaisesRegex(ValidationError, "unsafe archive path"):
            validate_package(path)

    def test_rejects_case_insensitive_duplicate_paths(self):
        path = self.package([("manifest.json", "{}"), ("MANIFEST.JSON", "{}")])
        with self.assertRaisesRegex(ValidationError, "duplicate or case-colliding"):
            validate_package(path)

    def test_rejects_windows_reserved_names(self):
        for reserved_name in ("CON.txt", "CONIN$.txt", "CONOUT$.dll", "LPT1.log"):
            with self.subTest(name=reserved_name):
                path = self.package([
                    ("manifest.json", "{}"),
                    (reserved_name, "bad"),
                ])
                with self.assertRaisesRegex(ValidationError, "reserved Windows filename"):
                    validate_package(path)

    def test_rejects_file_directory_conflicts(self):
        path = self.package([
            ("manifest.json", "{}"),
            ("assets", b"file"),
            ("assets/icon.dll", b"nested"),
        ])
        with self.assertRaisesRegex(ValidationError, "conflicts with a directory path"):
            validate_package(path)

    def test_rejects_invalid_manifest_id(self):
        manifest = base_manifest()
        manifest["id"] = "../bad"
        path = self.package([
            ("manifest.json", json.dumps(manifest)),
            ("bin/test.dll", make_test_pe()),
        ])
        with self.assertRaisesRegex(ValidationError, "id must"):
            validate_package(path)

    def test_rejects_unsupported_target_process(self):
        manifest = base_manifest()
        manifest["targets"] = ["notepad.exe"]
        path = self.package([
            ("manifest.json", json.dumps(manifest)),
            ("bin/test.dll", make_test_pe()),
        ])
        with self.assertRaisesRegex(ValidationError, "not allowed"):
            validate_package(path)

    def test_accepts_additional_windows_ui_targets(self):
        manifest = base_manifest()
        manifest["targets"] = [
            "explorer.exe",
            "StartMenuExperienceHost.exe",
            "ShellExperienceHost.exe",
            "SearchHost.exe",
            "TextInputHost.exe",
            "dwm.exe",
            "ApplicationFrameHost.exe",
            "LockApp.exe",
            "Widgets.exe",
            "Taskmgr.exe",
        ]
        path = self.package([
            ("manifest.json", json.dumps(manifest)),
            ("bin/test.dll", make_test_pe()),
        ])
        result = validate_package(path)
        self.assertEqual(result["targets"], manifest["targets"])

    def test_rejects_non_x64_machine(self):
        path = self.package([
            ("manifest.json", json.dumps(base_manifest())),
            ("bin/test.dll", make_test_pe(machine=0x14C)),
        ])
        with self.assertRaisesRegex(ValidationError, "AMD64/x64"):
            validate_package(path)

    def test_rejects_missing_lifecycle_export(self):
        path = self.package([
            ("manifest.json", json.dumps(base_manifest())),
            ("bin/test.dll", make_test_pe(exports=EXPORTS[:-1])),
        ])
        with self.assertRaisesRegex(ValidationError, "missing required exports"):
            validate_package(path)

    def test_pe_parser_rejects_short_input(self):
        with self.assertRaisesRegex(ValidationError, "missing MZ header"):
            inspect_pe_exports(b"MZ")

    def test_builder_creates_valid_deterministic_package(self):
        root = self.temp_dir.name
        manifest_path = os.path.join(root, "manifest.json")
        dll_path = os.path.join(root, "test.dll")
        output1 = os.path.join(root, "out", "test1.zip")
        output2 = os.path.join(root, "out", "test2.zip")
        with open(manifest_path, "w", encoding="utf-8") as handle:
            json.dump(base_manifest(), handle, separators=(",", ":"))
        with open(dll_path, "wb") as handle:
            handle.write(make_test_pe())

        first = build_package(manifest_path, dll_path, output1)
        second = build_package(manifest_path, dll_path, output2)
        self.assertEqual(first["id"], "com.example.test")
        self.assertEqual(second["id"], "com.example.test")
        with open(output1, "rb") as handle:
            bytes1 = handle.read()
        with open(output2, "rb") as handle:
            bytes2 = handle.read()
        self.assertEqual(bytes1, bytes2)
        with zipfile.ZipFile(output1) as archive:
            self.assertEqual(set(archive.namelist()), {"manifest.json", "bin/test.dll"})

    def test_builder_rejects_non_zip_output(self):
        root = self.temp_dir.name
        manifest_path = os.path.join(root, "manifest.json")
        dll_path = os.path.join(root, "test.dll")
        with open(manifest_path, "w", encoding="utf-8") as handle:
            json.dump(base_manifest(), handle)
        with open(dll_path, "wb") as handle:
            handle.write(make_test_pe())
        with self.assertRaisesRegex(ValidationError, "must end with .zip"):
            build_package(manifest_path, dll_path, os.path.join(root, "bad.output"))


    def test_scaffold_creates_expected_project_files(self):
        destination = create_project(
            "com.example.generated",
            "Generated Mod",
            self.temp_dir.name,
            target="explorer.exe",
        )
        expected = {"manifest.json", "mod.cpp", "CMakeLists.txt", "README.md"}
        self.assertEqual({name for name in os.listdir(destination)}, expected)
        with open(os.path.join(destination, "manifest.json"), encoding="utf-8") as handle:
            manifest = json.load(handle)
        self.assertEqual(manifest["id"], "com.example.generated")
        self.assertEqual(manifest["module"], "bin/com_example_generated.dll")
        self.assertEqual(manifest["targets"], ["explorer.exe"])
        with open(os.path.join(destination, "CMakeLists.txt"), encoding="utf-8") as handle:
            cmake = handle.read()
        self.assertIn("$" + "{WMOD_SDK_DIR}", cmake)
        self.assertNotIn("\\" + "$" + "{WMOD_SDK_DIR}", cmake)
        self.assertIn("target_link_libraries(com_example_generated PRIVATE winhttp dbghelp psapi)", cmake)
        with open(os.path.join(destination, "mod.cpp"), encoding="utf-8") as handle:
            source = handle.read()
        for symbol in EXPORTS:
            self.assertIn(symbol, source)
        self.assertIn("windhawk_nonhook_compat.h", source)
        self.assertIn("windhawk_hook_compat.h", source)
        self.assertIn("windhawk_symbol_compat.h", source)
        self.assertIn("Wh_ApplyHookOperations()", source)
        self.assertIn("Wh_Log(", source)
        self.assertIn("x64 function-hook registration/application", open(os.path.join(destination, "README.md"), encoding="utf-8").read())

    def test_c_scaffold_creates_c_only_lifecycle_project(self):
        destination = create_project(
            "com.example.c_generated",
            "Generated C Mod",
            self.temp_dir.name,
            language="c",
        )
        expected = {"manifest.json", "mod.c", "CMakeLists.txt", "README.md"}
        self.assertEqual({name for name in os.listdir(destination)}, expected)
        with open(os.path.join(destination, "manifest.json"), encoding="utf-8") as handle:
            manifest = json.load(handle)
        self.assertEqual(manifest["module"], "bin/com_example_c_generated.dll")
        with open(os.path.join(destination, "CMakeLists.txt"), encoding="utf-8") as handle:
            cmake = handle.read()
        self.assertIn("project(com_example_c_generated LANGUAGES C)", cmake)
        self.assertIn("add_library(com_example_c_generated SHARED mod.c)", cmake)
        self.assertIn("${WMOD_SDK_DIR}", cmake)
        self.assertNotIn("windhawk_hook_compat.cpp", cmake)
        with open(os.path.join(destination, "mod.c"), encoding="utf-8") as handle:
            source = handle.read()
        for symbol in EXPORTS:
            self.assertIn(symbol, source)
        self.assertIn("WMOD_00047_ABI_VERSION", source)
        self.assertIn("InterlockedExchange", source)
        self.assertNotIn("std::", source)
        self.assertNotIn('extern "C"', source)
        with open(os.path.join(destination, "README.md"), encoding="utf-8") as handle:
            readme = handle.read()
        self.assertIn("C lifecycle ABI", readme)
        self.assertIn("without a sandbox", readme)

    def test_rust_scaffold_creates_language_neutral_cdylib_project(self):
        destination = create_project(
            "com.example.rust_generated",
            "Generated Rust Mod",
            self.temp_dir.name,
            target="explorer.exe",
            language="rust",
        )
        expected = {"manifest.json", "Cargo.toml", "Cargo.lock", "src", "README.md"}
        self.assertEqual({name for name in os.listdir(destination)}, expected)
        with open(os.path.join(destination, "manifest.json"), encoding="utf-8") as handle:
            manifest = json.load(handle)
        self.assertEqual(manifest["module"], "bin/com_example_rust_generated.dll")
        with open(os.path.join(destination, "Cargo.toml"), encoding="utf-8") as handle:
            cargo = handle.read()
        self.assertIn('crate-type = ["cdylib"]', cargo)
        self.assertIn('name = "com_example_rust_generated"', cargo)
        self.assertIn('name = "com-example-rust-generated"', cargo)
        with open(os.path.join(destination, "Cargo.lock"), encoding="utf-8") as handle:
            lock = handle.read()
        self.assertIn('name = "com-example-rust-generated"', lock)
        with open(os.path.join(destination, "src", "lib.rs"), encoding="utf-8") as handle:
            source = handle.read()
        for symbol in EXPORTS:
            self.assertIn(symbol, source)
        self.assertIn('extern "system"', source)
        self.assertIn("AtomicBool", source)
        self.assertFalse(os.path.exists(os.path.join(destination, "mod.cpp")))
        self.assertFalse(os.path.exists(os.path.join(destination, "CMakeLists.txt")))

    def test_rust_scaffold_normalizes_leading_separator_in_package_name(self):
        destination = create_project(
            "-leading",
            "Leading Separator Rust Mod",
            self.temp_dir.name,
            language="rust",
        )
        with open(os.path.join(destination, "manifest.json"), encoding="utf-8") as handle:
            manifest = json.load(handle)
        self.assertEqual(manifest["module"], "bin/_leading.dll")
        with open(os.path.join(destination, "Cargo.toml"), encoding="utf-8") as handle:
            cargo = handle.read()
        self.assertIn('name = "leading"', cargo)
        self.assertIn('name = "_leading"', cargo)
        with open(os.path.join(destination, "Cargo.lock"), encoding="utf-8") as handle:
            lock = handle.read()
        self.assertIn('name = "leading"', lock)

    def test_scaffold_rejects_unknown_language(self):
        with self.assertRaisesRegex(ValidationError, "language must be cpp, c, or rust"):
            create_project("com.example.badlang", "Bad Language", self.temp_dir.name, language="python")

    def test_scaffold_refuses_invalid_id(self):
        with self.assertRaisesRegex(ValidationError, "id must"):
            create_project("../bad", "Bad", self.temp_dir.name)

    def test_scaffold_refuses_unsupported_process(self):
        with self.assertRaisesRegex(ValidationError, "allow-list"):
            create_project("com.example.bad", "Bad", self.temp_dir.name, target="notepad.exe")

    def test_scaffold_accepts_additional_windows_ui_target(self):
        destination = create_project(
            "com.example.dwm-mod",
            "DWM Mod",
            self.temp_dir.name,
            target="dwm.exe",
        )
        with open(destination / "manifest.json", encoding="utf-8") as handle:
            manifest = json.load(handle)
        self.assertEqual(manifest["targets"], ["dwm.exe"])

    def test_scaffold_does_not_overwrite_existing_project(self):
        destination = create_project("com.example.existing", "Existing", self.temp_dir.name)
        sentinel = os.path.join(destination, "keep.txt")
        with open(sentinel, "w", encoding="utf-8") as handle:
            handle.write("preserve")
        with self.assertRaisesRegex(ValidationError, "refusing to overwrite"):
            create_project("com.example.existing", "Replacement", self.temp_dir.name)
        with open(sentinel, encoding="utf-8") as handle:
            self.assertEqual(handle.read(), "preserve")


if __name__ == "__main__":
    unittest.main()
