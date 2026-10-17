# Adding blocks to 00047

A block is a Condition (IF/ELSE) or an Action. Pick the route that fits.

## 1. Users: a block pack (JSON, no coding)

Use **Settings → Import actions / conditions…** to pick a block-pack `.json` file. The app previews
the commands/scripts before copying it. You can also open **Manage custom blocks…** to inspect installed blocks,
open the blocks folder, and choose **Reload** (00047 restarts) to activate imports.
An example pack is in `docs/examples/blocks/sample-pack.json`.

```json
{
  "kind": "user.open_url",
  "category": "action",
  "title": "Open URL",
  "icon": "play",                     // clock chip trim hourglass play repeat window file note proc stop bell
  "description": "Opens a web address.",
  "fields": [
    { "key": "url", "label": "Address", "type": "text", "required": true, "maxLen": 500 },
    { "preset": "timeout" },
    { "preset": "onFail" }
  ],
  "summary": "Open {url}",
  "run": { "type": "powershell", "script": "Start-Process $env:FIELD_URL" }
}
```

A file may hold one block, an array of blocks, or `{ "blocks": [ ... ] }`.

## Plugin packages

Use **Settings → Import plugin…** to pick a plugin-package `.json` file. A plugin package groups actions and conditions under a name, version, author, and ID. The app previews every command/script and asks for confirmation before installing. Installed plugin packs are stored separately in the app's `plugins` folder; choose **Reload** to activate or remove their blocks. A ready-to-import example is `docs/examples/plugins/sample-plugin.json`; it contains one weekday condition and one log-note action, and does not change files or system settings.

A plugin package has this shape:

```json
{
  "plugin": {
    "id": "sample.utilities",
    "name": "Sample Utilities",
    "version": "1.0.0",
    "author": "Your name",
    "description": "Example action and condition blocks"
  },
  "blocks": [
    {
      "kind": "user.sample_utilities.weekday",
      "category": "condition",
      "title": "Weekday",
      "icon": "clock",
      "description": "Satisfied Monday through Friday.",
      "fields": [],
      "run": {
        "type": "powershell",
        "script": "$day = (Get-Date).DayOfWeek.ToString(); if ($day -in @('Saturday','Sunday')) { exit 1 } else { exit 0 }"
      }
    }
  ]
}
```

The `plugin.id` must start with a lowercase letter and contain only lowercase letters, digits, dots, underscores, or hyphens. Each block still uses the same `user.*` kind, field, and run definitions as a normal block pack. A package must contain 1–200 valid blocks. Updating an installed plugin means importing a package with the same ID again, then restarting.

**Run types**
- `command`: `{ "type":"command", "program":"C:\\Tools\\x.exe", "args":["--in","{file}"], "cwd":"{folder}", "skipEmptyArgs":true, "wait":true, "okExitCodes":[0], "capture":true }`
  Arguments are passed one by one (no shell), so field text cannot inject extra commands.
- `powershell`: `{ "type":"powershell", "script":"..." }`. Field values arrive as environment variables
  `FIELD_<KEY>` (key upper-cased), for example `$env:FIELD_URL`. They are never pasted into the script text.

**Results.** Exit code in `okExitCodes` (default `[0]`): a Condition is satisfied (else not satisfied); an Action
succeeds (else it fails and follows its "On failure" setting). `wait:false` starts the program and continues at once.
`{field}` placeholders work in `program`, `args` and `cwd`; use `{{` and `}}` for literal braces.

**Fields.** `type`: `text`, `file` (adds Browse), `number` (needs `min` and `max`), `select` (`options`: `[["value","Label"]]`),
`bool`, `time` (HH:MM). Common keys: `key`, `label`, `default`, `required`, `maxLen`, `unit`, `step`, `help`,
`show` (`{"key":"mode","eq":"wait"}`, `ne`, or `in:[...]`).

**Safety.** A pack runs programs with your permissions. Install only packs you trust. Actions from packs fall under
the runaway guard (at most 12 runs per 10 s). Imported automations cannot install packs.

## 2. Developers: a native block (Rust)

1. In `src-tauri/src/blocks.rs` add a struct and `impl Block for ...`:
   - `title()`, `category()`: required.
   - `spec()`: return the same JSON as above (without `run`) and the UI is generated automatically:
     **no change to `ui/blocks.js` is needed**.
   - `validate(cfg)`: `spec::validate(&my_spec(), cfg)` checks every field from the spec for you.
   - `run(cx)`: do the work and return `Outcome::Satisfied` / `NotSatisfied`, or `Err("reason")`.
     Use `cx.report(state, progress, text)`, `cx.log(level, text)`, `cx.resolve()` (wired targets),
     `cx.token` (cancellation), `cx.visit` (times reached).
   - `supports_timeout()` true if the spec has a `timeout` preset field; `risky()` true for side-effect actions.
2. Register it in `registry()`: `m.insert("cond.my_block", Box::new(MyBlock));`
3. `Time window` (`cond.schedule`) is the reference example.

Blocks that need a custom editor widget can still be defined by hand in `ui/blocks.js` as before; a hand-written
definition wins over a spec with the same kind.