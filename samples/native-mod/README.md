# 00047 sample native mod

This is a deliberately non-invasive x64 DLL sample. It only writes debug output when loaded and unloaded; it does not patch Explorer or modify the taskbar.

## Build

Use a Visual Studio x64 C++ toolchain and CMake:

    cmake -S samples/native-mod -B build -A x64
    cmake --build build --config Release

The sample DLL is produced under build/Release/00047_sample_native_mod.dll. The compiled binary is intentionally not committed.

## Package and validate

From the repository root, run:

    python tools/package_native_mod.py --manifest samples/native-mod/manifest.json --dll build/Release/00047_sample_native_mod.dll --output dist/00047-sample-native-mod.zip
    python tools/validate_native_mod.py dist/00047-sample-native-mod.zip

The builder puts manifest.json at the ZIP root and copies the DLL to the module path declared in the manifest. It validates the completed archive before publishing the output ZIP. The validator can also be used on ZIPs produced by other tools.

## Install and verify

1. Install the ZIP from 00047 → Native Mods.
2. Use the preview to inspect target processes and required exports.
3. Enable the mod only if you understand and trust its code.
4. Inspect DebugView or another debugger's output for the two sample messages.
5. Disable the mod to test its shutdown export.

## ABI v2

The module must export exact x64 names:

- WmodGetApiVersion() returning 2
- WmodInitializeV1() returning non-zero on success
- WmodShutdownV1() called before unload
- WmodIsInitializedV1() reporting whether initialization is active
- WmodCanUnloadV1() reporting whether tracked hooks and detectable in-flight module references have cleared

The SDK's x64 hook compatibility layer uses MinHook; generated projects and this sample compile it into the DLL, but no hooks are installed by this example. The host requires WmodCanUnloadV1() to approve unload and leaves a DLL mapped if quiescence cannot be established. This is a best-effort safety check, not a universal proof for all custom fibers or saved execution contexts. Crashes in native mods can still crash or hang the target process; use a VM or disposable test environment for new mods.
