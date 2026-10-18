# External extension packages and runtime

00047 distinguishes workflow block packs from independent extension packages. Block packs add conditions and actions to automation workflows. Extension packages are managed from the Extensions view and can launch applications, long-running services, or one-shot commands without requiring an automation node.

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

Component kind is `app`, `service`, or `command`. Apps and services are launched and tracked as persistent processes; the distinction is whether the component is intended as a user-facing application or a long-running helper. A `command` component runs only when the user presses **Run** and returns the exit status, standard output, standard error, elapsed time, and timeout status to 00047. It does not stay in the process list and cannot be started or restarted as a service.

Use any executable or interpreter available on PATH, or bundle an executable in the ZIP and refer to it through the extensionDir placeholder. For script runtimes, use the entrypoint placeholder. Arguments are passed as separate OS arguments; they are not concatenated into a shell command. Commands have a 15-second default timeout. Set `runtime.timeoutMs` to a value from 100 to 120000 for a command component; it is rejected on apps/services. Standard output and standard error are each capped at 256 KiB, with excess drained but not stored, and the UI further limits displayed output to 12000 characters per stream.

A one-shot command component can omit `entrypoint` and provide separate runtime arguments:

    {
      "id": "verify-config",
      "name": "Verify configuration",
      "kind": "command",
      "runtime": {
        "command": "python",
        "args": ["-c", "print('configuration check passed')"],
        "workingDirectory": "{extensionDir}",
        "timeoutMs": 10000
      }
    }

The default timeout is 15000 ms; a declared `timeoutMs` must be between 100 and 120000 ms. The standard-output and standard-error captures are each limited to 256 KiB. The app can terminate the direct command process at timeout but does not sandbox it or guarantee control over separately detached descendants.

Supported placeholders are {extensionDir}, {entrypoint}, and {componentId}. The process also receives EXTENSION_DIR, EXTENSION_ENTRYPOINT, and EXTENSION_COMPONENT_ID, plus any variables declared in runtime.environment.

## Install and manage

1. Open 00047 → Extensions → Install ZIP extension….
2. Inspect the package and displayed runtime command.
3. Install the ZIP.
4. Use Start, Stop, or Restart for apps/services. Use Run for a one-shot command and review its status/output dialog.

The importer enforces a 20 MB compressed ZIP limit, a 50 MB expanded limit, at most 500 ZIP entries, and a 1 MB manifest limit. It rejects absolute/traversal paths, duplicate entries, and symbolic links. Reinstalling a package ID replaces that package after staging a validated copy. Stop running components before updating a package.

## Security model

Extension code is not sandboxed. It runs under the current Windows user and can access files, network resources, launch programs, and change user-level settings the account can access. Review source, manifests, and commands before installation. This is a developer/extensibility system, not a trust boundary.

## Authoring tools

The repository includes a standard-library-only scaffold, deterministic packager, and validator for extension authors. From the repository root:

    python tools/new_extension.py com.example.status-check "Status Check" --kind command --out extensions
    python tools/package_extension.py extensions/com.example.status-check dist/status-check.zip
    python tools/validate_extension.py dist/status-check.zip

Use `--kind service` to scaffold a persistent helper instead. The command template uses a 15-second timeout and exits after printing its result; the service template is a minimal long-running loop. The packager sorts files and fixes ZIP metadata so the same project produces reproducible package bytes, then validates the generated ZIP before replacing the destination file. The validator checks archive size/paths/CRC, Windows path collisions and file-directory conflicts, manifest fields, placeholders, entrypoints, and command timeout bounds.

Run author-tool regression tests with:

    python -m unittest discover -s tools/tests -v

These tools complement the install-time validator; they do not replace it or make extension code trusted.

## Capability boundary

The external extension host is separate from the Windows **Native Mods** subsystem. It does not load DLLs into Explorer or other shell processes. See [NATIVE_MODS.md](NATIVE_MODS.md) for the native-mod package contract, fixed target allow-list, SDK, settings, logs, and recovery limitations. The runtime can launch any language for which an executable/interpreter is available or packaged.

External commands and services are not sandboxed: timeouts and bounded output prevent some resource-exhaustion cases, but they do not restrict file, network, or process access under the current user's permissions.
