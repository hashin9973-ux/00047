//! Advanced data-flow, file, HTTP, and desktop automation blocks.
use super::{extra, Block, Cat, NodeCx, Outcome};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

fn txt(c: &Value, k: &str) -> String {
    c.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}
fn raw(c: &Value, k: &str) -> String {
    c.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}
fn boolv(c: &Value, k: &str, d: bool) -> bool {
    c.get(k).and_then(Value::as_bool).unwrap_or(d)
}
fn num(c: &Value, k: &str, d: f64, min: f64, max: f64, label: &str) -> Result<f64, String> {
    let v = match c.get(k) {
        None => d,
        Some(v) => v.as_f64().ok_or_else(|| format!("{label} must be a number"))?,
    };
    if v.is_finite() && (min..=max).contains(&v) { Ok(v) } else { Err(format!("{label} must be between {min} and {max}")) }
}
fn no_nul(c: &Value, keys: &[&str]) -> Result<(), String> {
    for k in keys {
        if raw(c, k).contains('\0') { return Err(format!("{} must not contain NUL characters", k)); }
    }
    Ok(())
}
fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}
fn spec_condition(title: &str, icon: &str, description: &str, mut fields: Vec<Value>, summary: &str, help: &str) -> Value {
    fields.extend(extra::behavior_fields());
    json!({"category":"condition","title":title,"icon":icon,"description":description,"fields":fields,"summary":summary,"help":help})
}
fn spec_action(title: &str, icon: &str, description: &str, fields: Vec<Value>, summary: &str, help: &str) -> Value {
    json!({"category":"action","title":title,"icon":icon,"description":description,"fields":fields,"summary":summary,"help":help})
}
fn variable_text(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), other => other.to_string() }
}
fn template(cx: &NodeCx, s: &str) -> Result<String, String> { cx.expand_template(s) }

fn wildcard_match(pattern: &str, name: &str) -> bool {
    // Small, allocation-light wildcard matcher supporting * and ?.
    let (mut p, mut n, mut star, mut mark) = (0usize, 0usize, None, 0usize);
    let pb = pattern.to_lowercase().into_bytes();
    let nb = name.to_lowercase().into_bytes();
    while n < nb.len() {
        if p < pb.len() && (pb[p] == b'?' || pb[p] == nb[n]) { p += 1; n += 1; }
        else if p < pb.len() && pb[p] == b'*' { star = Some(p); p += 1; mark = n; }
        else if let Some(s) = star { p = s + 1; mark += 1; n = mark; }
        else { return false; }
    }
    while p < pb.len() && pb[p] == b'*' { p += 1; }
    p == pb.len()
}

fn folder_snapshot(root: &Path, recursive: bool, pattern: &str, item_type: &str) -> Result<BTreeMap<String, (bool, u64, u128)>, String> {
    let mut result = BTreeMap::new();
    if !root.exists() { return Ok(result); }
    if !root.is_dir() { return Err("Watched path is not a directory".into()); }
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| format!("Could not read directory {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("Directory enumeration failed: {e}"))?;
            visited += 1;
            if visited > 100_000 { return Err("Directory scan limit reached (100,000 entries)".into()); }
            let kind = entry.file_type().map_err(|e| format!("Could not inspect file type: {e}"))?;
            // Do not recurse through symlinks/junction-like entries.
            if kind.is_symlink() { continue; }
            let path = entry.path();
            let meta = entry.metadata().map_err(|e| format!("Could not inspect {}: {e}", path.display()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = meta.is_dir();
            if is_dir && recursive { stack.push(path.clone()); }
            let include_kind = match item_type {
                "files" => meta.is_file(),
                "folders" => is_dir,
                _ => meta.is_file() || is_dir,
            };
            if include_kind && wildcard_match(pattern, &name) {
                if result.len() >= 10000 { return Err("Folder scan limit reached (10,000 matching items)".into()); }
                let stamp = meta.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
                result.insert(path.to_string_lossy().to_string(), (is_dir, meta.len(), stamp));
            }
        }
    }
    Ok(result)
}

// Conditions -----------------------------------------------------------------

struct FileWatcher;
fn file_watcher_spec() -> Value {
    spec_condition("File & Folder Watcher", "folder-search", "Waits for matching files or folders to be created, modified, deleted, or renamed in a directory.", vec![
        json!({"key":"path","label":"Directory to watch","type":"text","required":true,"maxLen":2048}),
        json!({"key":"pattern","label":"Filename pattern","type":"text","default":"*","required":true,"maxLen":512}),
        json!({"key":"itemType","label":"Watch","type":"select","default":"both","options":[["files","Files"],["folders","Folders"],["both","Files and folders"]]}),
        json!({"key":"recursive","label":"Include subdirectories","type":"bool","default":false}),
        json!({"key":"event","label":"Event","type":"select","default":"any","options":[["exists","At least one matching item exists"],["created","Created"],["modified","Modified"],["deleted","Deleted"],["any","Any create / modify / delete / rename"]]}),
    ], "Watch {path}: {event}", "A bounded polling snapshot detects matching files and/or folders, including nested items when recursive watching is enabled. Check once usually evaluates false for event-only modes; use Wait until satisfied to wait for an event. Poll interval and timeout are in Behavior.")
}
#[async_trait]
impl Block for FileWatcher {
    fn title(&self) -> &'static str { "File & Folder Watcher" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(file_watcher_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&file_watcher_spec(), c)?;
        no_nul(c, &["path", "pattern"])?;
        if txt(c, "pattern").is_empty() { return Err("Filename pattern is required".into()); }
        if !matches!(txt(c, "itemType").as_str(), "files"|"folders"|"both") { return Err("Unknown watcher item type".into()); }
        if !matches!(txt(c, "event").as_str(), "exists"|"created"|"modified"|"deleted"|"any") { return Err("Unknown file event".into()); }
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let root = PathBuf::from(template(cx, &txt(&cx.cfg, "path"))?);
        let pattern = template(cx, &txt(&cx.cfg, "pattern"))?;
        let recursive = boolv(&cx.cfg, "recursive", false);
        let item_type = txt(&cx.cfg, "itemType");
        let event = txt(&cx.cfg, "event");
        if !root.is_dir() { return Err("Watched directory must exist before the block starts".into()); }
        let baseline = folder_snapshot(&root, recursive, &pattern, &item_type)?;
        if event == "exists" {
            let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
            return extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
                let root = root.clone(); let pattern = pattern.clone(); let item_type = item_type.clone();
                Box::pin(async move {
                    let current = folder_snapshot(&root, recursive, &pattern, &item_type)?;
                    Ok(!current.is_empty())
                }) as extra::Probe<'_>
            }).await;
        }
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let root = root.clone(); let pattern = pattern.clone(); let baseline = baseline.clone();
            let event = event.clone(); let item_type = item_type.clone();
            Box::pin(async move {
                let current = folder_snapshot(&root, recursive, &pattern, &item_type)?;
                let created = current.keys().any(|k| !baseline.contains_key(k));
                let deleted = baseline.keys().any(|k| !current.contains_key(k));
                let modified = current.iter().any(|(k, v)| baseline.get(k).is_some_and(|b| b != v));
                Ok(match event.as_str() {
                    "created" => created,
                    "modified" => modified,
                    "deleted" => deleted,
                    _ => created || modified || deleted,
                })
            }) as extra::Probe<'_>
        }).await
    }
}

struct FileContent;
fn file_content_spec() -> Value {
    spec_condition("File Content Condition", "file-text", "Checks text in a file using a literal comparison or a regular expression.", vec![
        json!({"key":"path","label":"Text file path","type":"text","required":true,"maxLen":2048}),
        json!({"key":"mode","label":"Match","type":"select","default":"contains","options":[["contains","Contains text"],["not_contains","Does not contain text"],["equals","Equals full content"],["starts","Starts with text"],["regex","Matches regular expression"],["not_regex","Does not match regular expression"]]}),
        json!({"key":"pattern","label":"Text or regular expression","type":"text","required":true,"maxLen":4096}),
        json!({"key":"caseSensitive","label":"Case-sensitive match","type":"bool","default":false}),
        json!({"key":"maxKb","label":"Maximum file size (KiB)","type":"number","min":1,"max":8192,"step":1,"default":1024}),
    ], "File content: {path}", "Files larger than the configured read limit fail safely instead of loading unbounded content. Regular expressions have a 500 ms execution timeout.")
}
#[async_trait]
impl Block for FileContent {
    fn title(&self) -> &'static str { "File Content Condition" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(file_content_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&file_content_spec(), c)?;
        no_nul(c, &["path", "pattern"])?;
        if txt(c, "pattern").is_empty() { return Err("Text or regular expression is required".into()); }
        if !matches!(txt(c, "mode").as_str(), "contains"|"not_contains"|"equals"|"starts"|"regex"|"not_regex") { return Err("Unknown content comparison".into()); }
        num(c, "maxKb", 1024.0, 1.0, 8192.0, "Maximum file size")?;
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let path = template(cx, &txt(&cx.cfg, "path"))?;
        let pattern = template(cx, &raw(&cx.cfg, "pattern"))?;
        let mode = txt(&cx.cfg, "mode");
        let case_sensitive = boolv(&cx.cfg, "caseSensitive", false);
        let max_bytes = (num(&cx.cfg, "maxKb", 1024.0, 1.0, 8192.0, "Maximum file size")? * 1024.0) as u64;
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let path = path.clone(); let pattern = pattern.clone(); let mode = mode.clone();
            Box::pin(async move {
                let meta = match fs::metadata(&path) {
                    Ok(m) => m,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                    Err(e) => return Err(format!("Could not inspect text file: {e}")),
                };
                if !meta.is_file() { return Err("Configured path is not a file".into()); }
                if meta.len() > max_bytes { return Err(format!("Text file exceeds the configured {} KiB limit", max_bytes / 1024)); }
                let body = fs::read_to_string(&path).map_err(|e| format!("Could not read UTF-8 text file: {e}"))?;
                let (b, p) = if case_sensitive { (body, pattern.clone()) } else { (body.to_lowercase(), pattern.to_lowercase()) };
                let matched = match mode.as_str() {
                    "contains" => b.contains(&p),
                    "not_contains" => !b.contains(&p),
                    "equals" => b.trim_end_matches(['\r','\n']) == p.trim_end_matches(['\r','\n']),
                    "starts" => b.starts_with(&p),
                    "regex" | "not_regex" => {
                        // PowerShell's .NET regex engine supplies a bounded match timeout.
                        let script = format!("$ErrorActionPreference='Stop'; $body=[IO.File]::ReadAllText({}); $pattern={}; $opts=[Text.RegularExpressions.RegexOptions]::CultureInvariant; if(-not {}){{$opts=$opts -bor [Text.RegularExpressions.RegexOptions]::IgnoreCase}}; $re=[Text.RegularExpressions.Regex]::new($pattern,$opts,[TimeSpan]::FromMilliseconds(500)); $hit=$re.IsMatch($body); [bool]$(if({}){{$hit}}else{{-not $hit}}) | ConvertTo-Json -Compress",
                            extra::ps_quote(&path), extra::ps_quote(&pattern), if case_sensitive { "$true" } else { "$false" }, if mode == "regex" { "$true" } else { "$false" });
                        let out = extra::powershell(cx, &script).await?;
                        serde_json::from_str::<bool>(out.trim()).map_err(|e| format!("Invalid regex condition result: {e}"))?
                    },
                    _ => false,
                };
                Ok(matched)
            }) as extra::Probe<'_>
        }).await
    }
}

