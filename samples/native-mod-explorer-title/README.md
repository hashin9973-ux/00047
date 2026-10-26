# Explorer Title Prefix (Sample)

This is a real, narrowly scoped native-mod example for the 00047 SDK. When enabled for explorer.exe, it hooks user32!SetWindowTextW and prefixes captions for top-level windows whose class is CabinetWClass or ExploreWClass with [00047] . It also applies the prefix to existing matching windows during initialization.

When the mod shuts down, it tries to restore the last unprefixed title tracked for each window. Window messages use a bounded 250 ms timeout so an unresponsive window does not indefinitely hold up cleanup. A title may remain prefixed if Windows refuses the restoration message; the mod logs that case instead of blocking indefinitely.

## Build

Build from a Windows x64 Visual Studio Developer environment with CMake installed:

    cmake -S . -B build -A x64
    cmake --build build --config Release

The DLL is produced in build/bin/Release/00047_explorer_title_prefix_mod.dll. The SDK path defaults to the repository's sdk/native-mod directory; set WMOD_SDK_DIR explicitly if the sample is copied elsewhere.

Package and validate it from the repository root:

    python tools/package_native_mod.py --manifest samples/native-mod-explorer-title/manifest.json --dll samples/native-mod-explorer-title/build/bin/Release/00047_explorer_title_prefix_mod.dll --output dist/explorer-title-prefix.zip
    python tools/validate_native_mod.py dist/explorer-title-prefix.zip

The optional WMOD_BUILD_TESTS CMake option builds a synthetic-window runtime test. It verifies the actual hook against a test-process window registered with the same window class; it does not launch or modify the live Windows Explorer process.

## Safety

Native mods run unsandboxed inside the target process. This sample patches a User32 API entry point in explorer.exe and can conflict with other hooks or cause Explorer instability. It is disabled until the user explicitly enables the installed package. Test it in a VM or recoverable Windows profile. To recover from a problem, restart Explorer and disable the mod in 00047.
