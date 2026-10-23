# Windhawk source compatibility audit

The audit tool helps estimate whether a C/C++ Windhawk mod can be ported to the current 00047 native-mod SDK. It does not execute the source or prove safety, ABI equivalence, or correct behavior.

## Run it

From the repository root:

```powershell
python tools/audit_windhawk_mod.py path\to\mod.cpp path\to\include
python tools/audit_windhawk_mod.py path\to\mod --json
python tools/audit_windhawk_mod.py path\to\mod --fail-on-unsupported
```

The scanner reads C/C++ source/header files, masks comments and ordinary/raw string literals, and reports the source locations where `Wh_*` identifiers occur. It excludes common build-output folders and the SDK compatibility header by default. Files over 2 MiB are skipped. `--fail-on-unsupported` returns exit code 2 when unsupported, unknown, lifecycle-adaptation, or warning findings exist, so it can be used in a porting checklist or CI. `--sarif` emits SARIF 2.1.0 with source locations and severity levels for code-scanning integrations. `--output` writes JSON/SARIF to a file without mixing diagnostic text into the machine-readable report. Paths inside the current working tree are emitted as repository-relative URIs; external files use `file:` URIs and are not suitable for repository-hosted code-scanning uploads without remapping. The report describes static compatibility findings and does not certify runtime safety or ABI compatibility.

## Supported subset

The source-level helpers also provide x64 function-hook registration/application through the bundled MinHook backend, limited instruction-length/disassembly reporting through HDE64, and a bounded `Wh_HookSymbols` implementation that resolves exact decorated/undecorated names through DbgHelp. The non-hook helpers cover `Wh_Log`, `Wh_GetIntSetting`, `Wh_GetStringSetting`, `Wh_FreeStringSetting`, scalar/string/binary local-storage APIs, `Wh_DeleteValue`, `Wh_GetModStoragePath`, dynamic-select option management, bounded HTTP(S) retrieval through `Wh_GetUrlContent` / `Wh_FreeUrlContent`, and bounded `Wh_FindFirstSymbol` / `Wh_FindNextSymbol` / `Wh_FindCloseSymbol` enumeration using Windows DbgHelp. `Wh_ModSettingsChanged` can be wired through a separate opt-in lifecycle adapter.

“Supported with limitations” deliberately does not mean binary compatibility. Storage is package-scoped; URL retrieval is bounded and rejects credential-bearing URLs; settings use 00047's manifest/settings files; and runtime callbacks are different. Symbol enumeration is a DbgHelp-backed snapshot capped at 100,000 records and approximately 4 million stored name characters, depends on matching PDB/symbol availability, and may perform network I/O when a symbol server is selected. DbgHelp state is process-global, so lookup can fail or conflict when another component initialized DbgHelp incompatibly. Use an empty `symbolServer` in tests to suppress remote retrieval. Review [NATIVE_MODS.md](NATIVE_MODS.md) before porting.

## Implemented hook and symbol APIs: limitations

The SDK now provides source-level implementations of `Wh_SetFunctionHook`, `Wh_RemoveFunctionHook`, and `Wh_ApplyHookOperations` using the bundled x64 MinHook backend. Registration and removal are queued; the opt-in lifecycle adapter applies pending hooks after `Wh_ModInit` and removes tracked hooks after `Wh_ModBeforeUninit`. `Wh_Disasm` uses the vendored diStorm x64 decoder and Intel-syntax formatter, returning the decoded length, mnemonic, and operands instead of a mnemonic-only byte dump. Formatting can still differ from Windhawk's Zydis-based output for edge-case instructions. `Wh_HookSymbols` matches a selector group against the local DbgHelp snapshot, then queues matching detours through `Wh_SetFunctionHook`; it does not implement Windhawk's persistent symbol cache, online-cache URL behavior, hybrid-module handling, or full hook-session semantics.

