# Windhawk source compatibility audit

The audit tool helps estimate whether a C/C++ Windhawk mod can be ported to the current 00047 native-mod SDK. It does not execute the source or prove safety, ABI equivalence, or correct behavior.

## Run it

From the repository root:

```powershell
python tools/audit_windhawk_mod.py path\to\mod.cpp path\to\include
python tools/audit_windhawk_mod.py path\to\mod --json
python tools/audit_windhawk_mod.py path\to\mod --fail-on-unsupported
```

The scanner reads C/C++ source/header files, masks comments and ordinary/raw string literals, and reports the source locations where `Wh_*` identifiers occur. It excludes common build-output folders and the SDK compatibility header by default. Files over 2 MiB are skipped. `--fail-on-unsupported` returns exit code 2 when unsupported, unknown, lifecycle-adaptation, or warning findings exist, so it can be used in a porting checklist or CI.

## Supported subset

The current source-level helpers cover `Wh_Log`, `Wh_GetIntSetting`, `Wh_GetStringSetting`, `Wh_FreeStringSetting`, scalar/string/binary local-storage APIs, `Wh_DeleteValue`, `Wh_GetModStoragePath`, dynamic-select option management, and bounded HTTP(S) retrieval through `Wh_GetUrlContent` / `Wh_FreeUrlContent`, and bounded `Wh_FindFirstSymbol` / `Wh_FindNextSymbol` / `Wh_FindCloseSymbol` enumeration using Windows DbgHelp.

“Supported with limitations” deliberately does not mean binary compatibility. Storage is package-scoped; URL retrieval is bounded and rejects credential-bearing URLs; settings use 00047's manifest/settings files; and runtime callbacks are different. Symbol enumeration is a DbgHelp-backed snapshot capped at 100,000 records and approximately 4 million stored name characters, depends on matching PDB/symbol availability, and may perform network I/O when a symbol server is selected. DbgHelp state is process-global, so lookup can fail or conflict when another component initialized DbgHelp incompatibly. Use an empty `symbolServer` in tests to suppress remote retrieval. Review [NATIVE_MODS.md](NATIVE_MODS.md) before porting.

## Unsupported runtime APIs

The current SDK does not implement these Windhawk runtime operations:

- `Wh_SetFunctionHook`, `Wh_RemoveFunctionHook`, and `Wh_ApplyHookOperations`.
- `Wh_Disasm`.

A detected call to one of these APIs is a blocker, not a warning that can be ignored. Rewrite the mod around public Windows APIs or an independently supported alternative, or use an actual compatible Windhawk runtime. The auditor intentionally does not replace missing runtime implementations with no-op stubs.

## Lifecycle migration

For non-hook mods that provide the usual init and shutdown callbacks, include the opt-in header `sdk/native-mod/windhawk_nonhook_adapter.h` in the mod translation unit and place the macro invocation after the callback definitions. The header maps `Wh_ModInit`, `Wh_ModAfterInit`, `Wh_ModBeforeUninit`, and `Wh_ModUninit` onto 00047's `WmodGetApiVersion`, `WmodInitializeV1`, and `WmodShutdownV1` ABI. Define `WMOD_BUILD_DLL`, include the adapter header after the callback definitions, and ensure the callback signatures match the adapter contract:

```cpp
WMOD_DEFINE_WINDHAWK_LIFECYCLE(
    Wh_ModInit,
    Wh_ModAfterInit,
    Wh_ModBeforeUninit,
    Wh_ModUninit
)
```

The adapter guards repeated initialization and shutdown, and a failed init does not trigger the shutdown callbacks. Callback ordering is mapped, but timing remains different; the adapter is not a Windhawk runtime. If a port has no implementation for a void callback, `wmod::windhawk_adapter::NoopCallback` can be supplied explicitly. Do not use the adapter to hide missing behavior. The mod still must be free of unsupported hook and disassembly APIs and its settings/storage calls must be ported to the supported helpers.

`Wh_ModSettingsChanged` is **not** called automatically: 00047 does not yet provide an equivalent settings-change callback protocol. Any reload/update behavior needs a deliberate alternative. Review the mod's assumptions about callback timing, exceptions, threading, and cleanup before enabling it in a shell process.

## CI

Run the audit-tool unit tests with the rest of the developer-tool tests:

```powershell
python -m unittest discover -s tools/tests -v
```

Official reference: [Windhawk mods_api.h](https://github.com/ramensoftware/windhawk/blob/main/src/windhawk/engine/mods_api.h). The API names and docs can evolve; treat this compatibility document and the versioned 00047 SDK as the local implementation contract.
