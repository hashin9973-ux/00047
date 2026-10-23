//! Additional Windows automation conditions/actions. All OS commands are launched without a console window.
use super::{check_timeout, rng, spec, text, Block, Cat, NodeCx, Outcome};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    path::Path,
    pin::Pin,
    process::Stdio,
    time::{Duration, Instant, UNIX_EPOCH},
};
use tokio::io::AsyncReadExt;

const CAPTURE_LIMIT: usize = 32 * 1024;
pub(super) type Probe<'a> = Pin<Box<dyn Future<Output = Result<bool, String>> + Send + 'a>>;

async fn read_capped<R: tokio::io::AsyncRead + Unpin + Send>(reader: &mut R) -> Result<String, String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = reader.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 { break; }
        let keep = CAPTURE_LIMIT.saturating_sub(out.len()).min(n);
        out.extend_from_slice(&buf[..keep]);
    }
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

pub(super) fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\0', "").replace('\'', "''"))
}

pub(super) async fn powershell(cx: &NodeCx, script: &str) -> Result<String, String> {
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
            (cx.log)("warn", format!("PowerShell stderr: {}", err.chars().take(2000).collect::<String>()));
        }
        Ok(out)
    }
}

async fn ps_json<T: for<'de> Deserialize<'de>>(cx: &NodeCx, script: &str) -> Result<T, String> {
    let out = powershell(cx, script).await?;
    serde_json::from_str::<T>(&out).map_err(|e| format!("Could not parse Windows status result: {e}; output: {}", out.chars().take(300).collect::<String>()))
}

pub(super) fn behavior_fields() -> Vec<Value> {
    vec![
        json!({"key":"behavior","label":"Behavior","type":"select","default":"check","options":[["check","Check once (IF / ELSE)"],["wait","Wait until satisfied"],["change","Wait until state changes"]]}),
        json!({"key":"pollSec","label":"Poll every","type":"number","min":0.25,"max":3600,"step":0.25,"unit":"s","default":2,"show":{"key":"behavior","in":["wait","change"]}}),
        json!({"key":"stableForSec","label":"Require stable for","type":"number","min":0,"max":3600,"step":1,"unit":"s","default":0,"show":{"key":"behavior","in":["wait","change"]}}),
        json!({"preset":"timeout","show":{"key":"behavior","in":["wait","change"]}}),
        json!({"preset":"onFail"})
    ]
}

pub(super) async fn poll_condition<'a>(
    cx: &'a NodeCx, behavior: &str, poll_sec: f64, stable_sec: f64, timeout_sec: f64,
    mut probe: impl FnMut() -> Probe<'a>,
) -> Result<Outcome, String> {
    let start = Instant::now();
    let baseline = if behavior == "change" { Some(probe().await?) } else { None };
    let mut true_since: Option<Instant> = None;

    // A state-change check needs a real polling interval between its baseline
    // sample and the first comparison. Probing twice immediately is especially
    // wrong for stateful measurements such as network throughput.
    if behavior == "change" {
        let mut delay = Duration::from_secs_f64(poll_sec.max(0.05));
        if timeout_sec > 0.0 {
            let remaining = Duration::from_secs_f64(timeout_sec).saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(format!("condition timed out after {timeout_sec:.1} seconds"));
            }
            delay = delay.min(remaining);
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {},
            _ = cx.token.cancelled() => return Err("cancelled".into()),
        }
    }

    loop {
        if cx.token.is_cancelled() { return Err("cancelled".into()); }
        let value = probe().await?;
        if behavior == "check" {
            return Ok(if value { Outcome::Satisfied } else { Outcome::NotSatisfied });
        }
        let satisfied = if behavior == "change" { baseline.map(|b| b != value).unwrap_or(false) } else { value };
        if satisfied {
            let since = *true_since.get_or_insert_with(Instant::now);
            if stable_sec <= 0.0 || since.elapsed() >= Duration::from_secs_f64(stable_sec) {
                return Ok(Outcome::Satisfied);
            }
        } else {
            true_since = None;
        }
        if timeout_sec > 0.0 && start.elapsed() >= Duration::from_secs_f64(timeout_sec) {
            return Err(format!("condition timed out after {timeout_sec:.1} seconds"));
        }
        (cx.report)("waiting", None, format!("Waiting for condition · elapsed {:.1}s", start.elapsed().as_secs_f64()));
        let mut delay = Duration::from_secs_f64(poll_sec.max(0.05));
        if timeout_sec > 0.0 {
            let remaining = Duration::from_secs_f64(timeout_sec).saturating_sub(start.elapsed());
            delay = delay.min(remaining);
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {},
            _ = cx.token.cancelled() => return Err("cancelled".into()),
        }
    }
}

pub(super) fn wait_settings(c: &Value) -> Result<(String, f64, f64, f64), String> {
    // Saved automations created before behavior/polling fields existed must remain one-shot checks.
    let behavior = match text(c, "behavior").as_str() {
        "" => "check".to_string(),
        other => other.to_string(),
    };
    if !matches!(behavior.as_str(), "check" | "wait" | "change") {
        return Err("Unknown condition behavior".into());
    }
    let poll = rng(c, "pollSec", 2.0, 0.25, 3600.0, "Poll interval")?;
    let stable = rng(c, "stableForSec", 0.0, 0.0, 3600.0, "Stable duration")?;
    let timeout = rng(c, "timeoutSec", 0.0, 0.0, 86400.0, "Timeout")?;
    check_timeout(c)?;
    Ok((behavior, poll, stable, timeout))
}

#[derive(Deserialize, Clone, Default)]
struct NetSample {
    connected: bool,
    network_connected: bool,
    ssid: String,
    rx: u64,
    tx: u64,
    adapter_found: bool,
    wlan_text: String,
}

fn parse_wlan_status(output: &str) -> (bool, String) {
    let mut saw_state = false;
    let mut connected_state = false;
    let mut ssid = String::new();

    for line in output.lines() {
        let Some((label, value)) = line.trim().split_once(':') else { continue };
        match label.trim().to_ascii_lowercase().as_str() {
            "state" => {
                saw_state = true;
                connected_state |= value.trim().eq_ignore_ascii_case("connected");
            }
            "ssid" if ssid.is_empty() => {
                let value = value.trim();
                if !value.is_empty() {
                    ssid = value.to_string();
                }
            }
            _ => {}
        }
    }

    // Use SSID as a fallback if the State label is absent; if State is present,
    // an explicit "disconnected" result must not be overridden by an old SSID.
    (connected_state || (!saw_state && !ssid.is_empty()), ssid)
}

fn network_rate(bytes: u64, elapsed_seconds: f64, unit: &str) -> Option<f64> {
    if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
        return None;
    }
    Some(if unit == "MiB/s" {
        bytes as f64 / 1_048_576.0 / elapsed_seconds
    } else {
        bytes as f64 * 8.0 / 1_000_000.0 / elapsed_seconds
    })
}

