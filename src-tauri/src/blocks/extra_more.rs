//! Additional utility automation conditions and actions.
use super::{flag, rng, spec, text, Block, Cat, NodeCx, Outcome};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime},
};
use tokio::io::AsyncReadExt;

const OUTPUT_LIMIT: usize = 16 * 1024;

fn raw_text(c: &Value, key: &str) -> String {
    c.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\0', "").replace('\'', "''"))
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin + Send>(reader: &mut R) -> Result<String, String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = reader.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 { break; }
        let keep = OUTPUT_LIMIT.saturating_sub(out.len()).min(n);
        out.extend_from_slice(&buf[..keep]);
    }
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

async fn powershell(cx: &NodeCx, script: &str) -> Result<String, String> {
    #[cfg(not(windows))]
    {
        let _ = (cx, script);
        return Err("This built-in block is supported only on Windows.".into());
    }
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        {
            use std::os::windows::process::CommandExt;
            cmd.as_std_mut().creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().map_err(|e| format!("Could not start PowerShell: {e}"))?;
        let mut stdout = child.stdout.take().ok_or("PowerShell stdout unavailable")?;
        let mut stderr = child.stderr.take().ok_or("PowerShell stderr unavailable")?;
        let out_task = tokio::spawn(async move { read_capped(&mut stdout).await });
        let err_task = tokio::spawn(async move { read_capped(&mut stderr).await });
        let status = tokio::select! {
            s = child.wait() => s.map_err(|e| format!("PowerShell wait failed: {e}"))?,
            _ = cx.token.cancelled() => return Err("cancelled".into()),
        };
        let out = out_task.await.map_err(|e| e.to_string())??;
        let err = err_task.await.map_err(|e| e.to_string())??;
        if !status.success() {
            let detail: String = err.chars().take(600).collect();
            return Err(format!("PowerShell command failed with exit code {}{}", status.code().unwrap_or(-1), if detail.is_empty() { String::new() } else { format!(": {detail}") }));
        }
        if !err.is_empty() {
            (cx.log)("warn", format!("PowerShell stderr: {}", err.chars().take(1000).collect::<String>()));
        }
        Ok(out)
    }
}

async fn ps_bool(cx: &NodeCx, script: &str) -> Result<bool, String> {
    let out = powershell(cx, script).await?;
    serde_json::from_str::<bool>(out.trim()).map_err(|e| format!("Could not parse Windows condition result: {e}"))
}

fn condition_spec(title: &str, icon: &str, description: &str, fields: Vec<Value>, summary: &str, help: &str) -> Value {
    json!({"category":"condition","title":title,"icon":icon,"description":description,"fields":fields,"summary":summary,"help":help})
}

fn action_spec(title: &str, icon: &str, description: &str, fields: Vec<Value>, summary: &str, help: &str) -> Value {
    json!({"category":"action","title":title,"icon":icon,"description":description,"fields":fields,"summary":summary,"help":help})
}

fn comparison_matches(op: &str, actual: u64, expected: u64) -> bool {
    match op {
        "equal" => actual == expected,
        "not_equal" => actual != expected,
        "greater" => actual > expected,
        "at_least" => actual >= expected,
        "less" => actual < expected,
        "at_most" => actual <= expected,
        _ => false,
    }
}

fn count_comparison_fields(label: &str) -> Vec<Value> {
    vec![
        json!({"key":"comparison","label":"Comparison","type":"select","default":"at_least",
          "options":[["equal","Equal to"],["not_equal","Not equal to"],["greater","Greater than"],["at_least","At least"],["less","Less than"],["at_most","At most"]]}),
        json!({"key":"count","label":label,"type":"number","min":0,"max":1000000,"step":1,"default":1}),
        json!({"preset":"onFail"})
    ]
}

// Conditions -----------------------------------------------------------------

struct ProcessCount;
fn process_count_spec() -> Value {
    condition_spec("Process Count", "layers", "Checks how many instances of a process are running.", vec![
        json!({"key":"processName","label":"Process name","type":"text","required":true,"maxLen":260}),
        json!({"key":"comparison","label":"Comparison","type":"select","default":"at_least","options":[["equal","Equal to"],["not_equal","Not equal to"],["greater","Greater than"],["at_least","At least"],["less","Less than"],["at_most","At most"]]}),
        json!({"key":"count","label":"Expected instances","type":"number","min":0,"max":100000,"step":1,"default":1}),
    ], "Processes {processName}: {comparison} {count}", "Use the process name without .exe. This condition can check once or poll until the count matches.")
}
#[async_trait]
impl Block for ProcessCount {
    fn title(&self) -> &'static str { "Process Count" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(process_count_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&process_count_spec(), c)?;
        if text(c, "processName").contains('\0') { return Err("Process name must not contain NUL characters".into()); }
        let cmp = text(c, "comparison");
        if !matches!(cmp.as_str(), "equal"|"not_equal"|"greater"|"at_least"|"less"|"at_most") { return Err("Unknown process-count comparison".into()); }
        let n = rng(c, "count", 1.0, 0.0, 100000.0, "Expected process count")?;
        if n.fract() != 0.0 { return Err("Expected process count must be a whole number".into()); }
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let name = template(cx, &text(&cx.cfg, "processName"))?;
        let expected = rng(&cx.cfg, "count", 1.0, 0.0, 100000.0, "Expected process count")? as u64;
        let comparison = text(&cx.cfg, "comparison");
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let name = name.clone(); let comparison = comparison.clone();
            Box::pin(async move {
                let script = format!("$ErrorActionPreference='Stop'; $n={}; $p=@(Get-Process -Name $n -ErrorAction SilentlyContinue | Where-Object {{$_.ProcessName -eq $n}}); Write-Output ([int]$p.Count)", ps_quote(&name));
                let out = powershell(cx, &script).await?;
                let actual = out.trim().parse::<u64>().map_err(|e| format!("Invalid process count: {e}"))?;
                (cx.report)("waiting", None, format!("Process count for {name}: {actual}"));
                Ok(comparison_matches(&comparison, actual, expected))
            }) as extra::Probe<'_>
        }).await
    }
}

