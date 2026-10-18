# Extension host (first milestone)

00047 distinguishes workflow block packs from independent extension packages. Block packs add conditions and actions to automation workflows. Extension packages are managed from the Extensions view and can launch applications or long-running services without requiring an automation node.

## Package layout

Create a ZIP containing a root-level manifest.json and any scripts, executables, libraries, or assets it needs. For example:

    my-extension.zip
    ├── manifest.json
    ├── scripts/
    │   └── worker.py
    └── runtime/
        └── any-bundled-runtime.exe

The manifest is version 1:

    {
      "manifestVersion": 1,
      "id": "com.example.desktop-helper",
      "name": "Desktop Helper",
      "version": "1.0.0",
      "description": "Example independently managed extension.",
      "components": [
        {
          "id": "helper",
          "name": "Background helper",
          "kind": "service",
          "description": "Runs independently of any automation workflow.",
          "entrypoint": "scripts/worker.py",
          "runtime": {
            "command": "python",
            "args": ["{entrypoint}"],
            "workingDirectory": "{extensionDir}",
            "environment": { "HELPER_MODE": "desktop" }
          }
        }
      ]
    }

Component kind is currently app or service. Both are launched and tracked by 00047; the distinction is whether the component is intended as a user-facing application or a long-running helper. Use any executable or interpreter available on PATH, or bundle an executable in the ZIP and refer to it through the extensionDir placeholder. For script runtimes, use the entrypoint placeholder. Arguments are passed as separate OS arguments; they are not concatenated into a shell command.

Supported placeholders are {extensionDir}, {entrypoint}, and {componentId}. The process also receives EXTENSION_DIR, EXTENSION_ENTRYPOINT, and EXTENSION_COMPONENT_ID, plus any variables declared in runtime.environment.

## Install and manage

1. Open 00047 → Extensions → Install ZIP extension….
2. Inspect the package and displayed runtime command.
3. Install the ZIP.
4. Use Start, Stop, or Restart in the Extensions view to control components.

The importer enforces a 20 MB compressed ZIP limit, a 50 MB expanded limit, at most 500 ZIP entries, and a 1 MB manifest limit. It rejects absolute/traversal paths, duplicate entries, and symbolic links. Reinstalling a package ID replaces that package after staging a validated copy. Stop running components before updating a package.

## Security model

Extension code is not sandboxed. It runs under the current Windows user and can access files, network resources, launch programs, and change user-level settings the account can access. Review source, manifests, and commands before installation. This is a developer/extensibility system, not a trust boundary.

## Capability boundary and roadmap

This milestone adds independently managed external applications and services. It does not claim native process modification or Windhawk-equivalent DLL injection. Those capabilities require a separate Windows-specific mod engine with process targeting, architecture-aware native modules, a mod SDK/API, crash recovery, and compatibility handling across Windows updates.

The runtime can launch any language for which an executable/interpreter is available or packaged. In-process native extensions and mods that alter Explorer, Start, the taskbar, or notification processes remain a follow-on engine milestone rather than a pretend feature in this package manager.
