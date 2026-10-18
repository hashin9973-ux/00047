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
  "autoStart": false,
  "protocol": "stdio-jsonl-v1"
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

### Optional lifecycle control protocol

For long-running extensions, set `protocol` to `stdio-jsonl-v1` to opt into a line-delimited JSON control channel over standard input. The host sends one JSON object per line:

- On start: `{"type":"host.started","protocolVersion":1,"extensionId":"org.example.hello"}`
- On stop: `{"type":"host.shutdown","protocolVersion":1,"extensionId":"org.example.hello"}`

An extension using this protocol should read stdin line-by-line, handle `host.shutdown` by unregistering hooks/subscriptions and restoring any temporary system state, then exit normally. The host waits up to three seconds before forcing termination. Without this property, stdin remains closed and stop terminates the child immediately. Standard output and standard error are reserved for log text in this protocol version.

This protocol controls an external process; it is not an in-process ABI and does not itself provide DLL injection, native hooks, or direct access to Explorer internals.

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

## In-process native modules (Windows)

In addition to external process extensions, the **In-process native modules** section accepts Windows DLLs that export the ABI in [`docs/extension_sdk/00047_native_extension.h`](extension_sdk/00047_native_extension.h). This interface can be implemented in C, C++, Rust, or any language capable of producing a compatible Windows DLL and C ABI exports.

The DLL exports `extension_00047_get_api_v1`, returning an ABI v1 descriptor with an ID, display metadata, and `start`/`stop` callbacks. The host passes a small versioned host API table with a logging callback. The host calls `stop` before unloading and during normal app shutdown; a nonzero stop result prevents normal unload because the module may still have active code.

The C++ example is in [`docs/examples/native-module/native_hello.cpp`](examples/native-module/native_hello.cpp). Build it as a DLL with the supplied header, then use the Extensions view to load it. Loaded modules are copied into the app's managed native-module folder. Auto-start is opt-in.

**Native DLLs execute inside the 00047 process. They are not sandboxed and can crash or corrupt 00047.** The host must load a DLL to inspect its ABI, so even the initial inspection executes its DLL entrypoint. Only load modules whose source you trust.

This ABI is an in-process extension surface, not full Windhawk parity. Loading a DLL into 00047 does not, by itself, load it into `explorer.exe`, `StartMenuExperienceHost.exe`, or notification processes. Target-process loading, native hook lifetimes, Windows-build compatibility checks, crash recovery, and a target-process IPC contract remain a distinct engineering layer.

## Development status

The extension manager, in-process native module manager, and automation block-pack manager are separate systems:

1. **External process extensions**: language/runtime-agnostic execution and lifecycle management.
2. **In-process native modules**: versioned C ABI for code that must run inside 00047 itself.
3. **Host API and UI contributions**: planned versioned IPC for commands, events, settings, and custom extension UI.
4. **Windows target-process modules**: a later subsystem for Explorer/Start/notification integration, with architecture and Windows-build compatibility checks, explicit opt-in, and a recovery path.

Target-process modifications can destabilize Explorer and the desktop. A complete implementation should not unload a module while its code is running, should restore hooks/state on disable, and should have a reliable way to recover if 00047 or a target process terminates unexpectedly.