async fn net_sample(cx: &NodeCx, adapter_hint: &str) -> Result<NetSample, String> {
    let hint = ps_quote(adapter_hint);
    let script = format!(r#"$ErrorActionPreference='Stop';
$hint={hint};
$all=@(Get-NetAdapter -ErrorAction SilentlyContinue);
$wireless=@($all | Where-Object {{ $_.Name -match 'Wi-?Fi|WLAN|Wireless' -or $_.InterfaceDescription -match 'Wireless|802.11|Wi-?Fi' }});
if($hint) {{
    $candidates=@($all | Where-Object {{ $_.Name -like ('*'+$hint.Trim([char]39)+'*') -or $_.InterfaceDescription -like ('*'+$hint.Trim([char]39)+'*') }});
}} else {{
    $candidates=$wireless;
}}
$a=$candidates | Where-Object {{ $_.Status -eq 'Up' -and $_.HardwareInterface }} | Select-Object -First 1;
if($null -eq $a){{$a=$candidates | Where-Object {{ $_.Status -eq 'Up' }} | Select-Object -First 1}};
if($null -eq $a){{$a=$candidates | Select-Object -First 1}};
$adapterFound=($null -ne $a);
$rx=0L;
$tx=0L;
if($adapterFound){{try {{$stats=Get-NetAdapterStatistics -Name $a.Name -ErrorAction Stop; $rx=[long]$stats.ReceivedBytes; $tx=[long]$stats.SentBytes}} catch {{}}}};
# Resolve Wi-Fi association independently of which adapter is selected for byte counters.
$profileConnected=$false;
try {{
    $profiles=@(Get-NetConnectionProfile -ErrorAction SilentlyContinue);
    foreach($wa in $wireless) {{
        if($wa.Status -eq 'Up') {{
            $profile=$profiles | Where-Object {{ $_.InterfaceIndex -eq $wa.ifIndex }} | Select-Object -First 1;
            if($null -ne $profile){{$profileConnected=$true; break}}
        }}
    }}
}} catch {{}};
$wlanText='';
try {{$wlanText=(& netsh.exe wlan show interfaces 2>$null | Out-String)}} catch {{}};
[pscustomobject]@{{connected=$profileConnected;network_connected=$false;ssid='';rx=$rx;tx=$tx;adapter_found=$adapterFound;wlan_text=$wlanText}} | ConvertTo-Json -Compress"#);
    let mut sample: NetSample = ps_json(cx, &script).await?;
    let (wlan_connected, ssid) = parse_wlan_status(&sample.wlan_text);
    sample.connected |= wlan_connected;
    sample.ssid = ssid;
    // Any active Windows connection profile counts for the generic network mode;
    // this includes Ethernet and other non-WLAN interfaces.
    let generic_script = r#"$ErrorActionPreference='Stop'; $profiles=@(Get-NetConnectionProfile -ErrorAction SilentlyContinue); $connected=@($profiles | Where-Object { $_.IPv4Connectivity -ne 'Disconnected' -or $_.IPv6Connectivity -ne 'Disconnected' }).Count -gt 0; [bool]$connected | ConvertTo-Json -Compress"#;
    let generic_out = powershell(cx, generic_script).await?;
    sample.network_connected = generic_out.trim().eq_ignore_ascii_case("true");
    Ok(sample)
}

fn network_spec() -> Value {
    let mut fields = vec![
        json!({"key":"mode","label":"Measure","type":"select","default":"network_connected","options":[["network_connected","Any network connected (Wi-Fi/Ethernet)"],["network_disconnected","No network connected"],["wifi_connected","Wi-Fi connected"],["wifi_disconnected","Wi-Fi disconnected"],["ssid","Connected to specific SSID"],["download","Download throughput"],["upload","Upload throughput"],["internet","Internet/host reachable"]]}),
        json!({"key":"adapter","label":"Adapter name (optional)","type":"text","maxLen":128,"default":""}),
        json!({"key":"ssid","label":"SSID","type":"text","required":true,"maxLen":256,"show":{"key":"mode","eq":"ssid"}}),
        json!({"key":"host","label":"Host or URL","type":"text","maxLen":2048,"default":"www.msftconnecttest.com","show":{"key":"mode","eq":"internet"}}),
        json!({"key":"op","label":"Compare throughput","type":"select","default":"below","options":[["above","Above"],["below","At or below"]],"show":{"key":"mode","in":["download","upload"]}}),
        json!({"key":"threshold","label":"Threshold","type":"number","min":0,"max":100000,"step":0.1,"default":2,"show":{"key":"mode","in":["download","upload"]}}),
        json!({"key":"unit","label":"Rate unit","type":"select","default":"Mbps","options":[["Mbps","Mbps"],["MiB/s","MiB/s"]],"show":{"key":"mode","in":["download","upload"]}}),
    ];
    fields.extend(behavior_fields());
    json!({"category":"condition","title":"Network Condition","icon":"wifi","description":"Checks any active network connection (Wi-Fi or Ethernet), Wi-Fi/SSID state, internet reachability, or measured adapter throughput. Supports polling and stable-state debounce.","fields":fields,"summary":"Network: {mode}","help":"Any network status uses active Windows connection profiles, including Ethernet. Wi-Fi status is read independently from adapter-counter selection, using Windows WLAN state plus wireless network profiles. Use Any network connected when the requirement is connection to any network, not specifically Wi-Fi. Throughput is estimated from byte-counter deltas between samples; a one-shot throughput check samples for one second. Mbps is megabits/second; MiB/s is mebibytes/second. The default counter adapter prefers an active physical wireless adapter; enter an adapter name to measure another interface. Internet reachability probes a host/URL and may take several seconds."})
}

struct NetworkCondition;
#[async_trait]
impl Block for NetworkCondition {
    fn title(&self) -> &'static str { "Network Condition" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(network_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&network_spec(), c)?;
        let mode = text(c, "mode");
        if !matches!(mode.as_str(), "network_connected"|"network_disconnected"|"wifi_connected"|"wifi_disconnected"|"ssid"|"download"|"upload"|"internet") { return Err("Unknown network condition".into()); }
        if text(c,"adapter").contains('\0') || text(c,"ssid").contains('\0') || text(c,"host").contains('\0') { return Err("Network fields must not contain NUL characters".into()); }
        if mode == "internet" && text(c,"host").is_empty() { return Err("Host or URL is required".into()); }
        wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let mode = text(&cx.cfg,"mode");
        let adapter = cx.expand_template(&text(&cx.cfg,"adapter"))?;
        let ssid = cx.expand_template(&text(&cx.cfg,"ssid"))?;
        let host = cx.expand_template(&text(&cx.cfg,"host"))?;
        let threshold = rng(&cx.cfg,"threshold",2.0,0.0,100000.0,"Threshold")?;
        let op = text(&cx.cfg,"op");
        let unit = text(&cx.cfg,"unit");
        let (behavior,poll,stable,timeout) = wait_settings(&cx.cfg)?;
        let last = std::sync::Arc::new(std::sync::Mutex::new(None::<(u64,u64,Instant)>));
        let last2 = last.clone();

        // A one-shot throughput comparison needs two counter readings. Capture
        // a baseline, allow counters to advance, and then run the ordinary probe.
        if matches!(mode.as_str(), "download" | "upload") && behavior == "check" {
            let baseline = net_sample(cx, &adapter).await?;
            if !baseline.adapter_found {
                return Err("No matching network adapter was found for throughput measurement. Set Adapter name to the interface you want to measure.".into());
            }
            *last.lock().map_err(|_| "network counter lock poisoned".to_string())? =
                Some((baseline.rx, baseline.tx, Instant::now()));
            (cx.report)("waiting", None, "Sampling network throughput for 1 second".into());
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                _ = cx.token.cancelled() => return Err("cancelled".into()),
            }
        }

        poll_condition(cx, &behavior, poll, stable, timeout, || {
            let adapter=adapter.clone(); let ssid=ssid.clone(); let host=host.clone(); let mode=mode.clone();
            let op=op.clone(); let unit=unit.clone(); let last=last2.clone();
            Box::pin(async move {
                if mode == "internet" {
                    let target = if host.starts_with("http://") || host.starts_with("https://") {
                        host.split("://").nth(1).unwrap_or(&host).split('/').next().unwrap_or(&host).split(':').next().unwrap_or(&host).to_string()
                    } else { host.clone() };
                    let q = ps_quote(&target);
                    let script = format!("$ErrorActionPreference='SilentlyContinue'; $ok=$false; try {{$ok=Test-NetConnection -ComputerName {q} -Port 443 -InformationLevel Quiet -WarningAction SilentlyContinue}} catch {{$ok=$false}}; [bool]$ok | ConvertTo-Json -Compress");
                    let out = powershell(cx, &script).await?;
                    return Ok(out.trim().eq_ignore_ascii_case("true"));
                }
                let sample = net_sample(cx, &adapter).await?;
                match mode.as_str() {
                    "network_connected" => Ok(sample.network_connected),
                    "network_disconnected" => Ok(!sample.network_connected),
                    "wifi_connected" => Ok(sample.connected),
                    "wifi_disconnected" => Ok(!sample.connected),
                    "ssid" => Ok(sample.connected && sample.ssid.eq_ignore_ascii_case(&ssid)),
                    "download" | "upload" => {
                        if !sample.adapter_found {
                            return Err("No matching network adapter was found for throughput measurement. Set Adapter name to the interface you want to measure.".into());
                        }
                        let now = Instant::now();
                        let mut state = last.lock().map_err(|_| "network counter lock poisoned".to_string())?;
                        let current = (sample.rx, sample.tx, now);
                        let prior = state.replace(current);
                        let Some((prx, ptx, pt)) = prior else { return Ok(false) };
                        let elapsed = now.duration_since(pt).as_secs_f64();
                        let bytes = if mode == "download" {
                            sample.rx.saturating_sub(prx)
                        } else {
                            sample.tx.saturating_sub(ptx)
                        };
                        let Some(rate) = network_rate(bytes, elapsed, &unit) else { return Ok(false) };
                        Ok(if op == "above" { rate > threshold } else { rate <= threshold })
                    }
                    _ => Ok(false)
                }
            }) as Probe<'_>
        }).await
    }
}

