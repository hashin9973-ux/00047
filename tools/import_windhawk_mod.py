#!/usr/bin/env python3
"""Import a single-file Windhawk C++ mod into a buildable 00047 project."""
from __future__ import annotations

import argparse
import ast
import json
import re
import shlex
import shutil
import sys
from pathlib import Path

from audit_windhawk_mod import audit_text, mask_cpp_noncode
from new_native_mod import CANONICAL_TARGETS
from validate_native_mod import ID_PATTERN

META_START = "// ==WindhawkMod=="
META_END = "// ==/WindhawkMod=="
SETTINGS_START = "// ==WindhawkModSettings=="
SETTINGS_END = "// ==/WindhawkModSettings=="
README_START = "// ==WindhawkModReadme=="
README_END = "// ==/WindhawkModReadme=="
MAX_FILE_BYTES = 2 * 1024 * 1024
MAX_TREE_BYTES = 20 * 1024 * 1024
MAX_TREE_FILES = 200


class ImportError(ValueError):
    """A source feature cannot be represented safely by this importer."""


def extract_block(source: str, begin: str, end: str, required: bool = False) -> str:
    start = source.find(begin)
    if start < 0:
        if required:
            raise ImportError(f"source is missing {begin}")
        return ""
    if source.find(begin, start + len(begin)) >= 0:
        raise ImportError(f"source contains multiple {begin} blocks")
    finish = source.find(end, start + len(begin))
    if finish < 0:
        raise ImportError(f"source is missing matching {end}")
    return source[start + len(begin):finish]


def parse_metadata(source: str) -> dict[str, list[str]]:
    fields: dict[str, list[str]] = {}
    for line in extract_block(source, META_START, META_END, required=True).splitlines():
        match = re.match(r"^\s*//\s*@([A-Za-z][A-Za-z0-9_-]*(?::[A-Za-z0-9-]+)?)\s*(.*?)\s*$", line)
        if match:
            raw_key, value = match.groups()
            fields.setdefault(raw_key.split(":", 1)[0].casefold(), []).append(value.strip())
    return fields


def get_value(fields: dict[str, list[str]], key: str, required: bool = False) -> str:
    values = fields.get(key, [])
    if values and values[0]:
        return values[0]
    if required:
        raise ImportError(f"Windhawk metadata needs a non-empty @{key}")
    return ""


def strip_yaml_comment(value: str) -> str:
    quote, escaped = None, False
    for i, char in enumerate(value):
        if escaped:
            escaped = False
            continue
        if quote == '"' and char == "\\":
            escaped = True
        elif quote:
            if char == quote:
                quote = None
        elif char in ("'", '"'):
            quote = char
        elif char == "#" and (i == 0 or value[i - 1].isspace()):
            return value[:i].rstrip()
    return value.strip()


def split_array(value: str) -> list[str]:
    body = value.strip()[1:-1]
    if not body.strip():
        return []
    items, start, quote, escaped = [], 0, None, False
    for i, char in enumerate(body):
        if escaped:
            escaped = False
        elif quote == '"' and char == "\\":
            escaped = True
        elif quote:
            if char == quote:
                quote = None
        elif char in ("'", '"'):
            quote = char
        elif char == ",":
            items.append(body[start:i].strip())
            start = i + 1
    items.append(body[start:].strip())
    return items


def parse_scalar(value: str):
    value = strip_yaml_comment(value).strip()
    if not value:
        raise ImportError("empty defaults and nested settings groups are not supported")
    if value.startswith("[") and value.endswith("]"):
        result = [parse_scalar(part) for part in split_array(value)]
        if not result:
            return result
        if all(type(item) is str for item in result):
            return result
        if all(type(item) is int for item in result):
            return result
        raise ImportError("arrays must contain only strings or only integers; mixed and floating-point arrays are unsupported")
    if value.startswith("{") or value.casefold() in ("null", "~"):
        raise ImportError("mapping and null setting defaults are not supported")
    if value.startswith('"'):
        try:
            result = json.loads(value)
        except json.JSONDecodeError as exc:
            raise ImportError(f"invalid quoted YAML string: {exc}") from exc
        if not isinstance(result, str):
            raise ImportError("quoted defaults must be strings")
        return result
    if value.startswith("'") and value.endswith("'"):
        try:
            result = ast.literal_eval(value)
        except (SyntaxError, ValueError) as exc:
            raise ImportError("invalid single-quoted YAML string") from exc
        if not isinstance(result, str):
            raise ImportError("quoted defaults must be strings")
        return result
    if value.casefold() == "true":
        return True
    if value.casefold() == "false":
        return False
    if re.fullmatch(r"-?(?:0|[1-9][0-9]*)", value):
        return int(value)
    if re.fullmatch(r"-?(?:[0-9]+\.[0-9]*|[0-9]*\.[0-9]+)(?:[eE][+-]?[0-9]+)?", value):
        return float(value)
    if value[0] in ("&", "*", "!", "|", ">"):
        raise ImportError(f"unsupported YAML scalar: {value!r}")
    return value


