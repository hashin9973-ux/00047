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

The current source-level helpers cover `Wh_Log`, `Wh_GetIntSetting`, `Wh_GetStringSetting`, `Wh_FreeStringSetting`, scalar/string/binary local-storage APIs, `Wh_DeleteValue`, `Wh_GetModStoragePath`, dynamic-select option management, and bounded HTTP(S) retrieval through `Wh_GetUrlContent` / `Wh_FreeUrlContent`.

“Supported with limitations” deliberately does not mean binary compatibility. Storage is package-scoped; URL retrieval is bounded and rejects credential-bearing URLs; settings use 00047's manifest/settings files; and runtime callbacks are different. Review the implementation notes in [NATIVE_MODS.md](NATIVE_MODS.md) before porting.

## Unsupported runtime APIs

The current SDK does not implement these Windhawk runtime operations:

- `Wh_SetFunctionHook`, `Wh_RemoveFunctionHook`, and `Wh_ApplyHookOperations`.
- `Wh_FindFirstSymbol`, `Wh_FindNextSymbol`, and `Wh_FindCloseSymbol` for private/PDB symbol enumeration.
- `Wh_Disasm`.

A detected call to one of these APIs is a blocker, not a warning that can be ignored. Rewrite the mod around public Windows APIs or an independently supported alternative, or use an actual compatible Windhawk runtime. The auditor intentionally does not replace missing runtime implementations with no-op stubs.

## Lifecycle migration

Windhawk callbacks such as `Wh_ModInit`, `Wh_ModAfterInit`, `Wh_ModBeforeUninit`, `Wh_ModUninit`, and `Wh_ModSettingsChanged` require manual adaptation to 00047's `WmodGetApiVersion`, `WmodInitializeV1`, and `WmodShutdownV1` ABI. There is no automatic semantic conversion for settings-change callbacks.

## CI

Run the audit-tool unit tests with the rest of the developer-tool tests:

```powershell
python -m unittest discover -s tools/tests -v
```

Official reference: [Windhawk mods_api.h](https://github.com/ramensoftware/windhawk/blob/main/src/windhawk/engine/mods_api.h). The API names and docs can evolve; treat this compatibility document and the versioned 00047 SDK as the local implementation contract.
