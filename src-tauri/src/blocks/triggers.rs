//! Event-oriented workflow entry blocks.
use super::{extra, spec, Block, Cat, NodeCx, Outcome};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener, time::Duration};

fn text(c:&Value,k:&str)->String{c.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()}
fn raw(c:&Value,k:&str)->String{c.get(k).and_then(Value::as_str).unwrap_or("").to_string()}
fn no_nul(c:&Value,keys:&[&str])->Result<(),String>{
 for k in keys{if raw(c,k).contains('\0'){return Err(format!("{k} must not contain NUL characters"))}}
 Ok(())
}
fn name_ok(s:&str)->bool{!s.is_empty()&&s.len()<=64&&s.chars().next().is_some_and(|c|c.is_ascii_alphabetic()||c=='_')&&s.chars().all(|c|c.is_ascii_alphanumeric()||matches!(c,'_'|'-'|'.'))}
fn hotkey_spec()->Value{
 let mut f=vec![
  json!({"key":"key","label":"Key","type":"select","default":"F8","options":[["F6","F6"],["F7","F7"],["F8","F8"],["F9","F9"],["F10","F10"],["F11","F11"],["F12","F12"],["Pause","Pause"],["PrintScreen","Print Screen"],["Home","Home"],["End","End"],["Insert","Insert"],["Delete","Delete"],["PageUp","Page Up"],["PageDown","Page Down"]]}),
  json!({"key":"ctrl","label":"Ctrl","type":"bool","default":false}),
  json!({"key":"alt","label":"Alt","type":"bool","default":false}),
  json!({"key":"shift","label":"Shift","type":"bool","default":false}),
 ];
 f.extend(extra::behavior_fields());
 json!({"category":"condition","title":"Global Hotkey Condition","icon":"keyboard","description":"Detects a configured global key or modifier chord using the Windows keyboard state API.","fields":f,"summary":"Hotkey {key}","help":"This is a polling condition, not a registered exclusive shortcut. Prefer function keys and avoid combinations used by Windows or other apps. Repeated automation entries can retrigger while the key remains down; use an appropriate stable duration or separate debounce logic."})
}
fn vk(key:&str)->Option<u16>{match key{"F6"=>Some(0x75),"F7"=>Some(0x76),"F8"=>Some(0x77),"F9"=>Some(0x78),"F10"=>Some(0x79),"F11"=>Some(0x7A),"F12"=>Some(0x7B),"Pause"=>Some(0x13),"PrintScreen"=>Some(0x2C),"Home"=>Some(0x24),"End"=>Some(0x23),"Insert"=>Some(0x2D),"Delete"=>Some(0x2E),"PageUp"=>Some(0x21),"PageDown"=>Some(0x22),_=>None}}
fn hotkey_script(key:u16,ctrl:bool,alt:bool,shift:bool)->String{
 format!(r#"$ErrorActionPreference='Stop'; Add-Type -TypeDefinition 'using System.Runtime.InteropServices; public static class KeyProbe {{ [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int key); }}'; $down=(([KeyProbe]::GetAsyncKeyState({key}) -band 0x8000) -ne 0); $ctrl=(([KeyProbe]::GetAsyncKeyState(0x11) -band 0x8000) -ne 0); $alt=(([KeyProbe]::GetAsyncKeyState(0x12) -band 0x8000) -ne 0); $shift=(([KeyProbe]::GetAsyncKeyState(0x10) -band 0x8000) -ne 0); [bool]($down -and ($ctrl -eq {ctrl}) -and ($alt -eq {alt}) -and ($shift -eq {shift})) | ConvertTo-Json -Compress"#,
 key=key,ctrl=if ctrl{"$true"}else{"$false"},alt=if alt{"$true"}else{"$false"},shift=if shift{"$true"}else{"$false"})
}
struct Hotkey;
#[async_trait]
impl Block for Hotkey{
 fn title(&self)-> &'static str{"Global Hotkey Condition"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(hotkey_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&hotkey_spec(),c)?;if vk(&text(c,"key")).is_none(){return Err("Unknown hotkey key".into())}extra::wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let k=vk(&text(&cx.cfg,"key")).unwrap();let script=hotkey_script(k,cx.cfg.get("ctrl").and_then(Value::as_bool).unwrap_or(false),cx.cfg.get("alt").and_then(Value::as_bool).unwrap_or(false),cx.cfg.get("shift").and_then(Value::as_bool).unwrap_or(false));
  let(behavior,poll,stable,timeout)=extra::wait_settings(&cx.cfg)?;
  extra::poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{
   let out=extra::powershell(cx,&script).await?;
   let down=serde_json::from_str::<bool>(out.trim()).map_err(|e|format!("Could not parse keyboard state: {e}"))?;
   (cx.report)("waiting",None,format!("Hotkey {} is {}",text(&cx.cfg,"key"),if down{"pressed"}else{"not pressed"}));
   Ok(down)
  }) as extra::Probe<'_>).await
 }
}

