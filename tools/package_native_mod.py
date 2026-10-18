#!/usr/bin/env python3
"""Build a deterministic, install-ready 00047 native-mod ZIP from a manifest and DLL."""
from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import zipfile
from pathlib import Path

from validate_native_mod import ValidationError, _safe_zip_path, validate_package


def build_package(manifest_path: str, dll_path: str, output_path: str) -> dict:
    manifest_file = Path(manifest_path)
    dll_file = Path(dll_path)
    output_file = Path(output_path)

    for file_path, label in ((manifest_file, "manifest"), (dll_file, "DLL")):
        if file_path.is_symlink():
            raise ValidationError(f"{label} input must not be a symbolic link")
        if not file_path.is_file():
            raise ValidationError(f"{label} input does not exist or is not a file: {file_path}")
    if output_file.suffix.lower() != ".zip":
        raise ValidationError("output filename must end with .zip")

    manifest_real = manifest_file.resolve()
    dll_real = dll_file.resolve()
    output_real = output_file.resolve()
    if output_real in (manifest_real, dll_real):
        raise ValidationError("output path must not overwrite the manifest or source DLL")
    if manifest_real == dll_real:
        raise ValidationError("manifest and DLL inputs must be different files")

    try:
        manifest_bytes = manifest_file.read_bytes()
        manifest = json.loads(manifest_bytes.decode("utf-8-sig"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValidationError(f"cannot read manifest JSON: {exc}") from exc
    if not isinstance(manifest, dict):
        raise ValidationError("manifest.json must contain a JSON object")
    module = manifest.get("module")
    if not isinstance(module, str):
        raise ValidationError("manifest must declare a relative module DLL path")
    _safe_zip_path(module, False)
    if not module.lower().endswith(".dll"):
        raise ValidationError("manifest module must end in .dll")

    output_file.parent.mkdir(parents=True, exist_ok=True)
    fd, staging_name = tempfile.mkstemp(
        prefix=f".{output_file.stem}.", suffix=".zip", dir=str(output_file.parent)
    )
    os.close(fd)
    try:
        with zipfile.ZipFile(staging_name, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            for internal_name, payload in (
                ("manifest.json", manifest_bytes),
                (module, dll_file.read_bytes()),
            ):
                info = zipfile.ZipInfo(internal_name, date_time=(1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.create_system = 3
                info.external_attr = (0o100644 & 0xFFFF) << 16
                archive.writestr(info, payload, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
        result = validate_package(staging_name)
        os.replace(staging_name, output_file)
        return result
    except Exception:
        try:
            os.unlink(staging_name)
        except FileNotFoundError:
            pass
        raise


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, help="source manifest.json")
    parser.add_argument("--dll", required=True, help="compiled x64 DLL matching manifest.module")
    parser.add_argument("--output", required=True, help="destination .zip package")
    args = parser.parse_args(argv)
    try:
        result = build_package(args.manifest, args.dll, args.output)
    except (ValidationError, OSError, zipfile.BadZipFile) as exc:
        print(f"PACKAGE FAILED: {exc}", file=sys.stderr)
        return 1
    print(f"PACKAGE CREATED: {args.output}")
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
