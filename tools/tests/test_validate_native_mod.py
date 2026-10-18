import json
import os
import tempfile
import unittest
import zipfile

from validate_native_mod import ValidationError, inspect_pe_exports, validate_package


def make_package(entries):
    temp = tempfile.NamedTemporaryFile(suffix=".zip", delete=False)
    temp.close()
    with zipfile.ZipFile(temp.name, "w") as archive:
        for name, payload in entries:
            archive.writestr(name, payload)
    return temp.name


class NativeModValidatorTests(unittest.TestCase):
    def tearDown(self):
        for path in getattr(self, "paths", []):
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass

    def package(self, entries):
        path = make_package(entries)
        if not hasattr(self, "paths"):
            self.paths = []
        self.paths.append(path)
        return path

    def test_rejects_missing_manifest(self):
        path = self.package([("readme.txt", "hello")])
        with self.assertRaisesRegex(ValidationError, "manifest.json"):
            validate_package(path)

    def test_rejects_traversal_path(self):
        path = self.package([("../outside.txt", "bad"), ("manifest.json", "{}")])
        with self.assertRaisesRegex(ValidationError, "unsafe archive path"):
            validate_package(path)

    def test_rejects_case_insensitive_duplicate_paths(self):
        path = self.package([
            ("manifest.json", "{}"),
            ("MANIFEST.JSON", "{}"),
        ])
        with self.assertRaisesRegex(ValidationError, "duplicate archive path"):
            validate_package(path)

    def test_rejects_non_pe_module(self):
        manifest = {
            "manifestVersion": 1,
            "id": "com.example.test",
            "name": "Test",
            "version": "1.0.0",
            "description": "test package",
            "architecture": "x64",
            "module": "bin/test.dll",
            "targets": ["explorer.exe"],
        }
        path = self.package([
            ("manifest.json", json.dumps(manifest)),
            ("bin/test.dll", b"not a PE file"),
        ])
        with self.assertRaisesRegex(ValidationError, "missing MZ header"):
            validate_package(path)

    def test_pe_parser_rejects_short_input(self):
        with self.assertRaisesRegex(ValidationError, "missing MZ header"):
            inspect_pe_exports(b"MZ")


if __name__ == "__main__":
    unittest.main()