struct ServiceState;
fn service_state_spec() -> Value {
    condition_spec("Service State", "settings", "Checks the status of a Windows service by service name.", vec![
        json!({"key":"serviceName","label":"Service name","type":"text","required":true,"maxLen":256}),
        json!({"key":"state","label":"Expected state","type":"select","default":"Running","options":[["Running","Running"],["Stopped","Stopped"],["Paused","Paused"],["StartPending","Start pending"],["StopPending","Stop pending"],["Missing","Not installed / missing"]]}),
    ], "Service {serviceName}: {state}", "Enter the service name rather than its display name. You can wait for the service to reach a desired state.")
}
#[async_trait]
impl Block for ServiceState {
    fn title(&self) -> &'static str { "Service State" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(service_state_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&service_state_spec(), c)?;
        if text(c, "serviceName").contains('\0') { return Err("Service name must not contain NUL characters".into()); }
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let name = template(cx, &text(&cx.cfg, "serviceName"))?;
        let expected = text(&cx.cfg, "state");
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let name = name.clone(); let expected = expected.clone();
            Box::pin(async move {
                let script = format!("$ErrorActionPreference='Stop'; $n={}; $s=@(Get-Service -Name $n -ErrorAction SilentlyContinue | Where-Object {{$_.Name -eq $n}}); if($s.Count -eq 0){{'Missing'}}else{{$s[0].Status.ToString()}}", ps_quote(&name));
                let state = powershell(cx, &script).await?;
                let matches = state.trim().eq_ignore_ascii_case(&expected);
                (cx.report)("waiting", None, format!("Service {name}: {}", state.trim()));
                Ok(matches)
            }) as extra::Probe<'_>
        }).await
    }
}

struct ClipboardText;
fn clipboard_spec() -> Value {
    condition_spec("Clipboard Text", "clipboard", "Checks clipboard text without logging its contents.", vec![
        json!({"key":"mode","label":"Match","type":"select","default":"contains","options":[["empty","Is empty"],["equal","Equals text"],["contains","Contains text"],["starts","Starts with text"]]}),
        json!({"key":"text","label":"Text to compare","type":"text","maxLen":4000,"show":{"key":"mode","eq":"equal"}}),
        json!({"key":"containsText","label":"Text to find","type":"text","maxLen":4000,"show":{"key":"mode","eq":"contains"}}),
        json!({"key":"startsText","label":"Required prefix","type":"text","maxLen":4000,"show":{"key":"mode","eq":"starts"}}),
    ], "Clipboard {mode}", "Only the comparison result is logged. Requires an interactive Windows user session.")
}
#[async_trait]
impl Block for ClipboardText {
    fn title(&self) -> &'static str { "Clipboard Text" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(clipboard_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&clipboard_spec(), c)?;
        let mode = text(c, "mode");
        if !matches!(mode.as_str(), "empty"|"equal"|"contains"|"starts") { return Err("Unknown clipboard match mode".into()); }
        let key = match mode.as_str() { "equal" => "text", "contains" => "containsText", "starts" => "startsText", _ => "" };
        if !key.is_empty() && raw_text(c, key).is_empty() { return Err("Text to compare is required for this match mode".into()); }
        for key in ["text", "containsText", "startsText"] {
            if raw_text(c, key).contains('\0') { return Err("Clipboard comparison text must not contain NUL characters".into()); }
        }
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let mode = text(&cx.cfg, "mode");
        let expected_raw = match mode.as_str() { "equal" => raw_text(&cx.cfg, "text"), "contains" => raw_text(&cx.cfg, "containsText"), "starts" => raw_text(&cx.cfg, "startsText"), _ => String::new() };
        let expected = template(cx, &expected_raw)?;
        let script = format!(
            "$ErrorActionPreference='Stop'; $v=[string](Get-Clipboard -Raw -ErrorAction Stop); $e={}; $m={}; $ok=switch($m){{'empty'{{[string]::IsNullOrEmpty($v)}}'equal'{{[string]::Equals($v,$e,[System.StringComparison]::OrdinalIgnoreCase)}}'contains'{{$v.IndexOf($e,[System.StringComparison]::OrdinalIgnoreCase) -ge 0}}'starts'{{$v.StartsWith($e,[System.StringComparison]::OrdinalIgnoreCase)}}default{{$false}}}}; [bool]$ok | ConvertTo-Json -Compress",
            ps_quote(&expected), ps_quote(&mode));
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let script = script.clone(); let mode = mode.clone();
            Box::pin(async move {
                let matches = extra::ps_bool(cx, &script).await?;
                (cx.report)("waiting", None, format!("Clipboard text condition ({mode}): {matches}"));
                Ok(matches)
            }) as extra::Probe<'_>
        }).await
    }
}

