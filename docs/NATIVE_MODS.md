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

## Package authoring tools

The repository includes a standard-library-only project scaffold generator, package builder, and validator. Start by generating an example project (choose an ID and name appropriate for your mod):

    python tools/new_native_mod.py --id com.example.my-mod --name "My Mod" --output-dir mods

The generator creates a manifest, C++ lifecycle exports, CMake configuration, and build/package instructions. It will refuse to overwrite an existing project directory. This starter DLL only logs lifecycle messages; it does not patch Windows UI.

Build the generated project with the x64 Visual Studio C++ toolchain, pointing CMake to the SDK headers:

    cmake -S mods/com.example.my-mod -B mods/com.example.my-mod/build -A x64 -DWMOD_SDK_DIR=C:/path/to/00047/sdk/native-mod
    cmake --build mods/com.example.my-mod/build --config Release

Build the ZIP from the repository root (replace the DLL path with the filename recorded in that generated project's manifest):

    python tools/package_native_mod.py --manifest mods/com.example.my-mod/manifest.json --dll mods/com.example.my-mod/build/bin/Release/com_example_my_mod.dll --output dist/my-mod.zip

Run the package builder against any manifest/DLL pair from the repository root:

    python tools/package_native_mod.py --manifest path/to/manifest.json --dll path/to/mod.dll --output dist/my-mod.zip

The builder puts the manifest at the ZIP root and places the DLL at the exact path specified by manifest.module. It writes a deterministic archive and validates it before replacing the requested output file. The same manifest and DLL inputs should produce byte-identical ZIPs with the same Python version.

Validate a package independently at any time:

    python tools/validate_native_mod.py dist/my-mod.zip

Validation checks archive and decompression limits, path traversal, case-collisions, Windows-reserved names and file/directory conflicts, symbolic links and encryption, manifest fields, allowed target process names, the x64 PE32+ machine type, export-table bounds and the three required lifecycle exports. All archive entries are read to verify their CRCs. Invalid packages fail before an output package is committed.

The author tools do not replace the application's independent validation at installation time, and they do not provide a security sandbox or certify that a DLL is safe. They confirm the package structure and ABI prerequisites only. Regression tests include a synthetic PE32+ export-table fixture, invalid package cases, and deterministic packaging:

    python -m unittest discover -s tools/tests -v

Windows CI runs those tests and builds/validates the real no-op sample package.

## Settings and logs

A mod may declare an optional `settings` array in `manifest.json`. Each setting has a unique `id`, a `label`, a `type`, and a `default`. Supported types are `boolean`, `string`, `stringArray`, `integer`, `number`, and `select`. Numeric settings can declare `min`, `max`, and `step`; string settings can declare `maxLength`; select and string-array settings can declare `options` with unique `value` and `label` properties. A string-array setting stores a JSON array of strings; without `options` it is edited as a list of text values, and with `dynamicSelect: true` each element uses the same runtime option list.

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

The SDK provides loaded-module export lookup through `wmod::ModuleHandle` and `wmod::ResolveSymbol`. These helpers only resolve exports from modules already loaded in the current process; they do not load arbitrary DLLs or resolve private/PDB symbols. The sample verifies lookup of `kernel32!GetCurrentProcessId` without calling it or changing desktop behavior.

### Limited non-hook Windhawk-style helpers

`sdk/native-mod/windhawk_nonhook_compat.h` provides a deliberately limited source-level subset for mods that need logging, settings, and local storage: `Wh_Log`, `Wh_GetIntSetting`, `Wh_GetStringSetting`, `Wh_FreeStringSetting`, `Wh_GetIntValue`, `Wh_SetIntValue`, `Wh_GetStringValue`, `Wh_SetStringValue`, `Wh_GetBinaryValue`, `Wh_SetBinaryValue`, `Wh_DeleteValue`, `Wh_GetModStoragePath`, `Wh_SetDynamicSelectOption`, `Wh_DeleteDynamicSelectOption`, and `Wh_ClearDynamicSelectOptions`. Settings come from `settings.json` with schema defaults from `manifest.json`. Storage values are package-scoped in `wh_values.ini`; binary payloads are hexadecimal in a separate section and capped at 16,383 bytes. `Wh_GetModStoragePath` creates a package-scoped `storage` directory on first use. Dynamic select APIs store entries using Windhawk's `::wh_select_option::<setting path>::<value>` convention. A dynamic option list is declared on a string setting using `"type": "string", "dynamicSelect": true`; fixed string choices can additionally be declared in its `options` array. The settings UI renders the dynamic string as a combobox, merges persisted runtime options with static options when settings are opened, replaces matching static labels with runtime labels, and keeps the user's current selection visible if an option disappears. The same option list can be applied to each item of a `"type": "stringArray", "dynamicSelect": true` setting. A separate fixed `"type": "select"` field with `dynamicSelect: true` is also accepted. This implementation supports flat setting IDs (including dotted IDs) and caps runtime option values/labels at 100 characters and per-setting option count at 100.

`Wh_GetUrlContent` and `Wh_FreeUrlContent` use WinHTTP to retrieve a response by HTTP(S) GET. The helper rejects URL-embedded credentials, limits URL length to 8,192 characters and response bodies to 8 MiB, uses bounded connection/read timeouts, and returns both the body and HTTP status code. With `targetFilePath == nullptr`, body bytes are returned in memory; otherwise the body is written to a temporary sibling file and moved into place only after the response is read successfully. The structure must be released with `Wh_FreeUrlContent`. This helper is synchronous; avoid calling it on latency-sensitive shell threads. C++ consumers that call it must link the `winhttp` library. These facilities are a source-level non-hook subset, not ABI-compatible with the Windhawk engine.

The compatibility header intentionally does **not** provide `Wh_SetFunctionHook`, `Wh_RemoveFunctionHook`, `Wh_ApplyHookOperations`, `Wh_FindFirstSymbol`, callback dispatch, or the Windhawk runtime. It does not include a function-hook framework or private/PDB symbol resolution. Do not treat a mod as portable merely because it compiles against the helper header. Mods that need those omitted APIs must be adapted to available functionality or run under a compatible runtime. Native mods can also use Windows APIs and their own third-party hook frameworks today, subject to the target allow-list and architecture; the app does not bundle or support such frameworks.

## Safety and recovery

Native mods are arbitrary machine code and are not sandboxed. Loading them can crash or hang their target process. The UI requires a second confirmation before enabling a mod and states the target names before install. Test new DLLs in a VM or with a recoverable Windows user profile. If Explorer becomes unstable, use Task Manager to restart Windows Explorer or launch 00047 and disable the offending mod. Mods are not loaded into arbitrary user-selected processes.

This engine does not yet have digital-signature trust enforcement, a stable cross-process host callback API, a compiler/build service, or guaranteed recovery from a DLL that crashes a target after successful initialization. The current x64 allow-list and explicit enable/disable flow are first-stage restrictions, not a sandbox.


## Porting a Windhawk mod

Use [WINDHAWK_COMPATIBILITY.md](WINDHAWK_COMPATIBILITY.md) and `tools/audit_windhawk_mod.py` to identify supported non-hook API calls, unsupported hook/symbol/disassembly calls, and lifecycle callbacks that require manual adaptation. The audit is static source analysis only and does not certify compatibility or safety.
