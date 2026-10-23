# Built-in automation blocks

The native block registry includes these built-in additions:

## Conditions

- **Network Condition** checks Wi-Fi connection/disconnection, current SSID, estimated adapter upload/download rate, and a TCP reachability probe for a host. Rate thresholds may use Mbps or MiB/s. It supports check-once, wait-until, state-change, poll intervals, stable-duration debounce, and timeout.
- **Battery and Power State** checks battery percentage, charging, AC power, and battery operation using the Windows power-status API.
- **CPU Usage** checks system CPU or an approximate named-process CPU share against a threshold.
- **Disk Space** checks free percentage, used percentage, or free GB on a drive letter.
- **File or Folder State** checks existence, absence, size, or changes to existence/size/last-write time after the block starts.
- **Active Window** checks the process name or title text for the foreground window.
- **Bluetooth Device State**, **USB Device State**, and **Audio Device State** check whether a named Plug and Play device is present and reports status OK.
- **Workstation Lock State** checks whether the active input desktop is the Windows lock/secure desktop.
- **User Idle Time** checks elapsed time since keyboard or mouse input.
- **Application Responsiveness** checks whether a named process is running and its main window is responding.

Existing **RAM Usage**, **Process Running**, **Visible Windows**, and **Process Outputting Audio** blocks cover the other suggested memory, process, and window/audio observations. Network Condition combines Wi-Fi state, internet reachability, and network-adapter throughput so the palette does not need separate blocks for each network metric.

## Actions

- **Open URL** opens an HTTP(S) URL in the default browser.
- **Set System Volume** sets master output volume or mute state through Windows Core Audio.
- **Set Process Priority** changes the priority class of matching process instances; Real-time priority is not offered.
- **Control Window** focuses, minimizes, maximizes, or restores an application's main window.
- **Copy or Move File** copies or moves a single file and makes destination collision handling explicit.
- **Set Clipboard Text** places configured text on the clipboard without logging its contents.
- **HTTP Request / Webhook** sends GET, HEAD, POST, PUT, PATCH, or DELETE requests. It logs the response status by default; response-body logging is opt-in, and a bounded response excerpt and status code can be saved into workflow variables.
- **Media Control** sends play/pause, next, previous, or stop media keys.
- **Power Control** locks, sleeps, hibernates, shuts down, restarts, or cancels a pending shutdown.
- **Delete or Recycle File** sends a file/folder to the Recycle Bin or permanently deletes it.
- **Play Sound or Speak Text** plays a WAV file or uses Windows text-to-speech.
- **Start, Stop, or Restart Service** controls a Windows service by service name.

## Runtime and safety

Polling blocks use the configured poll interval, cancellation token, optional stable-duration debounce, and timeout. Conditions return **Not Satisfied** for a failed one-shot check; timed wait failures return an error for the workflow's configured failure handling.

These blocks are Windows-specific and launch hidden PowerShell helpers for Windows facilities that do not have an existing native wrapper. Commands are not a sandbox and execute as the current user. HTTP requests may access private/internal endpoints; avoid storing secrets in block fields and review workflow exports. File deletion can be irreversible, process-priority/window changes may be denied by Windows, and service/power operations may require elevated rights or hardware support. The power block gives shutdown/restart a configurable delay (default 60 seconds) to leave time to cancel.

Network throughput uses adapter byte-counter deltas between polls, not a speed-test of the ISP connection. The internet check probes TCP port 443 on a host (or URL host); it is not a full DNS/HTTP certificate or application-health check. Bluetooth/USB/audio presence uses Plug and Play enumeration and may lag hardware events briefly. Workstation lock detection can be unavailable in restricted desktop/session environments.


## Additional utilities

The next utility batch adds these built-in blocks:

### Conditions

- **Process Count** compares the number of running instances of a process to a configured value.
- **Service State** checks whether a Windows service is running, stopped, paused, pending, or missing.
- **Clipboard Text** checks whether clipboard text is empty, equal to a string, contains text, or starts with a prefix. It never logs clipboard contents and requires an interactive user session.
- **Environment Variable** checks process-, user-, or machine-scope variables without logging their values.
- **File Age** compares a file or directory's last-modified age in minutes.
- **Directory Item Count** counts files, directories, or both, with optional recursive enumeration and numeric comparisons.

### Actions

