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

The v1 API is a lifecycle ABI, not a broad host-services API. Mods that need settings UIs, host log routing, structured configuration, hot reload, or a shared cross-process hook framework will need additional SDK work. Mods can use Windows APIs and bundle their own hook libraries today.

## Safety and recovery

Native mods are arbitrary machine code and are not sandboxed. Loading them can crash or hang their target process. The UI requires a second confirmation before enabling a mod and states the target names before install. Test new DLLs in a VM or with a recoverable Windows user profile. If Explorer becomes unstable, use Task Manager to restart Windows Explorer or launch 00047 and disable the offending mod. Mods are not loaded into arbitrary user-selected processes.

This engine does not yet have digital-signature trust enforcement, a stable cross-process host callback API, a compiler/build service, or guaranteed recovery from a DLL that crashes a target after successful initialization. The current x64 allow-list and explicit enable/disable flow are first-stage restrictions, not a sandbox.
