#!/usr/bin/env python3
"""Validate 00047 native-mod ZIP packages before installing them.

Uses only the Python standard library. This is author-side validation, not a
replacement for the application's independent validation at install time.
"""
from __future__ import annotations

import argparse
import json
import os
import stat
import struct
import sys
import zipfile
from pathlib import PurePosixPath

MAX_COMPRESSED = 20 * 1024 * 1024
MAX_EXPANDED = 50 * 1024 * 1024
MAX_ENTRIES = 500
MAX_MANIFEST = 1024 * 1024
ALLOWED_TARGETS = {
    "explorer.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "searchhost.exe",
}
REQUIRED_EXPORTS = {
    "WmodGetApiVersion",
    "WmodInitializeV1",
    "WmodShutdownV1",
}


class ValidationError(ValueError):
    """Raised when a package violates the native-mod package contract."""


def _rva_offset(data: bytes, rva: int, sections: list[tuple[int, int, int, int]]) -> int:
    for virtual_address, virtual_size, raw_offset, raw_size in sections:
        span = max(virtual_size, raw_size)
        if virtual_address <= rva < virtual_address + span:
            delta = rva - virtual_address
            if delta >= raw_size:
                break
            offset = raw_offset + delta
            if offset >= len(data):
                break
            return offset
    raise ValidationError(f"PE RVA 0x{rva:x} does not map to file data")


def _cstring(data: bytes, offset: int) -> str:
    if offset < 0 or offset >= len(data):
        raise ValidationError("PE string offset is out of bounds")
    end = data.find(b"\0", offset, min(len(data), offset + 4096))
    if end < 0:
        raise ValidationError("unterminated PE export name")
    try:
        return data[offset:end].decode("ascii")
    except UnicodeDecodeError as exc:
        raise ValidationError("non-ASCII PE export name") from exc


def inspect_pe_exports(data: bytes) -> tuple[int, set[str]]:
    """Return (COFF machine, export names) from a bounded PE image."""
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise ValidationError("module is not a valid PE image (missing MZ header)")
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if pe_offset > len(data) - 24 or data[pe_offset:pe_offset + 4] != b"PE\0\0":
        raise ValidationError("module has an invalid PE signature")
    machine, section_count, _, _, _, optional_size, _ = struct.unpack_from(
        "<HHIIIHH", data, pe_offset + 4
    )
    optional = pe_offset + 24
    if section_count == 0 or section_count > 96:
        raise ValidationError("invalid PE section count")
    if optional_size < 112 or optional + optional_size > len(data):
        raise ValidationError("invalid PE optional header")
    magic = struct.unpack_from("<H", data, optional)[0]
    if magic != 0x20B:
        raise ValidationError("native mod must be PE32+ (64-bit)")
    directory_count = struct.unpack_from("<I", data, optional + 108)[0]
    if directory_count < 1 or optional_size < 120:
        raise ValidationError("PE image has no export directory")
    export_rva, export_size = struct.unpack_from("<II", data, optional + 112)
    if not export_rva or export_size < 40:
        raise ValidationError("PE image has no usable export directory")

    section_table = optional + optional_size
    if section_table + section_count * 40 > len(data):
        raise ValidationError("PE section table is truncated")
    sections = []
    for i in range(section_count):
        off = section_table + i * 40
        virtual_size, virtual_address, raw_size, raw_offset = struct.unpack_from(
            "<IIII", data, off + 8
        )
        sections.append((virtual_address, virtual_size, raw_offset, raw_size))

    export_off = _rva_offset(data, export_rva, sections)
    if export_off + 40 > len(data):
        raise ValidationError("PE export directory is truncated")
    fields = struct.unpack_from("<IIHHIIIIIII", data, export_off)
    number_of_names, names_rva = fields[7], fields[9]
    if number_of_names > 100_000:
        raise ValidationError("unreasonable PE export-name count")
    names_off = _rva_offset(data, names_rva, sections)
    if names_off + number_of_names * 4 > len(data):
        raise ValidationError("PE export-name table is truncated")
    names = set()
    for i in range(number_of_names):
        name_rva = struct.unpack_from("<I", data, names_off + i * 4)[0]
        names.add(_cstring(data, _rva_offset(data, name_rva, sections)))
    return machine, names