def split_top_level(text: str, separator: str) -> list[str]:
    parts, start, quote, escaped, stack = [], 0, None, False, []
    pairs = {"[": "]", "{": "}", "(": ")"}
    for index, char in enumerate(text):
        if escaped:
            escaped = False
            continue
        if quote == '"' and char == "\\":
            escaped = True
            continue
        if quote:
            if char == quote:
                quote = None
            continue
        if char in ("'", '"'):
            quote = char
            continue
        if char in pairs:
            stack.append(pairs[char])
            continue
        if stack and char == stack[-1]:
            stack.pop()
            continue
        if char == separator and not stack:
            parts.append(text[start:index].strip())
            start = index + 1
    parts.append(text[start:].strip())
    return parts


def condition_values(raw: str, setting_id: str, annotation: str) -> list[object]:
    raw = raw.strip()
    if raw.startswith("[") and raw.endswith("]"):
        items = split_top_level(raw[1:-1], ",")
        values = [parse_scalar(item) for item in items if item]
    else:
        values = [parse_scalar(raw)] if raw else []
    if not values or len(values) > 100:
        raise ImportError(f"setting {setting_id}: {annotation} conditions need 1 to 100 values")
    for value in values:
        if isinstance(value, float) or not isinstance(value, (str, int, bool)):
            raise ImportError(f"setting {setting_id}: {annotation} values must be strings, integers, or booleans")
    return values


def add_condition_entry(conditions: dict, pair: str, setting_id: str, annotation: str) -> None:
    pieces = split_top_level(pair, ":")
    if len(pieces) < 2 or not pieces[0] or not ":".join(pieces[1:]).strip():
        raise ImportError(f"setting {setting_id}: malformed {annotation} condition {pair!r}")
    key = parse_scalar(pieces[0])
    if not isinstance(key, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.-]{0,127}", key):
        raise ImportError(f"setting {setting_id}: invalid {annotation} condition key")
    if key in conditions:
        raise ImportError(f"setting {setting_id}: duplicate {annotation} condition for {key!r}")
    conditions[key] = condition_values(":".join(pieces[1:]), setting_id, annotation)


def parse_condition_map(raw: str, setting_id: str, annotation: str) -> dict[str, list[object]]:
    raw = raw.strip()
    if not (raw.startswith("{") and raw.endswith("}")):
        raise ImportError(f"setting {setting_id}: {annotation} must be a flat map such as {{Enabled: true}}")
    conditions: dict[str, list[object]] = {}
    for pair in split_top_level(raw[1:-1], ","):
        if pair:
            add_condition_entry(conditions, pair, setting_id, annotation)
    if not conditions:
        raise ImportError(f"setting {setting_id}: {annotation} map cannot be empty")
    return conditions


def clean_settings_lines(block: str) -> list[tuple[int, str]]:
    body = block.strip()
    if body.startswith("/*"):
        body = body[2:]
    body = body.rstrip()
    if body.endswith("*/"):
        body = body[:-2]
    output = []
    for raw in body.splitlines():
        line = raw.rstrip()
        indent = len(line) - len(line.lstrip())
        text = line[indent:]
        if text.startswith("*"):
            text = text[1:]
            spaces = len(text) - len(text.lstrip())
            indent += max(0, spaces - 1)
            text = text.lstrip()
        if text.startswith("//"):
            text = text[2:]
            spaces = len(text) - len(text.lstrip())
            indent += max(0, spaces - 1)
            text = text.lstrip()
        if text.startswith("#!"):
            if not text.startswith("#! "):
                raise ImportError("settings annotations must use '#! ' with exactly one space")
            text = text[3:]
        elif text.startswith("#") or not text.strip():
            continue
        output.append((indent, text.strip()))
    return output


