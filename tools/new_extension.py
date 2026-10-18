#!/usr/bin/env python3
"""Scaffold a minimal 00047 external command or service extension."""
from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

from validate_extension import ValidationError, valid_id

KINDS = {"command", "service"}


def create_project(
    extension_id: str,
    name: str,
    output_dir: str,
    kind: str = "command",
    description: str = "A starter 00047 external extension.",
) -> Path:
    if not valid_id(extension_id):
        raise ValidationError("id must use lowercase ASCII letters, digits, dots, dashes or underscores (max 100 bytes)")
    if not isinstance(name, str) or not name.strip() or len(name) > 100:
        raise ValidationError("name is required and must be no longer than 100 characters")
    if not isinstance(description, str) or len(description) > 1000:
        raise ValidationError("description must be text no longer than 1000 characters")
    if kind not in KINDS:
        raise ValidationError("kind must be 'command' or 'service'")

    output_root = Path(output_dir)
    output_root.mkdir(parents=True, exist_ok=True)
    destination = output_root / extension_id
    if destination.exists() or destination.is_symlink():
        raise ValidationError(f"destination already exists; refusing to overwrite: {destination}")

    component_id = "main"
    runtime = {
        "command": "python",
        "args": ["{entrypoint}"],
        "workingDirectory": "{extensionDir}",
    }
    if kind == "command":
        runtime["timeoutMs"] = 15_000
        script = '''#!/usr/bin/env python3
"""One-shot command started by 00047."""
from datetime import datetime

print("Extension command completed at", datetime.now().astimezone().isoformat())
'''
        usage = "Choose Run in the Extensions view. The command exits after printing a result."
    else:
        script = '''#!/usr/bin/env python3
"""Minimal long-running helper managed by 00047."""
import signal
import time

stopping = False


def request_stop(_signum, _frame):
    global stopping
    stopping = True


for signal_name in ("SIGINT", "SIGTERM"):
    signal_value = getattr(signal, signal_name, None)
    if signal_value is not None:
        try:
            signal.signal(signal_value, request_stop)
        except (OSError, ValueError):
            pass

print("Extension service started", flush=True)
while not stopping:
    time.sleep(1)
print("Extension service stopped", flush=True)
'''
        usage = "Choose Start to launch the helper, then Stop when it is no longer needed."

    manifest = {
        "manifestVersion": 1,
        "id": extension_id,
        "name": name.strip(),
        "version": "1.0.0",
        "description": description,
        "components": [
            {
                "id": component_id,
                "name": name.strip(),
                "kind": kind,
                "description": description,
                "entrypoint": "scripts/main.py",
                "runtime": runtime,
            }
        ],
    }
    files = {
        "manifest.json": json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
        "scripts/main.py": script,
        "README.md": (
            f"# {name.strip()}\n\n"
            f"{description}\n\n"
            f"{usage}\n\n"
            "## Build a package\n\n"
            "From the 00047 repository root, run:\n\n"
            f"    python tools/package_extension.py path/to/{extension_id} dist/{extension_id}.zip\n\n"
            "Review the source and manifest before installing. Extensions execute as your current user; "
            "the host is not a sandbox.\n"
        ),
    }

    staging = Path(tempfile.mkdtemp(prefix=f".{extension_id}.", dir=output_root))
    try:
        for relative, payload in files.items():
            target = staging / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(payload, encoding="utf-8", newline="\n")
        staging.rename(destination)
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return destination


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("id", help="lowercase package ID, e.g. com.example.status-check")
    parser.add_argument("name", help="display name")
    parser.add_argument("--out", default="extensions", help="output directory (default: extensions)")
    parser.add_argument("--kind", choices=sorted(KINDS), default="command")
    parser.add_argument("--description", default="A starter 00047 external extension.")
    args = parser.parse_args(argv)
    try:
        project = create_project(args.id, args.name, args.out, args.kind, args.description)
    except (ValidationError, OSError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    print(f"Created extension project: {project}")
    print(f"Next: python tools/package_extension.py {project} {project.name}.zip")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