fn battery_spec() -> Value {
    let mut fields=vec![
        json!({"key":"mode","label":"Power state","type":"select","default":"battery_below","options":[["battery_below","Battery below threshold"],["battery_above","Battery above threshold"],["charging","Charging"],["on_ac","AC power connected"],["on_battery","Running on battery"]]}),
        json!({"key":"threshold","label":"Battery percentage","type":"number","min":0,"max":100,"step":1,"default":20,"show":{"key":"mode","in":["battery_below","battery_above"]}}),
    ]; fields.extend(behavior_fields());
    json!({"category":"condition","title":"Battery and Power State","icon":"battery","description":"Checks battery percentage, charging, and AC power state.","fields":fields,"summary":"Power: {mode}","help":"Uses the Windows GetSystemPowerStatus API. Desktop PCs without a battery may not report a percentage; battery percentage comparisons remain false when Windows reports it as unknown."})
}
#[derive(Deserialize)]
struct PowerSample { ac: i32, battery: i32, charging: bool }
async fn power_sample(cx:&NodeCx)->Result<PowerSample,String> {
    let src = r#"using System; using System.Runtime.InteropServices; public struct SYSPOWER { public byte ACLineStatus; public byte BatteryFlag; public byte BatteryLifePercent; public byte Reserved; public int BatteryLifeTime; public int BatteryFullLifeTime; } public class PowerNative { [DllImport("kernel32.dll")] public static extern bool GetSystemPowerStatus(out SYSPOWER s); }"#;
    let script=format!("Add-Type -TypeDefinition {}; $s=New-Object SYSPOWER; if(-not [PowerNative]::GetSystemPowerStatus([ref]$s)){{throw 'GetSystemPowerStatus failed'}}; [pscustomobject]@{{ac=[int]$s.ACLineStatus;battery=[int]$s.BatteryLifePercent;charging=(($s.BatteryFlag -band 8) -ne 0)}} | ConvertTo-Json -Compress", ps_quote(src));
    ps_json(cx,&script).await
}
struct BatteryPower;
#[async_trait]
impl Block for BatteryPower {
    fn title(&self)-> &'static str { "Battery and Power State" }
    fn category(&self)->Cat { Cat::Condition }
    fn supports_timeout(&self)->bool { true }
    fn spec(&self)->Option<Value>{Some(battery_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{ spec::validate(&battery_spec(),c)?; wait_settings(c).map(|_|()) }
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let mode=text(&cx.cfg,"mode");
        let threshold=rng(&cx.cfg,"threshold",20.0,0.0,100.0,"Battery percentage")?;
        let (behavior,poll,stable,timeout)=wait_settings(&cx.cfg)?;
        poll_condition(cx,&behavior,poll,stable,timeout,|| Box::pin(async {
            let p=power_sample(cx).await?;
            Ok(match mode.as_str() {
                "battery_below"=>p.battery>=0 && p.battery<=100 && (p.battery as f64)<threshold,
                "battery_above"=>p.battery>=0 && p.battery<=100 && (p.battery as f64)>threshold,
                "charging"=>p.charging,
                "on_ac"=>p.ac==1,
                "on_battery"=>p.ac==0,
                _=>false
            })
        }) as Probe<'_>).await
    }
}

fn cpu_spec()->Value{
 let mut fields=vec![
 json!({"key":"scope","label":"Measure","type":"select","default":"system","options":[["system","Total system CPU"],["process","Specific process"]]}),
 json!({"key":"processName","label":"Process name","type":"text","required":true,"maxLen":260,"show":{"key":"scope","eq":"process"}}),
 json!({"key":"op","label":"Compare","type":"select","default":"above","options":[["above","Above"],["below","At or below"]]}),
 json!({"key":"thresholdPct","label":"CPU usage (%)","type":"number","min":0,"max":100,"step":1,"default":80}),
 ]; fields.extend(behavior_fields());
 json!({"category":"condition","title":"CPU Usage","icon":"activity","description":"Checks overall CPU utilization or a selected process's approximate utilization.","fields":fields,"summary":"CPU {op} {thresholdPct}%","help":"Uses Windows performance counters for total system CPU; per-process utilization is estimated from cumulative CPU time over a short sample and normalized by logical processor count."})
}
async fn cpu_percent(cx:&NodeCx,scope:&str,name:&str)->Result<f64,String>{
 let n=ps_quote(name);
 let script=if scope=="system" {
 "$ErrorActionPreference='Stop'; $p=Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor -Filter \"Name='_Total'\"; if($null -eq $p){throw 'CPU counter unavailable'}; [double]$p.PercentProcessorTime | ConvertTo-Json -Compress".to_string()
 } else {
 format!("$ErrorActionPreference='Stop'; $n={n}; $ps=@(Get-Process -Name $n -ErrorAction SilentlyContinue); if($ps.Count -eq 0){{throw ('process not running: '+$n)}}; $a=($ps | Measure-Object CPU -Sum).Sum; Start-Sleep -Milliseconds 500; $ps2=@(Get-Process -Name $n -ErrorAction SilentlyContinue); if($ps2.Count -eq 0){{throw ('process exited: '+$n)}}; $b=($ps2 | Measure-Object CPU -Sum).Sum; $cores=[Environment]::ProcessorCount; $pct=([Math]::Max(0,([double]$b-[double]$a))/0.5/[Math]::Max(1,$cores))*100; [double][Math]::Min(100,$pct) | ConvertTo-Json -Compress")
 };
 let s=powershell(cx,&script).await?;
 s.trim().parse::<f64>().map_err(|e|format!("Invalid CPU usage result: {e}"))
}
struct CpuUsage;
#[async_trait]
impl Block for CpuUsage {
 fn title(&self)-> &'static str{"CPU Usage"} fn category(&self)->Cat{Cat::Condition} fn supports_timeout(&self)->bool{true} fn spec(&self)->Option<Value>{Some(cpu_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&cpu_spec(),c)?;wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let scope=text(&cx.cfg,"scope");let name=cx.expand_template(&text(&cx.cfg,"processName"))?;let op=text(&cx.cfg,"op");let threshold=rng(&cx.cfg,"thresholdPct",80.0,0.0,100.0,"CPU usage")?;let(behavior,poll,stable,timeout)=wait_settings(&cx.cfg)?;
  poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{let v=cpu_percent(cx,&scope,&name).await?;(cx.report)("running",None,format!("{} CPU: {v:.1}%",if scope=="system"{"System"}else{name.as_str()}));Ok(if op=="above"{v>threshold}else{v<=threshold})}) as Probe<'_>).await
 }
}

fn disk_spec()->Value{
 let mut fields=vec![
 json!({"key":"drive","label":"Drive (for example C:)","type":"text","required":true,"maxLen":3,"default":"C:"}),
 json!({"key":"metric","label":"Measure","type":"select","default":"free_pct","options":[["free_pct","Free space (%)"],["used_pct","Used space (%)"],["free_gb","Free space (GB)"]]}),
 json!({"key":"op","label":"Compare","type":"select","default":"below","options":[["above","Above"],["below","At or below"]]}),
 json!({"key":"threshold","label":"Threshold","type":"number","min":0,"max":1000000,"step":1,"default":15}),
 ];fields.extend(behavior_fields());json!({"category":"condition","title":"Disk Space","icon":"hard-drive","description":"Checks free or used space on a Windows drive.","fields":fields,"summary":"Disk {drive}: {metric} {op} {threshold}","help":"Drive must use the Windows drive-letter form, such as C:. Percentage metrics use 0–100; free GB uses decimal gigabytes."})
}
#[derive(Deserialize)]struct DiskSample{free:u64,total:u64}
async fn disk_sample(cx:&NodeCx,drive:&str)->Result<DiskSample,String>{
 let d=ps_quote(drive);
 let script=format!("$ErrorActionPreference='Stop'; $d={d}; $x=Get-CimInstance Win32_LogicalDisk -Filter ('DeviceID=' + [char]39 + $d + [char]39); if($null -eq $x){{throw ('drive not found: '+$d)}}; [pscustomobject]@{{free=[UInt64]$x.FreeSpace;total=[UInt64]$x.Size}} | ConvertTo-Json -Compress");
 ps_json(cx,&script).await
}
struct DiskSpace;
#[async_trait]
impl Block for DiskSpace {
 fn title(&self)-> &'static str{"Disk Space"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(disk_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&disk_spec(),c)?;let d=text(c,"drive");if d.len()!=2||!d.ends_with(':')||!d.chars().next().unwrap_or(' ').is_ascii_alphabetic(){return Err("Drive must look like C:".into())};wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let drive=text(&cx.cfg,"drive");let metric=text(&cx.cfg,"metric");let op=text(&cx.cfg,"op");let threshold=rng(&cx.cfg,"threshold",15.0,0.0,1_000_000.0,"Threshold")?;let(behavior,poll,stable,timeout)=wait_settings(&cx.cfg)?;
  poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{let d=disk_sample(cx,&drive).await?;let free_pct=if d.total==0{0.0}else{d.free as f64/d.total as f64*100.0};let v=match metric.as_str(){"used_pct"=>100.0-free_pct,"free_gb"=>d.free as f64/1_000_000_000.0,_=>free_pct};(cx.report)("running",None,format!("{drive}: {v:.1} {}",if metric=="free_gb" { "GB" } else { "%" }));Ok(if op=="above"{v>threshold}else{v<=threshold})})as Probe<'_>).await
 }
}