These APIs are supported with limitations, not a claim of full runtime equivalence. Each DLL has its own hook-operation queue, while a named per-process registry serializes target claims and rejects duplicate hook target addresses across independently linked mods. The registry also prevents unloading a module while another registered hook targets an address inside it. The backend is x64-only and expects a valid executable target address; overlapping-but-distinct entry hooks and full Windhawk hook-session behavior are not coordinated. The ABI v2 `WmodCanUnloadV1` query is wired into the host's unload path. The hook backend disables/removes tracked hooks and checks shared-registry references, suspended target-thread instruction pointers, and stack words for references into the module; if it cannot confirm quiescence within the host timeout, the DLL is retained instead of forcibly unloaded. This is a best-effort Windows stack scan, not a universal proof for custom fibers, saved continuations, or other control-flow state not represented on inspected thread stacks. Treat hook-enabled mods as experimental and test in a disposable environment. The opt-in settings adapter also supports `BOOL Wh_ModSettingsChanged(BOOL* bReload)`. A successful callback that sets `*bReload = TRUE` exports a reload request; the host observes it on the next enabled-mod scan and performs shutdown, quiescence validation, unload, and a fresh load. If quiescence cannot be proven, the DLL remains mapped and the host retries on a later scan rather than forcing unload. Unknown API calls remain blockers. A passing source audit cannot certify ABI equivalence, mod behavior, or safety.

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

The adapter guards repeated initialization and shutdown, applies queued hooks after the init callback, and removes tracked hooks between the before-uninit and uninit callbacks. Callback ordering is mapped, but timing remains different; the adapter is not a Windhawk runtime. If a port has no implementation for a void callback, `wmod::windhawk_adapter::NoopCallback` can be supplied explicitly. Do not use the adapter to hide missing behavior. The mod must still be checked for unsupported APIs and manual lifecycle needs. Hook calls must be linked against the bundled SDK sources, and settings/storage calls must use the supported helpers. The shared registry rejects exact duplicate target addresses, but overlapping hook entries and exact Zydis disassembly formatting remains a limitation.

Mods using either `void Wh_ModSettingsChanged(void)` or `BOOL Wh_ModSettingsChanged(BOOL* bReload)` can use `sdk/native-mod/windhawk_settings_changed_adapter.h` and `WMOD_DEFINE_WINDHAWK_LIFECYCLE_WITH_SETTINGS_CHANGED` instead of the basic lifecycle macro. It watches the mod package directory for changes to `settings.json` and invokes the callback on a dedicated worker thread, then stops and joins that thread before shutdown callbacks. Thread affinity differs from Windhawk: the callback must be thread-safe, brief, and not assume UI/shell-thread affinity. Initialization fails if the directory cannot be watched. For the `BOOL(BOOL* bReload)` variant, the callback's return value indicates whether settings handling succeeded; only a successful callback with `*bReload = TRUE` requests reload. The host polls that request during its next enabled-mod scan (normally within the five-second scan interval), then shuts down the module, verifies quiescence, unloads it, and maps a fresh image. The request remains set while a DLL is retained for safety so a later scan can retry. The static auditor marks this callback as adapter-available, not fully Windhawk-identical; callback thread affinity and timing still differ. Review the mod's assumptions about callback timing, exceptions, threading, and cleanup before enabling it in a shell process.

## Native DLL publisher trust

The 00047 application can optionally require Trusted Authenticode signatures for native mods. This policy is OFF by default. When enabled, load-time verification calls Windows `WinVerifyTrust` using cached certificate/revocation data only; missing or invalid trust data fails closed, and verification does not perform network retrieval. The UI exposes the policy switch, persists it, disables all installed packages that do not verify (including ones previously marked disabled), and records unload retries durably. Pending cleanup is retried during background scans and blocks re-enable or package replacement until safe unload succeeds. The loader never forces unload when hook quiescence is uncertain. Signed code is still native unsandboxed code and should be reviewed.

## CI

Run the audit-tool unit tests with the rest of the developer-tool tests:

```powershell
python -m unittest discover -s tools/tests -v
```

Official reference: [Windhawk mods_api.h](https://github.com/ramensoftware/windhawk/blob/main/src/windhawk/engine/mods_api.h). The API names and docs can evolve; treat this compatibility document and the versioned 00047 SDK as the local implementation contract.
