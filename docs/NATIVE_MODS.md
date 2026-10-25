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

Before installation, the importer validates archive paths and size, checks the PE machine type and PE32+ format, and verifies that the DLL exports WmodGetApiVersion, WmodInitializeV1, WmodShutdownV1, WmodIsInitializedV1, and WmodCanUnloadV1. Package access also rejects symlinked package roots, package folders, and manifest/DLL files, and checks that each resolved module path remains inside its managed package directory. The enabled-mod state file is bounded to 1 MiB, filters invalid IDs when loaded, refuses symlink destinations, and is saved via a same-directory temporary file and rename to avoid partial JSON writes. ZIP import validates Windows-reserved device names, forbidden filename characters, trailing dots/spaces, case collisions, and file/directory conflicts both during validation and extraction, so validation cannot be bypassed by changing the archive between those steps. This is defense-in-depth for the local mod store; it does not sandbox the native DLL. Optional Trusted Authenticode enforcement can be enabled from the Native Mods view. When enabled, each native DLL must pass Windows `WinVerifyTrust` validation using cached trust/revocation data only; no network retrieval is performed and missing/invalid verification data fails closed. Enabling the policy disables enabled packages that do not verify and attempts safe unload. The policy defaults OFF to preserve existing unsigned development packages. A valid signature confirms publisher-chain trust under the local Windows trust policy; it does not sandbox or certify the DLL.  Remote calls have a 10-second wait limit. If a remote thread times out or its wait result is uncertain, the host marks that PID indeterminate and quarantines it from further native-mod operations until the process exits; it deliberately does not race shutdown/unload against a possibly active initializer. Per-mod UI status lists each discovered target PID and its latest lifecycle result. Enabling calls the version function and initialization function inside each currently running supported target. The app scans every five seconds while running, so enabled mods are also applied when a supported shell process appears later. The per-target lifecycle panel reports detected, loading, initializing, initialized, stopping, unloaded, failed, rolled-back, and indeterminate states independently for each PID. Disabling calls WmodShutdownV1 and attempts to unload the DLL from matching supported processes. If a load attempt fails on a present target (including ABI mismatch or an initialization failure), the host disables the mod and attempts to roll back partial loads instead of retrying that failure every five seconds.

## Trusted Authenticode policy

The Native Mods view exposes a global **Require trusted signatures** control, persisted alongside the enabled-mod list in `enabled.json`. It is OFF by default. When ON, 00047 calls Windows `WinVerifyTrust` with the generic Authenticode policy, local-cache-only URL retrieval, and certificate-chain revocation checks. Verification never triggers network retrieval; if required trust or revocation data is not available locally, verification fails closed. A failed verification blocks new loads, removes the mod from persisted enabled intent, and records signature-specific and general safe-unload cleanup in `enabled.json`. The manager also checks disabled installed packages because a previous failed unload can leave their DLL mapped. Any uncertain native-mod unload—not only a signature-policy rejection—is persisted as safe-unload work and retried during later background scans. A pending cleanup record prevents re-enabling, updating, or uninstalling the package until unload is confirmed. For an already loaded untrusted mod, 00047 attempts shutdown and unload; if unload safety cannot be confirmed, the target DLL remains mapped and cleanup is retried during later background scans. Pending cleanup prevents re-enabling, updating, or uninstalling the package until a safe-unload attempt succeeds. The UI reports when a shell restart may be needed. Turning the policy OFF permits unsigned DLLs again. This is a publisher-trust gate, not an execution sandbox.

## Durable safe-unload recovery

Every unload failure or uncertain completion is recorded in `enabled.json`, independently of the Authenticode policy. The background monitor retries safe shutdown and unload during subsequent scans. The Native Mods view reports pending cleanup; install/update, re-enable, and uninstall are blocked while the previous DLL image may still be mapped. A cleanup marker is cleared only after unload is confirmed and the marker update is persisted. If the host cannot prove safe unload, restart the affected shell process rather than forcing an unmap.

## SDK ABI v2

Include sdk/native-mod/00047_mod_api.h when building a native x64 DLL. The exact export contract is:

- WmodGetApiVersion() returns 2.
- WmodInitializeV1() returns non-zero if initialization succeeded.
- WmodShutdownV1() releases mod-owned resources and disables/removes tracked hooks.
- WmodIsInitializedV1() reports whether initialization is active.
- WmodCanUnloadV1() reports whether the module has no remaining registered hooks or detectable thread instruction/stack references to its image. The host requires this check to pass before FreeLibrary; otherwise it leaves the DLL mapped.

Mods that opt into `windhawk_settings_changed_adapter.h` may additionally export `WmodIsReloadRequestedV1()`. This optional query reports a pending reload from `BOOL Wh_ModSettingsChanged(BOOL* bReload)` and is not a required package export.