fn file_spec()->Value{
 let mut fields=vec![
 json!({"key":"path","label":"File or folder path","type":"text","required":true,"maxLen":2048}),
 json!({"key":"state","label":"State","type":"select","default":"exists","options":[["exists","Exists"],["missing","Missing"],["size_above","Size above threshold"],["size_below","Size at or below threshold"],["changed","Created, removed, or modified since this block started"]]}),
 json!({"key":"thresholdMb","label":"Size threshold (MiB)","type":"number","min":0,"max":1048576,"step":1,"default":100,"show":{"key":"state","in":["size_above","size_below"]}}),
 ];fields.extend(behavior_fields());json!({"category":"condition","title":"File or Folder State","icon":"file","description":"Checks file/folder existence, size, or changes while a workflow is waiting.","fields":fields,"summary":"File state: {state}","help":"Use an explicit path. Changed compares existence, last-write time, and size to the values observed when this block started."})
}
fn file_fingerprint(path:&str)->Result<Option<(u64,u128,bool)>,String>{
 match std::fs::metadata(path){Ok(m)=>{let len=m.len();let t=m.modified().ok().and_then(|x|x.duration_since(UNIX_EPOCH).ok()).map(|x|x.as_millis()).unwrap_or(0);Ok(Some((len,t,m.is_dir())))},Err(e)if e.kind()==std::io::ErrorKind::NotFound=>Ok(None),Err(e)=>Err(format!("Could not inspect '{}': {e}",path))}
}
struct FileFolder;
#[async_trait]
impl Block for FileFolder{
 fn title(&self)-> &'static str{"File or Folder State"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(file_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&file_spec(),c)?;if text(c,"path").contains('\0'){return Err("Path must not contain NUL characters".into())};wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let path=cx.expand_template(&text(&cx.cfg,"path"))?;let state=text(&cx.cfg,"state");let threshold=rng(&cx.cfg,"thresholdMb",100.0,0.0,1048576.0,"Size threshold")?*1_048_576.0;let(behavior,poll,stable,timeout)=wait_settings(&cx.cfg)?;
  let baseline=file_fingerprint(&path)?;
  poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{let current=file_fingerprint(&path)?;let ok=match state.as_str(){"exists"=>current.is_some(),"missing"=>current.is_none(),"size_above"=>current.as_ref().map(|v|v.0 as f64>threshold).unwrap_or(false),"size_below"=>current.as_ref().map(|v|v.0 as f64<=threshold).unwrap_or(false),"changed"=>current!=baseline,_=>false};Ok(ok)})as Probe<'_>).await
 }
}

fn active_window_spec()->Value{
 let mut fields=vec![
 json!({"key":"match","label":"Match by","type":"select","default":"process","options":[["process","Process name"],["title","Window title contains"]]}),
 json!({"key":"pattern","label":"Process name or title text","type":"text","required":true,"maxLen":512}),
 json!({"key":"state","label":"Condition","type":"select","default":"active","options":[["active","Is the active window"],["inactive","Is not the active window"]]}),
 ];fields.extend(behavior_fields());json!({"category":"condition","title":"Active Window","icon":"app-window","description":"Checks whether a process or window title belongs to the foreground window.","fields":fields,"summary":"Active window: {pattern}","help":"Process matching is case-insensitive. Title matching checks whether the foreground window title contains the configured text."})
}
#[derive(Deserialize)]struct ActiveSample{pid:u32,title:String,process:String}
async fn active_sample(cx:&NodeCx)->Result<ActiveSample,String>{
 let src=r#"using System; using System.Text; using System.Runtime.InteropServices; public class ForegroundNative { [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder t, int n); [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p); }"#;
 let script=format!("Add-Type -TypeDefinition {}; $h=[ForegroundNative]::GetForegroundWindow(); $p=0; if($h -ne [IntPtr]::Zero){{[void][ForegroundNative]::GetWindowThreadProcessId($h,[ref]$p)}}; $t=New-Object System.Text.StringBuilder 1024; if($h -ne [IntPtr]::Zero){{[void][ForegroundNative]::GetWindowText($h,$t,1024)}}; $n=''; try{{$n=(Get-Process -Id $p -ErrorAction Stop).ProcessName}}catch{{}}; [pscustomobject]@{{pid=[uint32]$p;title=$t.ToString();process=$n}} | ConvertTo-Json -Compress",ps_quote(src));
 ps_json(cx,&script).await
}
struct ActiveWindow;
#[async_trait]
impl Block for ActiveWindow{
 fn title(&self)-> &'static str{"Active Window"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(active_window_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&active_window_spec(),c)?;wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let matching=text(&cx.cfg,"match");let pattern=cx.expand_template(&text(&cx.cfg,"pattern"))?;let state=text(&cx.cfg,"state");let(behavior,poll,stable,timeout)=wait_settings(&cx.cfg)?;
 poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{let a=active_sample(cx).await?;let matched=if matching=="title"{a.title.to_lowercase().contains(&pattern.to_lowercase())}else{a.process.eq_ignore_ascii_case(pattern.trim_end_matches(".exe"))||a.process.eq_ignore_ascii_case(pattern.trim_end_matches(".EXE"))};Ok(if state=="inactive"{!matched}else{matched})})as Probe<'_>).await
 }
}

fn device_spec(title:&str, class:&str)->Value{
 let mut fields=vec![
 json!({"key":"deviceName","label":"Device name contains","type":"text","required":true,"maxLen":512}),
 json!({"key":"state","label":"Condition","type":"select","default":"present","options":[["present","Device is available/connected"],["missing","Device is missing/disconnected"]]}),
 ];
 fields.extend(behavior_fields());
 json!({"category":"condition","title":title,"icon":"usb","description":format!("Checks Windows Plug and Play device state for the {class} device class."),"fields":fields,"summary":format!("{title}: {{deviceName}}"),"help":"Matches the device's friendly name case-insensitively. Windows device enumeration can lag physical connect/disconnect events by a short interval."})
}
struct BluetoothDevice;
struct UsbDevice;
struct AudioDevice;
async fn device_present(cx:&NodeCx,class:&str,name:&str)->Result<bool,String>{
 let c=ps_quote(class);let n=ps_quote(name);
 let script=format!("$ErrorActionPreference='Stop'; $class={c}; $needle={n}; $d=@(Get-PnpDevice -Class $class -PresentOnly -ErrorAction SilentlyContinue | Where-Object {{ $_.FriendlyName -like ('*'+$needle.Trim([char]39)+'*') }}); [bool]($d.Count -gt 0 -and (@($d | Where-Object Status -eq 'OK').Count -gt 0)) | ConvertTo-Json -Compress");
 let out=powershell(cx,&script).await?;Ok(out.eq_ignore_ascii_case("true"))
}
macro_rules! device_block {
 ($ty:ty,$title:literal,$kind:literal) => {
 #[async_trait]
 impl Block for $ty {
  fn title(&self)-> &'static str{$title} fn category(&self)->Cat{Cat::Condition} fn supports_timeout(&self)->bool{true}
  fn spec(&self)->Option<Value>{Some(device_spec($title,$kind))}
  fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&device_spec($title,$kind),c)?;if text(c,"deviceName").contains('\0'){return Err("Device name must not contain NUL characters".into())};wait_settings(c).map(|_|())}
  async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let name=cx.expand_template(&text(&cx.cfg,"deviceName"))?;let state=text(&cx.cfg,"state");let class=$kind.to_string();let(behavior,poll,stable,timeout)=wait_settings(&cx.cfg)?;poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{let present=device_present(cx,&class,&name).await?;Ok(if state=="missing"{!present}else{present})})as Probe<'_>).await}
 }
 }
}
device_block!(BluetoothDevice,"Bluetooth Device State","Bluetooth");
device_block!(UsbDevice,"USB Device State","USB");
device_block!(AudioDevice,"Audio Device State","AudioEndpoint");

fn lock_spec()->Value{let mut fields=vec![json!({"key":"state","label":"Session state","type":"select","default":"locked","options":[["locked","Workstation locked"],["unlocked","Workstation unlocked"]]})];fields.extend(behavior_fields());json!({"category":"condition","title":"Workstation Lock State","icon":"lock","description":"Checks whether Windows has switched the input desktop to the secure lock screen.","fields":fields,"summary":"Workstation {state}","help":"Uses the active Windows input desktop name (Winlogon versus Default). Availability can vary by session type and security software."})}
async fn workstation_locked(cx:&NodeCx)->Result<bool,String>{
 let src=r#"using System; using System.Text; using System.Runtime.InteropServices; public class DesktopNative { [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr OpenInputDesktop(uint f, bool i, uint a); [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern bool GetUserObjectInformation(IntPtr h, int i, StringBuilder b, int n, out uint need); [DllImport("user32.dll")] public static extern bool CloseDesktop(IntPtr h); }"#;
 let script=format!("Add-Type -TypeDefinition {}; $h=[DesktopNative]::OpenInputDesktop(0,$false,1); if($h -eq [IntPtr]::Zero){{throw 'Could not inspect input desktop'}}; try{{$b=New-Object System.Text.StringBuilder 256; $need=0; if(-not [DesktopNative]::GetUserObjectInformation($h,2,$b,256,[ref]$need)){{throw 'Could not read input desktop'}}; [bool]($b.ToString() -match 'Winlogon') | ConvertTo-Json -Compress}}finally{{[void][DesktopNative]::CloseDesktop($h)}}",ps_quote(src));
 let out=powershell(cx,&script).await?;Ok(out.eq_ignore_ascii_case("true"))
}
struct WorkstationLock;
#[async_trait]
impl Block for WorkstationLock{
 fn title(&self)-> &'static str{"Workstation Lock State"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(lock_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&lock_spec(),c)?;wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let state=text(&cx.cfg,"state");let(behavior,poll,stable,timeout)=wait_settings(&cx.cfg)?;poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{let locked=workstation_locked(cx).await?;Ok(if state=="unlocked"{!locked}else{locked})})as Probe<'_>).await}
}