def flatten_nested_setting_groups(
    lines: list[tuple[int, str]], root_indent: int
) -> tuple[list[tuple[int, str]], dict[str, dict]]:
    """Flatten nested mapping groups into dotted IDs supported by 00047.

    Object arrays (YAML sequence rows whose first nested item is itself a
    sequence) are intentionally rejected because they need per-row templates.
    """
    group_metadata: dict[str, dict] = {}
    flattened: list[tuple[int, str]] = []

    def parse_group_metadata(group_path: str, fallback_name: str,
                             properties: list[tuple[int, str]]) -> dict:
        info = {"name": fallback_name, "showIf": {}, "hideIf": {}}
        for _, text in properties:
            match = re.match(r"^(\$[A-Za-z][A-Za-z0-9_-]*(?::[A-Za-z0-9-]+)?)\s*:\s*(.*?)\s*$", text)
            if not match:
                raise ImportError(f"unsupported metadata on setting group {group_path}: {text!r}")
            name, value = match.groups()
            if ":" in name:
                continue
            name = name.casefold()
            if name in ("$name", "$description"):
                parsed = parse_scalar(value)
                if not isinstance(parsed, str):
                    raise ImportError(f"setting group {group_path}: {name} must be a string")
                if name == "$name":
                    info["name"] = parsed
            elif name in ("$showif", "$hideif"):
                if not value:
                    raise ImportError(
                        f"setting group {group_path}: multiline {name} maps must be converted to inline maps before import"
                    )
                info["showIf" if name == "$showif" else "hideIf"] = parse_condition_map(value, group_path, name)
            else:
                raise ImportError(
                    f"setting group {group_path}: {name} is not supported on a group; move it to a child setting"
                )
        return info

    def process_level(source: list[tuple[int, str]], level: int, prefix: str) -> None:
        index = 0
        while index < len(source):
            indent, text = source[index]
            if indent != level or not text.startswith("- "):
                index += 1
                continue
            match = re.match(r"^([A-Za-z_][A-Za-z0-9_.-]{0,127})\s*:\s*(.*?)\s*$", text[2:].strip())
            if not match:
                raise ImportError(f"unsupported setting entry: {text!r}")
            key, default = match.groups()
            item_end = index + 1
            while item_end < len(source):
                next_indent, next_text = source[item_end]
                if next_text.startswith("- ") and next_indent <= level:
                    break
                item_end += 1
            body = source[index + 1:item_end]
            nested_dash_indents = [
                nested_indent for nested_indent, nested_text in body
                if nested_text.startswith("- ") and nested_indent > level
            ]
            object_array_row = any(
                nested_text.startswith("- -")
                for nested_indent, nested_text in body
                if nested_text.startswith("- ") and nested_indent > level
            )
            if object_array_row:
                raise ImportError(
                    "object-array setting templates are not supported by the current importer; convert them to a flat setting schema"
                )
            path = f"{prefix}.{key}" if prefix else key
            if not default and nested_dash_indents:
                child_indent = min(nested_dash_indents)
                group_properties = [
                    (body_index + index + 1, line_text)
                    for body_index, (line_indent, line_text) in enumerate(body)
                    if line_indent == child_indent and line_text.startswith("$")
                ]
                child_lines = [
                    (line_indent, line_text)
                    for line_indent, line_text in body
                    if not (line_indent == child_indent and line_text.startswith("$"))
                ]
                metadata = parse_group_metadata(path, key, group_properties)
                group_metadata[path] = metadata
                process_level(child_lines, child_indent, path)
            else:
                if path.count(".") > 8:
                    raise ImportError(f"setting nesting depth exceeds 8 at {path}")
                if len(path) > 64:
                    raise ImportError(f"flattened setting ID exceeds 64 characters: {path}")
                flattened.append((root_indent, f"- {path}: {default}"))
                for line_indent, line_text in body:
                    if line_indent <= level:
                        continue
                    flattened.append((root_indent + line_indent - level, line_text))
            index = item_end

    first = next((indent for indent, text in lines if text.startswith("- ")), None)
    if first is None:
        return lines, group_metadata
    if first != root_indent:
        raise ImportError("settings list has inconsistent root indentation")
    process_level(lines, root_indent, "")
    return flattened, group_metadata


