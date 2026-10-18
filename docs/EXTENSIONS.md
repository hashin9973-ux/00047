# General extension host

00047 now has two distinct extension paths:

- **Runtime extensions** are standalone processes launched by the app. They can be scripts, command-line tools, compiled executables, or a bundled interpreter/runtime.
- **Automation block packs** add reusable conditions/actions to the visual automation editor. They remain supported separately.

Runtime extensions are managed in **Extensions → Installed runtime extensions**. Packages are installed under the app data directory at `extensions/packages/<id>/`.

## Package format (version 1)

A ZIP has `extension.json` at its root and the files referenced by the manifest. Do not add an enclosing folder.

```text
hello-extension.zip
├── extension.json
└── scripts/
    └── main.js
```

Example manifest:

```json
{
  "manifestVersion": 1,
  "id": "org.example.hello",
  "name": "Hello extension",
  "version": "1.0.0",
  "description": "A long-running JavaScript helper process.",
  "entrypoint": "scripts/main.js",
  "runtime": {
    "command": "node",
    "args": ["{entrypoint}"],
    "env": {
      "LOG_LEVEL": "info"
    }
  },
  "autoStart": false
}
```

Required fields:

- `manifestVersion`: currently `1`.
- `id`: unique lowercase ID. Letters `a-z`, digits, dots, dashes, and underscores are accepted. The ID is also the installed directory name.
- `name`, `version`: metadata shown in the manager.
- `entrypoint`: safe relative path to a file packaged in the ZIP.
- `runtime.command`: executable or interpreter to run.
- `runtime.args`: argument array. Arguments are passed separately to the process; 00047 does not concatenate them into a shell command.
- `runtime.env`: optional environment variables.
- `autoStart`: whether to launch the extension when 00047 starts. This can be changed in the Extensions view.

### Runtime placeholders and environment

The runtime command, arguments, and environment values support these placeholders:

- `{extensionDir}`: absolute path to the installed extension directory.
- `{entrypoint}`: absolute path to the manifest entrypoint.
- `{extensionId}`: extension ID.

Every process receives `EXTENSION_ID`, `EXTENSION_DIR`, and `EXTENSION_ENTRYPOINT`. Manifest environment variables are also passed to the process.

A runtime can be an interpreter available on PATH (`python`, `node`, `pwsh`, `ruby`, etc.), a compiled program, or a runtime packaged inside the ZIP. For a bundled executable, set `runtime.command` to a path such as `{extensionDir}/runtime/runner.exe`. The package author is responsible for bundling the correct runtime for the target OS/architecture and for all required libraries.

The extension host captures standard output and standard error and displays process lifecycle status. Start, stop, restart, install, and uninstall operate on the managed process/package, not on arbitrary unrelated processes.

## Package limits and validation

The installer currently enforces:

- 20 MB maximum compressed ZIP.
- 50 MB maximum uncompressed size.
- 500 ZIP entries maximum.
- 1 MB maximum `extension.json`.
- Safe relative paths, no duplicate entries, and no symbolic links.

These checks protect the extraction process; they are not a security sandbox or malware scanner.

## Trust and security

A runtime extension is arbitrary code execution. It runs as the signed-in Windows user and can use any permissions available to that account, including accessing files, using the network, launching programs, and calling Windows APIs. Only install code you trust. Autostart is opt-in per package and can be disabled in the app.

The host starts a separate process. It does **not** yet provide a stable in-process native module ABI, inject DLLs into Explorer/Start/notification processes, or implement Windhawk's mod/hook system. Those features require a separate native-extension layer with architecture checks, a compatibility contract, safe enable/disable and recovery paths, and careful validation against Windows versions. This version is the runtime/lifecycle foundation for that work, not Windhawk parity.

## Development status

The extension manager and block-pack manager are intentionally separate. A future native-mod API should not be forced into an automation-node model. The planned architecture is:

1. Process extensions: language/runtime-agnostic execution and lifecycle management (implemented in this phase).
2. Host API: versioned IPC for commands, events, settings, and custom UI contributions.
3. Native Windows modules: separately reviewed in-process APIs and, if required, controlled target-process integration for shell customization.

Native target-process modifications can destabilize Explorer and the desktop. That work should include per-mod isolation where possible, explicit opt-in, Windows/build compatibility checks, and a documented recovery path.