- **Create Folder** creates a directory including missing parent directories.
- **Write Text File** creates, replaces, or appends UTF-8 text without logging the content.
- **Create ZIP Archive** compresses a file or directory and requires explicit destination-overwrite behavior.
- **Open Folder** opens an existing directory in File Explorer.
- **Create Windows Shortcut** creates a .lnk shortcut with optional arguments, working directory, and icon.
- **Type Text into Active Window** sends Unicode text to the currently focused application. It does not select or activate the target window.
- **Capture Screenshot** saves the full virtual desktop as PNG and requires a user-selected overwrite policy.
- **Set Desktop Wallpaper** changes the desktop wallpaper to an existing image.

The additional conditions and actions use declarative specs for their editor fields. PowerShell-backed blocks are Windows-only; file-system blocks follow the current user's permissions. Typing text and capturing screenshots require an interactive desktop, and screenshots can contain private information. ZIP overwrite and screenshot overwrite are explicit settings rather than silent defaults.


## Advanced automation, data flow, and wait behavior

### Additional conditions

- **File & Folder Watcher** watches a directory snapshot for matching file or folder creation, modification, deletion, or rename activity. The **At least one matching item exists** event supports useful one-shot checks, while other event modes compare against the initial snapshot. Polling is bounded to 10,000 matching items and is configurable.
- **File Content Condition** compares a bounded-size UTF-8 file by literal text or a time-limited .NET regular expression.
- **HTTP Status & API Condition** waits for an HTTP endpoint to return a matching status, success status, response text, or dot-separated JSON property value. Requests are capped at 10 seconds per probe and do not log response bodies.
- **Window State Condition** checks whether an app has a window, is foreground, minimized, maximized, or restored.

### Additional actions

- **Set Variable**, **Get Variable**, **Increment Variable**, and **Clear Variable** provide per-automation-run workflow variables. Values are isolated by automation run and discarded when the run ends; concurrent automations do not share variable stores.
- **Send Keyboard Shortcut** dispatches a shortcut to the currently focused app using Windows SendKeys conventions (Ctrl = `^`, Shift = `+`, Alt = `%`).
- **Mouse Automation** moves/clicks/scrolls using virtual-desktop coordinates or offsets relative to a selected process window. Coordinate scaling, DPI, and monitor layout can affect which target receives the input.
- **Extract ZIP Archive** expands a ZIP to a staging directory before installing it. When replacing an existing destination, the action retains a temporary backup and attempts restoration if installation fails.
- **Bulk File Operations** copies, moves, renames, or recycles matching files. A preview-only mode prints up to 200 planned changes and flags destination collisions without modifying files; actual operations refuse to exceed the configured match cap and can preflight destination collisions.
- **Set Display Brightness** uses the Windows WMI brightness interface when a monitor supports it.
- **Set Power Plan** activates an existing Windows power plan.
- **Ask User / Input / Choice** displays a blocking confirmation, text-input, or configured-choice dialog. Yes/OK stores an accepted input or choice in a per-run variable; Cancel does not alter the variable and follows ELSE.
- **Retry Command** runs PowerShell with bounded attempts and fixed or exponential backoff. The block supports the workflow timeout setting in addition to the retry count.
- **Find File & Store Path** finds a matching file using a selected ordering (first, newest, oldest, largest, or smallest) and saves the full path in a workflow variable. No matches return Not Satisfied so the ELSE route can handle it.

### Passing values between blocks

Use `{{variable_name}}` placeholders in supported text settings, including the Run Command, HTTP request/condition, file, directory, window/process, and desktop action fields implemented for template expansion. A missing variable is an explicit error, not silently replaced with an empty string. Run Command can save standard output into a variable. HTTP Request can save its response excerpt and numeric status code into variables. HTTP response-body logging is opt-in.

### Wait until satisfied, polling, and timeouts

Where conditions represent a state that can change over time, they support **Check once**, **Wait until satisfied**, and **Wait until state changes** with poll interval, optional stable-duration debounce, and timeout. This applies to network, CPU, disk, battery, file/folder, active-window, device-presence, lock/idle, application-responsiveness, visible-window-count, process-audio, process-count, service-state, clipboard, environment-variable, file-age, directory-count, file-watcher, file-content, HTTP-status, and window-state conditions. The legacy RAM Usage and Process Running conditions, plus Time Window, also expose wait and state-change modes with real polling, stable-duration debounce, and timeout handling. Interval remains a time gate rather than a polled state condition. For existing automations that predate the Behavior field, an absent setting defaults to **Check once** to preserve their previous semantics. Interval remains a time gate rather than a polled state condition.