def parse_settings(source: str) -> tuple[list[dict], list[str], str]:
    raw = extract_block(source, SETTINGS_START, SETTINGS_END)
    if not raw.strip():
        return [], [], ""
    lines = clean_settings_lines(raw)
    first_item = next(((indent, text) for indent, text in lines if text.startswith("- ")), None)
    if first_item is None:
        raise ImportError("settings block does not contain a YAML list of defaults")
    root_indent = first_item[0]
    lines, group_metadata = flatten_nested_setting_groups(lines, root_indent)
    entries, warnings, current, current_property, current_property_indent = [], [], None, None, None
    for indent, text in lines:
        if text.startswith("- "):
            if current is None or indent == root_indent:
                match = re.match(r"^([A-Za-z_][A-Za-z0-9_.-]{0,127})\s*:\s*(.*?)\s*$", text[2:].strip())
                if not match:
                    raise ImportError(f"unsupported setting entry: {text!r}")
                key, default = match.groups()
                if any(item["id"] == key for item in entries):
                    raise ImportError(f"duplicate setting name: {key}")
                current = {"id": key, "_raw": default, "_props": {}, "_options": []}
                entries.append(current)
                current_property = None
                current_property_indent = None
            elif current_property == "$options" and indent > root_indent:
                match = re.match(r"^([^:]+?)\s*:\s*(.*?)\s*$", text[2:].strip())
                if not match:
                    raise ImportError(f"unsupported static option: {text!r}")
                value, label = parse_scalar(match.group(1)), parse_scalar(match.group(2))
                if not isinstance(value, str) or not isinstance(label, str):
                    raise ImportError("static option values and labels must be strings")
                if any(item["value"] == value for item in current["_options"]):
                    raise ImportError(f"duplicate option value {value!r} for {current['id']}")
                current["_options"].append({"value": value, "label": label})
            else:
                raise ImportError("nested setting groups and arrays of nested settings are not supported yet")
            continue
        if current is None:
            raise ImportError(f"unexpected settings content: {text!r}")
        if current_property in ("$showif", "$hideif") and current_property_indent is not None and indent > current_property_indent:
            add_condition_entry(current["_props"][current_property], text, current["id"], current_property)
            continue
        match = re.match(r"^(\$[A-Za-z][A-Za-z0-9_-]*(?::[A-Za-z0-9-]+)?)\s*:\s*(.*?)\s*$", text)
        if not match:
            raise ImportError(f"unsupported settings YAML: {text!r}")
        name, value = match.groups()
        if ":" in name:
            continue  # Locale-specific labels remain in the original source/import record.
        name = name.casefold()
        current_property = name
        current_property_indent = indent
        if name == "$options":
            if value:
                raise ImportError("inline $options maps are not supported; use a YAML list")
            current["_options"] = []
        elif name in ("$name", "$description"):
            parsed = parse_scalar(value)
            if not isinstance(parsed, str):
                raise ImportError(f"{name} must be a string")
            current["_props"][name] = parsed
        elif name in ("$float", "$dynamicselect"):
            if value.casefold() not in ("true", "false"):
                raise ImportError(f"{name} must be true or false")
            current["_props"][name] = value.casefold() == "true"
        elif name in ("$min", "$max"):
            parsed = parse_scalar(value)
            if isinstance(parsed, bool) or not isinstance(parsed, (int, float)):
                raise ImportError(f"{name} must be numeric")
            current["_props"][name] = parsed
        elif name in ("$showif", "$hideif"):
            current["_props"][name] = parse_condition_map(value, current["id"], name) if value else {}
        elif name == "$format":
            parsed = parse_scalar(value)
            if not isinstance(parsed, str) or not parsed:
                raise ImportError(f"setting {current['id']}: $format must be a non-empty string")
            current["_props"][name] = parsed
        else:
            raise ImportError(f"unsupported settings annotation: {name}")

    converted = []
    for entry in entries:
        default, props, options = parse_scalar(entry["_raw"]), entry["_props"], entry["_options"]
        dynamic, floating = props.get("$dynamicselect", False), props.get("$float", False)
        format_hint = props.get("$format")
        if isinstance(default, bool):
            kind = "boolean"
        elif isinstance(default, int):
            kind = "integer"
        elif isinstance(default, float):
            if not floating:
                raise ImportError(f"setting {entry['id']}: decimal defaults require $float: true")
            kind, default = "string", str(default)
        elif isinstance(default, list):
            if not default or all(type(value) is str for value in default):
                kind = "stringArray"
            elif all(type(value) is int for value in default):
                if any(value < -(2**31) or value > 2**31 - 1 for value in default):
                    raise ImportError(f"setting {entry['id']}: numeric array values must be signed 32-bit integers")
                kind = "numberArray"
            else:
                raise ImportError(f"setting {entry['id']}: arrays must contain only strings or only integers")
        elif isinstance(default, str):
            kind = "string"
        else:
            raise ImportError(f"setting {entry['id']}: unsupported default type")

        if floating:
            if isinstance(default, bool) or kind not in ("integer", "string"):
                raise ImportError(f"setting {entry['id']}: $float requires a scalar numeric/string default")
            if isinstance(default, int):
                default = str(default)
            kind = "string"
            if ("$min" in props or "$max" in props) and format_hint != "slider":
                warnings.append(f"setting {entry['id']}: floating-point range limits are not represented in the 00047 text field")
        if options and kind == "string" and not dynamic:
            kind = "select"
        if kind == "numberArray" and (options or dynamic or floating or format_hint or "$min" in props or "$max" in props):
            raise ImportError(f"setting {entry['id']}: numeric arrays do not support options, $min/$max, $float, $dynamicSelect, or $format")
        item = {"id": entry["id"], "label": props.get("$name", entry["id"]), "type": kind, "default": default}
        if format_hint:
            item["format"] = format_hint
        if "$description" in props:
            item["description"] = props["$description"]
        if props.get("$showif"):
            item["showIf"] = props["$showif"]
        if props.get("$hideif"):
            item["hideIf"] = props["$hideif"]
        if options:
            item["options"] = options
        if dynamic:
            if kind not in ("string", "select", "stringArray"):
                raise ImportError(f"setting {entry['id']}: $dynamicSelect requires strings")
            item["dynamicSelect"] = True
        if kind in ("integer", "number") or format_hint == "slider":
            for bound in ("$min", "$max"):
                if bound in props:
                    item[bound[1:]] = props[bound]
        if format_hint == "slider":
            if kind not in ("integer", "number", "string") or "$min" not in props or "$max" not in props:
                raise ImportError(f"setting {entry['id']}: $format: slider requires a numeric setting with $min and $max")
            try:
                numeric_default = float(default)
            except (TypeError, ValueError):
                raise ImportError(f"setting {entry['id']}: slider default must be numeric")
            if not (float(props["$min"]) <= numeric_default <= float(props["$max"])):
                raise ImportError(f"setting {entry['id']}: slider default is outside $min/$max")
        if format_hint in ("colorRgb", "colorArgb"):
            expected = 6 if format_hint == "colorRgb" else 8
            if kind != "string" or dynamic or options or not isinstance(default, str) or not re.fullmatch(r"#[0-9a-fA-F]{" + str(expected) + r"}", default):
                raise ImportError(f"setting {entry['id']}: $format: {format_hint} requires a matching #hex string default and no select options")
        if format_hint in ("filePath", "folderPath", "fontFamily", "hotkey"):
            if kind != "string":
                raise ImportError(f"setting {entry['id']}: $format: {format_hint} requires a string default")
            if format_hint == "hotkey":
                warnings.append(f"setting {entry['id']}: hotkey capture uses webview keyboard events and may not capture Windows-reserved shortcuts exactly")
        converted.append(item)

    by_id = {item["id"]: item for item in converted}

    def resolve_condition(source_id: str, reference: str) -> str | None:
        # Both a leaf ID and a group ID are resolved relative to their parent.
        # For a group ID this is intentionally one level above its children.
        scope = source_id.split(".")[:-1]
        candidates = []
        for size in range(len(scope), -1, -1):
            prefix = ".".join(scope[:size])
            candidate = f"{prefix}.{reference}" if prefix else reference
            if candidate not in candidates:
                candidates.append(candidate)
        for candidate in candidates:
            if candidate in by_id:
                return candidate
        return None

    # Resolve leaf-local conditions first. The keys become canonical IDs,
    # making them distinguishable from group conditions with the same spelling.
    for item in converted:
        for field in ("showIf", "hideIf"):
            resolved = {}
            for target_ref, accepted in item.get(field, {}).items():
                target_id = resolve_condition(item["id"], target_ref)
                if target_id is None:
                    raise ImportError(f"setting {item['id']}: {field} references unknown setting {target_ref!r}")
                if target_id in resolved:
                    raise ImportError(f"setting {item['id']}: {field} references {target_id!r} more than once")
                resolved[target_id] = accepted
            item[field] = resolved

    # Group metadata is represented in this editor as dotted setting IDs and
    # a readable label path. Group conditions resolve at the group's parent
    # scope, then combine with leaf conditions using canonical target IDs.
    for item in converted:
        item_path = item["id"]
        ancestors = item_path.split(".")[:-1]
        label_parts = []
        for index in range(1, len(ancestors) + 1):
            group_path = ".".join(ancestors[:index])
            meta = group_metadata.get(group_path)
            if not meta:
                continue
            label_parts.append(meta["name"])
            for field in ("showIf", "hideIf"):
                inherited = meta.get(field, {})
                existing = item.setdefault(field, {})
                for condition_ref, condition_values_list in inherited.items():
                    resolved_group_condition = resolve_condition(group_path, condition_ref)
                    if resolved_group_condition is None:
                        raise ImportError(
                            f"setting group {group_path}: {field} references unknown setting {condition_ref!r}"
                        )
                    if resolved_group_condition in existing:
                        accepted = [
                            value for value in existing[resolved_group_condition]
                            if any(type(value) is type(inherited_value) and value == inherited_value
                                   for inherited_value in condition_values_list)
                        ]
                        if not accepted:
                            raise ImportError(
                                f"setting {item_path}: inherited group visibility conflicts on {resolved_group_condition!r}"
                            )
                        existing[resolved_group_condition] = accepted
                    else:
                        existing[resolved_group_condition] = list(condition_values_list)
        if label_parts:
            leaf_label = item["label"] if item["label"] != item["id"] else item["id"].rsplit(".", 1)[-1]
            item["label"] = " / ".join(label_parts + [leaf_label])

    # Check the canonical condition targets against the referenced setting types.
    for item in converted:
        for field in ("showIf", "hideIf"):
            for target_id, accepted in item.get(field, {}).items():
                target = by_id.get(target_id)
                if target is None:
                    raise ImportError(f"setting {item['id']}: {field} references unknown setting {target_id!r}")
                target_kind = target["type"]
                def type_matches(value):
                    if target_kind == "boolean":
                        return isinstance(value, bool)
                    if target_kind == "integer":
                        return isinstance(value, int) and not isinstance(value, bool)
                    if target_kind == "number":
                        return isinstance(value, (int, float)) and not isinstance(value, bool)
                    if target_kind in ("string", "select"):
                        return isinstance(value, str)
                    return False
                if not all(type_matches(value) for value in accepted):
                    raise ImportError(f"setting {item['id']}: {field} values do not match setting {target_id!r}")
    graph = {item["id"]: list(item.get("showIf", {})) + list(item.get("hideIf", {})) for item in converted}
    visiting, visited = set(), set()
    def visit(key):
        if key in visiting:
            raise ImportError("visibility conditions contain a cycle")
        if key in visited:
            return
        visiting.add(key)
        for dependency in graph[key]:
            visit(dependency)
        visiting.remove(key)
        visited.add(key)
    for key in graph:
        visit(key)
    return converted, warnings, raw.strip()