fn idle_spec()->Value{let mut fields=vec![json!({"key":"state","label":"Activity","type":"select","default":"idle","options":[["idle","User is idle"],["active","User is active"]]}),json!({"key":"seconds","label":"Idle threshold","type":"number","min":1,"max":86400,"step":1,"unit":"s","default":300})];fields.extend(behavior_fields());json!({"category":"condition","title":"User Idle Time","icon":"timer","description":"Checks time since the last keyboard or mouse input.","fields":fields,"summary":"User {state} for {seconds}s","help":"Uses the Windows GetLastInputInfo API. Idle duration resets on keyboard or mouse input."})}
async fn idle_seconds(cx:&NodeCx)->Result<f64,String>{
 let src=r#"using System; using System.Runtime.InteropServices; [StructLayout(LayoutKind.Sequential)] public struct LASTINPUTINFO { public uint cbSize; public uint dwTime; } public class IdleNative { [DllImport("user32.dll")] public static extern bool GetLastInputInfo(ref LASTINPUTINFO x); [DllImport("kernel32.dll")] public static extern uint GetTickCount(); }"#;
 let script=format!("Add-Type -TypeDefinition {}; $x=New-Object LASTINPUTINFO; $x.cbSize=[Runtime.InteropServices.Marshal]::SizeOf([type][LASTINPUTINFO]); if(-not [IdleNative]::GetLastInputInfo([ref]$x)){{throw 'GetLastInputInfo failed'}}; [double]([uint32]([IdleNative]::GetTickCount()-$x.dwTime)/1000.0) | ConvertTo-Json -Compress",ps_quote(src));
 powershell(cx,&script).await?.trim().parse::<f64>().map_err(|e|format!("Invalid idle duration: {e}"))
}
struct UserIdle;
#[async_trait]
impl Block for UserIdle{
 fn title(&self)-> &'static str{"User Idle Time"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(idle_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&idle_spec(),c)?;wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let state=text(&cx.cfg,"state");let seconds=rng(&cx.cfg,"seconds",300.0,1.0,86400.0,"Idle threshold")?;let(behavior,poll,stable,timeout)=wait_settings(&cx.cfg)?;poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{let n=idle_seconds(cx).await?;Ok(if state=="active"{n<seconds}else{n>=seconds})})as Probe<'_>).await}
}

fn responsive_spec()->Value{
 let mut fields=vec![json!({"key":"processName","label":"Process name","type":"text","required":true,"maxLen":260}),json!({"key":"state","label":"Condition","type":"select","default":"responsive","options":[["responsive","Responding"],["not_responding","Not responding"],["exited","Process exited"]]})];fields.extend(behavior_fields());json!({"category":"condition","title":"Application Responsiveness","icon":"activity","description":"Checks whether a named application exists and its main window is responding.","fields":fields,"summary":"{processName}: {state}","help":"Requires a process name without a path. Console/background processes without a main window may be classified as not responding by Windows."})
}
async fn responsive_state(cx:&NodeCx,name:&str)->Result<String,String>{
 let q=ps_quote(name);
 let script=format!("$ErrorActionPreference='Stop'; $n={q}; $p=@(Get-Process -Name $n -ErrorAction SilentlyContinue); if($p.Count -eq 0){{$s='exited'}}elseif(@($p | Where-Object {{$_.MainWindowHandle -ne 0 -and $_.Responding}}).Count -gt 0 -or @($p | Where-Object {{$_.MainWindowHandle -eq 0}}).Count -eq $p.Count){{$s='responsive'}}else{{$s='not_responding'}}; ConvertTo-Json -InputObject $s -Compress");
 powershell(cx,&script).await?.trim().parse::<String>().map_err(|e|format!("Invalid process responsiveness result: {e}"))
}
struct AppResponsiveness;
#[async_trait]
impl Block for AppResponsiveness{
 fn title(&self)-> &'static str{"Application Responsiveness"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(responsive_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&responsive_spec(),c)?;wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let name=text(&cx.cfg,"processName");let state=text(&cx.cfg,"state");let(behavior,poll,stable,timeout)=wait_settings(&cx.cfg)?;poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{Ok(responsive_state(cx,&name).await?==state)})as Probe<'_>).await}
}

// Actions -------------------------------------------------------------------
fn simple_action_spec(title:&str,icon:&str,description:&str,fields:Vec<Value>,summary:&str,help:&str)->Value{json!({"category":"action","title":title,"icon":icon,"description":description,"fields":fields,"summary":summary,"help":help})}

struct OpenUrl;
fn open_url_spec()->Value{simple_action_spec("Open URL","globe","Opens an HTTP or HTTPS URL in the default browser.",vec![json!({"key":"url","label":"URL","type":"text","required":true,"maxLen":2048}),json!({"preset":"onFail"})],"Open {url}","Only http:// and https:// URLs are accepted.")}
#[async_trait]
impl Block for OpenUrl{
 fn title(&self)-> &'static str{"Open URL"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(open_url_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&open_url_spec(),c)?;let u=text(c,"url");if !(u.starts_with("https://")||u.starts_with("http://")){return Err("URL must start with http:// or https://".into())};if u.contains('\0'){return Err("URL contains a NUL character".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let url=cx.expand_template(&text(&cx.cfg,"url"))?;let log_url=url.clone();tokio::task::spawn_blocking(move||super::super::sys::start_file(&url,"")).await.map_err(|e|e.to_string())??;(cx.log)("info",format!("Opened URL {log_url}"));Ok(Outcome::Satisfied)}
}

fn volume_spec()->Value{simple_action_spec("Set System Volume","volume-2","Sets or mutes the default Windows playback endpoint.",vec![json!({"key":"mode","label":"Operation","type":"select","default":"set","options":[["set","Set volume"],["mute","Mute"],["unmute","Unmute"],["toggle_mute","Toggle mute"]]}),json!({"key":"percent","label":"Volume (%)","type":"number","min":0,"max":100,"step":1,"default":50,"show":{"key":"mode","eq":"set"}}),json!({"preset":"onFail"})],"Volume: {mode}","Uses the Windows Core Audio endpoint API.")}
struct SetSystemVolume;
#[async_trait]
impl Block for SetSystemVolume{
 fn title(&self)-> &'static str{"Set System Volume"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(volume_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&volume_spec(),c)}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let mode=text(&cx.cfg,"mode");let pct=rng(&cx.cfg,"percent",50.0,0.0,100.0,"Volume")?;
 let code=r#"using System; using System.Runtime.InteropServices; [ComImport,Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class MMDeviceEnumerator {} [ComImport,Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"),InterfaceType(ComInterfaceType.InterfaceIsIUnknown)] interface IMMDeviceEnumerator { int EnumAudioEndpoints(int f,uint s,out IntPtr d); int GetDefaultAudioEndpoint(int f,int r,out IMMDevice d); int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string id,out IMMDevice d); int RegisterEndpointNotificationCallback(IntPtr c); int UnregisterEndpointNotificationCallback(IntPtr c); } [ComImport,Guid("D666063F-1587-4E43-81F1-B948E807363F"),InterfaceType(ComInterfaceType.InterfaceIsIUnknown)] interface IMMDevice { int Activate(ref Guid iid,uint ctx,IntPtr p,[MarshalAs(UnmanagedType.IUnknown)] out object o); int OpenPropertyStore(int a,out IntPtr p); int GetId(out IntPtr p); int GetState(out int s); } [ComImport,Guid("5CDF2C82-841E-4546-9722-0CF74078229A"),InterfaceType(ComInterfaceType.InterfaceIsIUnknown)] interface IAudioEndpointVolume { void RegisterControlChangeNotify(IntPtr p); void UnregisterControlChangeNotify(IntPtr p); int GetChannelCount(out uint c); int SetMasterVolumeLevel(float v,ref Guid g); int SetMasterVolumeLevelScalar(float v,ref Guid g); int GetMasterVolumeLevel(out float v); int GetMasterVolumeLevelScalar(out float v); int SetChannelVolumeLevel(uint c,float v,ref Guid g); int SetChannelVolumeLevelScalar(uint c,float v,ref Guid g); int GetChannelVolumeLevel(uint c,out float v); int GetChannelVolumeLevelScalar(uint c,out float v); int SetMute([MarshalAs(UnmanagedType.Bool)] bool m,ref Guid g); int GetMute([MarshalAs(UnmanagedType.Bool)] out bool m); int GetVolumeStepInfo(out uint a,out uint b); int VolumeStepUp(ref Guid g); int VolumeStepDown(ref Guid g); int QueryHardwareSupport(out uint m); int GetVolumeRange(out float a,out float b,out float c); } public class AudioLevel { static IAudioEndpointVolume Get(){var e=(IMMDeviceEnumerator)new MMDeviceEnumerator(); IMMDevice d; Marshal.ThrowExceptionForHR(e.GetDefaultAudioEndpoint(0,1,out d)); object o; var iid=new Guid("5CDF2C82-841E-4546-9722-0CF74078229A"); Marshal.ThrowExceptionForHR(d.Activate(ref iid,23,IntPtr.Zero,out o)); return (IAudioEndpointVolume)o;} public static void Set(float p){var a=Get();var g=Guid.Empty;Marshal.ThrowExceptionForHR(a.SetMasterVolumeLevelScalar((float)Math.Max(0,Math.Min(1,p)),ref g));} public static void Mute(bool b){var a=Get();var g=Guid.Empty;Marshal.ThrowExceptionForHR(a.SetMute(b,ref g));} public static bool IsMuted(){var a=Get();bool b;Marshal.ThrowExceptionForHR(a.GetMute(out b));return b;} }"#;
 let op=match mode.as_str(){"set"=>format!("[AudioLevel]::Set({})",pct/100.0),"mute"=>"[AudioLevel]::Mute($true)".into(),"unmute"=>"[AudioLevel]::Mute($false)".into(),_=>"[AudioLevel]::Mute(-not [AudioLevel]::IsMuted())".into()};
 let script=format!("Add-Type -TypeDefinition {}; {}; 'System volume updated.'",ps_quote(code),op);
 let out=powershell(cx,&script).await?;(cx.log)("info",out);Ok(Outcome::Satisfied)}
}

