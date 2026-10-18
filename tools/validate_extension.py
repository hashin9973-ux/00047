#!/usr/bin/env python3
"""Validate an external 00047 extension ZIP using only the Python standard library.

Author-side checks complement, but do not replace, the application's install-time
validation. This tool cannot sandbox an extension: code executes as the current user.
"""
from __future__ import annotations

import argparse
import json
import os
import stat
import sys
import zipfile
from pathlib import PurePosixPath

MAX_COMPRESSED = 20 * 1024 * 1024
MAX_EXPANDED = 50 * 1024 * 1024
MAX_ENTRIES = 500
MAX_MANIFEST = 1024 * 1024
MIN_COMMAND_TIMEOUT_MS = 100
MAX_COMMAND_TIMEOUT_MS = 120_000
DEFAULT_COMMAND_TIMEOUT_MS = 15_000
ALLOWED_KINDS = {"app", "service", "command"}
ALLOWED_PLACEHOLDERS = {"extensionDir", "entrypoint", "componentId"}


class ValidationError(ValueError):
    """Raised when a package violates the extension package contract."""


def valid_id(value: object) -> bool:
    return (
        isinstance(value, str)
        and 0 < len(value.encode("utf-8")) <= 100
        and not value.startswith(".")
        and not value.endswith(".")
        and ".." not in value
        and all(ch.isascii() and (ch.islower() or ch.isdigit() or ch in "._-") for ch in value)
    )


def _safe_relative_path(value: object) -> bool:
    if not isinstance(value, str) or not value or value.startswith("/") or "\\" in value or ":" in value:
        return False
    parts = value.split("/")
    return all(part not in ("", ".", "..") for part in parts)


def _archive_path(name: str, is_dir: bool) -> str:
    if not name or "\\" in name or ":" in name or name.startswith("/"):
        raise ValidationError(f"unsafe ZIP path: {name!r}")
    normalized = name[:-1] if is_dir and name.endswith("/") else name
    path = PurePosixPath(normalized)
    if (
        not normalized
        or path.is_absolute()
        or any(part in ("", ".", "..") for part in normalized.split("/"))
        or (normalized.split("/")[0].startswith("//"))
    ):
        raise ValidationError(f"unsafe ZIP path: {name!r}")
    return normalized


def _check_text(value: object, label: str, limit: int, allow_empty: bool = False) -> str:
    if not isinstance(value, str):
        raise ValidationError(f"{label} must be text")
    if (not allow_empty and not value.strip()) or len(value) > limit:
        condition = f"at most {limit} characters"
        raise ValidationError(f"{label} must be non-empty and {condition}" if not allow_empty else f"{label} must be {condition}")
    return value


