# Adding blocks to 00047

A block is a Condition (IF/ELSE) or an Action. Pick the route that fits.

## 1. Users: a block pack (JSON, no coding)

Put a `.json` file in the blocks folder (Settings → Custom blocks → Open folder), then choose **Reload**
(00047 restarts). Or use **Install pack…**, which shows exactly what each block will run before copying it.
An example is in `docs/examples/blocks/sample-pack.json`.

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