fn priority_spec()->Value{simple_action_spec("Set Process Priority","gauge","Changes a process priority without enabling real-time priority.",vec![json!({"key":"processName","label":"Process name","type":"text","required":true,"maxLen":260}),json!({"key":"priority","label":"Priority","type":"select","default":"BelowNormal","options":[["Idle","Idle"],["BelowNormal","Below Normal"],["Normal","Normal"],["AboveNormal","Above Normal"],["High","High"]]}),json!({"preset":"onFail"})],"Priority {processName}: {priority}","Requires sufficient rights to change the selected process priority. High priority can affect system responsiveness; real-time priority is intentionally omitted.")}
struct SetProcessPriority;
#[async_trait]
impl Block for SetProcessPriority{
 fn title(&self)-> &'static str{"Set Process Priority"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(priority_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&priority_spec(),c)}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let n=cx.expand_template(&text(&cx.cfg,"processName"))?;let p=text(&cx.cfg,"priority");let script=format!("$ErrorActionPreference='Stop'; $n={}; $p=@(Get-Process -Name $n -ErrorAction Stop); foreach($x in $p){{$x.PriorityClass={}}}; 'Updated priority for '+$p.Count+' process instance(s).'",ps_quote(&n),ps_quote(&p));let out=powershell(cx,&script).await?;(cx.log)("info",out);Ok(Outcome::Satisfied)}
}

fn window_action_spec()->Value{simple_action_spec("Control Window","app-window","Focus, minimize, maximize, or restore a selected application's main window.",vec![json!({"key":"processName","label":"Process name","type":"text","required":true,"maxLen":260}),json!({"key":"operation","label":"Operation","type":"select","default":"focus","options":[["focus","Focus"],["minimize","Minimize"],["maximize","Maximize"],["restore","Restore"]]}),json!({"preset":"onFail"})],"Window {operation}: {processName}","Focus can fail if Windows foreground-lock rules prevent stealing focus. Matches the first process instance with a main window.")}
struct ControlWindow;
#[async_trait]
impl Block for ControlWindow{
 fn title(&self)-> &'static str{"Control Window"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(window_action_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&window_action_spec(),c)}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let n=cx.expand_template(&text(&cx.cfg,"processName"))?;let op=text(&cx.cfg,"operation");let code=r#"using System; using System.Runtime.InteropServices; public class WindowNative { [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr h,int c); [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h); }"#;let show=match op.as_str(){"minimize"=>6,"maximize"=>3,"restore"=>9,_=>9};let script=format!("Add-Type -TypeDefinition {}; $n={}; $p=Get-Process -Name $n -ErrorAction Stop | Where-Object {{$_.MainWindowHandle -ne 0}} | Select-Object -First 1; if($null -eq $p){{throw 'No main window found for process'}}; [void][WindowNative]::ShowWindowAsync($p.MainWindowHandle,{}); if('{}' -eq 'focus'){{Start-Sleep -Milliseconds 100; [void][WindowNative]::SetForegroundWindow($p.MainWindowHandle)}}; 'Window operation completed.'",ps_quote(code),ps_quote(&n),show,op);let out=powershell(cx,&script).await?;(cx.log)("info",out);Ok(Outcome::Satisfied)}
}

fn copy_move_spec()->Value{simple_action_spec("Copy or Move File","files","Copies or moves a single file to a destination path.",vec![json!({"key":"operation","label":"Operation","type":"select","default":"copy","options":[["copy","Copy"],["move","Move"]]}),json!({"key":"source","label":"Source file","type":"text","required":true,"maxLen":2048}),json!({"key":"destination","label":"Destination file path","type":"text","required":true,"maxLen":2048}),json!({"key":"collision","label":"If destination exists","type":"select","default":"fail","options":[["fail","Fail"],["overwrite","Overwrite"]]}),json!({"preset":"onFail"})],"File {operation}","Only ordinary files are handled; folder copy is deliberately not recursive. Use Delete File when removing an item.")}
struct CopyMoveFile;
#[async_trait]
impl Block for CopyMoveFile{
 fn title(&self)-> &'static str{"Copy or Move File"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(copy_move_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&copy_move_spec(),c)?;for k in ["source","destination"]{if text(c,k).contains('\0'){return Err(format!("{k} must not contain NUL characters"))}}Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let src=cx.expand_template(&text(&cx.cfg,"source"))?;let dst=cx.expand_template(&text(&cx.cfg,"destination"))?;let op=text(&cx.cfg,"operation");let collision=text(&cx.cfg,"collision");let s=Path::new(&src);let d=Path::new(&dst);let m=std::fs::metadata(s).map_err(|e|format!("Could not inspect source file: {e}"))?;if !m.is_file(){return Err("Source must be a regular file".into())};if d.exists()&&collision!="overwrite"{return Err("Destination already exists".into())};if let Some(parent)=d.parent(){if !parent.exists(){return Err("Destination directory does not exist".into())}}
 if op=="copy"{std::fs::copy(s,d).map_err(|e|format!("Copy failed: {e}"))?;}else{if d.exists(){std::fs::remove_file(d).map_err(|e|format!("Could not replace destination: {e}"))?}if let Err(e)=std::fs::rename(s,d){if e.raw_os_error()==Some(17){std::fs::copy(s,d).map_err(|x|format!("Move copy stage failed: {x}"))?;std::fs::remove_file(s).map_err(|x|format!("Copied but could not remove source: {x}"))?;}else{return Err(format!("Move failed: {e}"))}}}
 (cx.log)("info",format!("{} '{}' -> '{}'",op,src,dst));Ok(Outcome::Satisfied)}
}

fn clipboard_spec()->Value{simple_action_spec("Set Clipboard Text","clipboard","Copies configured text to the Windows clipboard.",vec![json!({"key":"text","label":"Text to copy","type":"text","required":true,"maxLen":16000}),json!({"preset":"onFail"})],"Copy text to clipboard","Clipboard content may be sensitive; this action does not log the clipboard text.")}
struct ClipboardText;
#[async_trait]
impl Block for ClipboardText{
 fn title(&self)-> &'static str{"Set Clipboard Text"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(clipboard_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&clipboard_spec(),c)?;if text(c,"text").contains('\0'){return Err("Clipboard text must not contain NUL characters".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let t=cx.expand_template(&text(&cx.cfg,"text"))?;let script=format!("$ErrorActionPreference='Stop'; Set-Clipboard -Value {}",ps_quote(&t));powershell(cx,&script).await?;(cx.log)("info","Clipboard text updated (content omitted from logs)".into());Ok(Outcome::Satisfied)}
}

