#!/usr/bin/env python3
"""Create a reproducible, install-ready ZIP from an external 00047 extension project."""
from __future__ import annotations

import argparse
import os
import sys
import tempfile
import zipfile
from pathlib import Path

from validate_extension import ValidationError, _archive_path, validate_package


EXCLUDED_DIRS = {".git", ".hg", ".svn", "__pycache__"}
EXCLUDED_SUFFIXES = {".pyc", ".pyo"}


def _project_files(root: Path, output: Path) -> list[tuple[str, Path]]:
    if root.is_symlink() or not root.is_dir():
        raise ValidationError(f"project path is not a regular directory: {root}")
    root = root.resolve()
    output_resolved = output.resolve()
    try:
        output_resolved.relative_to(root)
    except ValueError:
        pass
    else:
        raise ValidationError("output ZIP must be outside the project directory")

    found: list[tuple[str, Path]] = []
    for directory, dirnames, filenames in os.walk(root, followlinks=False):
        current = Path(directory)
        kept_dirs = []
        for dirname in sorted(dirnames):
            path = current / dirname
            if dirname in EXCLUDED_DIRS:
                continue
            if path.is_symlink():
                raise ValidationError(f"project contains a symbolic-link directory: {path}")
            kept_dirs.append(dirname)
        dirnames[:] = kept_dirs
        for filename in filenames:
            path = current / filename
            if path.is_symlink():
                raise ValidationError(f"project contains a symbolic-link file: {path}")
            if path.suffix.casefold() in EXCLUDED_SUFFIXES:
                continue
            if not path.is_file():
                raise ValidationError(f"project entry is not a regular file: {path}")
            relative = path.relative_to(root).as_posix()
            _archive_path(relative, False)
            found.append((relative, path))
    found.sort(key=lambda item: item[0])
    return found


def build_package(project_dir: str, output_path: str) -> dict:
    root = Path(project_dir)
    output = Path(output_path)
    if output.suffix.casefold() != ".zip":
        raise ValidationError("output filename must end in .zip")
    if output.is_symlink():
        raise ValidationError("output ZIP must not be a symbolic link")
    files = _project_files(root, output)
    if not any(name == "manifest.json" for name, _ in files):
        raise ValidationError("project directory must contain a root manifest.json")

    output.parent.mkdir(parents=True, exist_ok=True)
    fd, staging_name = tempfile.mkstemp(
        prefix=f".{output.stem}.", suffix=".zip", dir=str(output.parent)
    )
    os.close(fd)
    staging = Path(staging_name)
    try:
        with zipfile.ZipFile(
            staging,
            "w",
            compression=zipfile.ZIP_DEFLATED,
            compresslevel=9,
        ) as archive:
            for internal_name, source in files:
                info = zipfile.ZipInfo(internal_name, date_time=(1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.create_system = 3
                info.external_attr = (0o100644 & 0xFFFF) << 16
                info.flag_bits = 0
                archive.writestr(info, source.read_bytes(), compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
        report = validate_package(str(staging))
        os.replace(staging, output)
        return report
    except Exception:
        try:
            staging.unlink()
        except FileNotFoundError:
            pass
        raise


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("project", help="extension project directory containing manifest.json")
    parser.add_argument("output", help="destination ZIP, outside the project directory")
    args = parser.parse_args(argv)
    try:
        report = build_package(args.project, args.output)
    except (ValidationError, OSError, zipfile.BadZipFile) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    print(f"Created validated extension package: {args.output}")
    print(f"ID: {report['id']} | components: {report['components']} | entries: {report['entries']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