def compiler_options(fields: dict[str, list[str]]) -> tuple[int, list[str], list[str], list[str]]:
    standard, definitions, libraries, warnings = None, [], [], []
    for key in ("compileroptions", "linkeroptions"):
        args = []
        for value in fields.get(key, []):
            try:
                args.extend(shlex.split(value))
            except ValueError as exc:
                raise ImportError(f"cannot parse @{key}: {exc}") from exc
        i = 0
        while i < len(args):
            arg = args[i]
            if arg.startswith("-std="):
                match = re.fullmatch(r"-std=(?:gnu\+\+|c\+\+)(11|14|17|20)", arg)
                if not match:
                    raise ImportError(f"unsupported C++ standard: {arg}")
                requested = int(match.group(1))
                if standard is not None and standard != requested:
                    raise ImportError("conflicting C++ standard options")
                standard = requested
            elif arg == "-l":
                i += 1
                if i >= len(args):
                    raise ImportError(f"missing library after @{key} -l")
                libraries.append(args[i])
            elif arg.startswith("-l") and len(arg) > 2:
                libraries.append(arg[2:])
            elif arg.startswith("-D") or arg.startswith("/D"):
                definition = arg[2:]
                if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*(?:=[A-Za-z0-9_.,:+/-]+)?", definition):
                    raise ImportError(f"unsupported define option: {arg}")
                definitions.append(definition)
            elif arg in ("-O0", "-O1", "-O2", "-O3", "-g", "-Wall", "-Wextra"):
                warnings.append(f"ignored GCC/Clang-only option from @{key}: {arg}")
            elif arg:
                raise ImportError(f"unsupported @{key} option {arg!r}; port it manually rather than dropping it")
            i += 1
    for library in libraries:
        if not re.fullmatch(r"[A-Za-z0-9_.+-]+", library):
            raise ImportError(f"invalid library name: {library!r}")
    return standard or 17, list(dict.fromkeys(definitions)), list(dict.fromkeys(libraries)), warnings