struct EnvironmentVariable;
fn environment_spec() -> Value {
    condition_spec("Environment Variable", "variable", "Checks whether a process, user, or machine environment variable matches the configured condition.", vec![
        json!({"key":"name","label":"Variable name","type":"text","required":true,"maxLen":256}),
        json!({"key":"scope","label":"Scope","type":"select","default":"Process","options":[["Process","Current process"],["User","Current user"],["Machine","Local machine"]]}),
        json!({"key":"mode","label":"Match","type":"select","default":"exists","options":[["exists","Exists / is defined"],["equal","Equals text"],["contains","Contains text"]]}),
        json!({"key":"expected","label":"Expected value","type":"text","maxLen":4000,"show":{"key":"mode","eq":"equal"}}),
        json!({"key":"containsValue","label":"Text to find","type":"text","maxLen":4000,"show":{"key":"mode","eq":"contains"}}),
    ], "Environment variable {name}", "The variable's contents are not logged. Machine scope is read-only.")
}
#[async_trait]
impl Block for EnvironmentVariable {
    fn title(&self) -> &'static str { "Environment Variable" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(environment_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&environment_spec(), c)?;
        for key in ["name", "expected", "containsValue"] {
            if raw_text(c, key).contains('\0') { return Err("Environment-variable fields must not contain NUL characters".into()); }
        }
        if text(c, "name").is_empty() { return Err("Variable name is required".into()); }
        if text(c, "name").contains('=') { return Err("Environment-variable names cannot contain an equals sign".into()); }
        let mode = text(c, "mode");
        if !matches!(mode.as_str(), "exists"|"equal"|"contains") { return Err("Unknown environment-variable match mode".into()); }
        if mode == "equal" && c.get("expected").and_then(Value::as_str).is_none() { return Err("Expected value is required".into()); }
        if mode == "contains" && raw_text(c, "containsValue").is_empty() { return Err("Text to find is required".into()); }
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let name = template(cx, &text(&cx.cfg, "name"))?;
        let scope = text(&cx.cfg, "scope");
        let mode = text(&cx.cfg, "mode");
        let expected_raw = match mode.as_str() { "equal" => raw_text(&cx.cfg, "expected"), "contains" => raw_text(&cx.cfg, "containsValue"), _ => String::new() };
        let expected = template(cx, &expected_raw)?;
        let script = format!(
            "$ErrorActionPreference='Stop'; $n={}; $target=[System.EnvironmentVariableTarget]::{scope}; $v=[Environment]::GetEnvironmentVariable($n,$target); $ok=switch({}){{'exists'{{$null -ne $v}}'equal'{{[string]::Equals([string]$v,{},[System.StringComparison]::OrdinalIgnoreCase)}}'contains'{{$null -ne $v -and ([string]$v).IndexOf({},[System.StringComparison]::OrdinalIgnoreCase) -ge 0}}default{{$false}}}}; [bool]$ok | ConvertTo-Json -Compress",
            ps_quote(&name), ps_quote(&mode), ps_quote(&expected), ps_quote(&expected));
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let script = script.clone(); let name = name.clone(); let scope = scope.clone(); let mode = mode.clone();
            Box::pin(async move {
                let matches = extra::ps_bool(cx, &script).await?;
                (cx.report)("waiting", None, format!("Environment variable {name} ({scope}/{mode}): {matches}"));
                Ok(matches)
            }) as extra::Probe<'_>
        }).await
    }
}

struct FileAge;
fn file_age_spec() -> Value {
    condition_spec("File Age", "calendar-clock", "Checks a file or directory's last-modified age in minutes.", vec![
        json!({"key":"path","label":"File or directory path","type":"text","required":true,"maxLen":2048}),
        json!({"key":"mode","label":"Age test","type":"select","default":"older","options":[["older","At least this old"],["newer","Newer than this age"]]}),
        json!({"key":"minutes","label":"Age (minutes)","type":"number","min":0,"max":5256000,"step":1,"default":60}),
    ], "File age: {path}", "A missing path is not satisfied. Use wait-until when waiting for a file to become old enough or sufficiently recent.")
}
#[async_trait]
impl Block for FileAge {
    fn title(&self) -> &'static str { "File Age" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(file_age_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&file_age_spec(), c)?;
        if text(c, "path").contains('\0') { return Err("Path must not contain NUL characters".into()); }
        if !matches!(text(c, "mode").as_str(), "older"|"newer") { return Err("Unknown file-age mode".into()); }
        rng(c, "minutes", 60.0, 0.0, 5256000.0, "Age (minutes)")?;
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let path = template(cx, &text(&cx.cfg, "path"))?;
        let mode = text(&cx.cfg, "mode");
        let limit = rng(&cx.cfg, "minutes", 60.0, 0.0, 5256000.0, "Age (minutes)")?;
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let path = path.clone(); let mode = mode.clone();
            Box::pin(async move {
                let meta = match fs::metadata(&path) { Ok(m) => m, Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false), Err(e) => return Err(format!("Could not inspect path: {e}")) };
                let modified = meta.modified().map_err(|e| format!("Could not read last-modified time: {e}"))?;
                let age = SystemTime::now().duration_since(modified).unwrap_or(Duration::ZERO).as_secs_f64() / 60.0;
                let matches = if mode == "older" { age >= limit } else { age < limit };
                (cx.report)("waiting", None, format!("File age for {path}: {:.1} minutes", age));
                Ok(matches)
            }) as extra::Probe<'_>
        }).await
    }
}