fn webhook_spec()->Value{json!({"category":"action","title":"Local Webhook Trigger","icon":"network","description":"Listens on localhost for an authenticated HTTP POST and continues the workflow when received.","fields":[
 {"key":"port","label":"Local port","type":"number","min":1024,"max":65535,"default":48747},
 {"key":"tokenVar","label":"Required secret/token variable","type":"text","required":true,"maxLen":64},
 {"key":"saveBodyAs","label":"Save request body as variable","type":"text","default":"webhookBody","required":true,"maxLen":64},
 {"key":"saveHeadersAs","label":"Save safe headers as JSON variable","type":"text","default":"webhookHeaders","required":true,"maxLen":64},
 {"preset":"timeout"},{"preset":"onFail"}],"summary":"Wait for authenticated webhook on port {port}","help":"Binds only to 127.0.0.1, accepts POST only, requires X-00047-Token matching the named workflow variable, caps headers/body at 32 KiB, and saves only the body plus non-secret header names/values selected by the runtime. A local-only listener is not reachable from another machine without a separate trusted relay."})}
struct Webhook;
#[async_trait]
impl Block for Webhook{
 fn title(&self)-> &'static str{"Local Webhook Trigger"}fn category(&self)->Cat{Cat::Action}fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(webhook_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{
  spec::validate(&webhook_spec(),c)?;
  let p=c.get("port").and_then(Value::as_u64).unwrap_or(48747);if !(1024..=65535).contains(&p){return Err("Port must be 1024–65535".into())}
  for k in ["tokenVar","saveBodyAs","saveHeadersAs"]{if !name_ok(&text(c,k)){return Err(format!("{k} must be a valid variable name"))}}
  if text(c,"tokenVar")==text(c,"saveBodyAs")||text(c,"tokenVar")==text(c,"saveHeadersAs"){return Err("Token and output variables must have different names".into())}
  if text(c,"saveBodyAs")==text(c,"saveHeadersAs"){return Err("Body and headers variables must have different names".into())}
  let t=c.get("timeoutSec").and_then(Value::as_f64).unwrap_or(0.0);if !t.is_finite()||!(0.0..=86400.0).contains(&t){return Err("Timeout must be between 0 and 86400 seconds".into())}
  Ok(())
 }
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;
  let port=cx.cfg.get("port").and_then(Value::as_u64).unwrap_or(48747) as u16;
  let token=cx.get_variable(&text(&cx.cfg,"tokenVar")).ok_or("Webhook token variable is not set")?;
  let token=match token{Value::String(s) if !s.is_empty()=>s,_=>return Err("Webhook token must be a non-empty string variable".into())};
  let listener=TcpListener::bind(("127.0.0.1",port)).await.map_err(|e|format!("Could not listen on 127.0.0.1:{port}: {e}"))?;
  (cx.report)("waiting",None,format!("Listening on 127.0.0.1:{port} for authenticated POST"));
  loop {
   let (mut stream,addr)=tokio::select!{r=listener.accept()=>r.map_err(|e|format!("Webhook accept failed: {e}"))?,_=cx.token.cancelled()=>return Err("cancelled".into())};
   // A localhost listener never accepts traffic from non-loopback peers.
   if !addr.ip().is_loopback(){continue}
   let mut request=vec![0u8;8192];
   let mut used=0usize;
   loop{
    if used==request.len(){break}
    let n=tokio::select!{r=stream.read(&mut request[used..])=>r.map_err(|e|format!("Webhook read failed: {e}"))?,_=cx.token.cancelled()=>return Err("cancelled".into())};
    if n==0{break}used+=n;
    if request[..used].windows(4).any(|w|w==b"\r\n\r\n"){break}
   }
   let headers_end=request[..used].windows(4).position(|w|w==b"\r\n\r\n");
   let Some(header_end)=headers_end else {let _=stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;continue};
   let headers=String::from_utf8_lossy(&request[..header_end]);
   let mut lines=headers.split("\r\n");
   let start=lines.next().unwrap_or("");
   if !start.starts_with("POST /") {let _=stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;continue}
   let mut supplied=String::new();let mut length=0usize;let mut safe=serde_json::Map::new();
   for line in lines{
    if let Some((k,v))=line.split_once(':'){
     let key=k.trim();let value=v.trim();
     if key.eq_ignore_ascii_case("X-00047-Token"){supplied=value.to_string();}
     if key.eq_ignore_ascii_case("content-length"){length=value.parse().unwrap_or(usize::MAX);}
     if ["content-type","user-agent","x-request-id"].iter().any(|x|key.eq_ignore_ascii_case(x)){safe.insert(key.to_ascii_lowercase(),Value::String(value.chars().take(256).collect()));}
    }
   }
   if supplied.as_bytes()!=token.as_bytes(){let _=stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;continue}
   if length>16384||length==usize::MAX{let _=stream.write_all(b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;continue}
   let body_start=header_end+4;
   let mut body=request[body_start.min(used)..used].to_vec();
   if body.len()>length{body.truncate(length)}
   while body.len()<length{
    let remain=length-body.len();let take=remain.min(4096);let mut buf=vec![0u8;take];
    let n=tokio::select!{r=stream.read(&mut buf)=>r.map_err(|e|format!("Webhook body read failed: {e}"))?,_=cx.token.cancelled()=>return Err("cancelled".into())};
    if n==0{return Err("Webhook request body ended early".into())}body.extend_from_slice(&buf[..n]);
   }
   let body=String::from_utf8(body).map_err(|_|"Webhook body must be valid UTF-8")?;
   let _=stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
   cx.set_variable(text(&cx.cfg,"saveBodyAs"),Value::String(body))?;
   cx.set_variable(text(&cx.cfg,"saveHeadersAs"),Value::Object(safe))?;
   (cx.log)("info","Authenticated local webhook received; request body omitted from logs".into());
   return Ok(Outcome::Satisfied)
  }
 }
}