def has_callback(code: str, name: str, return_pattern: str) -> bool:
    pattern = rf"\b{return_pattern}\s+(?:(?:WINAPI|CALLBACK|__cdecl)\s+)?{name}\s*\([^{{}};]*\)\s*\{{"
    if re.search(pattern, code):
        return True
    if re.search(rf"\b{name}\b", code):
        raise ImportError(f"callback {name} is present but its signature is not supported")
    return False


def make_entry(source: str) -> str:
    code = mask_cpp_noncode(source)
    if not has_callback(code, "Wh_ModInit", r"(?:BOOL|bool)"):
        raise ImportError("source must define BOOL Wh_ModInit()")
    after = has_callback(code, "Wh_ModAfterInit", "void")
    before = has_callback(code, "Wh_ModBeforeUninit", "void")
    uninit = has_callback(code, "Wh_ModUninit", "void")
    setting_void = re.search(r"\bvoid\s+Wh_ModSettingsChanged\s*\([^{};]*\)\s*\{", code) is not None
    setting_reload = re.search(r"\bBOOL\s+Wh_ModSettingsChanged\s*\(\s*BOOL\s*\*\s*[A-Za-z_]\w*\s*\)\s*\{", code) is not None
    setting_named = re.search(r"\bWh_ModSettingsChanged\b", code) is not None
    if setting_named and not (setting_void or setting_reload):
        raise ImportError("Wh_ModSettingsChanged must be void Wh_ModSettingsChanged() or BOOL Wh_ModSettingsChanged(BOOL* bReload)")
    if setting_void and setting_reload:
        raise ImportError("ambiguous Wh_ModSettingsChanged definition")
    after_arg = "Wh_ModAfterInit" if after else "::wmod::windhawk_adapter::NoopCallback"
    before_arg = "Wh_ModBeforeUninit" if before else "::wmod::windhawk_adapter::NoopCallback"
    uninit_arg = "Wh_ModUninit" if uninit else "::wmod::windhawk_adapter::NoopCallback"
    if setting_void or setting_reload:
        macro = (
            "WMOD_DEFINE_WINDHAWK_LIFECYCLE_WITH_SETTINGS_CHANGED(\n"
            f"    Wh_ModInit, {after_arg}, Wh_ModSettingsChanged,\n"
            f"    {before_arg}, {uninit_arg}\n)"
        )
    else:
        macro = (
            "WMOD_DEFINE_WINDHAWK_LIFECYCLE(\n"
            f"    Wh_ModInit, {after_arg}, {before_arg}, {uninit_arg}\n)"
        )
    return '#include "windhawk_api.h"\n#include "windhawk_settings_changed_adapter.h"\n#include "mod.cpp"\n\n' + macro + "\n"