ABI version 2 adds the two lifecycle-query exports needed to recover safely from a deferred unload. Packages with only the original three exports are not accepted by this SDK version.

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

Validation checks archive and decompression limits, path traversal, case-collisions, Windows-reserved names and file/directory conflicts, symbolic links and encryption, manifest fields, allowed target process names, the x64 PE32+ machine type, export-table bounds and the five required lifecycle exports. All archive entries are read to verify their CRCs. Invalid packages fail before an output package is committed.

The author tools do not replace the application's independent validation at installation time, and they do not provide a security sandbox or certify that a DLL is safe. They confirm the package structure and ABI prerequisites only. Regression tests include a synthetic PE32+ export-table fixture, invalid package cases, and deterministic packaging:

    python -m unittest discover -s tools/tests -v

Windows CI runs those tests, builds/validates the real no-op sample package, and exercises the remote loader against a test-only process whose output name is `explorer.exe`. That process is not the user's Windows shell: a separate test DLL hooks its exported probe function, verifies that the result changes from 7 to 17 while enabled, then confirms it returns to 7 after unload. CI also checks ABI/init failure rollback and repeated load/unload cycles.

## Build and install from source in the app

The Native Mods view has a **Build from source...** action for CMake projects created by `tools/new_native_mod.py` or equivalent projects that follow the same SDK layout. Choose the project folder containing `manifest.json` and `CMakeLists.txt`. The project manifest must point to `bin/<module-name>.dll`.

On Windows, the app invokes CMake with an x64 Visual Studio generator, points `WMOD_SDK_DIR` at the bundled `sdk/native-mod` resources, and builds the Release configuration. CMake and the Visual Studio C++ x64 build tools must already be installed and available to the current user. Each configure/build stage has a five-minute timeout and captured console output is bounded. The temporary build workspace is deleted after the package is installed.

**Security:** CMake files and build scripts can run arbitrary commands with your Windows user permissions. Only build source you trust. After compilation, 00047 creates a package and passes it through the normal native-mod package validator and installer. The mod is installed **disabled by default**; it does not execute in Explorer until you explicitly enable it. If trusted Authenticode enforcement is on, the build will be rejected at installation unless the resulting DLL has a signature trusted by Windows under the configured local policy. The in-app builder currently packages the manifest and the primary DLL only; projects that require additional runtime files should be packaged using the ZIP authoring workflow instead.

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

The compatibility layer provides source-level `Wh_SetFunctionHook`, `Wh_RemoveFunctionHook`, and `Wh_ApplyHookOperations` through bundled MinHook, plus bounded `Wh_Disasm` decoding through diStorm; it remains a source-level subset, not the Windhawk runtime. A named per-process registry rejects duplicate targets and conservatively rejects distinct entry addresses less than 10 bytes apart across independently linked mod DLLs. Hook creation, hook-apply batches, shutdown removal, and unload-quiescence scans hold the shared registry mutex for the duration, serializing bundled MinHook backend mutations and patch transactions across mod DLLs. This may reject compact but separate functions, and arbitrary executable-memory patches are not coordinated. The registry also prevents unloading a module while another registered hook targets an address inside that module. A successful unload-safety query records a retiring-module marker before returning; subsequent SDK hook claims into that image fail, closing the query-to-unload race for cooperating SDK users. A fresh lifecycle clears the marker when the same module base is reused. The umbrella header includes `windhawk_symbol_compat.h`, which implements bounded `Wh_FindFirstSymbol`, `Wh_FindNextSymbol`, and `Wh_FindCloseSymbol` enumeration through DbgHelp. It snapshots at most 100,000 symbol records and approximately four million name characters per search, depends on the symbols/PDB available for the exact target build, and can perform network I/O when a symbol server is selected. `Wh_HookSymbols` checks a bounded persistent per-user offset cache, imports validated compatible cache text from an existing Windhawk `HKLM\SOFTWARE\Windhawk\Engine\ModsWritable\<mod-id>\SymbolCache` key where available, and supports HTTPS online-cache lookup (explicit `onlineCacheUrl` or a default package-ID URL) with a 3.5-second request budget and at most one automatic redirect. It merges compatible records back only when the Windhawk key already exists and is writable; it never creates the HKLM key tree. Repeated unresolved required symbols use a 60-second per-user throttle sidecar and the Windhawk `error:3` registry record format when the existing `SymbolCache` key is available. The registry record validates target-process identity, mod version/library filename, and target image; it uses target-process creation identity instead of Windhawk's manager-process identity, so session-policy equivalence is partial. Cache entries are accepted only when image identity matches and every offset points to executable memory within the loaded image; validated online entries are saved locally. Windhawk's error-throttling protocol, hybrid-module behavior, and full hook-session semantics are not implemented. DbgHelp has process-global state and may conflict with other symbol-engine users; these helpers do not guarantee that every private symbol can be found. Use an empty `symbolServer` in `WH_FIND_SYMBOL_OPTIONS` to suppress symbol-server retrieval in tests. Do not treat a mod as portable merely because it compiles against the helper header. Native mods can also use Windows APIs and their own third-party hook frameworks today, subject to the target allow-list and architecture; the app does not manage those frameworks.