struct HttpStatus;
#[derive(Default)]
struct HttpProbe { status: u16, matches: bool }
fn http_status_spec() -> Value {
    spec_condition("HTTP Status & API Condition", "network", "Checks an HTTP endpoint's status, response text, or a JSON field.", vec![
        json!({"key":"url","label":"HTTP or HTTPS URL","type":"text","required":true,"maxLen":2048}),
        json!({"key":"method","label":"Request method","type":"select","default":"GET","options":[["GET","GET"],["HEAD","HEAD"]]}),
        json!({"key":"mode","label":"Match","type":"select","default":"status","options":[["status","Status code"],["success","HTTP 2xx or 3xx success"],["contains","Response contains text"],["json_equals","JSON field equals value"]]}),
        json!({"key":"expectedStatus","label":"Expected status code","type":"number","min":100,"max":599,"step":1,"default":200,"show":{"key":"mode","eq":"status"}}),
        json!({"key":"containsText","label":"Text to find","type":"text","maxLen":2048,"show":{"key":"mode","eq":"contains"}}),
        json!({"key":"jsonPath","label":"JSON property path (dot-separated)","type":"text","maxLen":512,"show":{"key":"mode","eq":"json_equals"}}),
        json!({"key":"expectedValue","label":"Expected JSON value","type":"text","maxLen":2048,"show":{"key":"mode","eq":"json_equals"}}),
    ], "HTTP condition: {url}", "Requests are sent using Windows PowerShell and may reach localhost or private network hosts. Responses are not logged. Each request is capped at 10 seconds; polling retries according to Behavior.")
}
async fn http_probe(cx: &NodeCx, url: &str, method: &str, mode: &str, expected_status: u16, contains: &str, json_path: &str, expected_value: &str) -> Result<HttpProbe, String> {
    let url = template(cx, url)?;
    let script = format!(
        "$ErrorActionPreference='Stop'; $u={}; $method={}; $mode={}; $contains={}; $path={}; $expected={}; $status=0; $body=''; try {{$r=Invoke-WebRequest -UseBasicParsing -Uri $u -Method $method -TimeoutSec 10 -MaximumRedirection 5 -ErrorAction Stop; $status=[int]$r.StatusCode; $body=[string]$r.Content}} catch {{ $ex=$_.Exception; if($null -ne $ex.Response){{try{{$status=[int]$ex.Response.StatusCode}}catch{{}}; try{{$sr=New-Object IO.StreamReader($ex.Response.GetResponseStream());$body=$sr.ReadToEnd();$sr.Dispose()}}catch{{}} }} else {{$status=0; $body=''}} }}; $ok=$false; switch($mode){{'status'{{$ok=$status -eq {}}}'success'{{$ok=$status -ge 200 -and $status -lt 400}}'contains'{{$ok=$body.IndexOf($contains,[StringComparison]::OrdinalIgnoreCase) -ge 0}}'json_equals'{{try{{$o=ConvertFrom-Json -InputObject $body -ErrorAction Stop; foreach($part in ($path -split '\\.')){{if($null -eq $o){{break}};$o=$o.$part}}; $ok=([string]$o -ceq $expected)}}catch{{$ok=$false}}}}}}; [pscustomobject]@{{status=$status;matches=[bool]$ok}} | ConvertTo-Json -Compress",
        extra::ps_quote(&url), extra::ps_quote(method), extra::ps_quote(mode), extra::ps_quote(contains),
        extra::ps_quote(json_path), extra::ps_quote(expected_value), expected_status);
    let out = extra::powershell(cx, &script).await?;
    let value: Value = serde_json::from_str(&out).map_err(|e| format!("Could not parse HTTP probe result: {e}"))?;
    Ok(HttpProbe {
        status: value.get("status").and_then(Value::as_u64).unwrap_or(0).min(u16::MAX as u64) as u16,
        matches: value.get("matches").and_then(Value::as_bool).unwrap_or(false),
    })
}
#[async_trait]
impl Block for HttpStatus {
    fn title(&self) -> &'static str { "HTTP Status & API Condition" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(http_status_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&http_status_spec(), c)?;
        no_nul(c, &["url", "containsText", "jsonPath", "expectedValue"])?;
        let url = txt(c, "url");
        if !(url.starts_with("http://") || url.starts_with("https://")) { return Err("URL must start with http:// or https://".into()); }
        let method = txt(c, "method");
        let mode = txt(c, "mode");
        if !matches!(method.as_str(), "GET"|"HEAD") { return Err("Unsupported HTTP method".into()); }
        if !matches!(mode.as_str(), "status"|"success"|"contains"|"json_equals") { return Err("Unknown HTTP match mode".into()); }
        if mode == "contains" && method == "HEAD" { return Err("Use GET when matching response text".into()); }
        if mode == "contains" && raw(c, "containsText").is_empty() { return Err("Response text to find is required".into()); }
        if mode == "json_equals" && (method == "HEAD" || txt(c, "jsonPath").is_empty() || c.get("expectedValue").and_then(Value::as_str).is_none()) { return Err("GET, a JSON property path, and an expected value are required for JSON matching".into()); }
        if mode == "status" {
            let status = num(c, "expectedStatus", 200.0, 100.0, 599.0, "Expected status code")?;
            if status.fract() != 0.0 { return Err("HTTP status code must be a whole number".into()); }
        }
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let url = txt(&cx.cfg, "url");
        let method = txt(&cx.cfg, "method");
        let mode = txt(&cx.cfg, "mode");
        let code = if mode == "status" { num(&cx.cfg, "expectedStatus", 200.0, 100.0, 599.0, "Expected status code")? as u16 } else { 200u16 };
        let contains = template(cx, &raw(&cx.cfg, "containsText"))?;
        let path = template(cx, &txt(&cx.cfg, "jsonPath"))?;
        let expected = template(cx, &raw(&cx.cfg, "expectedValue"))?;
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let url = url.clone(); let method = method.clone(); let mode = mode.clone();
            let contains = contains.clone(); let path = path.clone(); let expected = expected.clone();
            Box::pin(async move {
                let result = http_probe(cx, &url, &method, &mode, code, &contains, &path, &expected).await?;
                (cx.report)("waiting", None, format!("HTTP status {} · {}", result.status, if result.matches { "condition satisfied" } else { "not satisfied" }));
                Ok(result.matches)
            }) as extra::Probe<'_>
        }).await
    }
}

struct WindowState;
fn window_state_spec() -> Value {
    spec_condition("Window State Condition", "app-window", "Checks whether an application's main window exists, is foreground, minimized, maximized, or normal.", vec![
        json!({"key":"processName","label":"Process name","type":"text","required":true,"maxLen":260}),
        json!({"key":"state","label":"Expected state","type":"select","default":"foreground","options":[["exists","Has a window"],["foreground","Is foreground"],["minimized","Is minimized"],["maximized","Is maximized"],["normal","Is normal / restored"]]}),
    ], "Window {processName}: {state}", "Use the process name without .exe. Foreground focus can be restricted by Windows; the check reads the current session's desktop.")
}
async fn window_state_probe(cx: &NodeCx, name: &str, state: &str) -> Result<bool, String> {
    let code = r#"using System;using System.Runtime.InteropServices; public static class WindowProbe { [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h); [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h); [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); }"#;
    let script = format!("$ErrorActionPreference='Stop'; Add-Type -TypeDefinition {}; $n={}; $want={}; $p=@(Get-Process -Name $n -ErrorAction SilentlyContinue | Where-Object {{$_.MainWindowHandle -ne 0}} | Select-Object -First 1); $ok=$false; if($want -eq 'exists'){{$ok=@(Get-Process -Name $n -ErrorAction SilentlyContinue).Count -gt 0}}elseif($p.Count -gt 0){{$h=[IntPtr]$p[0].MainWindowHandle; $mini=[WindowProbe]::IsIconic($h); $max=[WindowProbe]::IsZoomed($h); $front=([WindowProbe]::GetForegroundWindow() -eq $h); $ok=switch($want){{'foreground'{{$front}}'minimized'{{$mini}}'maximized'{{$max}}'normal'{{-not $mini -and -not $max}}default{{$false}}}}}}; [bool]$ok | ConvertTo-Json -Compress", extra::ps_quote(code), extra::ps_quote(name), extra::ps_quote(state));
    let out = extra::powershell(cx, &script).await?;
    serde_json::from_str::<bool>(out.trim()).map_err(|e| format!("Invalid window state result: {e}"))
}
#[async_trait]
impl Block for WindowState {
    fn title(&self) -> &'static str { "Window State Condition" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(window_state_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&window_state_spec(), c)?;
        no_nul(c, &["processName"])?;
        if !matches!(txt(c, "state").as_str(), "exists"|"foreground"|"minimized"|"maximized"|"normal") { return Err("Unknown window state".into()); }
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let name = template(cx, &txt(&cx.cfg, "processName"))?;
        let state = txt(&cx.cfg, "state");
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let name = name.clone(); let state = state.clone();
            Box::pin(async move { window_state_probe(cx, &name, &state).await }) as extra::Probe<'_>
        }).await
    }
}