def validate_package(path: str) -> dict:
    if not os.path.isfile(path):
        raise ValidationError(f"package does not exist: {path}")
    if os.path.getsize(path) > MAX_COMPRESSED:
        raise ValidationError("compressed package exceeds 20 MiB")
    try:
        archive = zipfile.ZipFile(path)
    except (zipfile.BadZipFile, OSError) as exc:
        raise ValidationError(f"cannot read ZIP package: {exc}") from exc

    with archive:
        infos = archive.infolist()
        if not infos or len(infos) > MAX_ENTRIES:
            raise ValidationError("package must contain 1 to 500 entries")
        seen = set()
        expanded = 0
        for info in infos:
            name = info.filename
            normalized = name.replace("\\", "/")
            parts = PurePosixPath(normalized).parts
            if (
                not name or normalized.startswith("/") or
                any(part in ("", ".", "..") for part in parts) or
                (parts and ":" in parts[0])
            ):
                raise ValidationError(f"unsafe archive path: {name!r}")
            key = normalized.casefold().rstrip("/")
            if key in seen:
                raise ValidationError(f"duplicate archive path: {name!r}")
            seen.add(key)
            mode = (info.external_attr >> 16) & 0xFFFF
            if stat.S_ISLNK(mode):
                raise ValidationError(f"symbolic links are not allowed: {name!r}")
            if info.flag_bits & 0x1:
                raise ValidationError("encrypted ZIP entries are not supported")
            expanded += info.file_size
            if expanded > MAX_EXPANDED:
                raise ValidationError("expanded package exceeds 50 MiB")
            if info.file_size > 0 and info.compress_size == 0:
                raise ValidationError("invalid ZIP entry size")
            if info.compress_size and info.file_size / info.compress_size > 1000:
                raise ValidationError("suspicious ZIP compression ratio")

        manifest_info = next((i for i in infos if i.filename == "manifest.json"), None)
        if manifest_info is None or manifest_info.file_size > MAX_MANIFEST:
            raise ValidationError("root manifest.json is missing or exceeds 1 MiB")
        try:
            manifest = json.loads(archive.read(manifest_info).decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError, RuntimeError) as exc:
            raise ValidationError(f"manifest.json is invalid: {exc}") from exc
        if not isinstance(manifest, dict):
            raise ValidationError("manifest.json must contain a JSON object")
        if manifest.get("manifestVersion") != 1:
            raise ValidationError("manifestVersion must be 1")
        for field in ("id", "name", "version", "description"):
            value = manifest.get(field)
            if not isinstance(value, str) or not value.strip() or len(value) > 512:
                raise ValidationError(f"{field} must be a non-empty string (max 512 chars)")
        if manifest.get("architecture") != "x64":
            raise ValidationError("architecture must be x64")
        module = manifest.get("module")
        if not isinstance(module, str) or not module or "\\" in module:
            raise ValidationError("module must be a relative ZIP path using forward slashes")
        module_path = PurePosixPath(module)
        if module_path.is_absolute() or any(p in ("", ".", "..") for p in module_path.parts):
            raise ValidationError("module path must not escape the package")
        if not module.casefold().endswith(".dll"):
            raise ValidationError("module must point to a DLL")
        module_info = next((i for i in infos if i.filename == module), None)
        if module_info is None or module_info.is_dir():
            raise ValidationError(f"module DLL is missing from package: {module}")
        targets = manifest.get("targets")
        if not isinstance(targets, list) or not targets:
            raise ValidationError("targets must be a non-empty array")
        if any(not isinstance(t, str) or t.casefold() not in ALLOWED_TARGETS for t in targets):
            raise ValidationError("targets contains a process not allowed by this SDK")
        if len({t.casefold() for t in targets}) != len(targets):
            raise ValidationError("targets must not contain duplicates")
        try:
            dll = archive.read(module_info)
        except (RuntimeError, zipfile.BadZipFile) as exc:
            raise ValidationError(f"cannot read module DLL: {exc}") from exc
        machine, exports = inspect_pe_exports(dll)
        if machine != 0x8664:
            raise ValidationError("module must target AMD64/x64 (COFF machine 0x8664)")
        missing = REQUIRED_EXPORTS - exports
        if missing:
            raise ValidationError("module is missing required exports: " + ", ".join(sorted(missing)))
        return {
            "id": manifest["id"],
            "name": manifest["name"],
            "version": manifest["version"],
            "module": module,
            "targets": targets,
            "exports": sorted(REQUIRED_EXPORTS),
        }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", help="path to a native-mod ZIP package")
    args = parser.parse_args(argv)
    try:
        result = validate_package(args.package)
    except ValidationError as exc:
        print(f"INVALID: {exc}", file=sys.stderr)
        return 1
    print("VALID: native-mod package passed author-side checks")
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
