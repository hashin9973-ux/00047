# Script plugins for 00047

> **Runtime extensions are separate from block packs.** If you need a resident helper process, a script or executable that is not tied to an automation node, or a packaged runtime in any language, see [EXTENSIONS.md](EXTENSIONS.md). Block packs remain the right format for reusable conditions and actions inside an automation.

The recommended extension format is a **ZIP package with a root-level `manifest.json` and the script files it references**. JSON is only the manifest: the actual logic lives in scripts or executables inside the ZIP.

## ZIP layout

Create a ZIP with this layout. Do not include an extra enclosing folder; `manifest.json` must be at the ZIP root.

```text
file-tools.zip
├── manifest.json
└── scripts/
    └── file_exists.py
```

A complete example is in `docs/examples/script-plugin/`. Zip the *contents* of that directory to create the package.

## Manifest format, version 1

```json
{
  "manifestVersion": 1,
  "id": "com.example.file-tools",
  "name": "Example file tools",
  "version": "1.0.0",
  "description": "Example Python-based condition",
  "blocks": [
    {
      "kind": "user.file_exists",
      "category": "condition",
      "title": "File exists",
      "icon": "file",
      "description": "True if the selected path exists.",
      "entrypoint": "scripts/file_exists.py",
      "runtime": {
        "command": "python",
        "args": ["{entrypoint}"]
      },
      "fields": [
        { "key": "path", "label": "Path", "type": "text", "required": true, "maxLen": 1000 },
        { "preset": "onFail" }
      ],
      "summary": "Exists: {path}"
    }
  ]
}
```

Top-level properties:

- `manifestVersion`: currently `1`.
- `id`: unique package ID using lowercase letters, digits, dots, dashes or underscores. It is used as the installed package folder name.
- `name` and `version`: human-readable package metadata.
- `blocks`: 1–100 block definitions.

Each block uses the normal 00047 block UI metadata: `kind` (must start with `user.`), `category` (`condition` or `action`), `title`, optional `icon` and `description`, `fields`, and optional `summary`. Field types are `text`, `file`, `number` (with numeric `min` and `max`), `select` (with `options` such as `[["fast","Fast"]]`), `bool`, and `time`. Preset fields like `{"preset":"timeout"}` and `{"preset":"onFail"}` are also supported.

### Runtime and entrypoint

- `entrypoint`: a required path relative to the ZIP root. Use forward slashes and do not use `..`, absolute paths, or backslashes.
- `runtime.command`: the executable/interpreter to launch. It can be any executable available on the machine (for example `python`, `node`, `ruby`, `lua`, `pwsh`, `bash`, or a custom executable included in the package).
- `runtime.args`: an array of strings passed as separate OS arguments. Use `"{entrypoint}"` to pass the entrypoint to the runtime, e.g. `["{entrypoint}"]` for Python or Node, or `["-File", "{entrypoint}"]` for PowerShell.
- `{pluginDir}` resolves to the installed plugin directory. For a runtime shipped inside a package, for example, `"command": "{pluginDir}/runtime/runner.exe"` can invoke it directly.
- `{fieldKey}` placeholders in `runtime.command` or `runtime.args` refer to fields in that block. The field's value is also exposed as an environment variable named `FIELD_<KEY>` in uppercase (for example `FIELD_PATH`). The runtime also receives `PLUGIN_DIR` and `PLUGIN_ENTRYPOINT`.
- The plugin runs with its directory as the working directory. Arguments are passed separately, not concatenated into a shell command.

A script reports a Condition result via its exit code: `0` means satisfied and any other code means not satisfied. For Actions, exit code `0` means success and non-zero means failure. stdout/stderr are captured by the current block runner where available. Use the usual `summary`/field schema to configure the editor.

**This is an open runtime contract, not a list of built-in languages.** 00047 does not need to understand Python, JavaScript, Ruby, Lua, or another language; it invokes the `runtime.command` you declare. The interpreter must be installed and discoverable on PATH unless the ZIP provides a runnable interpreter/executable and the manifest points to it. Language package dependencies must also be installed or included by the plugin author.

## Importing

In 00047, open **Settings → Custom blocks → Install ZIP plugin…**. The app previews each block and runtime command before installation. After installation, choose **Reload (restarts 00047)** to add the blocks to the palette. The installed package is extracted under the app's custom-blocks folder in `packages/<id>/`.

The importer enforces a 20 MB compressed ZIP limit, a 50 MB unpacked limit, at most 500 ZIP entries, and a 1 MB manifest limit. It rejects unsafe paths, duplicate ZIP entries, and symbolic links. The app continues to load older JSON custom-block definitions already in the custom-blocks folder for compatibility, but new plugins should use the ZIP manifest format.

## Security and permissions

A script plugin is **arbitrary code execution**, not a sandbox. Scripts and executables run as the current Windows user and can do anything that account is allowed to do, including read/write files, access the network, launch programs, and modify user-level settings. Administrator-only operations still require elevation. Review the manifest and source files before installing a package; the command preview cannot explain what every line of a script does. Do not install unknown ZIPs.

Imported automations cannot install plugins. Plugin code should not try to import or install other packages automatically.

## Native blocks (Rust)

Native blocks are still appropriate when the feature needs direct access to 00047 internals. Implement the `Block` trait in `src-tauri/src/blocks.rs`, register it in `registry()`, and use `NodeCx` for cancellation, reporting, logging, and wired-target resolution. Native Rust blocks require a source change and app rebuild; script plugins are the normal path for user extensions.