// Actions --------------------------------------------------------------------

struct SetVariable;
fn set_variable_spec() -> Value {
    spec_action("Set Variable", "variable", "Stores a text, number, boolean, JSON value, or current timestamp for later blocks.", vec![
        json!({"key":"name","label":"Variable name","type":"text","required":true,"maxLen":64,"default":"result"}),
        json!({"key":"source","label":"Value source","type":"select","default":"literal","options":[["literal","Configured value"],["utc_now","Current UTC timestamp"],["local_now","Current local timestamp"],["environment","Environment variable"]]}),
        json!({"key":"valueType","label":"Value type","type":"select","default":"text","options":[["text","Text"],["number","Number"],["boolean","Boolean"],["json","JSON"]],"show":{"key":"source","eq":"literal"}}),
        json!({"key":"value","label":"Value","type":"textarea","rows":4,"maxLen":32768,"show":{"key":"source","eq":"literal"}}),
        json!({"key":"environmentName","label":"Environment variable name","type":"text","required":true,"maxLen":256,"show":{"key":"source","eq":"environment"}}),
        json!({"preset":"onFail"})
    ], "Set variable: {name}", "Use {{variable_name}} in supported text fields to insert a previously set variable. Variables are scoped to one automation run and disappear when it ends. Do not store passwords or tokens in logs.")
}
#[async_trait]
impl Block for SetVariable {
    fn title(&self) -> &'static str { "Set Variable" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { false }
    fn spec(&self) -> Option<Value> { Some(set_variable_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&set_variable_spec(), c)?;
        if !valid_name(&txt(c, "name")) { return Err("Variable name must start with a letter or underscore and contain only letters, numbers, dots, dashes, or underscores (max 64 characters)".into()); }
        no_nul(c, &["value", "environmentName"])?;
        if !matches!(txt(c, "source").as_str(), "literal"|"utc_now"|"local_now"|"environment") { return Err("Unknown variable source".into()); }
        if !matches!(txt(c, "valueType").as_str(), "text"|"number"|"boolean"|"json") { return Err("Unknown variable type".into()); }
        if txt(c, "source") == "environment" && txt(c, "environmentName").is_empty() { return Err("Environment variable name is required".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let name = txt(&cx.cfg, "name");
        let source = txt(&cx.cfg, "source");
        let val = match source.as_str() {
            "utc_now" => {
                let out = extra::powershell(cx, "[DateTime]::UtcNow.ToString('o')").await?;
                Value::String(out.trim().to_string())
            },
            "local_now" => {
                let out = extra::powershell(cx, "[DateTime]::Now.ToString('o')").await?;
                Value::String(out.trim().to_string())
            },
            "environment" => {
                let key = template(cx, &txt(&cx.cfg, "environmentName"))?;
                let script = format!("$v=[Environment]::GetEnvironmentVariable({}); if($null -eq $v){{throw 'Environment variable is undefined'}}; [string]$v", extra::ps_quote(&key));
                Value::String(extra::powershell(cx, &script).await?)
            },
            _ => {
                let raw_value = template(cx, &raw(&cx.cfg, "value"))?;
                match txt(&cx.cfg, "valueType").as_str() {
                    "text" => Value::String(raw_value),
                    "number" => {
                        let n = raw_value.trim().parse::<f64>().map_err(|e| format!("Value is not a valid number: {e}"))?;
                        if !n.is_finite() { return Err("Variable number must be finite".into()); }
                        json!(n)
                    },
                    "boolean" => match raw_value.trim().to_ascii_lowercase().as_str() { "true"|"1"|"yes" => Value::Bool(true), "false"|"0"|"no" => Value::Bool(false), _ => return Err("Boolean value must be true or false".into()) },
                    _ => serde_json::from_str::<Value>(&raw_value).map_err(|e| format!("Value is not valid JSON: {e}"))?,
                }
            }
        };
        cx.set_variable(name.clone(), val.clone())?;
        let shown = match &val { Value::String(v) => format!("text ({} characters)", v.chars().count()), Value::Number(_) => "number".into(), Value::Bool(_) => "boolean".into(), _ => "JSON value".into() };
        (cx.log)("info", format!("Variable {name} updated ({shown})"));
        Ok(Outcome::Satisfied)
    }
}

struct GetVariable;
fn get_variable_spec() -> Value {
    spec_action("Get Variable", "variable", "Reads a variable, optionally copies it to another name, and can log its value when explicitly enabled.", vec![
        json!({"key":"name","label":"Source variable","type":"text","required":true,"maxLen":64}),
        json!({"key":"destinationName","label":"Optional destination variable","type":"text","maxLen":64}),
        json!({"key":"logValue","label":"Log full value (may expose private data)","type":"bool","default":false}),
        json!({"preset":"onFail"})
    ], "Get variable: {name}", "By default, logs only the value type. The value is copied as its original JSON type; variable values are not automatically written to logs.")
}
#[async_trait]
impl Block for GetVariable {
    fn title(&self) -> &'static str { "Get Variable" }
    fn category(&self) -> Cat { Cat::Action }
    fn spec(&self) -> Option<Value> { Some(get_variable_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&get_variable_spec(), c)?;
        if !valid_name(&txt(c, "name")) { return Err("Source variable name is invalid".into()); }
        if !txt(c, "destinationName").is_empty() && !valid_name(&txt(c, "destinationName")) { return Err("Destination variable name is invalid".into()); }
        no_nul(c, &["name", "destinationName"])?;
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let name = txt(&cx.cfg, "name");
        let value = cx.get_variable(&name).ok_or_else(|| format!("Variable {name} is not set"))?;
        let dest = txt(&cx.cfg, "destinationName");
        if !dest.is_empty() { cx.set_variable(dest.clone(), value.clone())?; }
        if boolv(&cx.cfg, "logValue", false) {
            (cx.log)("info", format!("Variable {name} = {}", variable_text(&value)));
        } else {
            (cx.log)("info", format!("Variable {name} exists (type: {}){}", match value { Value::String(_) => "text", Value::Number(_) => "number", Value::Bool(_) => "boolean", _ => "JSON" }, if dest.is_empty() { String::new() } else { format!("; copied to {dest}") }));
        }
        Ok(Outcome::Satisfied)
    }
}

struct IncrementVariable;
fn increment_spec() -> Value {
    spec_action("Increment Variable", "plus", "Adds a numeric amount to an existing numeric variable.", vec![
        json!({"key":"name","label":"Variable name","type":"text","required":true,"maxLen":64}),
        json!({"key":"amount","label":"Add amount","type":"number","min":-1000000000000.0,"max":1000000000000.0,"step":1,"default":1}),
        json!({"key":"createIfMissing","label":"Create variable if missing (starts at zero)","type":"bool","default":false}),
        json!({"preset":"onFail"})
    ], "Increment {name} by {amount}", "Fails for non-numeric variables. Integer operations remain integers when both the input and increment are whole numbers.")
}
#[async_trait]
impl Block for IncrementVariable {
    fn title(&self) -> &'static str { "Increment Variable" }
    fn category(&self) -> Cat { Cat::Action }
    fn spec(&self) -> Option<Value> { Some(increment_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&increment_spec(), c)?;
        if !valid_name(&txt(c, "name")) { return Err("Variable name is invalid".into()); }
        num(c, "amount", 1.0, -1_000_000_000_000.0, 1_000_000_000_000.0, "Add amount").map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let name = txt(&cx.cfg, "name");
        let amount = num(&cx.cfg, "amount", 1.0, -1_000_000_000_000.0, 1_000_000_000_000.0, "Add amount")?;
        let old = match cx.get_variable(&name) {
            Some(Value::Number(v)) => v.as_f64().ok_or("Stored number is invalid")?,
            None if boolv(&cx.cfg, "createIfMissing", false) => 0.0,
            None => return Err(format!("Variable {name} is not set")),
            Some(_) => return Err(format!("Variable {name} is not numeric")),
        };
        let next = old + amount;
        if !next.is_finite() { return Err("Increment would produce a non-finite number".into()); }
        cx.set_variable(name.clone(), json!(next))?;
        (cx.log)("info", format!("Variable {name} incremented; new value is {next}"));
        Ok(Outcome::Satisfied)
    }
}

struct ClearVariable;
fn clear_variable_spec() -> Value {
    spec_action("Clear Variable", "eraser", "Removes one workflow variable, or clears all variables for the current run.", vec![
        json!({"key":"mode","label":"Clear","type":"select","default":"one","options":[["one","One variable"],["all","All variables"]]}),
        json!({"key":"name","label":"Variable name","type":"text","maxLen":64,"show":{"key":"mode","eq":"one"}}),
        json!({"preset":"onFail"})
    ], "Clear variable: {name}", "Clearing variables does not affect environment variables or any files on disk.")
}
#[async_trait]
impl Block for ClearVariable {
    fn title(&self) -> &'static str { "Clear Variable" }
    fn category(&self) -> Cat { Cat::Action }
    fn spec(&self) -> Option<Value> { Some(clear_variable_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&clear_variable_spec(), c)?;
        if !matches!(txt(c, "mode").as_str(), "one"|"all") { return Err("Unknown clear mode".into()); }
        if txt(c, "mode") == "one" && !valid_name(&txt(c, "name")) { return Err("Variable name is invalid".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        if txt(&cx.cfg, "mode") == "all" {
            cx.clear_variables()?;
            (cx.log)("info", "All workflow variables cleared".into());
        } else {
            let name = txt(&cx.cfg, "name");
            let removed = cx.remove_variable(&name)?;
            (cx.log)("info", if removed { format!("Variable {name} cleared") } else { format!("Variable {name} was already absent") });
        }
        Ok(Outcome::Satisfied)
    }
}

struct KeyboardShortcut;
fn keyboard_spec() -> Value {
    spec_action("Send Keyboard Shortcut", "keyboard", "Sends a Windows SendKeys shortcut to the currently focused application.", vec![
        json!({"key":"shortcut","label":"SendKeys shortcut (for example ^s or %{F4})","type":"text","required":true,"maxLen":128}),
        json!({"key":"delayMs","label":"Delay before sending (ms)","type":"number","min":0,"max":60000,"step":50,"default":100}),
        json!({"preset":"onFail"})
    ], "Keyboard shortcut: {shortcut}", "Uses Windows Forms SendKeys. ^ means Ctrl, + means Shift, % means Alt. The foreground app receives the keys; do not send shortcuts while passwords or unintended dialogs have focus.")
}
#[async_trait]
impl Block for KeyboardShortcut {
    fn title(&self) -> &'static str { "Send Keyboard Shortcut" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(keyboard_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&keyboard_spec(), c)?;
        no_nul(c, &["shortcut"])?;
        if txt(c, "shortcut").is_empty() { return Err("Shortcut is required".into()); }
        num(c, "delayMs", 100.0, 0.0, 60000.0, "Delay").map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let keys = template(cx, &txt(&cx.cfg, "shortcut"))?;
        let delay = num(&cx.cfg, "delayMs", 100.0, 0.0, 60000.0, "Delay")? as u64;
        let script = format!("$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; Start-Sleep -Milliseconds {delay}; [System.Windows.Forms.SendKeys]::SendWait({}); 'Keyboard shortcut sent.'", extra::ps_quote(&keys));
        extra::powershell(cx, &script).await?;
        (cx.log)("info", "Keyboard shortcut dispatched to current foreground window".into());
        Ok(Outcome::Satisfied)
    }
}

struct MouseAutomation;
fn mouse_spec() -> Value {
    spec_action("Mouse Automation", "mouse-pointer-2", "Moves the pointer, clicks, double-clicks, right-clicks, or scrolls at screen coordinates or offsets within a process window.", vec![
        json!({"key":"operation","label":"Operation","type":"select","default":"click","options":[["move","Move pointer"],["click","Left click"],["double","Double-click"],["right","Right click"],["scroll","Scroll wheel"]]}),
        json!({"key":"coordinateSpace","label":"Coordinate space","type":"select","default":"screen","options":[["screen","Virtual desktop"],["window","Relative to window"]]}),
        json!({"key":"processName","label":"Target window process","type":"text","required":true,"maxLen":260,"show":{"key":"coordinateSpace","eq":"window"}}),
        json!({"key":"x","label":"X coordinate / window offset","type":"number","min":-10000,"max":20000,"step":1,"default":500}),
        json!({"key":"y","label":"Y coordinate / window offset","type":"number","min":-10000,"max":20000,"step":1,"default":300}),
        json!({"key":"wheel","label":"Scroll delta (positive = up)","type":"number","min":-12000,"max":12000,"step":120,"default":120,"show":{"key":"operation","eq":"scroll"}}),
        json!({"preset":"onFail"})
    ], "Mouse: {operation} at ({x}, {y})", "Window-relative offsets are measured from the selected process's outer window top-left. Only the first matching process with a main window is used. Actions require an interactive unlocked desktop; layout changes or incorrect offsets can target the wrong control.")
}
#[async_trait]
impl Block for MouseAutomation {
    fn title(&self) -> &'static str { "Mouse Automation" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(mouse_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&mouse_spec(), c)?;
        if !matches!(txt(c, "operation").as_str(), "move"|"click"|"double"|"right"|"scroll") { return Err("Unknown mouse operation".into()); }
        if !matches!(txt(c, "coordinateSpace").as_str(), "screen"|"window") { return Err("Unknown coordinate space".into()); }
        if txt(c, "coordinateSpace") == "window" && txt(c, "processName").is_empty() { return Err("Target window process is required for window-relative coordinates".into()); }
        no_nul(c, &["processName"])?;
        num(c, "x", 500.0, -10000.0, 20000.0, "X coordinate")?;
        num(c, "y", 300.0, -10000.0, 20000.0, "Y coordinate")?;
        num(c, "wheel", 120.0, -12000.0, 12000.0, "Scroll delta").map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let x = num(&cx.cfg, "x", 500.0, -10000.0, 20000.0, "X coordinate")? as i32;
        let y = num(&cx.cfg, "y", 300.0, -10000.0, 20000.0, "Y coordinate")? as i32;
        let op = txt(&cx.cfg, "operation");
        let space = txt(&cx.cfg, "coordinateSpace");
        let process = template(cx, &txt(&cx.cfg, "processName"))?;
        let wheel = num(&cx.cfg, "wheel", 120.0, -12000.0, 12000.0, "Scroll delta")? as i32;
        let code = r#"using System;using System.Runtime.InteropServices;
[StructLayout(LayoutKind.Sequential)] public struct MouseRect { public int Left; public int Top; public int Right; public int Bottom; }
public static class MouseNative {
 [DllImport("user32.dll",SetLastError=true)] public static extern bool SetCursorPos(int x,int y);
 [DllImport("user32.dll",SetLastError=true)] public static extern void mouse_event(uint flags,uint dx,uint dy,int data,UIntPtr extra);
 [DllImport("user32.dll",SetLastError=true)] public static extern bool GetWindowRect(IntPtr hWnd,out MouseRect rect);
}"#;
        let pos = if space == "window" {
            format!("$n={}; $p=@(Get-Process -Name $n -ErrorAction SilentlyContinue | Where-Object {{$_.MainWindowHandle -ne 0}} | Select-Object -First 1); if($p.Count -eq 0){{throw 'No main window found for the selected process'}}; $rect=New-Object MouseRect; if(-not [MouseNative]::GetWindowRect([IntPtr]$p[0].MainWindowHandle,[ref]$rect)){{throw 'Could not read target window bounds'}}; $px=$rect.Left+({x}); $py=$rect.Top+({y});", extra::ps_quote(&process))
        } else {
            format!("$px={x}; $py={y};")
        };
        let (down, up) = match op.as_str() { "right" => (0x0008u32,0x0010u32), _ => (0x0002u32,0x0004u32) };
        let script = format!("$ErrorActionPreference='Stop'; Add-Type -TypeDefinition {}; {pos} if(-not [MouseNative]::SetCursorPos($px,$py)){{throw 'Could not position mouse pointer'}}; switch({}){{'move'{{}}'click'{{[MouseNative]::mouse_event({down},0,0,0,[UIntPtr]::Zero);[MouseNative]::mouse_event({up},0,0,0,[UIntPtr]::Zero)}}'double'{{1..2|ForEach-Object{{[MouseNative]::mouse_event(2,0,0,0,[UIntPtr]::Zero);[MouseNative]::mouse_event(4,0,0,0,[UIntPtr]::Zero);Start-Sleep -Milliseconds 60}}}}'right'{{[MouseNative]::mouse_event(8,0,0,0,[UIntPtr]::Zero);[MouseNative]::mouse_event(16,0,0,0,[UIntPtr]::Zero)}}'scroll'{{[MouseNative]::mouse_event(2048,0,0,{},[UIntPtr]::Zero)}}}}; 'Mouse action sent.'", extra::ps_quote(code), extra::ps_quote(&op), wheel);
        extra::powershell(cx, &script).await?;
        (cx.log)("info", format!("Mouse operation {op} dispatched at ({x}, {y})"));
        Ok(Outcome::Satisfied)
    }
}
struct ExtractZip;
fn extract_zip_spec() -> Value {
    spec_action("Extract ZIP Archive", "file-archive", "Extracts a ZIP archive into a selected directory.", vec![
        json!({"key":"sourcePath","label":"ZIP file path","type":"text","required":true,"maxLen":2048}),
        json!({"key":"destinationPath","label":"Destination directory","type":"text","required":true,"maxLen":2048}),
        json!({"key":"collision","label":"If destination directory exists","type":"select","default":"fail","options":[["fail","Fail without changes"],["replace","Delete destination and replace it"]]}),
        json!({"preset":"onFail"})
    ], "Extract ZIP: {sourcePath}", "Extracts into a staging directory before replacing the destination. It keeps a temporary backup and attempts restoration if the final move fails. Windows Expand-Archive rejects unsafe archive paths that escape the destination.")
}
#[async_trait]
impl Block for ExtractZip {
    fn title(&self) -> &'static str { "Extract ZIP Archive" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(extract_zip_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&extract_zip_spec(), c)?;
        no_nul(c, &["sourcePath", "destinationPath"])?;
        if !txt(c, "sourcePath").to_ascii_lowercase().ends_with(".zip") { return Err("Source file must end in .zip".into()); }
        if !matches!(txt(c, "collision").as_str(), "fail"|"replace") { return Err("Unknown collision mode".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let source = template(cx, &txt(&cx.cfg, "sourcePath"))?;
        let destination = template(cx, &txt(&cx.cfg, "destinationPath"))?;
        let replace = txt(&cx.cfg, "collision") == "replace";
        if !Path::new(&source).is_file() { return Err("ZIP archive does not exist".into()); }
        let source_abs = fs::canonicalize(&source).map_err(|e| format!("Could not resolve ZIP path: {e}"))?;
        let destination_path = Path::new(&destination);
        let destination_abs = if destination_path.exists() {
            let absolute = fs::canonicalize(destination_path).map_err(|e| format!("Could not resolve destination: {e}"))?;
            if !absolute.is_dir() { return Err("Existing extraction destination is not a directory".into()); }
            absolute
        } else {
            let name = destination_path.file_name().ok_or("Destination must name a directory")?;
            let parent = destination_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
            fs::canonicalize(parent).map_err(|e| format!("Extraction destination parent must exist: {e}"))?.join(name)
        };
        if destination_abs.parent().is_none() {
            return Err("A filesystem root cannot be used as an extraction destination".into());
        }
        if replace && source_abs.starts_with(&destination_abs) {
            return Err("The ZIP archive cannot be inside a destination directory that will be replaced".into());
        }
        let script = format!("$ErrorActionPreference='Stop'; $s={}; $d={}; $replace={}; if((Test-Path -LiteralPath $d) -and -not $replace){{throw 'Destination already exists'}}; $parent=Split-Path -Parent $d; if(-not $parent){{$parent='.'}}; $tmp=Join-Path $parent ([IO.Path]::GetFileName($d)+'.extract-'+[Guid]::NewGuid().ToString('N')); $backup=$d+'.backup-'+[Guid]::NewGuid().ToString('N'); New-Item -ItemType Directory -Path $tmp -ErrorAction Stop | Out-Null; try{{Expand-Archive -LiteralPath $s -DestinationPath $tmp -ErrorAction Stop; if(Test-Path -LiteralPath $d){{Move-Item -LiteralPath $d -Destination $backup -ErrorAction Stop; try{{Move-Item -LiteralPath $tmp -Destination $d -ErrorAction Stop; Remove-Item -LiteralPath $backup -Recurse -Force -ErrorAction SilentlyContinue}}catch{{if(Test-Path -LiteralPath $d){{Remove-Item -LiteralPath $d -Recurse -Force -ErrorAction SilentlyContinue}}; if(Test-Path -LiteralPath $backup){{Move-Item -LiteralPath $backup -Destination $d -ErrorAction SilentlyContinue}}; throw}}}}else{{Move-Item -LiteralPath $tmp -Destination $d -ErrorAction Stop}}}}finally{{if(Test-Path -LiteralPath $tmp){{Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue}}}}; 'ZIP archive extracted.'", extra::ps_quote(&source), extra::ps_quote(&destination), if replace { "$true" } else { "$false" });
        extra::powershell(cx, &script).await?;
        (cx.log)("info", format!("Extracted ZIP archive into {destination}"));
        Ok(Outcome::Satisfied)
    }
}

struct BulkFiles;
fn bulk_files_spec() -> Value {
    spec_action("Bulk File Operations", "files", "Finds matching files and copies, moves, renames, or recycles them in one guarded operation.", vec![
        json!({"key":"sourceFolder","label":"Source folder","type":"text","required":true,"maxLen":2048}),
        json!({"key":"pattern","label":"Filename pattern (for example *.log)","type":"text","required":true,"maxLen":512,"default":"*"}),
        json!({"key":"recursive","label":"Include subdirectories","type":"bool","default":false}),
        json!({"key":"operation","label":"Operation","type":"select","default":"copy","options":[["copy","Copy"],["move","Move"],["rename","Rename"],["recycle","Send to Recycle Bin"]]}),
        json!({"key":"destinationFolder","label":"Destination folder","type":"text","maxLen":2048,"show":{"key":"operation","in":["copy","move"]}}),
        json!({"key":"prefix","label":"Filename prefix","type":"text","maxLen":256,"show":{"key":"operation","eq":"rename"}}),
        json!({"key":"suffix","label":"Filename suffix","type":"text","maxLen":256,"show":{"key":"operation","eq":"rename"}}),
        json!({"key":"collision","label":"Name collision","type":"select","default":"fail","options":[["fail","Fail before changes"],["skip","Skip conflicts"],["overwrite","Overwrite existing files"]],"show":{"key":"operation","in":["copy","move","rename"]}}),
        json!({"key":"maxItems","label":"Maximum number of files per run","type":"number","min":1,"max":10000,"step":1,"default":500}),
        json!({"key":"previewOnly","label":"Preview matching files without changing them","type":"bool","default":false}),
        json!({"preset":"onFail"})
    ], "Bulk {operation}: {pattern}", "Bulk operations have side effects. Preview mode lists up to 200 planned changes without modifying files; the configured maximum blocks unexpectedly broad matches. Recycle sends items to the Recycle Bin, not permanent deletion.")
}
#[async_trait]
impl Block for BulkFiles {
    fn title(&self) -> &'static str { "Bulk File Operations" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(bulk_files_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&bulk_files_spec(), c)?;
        no_nul(c, &["sourceFolder", "pattern", "destinationFolder", "prefix", "suffix"])?;
        if !matches!(txt(c, "operation").as_str(), "copy"|"move"|"rename"|"recycle") { return Err("Unknown bulk operation".into()); }
        if matches!(txt(c, "operation").as_str(), "copy"|"move"|"rename")
            && !matches!(txt(c, "collision").as_str(), "fail"|"skip"|"overwrite") {
            return Err("Unknown collision mode".into());
        }
        let cap = num(c, "maxItems", 500.0, 1.0, 10000.0, "Maximum number of files")?;
        if cap.fract() != 0.0 { return Err("Maximum number of files must be a whole number".into()); }
        if matches!(txt(c, "operation").as_str(), "copy"|"move") && txt(c, "destinationFolder").is_empty() { return Err("Destination folder is required for copy and move".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let source = template(cx, &txt(&cx.cfg, "sourceFolder"))?;
        let destination = template(cx, &txt(&cx.cfg, "destinationFolder"))?;
        let pattern = template(cx, &txt(&cx.cfg, "pattern"))?;
        let operation = txt(&cx.cfg, "operation");
        let prefix = template(cx, &txt(&cx.cfg, "prefix"))?;
        let suffix = template(cx, &txt(&cx.cfg, "suffix"))?;
        let recursive = boolv(&cx.cfg, "recursive", false);
        let collision = txt(&cx.cfg, "collision");
        let cap = num(&cx.cfg, "maxItems", 500.0, 1.0, 10000.0, "Maximum number of files")? as usize;
        let preview_only = boolv(&cx.cfg, "previewOnly", false);
        if !Path::new(&source).is_dir() { return Err("Source folder does not exist".into()); }
        if matches!(operation.as_str(), "copy"|"move") {
            if !Path::new(&destination).is_dir() { return Err("Destination folder must already exist".into()); }
            let source_abs = fs::canonicalize(&source).map_err(|e| format!("Could not resolve source folder: {e}"))?;
            let destination_abs = fs::canonicalize(&destination).map_err(|e| format!("Could not resolve destination folder: {e}"))?;
            if destination_abs == source_abs || destination_abs.starts_with(&source_abs) || source_abs.starts_with(&destination_abs) {
                return Err("Source and destination folders must not be the same or overlap; choose separate folders".into());
            }
        }
        let script = format!(
            "$ErrorActionPreference='Stop'; $s={}; $d={}; $pat={}; $op={}; $prefix={}; $suffix={}; $recursive={}; $collision={}; $limit={}; $previewOnly={}; $items=@(Get-ChildItem -LiteralPath $s -Filter $pat -File -Recurse:$recursive -ErrorAction Stop); if($items.Count -gt $limit){{throw ('Matched '+$items.Count+' files, above safety limit '+$limit)}}; if($items.Count -eq 0){{'No matching files.'; exit 0}}; $plan=@(); foreach($f in $items){{$target=if($op -eq 'rename'){{Join-Path $f.DirectoryName ($prefix+$f.BaseName+$suffix+$f.Extension)}}elseif($op -in @('copy','move')){{Join-Path $d $f.Name}}else{{$null}}; if($target -and (Test-Path -LiteralPath $target) -and $target -ne $f.FullName -and $collision -eq 'fail' -and -not $previewOnly){{throw ('Conflict detected before changes: '+$target)}}; $plan+=,[pscustomobject]@{{source=$f.FullName;target=$target;conflict=($target -and (Test-Path -LiteralPath $target) -and $target -ne $f.FullName)}}}}; if($previewOnly){{'Preview: '+$plan.Count+' matching file(s); no changes made.'; $shown=0; foreach($x in $plan){{if($shown -ge 200){{break}}; if($x.target){{[Console]::WriteLine($x.source+' -> '+$x.target+$(if($x.conflict){{' [CONFLICT]'}}else{{''}}))}}else{{[Console]::WriteLine($x.source+' -> Recycle Bin')}}; $shown++}}; if($plan.Count -gt $shown){{'Omitted '+($plan.Count-$shown)+' item(s) from preview output.'}}; exit 0}}; Add-Type -AssemblyName Microsoft.VisualBasic; $done=0; $skipped=0; foreach($x in $plan){{if($x.conflict -and $collision -eq 'skip'){{$skipped++;continue}}; switch($op){{'copy'{{Copy-Item -LiteralPath $x.source -Destination $x.target -Force:($collision -eq 'overwrite') -ErrorAction Stop}}'move'{{Move-Item -LiteralPath $x.source -Destination $x.target -Force:($collision -eq 'overwrite') -ErrorAction Stop}}'rename'{{if($x.target -ne $x.source){{Rename-Item -LiteralPath $x.source -NewName ([IO.Path]::GetFileName($x.target)) -Force:($collision -eq 'overwrite') -ErrorAction Stop}}}}'recycle'{{[Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($x.source,'OnlyErrorDialogs','SendToRecycleBin')}}}}; $done++}}; 'Processed '+$done+' file(s); skipped '+$skipped+'.'",
            extra::ps_quote(&source), extra::ps_quote(&destination), extra::ps_quote(&pattern), extra::ps_quote(&operation),
            extra::ps_quote(&prefix), extra::ps_quote(&suffix), if recursive { "$true" } else { "$false" },
            extra::ps_quote(&collision), cap, if preview_only { "$true" } else { "$false" });
        let out = extra::powershell(cx, &script).await?;
        (cx.log)("info", format!("Bulk file operation completed in {source}: {}", out.trim()));
        Ok(Outcome::Satisfied)
    }
}

struct FindFile;
fn find_file_spec() -> Value {
    spec_action("Find File & Store Path", "folder-search", "Finds the first or a selected matching file and saves its full path to a workflow variable.", vec![
        json!({"key":"searchFolder","label":"Folder to search","type":"text","required":true,"maxLen":2048}),
        json!({"key":"pattern","label":"Filename pattern","type":"text","required":true,"maxLen":512,"default":"*"}),
        json!({"key":"recursive","label":"Include subdirectories","type":"bool","default":false}),
        json!({"key":"selection","label":"Which match to select","type":"select","default":"first","options":[["first","Alphabetically first path"],["newest","Newest modified"],["oldest","Oldest modified"],["largest","Largest file"],["smallest","Smallest file"]]}),
        json!({"key":"saveAs","label":"Store path in variable","type":"text","required":true,"maxLen":64,"default":"filePath"}),
        json!({"preset":"onFail"})
    ], "Find {pattern} in {searchFolder}", "Search is bounded to 100,000 directory entries and 10,000 matching files. If there is no match the block returns Not Satisfied (ELSE); on success the full path is saved to the selected per-run variable.")
}
#[async_trait]
impl Block for FindFile {
    fn title(&self) -> &'static str { "Find File & Store Path" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { false }
    fn spec(&self) -> Option<Value> { Some(find_file_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&find_file_spec(), c)?;
        no_nul(c, &["searchFolder", "pattern", "saveAs"])?;
        if txt(c, "pattern").is_empty() { return Err("Filename pattern is required".into()); }
        if !matches!(txt(c, "selection").as_str(), "first"|"newest"|"oldest"|"largest"|"smallest") { return Err("Unknown file selection mode".into()); }
        if !valid_name(&txt(c, "saveAs")) { return Err("Output variable name is invalid".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let folder = PathBuf::from(template(cx, &txt(&cx.cfg, "searchFolder"))?);
        let pattern = template(cx, &txt(&cx.cfg, "pattern"))?;
        let recursive = boolv(&cx.cfg, "recursive", false);
        let selection = txt(&cx.cfg, "selection");
        let save_as = txt(&cx.cfg, "saveAs");
        if !folder.is_dir() { return Err("Search folder does not exist".into()); }
        let matches = folder_snapshot(&folder, recursive, &pattern, "files")?;
        if matches.is_empty() {
            (cx.log)("info", format!("No files matched {pattern} in {}", folder.display()));
            return Ok(Outcome::NotSatisfied);
        }
        let selected = match selection.as_str() {
            "newest" => matches.into_iter().max_by_key(|(_, (_, _, stamp))| *stamp),
            "oldest" => matches.into_iter().min_by_key(|(_, (_, _, stamp))| *stamp),
            "largest" => matches.into_iter().max_by_key(|(_, (_, size, _))| *size),
            "smallest" => matches.into_iter().min_by_key(|(_, (_, size, _))| *size),
            _ => matches.into_iter().next(),
        }.ok_or("No matching file could be selected")?;
        let path = selected.0;
        let size = selected.1.1;
        cx.set_variable(save_as.clone(), Value::String(path.clone()))?;
        (cx.log)("info", format!("Found file and saved path to {save_as} ({} bytes): {path}", size));
        Ok(Outcome::Satisfied)
    }
}

struct SetBrightness;
fn brightness_spec() -> Value {
    spec_action("Set Display Brightness", "sun", "Sets brightness on Windows monitors that expose the WMI brightness-control interface.", vec![
        json!({"key":"percent","label":"Brightness (%)","type":"number","min":0,"max":100,"step":1,"default":50}),
        json!({"preset":"onFail"})
    ], "Brightness: {percent}%", "Supported mainly by internal laptop displays; many external monitors do not expose brightness controls through WMI.")
}
#[async_trait]
impl Block for SetBrightness {
    fn title(&self) -> &'static str { "Set Display Brightness" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(brightness_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> { super::spec::validate(&brightness_spec(), c).map(|_| ()) }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let percent = num(&cx.cfg, "percent", 50.0, 0.0, 100.0, "Brightness")? as u8;
        let script = format!("$ErrorActionPreference='Stop'; $m=@(Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods -ErrorAction Stop); if($m.Count -eq 0){{throw 'No supported monitor brightness controls were found'}}; foreach($x in $m){{Invoke-CimMethod -InputObject $x -MethodName WmiSetBrightness -Arguments @{{Timeout=1;Brightness={percent}}} -ErrorAction Stop | Out-Null}}; 'Brightness updated on '+$m.Count+' monitor(s).");
        let out = extra::powershell(cx, &script).await?;
        (cx.log)("info", out);
        Ok(Outcome::Satisfied)
    }
}

struct SetPowerPlan;
fn power_plan_spec() -> Value {
    spec_action("Set Power Plan", "battery-charging", "Selects a Windows power plan using powercfg.", vec![
        json!({"key":"plan","label":"Power plan","type":"select","default":"balanced","options":[["balanced","Balanced"],["power_saver","Power saver"],["high_performance","High performance"],["ultimate","Ultimate performance"]]}),
        json!({"preset":"onFail"})
    ], "Power plan: {plan}", "Some editions do not include every plan. A missing plan returns an error instead of creating or enabling one silently.")
}
#[async_trait]
impl Block for SetPowerPlan {
    fn title(&self) -> &'static str { "Set Power Plan" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(power_plan_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&power_plan_spec(), c)?;
        if !matches!(txt(c, "plan").as_str(), "balanced"|"power_saver"|"high_performance"|"ultimate") { return Err("Unknown power plan".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let plan = txt(&cx.cfg, "plan");
        let guid = match plan.as_str() {
            "balanced" => "381b4222-f694-41f0-9685-ff5bb260df2e",
            "power_saver" => "a1841308-3541-4fab-bc81-f71556f20b4a",
            "high_performance" => "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c",
            _ => "e9a42b02-d5df-448d-aa00-03f14749eb61",
        };
        let script = format!("$ErrorActionPreference='Stop'; $plans=powercfg /list 2>&1 | Out-String; if($LASTEXITCODE -ne 0){{throw 'powercfg /list failed'}}; if($plans -notmatch [regex]::Escape('{}')){{throw 'Requested power plan is not available on this PC'}}; powercfg /setactive {}; if($LASTEXITCODE -ne 0){{throw 'powercfg could not activate the requested plan'}}; 'Power plan activated.'", guid, guid);
        let out = extra::powershell(cx, &script).await?;
        (cx.log)("info", format!("Power plan {plan}: {}", out.trim()));
        Ok(Outcome::Satisfied)
    }
}

struct AskUser;
fn ask_user_spec() -> Value {
    spec_action("Ask User / Input / Choice", "message-square", "Pauses the automation for confirmation, text input, or a choice and can save the answer to a workflow variable.", vec![
        json!({"key":"mode","label":"Dialog type","type":"select","default":"confirm","options":[["confirm","Yes / No confirmation"],["input","Text input"],["choice","Choose from options"]]}),
        json!({"key":"title","label":"Dialog title","type":"text","default":"Automation prompt","required":true,"maxLen":128}),
        json!({"key":"message","label":"Prompt","type":"textarea","rows":4,"required":true,"maxLen":2000}),
        json!({"key":"buttons","label":"Buttons","type":"select","default":"yes_no","options":[["yes_no","Yes / No"],["yes_no_cancel","Yes / No / Cancel"]],"show":{"key":"mode","eq":"confirm"}}),
        json!({"key":"defaultValue","label":"Default text","type":"text","maxLen":1000,"show":{"key":"mode","eq":"input"}}),
        json!({"key":"choicesJson","label":"Choices (JSON array of 2–100 strings)","type":"textarea","rows":4,"default":"[\"Option A\", \"Option B\"]","maxLen":8000,"show":{"key":"mode","eq":"choice"}}),
        json!({"key":"saveAs","label":"Save answer to variable","type":"text","required":true,"maxLen":64,"show":{"key":"mode","in":["input","choice"]}}),
        json!({"preset":"timeout","default":0}),
        json!({"preset":"onFail"})
    ], "Ask user: {mode}", "Requires an interactive Windows desktop. Confirmation is satisfied only by Yes; Cancel, No, and dismissing the dialog follow ELSE. Input/choice modes save the accepted answer to the configured per-run variable; cancelled dialogs do not set a value. Timeout 0 means wait indefinitely.")
}
#[async_trait]
impl Block for AskUser {
    fn title(&self) -> &'static str { "Ask User / Input / Choice" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(ask_user_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&ask_user_spec(), c)?;
        no_nul(c, &["title", "message", "defaultValue", "choicesJson", "saveAs"])?;
        let mode = txt(c, "mode");
        if !matches!(mode.as_str(), "confirm"|"input"|"choice") { return Err("Unknown dialog type".into()); }
        if mode == "confirm" && !matches!(txt(c, "buttons").as_str(), "yes_no"|"yes_no_cancel") {
            return Err("Unknown confirmation button set".into());
        }
        if matches!(mode.as_str(), "input"|"choice") && !valid_name(&txt(c, "saveAs")) {
            return Err("Answer variable name is invalid".into());
        }
        if mode == "choice" {
            let choices_text = c.get("choicesJson").and_then(Value::as_str).unwrap_or(r#"["Option A", "Option B"]"#);
            let options: Value = serde_json::from_str(choices_text).map_err(|e| format!("Choices must be a JSON array: {e}"))?;
            let arr = options.as_array().ok_or("Choices must be a JSON array")?;
            if !(2..=100).contains(&arr.len()) { return Err("Provide between 2 and 100 choices".into()); }
            let mut unique = std::collections::HashSet::new();
            for (i, option) in arr.iter().enumerate() {
                let Some(value) = option.as_str() else { return Err(format!("Choice {} must be text", i + 1)); };
                if value.trim().is_empty() || value.chars().count() > 128 { return Err(format!("Choice {} must be 1–128 characters", i + 1)); }
                if !unique.insert(value.to_lowercase()) { return Err("Choices must be unique (ignoring case)".into()); }
            }
        }
        num(c, "timeoutSec", 0.0, 0.0, 86400.0, "Timeout").map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let mode = txt(&cx.cfg, "mode");
        let title = template(cx, &txt(&cx.cfg, "title"))?;
        let message = template(cx, &raw(&cx.cfg, "message"))?;
        let script = match mode.as_str() {
            "confirm" => {
                let buttons = txt(&cx.cfg, "buttons");
                let style = if buttons == "yes_no" { "YesNo" } else { "YesNoCancel" };
                format!("$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; $r=[System.Windows.Forms.MessageBox]::Show({}, {}, [System.Windows.Forms.MessageBoxButtons]::{style}, [System.Windows.Forms.MessageBoxIcon]::Question); $s=[string]$r; [pscustomobject]@{{accepted=($s -eq 'Yes');cancelled=($s -eq 'Cancel');value=$s}} | ConvertTo-Json -Compress", extra::ps_quote(&message), extra::ps_quote(&title))
            },
            "input" => {
                let default_value = template(cx, &raw(&cx.cfg, "defaultValue"))?;
                format!("$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; $form=New-Object System.Windows.Forms.Form; $form.Text={}; $form.StartPosition='CenterScreen'; $form.FormBorderStyle='FixedDialog'; $form.MaximizeBox=$false; $form.MinimizeBox=$false; $form.ShowInTaskbar=$true; $form.ClientSize=New-Object System.Drawing.Size(500,175); $label=New-Object System.Windows.Forms.Label; $label.Text={}; $label.Location=New-Object System.Drawing.Point(12,12); $label.Size=New-Object System.Drawing.Size(476,48); $box=New-Object System.Windows.Forms.TextBox; $box.Location=New-Object System.Drawing.Point(12,68); $box.Size=New-Object System.Drawing.Size(476,24); $box.Text={}; $ok=New-Object System.Windows.Forms.Button; $ok.Text='OK'; $ok.Location=New-Object System.Drawing.Point(324,120); $ok.Size=New-Object System.Drawing.Size(78,30); $ok.DialogResult=[System.Windows.Forms.DialogResult]::OK; $cancel=New-Object System.Windows.Forms.Button; $cancel.Text='Cancel'; $cancel.Location=New-Object System.Drawing.Point(410,120); $cancel.Size=New-Object System.Drawing.Size(78,30); $cancel.DialogResult=[System.Windows.Forms.DialogResult]::Cancel; $form.Controls.AddRange(@($label,$box,$ok,$cancel)); $form.AcceptButton=$ok; $form.CancelButton=$cancel; $form.Add_Shown({{$box.Focus()}}); try{{$r=$form.ShowDialog(); $accepted=($r -eq [System.Windows.Forms.DialogResult]::OK); $value=if($accepted){{$box.Text}}else{{''}}; [pscustomobject]@{{accepted=$accepted;cancelled=(-not $accepted);value=[string]$value}} | ConvertTo-Json -Compress}}finally{{$form.Dispose()}}", extra::ps_quote(&title), extra::ps_quote(&message), extra::ps_quote(&default_value))
            },
            _ => {
                let choices_raw = cx.cfg.get("choicesJson").and_then(Value::as_str).unwrap_or(r#"["Option A", "Option B"]"#);
                let choices = template(cx, choices_raw)?;
                format!("$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; $choices=@(ConvertFrom-Json -InputObject {} -ErrorAction Stop); if($choices.Count -lt 2){{throw 'At least two choices are required'}}; $form=New-Object System.Windows.Forms.Form; $form.Text={}; $form.StartPosition='CenterScreen'; $form.FormBorderStyle='FixedDialog'; $form.MaximizeBox=$false; $form.MinimizeBox=$false; $form.ShowInTaskbar=$true; $form.ClientSize=New-Object System.Drawing.Size(500,195); $label=New-Object System.Windows.Forms.Label; $label.Text={}; $label.Location=New-Object System.Drawing.Point(12,12); $label.Size=New-Object System.Drawing.Size(476,60); $combo=New-Object System.Windows.Forms.ComboBox; $combo.DropDownStyle=[System.Windows.Forms.ComboBoxStyle]::DropDownList; $combo.Location=New-Object System.Drawing.Point(12,82); $combo.Size=New-Object System.Drawing.Size(476,26); foreach($choice in $choices){{[void]$combo.Items.Add([string]$choice)}}; $combo.SelectedIndex=0; $ok=New-Object System.Windows.Forms.Button; $ok.Text='OK'; $ok.Location=New-Object System.Drawing.Point(324,136); $ok.Size=New-Object System.Drawing.Size(78,30); $ok.DialogResult=[System.Windows.Forms.DialogResult]::OK; $cancel=New-Object System.Windows.Forms.Button; $cancel.Text='Cancel'; $cancel.Location=New-Object System.Drawing.Point(410,136); $cancel.Size=New-Object System.Drawing.Size(78,30); $cancel.DialogResult=[System.Windows.Forms.DialogResult]::Cancel; $form.Controls.AddRange(@($label,$combo,$ok,$cancel)); $form.AcceptButton=$ok; $form.CancelButton=$cancel; try{{$r=$form.ShowDialog(); $accepted=($r -eq [System.Windows.Forms.DialogResult]::OK); $value=if($accepted){{$combo.SelectedItem.ToString()}}else{{''}}; [pscustomobject]@{{accepted=$accepted;cancelled=(-not $accepted);value=[string]$value}} | ConvertTo-Json -Compress}}finally{{$form.Dispose()}}", extra::ps_quote(&choices), extra::ps_quote(&title), extra::ps_quote(&message))
            }
        };
        let out = extra::powershell(cx, &script).await?;
        let result: Value = serde_json::from_str(out.trim()).map_err(|e| format!("Could not read dialog result: {e}"))?;
        let accepted = result.get("accepted").and_then(Value::as_bool).unwrap_or(false);
        if mode == "confirm" {
            let answer = result.get("value").and_then(Value::as_str).unwrap_or("Cancel");
            (cx.log)("info", format!("Confirmation dialog response: {answer}"));
            return Ok(if accepted { Outcome::Satisfied } else { Outcome::NotSatisfied });
        }
        if accepted {
            let value = result.get("value").and_then(Value::as_str).unwrap_or("").to_string();
            let name = txt(&cx.cfg, "saveAs");
            cx.set_variable(name.clone(), Value::String(value.clone()))?;
            (cx.log)("info", format!("User answer saved to variable {name} ({} characters)", value.chars().count()));
            Ok(Outcome::Satisfied)
        } else {
            (cx.log)("info", "User cancelled the dialog; no variable was changed".into());
            Ok(Outcome::NotSatisfied)
        }
    }
}

struct RetryCommand;
fn retry_command_spec() -> Value {
    spec_action("Retry Command", "refresh-cw", "Runs a PowerShell command with bounded retries and exponential or fixed backoff.", vec![
        json!({"key":"command","label":"PowerShell command","type":"textarea","rows":5,"required":true,"maxLen":8000}),
        json!({"key":"attempts","label":"Total attempts","type":"number","min":1,"max":10,"step":1,"default":3}),
        json!({"key":"delaySec","label":"Initial delay (seconds)","type":"number","min":0,"max":3600,"step":0.5,"default":2}),
        json!({"key":"backoff","label":"Backoff","type":"select","default":"exponential","options":[["fixed","Fixed delay"],["exponential","Double delay after each failure"]]}),
        json!({"key":"saveOutputAs","label":"Save final stdout to variable (optional)","type":"text","maxLen":64}),
        json!({"preset":"timeout","default":0}),
        json!({"preset":"onFail"})
    ], "Retry command ({attempts} attempts)", "A command that succeeds exits through IF. After every failed attempt the block waits according to Backoff, then tries again. The command runs with current-user permissions and is not sandboxed.")
}
#[async_trait]
impl Block for RetryCommand {
    fn title(&self) -> &'static str { "Retry Command" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(retry_command_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        super::spec::validate(&retry_command_spec(), c)?;
        no_nul(c, &["command", "saveOutputAs"])?;
        if txt(c, "command").is_empty() { return Err("PowerShell command is required".into()); }
        if !txt(c, "saveOutputAs").is_empty() && !valid_name(&txt(c, "saveOutputAs")) { return Err("Output variable name is invalid".into()); }
        let attempts = num(c, "attempts", 3.0, 1.0, 10.0, "Attempts")?;
        if attempts.fract() != 0.0 { return Err("Attempts must be a whole number".into()); }
        num(c, "delaySec", 2.0, 0.0, 3600.0, "Initial delay")?;
        if !matches!(txt(c, "backoff").as_str(), "fixed"|"exponential") { return Err("Unknown backoff mode".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let command = template(cx, &raw(&cx.cfg, "command"))?;
        let attempts = num(&cx.cfg, "attempts", 3.0, 1.0, 10.0, "Attempts")? as u32;
        let delay = num(&cx.cfg, "delaySec", 2.0, 0.0, 3600.0, "Initial delay")?;
        let backoff = txt(&cx.cfg, "backoff");
        let save_as = txt(&cx.cfg, "saveOutputAs");
        let mut last_error = String::new();
        for attempt in 1..=attempts {
            if cx.token.is_cancelled() { return Err("cancelled".into()); }
            (cx.report)("running", Some((attempt - 1) as f64 / attempts as f64), format!("Command attempt {attempt} of {attempts}"));
            let script = format!("$ErrorActionPreference='Stop'; try {{\n{}\n}} catch {{ [Console]::Error.WriteLine($_.ToString()); exit 1 }}; if ($null -ne $LASTEXITCODE) {{ exit $LASTEXITCODE }}", command);
            match extra::powershell(cx, &script).await {
                Ok(out) => {
                    if !save_as.is_empty() { cx.set_variable(save_as.clone(), Value::String(out.clone()))?; }
                    (cx.log)("info", format!("Retry Command succeeded on attempt {attempt}/{attempts}"));
                    return Ok(Outcome::Satisfied);
                },
                Err(e) => {
                    last_error = e;
                    (cx.log)("warn", format!("Command attempt {attempt}/{attempts} failed: {}", last_error.chars().take(400).collect::<String>()));
                    if attempt < attempts {
                        let wait = if backoff == "exponential" { delay * 2f64.powi((attempt - 1) as i32) } else { delay };
                        if wait > 0.0 {
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs_f64(wait.min(86400.0))) => {},
                                _ = cx.token.cancelled() => return Err("cancelled".into()),
                            }
                        }
                    }
                }
            }
        }
        Err(format!("Command failed after {attempts} attempts: {}", last_error.chars().take(500).collect::<String>()))
    }
}

pub(super) fn register_into(m: &mut HashMap<&'static str, Box<dyn Block>>) {
    m.insert("cond.file_watcher", Box::new(FileWatcher));
    m.insert("cond.file_content", Box::new(FileContent));
    m.insert("cond.http_status", Box::new(HttpStatus));
    m.insert("cond.window_state", Box::new(WindowState));
    m.insert("act.set_variable", Box::new(SetVariable));
    m.insert("act.get_variable", Box::new(GetVariable));
    m.insert("act.increment_variable", Box::new(IncrementVariable));
    m.insert("act.clear_variable", Box::new(ClearVariable));
    m.insert("act.keyboard_shortcut", Box::new(KeyboardShortcut));
    m.insert("act.mouse", Box::new(MouseAutomation));
    m.insert("act.extract_zip", Box::new(ExtractZip));
    m.insert("act.bulk_files", Box::new(BulkFiles));
    m.insert("act.set_brightness", Box::new(SetBrightness));
    m.insert("act.power_plan", Box::new(SetPowerPlan));
    m.insert("act.ask_user", Box::new(AskUser));
    m.insert("act.retry_command", Box::new(RetryCommand));
    m.insert("act.find_file", Box::new(FindFile));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn all_advanced_blocks_are_registered() {
        let expected: [(&str, Cat); 17] = [
            ("cond.file_watcher", Cat::Condition), ("cond.file_content", Cat::Condition),
            ("cond.http_status", Cat::Condition), ("cond.window_state", Cat::Condition),
            ("act.set_variable", Cat::Action), ("act.get_variable", Cat::Action),
            ("act.increment_variable", Cat::Action), ("act.clear_variable", Cat::Action),
            ("act.keyboard_shortcut", Cat::Action), ("act.mouse", Cat::Action),
            ("act.extract_zip", Cat::Action), ("act.bulk_files", Cat::Action),
            ("act.set_brightness", Cat::Action), ("act.power_plan", Cat::Action),
            ("act.ask_user", Cat::Action), ("act.retry_command", Cat::Action),
            ("act.find_file", Cat::Action),
        ];
        let registry = super::super::registry();
        for (id, category) in expected {
            let b = registry.get(id).unwrap_or_else(|| panic!("{id} missing"));
            assert_eq!(b.category(), category, "{id}");
            assert!(b.spec().is_some(), "{id} missing declarative UI spec");
        }
    }

    #[test]
    fn file_watcher_supports_existence_and_event_modes() {
        assert!(FileWatcher.validate(&json!({"path":"C:\\\\Temp","pattern":"*.log","itemType":"files","recursive":false,"event":"exists","behavior":"check","onFail":"stop"})).is_ok());
        assert!(FileWatcher.validate(&json!({"path":"C:\\\\Temp","pattern":"*","itemType":"both","recursive":true,"event":"modified","behavior":"wait","pollSec":1,"stableForSec":0,"timeoutSec":30,"onFail":"stop"})).is_ok());
        assert!(FileWatcher.validate(&json!({"path":"C:\\\\Temp","pattern":"*","itemType":"other","event":"exists","behavior":"check","onFail":"stop"})).is_err());
    }

    #[test]
    fn workflow_variables_expand_and_missing_values_fail_closed() {
        let cx = NodeCx {
            cfg: json!({}),
            token: tokio_util::sync::CancellationToken::new(),
            visit: 1,
            report: Box::new(|_: &str, _: Option<f64>, _: String| {}),
            log: Box::new(|_: &str, _: String| {}),
            resolve: Box::new(|| Ok(Vec::new())),
            visible: Box::new(|| false),
            variables: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            secret_reader: std::sync::Arc::new(|name: &str| Err(format!("secret {name} unavailable in test"))),
            sensitive_values: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            subflow_return: std::sync::Arc::new(std::sync::Mutex::new(None)),
        };
        cx.set_variable("folder".into(), json!("C:\\Temp\\logs")).unwrap();
        cx.set_variable("count".into(), json!(3)).unwrap();
        assert_eq!(cx.expand_template("{{folder}}\\run-{{count}}.txt").unwrap(), "C:\\Temp\\logs\\run-3.txt");
        assert!(cx.expand_template("{{unset}}").is_err());
    }

    #[test]
    fn variable_names_and_wildcards_are_validated() {
        assert!(valid_name("file.count_1"));
        assert!(!valid_name("1bad"));
        assert!(!valid_name("bad name"));
        assert!(wildcard_match("*.log", "run.LOG"));
        assert!(wildcard_match("report-??.csv", "report-01.csv"));
        assert!(!wildcard_match("report-??.csv", "report-001.csv"));
        assert!(comparison_matches("at_least", 2, 2));
    }

    fn comparison_matches(op: &str, a: u64, b: u64) -> bool {
        match op { "equal" => a == b, "not_equal" => a != b, "greater" => a > b, "at_least" => a >= b, "less" => a < b, "at_most" => a <= b, _ => false }
    }

    #[test]
    fn advanced_block_validation_rejects_bad_values() {
        assert!(SetVariable.validate(&json!({"name":"ok","source":"literal","valueType":"number","value":"42","onFail":"stop"})).is_ok());
        assert!(SetVariable.validate(&json!({"name":"1bad","source":"literal","valueType":"text","value":"42","onFail":"stop"})).is_err());
        assert!(IncrementVariable.validate(&json!({"name":"count","amount":1,"createIfMissing":true,"onFail":"stop"})).is_ok());
        assert!(KeyboardShortcut.validate(&json!({"shortcut":"^s","delayMs":100,"onFail":"stop"})).is_ok());
        assert!(MouseAutomation.validate(&json!({"operation":"click","coordinateSpace":"window","processName":"notepad","x":10,"y":20,"onFail":"stop"})).is_ok());
        assert!(MouseAutomation.validate(&json!({"operation":"click","coordinateSpace":"window","x":10,"y":20,"onFail":"stop"})).is_err());
        assert!(BulkFiles.validate(&json!({"sourceFolder":"C:\\Temp","pattern":"*.log","operation":"recycle","recursive":false,"collision":"fail","maxItems":100,"previewOnly":true,"onFail":"stop"})).is_ok());

        assert!(ExtractZip.validate(&json!({"sourcePath":"C:\\\\Temp\\\\a.zip","destinationPath":"C:\\\\Temp\\\\out","collision":"fail","onFail":"stop"})).is_ok());
        assert!(ExtractZip.validate(&json!({"sourcePath":"C:\\\\Temp\\\\a.txt","destinationPath":"C:\\\\Temp\\\\out","collision":"fail","onFail":"stop"})).is_err());
        assert!(HttpStatus.validate(&json!({"url":"https://example.com","method":"GET","mode":"success","expectedStatus":200,"behavior":"check","pollSec":2,"stableForSec":0,"timeoutSec":0,"onFail":"stop"})).is_ok());
        assert!(HttpStatus.validate(&json!({"url":"file:///C:/secret.txt","method":"GET","mode":"success","expectedStatus":200,"onFail":"stop"})).is_err());
        assert!(RetryCommand.validate(&json!({"command":"Get-Date","attempts":3,"delaySec":1,"backoff":"exponential","saveOutputAs":"lastOutput","timeoutSec":30,"onFail":"stop"})).is_ok());
        assert!(FindFile.validate(&json!({"searchFolder":"C:\\\\Temp","pattern":"*.log","recursive":true,"selection":"newest","saveAs":"latestLog","onFail":"stop"})).is_ok());
        assert!(FindFile.validate(&json!({"searchFolder":"C:\\\\Temp","pattern":"*.log","recursive":true,"selection":"newest","saveAs":"1bad","onFail":"stop"})).is_err());
        assert!(AskUser.validate(&json!({"mode":"confirm","title":"Proceed?","message":"Continue?","buttons":"yes_no","timeoutSec":10,"onFail":"stop"})).is_ok());
        assert!(AskUser.validate(&json!({"mode":"input","title":"Name","message":"Enter a name","defaultValue":"default","saveAs":"answer","timeoutSec":0,"onFail":"stop"})).is_ok());
        assert!(AskUser.validate(&json!({"mode":"choice","title":"Pick","message":"Choose one","choicesJson":"[\"A\",\"B\"]","saveAs":"choice","timeoutSec":10,"onFail":"stop"})).is_ok());
        assert!(AskUser.validate(&json!({"mode":"choice","title":"Pick","message":"Choose one","choicesJson":"[\"A\"]","saveAs":"choice","timeoutSec":10,"onFail":"stop"})).is_err());
    }
}