fn file_event_spec()->Value{
 json!({"category":"condition","title":"File System Event","icon":"folder-search","description":"Waits for native Windows FileSystemWatcher notifications for files or folders.","fields":[
  {"key":"path","label":"Directory to watch","type":"text","required":true,"maxLen":4000},
  {"key":"pattern","label":"Name filter","type":"text","default":"*","required":true,"maxLen":260},
  {"key":"recursive","label":"Include subdirectories","type":"bool","default":false},
  {"key":"event","label":"Event type","type":"select","default":"any","options":[["created","Created"],["modified","Modified"],["deleted","Deleted"],["renamed","Renamed"],["any","Any file-system event"]]},
  {"key":"savePathAs","label":"Save triggering path to variable (optional)","type":"text","maxLen":64},
  {"preset":"timeout"},{"preset":"onFail"}],
  "summary":"Wait for {event} in {path}",
  "help":"Uses Windows FileSystemWatcher notifications instead of periodic snapshots. Can report the full path of the triggering item to a variable. Network shares, inaccessible directories and OS notification buffer overflow can affect reliability."})
}
struct FileEvent;
#[async_trait]
impl Block for FileEvent{
 fn title(&self)-> &'static str{"File System Event"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(file_event_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{
  spec::validate(&file_event_spec(),c)?;
  if text(c,"path").is_empty(){return Err("Directory to watch is required".into())}
  if text(c,"pattern").is_empty(){return Err("Name filter is required".into())}
  no_nul(c,&["path","pattern","savePathAs"])?;
  if !matches!(text(c,"event").as_str(),"created"|"modified"|"deleted"|"renamed"|"any"){return Err("Unknown file-system event".into())}
  if !text(c,"savePathAs").is_empty()&&!name_ok(&text(c,"savePathAs")){return Err("Output variable name is invalid".into())}
  let t=c.get("timeoutSec").and_then(Value::as_f64).unwrap_or(0.0);if !t.is_finite()||!(0.0..=86400.0).contains(&t){return Err("Timeout must be between 0 and 86400 seconds".into())}
  Ok(())
 }
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;
  let path=cx.expand_template(&text(&cx.cfg,"path"))?;
  let pattern=cx.expand_template(&text(&cx.cfg,"pattern"))?;
  let recursive=cx.cfg.get("recursive").and_then(Value::as_bool).unwrap_or(false);
  let event=text(&cx.cfg,"event");
  let save_as=text(&cx.cfg,"savePathAs");
  let path_q=extra::ps_quote(&path);let pattern_q=extra::ps_quote(&pattern);
  let recursive_q=if recursive{"$true"}else{"$false"};
  let registrations=match event.as_str(){
   "created"=>vec!["Created"],"modified"=>vec!["Changed"],"deleted"=>vec!["Deleted"],"renamed"=>vec!["Renamed"],
   _=>vec!["Created","Changed","Deleted","Renamed"]
  };
  let registrations=registrations.iter().map(|e|format!("Register-ObjectEvent -InputObject $watcher -EventName {e} -SourceIdentifier ($src+'-{e}') | Out-Null;")).collect::<String>();
  let script=format!(r#"$ErrorActionPreference='Stop';$directory={path};$filter={pattern};if(-not(Test-Path -LiteralPath $directory -PathType Container)){{throw 'Watch directory does not exist'}};
$watcher=New-Object IO.FileSystemWatcher;$watcher.Path=$directory;$watcher.Filter=$filter;$watcher.IncludeSubdirectories={recursive};$watcher.NotifyFilter=[IO.NotifyFilters]::FileName -bor [IO.NotifyFilters]::DirectoryName -bor [IO.NotifyFilters]::LastWrite -bor [IO.NotifyFilters]::Size;
$src='00047-'+[guid]::NewGuid().ToString('N');{registrations}$watcher.EnableRaisingEvents=$true;
try{{while($true){{$ev=Wait-Event;$match=$false;foreach($suffix in @('Created','Changed','Deleted','Renamed')){{if($ev.SourceIdentifier -eq ($src+'-'+$suffix)){{$match=$true;break}}}};if(-not $match){{continue}};$args=$ev.SourceEventArgs;$full=[string]$args.FullPath;$kind=$ev.SourceIdentifier.Substring($src.Length+1);$old='';if($args -is [System.IO.RenamedEventArgs]){{$old=[string]$args.OldFullPath}};Remove-Event -EventIdentifier $ev.EventIdentifier -ErrorAction SilentlyContinue;[pscustomobject]@{{event=$kind;path=$full;oldPath=$old}}|ConvertTo-Json -Compress;break}}}}
finally{{foreach($suffix in @('Created','Changed','Deleted','Renamed')){{Unregister-Event -SourceIdentifier ($src+'-'+$suffix) -ErrorAction SilentlyContinue}};$watcher.EnableRaisingEvents=$false;$watcher.Dispose();Get-Event -ErrorAction SilentlyContinue|Where-Object{{$_.SourceIdentifier -like ($src+'-*')}}|Remove-Event -ErrorAction SilentlyContinue}}"#,
 path=path_q,pattern=pattern_q,recursive=recursive_q,registrations=registrations);
  (cx.report)("waiting",None,format!("Watching {path} for {event} events matching {pattern}"));
  let out=extra::powershell(cx,&script).await?;
  if out.is_empty(){return Err("File system watcher ended without an event".into())}
  let v:Value=serde_json::from_str(&out).map_err(|e|format!("Could not parse file-system event: {e}"))?;
  if !save_as.is_empty(){cx.set_variable(save_as,Value::String(v.get("path").and_then(Value::as_str).unwrap_or("").to_string()))?}
  (cx.log)("info",format!("File system event {}: {}",v.get("event").and_then(Value::as_str).unwrap_or("unknown"),v.get("path").and_then(Value::as_str).unwrap_or("")));
  Ok(Outcome::Satisfied)
 }
}

