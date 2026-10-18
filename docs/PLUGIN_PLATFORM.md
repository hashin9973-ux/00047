# 00047 plugin platform: runtime extensions and native-mod roadmap

## Current direction

00047 is evolving from a block-pack loader into a general extension host. There are two distinct extension tiers:

1. **Out-of-process runtime extensions** are packaged in ZIPs and launched by an executable/interpreter. They can contribute automation blocks, app-level commands, or background services.
2. **In-process native mods** require a separately designed Windows-only loader and stable native ABI. They are not loaded by the current external runtime host.

The first tier is now the generic integration surface for scripting languages and external programs. It uses process boundaries so a script crash does not directly corrupt 00047's Rust process. This is not a security sandbox: a plugin runs with the current Windows account's permissions.

## Extension types

- `blocks`: existing `user.*` conditions and actions in the automation editor. Version 1 behavior is preserved.
- `commands`: one-shot app-level operations, visible in the Plugins page and callable independently of an automation graph.
- `services`: long-running external worker processes. The Plugins page can start and stop them; services with `autoStart: true` are started during app startup and managed children are stopped on app exit or restart.

Commands and services use the same `runtime.command` / `runtime.args` model as block plugins. The host does not inspect source-language syntax; it invokes the declared program with separate arguments. Use an installed runtime such as Python, Node.js, PowerShell, Ruby, or an executable bundled in the ZIP. A source file is not self-executing: its interpreter or compiler/runtime must be present and compatible with the platform.

## Manifest example: app command + worker service

This is a package manifest shape, not a complete installable package; the referenced Python files must also be present in the ZIP.

```json
{
  "manifestVersion": 1,
  "id": "com.example.runtime-toolbox",
  "name": "Runtime Toolbox",
  "version": "1.0.0",
  "description": "Example app-level command and background worker.",
  "commands": [
    {
      "id": "write-note",
      "title": "Write note",
      "description": "Writes the provided text to a configured path.",
      "entrypoint": "scripts/write_note.py",
      "runtime": {
        "command": "python",
        "args": ["{entrypoint}"]
      },
      "fields": [
        {
          "key": "path",
          "label": "Output path",
          "type": "text",
          "required": true,
          "maxLen": 1000
        },
        {
          "key": "text",
          "label": "Text",
          "type": "text",
          "required": true,
          "maxLen": 10000
        }
      ],
      "defaults": {
        "path": "C:/Temp/00047-note.txt",
        "text": "Hello from 00047"
      },
      "timeoutSec": 30
    }
  ],
  "services": [
    {
      "id": "sample-watcher",
      "title": "Sample watcher",
      "description": "An optional long-running worker.",
      "entrypoint": "scripts/watcher.py",
      "runtime": {
        "command": "python",
        "args": ["{entrypoint}"]
      },
      "autoStart": false,
      "fields": [],
      "defaults": {}
    }
  ]
}
```

### Runtime contract

The host resolves `entrypoint` under the installed package directory and rejects path traversal. `runtime.args` must be an array of strings; arguments are not concatenated into a shell command. The host sets the working directory to the package directory and provides:

- `PLUGIN_ID`, `PLUGIN_EXTENSION_ID`
- `PLUGIN_DIR`, `PLUGIN_ENTRYPOINT`
- `FIELD_<KEY>` environment variables for declared fields

`{pluginDir}`, `{entrypoint}`, and `{fieldKey}` placeholders can be used in `runtime.command` and `runtime.args`. `defaults` is a JSON object merged with values entered in the Plugins UI. Command output and exit code are returned to the UI. One-shot commands default to a 120-second timeout; `timeoutSec` may be set to an integer from 1 to 86400. Background services have no automatic restart policy in this version; the host only starts/stops declared processes and stops its managed children during shutdown.

## Native Windows modification roadmap

The runtime host by itself does **not** provide Windhawk-level shell customization. Replacing or hooking Explorer, Start, taskbar, notification, or other shell components requires a Windows-native mod subsystem with its own architecture:

1. **Native mod package contract**: target processes, DLL/module artifacts, architecture, version compatibility, lifecycle entry points, and settings metadata.
2. **In-process module host**: load and manage mods in the correct process; define ABI and lifecycle rules, and keep external scripting workers separate from injected/native code.
3. **Hooking and compatibility layer**: reliable native hooks, process targeting, Windows-build detection, conflict diagnostics, and feature isolation.
4. **Recovery controls**: safe mode that disables all native mods, per-mod disable/rollback, watchdog/restart behavior, explicit consent before process-level changes, and logs sufficient to recover from a broken mod.
5. **Packaging/security**: trusted-source/signature policy, architecture checks, dependency/runtime detection, and a reviewable declaration of each mod's requested privileges.

Native mods can execute inside critical Windows processes and may crash or destabilize the desktop. They should not be treated as ordinary scripts or silently auto-loaded. This subsystem needs implementation and Windows-specific testing before claiming Windhawk-like capability.

## Compatibility and safety

- Existing ZIP packages containing only `blocks` continue to work.
- Legacy JSON block packs remain supported.
- ZIP import still enforces size and archive-path limits.
- Install and preview validate declared runtime entrypoints, but that is not proof the program itself is safe.
- External source code is arbitrary code execution under the current user. It can access files/network and launch other programs according to that user's permissions. Installing a plugin is a trust decision.
- Plugins should not download or install dependencies automatically. Ship required resources with the package or document manual prerequisite setup.
