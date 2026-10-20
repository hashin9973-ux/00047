import hashlib
import json
import os
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from new_extension import create_project
from package_extension import build_package
from validate_extension import ValidationError, validate_package


def command_manifest(**overrides):
    component = {
        "id": "run",
        "name": "Run check",
        "kind": "command",
        "description": "One-shot test command.",
        "entrypoint": "scripts/main.py",
        "runtime": {
            "command": "python",
            "args": ["{entrypoint}"],
            "workingDirectory": "{extensionDir}",
            "timeoutMs": 5000,
        },
    }
    component.update(overrides)
    return {
        "manifestVersion": 1,
        "id": "com.example.extension-test",
        "name": "Extension test",
        "version": "1.0.0",
        "description": "An author-tooling test extension.",
        "components": [component],
    }


def make_zip(entries, directory):
    path = Path(directory) / f"test-{len(list(Path(directory).glob('*.zip')))}.zip"
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, payload in entries:
            archive.writestr(name, payload)
    return str(path)


class ExtensionAuthoringTests(unittest.TestCase):
    def test_accepts_valid_command_package(self):
        with tempfile.TemporaryDirectory() as directory:
            package = make_zip([
                ("manifest.json", json.dumps(command_manifest())),
                ("scripts/main.py", "print('ok')"),
            ], directory)
            report = validate_package(package)
            self.assertEqual(report["id"], "com.example.extension-test")
            self.assertEqual(report["components"], 1)

    def test_rejects_traversal_and_case_collisions(self):
        with tempfile.TemporaryDirectory() as directory:
            traversal = make_zip([
                ("../escape.txt", "not safe"),
                ("manifest.json", json.dumps(command_manifest())),
            ], directory)
            with self.assertRaisesRegex(ValidationError, "unsafe.*ZIP path"):
                validate_package(traversal)

            duplicate = make_zip([
                ("manifest.json", json.dumps(command_manifest())),
                ("MANIFEST.JSON", "{}"),
            ], directory)
            with self.assertRaisesRegex(ValidationError, "duplicate or case-colliding"):
                validate_package(duplicate)

    def test_rejects_windows_reserved_names_invalid_chars_and_ambiguous_trailing_chars(self):
        unsafe_paths = [
            "CON",
            "scripts/NUL.txt",
            "LPT1.data",
            "COM¹.log",
            "scripts/bad?.txt",
            "scripts/trailing./file.py",
            "scripts/trailing /file.py",
            "scripts/control\x1fchar.txt",
        ]
        with tempfile.TemporaryDirectory() as directory:
            for unsafe_path in unsafe_paths:
                with self.subTest(path=unsafe_path):
                    package = make_zip([
                        ("manifest.json", json.dumps(command_manifest())),
                        ("scripts/main.py", "print('ok')"),
                        (unsafe_path, "not safe"),
                    ], directory)
                    with self.assertRaises(ValidationError):
                        validate_package(package)

            with self.assertRaises(ValidationError):
                validate_package(make_zip([
                    ("manifest.json", json.dumps(command_manifest())),
                    ("scripts/CON.txt", "print('unsafe entrypoint')"),
                ], directory))

    def test_rejects_bad_timeout_placeholder_and_missing_entrypoint(self):
        with tempfile.TemporaryDirectory() as directory:
            bad_timeout = command_manifest()
            bad_timeout["components"][0]["runtime"]["timeoutMs"] = 50
            package = make_zip([
                ("manifest.json", json.dumps(bad_timeout)),
                ("scripts/main.py", "print('ok')"),
            ], directory)
            with self.assertRaisesRegex(ValidationError, "timeoutMs"):
                validate_package(package)

            bad_placeholder = command_manifest()
            bad_placeholder["components"][0]["runtime"]["args"] = ["{unknown}"]
            package = make_zip([
                ("manifest.json", json.dumps(bad_placeholder)),
                ("scripts/main.py", "print('ok')"),
            ], directory)
            with self.assertRaisesRegex(ValidationError, "unsupported placeholder"):
                validate_package(package)

            missing = make_zip([
                ("manifest.json", json.dumps(command_manifest())),
            ], directory)
            with self.assertRaisesRegex(ValidationError, "entrypoint is missing"):
                validate_package(missing)

    def test_package_is_reproducible_and_scaffold_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            project_root = Path(directory) / "projects"
            project = create_project(
                "com.example.scaffold-test",
                "Scaffold test",
                str(project_root),
                kind="command",
            )
            first = Path(directory) / "first.zip"
            second = Path(directory) / "second.zip"
            first_report = build_package(str(project), str(first))
            second_report = build_package(str(project), str(second))
            self.assertEqual(first_report, second_report)
            self.assertEqual(
                hashlib.sha256(first.read_bytes()).digest(),
                hashlib.sha256(second.read_bytes()).digest(),
            )
            self.assertEqual(validate_package(str(first))["id"], "com.example.scaffold-test")
            with self.assertRaisesRegex(ValidationError, "refusing to overwrite"):
                create_project(
                    "com.example.scaffold-test",
                    "Scaffold test",
                    str(project_root),
                    kind="command",
                )

    def test_packager_refuses_output_inside_project(self):
        with tempfile.TemporaryDirectory() as directory:
            project = create_project(
                "com.example.inside-output",
                "Inside output",
                directory,
                kind="command",
            )
            with self.assertRaisesRegex(ValidationError, "outside the project"):
                build_package(str(project), str(project / "extension.zip"))


if __name__ == "__main__":
    unittest.main()