fn http_spec()->Value{simple_action_spec("HTTP Request / Webhook","globe","Sends an HTTP request to a URL and optionally stores response data in workflow variables.",vec![json!({"key":"url","label":"URL","type":"text","required":true,"maxLen":2048}),json!({"key":"method","label":"Method","type":"select","default":"GET","options":[["GET","GET"],["POST","POST"],["PUT","PUT"],["PATCH","PATCH"],["DELETE","DELETE"],["HEAD","HEAD"]]}),json!({"key":"body","label":"Request body (optional)","type":"textarea","rows":4,"maxLen":16000,"default":"","show":{"key":"method","in":["POST","PUT","PATCH"]}}),json!({"key":"headersJson","label":"Headers as JSON object (optional)","type":"textarea","rows":3,"maxLen":8000,"default":""}),json!({"key":"saveBodyAs","label":"Save response excerpt to variable (optional)","type":"text","maxLen":64}),json!({"key":"saveStatusAs","label":"Save status code to variable (optional)","type":"text","maxLen":64}),json!({"key":"logResponse","label":"Log response body excerpt","type":"bool","default":false}),json!({"key":"timeoutSec","label":"Request timeout","type":"number","min":1,"max":120,"step":1,"unit":"s","default":15}),json!({"preset":"onFail"})],"HTTP {method} {url}","Only http:// and https:// URLs are accepted. Use {{variable_name}} placeholders in URL, request body, and headers. Response excerpt (up to 4,000 characters) and numeric status can be saved as workflow variables. Bodies are not logged unless explicitly enabled; authorization headers are never logged.")}
struct HttpRequest;
#[async_trait]
impl Block for HttpRequest{
 fn title(&self)-> &'static str{"HTTP Request / Webhook"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(http_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&http_spec(),c)?;let u=text(c,"url");if !(u.starts_with("http://")||u.starts_with("https://")){return Err("URL must start with http:// or https://".into())};if ["url","body","headersJson","saveBodyAs","saveStatusAs"].iter().any(|k|text(c,k).contains('\0')){return Err("HTTP fields must not contain NUL characters".into())};for key in ["saveBodyAs","saveStatusAs"]{let name=text(c,key);if !name.is_empty()&&!(name.len()<=64&&name.chars().next().is_some_and(|ch|ch.is_ascii_alphabetic()||ch=='_')&&name.chars().all(|ch|ch.is_ascii_alphanumeric()||matches!(ch,'_'|'-'|'.'))){return Err(format!("{key} variable name is invalid"))}};if !text(c,"headersJson").is_empty(){let v:Value=serde_json::from_str(&text(c,"headersJson")).map_err(|e|format!("Headers must be valid JSON: {e}"))?;if !v.is_object(){return Err("Headers must be a JSON object".into())}}check_timeout(c)}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let url=cx.expand_template(&text(&cx.cfg,"url"))?;let method=text(&cx.cfg,"method");let body=cx.expand_template(&text(&cx.cfg,"body"))?;let headers=cx.expand_template(&text(&cx.cfg,"headersJson"))?;let save_body=text(&cx.cfg,"saveBodyAs");let save_status=text(&cx.cfg,"saveStatusAs");let log_response=cx.cfg.get("logResponse").and_then(Value::as_bool).unwrap_or(false);let seconds=rng(&cx.cfg,"timeoutSec",15.0,1.0,120.0,"Request timeout")?;let src=format!("$ErrorActionPreference='Stop'; $u={}; $m={}; $timeout={}; $headers=@{{}}; $hjson={}; if($hjson){{$hj=ConvertFrom-Json $hjson; foreach($prop in $hj.PSObject.Properties){{$headers[$prop.Name]=[string]$prop.Value}}}}; $args=@{{Uri=$u;Method=$m;TimeoutSec=$timeout;UseBasicParsing=$true;ErrorAction='Stop'}}; if($headers.Count -gt 0){{$args.Headers=$headers}}; $body={}; if($body -and @('POST','PUT','PATCH') -contains $m){{$args.Body=$body}}; try{{$r=Invoke-WebRequest @args; $content=[string]$r.Content; if($content.Length -gt 4000){{$content=$content.Substring(0,4000)+' …[truncated]'}}; [pscustomobject]@{{status=[int]$r.StatusCode;content=$content}} | ConvertTo-Json -Compress -Depth 4}}catch{{if($_.Exception.Response){{$status=[int]$_.Exception.Response.StatusCode; [pscustomobject]@{{status=$status;content='HTTP request returned an error status'}} | ConvertTo-Json -Compress}}else{{throw}}}}",ps_quote(&url),ps_quote(&method),seconds,ps_quote(&headers),ps_quote(&body));
 let out=powershell(cx,&src).await?;let v:Value=serde_json::from_str(&out).map_err(|e|format!("HTTP response parse failed: {e}"))?;let status=v.get("status").and_then(Value::as_i64).unwrap_or(0);let excerpt=v.get("content").and_then(Value::as_str).unwrap_or("");if !save_body.is_empty(){cx.set_variable(save_body,Value::String(excerpt.to_string()))?};if !save_status.is_empty(){cx.set_variable(save_status,json!(status))?};(cx.log)(if (200..400).contains(&status){"info"}else{"error"},if log_response{format!("HTTP {method} returned status {status}; response excerpt: {}",excerpt.chars().take(4000).collect::<String>())}else{format!("HTTP {method} returned status {status} (response body omitted)")});if (200..400).contains(&status){Ok(Outcome::Satisfied)}else{Err(format!("HTTP request returned status {status}"))}}
}

fn media_spec()->Value{simple_action_spec("Media Control","play","Sends a Windows media key to the active media session.",vec![json!({"key":"operation","label":"Operation","type":"select","default":"play_pause","options":[["play_pause","Play / pause"],["next","Next track"],["previous","Previous track"],["stop","Stop"]]}),json!({"preset":"onFail"})],"Media: {operation}","Uses the Windows media keyboard keys. The media application decides how each key is handled; Stop maps to the media stop key where supported.")}
struct MediaControl;
#[async_trait]
impl Block for MediaControl{
 fn title(&self)-> &'static str{"Media Control"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(media_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&media_spec(),c)}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let op=text(&cx.cfg,"operation");let vk=match op.as_str(){"next"=>0xB0,"previous"=>0xB1,"stop"=>0xB2,_=>0xB3};let src=r#"using System; using System.Runtime.InteropServices; public class MediaNative { [DllImport("user32.dll")] public static extern void keybd_event(byte k,byte scan,uint flags,UIntPtr extra); public static void Press(byte k){keybd_event(k,0,0,UIntPtr.Zero);keybd_event(k,0,2,UIntPtr.Zero);} }"#;let script=format!("Add-Type -TypeDefinition {}; [MediaNative]::Press([byte]{}); 'Media key sent.'",ps_quote(src),vk);let out=powershell(cx,&script).await?;(cx.log)("info",out);Ok(Outcome::Satisfied)}
}

