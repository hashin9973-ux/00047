#!/usr/bin/env python3
"""Create a minimal 00047 native-mod project without overwriting existing files."""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from validate_native_mod import ALLOWED_TARGETS, ID_PATTERN, ValidationError

CANONICAL_TARGETS = {
    "explorer.exe": "explorer.exe",
    "startmenuexperiencehost.exe": "StartMenuExperienceHost.exe",
    "shellexperiencehost.exe": "ShellExperienceHost.exe",
    "searchhost.exe": "SearchHost.exe",
}


def create_project(mod_id: str, name: str, output_dir: str, target: str = "explorer.exe",
                   description: str = "A starter 00047 native mod.") -> Path:
    if not ID_PATTERN.fullmatch(mod_id) or mod_id.startswith(".") or mod_id.endswith(".") or ".." in mod_id:
        raise ValidationError("id must use lowercase a-z, digits, dots, dashes or underscores and cannot begin/end with a dot")
    if not isinstance(name, str) or not name.strip() or len(name) > 100:
        raise ValidationError("name is required and must be no longer than 100 characters")
    if not isinstance(description, str) or len(description) > 1000:
        raise ValidationError("description must be text no longer than 1000 characters")
    canonical_target = CANONICAL_TARGETS.get(target.casefold()) if isinstance(target, str) else None
    if canonical_target is None:
        raise ValidationError("target is not in the supported native-mod process allow-list")

    root = Path(output_dir)
    destination = root / mod_id
    if destination.exists() or destination.is_symlink():
        raise ValidationError(f"destination already exists; refusing to overwrite: {destination}")

    module_name = re.sub(r"[^A-Za-z0-9_]", "_", mod_id)
    if not module_name or module_name[0].isdigit():
        module_name = "mod_" + module_name
    manifest = {
        "manifestVersion": 1,
        "id": mod_id,
        "name": name.strip(),
        "version": "1.0.0",
        "description": description,
        "architecture": "x64",
        "module": f"bin/{module_name}.dll",
        "targets": [canonical_target],
    }
    files = {
        "manifest.json": json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
        "mod.cpp": r'''#include "00047_mod_api.h"
#include "00047_mod_services.h"

WMOD_00047_EXPORT std::uint32_t WINAPI WmodGetApiVersion(void) {
    return WMOD_00047_ABI_VERSION;
}

WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void) {
    wmod::Log(L"info", L"Starter mod initialized");
    return TRUE;
}

WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void) {
    wmod::Log(L"info", L"Starter mod shutting down");
}
''',
        "CMakeLists.txt": f'''cmake_minimum_required(VERSION 3.21)
project({module_name} LANGUAGES CXX)

set(WMOD_SDK_DIR "" CACHE PATH "Path to the 00047 repository sdk/native-mod directory")
if(NOT EXISTS "${{WMOD_SDK_DIR}}/00047_mod_api.h")
  message(FATAL_ERROR "Set WMOD_SDK_DIR to the 00047 sdk/native-mod directory")
endif()

add_library({module_name} SHARED mod.cpp)
target_compile_features({module_name} PRIVATE cxx_std_17)
target_compile_definitions({module_name} PRIVATE WMOD_BUILD_DLL WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories({module_name} PRIVATE "${{WMOD_SDK_DIR}}")
set_target_properties({module_name} PROPERTIES
  PREFIX ""
  OUTPUT_NAME "{module_name}"
  RUNTIME_OUTPUT_DIRECTORY "${{CMAKE_BINARY_DIR}}/bin"
)
''',
        "README.md": f'''# {name.strip()}

Starter project for the 00047 native-mod SDK. The generated DLL only writes log messages; it does not alter the target process.

## Build (Windows x64)

From this directory, run CMake with a Visual Studio x64 toolchain. Replace C:/path/to/00047/sdk/native-mod with the SDK directory:

    cmake -S . -B build -A x64 -DWMOD_SDK_DIR=C:/path/to/00047/sdk/native-mod
    cmake --build build --config Release

The DLL is normally produced at build/bin/Release/{module_name}.dll.

## Package and validate

    python C:/path/to/00047/tools/package_native_mod.py --manifest manifest.json --dll build/bin/Release/{module_name}.dll --output dist/{module_name}.zip
    python C:/path/to/00047/tools/validate_native_mod.py dist/{module_name}.zip

The first command builds a deterministic ZIP and validates it before publishing the package. The second command independently validates it.

## Native code warning

Native mods execute inside their target process without a sandbox. This template does not contain a hook engine or Windhawk API compatibility layer. Keep WmodShutdownV1 responsible for releasing resources owned by your mod, test in a disposable Windows environment, and enable only code you trust.
''',
    }

    root.mkdir(parents=True, exist_ok=True)
    destination.mkdir()
    try:
        for relative, contents in files.items():
            (destination / relative).write_text(contents, encoding="utf-8", newline="\n")
    except Exception:
        import shutil
        shutil.rmtree(destination, ignore_errors=True)
        raise
    return destination


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--id", required=True, dest="mod_id", help="lowercase reverse-domain-style mod ID")
    parser.add_argument("--name", required=True, help="human-readable mod name")
    parser.add_argument("--output-dir", required=True, help="parent directory for the new project")
    parser.add_argument("--target", default="explorer.exe", choices=sorted(CANONICAL_TARGETS),
                        help="allow-listed Windows shell process (default: explorer.exe)")
    parser.add_argument("--description", default="A starter 00047 native mod.")
    args = parser.parse_args(argv)
    try:
        path = create_project(args.mod_id, args.name, args.output_dir, args.target, args.description)
    except (ValidationError, OSError) as exc:
        print(f"SCAFFOLD FAILED: {exc}", file=sys.stderr)
        return 1
    print(f"PROJECT CREATED: {path}")
    print("Read README.md in the generated directory before building or enabling the native DLL.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