struct DirectoryCount;
fn directory_count_spec() -> Value {
    condition_spec("Directory Item Count", "folder-tree", "Counts files, folders, or both in a directory.", vec![
        json!({"key":"path","label":"Directory path","type":"text","required":true,"maxLen":2048}),
        json!({"key":"kind","label":"Count","type":"select","default":"files","options":[["files","Files"],["folders","Folders"],["both","Files and folders"]]}),
        json!({"key":"recursive","label":"Include subdirectories","type":"bool","default":false}),
        json!({"key":"comparison","label":"Comparison","type":"select","default":"at_least","options":[["equal","Equal to"],["not_equal","Not equal to"],["greater","Greater than"],["at_least","At least"],["less","Less than"],["at_most","At most"]]}),
        json!({"key":"count","label":"Expected count","type":"number","min":0,"max":1000000,"step":1,"default":1}),
    ], "Directory item count: {path}", "Recursive scans may take time on large directories. Use wait-until to wait for a directory to reach the expected count.")
}
#[async_trait]
impl Block for DirectoryCount {
    fn title(&self) -> &'static str { "Directory Item Count" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(directory_count_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&directory_count_spec(), c)?;
        if text(c, "path").contains('\0') { return Err("Path must not contain NUL characters".into()); }
        if !matches!(text(c, "kind").as_str(), "files"|"folders"|"both") { return Err("Unknown directory item type".into()); }
        let cmp = text(c, "comparison");
        if !matches!(cmp.as_str(), "equal"|"not_equal"|"greater"|"at_least"|"less"|"at_most") { return Err("Unknown item-count comparison".into()); }
        let n = rng(c, "count", 1.0, 0.0, 1000000.0, "Expected count")?;
        if n.fract() != 0.0 { return Err("Expected count must be a whole number".into()); }
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let path = template(cx, &text(&cx.cfg, "path"))?;
        let kind = text(&cx.cfg, "kind");
        let recurse = flag(&cx.cfg, "recursive", false);
        let enumerate = if recurse {
            "Get-ChildItem -LiteralPath $p -Force -Recurse -ErrorAction Stop"
        } else {
            "Get-ChildItem -LiteralPath $p -Force -ErrorAction Stop"
        };
        let filter = match kind.as_str() {
            "files" => "$items=@($items | Where-Object { -not $_.PSIsContainer });",
            "folders" => "$items=@($items | Where-Object { $_.PSIsContainer });",
            _ => ""
        };
        let script = format!("$ErrorActionPreference='Stop'; $p={}; if(-not (Test-Path -LiteralPath $p -PathType Container)){{throw 'Directory does not exist'}}; $items=@({enumerate}); {filter} Write-Output ([int]$items.Count)", ps_quote(&path));
        let expected = rng(&cx.cfg, "count", 1.0, 0.0, 1000000.0, "Expected count")? as u64;
        let comparison = text(&cx.cfg, "comparison");
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let script = script.clone(); let path = path.clone(); let comparison = comparison.clone();
            Box::pin(async move {
                let out = powershell(cx, &script).await?;
                let actual = out.trim().parse::<u64>().map_err(|e| format!("Invalid directory item count: {e}"))?;
                let matches = comparison_matches(&comparison, actual, expected);
                (cx.report)("waiting", None, format!("Directory item count for {path}: {actual}"));
                Ok(matches)
            }) as extra::Probe<'_>
        }).await
    }
}

// Actions --------------------------------------------------------------------