fn process_event_spec()->Value{
 json!({"category":"condition","title":"Process Start / Exit Event","icon":"activity","description":"Waits for the Windows process-event provider to report a matching process start or exit.","fields":[
  {"key":"event","label":"Event","type":"select","default":"started","options":[["started","Process started"],["exited","Process exited"]]},
  {"key":"processName","label":"Process executable name","type":"text","required":true,"maxLen":260,"placeholder":"notepad.exe"},
  {"preset":"timeout"},{"preset":"onFail"}],
  "summary":"Wait for {processName} {event}",
  "help":"Uses the Windows WMI/CIM process event provider (Win32_ProcessStartTrace / Win32_ProcessStopTrace). Matching events are detected without polling. System WMI availability and permissions apply."})
}
struct ProcessEvent;
#[async_trait]
impl Block for ProcessEvent{
 fn title(&self)-> &'static str{"Process Start / Exit Event"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(process_event_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{
  spec::validate(&process_event_spec(),c)?;
  if text(c,"processName").is_empty(){return Err("Process executable name is required".into())}
  let p=text(c,"processName");if p.contains('/')||p.contains('\\')||p.contains('\0'){return Err("Use an executable name, not a path".into())}
  if !matches!(text(c,"event").as_str(),"started"|"exited"){return Err("Unknown process event".into())}
  let t=c.get("timeoutSec").and_then(Value::as_f64).unwrap_or(0.0);if !t.is_finite()||!(0.0..=86400.0).contains(&t){return Err("Timeout must be between 0 and 86400 seconds".into())}
  Ok(())
 }
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;
  let name=text(&cx.cfg,"processName");let event=text(&cx.cfg,"event");
  let class=if event=="started"{"Win32_ProcessStartTrace"}else{"Win32_ProcessStopTrace"};
  let query=format!("SELECT * FROM {class}");
  let script=format!(r#"$ErrorActionPreference='Stop';$query={};$needle={};$src='00047-'+[guid]::NewGuid().ToString('N');
Register-CimIndicationEvent -Namespace root/cimv2 -Query $query -SourceIdentifier $src | Out-Null;
try{{while($true){{$ev=Wait-Event -SourceIdentifier $src;if($null -eq $ev){{continue}};$rec=$ev.SourceEventArgs.NewEvent;$match=([string]$rec.ProcessName -ieq $needle);$pidValue=[uint32]$rec.ProcessID;Remove-Event -EventIdentifier $ev.EventIdentifier -ErrorAction SilentlyContinue;if($match){{[pscustomobject]@{{processName=[string]$rec.ProcessName;processId=$pidValue}}|ConvertTo-Json -Compress;break}}}}}}
finally{{Unregister-Event -SourceIdentifier $src -ErrorAction SilentlyContinue;Get-Event -SourceIdentifier $src -ErrorAction SilentlyContinue|Remove-Event -ErrorAction SilentlyContinue}}"#,extra::ps_quote(&query),extra::ps_quote(&name));
  (cx.report)("waiting",None,format!("Waiting for process {name} to {event} via Windows process events"));
  let out=extra::powershell(cx,&script).await?;
  if out.is_empty(){return Err("Process event listener ended without a matching event".into())}
  let data:Value=serde_json::from_str(&out).map_err(|e|format!("Could not parse process event: {e}"))?;
  (cx.log)("info",format!("Observed process {}: {} (PID {})",event,data.get("processName").and_then(Value::as_str).unwrap_or(&name),data.get("processId").and_then(Value::as_u64).unwrap_or(0)));
  Ok(Outcome::Satisfied)
 }
}