## UI Automation, data pipelines, debugging, events, and credentials

### Accessible desktop control

- **UI Element Condition** locates a uniquely matched accessible Windows control using its Automation ID, exact element name, control type, owning process, and/or containing-window title. It supports polling and state-change waits.
- **UI Automation Action** invokes or focuses a control, sets a text value, toggles a checkbox, or selects a list item. It requires a unique selector match to prevent accidental interaction with a similarly named control.
- Use these controls instead of screen coordinates when an application exposes a usable Windows UI Automation tree. Elevated applications, games, remote desktops, and custom-drawn canvases may not expose accessible controls. Coordinate-based mouse actions remain available as a fallback.

### Data preparation and branching

- **Parse JSON** parses JSON text into a structured workflow variable and can select a nested object property or array index.
- **Get JSON Property** selects a property or array item from a structured variable.
- **Regex Extract** captures a match group; **Regex Replace** transforms matching text. These use .NET regular expressions with a bounded execution time.
- **Read CSV into Variable**, **Filter Array Rows**, and **Write CSV from Variable** support bounded CSV-to-array workflows. CSV parser input and row counts are capped; write operations stage the output and preserve an existing file if installation fails.
- **Transform Value** supports safe string operations, numeric arithmetic, length, splitting, and unit conversion. It does not evaluate user-provided expressions as code; arithmetic expressions use a bounded parser.
- **Variable Condition** compares run-scoped values. **Switch / Case** routes execution based on configured comparisons.
- **For Each Item** iterates a JSON array sequentially. Connect each body path to **For Each End**, **For Each Continue**, or **For Each Break**; the body must reach a loop-control marker. Input, item, and index variables must be distinct, and loops are bounded to 100,000 items.
- **Call Subflow** calls another saved automation with explicit JSON input/output variable mappings. **Try / Catch Subflows** invokes a catch automation when the try automation fails, passing the error text in a named variable. Nested calls have a maximum depth and direct self-calls are rejected.

### Debugging and run history

- **Debugger Breakpoint** pauses when the automation is running in debug mode and the breakpoint is enabled. From the editor, Pause takes effect at a block boundary; Step executes one block before pausing again; Resume continues normal execution. Selected-block breakpoints are stored in editor-local settings.
- **Variables** shows live values while a run is active, or the last run's snapshot after it ends. Sensitive-looking variable names (such as names containing password, token, secret, or credential) are redacted, and long values are shortened.
- **History / Trace** displays bounded per-block execution summaries with state, duration, visit count, and a short detail string. A run also retains a bounded copy of the automation graph when it is small enough. **Copy Snapshot** recreates that graph as a separate stopped automation for inspection; it never starts the copy automatically.
- Existing historical records from before per-node traces and graph snapshots were introduced remain readable; those older records simply have no trace/snapshot data.

### Event-oriented entry conditions and local webhooks

- **Global Hotkey Condition** registers a configured global shortcut using Windows' non-repeating hotkey registration. The chord may fail to register if another application owns it.
- **File System Event** waits for FileSystemWatcher notifications rather than repeatedly scanning the directory. It can save the triggering path to a variable. High-rate changes, remote shares, access restrictions, or filesystem notification-buffer overflow can affect event delivery.
- **Process Start / Exit Event** uses the Windows process-event provider to detect a matching executable starting or exiting.
- **Windows Event Log Condition** filters recent records by selected event log, optional event IDs, provider, and message text. Security-log access may require additional permissions; each poll intentionally scans a bounded number of recent records.
- **Local Webhook Trigger** binds only to `127.0.0.1`, accepts authenticated POST requests using `X-00047-Token`, caps request-body size, and stores the body and selected safe headers in variables. A local listener is not exposed to other machines without a separately configured relay. Keep token values in the credential vault where practical.

### Credential vault

Use **Secrets vault…** in the application settings to create or delete named credentials. Values are encrypted for the current Windows user using DPAPI and are not saved in workflow definitions. Reference a named credential in supported text fields with `{{secret:name}}`; regular per-run values use `{{variable_name}}`. Secret values can still become visible to a destination application or external service when a workflow intentionally sends or types them, so review destination URLs and target controls and do not log expanded secret values.

All desktop automation and event providers are subject to Windows session, permissions, accessibility, and system-configuration limits. The new blocks execute with the current user's permissions; they do not bypass Windows privilege boundaries or create a sandbox for scripts and commands.
