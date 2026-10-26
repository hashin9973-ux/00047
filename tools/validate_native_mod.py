#!/usr/bin/env python3
"""Validate 00047 native-mod ZIP packages before installing them.

Uses only the Python standard library. This is author-side validation, not a
replacement for the application's independent validation at install time.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import stat
import struct
import sys
import zipfile

MAX_COMPRESSED = 20 * 1024 * 1024
MAX_EXPANDED = 50 * 1024 * 1024
MAX_ENTRIES = 500
MAX_MANIFEST = 1024 * 1024
ALLOWED_TARGETS = {
    "explorer.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "searchhost.exe",
    "textinputhost.exe",
    "dwm.exe",
    "applicationframehost.exe",
    "lockapp.exe",
    "widgets.exe",
    "taskmgr.exe",
}
REQUIRED_EXPORTS = {
    "WmodGetApiVersion",
    "WmodInitializeV1",
    "WmodShutdownV1",
    "WmodIsInitializedV1",
    "WmodCanUnloadV1",
}
ID_PATTERN = re.compile(r"^[a-z0-9._-]{1,100}$")


class ValidationError(ValueError):
    """Raised when a package violates the native-mod package contract."""


def _is_reserved_windows_name(part: str) -> bool:
    stem = part.split(".", 1)[0].rstrip(" ").casefold()
    if stem in {"con", "prn", "aux", "nul", "conin$", "conout$"}:
        return True
    return bool(re.fullmatch(r"(?:com|lpt)[1-9¹²³]", stem))


def _safe_zip_path(name: str, is_dir: bool = False) -> str:
    """Return a canonical relative path or reject unsafe Windows ZIP names."""
    if not name or "\\" in name or ":" in name or name.startswith("/"):
        raise ValidationError(f"unsafe archive path: {name!r}")
    canonical = name[:-1] if is_dir and name.endswith("/") else name
    parts = canonical.split("/")
    if not canonical or any(part in ("", ".", "..") for part in parts):
        raise ValidationError(f"unsafe archive path: {name!r}")
    if any(any(ch in part for ch in '<>:"\\|?*') for part in parts):
        raise ValidationError(f"Windows-incompatible archive path: {name!r}")
    if any(part.endswith((" ", ".")) for part in parts):
        raise ValidationError(f"Windows-normalized archive path is ambiguous: {name!r}")
    if any(_is_reserved_windows_name(part) for part in parts):
        raise ValidationError(f"reserved Windows filename in archive path: {name!r}")
    return canonical


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
    """Return (COFF machine, exported names) from a bounded PE image."""
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise ValidationError("module is not a valid PE image (missing MZ header)")
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if pe_offset > len(data) - 24 or data[pe_offset:pe_offset + 4] != b"PE\0\0":
        raise ValidationError("module has an invalid PE signature")
    machine, section_count, _, _, _, optional_size, _ = struct.unpack_from(
        "<HHIIIHH", data, pe_offset + 4
    )
    optional = pe_offset + 24
    if not 1 <= section_count <= 96:
        raise ValidationError("invalid PE section count")
    if optional_size < 120 or optional + optional_size > len(data):
        raise ValidationError("invalid PE optional header")
    if struct.unpack_from("<H", data, optional)[0] != 0x20B:
        raise ValidationError("native mod must be PE32+ (64-bit)")
    directory_count = struct.unpack_from("<I", data, optional + 108)[0]
    if directory_count < 1:
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
        if raw_offset > len(data) or raw_size > len(data) - raw_offset:
            raise ValidationError("PE section raw data is out of bounds")
        sections.append((virtual_address, virtual_size, raw_offset, raw_size))

    export_off = _rva_offset(data, export_rva, sections)
    if export_off + 40 > len(data):
        raise ValidationError("PE export directory is truncated")
    fields = struct.unpack_from("<IIHHIIIIIII", data, export_off)
    function_count, name_count, functions_rva, names_rva, ordinals_rva = (
        fields[6], fields[7], fields[8], fields[9], fields[10]
    )
    if name_count > 100_000 or function_count > 100_000:
        raise ValidationError("unreasonable PE export count")
    if name_count and not (functions_rva and names_rva and ordinals_rva):
        raise ValidationError("PE export address tables are incomplete")

    names_off = _rva_offset(data, names_rva, sections) if name_count else 0
    ordinals_off = _rva_offset(data, ordinals_rva, sections) if name_count else 0
    functions_off = _rva_offset(data, functions_rva, sections) if function_count else 0
    if names_off + name_count * 4 > len(data):
        raise ValidationError("PE export-name table is truncated")
    if ordinals_off + name_count * 2 > len(data):
        raise ValidationError("PE export-ordinal table is truncated")
    if functions_off + function_count * 4 > len(data):
        raise ValidationError("PE export-address table is truncated")

    names = set()
    for i in range(name_count):
        name_rva = struct.unpack_from("<I", data, names_off + i * 4)[0]
        ordinal = struct.unpack_from("<H", data, ordinals_off + i * 2)[0]
        if ordinal >= function_count:
            raise ValidationError("PE export ordinal is outside the function table")
        names.add(_cstring(data, _rva_offset(data, name_rva, sections)))
    return machine, names


def _validate_manifest(manifest: object, archive_names: set[str]) -> dict:
    if not isinstance(manifest, dict):
        raise ValidationError("manifest.json must contain a JSON object")
    version = manifest.get("manifestVersion")
    if isinstance(version, bool) or version != 1:
        raise ValidationError("manifestVersion must be 1")
    mod_id = manifest.get("id")
    if not isinstance(mod_id, str) or not ID_PATTERN.fullmatch(mod_id):
        raise ValidationError("id must use lowercase a-z, digits, dots, dashes or underscores")
    if mod_id.startswith(".") or mod_id.endswith(".") or ".." in mod_id:
        raise ValidationError("id must not start/end with a dot or contain consecutive dots")

    name = manifest.get("name")
    if not isinstance(name, str) or not name.strip() or len(name) > 100:
        raise ValidationError("name is required (max 100 characters)")
    release = manifest.get("version")
    if not isinstance(release, str) or not release.strip() or len(release.encode("utf-8")) > 40:
        raise ValidationError("version is required (max 40 UTF-8 bytes)")
    description = manifest.get("description", "")
    if not isinstance(description, str) or len(description) > 1000:
        raise ValidationError("description must be text (max 1000 characters)")
    if manifest.get("architecture") != "x64":
        raise ValidationError("architecture must be x64")

    module = manifest.get("module")
    if not isinstance(module, str) or _safe_zip_path(module) != module or not module.lower().endswith(".dll"):
        raise ValidationError("module must be a safe relative path to a .dll file")
    if module not in archive_names:
        raise ValidationError(f"module DLL is missing from package: {module}")

    targets = manifest.get("targets")
    if not isinstance(targets, list) or not 1 <= len(targets) <= len(ALLOWED_TARGETS):
        raise ValidationError(
            f"targets must contain between 1 and {len(ALLOWED_TARGETS)} supported process names"
        )
    if any(not isinstance(target, str) or target.casefold() not in ALLOWED_TARGETS for target in targets):
        raise ValidationError("targets contains a process not allowed by this SDK")
    if len({target.casefold() for target in targets}) != len(targets):
        raise ValidationError("targets must not contain duplicates")
    return manifest


def validate_package(path: str) -> dict:
    if not os.path.isfile(path):
        raise ValidationError(f"package does not exist: {path}")
    if not path.lower().endswith(".zip"):
        raise ValidationError("native mod packages must use the .zip extension")
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
        seen: set[str] = set()
        expanded = 0
        archive_names: set[str] = set()
        paths: list[tuple[str, bool]] = []
        for info in infos:
            name = info.filename
            is_dir = info.is_dir()
            canonical = _safe_zip_path(name, is_dir)
            key = canonical.casefold()
            if key in seen:
                raise ValidationError(f"duplicate or case-colliding archive path: {name!r}")
            seen.add(key)
            paths.append((key, is_dir))
            if not is_dir:
                archive_names.add(canonical)
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

        file_paths = {key for key, is_dir in paths if not is_dir}
        for key, is_dir in paths:
            parts = key.split("/")
            ancestors = ["/".join(parts[:i]) for i in range(1, len(parts))]
            if any(parent in file_paths for parent in ancestors):
                raise ValidationError("archive contains a file used as a parent directory")
            if not is_dir and any(other.startswith(key + "/") for other, _ in paths):
                raise ValidationError("archive file conflicts with a directory path")

        manifest_info = next((i for i in infos if i.filename == "manifest.json"), None)
        if manifest_info is None or manifest_info.is_dir() or manifest_info.file_size > MAX_MANIFEST:
            raise ValidationError("root manifest.json is missing or exceeds 1 MiB")
        try:
            manifest = json.loads(archive.read(manifest_info).decode("utf-8-sig"))
        except (UnicodeDecodeError, json.JSONDecodeError, RuntimeError, zipfile.BadZipFile) as exc:
            raise ValidationError(f"manifest.json is invalid: {exc}") from exc
        manifest = _validate_manifest(manifest, archive_names)

        module_info = next((i for i in infos if i.filename == manifest["module"]), None)
        if module_info is None or module_info.is_dir():
            raise ValidationError(f"module DLL is missing from package: {manifest['module']}")
        try:
            dll = archive.read(module_info)
            for info in infos:
                if info is not manifest_info and info is not module_info and not info.is_dir():
                    archive.read(info)  # Check CRCs for every package member.
        except (RuntimeError, zipfile.BadZipFile, OSError) as exc:
            raise ValidationError(f"cannot read package member: {exc}") from exc
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
            "module": manifest["module"],
            "targets": manifest["targets"],
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