fn windows_event_spec()->Value{
 let mut fields=vec![
  json!({"key":"logName","label":"Event log","type":"select","default":"System","options":[["System","System"],["Application","Application"],["Security","Security"],["Setup","Setup"]]}),
  json!({"key":"eventIds","label":"Event IDs (comma-separated, optional)","type":"text","maxLen":128,"placeholder":"4801,1074"}),
  json!({"key":"provider","label":"Provider/source contains (optional)","type":"text","maxLen":256}),
  json!({"key":"messageContains","label":"Event message contains (optional)","type":"text","maxLen":512}),
 ];
 fields.extend(extra::behavior_fields());
 json!({"category":"condition","title":"Windows Event Log Condition","icon":"calendar-clock","description":"Matches recent Windows Event Log records by log, event IDs, provider, and message content.","fields":fields,"summary":"Windows event: {logName} {eventIds}","help":"Only records created after this block begins are considered. At most 100 recent records are inspected per poll. Reading Security events can require additional permissions. Use the System log for common power, sleep/resume and device events; use Security for logon/unlock auditing when enabled."})
}
struct WindowsEvent;
#[async_trait]
impl Block for WindowsEvent{
 fn title(&self)-> &'static str{"Windows Event Log Condition"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(windows_event_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{
  spec::validate(&windows_event_spec(),c)?;
  if !matches!(text(c,"logName").as_str(),"System"|"Application"|"Security"|"Setup"){return Err("Unknown Windows event log".into())}
  for k in ["eventIds","provider","messageContains"]{if raw(c,k).contains('\0'){return Err(format!("{k} must not contain NUL"))}}
  let ids=text(c,"eventIds");
  if !ids.is_empty(){let parts:Vec<_>=ids.split(',').collect();if parts.len()>20{return Err("Enter at most 20 event IDs".into())}for p in parts{if p.trim().parse::<u32>().is_err(){return Err("Event IDs must be comma-separated whole numbers".into())}}}
  extra::wait_settings(c).map(|_|())
 }
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;
  let log=extra::ps_quote(&text(&cx.cfg,"logName"));
  let ids:Vec<String>=text(&cx.cfg,"eventIds").split(',').map(str::trim).filter(|s|!s.is_empty()).map(|s|s.parse::<u32>().map(|x|x.to_string()).map_err(|_|"Invalid event ID".to_string())).collect::<Result<_,_>>()?;
  let idlist=ids.join(",");
  let provider=extra::ps_quote(&text(&cx.cfg,"provider"));
  let message=extra::ps_quote(&text(&cx.cfg,"messageContains"));
  use std::time::{SystemTime,UNIX_EPOCH};
  let start=SystemTime::now().duration_since(UNIX_EPOCH).map(|d|d.as_secs()).unwrap_or(0).to_string();
  let script=format!(r#"$ErrorActionPreference='Stop';$log={log};$ids={ids};$provider={provider};$needle={message};$start=[DateTimeOffset]::FromUnixTimeSeconds([long]({start})).LocalDateTime;
$items=@(Get-WinEvent -FilterHashtable @{{LogName=$log;StartTime=$start}} -MaxEvents 100 -ErrorAction SilentlyContinue);
$match=@($items|Where-Object{{($ids -eq '' -or $ids.Split(',') -contains [string]$_.Id) -and ($provider -eq '' -or $_.ProviderName -like ('*'+$provider+'*')) -and ($needle -eq '' -or $_.Message -like ('*'+$needle+'*'))}}|Select-Object -First 1);
if($match.Count -gt 0){{[pscustomobject]@{{matched=$true;id=$match[0].Id;provider=$match[0].ProviderName;time=$match[0].TimeCreated.ToString('o')}}|ConvertTo-Json -Compress}}else{{[pscustomobject]@{{matched=$false;id=0;provider='';time=''}}|ConvertTo-Json -Compress}}"#,
 log=log,ids=extra::ps_quote(&idlist),provider=provider,message=message,start=start);
  let(behavior,poll,stable,timeout)=extra::wait_settings(&cx.cfg)?;
  extra::poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{
   let out=extra::powershell(cx,&script).await?;let v:Value=serde_json::from_str(&out).map_err(|e|format!("Could not parse Windows Event Log result: {e}"))?;
   let matched=v.get("matched").and_then(Value::as_bool).unwrap_or(false);
   (cx.report)("waiting",None,format!("Windows Event Log {}: {}",text(&cx.cfg,"logName"),if matched{"matching event found"}else{"no matching event yet"}));
   if matched {(cx.log)("info",format!("Windows Event Log match: ID {} · {}",v.get("id").and_then(Value::as_u64).unwrap_or(0),v.get("provider").and_then(Value::as_str).unwrap_or("")));}
   Ok(matched)
  }) as extra::Probe<'_>).await
 }
}