fn power_spec()->Value{simple_action_spec("Power Control","power","Locks, sleeps, hibernates, restarts, shuts down, or cancels a scheduled shutdown.",vec![json!({"key":"operation","label":"Operation","type":"select","default":"lock","options":[["lock","Lock workstation"],["sleep","Sleep"],["hibernate","Hibernate"],["shutdown","Shut down"],["restart","Restart"],["cancel_shutdown","Cancel pending shutdown"]]}),json!({"key":"delaySec","label":"Shutdown/restart delay","type":"number","min":0,"max":600,"step":5,"unit":"s","default":60,"show":{"key":"operation","in":["shutdown","restart"]}}),json!({"preset":"onFail"})],"Power: {operation}","Shutdown/restart default to a 60-second cancellation window. Sleep/hibernate availability depends on hardware and power policy. This block is marked risky; use it only in workflows you trust.")}
struct PowerControl;
#[async_trait]
impl Block for PowerControl{
 fn title(&self)-> &'static str{"Power Control"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(power_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&power_spec(),c)}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let op=text(&cx.cfg,"operation");let delay=rng(&cx.cfg,"delaySec",60.0,0.0,600.0,"Shutdown delay")? as u32;let script=match op.as_str(){"lock"=>"rundll32.exe user32.dll,LockWorkStation".to_string(),"sleep"=>"Add-Type -TypeDefinition 'using System.Runtime.InteropServices; public class PowerNative { [DllImport(\"powrprof.dll\", SetLastError=true)] public static extern bool SetSuspendState(bool hibernate,bool forceCritical,bool disableWakeEvent); }'; [void][PowerNative]::SetSuspendState($false,$false,$false)".into(),"hibernate"=>"shutdown.exe /h".into(),"shutdown"=>format!("shutdown.exe /s /t {delay} /c \"Scheduled by 00047; use Cancel shutdown to abort.\""),"restart"=>format!("shutdown.exe /r /t {delay} /c \"Scheduled by 00047; use Cancel shutdown to abort.\""),"cancel_shutdown"=>"shutdown.exe /a".into(),_=>return Err("Unknown power operation".into())};let out=powershell(cx,&format!("$ErrorActionPreference='Stop'; {script}; 'Power operation requested.'")).await?;(cx.log)("warn",format!("Power control action '{}': {}",op,out));Ok(Outcome::Satisfied)}
}

fn delete_spec()->Value{simple_action_spec("Delete or Recycle File","trash-2","Deletes a file or sends it to the Windows Recycle Bin.",vec![json!({"key":"path","label":"File path","type":"text","required":true,"maxLen":2048}),json!({"key":"mode","label":"Deletion method","type":"select","default":"recycle","options":[["recycle","Send to Recycle Bin"],["permanent","Delete permanently"]]}),json!({"preset":"onFail"})],"Delete: {path}","Does not support wildcard patterns. Permanent deletion is irreversible. Recycle Bin behavior depends on volume and Windows policy.")}
struct DeleteFile;
#[async_trait]
impl Block for DeleteFile{
 fn title(&self)-> &'static str{"Delete or Recycle File"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(delete_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&delete_spec(),c)?;if text(c,"path").contains('\0'){return Err("Path must not contain NUL characters".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let path=cx.expand_template(&text(&cx.cfg,"path"))?;let mode=text(&cx.cfg,"mode");if !Path::new(&path).exists(){return Err("Path does not exist".into())};if mode=="permanent"{if Path::new(&path).is_dir(){std::fs::remove_dir_all(&path).map_err(|e|format!("Directory delete failed: {e}"))?}else{std::fs::remove_file(&path).map_err(|e|format!("File delete failed: {e}"))?}}else{let script=format!("Add-Type -AssemblyName Microsoft.VisualBasic; $p={}; if([System.IO.Directory]::Exists($p)){{[Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($p,'OnlyErrorDialogs','SendToRecycleBin')}}elseif([System.IO.File]::Exists($p)){{[Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($p,'OnlyErrorDialogs','SendToRecycleBin')}}else{{throw 'Path does not exist'}}",ps_quote(&path));let _ = powershell(cx,&script).await?;};(cx.log)("warn",format!("File deletion operation '{}' completed for '{}'",mode,path));Ok(Outcome::Satisfied)}
}

fn sound_spec()->Value{simple_action_spec("Play Sound or Speak Text","volume-2","Plays a WAV file or speaks text using Windows text-to-speech.",vec![json!({"key":"mode","label":"Operation","type":"select","default":"speak","options":[["speak","Speak text"],["sound","Play WAV file"]]}),json!({"key":"text","label":"Text to speak","type":"text","required":true,"maxLen":2000,"show":{"key":"mode","eq":"speak"}}),json!({"key":"path","label":"WAV file path","type":"text","required":true,"maxLen":2048,"show":{"key":"mode","eq":"sound"}}),json!({"preset":"onFail"})],"Audio: {mode}","Text-to-speech uses the Windows System.Speech assembly. WAV playback waits until playback finishes.")}
struct PlaySoundSpeak;
#[async_trait]
impl Block for PlaySoundSpeak{
 fn title(&self)-> &'static str{"Play Sound or Speak Text"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(sound_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&sound_spec(),c)?;if text(c,"text").contains('\0')||text(c,"path").contains('\0'){return Err("Audio input contains a NUL character".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let mode=text(&cx.cfg,"mode");let script=if mode=="sound"{let p=cx.expand_template(&text(&cx.cfg,"path"))?;if !Path::new(&p).is_file(){return Err("WAV file does not exist".into())};format!("$ErrorActionPreference='Stop'; $s=New-Object System.Media.SoundPlayer {}; $s.Load(); $s.PlaySync(); 'Sound playback completed.'",ps_quote(&p))}else{format!("Add-Type -AssemblyName System.Speech; $s=New-Object System.Speech.Synthesis.SpeechSynthesizer; try{{$s.Speak({})}}finally{{$s.Dispose()}}; 'Speech completed.'",ps_quote(&cx.expand_template(&text(&cx.cfg,"text"))?))};let out=powershell(cx,&script).await?;(cx.log)("info",out);Ok(Outcome::Satisfied)}
}

fn service_spec()->Value{simple_action_spec("Start, Stop, or Restart Service","settings","Controls a Windows service by its service name.",vec![json!({"key":"serviceName","label":"Service name","type":"text","required":true,"maxLen":256}),json!({"key":"operation","label":"Operation","type":"select","default":"start","options":[["start","Start"],["stop","Stop"],["restart","Restart"]]}),json!({"preset":"onFail"})],"Service {operation}: {serviceName}","Service control may require administrator privileges. Use the service name, not its display name.")}
struct ServiceControl;
#[async_trait]
impl Block for ServiceControl{
 fn title(&self)-> &'static str{"Start, Stop, or Restart Service"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(service_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&service_spec(),c)?;if text(c,"serviceName").contains('\0'){return Err("Service name must not contain NUL characters".into())};check_timeout(c)}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let n=cx.expand_template(&text(&cx.cfg,"serviceName"))?;let op=text(&cx.cfg,"operation");let verb=match op.as_str(){"start"=>"Start-Service","stop"=>"Stop-Service",_=>"Restart-Service"};let script=format!("$ErrorActionPreference='Stop'; {verb} -Name {} -ErrorAction Stop; (Get-Service -Name {} | Select-Object -First 1).Status.ToString()",ps_quote(&n),ps_quote(&n));let out=powershell(cx,&script).await?;(cx.log)("info",format!("Service {n}: {out}"));Ok(Outcome::Satisfied)}
}

pub(super) fn register_into(m:&mut HashMap<&'static str,Box<dyn Block>>){
 m.insert("cond.network",Box::new(NetworkCondition));
 m.insert("cond.battery_power",Box::new(BatteryPower));
 m.insert("cond.cpu_usage",Box::new(CpuUsage));
 m.insert("cond.disk_space",Box::new(DiskSpace));
 m.insert("cond.file_folder",Box::new(FileFolder));
 m.insert("cond.active_window",Box::new(ActiveWindow));
 m.insert("cond.bluetooth",Box::new(BluetoothDevice));
 m.insert("cond.usb_device",Box::new(UsbDevice));
 m.insert("cond.audio_device",Box::new(AudioDevice));
 m.insert("cond.workstation_lock",Box::new(WorkstationLock));
 m.insert("cond.user_idle",Box::new(UserIdle));
 m.insert("cond.app_responsive",Box::new(AppResponsiveness));
 m.insert("act.open_url",Box::new(OpenUrl));
 m.insert("act.set_volume",Box::new(SetSystemVolume));
 m.insert("act.process_priority",Box::new(SetProcessPriority));
 m.insert("act.control_window",Box::new(ControlWindow));
 m.insert("act.copy_move_file",Box::new(CopyMoveFile));
 m.insert("act.clipboard",Box::new(ClipboardText));
 m.insert("act.http",Box::new(HttpRequest));
 m.insert("act.media",Box::new(MediaControl));
 m.insert("act.power",Box::new(PowerControl));
 m.insert("act.delete_file",Box::new(DeleteFile));
 m.insert("act.play_sound",Box::new(PlaySoundSpeak));
 m.insert("act.service",Box::new(ServiceControl));
}


#[cfg(test)]
mod network_condition_regression_tests {
    use super::*;

    #[test]
    fn wlan_connected_state_is_true_even_when_ssid_is_hidden() {
        let output = "Name : Wi-Fi\nState : connected\nSSID :\nBSSID : 00:11:22:33:44:55\n";
        assert_eq!(parse_wlan_status(output), (true, String::new()));
    }

    #[test]
    fn explicitly_disconnected_state_does_not_trust_a_stale_ssid_line() {
        let output = "Name : Wi-Fi\nState : disconnected\nSSID : PreviousNetwork\n";
        assert_eq!(parse_wlan_status(output), (false, "PreviousNetwork".into()));
    }

    #[test]
    fn ssid_is_a_fallback_when_state_is_not_reported() {
        let output = "Name : Wi-Fi\nSSID : OfficeNet\nBSSID : 00:11:22:33:44:55\n";
        assert_eq!(parse_wlan_status(output), (true, "OfficeNet".into()));
    }

    #[test]
    fn network_rate_converts_bytes_to_mib_per_second_and_mbps() {
        assert_eq!(network_rate(1_048_576, 1.0, "MiB/s"), Some(1.0));
        assert_eq!(network_rate(1_000_000, 1.0, "Mbps"), Some(8.0));
        assert_eq!(network_rate(1, 0.0, "MiB/s"), None);
        assert_eq!(network_rate(1, f64::NAN, "Mbps"), None);
    }
}


#[cfg(test)]
mod generic_network_condition_tests {
    use super::*;

    #[test]
    fn any_network_mode_is_distinct_from_wifi_mode() {
        let spec = network_spec();
        let options = spec["fields"][0]["options"].as_array().unwrap();
        assert!(options.iter().any(|option| option[0] == "network_connected"));
        assert!(options.iter().any(|option| option[0] == "wifi_connected"));
    }

    #[test]
    fn localized_wlan_state_label_uses_ssid_fallback() {
        let output = "Nom : Wi-Fi\nÉtat : connecté\nSSID : OfficeNet\n";
        assert_eq!(parse_wlan_status(output), (true, "OfficeNet".into()));
    }
}
