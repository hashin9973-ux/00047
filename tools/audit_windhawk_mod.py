#!/usr/bin/env python3
"""Audit a Windhawk C/C++ mod for 00047 source-level compatibility.

This is a static source review aid. It does not compile, sandbox, or certify a
mod. Unknown APIs are reported conservatively; unsupported runtime APIs must be
ported or run under a compatible host.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

SOURCE_SUFFIXES = {".c", ".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp", ".hxx"}
IGNORED_PARTS = {
    ".git", ".vs", "build", "dist", "node_modules", "out", "target", "vendor",
}
IGNORED_FILENAMES = {
    "mods_api.h", "mods_api_internal.h", "windhawk_nonhook_compat.h", "windhawk_symbol_compat.h", "windhawk_hook_compat.h",
}

SUPPORTED = {
    "Wh_Log": "Wide-string printf-style logging routed to the 00047 mod log.",
    "Wh_GetIntSetting": "Settings JSON/defaults; format-style setting-path arguments supported.",
    "Wh_GetStringSetting": "Settings JSON/defaults; returned text must be freed with Wh_FreeStringSetting.",
    "Wh_FreeStringSetting": "Frees strings returned by the 00047 Wh_GetStringSetting helper.",
    "Wh_GetIntValue": "Package-scoped INI storage.",
    "Wh_SetIntValue": "Package-scoped INI storage.",
    "Wh_GetStringValue": "Package-scoped INI storage; fails without partial copies if the buffer is too small.",
    "Wh_SetStringValue": "Package-scoped INI storage.",
    "Wh_GetBinaryValue": "Package-scoped storage; bounded binary value, no partial copy on undersized buffer.",
    "Wh_SetBinaryValue": "Package-scoped storage; bounded binary value.",
    "Wh_DeleteValue": "Deletes a value from package-scoped storage.",
    "Wh_GetModStoragePath": "Returns the package-scoped storage directory, not Windhawk's own data directory.",
    "Wh_SetDynamicSelectOption": "Supported for manifest-declared dynamicSelect settings; option storage is package-scoped.",
    "Wh_DeleteDynamicSelectOption": "Supported for manifest-declared dynamicSelect settings; option storage is package-scoped.",
    "Wh_ClearDynamicSelectOptions": "Supported for manifest-declared dynamicSelect settings; option storage is package-scoped.",
    "Wh_GetUrlContent": "Bounded HTTP(S)-only URL retrieval; semantics and options are not guaranteed ABI-identical.",
    "Wh_FreeUrlContent": "Frees content returned by the 00047 Wh_GetUrlContent helper.",
    "Wh_FindFirstSymbol": "Bounded DbgHelp enumeration of symbols for a loaded module; PDB/network behavior is limited and must be reviewed.",
    "Wh_FindNextSymbol": "Iterates a bounded snapshot returned by Wh_FindFirstSymbol; result pointers are owned by the search handle.",
    "Wh_FindCloseSymbol": "Releases a symbol search snapshot returned by Wh_FindFirstSymbol.",
    "Wh_HookSymbols": "Checks a bounded local offset cache, then the HTTPS online symbol cache when configured/defaulted, before bounded DbgHelp enumeration; validates image identity and executable offsets, then queues matching x64 hooks. Windhawk cache registry/error throttling, hybrid-module support, and full hook-session behavior are not implemented.",
    "Wh_SetFunctionHook": "Queues an x64 inline detour through bundled MinHook; a named per-process registry atomically rejects duplicate or conservatively overlapping entry-target regions across separately linked mod DLLs.",
    "Wh_RemoveFunctionHook": "Queues removal of a hook registered by this mod DLL and releases the shared target claim only after backend removal succeeds.",
    "Wh_ApplyHookOperations": "Applies this mod DLL's queued hook additions and removals; rollback is best-effort if a Windows patch operation fails.",
    "Wh_Disasm": "Uses a vendored x64 decoder to return Intel-syntax mnemonics and operands; formatting can differ from Windhawk's Zydis-based output.",
}
UNSUPPORTED = {}
CALLBACK_PREFIX = "Wh_Mod"
IDENTIFIER = re.compile(r"\bWh_[A-Za-z0-9_]+\b")
ADAPTER_CALLBACKS = {
    "Wh_ModInit",
    "Wh_ModAfterInit",
    "Wh_ModBeforeUninit",
    "Wh_ModUninit",
}
MANUAL_CALLBACKS = set()


def mask_cpp_noncode(source: str) -> str:
    """Replace comments and literal contents with spaces, preserving offsets/newlines."""
    chars = list(source)
    n = len(source)
    i = 0

    def blank(start: int, end: int) -> None:
        for j in range(start, end):
            if chars[j] not in "\r\n":
                chars[j] = " "

    while i < n:
        if source.startswith("//", i):
            end = source.find("\n", i + 2)
            if end < 0:
                end = n
            blank(i, end)
            i = end
            continue
        if source.startswith("/*", i):
            close = source.find("*/", i + 2)
            end = n if close < 0 else close + 2
            blank(i, end)
            i = end
            continue

        # C++ raw literals: R"delimiter(contents)delimiter", including u8R prefixes.
        raw_start = i
        if source.startswith('R"', i):
            quote_pos = i + 1
        elif source.startswith('u8R"', i):
            quote_pos = i + 3
        elif i + 2 < n and source[i] in "uUL" and source.startswith('R"', i + 1):
            quote_pos = i + 2
        else:
            quote_pos = -1
        if quote_pos >= 0:
            open_paren = source.find("(", quote_pos + 1)
            if open_paren >= 0 and open_paren - quote_pos <= 18:
                delimiter = source[quote_pos + 1:open_paren]
                close_token = ")" + delimiter + '"'
                close = source.find(close_token, open_paren + 1)
                end = n if close < 0 else close + len(close_token)
                blank(raw_start, end)
                i = end
                continue

        if source[i] in ('"', "'") or (
            source[i] in "uUL" and i + 1 < n and source[i + 1] in ('"', "'")
        ) or (
            source.startswith("u8", i) and i + 2 < n and source[i + 2] in ('"', "'")
        ):
            start = i
            if source.startswith("u8", i) and i + 2 < n and source[i + 2] in ('"', "'"):
                quote = source[i + 2]
                i += 3
            elif source[i] in "uUL" and i + 1 < n and source[i + 1] in ('"', "'"):
                quote = source[i + 1]
                i += 2
            else:
                quote = source[i]
                i += 1
            while i < n:
                if source[i] == "\\":
                    i += 2
                    continue
                if source[i] == quote:
                    i += 1
                    break
                if source[i] in "\r\n":
                    # Malformed literal: recover at line boundary.
                    break
                i += 1
            blank(start, min(i, n))
            continue
        i += 1
    return "".join(chars)


def collect_sources(inputs: list[Path]) -> list[Path]:
    found: set[Path] = set()
    for item in inputs:
        if not item.exists():
            raise ValueError(f"path does not exist: {item}")
        if item.is_file():
            if item.suffix.lower() in SOURCE_SUFFIXES and item.name.lower() not in IGNORED_FILENAMES:
                found.add(item.resolve())
            continue
        for path in item.rglob("*"):
            if not path.is_file() or path.is_symlink():
                continue
            relative_parts = path.relative_to(item).parts
            if any(part.lower() in IGNORED_PARTS for part in relative_parts):
                continue
            if path.name.lower() in IGNORED_FILENAMES or path.suffix.lower() not in SOURCE_SUFFIXES:
                continue
            found.add(path.resolve())
    return sorted(found)


def audit_text(path: str, source: str) -> list[dict]:
    code = mask_cpp_noncode(source)
    findings: dict[str, dict] = {}
    for match in IDENTIFIER.finditer(code):
        name = match.group(0)
        callback_context = code[max(0, match.start() - 120):min(len(code), match.end() + 120)]
        settings_reload_variant = (
            name == "Wh_ModSettingsChanged"
            and re.search(
                r"\b(?:BOOL|bool)\s+Wh_ModSettingsChanged\s*\(\s*(?:BOOL|bool)\s*\*",
                callback_context,
            )
        )
        if settings_reload_variant:
            category = "adapter-available-lifecycle"
            reason = (
                "The opt-in settings adapter supports BOOL Wh_ModSettingsChanged(BOOL* bReload). "
                "A successful callback setting *bReload to TRUE requests a safe unload/reload on "
                "the next enabled-mod scan; callback thread affinity differs from Windhawk."
            )
        elif name in SUPPORTED:
            category, reason = "supported-with-limitations", SUPPORTED[name]
        elif name in UNSUPPORTED:
            category, reason = "unsupported", UNSUPPORTED[name]
        elif name == "Wh_ModSettingsChanged":
            category = "adapter-available-lifecycle"
            reason = "Only void Wh_ModSettingsChanged(void) is supported by the opt-in watcher; it runs on a worker thread, not a Windhawk/UI thread."
        elif name in ADAPTER_CALLBACKS:
            category = "adapter-available-lifecycle"
            reason = "The opt-in windhawk_nonhook_adapter.h maps this lifecycle callback onto the Wmod ABI; callback ordering must be reviewed."
        elif name in MANUAL_CALLBACKS or name.startswith(CALLBACK_PREFIX):
            category = "manual-lifecycle-adaptation"
            reason = "This callback is not delivered automatically by 00047; settings-change/runtime semantics require manual adaptation."
        else:
            category = "unknown-api"
            reason = "Not in the audit tool's supported API matrix; inspect and port manually."
        item = findings.setdefault(name, {
            "name": name,
            "category": category,
            "reason": reason,
            "locations": [],
        })
        line = code.count("\n", 0, match.start()) + 1
        item["locations"].append({"file": path, "line": line})
    return sorted(findings.values(), key=lambda entry: (entry["category"], entry["name"]))


def audit_paths(inputs: list[Path]) -> dict:
    files = collect_sources(inputs)
    findings = []
    for path in files:
        try:
            if path.stat().st_size > 2 * 1024 * 1024:
                findings.append({
                    "name": "<skipped-file>",
                    "category": "warning",
                    "reason": "Source file exceeds 2 MiB audit limit.",
                    "locations": [{"file": str(path), "line": 1}],
                })
                continue
            source = path.read_text(encoding="utf-8-sig", errors="replace")
        except OSError as exc:
            findings.append({
                "name": "<unreadable-file>",
                "category": "warning",
                "reason": str(exc),
                "locations": [{"file": str(path), "line": 1}],
            })
            continue
        findings.extend(audit_text(str(path), source))
    # Merge findings across files into one entry per API.
    merged: dict[tuple[str, str], dict] = {}
    for item in findings:
        key = (item["name"], item["category"])
        if key not in merged:
            merged[key] = {**item, "locations": list(item["locations"])}
        else:
            merged[key]["locations"].extend(item["locations"])
    final = sorted(merged.values(), key=lambda entry: (entry["category"], entry["name"]))
    counts: dict[str, int] = {}
    for item in final:
        counts[item["category"]] = counts.get(item["category"], 0) + 1
    return {
        "files_scanned": len(files),
        "findings": final,
        "summary": counts,
        "note": (
            "Static audit only. Passing does not prove ABI compatibility or safety. "
            "Unsupported function-hook and disassembly APIs, plus lifecycle callbacks "
            "without an automatic adapter require manual porting or a compatible runtime."
        ),
    }




def _sarif_artifact_uri(raw_path: str) -> str:
    """Use repository-relative URIs for in-tree files and file URIs otherwise."""
    path = Path(raw_path)
    try:
        resolved = path.resolve()
        try:
            return resolved.relative_to(Path.cwd().resolve()).as_posix()
        except ValueError:
            return resolved.as_uri()
    except (OSError, ValueError):
        # Keep malformed or platform-specific user input diagnosable without
        # allowing backslashes to be interpreted as URI escape sequences.
        return raw_path.replace("\\", "/")

def to_sarif(report: dict) -> dict:
    """Convert an audit report into SARIF 2.1.0 for code-scanning integrations."""
    categories = sorted({item["category"] for item in report["findings"]})
    rules = []
    rule_ids = {}
    for category in categories:
        rule_id = "windhawk-api-" + re.sub(r"[^a-z0-9-]+", "-", category.lower()).strip("-")
        rule_ids[category] = rule_id
        rules.append({
            "id": rule_id,
            "shortDescription": {"text": category.replace("-", " ").title()},
            "help": {"text": (
                "Static compatibility finding from audit_windhawk_mod.py. "
                "Review the API and its documented limitations before porting."
            )},
        })

    results = []
    for item in report["findings"]:
        level = "error" if item["category"] == "unsupported" else (
            "warning" if item["category"] in {
                "unknown-api", "manual-lifecycle-adaptation", "warning"
            } else "note"
        )
        locations = []
        for location in item["locations"]:
            path = _sarif_artifact_uri(str(location["file"]))
            locations.append({
                "physicalLocation": {
                    "artifactLocation": {"uri": path},
                    "region": {"startLine": max(1, int(location["line"]))},
                }
            })
        results.append({
            "ruleId": rule_ids[item["category"]],
            "level": level,
            "message": {"text": f'{item["name"]}: {item["reason"]}'},
            "locations": locations,
        })

    return {
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "00047 Windhawk Compatibility Audit",
                    "informationUri": "https://github.com/hashin9973-ux/00047",
                    "rules": rules,
                }
            },
            "results": results,
            "properties": {
                "filesScanned": report["files_scanned"],
                "summary": report["summary"],
                "note": report["note"],
            },
        }],
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="+", type=Path, help="C/C++ source files or directories")
    output = parser.add_mutually_exclusive_group()
    output.add_argument("--json", action="store_true", help="emit a machine-readable JSON report")
    output.add_argument("--sarif", action="store_true", help="emit SARIF 2.1.0 for code-scanning integrations")
    parser.add_argument("--output", type=Path, help="write --json or --sarif output to this file instead of stdout")
    parser.add_argument(
        "--fail-on-unsupported",
        action="store_true",
        help="exit with code 2 when unsupported, unknown, or lifecycle APIs are found",
    )
    args = parser.parse_args(argv)
    try:
        report = audit_paths(args.paths)
    except ValueError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    if args.output and not (args.sarif or args.json):
        parser.error("--output requires either --json or --sarif")
    if args.sarif or args.json:
        payload = to_sarif(report) if args.sarif else report
        rendered = json.dumps(payload, indent=2)
        if args.output:
            try:
                args.output.write_text(rendered + chr(10), encoding="utf-8")
            except OSError as exc:
                print(f"ERROR: cannot write report: {exc}", file=sys.stderr)
                return 1
        else:
            print(rendered)
    else:
        print(f"Files scanned: {report['files_scanned']}")
        for item in report["findings"]:
            locations = ", ".join(f"{p['file']}:{p['line']}" for p in item["locations"])
            print(f"[{item['category']}] {item['name']} — {item['reason']}")
            print(f"  {locations}")
        print("Summary: " + (
            ", ".join(f"{key}={value}" for key, value in sorted(report["summary"].items()))
            if report["summary"] else "no Windhawk API identifiers found"
        ))
        print(report["note"])
    blockers = {"unsupported", "unknown-api", "manual-lifecycle-adaptation", "warning"}
    if args.fail_on_unsupported and any(item["category"] in blockers for item in report["findings"]):
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