struct CreateFolder;
fn create_folder_spec() -> Value {
    action_spec("Create Folder", "folder-plus", "Creates a directory and any missing parent directories.", vec![
        json!({"key":"path","label":"New folder path","type":"text","required":true,"maxLen":2048}),
        json!({"preset":"onFail"})
    ], "Create folder: {path}", "Already-existing directories are accepted. This does not delete or overwrite existing contents.")
}
#[async_trait]
impl Block for CreateFolder {
    fn title(&self) -> &'static str { "Create Folder" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(create_folder_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&create_folder_spec(), c)?;
        if text(c, "path").contains('\0') { return Err("Path must not contain NUL characters".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let path = template(cx, &text(&cx.cfg, "path"))?;
        if Path::new(&path).exists() && !Path::new(&path).is_dir() { return Err("A file already exists at the requested folder path".into()); }
        fs::create_dir_all(&path).map_err(|e| format!("Could not create folder: {e}"))?;
        (cx.log)("info", format!("Folder is ready: {path}"));
        Ok(Outcome::Satisfied)
    }
}

struct WriteTextFile;
fn write_text_spec() -> Value {
    action_spec("Write Text File", "file-pen", "Creates or updates a UTF-8 text file, replacing its contents or appending text.", vec![
        json!({"key":"path","label":"File path","type":"text","required":true,"maxLen":2048}),
        json!({"key":"mode","label":"Write mode","type":"select","default":"overwrite","options":[["overwrite","Replace file contents"],["append","Append to file"]]}),
        json!({"key":"content","label":"Text to write","type":"textarea","rows":6,"required":true,"maxLen":32768}),
        json!({"preset":"onFail"})
    ], "Write text file: {path}", "The parent directory must already exist. The text content is not copied to workflow logs. Appending does not add a newline automatically.")
}
#[async_trait]
impl Block for WriteTextFile {
    fn title(&self) -> &'static str { "Write Text File" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(write_text_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&write_text_spec(), c)?;
        if text(c, "path").contains('\0') { return Err("Path must not contain NUL characters".into()); }
        if !matches!(text(c, "mode").as_str(), "overwrite"|"append") { return Err("Unknown text-file write mode".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let path = template(cx, &text(&cx.cfg, "path"))?;
        let content = template(cx, cx.cfg.get("content").and_then(Value::as_str).unwrap_or(""))?;
        let mode = text(&cx.cfg, "mode");
        let mut opts = OpenOptions::new();
        if mode == "append" { opts.create(true).append(true); } else { opts.create(true).write(true).truncate(true); }
        let mut file = opts.open(&path).map_err(|e| format!("Could not open output file: {e}"))?;
        file.write_all(content.as_bytes()).map_err(|e| format!("Could not write text file: {e}"))?;
        (cx.log)("info", format!("Wrote {} UTF-8 bytes to {path} (mode: {mode})", content.len()));
        Ok(Outcome::Satisfied)
    }
}

struct ZipArchive;
fn zip_spec() -> Value {
    action_spec("Create ZIP Archive", "file-archive", "Compresses a file or folder into a ZIP archive using Windows Compress-Archive.", vec![
        json!({"key":"sourcePath","label":"File or folder to compress","type":"text","required":true,"maxLen":2048}),
        json!({"key":"destinationPath","label":"ZIP destination path","type":"text","required":true,"maxLen":2048}),
        json!({"key":"collision","label":"If destination exists","type":"select","default":"fail","options":[["fail","Fail without replacing"],["replace","Replace existing archive"]]}),
        json!({"preset":"onFail"})
    ], "Create ZIP: {destinationPath}", "The destination's parent directory must exist. A destination inside the source folder is rejected to prevent self-inclusion. Replacing an existing ZIP overwrites it.")
}
fn archive_destination(path: &Path) -> Result<PathBuf, String> {
    let name = path.file_name().ok_or("Destination must include a file name")?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let canonical_parent = fs::canonicalize(parent).map_err(|e| format!("Destination parent must exist: {e}"))?;
    Ok(canonical_parent.join(name))
}
#[async_trait]
impl Block for ZipArchive {
    fn title(&self) -> &'static str { "Create ZIP Archive" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(zip_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&zip_spec(), c)?;
        for key in ["sourcePath", "destinationPath"] {
            if text(c, key).contains('\0') { return Err("Archive paths must not contain NUL characters".into()); }
        }
        if !text(c, "destinationPath").to_ascii_lowercase().ends_with(".zip") { return Err("ZIP destination must end in .zip".into()); }
        if !matches!(text(c, "collision").as_str(), "fail"|"replace") { return Err("Unknown destination collision mode".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let source = PathBuf::from(template(cx, &text(&cx.cfg, "sourcePath"))?);
        let destination = PathBuf::from(template(cx, &text(&cx.cfg, "destinationPath"))?);
        if !source.exists() { return Err("Archive source does not exist".into()); }
        let source_abs = fs::canonicalize(&source).map_err(|e| format!("Could not resolve source path: {e}"))?;
        let destination_abs = archive_destination(&destination)?;
        if source_abs == destination_abs { return Err("Source and ZIP destination must be different paths".into()); }
        if source_abs.is_dir() && destination_abs.starts_with(&source_abs) {
            return Err("ZIP destination cannot be inside the source folder".into());
        }
        let destination_text = destination.to_string_lossy().to_string();
        let source_text = source.to_string_lossy().to_string();
        let replace = text(&cx.cfg, "collision") == "replace";
        if destination.exists() && !replace { return Err("ZIP destination already exists; choose Replace or a different path".into()); }
        let script = format!("$ErrorActionPreference='Stop'; $s={}; $d={}; if(-not (Test-Path -LiteralPath $s)){{throw 'Source does not exist'}}; if((Test-Path -LiteralPath $d) -and -not {}){{throw 'Destination already exists'}}; Compress-Archive -LiteralPath $s -DestinationPath $d -Force:{} -ErrorAction Stop; 'ZIP archive created.'", ps_quote(&source_text), ps_quote(&destination_text), if replace { "$true" } else { "$false" }, if replace { "$true" } else { "$false" });
        let out = powershell(cx, &script).await?;
        (cx.log)("info", format!("Created ZIP archive at {destination_text}. {}", out.trim()));
        Ok(Outcome::Satisfied)
    }
}

struct OpenFolder;
fn open_folder_spec() -> Value {
    action_spec("Open Folder", "folder-open", "Opens an existing folder in Windows File Explorer.", vec![
        json!({"key":"path","label":"Folder path","type":"text","required":true,"maxLen":2048}),
        json!({"preset":"onFail"})
    ], "Open folder: {path}", "The target must already exist and be a directory.")
}
#[async_trait]
impl Block for OpenFolder {
    fn title(&self) -> &'static str { "Open Folder" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(open_folder_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&open_folder_spec(), c)?;
        if text(c, "path").contains('\0') { return Err("Path must not contain NUL characters".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let path = template(cx, &text(&cx.cfg, "path"))?;
        if !Path::new(&path).is_dir() { return Err("Folder does not exist".into()); }
        let open_path = path.clone();
        tokio::task::spawn_blocking(move || super::super::sys::start_file(&open_path, ""))
            .await.map_err(|e| format!("Open folder task failed: {e}"))??;
        (cx.log)("info", format!("Opened folder: {path}"));
        Ok(Outcome::Satisfied)
    }
}

struct CreateShortcut;
fn shortcut_spec() -> Value {
    action_spec("Create Windows Shortcut", "external-link", "Creates a .lnk shortcut with optional arguments, working directory, and icon.", vec![
        json!({"key":"shortcutPath","label":"Shortcut file path (.lnk)","type":"text","required":true,"maxLen":2048}),
        json!({"key":"targetPath","label":"Target program or file","type":"text","required":true,"maxLen":2048}),
        json!({"key":"arguments","label":"Arguments","type":"text","maxLen":2048}),
        json!({"key":"workingDirectory","label":"Working directory","type":"text","maxLen":2048}),
        json!({"key":"iconPath","label":"Optional icon file","type":"text","maxLen":2048}),
        json!({"preset":"onFail"})
    ], "Create shortcut: {shortcutPath}", "The parent directory must exist. This creates a normal Windows .lnk file; it does not run the target.")
}
#[async_trait]
impl Block for CreateShortcut {
    fn title(&self) -> &'static str { "Create Windows Shortcut" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(shortcut_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&shortcut_spec(), c)?;
        for key in ["shortcutPath", "targetPath", "arguments", "workingDirectory", "iconPath"] {
            if text(c, key).contains('\0') { return Err("Shortcut fields must not contain NUL characters".into()); }
        }
        if !text(c, "shortcutPath").to_ascii_lowercase().ends_with(".lnk") { return Err("Shortcut file path must end in .lnk".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let dest = template(cx, &text(&cx.cfg, "shortcutPath"))?;
        let target = template(cx, &text(&cx.cfg, "targetPath"))?;
        let args = template(cx, &text(&cx.cfg, "arguments"))?;
        let work = template(cx, &text(&cx.cfg, "workingDirectory"))?;
        let icon = template(cx, &text(&cx.cfg, "iconPath"))?;
        if let Some(parent) = Path::new(&dest).parent().filter(|p| !p.as_os_str().is_empty()) {
            if !parent.is_dir() { return Err("Shortcut parent directory must exist".into()); }
        }
        let script = format!("$ErrorActionPreference='Stop'; $shell=New-Object -ComObject WScript.Shell; $l=$shell.CreateShortcut({}); $l.TargetPath={}; $l.Arguments={}; $l.WorkingDirectory={}; if({} -ne ''){{$l.IconLocation={}}}; $l.Save(); 'Shortcut created.'", ps_quote(&dest), ps_quote(&target), ps_quote(&args), ps_quote(&work), ps_quote(&icon), ps_quote(&icon));
        let out = powershell(cx, &script).await?;
        (cx.log)("info", format!("Created shortcut at {dest}. {}", out.trim()));
        Ok(Outcome::Satisfied)
    }
}

struct TypeText;
fn type_text_spec() -> Value {
    action_spec("Type Text into Active Window", "keyboard", "Types configured text into whichever application currently has keyboard focus.", vec![
        json!({"key":"text","label":"Text to type","type":"text","required":true,"maxLen":4000}),
        json!({"preset":"onFail"})
    ], "Type text into active window", "Requires an interactive unlocked desktop and a target window already focused. Text is sent as Unicode input and is not logged. It will not activate the target application for you.")
}
#[async_trait]
impl Block for TypeText {
    fn title(&self) -> &'static str { "Type Text into Active Window" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(type_text_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&type_text_spec(), c)?;
        if raw_text(c, "text").contains('\0') { return Err("Text to type must not contain NUL characters".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let value = template(cx, &raw_text(&cx.cfg, "text"))?;
        let code = r#"
using System;
using System.Runtime.InteropServices;
public static class AutomationTextInput {
 [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public InputUnion U; }
 [StructLayout(LayoutKind.Explicit)] struct InputUnion { [FieldOffset(0)] public KEYBDINPUT ki; }
 [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo; }
 [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint nInputs, INPUT[] pInputs, int cbSize);
 public static void TypeText(string s) {
  INPUT[] a = new INPUT[s.Length * 2];
  for(int i=0;i<s.Length;i++) {
   a[i*2].type=1; a[i*2].U.ki.wScan=s[i]; a[i*2].U.ki.dwFlags=4;
   a[i*2+1].type=1; a[i*2+1].U.ki.wScan=s[i]; a[i*2+1].U.ki.dwFlags=6;
  }
  if(a.Length==0) return;
  uint sent=SendInput((uint)a.Length,a,Marshal.SizeOf(typeof(INPUT)));
  if(sent!=(uint)a.Length) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
 }
}"#;
        let script = format!("Add-Type -TypeDefinition {}; [AutomationTextInput]::TypeText({}); 'Unicode text sent to the active window.'", ps_quote(code), ps_quote(&value));
        let out = powershell(cx, &script).await?;
        (cx.log)("info", format!("Sent {} characters to the active window. {}", value.chars().count(), out.trim()));
        Ok(Outcome::Satisfied)
    }
}

struct CaptureScreenshot;
fn screenshot_spec() -> Value {
    action_spec("Capture Screenshot", "monitor-down", "Captures the full virtual desktop (all monitors) to a PNG file.", vec![
        json!({"key":"path","label":"PNG output path","type":"text","required":true,"maxLen":2048}),
        json!({"key":"overwrite","label":"If file exists","type":"select","default":"fail","options":[["fail","Fail without replacing"],["replace","Replace file"]]}),
        json!({"preset":"onFail"})
    ], "Capture screenshot: {path}", "Captures the current visible desktop, which may contain private information. Requires an interactive Windows desktop and an existing parent directory.")
}
#[async_trait]
impl Block for CaptureScreenshot {
    fn title(&self) -> &'static str { "Capture Screenshot" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(screenshot_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&screenshot_spec(), c)?;
        if text(c, "path").contains('\0') { return Err("Screenshot path must not contain NUL characters".into()); }
        if !text(c, "path").to_ascii_lowercase().ends_with(".png") { return Err("Screenshot output path must end in .png".into()); }
        if !matches!(text(c, "overwrite").as_str(), "fail"|"replace") { return Err("Unknown screenshot collision mode".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let path = template(cx, &text(&cx.cfg, "path"))?;
        let overwrite = text(&cx.cfg, "overwrite") == "replace";
        if Path::new(&path).exists() && !overwrite { return Err("Screenshot file already exists; choose Replace or a different path".into()); }
        if let Some(parent) = Path::new(&path).parent().filter(|p| !p.as_os_str().is_empty()) {
            if !parent.is_dir() { return Err("Screenshot parent directory must exist".into()); }
        }
        let script = format!(
            "$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $p={}; if(Test-Path -LiteralPath $p){{if(-not {}){{throw 'Screenshot file already exists'}}else{{Remove-Item -LiteralPath $p -Force -ErrorAction Stop}}}}; $b=[System.Windows.Forms.SystemInformation]::VirtualScreen; if($b.Width -le 0 -or $b.Height -le 0){{throw 'Virtual desktop size is unavailable'}}; $img=[System.Drawing.Bitmap]::new($b.Width,$b.Height); $g=[System.Drawing.Graphics]::FromImage($img); try{{$g.CopyFromScreen($b.Left,$b.Top,0,0,$img.Size); $img.Save($p,[System.Drawing.Imaging.ImageFormat]::Png)}}finally{{$g.Dispose();$img.Dispose()}}; 'Screenshot saved.'",
            ps_quote(&path), if overwrite { "$true" } else { "$false" });
        let out = powershell(cx, &script).await?;
        (cx.log)("info", format!("Saved desktop screenshot to {path}. {}", out.trim()));
        Ok(Outcome::Satisfied)
    }
}

struct SetWallpaper;
fn wallpaper_spec() -> Value {
    action_spec("Set Desktop Wallpaper", "image", "Changes the current Windows desktop wallpaper to an existing image.", vec![
        json!({"key":"path","label":"Image path","type":"text","required":true,"maxLen":2048}),
        json!({"preset":"onFail"})
    ], "Set wallpaper: {path}", "The image file must exist. Supported formats depend on Windows desktop wallpaper support and policy. This changes a user-level desktop setting.")
}
#[async_trait]
impl Block for SetWallpaper {
    fn title(&self) -> &'static str { "Set Desktop Wallpaper" }
    fn category(&self) -> Cat { Cat::Action }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(wallpaper_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&wallpaper_spec(), c)?;
        if text(c, "path").contains('\0') { return Err("Image path must not contain NUL characters".into()); }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let path = template(cx, &text(&cx.cfg, "path"))?;
        if !Path::new(&path).is_file() { return Err("Wallpaper image does not exist".into()); }
        let code = r#"
using System;
using System.Runtime.InteropServices;
public static class DesktopWallpaperNative {
 [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
 static extern bool SystemParametersInfo(int action, int param, string path, int flags);
 public static void Set(string path) {
  if(!SystemParametersInfo(20,0,path,3)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
 }
}"#;
        let script = format!("Add-Type -TypeDefinition {}; [DesktopWallpaperNative]::Set({}); 'Desktop wallpaper updated.'", ps_quote(code), ps_quote(&path));
        let out = powershell(cx, &script).await?;
        (cx.log)("info", format!("Set desktop wallpaper to {path}. {}", out.trim()));
        Ok(Outcome::Satisfied)
    }
}

pub(super) fn register_into(m: &mut HashMap<&'static str, Box<dyn Block>>) {
    m.insert("cond.process_count", Box::new(ProcessCount));
    m.insert("cond.service_state", Box::new(ServiceState));
    m.insert("cond.clipboard_text", Box::new(ClipboardText));
    m.insert("cond.environment_variable", Box::new(EnvironmentVariable));
    m.insert("cond.file_age", Box::new(FileAge));
    m.insert("cond.directory_count", Box::new(DirectoryCount));
    m.insert("act.create_folder", Box::new(CreateFolder));
    m.insert("act.write_text_file", Box::new(WriteTextFile));
    m.insert("act.zip_archive", Box::new(ZipArchive));
    m.insert("act.open_folder", Box::new(OpenFolder));
    m.insert("act.create_shortcut", Box::new(CreateShortcut));
    m.insert("act.type_text", Box::new(TypeText));
    m.insert("act.screenshot", Box::new(CaptureScreenshot));
    m.insert("act.set_wallpaper", Box::new(SetWallpaper));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comparison_modes_cover_boundaries() {
        assert!(comparison_matches("equal", 2, 2));
        assert!(!comparison_matches("equal", 2, 3));
        assert!(comparison_matches("not_equal", 2, 3));
        assert!(comparison_matches("greater", 3, 2));
        assert!(!comparison_matches("greater", 2, 2));
        assert!(comparison_matches("at_least", 2, 2));
        assert!(comparison_matches("less", 1, 2));
        assert!(comparison_matches("at_most", 2, 2));
        assert!(!comparison_matches("invalid", 1, 1));
    }

    #[test]
    fn new_utility_blocks_are_registered_with_specs() {
        let expected: [(&str, Cat); 14] = [
            ("cond.process_count", Cat::Condition),
            ("cond.service_state", Cat::Condition),
            ("cond.clipboard_text", Cat::Condition),
            ("cond.environment_variable", Cat::Condition),
            ("cond.file_age", Cat::Condition),
            ("cond.directory_count", Cat::Condition),
            ("act.create_folder", Cat::Action),
            ("act.write_text_file", Cat::Action),
            ("act.zip_archive", Cat::Action),
            ("act.open_folder", Cat::Action),
            ("act.create_shortcut", Cat::Action),
            ("act.type_text", Cat::Action),
            ("act.screenshot", Cat::Action),
            ("act.set_wallpaper", Cat::Action),
        ];
        let registry = super::super::registry();
        for (id, category) in expected {
            let block = registry.get(id).unwrap_or_else(|| panic!("{id} missing from registry"));
            assert_eq!(block.category(), category, "wrong category for {id}");
            assert!(block.spec().is_some(), "missing declarative spec for {id}");
        }
    }

    #[test]
    fn utility_block_validation_rejects_unsafe_or_invalid_config() {
        assert!(CreateFolder.validate(&json!({"path":"C:\\Temp\\new-folder","onFail":"stop"})).is_ok());
        assert!(CreateFolder.validate(&json!({"path":"bad\u{0000}path","onFail":"stop"})).is_err());
        assert!(WriteTextFile.validate(&json!({"path":"C:\\Temp\\x.txt","mode":"append","content":"line","onFail":"stop"})).is_ok());
        assert!(ZipArchive.validate(&json!({"sourcePath":"C:\\Temp","destinationPath":"C:\\Temp\\x.zip","collision":"fail","onFail":"stop"})).is_ok());
        assert!(ZipArchive.validate(&json!({"sourcePath":"C:\\Temp","destinationPath":"C:\\Temp\\x.txt","collision":"fail","onFail":"stop"})).is_err());
        assert!(CreateShortcut.validate(&json!({"shortcutPath":"C:\\Temp\\x.lnk","targetPath":"notepad.exe","onFail":"stop"})).is_ok());
        assert!(CreateShortcut.validate(&json!({"shortcutPath":"C:\\Temp\\x.txt","targetPath":"notepad.exe","onFail":"stop"})).is_err());
        assert!(CaptureScreenshot.validate(&json!({"path":"C:\\Temp\\screen.png","overwrite":"fail","onFail":"stop"})).is_ok());
        assert!(CaptureScreenshot.validate(&json!({"path":"C:\\Temp\\screen.jpg","overwrite":"fail","onFail":"stop"})).is_err());
        assert!(ProcessCount.validate(&json!({"processName":"notepad","comparison":"at_least","count":2,"onFail":"stop"})).is_ok());
        assert!(ProcessCount.validate(&json!({"processName":"notepad","comparison":"at_least","count":2.5,"onFail":"stop"})).is_err());
    }
}
