# Native Windows mod engine

This milestone adds a separate native-mod pipeline, distinct from workflow block packs and independent process extensions.

## Package schema

A mod is a ZIP with root-level manifest.json and its x64 DLL:

    taskbar-mod.zip
    ├── manifest.json
    └── bin/
        └── taskbar_mod.dll

Manifest v1:

    {
      "manifestVersion": 1,
      "id": "com.example.taskbar-mod",
      "name": "Taskbar Mod",
      "version": "1.0.0",
      "description": "Example x64 shell mod",
      "architecture": "x64",
      "module": "bin/taskbar_mod.dll",
      "targets": ["explorer.exe"]
    }

Only these target processes are accepted by this build: explorer.exe, StartMenuExperienceHost.exe, ShellExperienceHost.exe, and SearchHost.exe. The host does not accept arbitrary PIDs or arbitrary process names. It supports x64 DLLs only.

Before installation, the importer validates archive paths and size, checks the PE machine type and PE32+ format, and verifies that the DLL exports WmodGetApiVersion, WmodInitializeV1, and WmodShutdownV1. Enabling calls the version function and initialization function inside each currently running supported target. The app scans every five seconds while running, so enabled mods are also applied when a supported shell process appears later. Disabling calls WmodShutdownV1 and attempts to unload the DLL from matching supported processes. If a load attempt fails on a present target (including ABI mismatch or an initialization failure), the host disables the mod and attempts to roll back partial loads instead of retrying that failure every five seconds.

## SDK ABI v1

Include sdk/native-mod/00047_mod_api.h when building a native x64 DLL. The exact export ABI is:

- WmodGetApiVersion() returns 1.
- WmodInitializeV1() returns non-zero if initialization succeeded.
- WmodShutdownV1() removes hooks and releases mod-owned resources before unload.

A no-op C++ sample is in samples/native-mod. It writes debug output only, so it is safe as a loader-lifecycle example without modifying the desktop.

## Settings and logs

A mod may declare an optional `settings` array in `manifest.json`. Each setting has a unique `id`, a `label`, a `type`, and a `default`. Supported types are `boolean`, `string`, `integer`, `number`, and `select`. Numeric settings can declare `min`, `max`, and `step`; string settings can declare `maxLength`; select settings declare `options` with unique `value` and `label` properties.

```json
{
  "settings": [
    { "id": "blurEnabled", "label": "Enable blur", "type": "boolean", "default": true },
    { "id": "opacity", "label": "Opacity", "type": "number", "default": 0.65, "min": 0.1, "max": 1, "step": 0.05 },
    { "id": "style", "label": "Style", "type": "select", "default": "mica",
      "options": [{ "value": "mica", "label": "Mica" }, { "value": "acrylic", "label": "Acrylic" }] }
  ]
}
```

The Native Mods view validates this schema and edits `settings.json` in the installed package directory. The host validates values against the declared schema before saving. The DLL must opt in: include `sdk/native-mod/00047_mod_services.h`, call `wmod::SettingsPath()` or `wmod::ReadSettingsJson(...)`, parse the JSON with the parser of your choice, and reload it when your mod supports live changes. The settings editor does not force a mod to apply changes or mutate another process's memory.

The same SDK header provides `wmod::Log(level, message)`, which appends structured JSON Lines to the package's `mod.log`. Select **View logs** on a mod card to inspect the most recent lines in 00047. Log output is bounded when read by the UI; mods should avoid recording secrets.

The SDK provides loaded-module export lookup through `wmod::ModuleHandle` and `wmod::ResolveSymbol`. These helpers only resolve exports from modules already loaded in the current process; they do not load arbitrary DLLs or resolve private/PDB symbols. The sample verifies lookup of `kernel32!GetCurrentProcessId` without calling it or changing desktop behavior. This is not Windhawk API compatibility: the SDK still does not provide Windhawk's `Wh_*` API or a built-in function-hook framework. Mods can use Windows APIs and their own third-party hook frameworks today, subject to the target allow-list and architecture.

## Safety and recovery

Native mods are arbitrary machine code and are not sandboxed. Loading them can crash or hang their target process. The UI requires a second confirmation before enabling a mod and states the target names before install. Test new DLLs in a VM or with a recoverable Windows user profile. If Explorer becomes unstable, use Task Manager to restart Windows Explorer or launch 00047 and disable the offending mod. Mods are not loaded into arbitrary user-selected processes.

This engine does not yet have digital-signature trust enforcement, a stable cross-process host callback API, a compiler/build service, or guaranteed recovery from a DLL that crashes a target after successful initialization. The current x64 allow-list and explicit enable/disable flow are first-stage restrictions, not a sandbox.