def cmake_project(module: str, standard: int, definitions: list[str], libraries: list[str]) -> str:
    def args(items):
        return "\n".join('  "' + item + '"' for item in items)
    result = f'''cmake_minimum_required(VERSION 3.21)
project({module} LANGUAGES C CXX)

set(WMOD_SDK_DIR "" CACHE PATH "Path to the 00047 repository sdk/native-mod directory")
if(NOT EXISTS "@WMOD_SDK_DIR@/00047_mod_api.h")
  message(FATAL_ERROR "Set WMOD_SDK_DIR to the 00047 sdk/native-mod directory")
endif()
include("@WMOD_SDK_DIR@/windhawk_project_metadata.cmake")

set(WMOD_HOOK_ENGINE_SOURCES
  "@WMOD_SDK_DIR@/windhawk_hook_compat.cpp"
  "@WMOD_SDK_DIR@/third_party/minhook/src/buffer.c"
  "@WMOD_SDK_DIR@/third_party/minhook/src/hook.c"
  "@WMOD_SDK_DIR@/third_party/minhook/src/trampoline.c"
  "@WMOD_SDK_DIR@/third_party/minhook/src/hde/hde64.c"
  "@WMOD_SDK_DIR@/third_party/distorm/src/decoder.c"
  "@WMOD_SDK_DIR@/third_party/distorm/src/distorm.c"
  "@WMOD_SDK_DIR@/third_party/distorm/src/instructions.c"
  "@WMOD_SDK_DIR@/third_party/distorm/src/insts.c"
  "@WMOD_SDK_DIR@/third_party/distorm/src/mnemonics.c"
  "@WMOD_SDK_DIR@/third_party/distorm/src/operands.c"
  "@WMOD_SDK_DIR@/third_party/distorm/src/prefix.c"
  "@WMOD_SDK_DIR@/third_party/distorm/src/textdefs.c"
)

add_library({module} SHARED windhawk_entry.cpp @WMOD_HOOK_ENGINE_SOURCES@)
target_compile_features({module} PRIVATE cxx_std_{standard})
target_compile_definitions({module} PRIVATE WMOD_BUILD_DLL WH_MOD WIN32_LEAN_AND_MEAN NOMINMAX
{args(definitions)}
)
target_include_directories({module} PRIVATE
  "@WMOD_SDK_DIR@"
  "@WMOD_SDK_DIR@/third_party/minhook/include"
  "@WMOD_SDK_DIR@/third_party/minhook/src"
  "@WMOD_SDK_DIR@/third_party/minhook/src/hde"
  "@WMOD_SDK_DIR@/third_party/distorm/include"
  "@WMOD_SDK_DIR@/third_party/distorm/src"
)
target_link_libraries({module} PRIVATE
  winhttp dbghelp psapi user32 gdi32 shell32 advapi32 ole32 oleaut32 comctl32 comdlg32 dwmapi uxtheme
{args(libraries)}
)
set_target_properties({module} PROPERTIES
  PREFIX ""
  OUTPUT_NAME "{module}"
  RUNTIME_OUTPUT_DIRECTORY "@CMAKE_BINARY_DIR@/bin"
)
'''
    return result.replace("@WMOD_SDK_DIR@", "$" + "{WMOD_SDK_DIR}").replace(
        "@WMOD_HOOK_ENGINE_SOURCES@", "$" + "{WMOD_HOOK_ENGINE_SOURCES}"
    ).replace("@CMAKE_BINARY_DIR@", "$" + "{CMAKE_BINARY_DIR}")


def copy_dependencies(file: Path, source_root: Path, destination: Path,
                      visited: set[Path], total: list[int]) -> None:
    if file.is_symlink():
        raise ImportError(f"symbolic-link dependency is not allowed: {file}")
    file = file.resolve(strict=True)
    try:
        relative = file.relative_to(source_root)
    except ValueError as exc:
        raise ImportError(f"local include escapes source directory: {file}") from exc
    if file in visited:
        return
    if len(visited) >= MAX_TREE_FILES:
        raise ImportError(f"more than {MAX_TREE_FILES} local source files")
    data = file.read_bytes()
    if len(data) > MAX_FILE_BYTES:
        raise ImportError(f"source/dependency exceeds {MAX_FILE_BYTES} bytes: {file}")
    total[0] += len(data)
    if total[0] > MAX_TREE_BYTES:
        raise ImportError(f"local source dependencies exceed {MAX_TREE_BYTES} bytes")
    visited.add(file)
    target = destination / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(data)
    text = data.decode("utf-8-sig", errors="replace")
    for found in re.finditer(r'(?m)^\s*#\s*include\s*"([^"]+)"', text):
        candidate = file.parent / found.group(1)
        if candidate.exists():
            copy_dependencies(candidate, source_root, destination, visited, total)