## Safety and recovery

Native mods are arbitrary machine code and are not sandboxed. Loading them can crash or hang their target process. The UI requires a second confirmation before enabling a mod and states the target names before install. Test new DLLs in a VM or with a recoverable Windows user profile. If Explorer becomes unstable, use Task Manager to restart Windows Explorer or launch 00047 and disable the offending mod. Mods are not loaded into arbitrary user-selected processes.

The engine does not yet provide a stable cross-process host callback API or guaranteed recovery from a DLL that crashes or permanently hangs a target after successful initialization. The in-app builder invokes the installed CMake/Visual Studio toolchain and packages the result; project build scripts can execute arbitrary commands with the current user's permissions and are not sandboxed. Optional Authenticode publisher-trust enforcement is available, but it is not a sandbox. The current x64 allow-list and explicit enable/disable flow are first-stage restrictions, not a security boundary.


## Porting a Windhawk mod

Use [WINDHAWK_COMPATIBILITY.md](WINDHAWK_COMPATIBILITY.md) and `tools/audit_windhawk_mod.py` to identify supported API calls, unsupported APIs, and lifecycle callbacks that require manual adaptation. Hooking is implemented through a bundled x64 MinHook backend, but the source-level subset is not a drop-in Windhawk runtime. The audit is static source analysis only and does not certify compatibility or safety.


## Windhawk lifecycle adapter

For source ports that use the supported API subset, `sdk/native-mod/windhawk_nonhook_adapter.h` offers an opt-in mapping from the common `Wh_ModInit`, `Wh_ModAfterInit`, `Wh_ModBeforeUninit`, and `Wh_ModUninit` callbacks to the versioned `Wmod*` ABI. With this adapter, queued hooks are applied after initialization and tracked hooks are removed after the before-uninit callback and before DLL unload. Generated CMake projects compile the hook shim and vendored MinHook sources automatically. See [WINDHAWK_COMPATIBILITY.md](WINDHAWK_COMPATIBILITY.md) for details.

The bundled implementation provides x64 function-hook registration/application, bounded instruction decoding, and `Wh_HookSymbols` resolution through a local cache, HTTPS online-cache fallback, and DbgHelp on cache misses. It is not full runtime compatibility. Hook-operation queues are per mod DLL; the shared registry rejects duplicate or conservatively overlapping entry targets across mods and prevents unloading a module targeted by another mod's hook, but does not coordinate arbitrary executable-memory patches or full hook-session ordering. Cache registry/error-throttling and hybrid-module behavior remain unsupported, and disassembly formatting can differ from Windhawk's Zydis-based output. During unload, ABI v2's `WmodCanUnloadV1` query checks tracked hook removals, shared-registry references, suspended target-thread instruction pointers, and candidate return addresses inside the DLL. If quiescence is uncertain, the host retains the DLL. This scan is defensive but cannot prove safety for all custom fibers or saved execution contexts, so hook-enabled mods remain experimental and should be tested in a disposable environment. The optional settings-change adapter supports both `void Wh_ModSettingsChanged(void)` and `BOOL Wh_ModSettingsChanged(BOOL* bReload)`: it watches the installed package directory for changes to `settings.json` and invokes the callback on a dedicated worker thread. When the BOOL form successfully requests reload, the manager retries a safe shutdown/unload/reload during its next enabled-mod scan. This is not guaranteed to match Windhawk callback timing or thread affinity; callback code must be thread-safe and must not assume a UI or shell thread. The adapter waits for the watch to be armed before reporting initialization success and stops/joins the worker before shutdown callbacks. The BOOL callback form requests a full-image reload through the optional `WmodIsReloadRequestedV1` export. The host does not force an unload: it waits for the safety query, and if quiescence cannot be proven, it retains the mapped DLL and retries on a later scan. Symbol lookup is a bounded DbgHelp-backed compatibility subset, not a guarantee of full Windhawk symbol behavior. Use `tools/audit_windhawk_mod.py` to find known porting blockers before compiling; review all unknown API calls manually. Never treat a successful static audit as proof that a mod is safe or behaviorally equivalent.