pub(super) fn register_into(m:&mut std::collections::HashMap<&'static str,Box<dyn Block>>){
 m.insert("cond.hotkey",Box::new(Hotkey));
 m.insert("act.webhook_trigger",Box::new(Webhook));
 m.insert("cond.process_event",Box::new(ProcessEvent));
 m.insert("cond.windows_event",Box::new(WindowsEvent));
 m.insert("cond.file_event",Box::new(FileEvent));
}
#[cfg(test)]
mod tests{
 use super::*;
 #[test]fn hotkey_map_has_safe_known_keys(){assert_eq!(vk("F8"),Some(0x77));assert_eq!(vk("F12"),Some(0x7B));assert_eq!(vk("Unknown"),None);}
 #[test]fn webhook_requires_separate_valid_variables(){assert!(Webhook.validate(&json!({"port":48747,"tokenVar":"hookToken","saveBodyAs":"body","saveHeadersAs":"headers","timeoutSec":0,"onFail":"stop"})).is_ok());assert!(Webhook.validate(&json!({"port":48747,"tokenVar":"x","saveBodyAs":"x","saveHeadersAs":"headers","timeoutSec":0,"onFail":"stop"})).is_err());}
 #[test]fn file_event_fields_are_validated(){
  assert!(FileEvent.validate(&json!({"path":"C:\\\\Temp","pattern":"*.log","recursive":true,"event":"created","savePathAs":"changedPath","timeoutSec":30,"onFail":"stop"})).is_ok());
  assert!(FileEvent.validate(&json!({"path":"","pattern":"*","event":"any","timeoutSec":0,"onFail":"stop"})).is_err());
  assert!(FileEvent.validate(&json!({"path":"C:\\\\Temp","pattern":"*","event":"wrong","timeoutSec":0,"onFail":"stop"})).is_err());
 }
 #[test]fn windows_event_ids_are_validated(){assert!(WindowsEvent.validate(&json!({"logName":"System","eventIds":"4801,1074","provider":"","messageContains":"","behavior":"check","pollSec":2,"stableForSec":0,"timeoutSec":0,"onFail":"stop"})).is_ok());assert!(WindowsEvent.validate(&json!({"logName":"System","eventIds":"bad","behavior":"check","onFail":"stop"})).is_err());}
 #[test]fn process_event_requires_executable_name(){assert!(ProcessEvent.validate(&json!({"event":"started","processName":"notepad.exe","timeoutSec":30,"onFail":"stop"})).is_ok());assert!(ProcessEvent.validate(&json!({"event":"started","processName":"C:\\\\Windows\\\\notepad.exe","timeoutSec":30,"onFail":"stop"})).is_err());}
}