def import_project(source_path: Path, output_dir: Path) -> tuple[Path, list[str]]:
    if source_path.expanduser().is_symlink():
        raise ImportError("source must not be a symbolic link")
    source_path = source_path.expanduser().resolve(strict=True)
    if source_path.suffix.casefold() not in (".cpp", ".cxx", ".cc") or not source_path.is_file():
        raise ImportError("select a single-file Windhawk C++ source (.cpp, .cxx or .cc)")
    if source_path.stat().st_size > MAX_FILE_BYTES:
        raise ImportError(f"source exceeds {MAX_FILE_BYTES} bytes")
    source = source_path.read_text(encoding="utf-8-sig")
    fields = parse_metadata(source)
    mod_id = get_value(fields, "id", True)
    if not ID_PATTERN.fullmatch(mod_id) or mod_id.startswith(".") or mod_id.endswith(".") or ".." in mod_id:
        raise ImportError("@id cannot be represented as a safe 00047 mod ID")
    name, version = get_value(fields, "name", True), get_value(fields, "version", True)
    description = get_value(fields, "description")
    if len(name) > 100 or len(version.encode("utf-8")) > 40 or len(description) > 1000:
        raise ImportError("@name, @version, or @description exceeds 00047 manifest limits")
    if fields.get("exclude"):
        raise ImportError("@exclude rules cannot be represented by 00047's target model")
    includes = fields.get("include", [])
    if not includes:
        raise ImportError("metadata must contain at least one exact @include process name")
    targets = []
    for raw in includes:
        target = CANONICAL_TARGETS.get(raw.casefold())
        if target is None:
            raise ImportError(f"@include {raw!r} is not an exact allow-listed process name; wildcard/path targets need manual porting")
        if target not in targets:
            targets.append(target)
    architectures = [item.casefold() for item in fields.get("architecture", [])]
    if architectures and not any(item in ("amd64", "x86-64", "x64") for item in architectures):
        raise ImportError("metadata does not declare an x64-compatible @architecture")
    warnings = []
    if not architectures:
        warnings.append("no @architecture declared; 00047 imports it as x64 only")
    if len(set(raw.casefold() for raw in includes)) < len(includes):
        warnings.append("duplicate @include entries were collapsed")

    standard, definitions, libraries, option_warnings = compiler_options(fields)
    warnings.extend(option_warnings)
    settings, setting_warnings, raw_settings = parse_settings(source)
    warnings.extend(setting_warnings)
    module = re.sub(r"[^A-Za-z0-9_]", "_", mod_id)
    if not module or module[0].isdigit():
        module = "mod_" + module
    manifest = {
        "manifestVersion": 1,
        "id": mod_id,
        "name": name,
        "version": version,
        "description": description,
        "architecture": "x64",
        "module": f"bin/{module}.dll",
        "targets": targets,
    }
    if settings:
        manifest["settings"] = settings
    output = output_dir.expanduser().resolve()
    project = output / mod_id
    if project.exists() or project.is_symlink():
        raise ImportError(f"destination already exists; refusing to overwrite: {project}")
    entry = make_entry(source)
    api_findings = audit_text(source_path.name, source)
    api_summary: dict[str, int] = {}
    for finding in api_findings:
        category = finding["category"]
        api_summary[category] = api_summary.get(category, 0) + 1
        if category in {"unsupported", "unknown-api", "manual-lifecycle-adaptation", "warning"}:
            first_location = finding["locations"][0] if finding["locations"] else {"line": 1}
            warnings.append(
                f"API compatibility review required: {finding['name']} "
                f"({category}) at {first_location.get('file', source_path.name)}:{first_location['line']}"
            )
    requires_manual_api_review = any(
        item["category"] in {"unsupported", "unknown-api", "manual-lifecycle-adaptation", "warning"}
        for item in api_findings
    )
    output.mkdir(parents=True, exist_ok=True)
    project.mkdir()
    try:
        copy_dependencies(source_path, source_path.parent, project, set(), [0])
        # The generated lifecycle wrapper uses one stable include name. Also place
        # the selected root source there; the copied original-name file is kept so
        # source identity remains obvious in the imported project.
        shutil.copyfile(source_path, project / "mod.cpp")
        (project / "windhawk_entry.cpp").write_text(entry, encoding="utf-8", newline="\n")
        (project / "CMakeLists.txt").write_text(cmake_project(module, standard, definitions, libraries), encoding="utf-8", newline="\n")
        (project / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n")
        readme = extract_block(source, README_START, README_END)
        if readme.strip():
            (project / "WINDHAWK_README.md").write_text(readme.strip() + "\n", encoding="utf-8", newline="\n")
        (project / "windhawk-import.json").write_text(json.dumps({
            "formatVersion": 1,
            "sourceFile": source_path.name,
            "metadata": fields,
            "settingsBlock": raw_settings,
            "warnings": warnings,
            "apiCompatibility": {
                "summary": api_summary,
                "requiresManualReview": requires_manual_api_review,
                "findings": api_findings,
                "note": "Static source analysis only; this does not certify ABI equivalence, behavior, or safety.",
            },
            "runtimeLimitations": [
                "This is a source-level compatibility layer, not full Windhawk runtime equivalence.",
                "Wildcards, full-path process matching, exclusions and non-allow-listed targets are not imported.",
                "Nested setting groups, numeric arrays and selected settings UI annotations need manual porting.",
                "Settings-change callbacks run on a worker thread and reload requests are polled by 00047.",
            ],
        }, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n")
    except Exception:
        shutil.rmtree(project, ignore_errors=True)
        raise
    return project, warnings


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="Import a single-file Windhawk C++ mod into a buildable 00047 project.")
    parser.add_argument("source", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        project, warnings = import_project(args.source, args.output_dir)
    except (ImportError, OSError, UnicodeError) as exc:
        print(f"Windhawk import failed: {exc}", file=sys.stderr)
        return 2
    print(f"Imported project: {project}")
    print(f"Manifest: {project / 'manifest.json'}")
    print(f"Lifecycle wrapper: {project / 'windhawk_entry.cpp'}")
    print("Audit the source and verify build output before enabling the mod.")
    for warning in warnings:
        print(f"Warning: {warning}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