def _validate_manifest(manifest: object, files: dict[str, bool]) -> dict:
    if not isinstance(manifest, dict):
        raise ValidationError("manifest.json must contain a JSON object")
    if manifest.get("manifestVersion") != 1:
        raise ValidationError("manifestVersion must be 1")
    if not valid_id(manifest.get("id")):
        raise ValidationError("manifest id must use lowercase ASCII letters, digits, dots, dashes or underscores (max 100 bytes)")
    _check_text(manifest.get("name"), "manifest name", 100)
    version = _check_text(manifest.get("version"), "manifest version", 40)
    if len(version.encode("utf-8")) > 40:
        raise ValidationError("manifest version must be at most 40 UTF-8 bytes")
    _check_text(manifest.get("description", ""), "manifest description", 1000, allow_empty=True)

    components = manifest.get("components")
    if not isinstance(components, list) or not 1 <= len(components) <= 100:
        raise ValidationError("manifest must define between 1 and 100 components")

    component_ids: set[str] = set()
    for index, component in enumerate(components):
        label = f"components[{index}]"
        if not isinstance(component, dict):
            raise ValidationError(f"{label} must be an object")
        component_id = component.get("id")
        if not valid_id(component_id) or component_id in component_ids:
            raise ValidationError(f"{label}.id is invalid or duplicated")
        component_ids.add(component_id)
        _check_text(component.get("name"), f"{label}.name", 100)
        _check_text(component.get("description", ""), f"{label}.description", 1000, allow_empty=True)

        kind = component.get("kind")
        if kind not in ALLOWED_KINDS:
            raise ValidationError(f"{label}.kind must be app, service, or command")

        runtime = component.get("runtime")
        if not isinstance(runtime, dict):
            raise ValidationError(f"{label}.runtime must be an object")
        command = _check_text(runtime.get("command"), f"{label}.runtime.command", 2048)
        args = runtime.get("args", [])
        if not isinstance(args, list) or len(args) > 128:
            raise ValidationError(f"{label}.runtime.args must be an array with at most 128 items")
        for arg_index, arg in enumerate(args):
            if not isinstance(arg, str):
                raise ValidationError(f"{label}.runtime.args[{arg_index}] must be text")

        working_directory = runtime.get("workingDirectory")
        if working_directory is not None and not isinstance(working_directory, str):
            raise ValidationError(f"{label}.runtime.workingDirectory must be text")
        environment = runtime.get("environment", {})
        if not isinstance(environment, dict):
            raise ValidationError(f"{label}.runtime.environment must be an object")
        for key, value in environment.items():
            if (
                not isinstance(key, str)
                or not key
                or len(key) > 128
                or not all(ch.isascii() and (ch.isalnum() or ch == "_") for ch in key)
            ):
                raise ValidationError(f"{label}.runtime.environment contains an invalid variable name")
            if not isinstance(value, str):
                raise ValidationError(f"{label}.runtime.environment values must be text")

        timeout_ms = runtime.get("timeoutMs")
        if timeout_ms is not None:
            if kind != "command":
                raise ValidationError(f"{label}.runtime.timeoutMs is only valid for command components")
            if isinstance(timeout_ms, bool) or not isinstance(timeout_ms, int):
                raise ValidationError(f"{label}.runtime.timeoutMs must be an integer")
            if not MIN_COMMAND_TIMEOUT_MS <= timeout_ms <= MAX_COMMAND_TIMEOUT_MS:
                raise ValidationError(
                    f"{label}.runtime.timeoutMs must be between {MIN_COMMAND_TIMEOUT_MS} and {MAX_COMMAND_TIMEOUT_MS}"
                )

        entrypoint = component.get("entrypoint")
        if entrypoint is not None:
            if not _safe_relative_path(entrypoint):
                raise ValidationError(f"{label}.entrypoint must be a safe relative path using forward slashes")
            if entrypoint not in files or files[entrypoint]:
                raise ValidationError(f"{label}.entrypoint is missing or is not a file: {entrypoint}")

        templates = [command, *args]
        if working_directory is not None:
            templates.append(working_directory)
        templates.extend(environment.values())
        for text in templates:
            # Match the known runtime placeholders without treating escaped {{
            # braces as real placeholders, consistent with the application helper.
            offset = 0
            while offset < len(text):
                start = text.find("{", offset)
                if start < 0:
                    break
                if start + 1 < len(text) and text[start + 1] == "{":
                    offset = start + 2
                    continue
                end = text.find("}", start + 1)
                if end < 0:
                    break
                placeholder = text[start + 1:end]
                if placeholder not in ALLOWED_PLACEHOLDERS:
                    raise ValidationError(f"{label}: unsupported placeholder {{{placeholder}}}")
                if placeholder == "entrypoint" and entrypoint is None:
                    raise ValidationError(f"{label}: {{entrypoint}} is used but entrypoint is not set")
                offset = end + 1

    return manifest


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
            raise ValidationError("package must contain between 1 and 500 entries")
        files: dict[str, bool] = {}
        windows_keys: set[str] = set()
        total_expanded = 0

        for info in infos:
            if info.flag_bits & 0x1:
                raise ValidationError("encrypted ZIP entries are not supported")
            name = info.filename
            normalized = _archive_path(name, info.is_dir())
            key = normalized.rstrip("/").casefold()
            if key in windows_keys:
                raise ValidationError(f"duplicate or case-colliding ZIP path: {name!r}")
            windows_keys.add(key)
            files[normalized.rstrip("/")] = info.is_dir()

            mode = (info.external_attr >> 16) & 0xFFFF
            if stat.S_ISLNK(mode):
                raise ValidationError(f"symbolic links are not allowed: {name!r}")
            total_expanded += info.file_size
            if total_expanded > MAX_EXPANDED:
                raise ValidationError("expanded package exceeds 50 MiB")
            if normalized.casefold() == "manifest.json" and info.file_size > MAX_MANIFEST:
                raise ValidationError("manifest.json exceeds 1 MiB")
            if info.file_size > 0 and info.compress_size == 0:
                raise ValidationError(f"invalid ZIP size for {name!r}")
            if info.compress_size and info.file_size / info.compress_size > 1000:
                raise ValidationError(f"suspicious ZIP compression ratio for {name!r}")

        # Reject file/directory conflicts, e.g. a regular file "assets" and an
        # entry "assets/logo.png", before extraction tries to create both.
        for path_name, is_dir in files.items():
            parts = path_name.split("/")
            for i in range(1, len(parts)):
                parent = "/".join(parts[:i])
                if parent in files and not files[parent]:
                    raise ValidationError(f"file/directory path conflict at {parent!r}")
            if not is_dir:
                prefix = path_name.rstrip("/") + "/"
                if any(other.startswith(prefix) for other in files):
                    raise ValidationError(f"file/directory path conflict at {path_name!r}")

        manifest_info = next((info for info in infos if info.filename == "manifest.json"), None)
        if manifest_info is None or manifest_info.is_dir():
            raise ValidationError("root manifest.json is missing")
        if manifest_info.file_size > MAX_MANIFEST:
            raise ValidationError("manifest.json exceeds 1 MiB")
        try:
            manifest = json.loads(archive.read(manifest_info).decode("utf-8-sig"))
        except (UnicodeDecodeError, json.JSONDecodeError, RuntimeError, zipfile.BadZipFile) as exc:
            raise ValidationError(f"manifest.json is invalid: {exc}") from exc

        _validate_manifest(manifest, files)
        bad_entry = archive.testzip()
        if bad_entry is not None:
            raise ValidationError(f"ZIP CRC check failed for {bad_entry!r}")

        return {
            "id": manifest["id"],
            "name": manifest["name"],
            "version": manifest["version"],
            "components": len(manifest["components"]),
            "entries": len(infos),
            "expandedBytes": total_expanded,
        }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", help="path to an external extension ZIP package")
    args = parser.parse_args(argv)
    try:
        result = validate_package(args.package)
    except ValidationError as exc:
        print(f"INVALID: {exc}", file=sys.stderr)
        return 1
    print("VALID: extension package passed author-side checks")
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
