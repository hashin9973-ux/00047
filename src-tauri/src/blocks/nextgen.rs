//! Advanced Windows UI Automation, event triggers, data processing and flow-control blocks.
use super::{extra, spec, text, Block, Cat, NodeCx, Outcome};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{fs, path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn raw(c: &Value, k: &str) -> String { c.get(k).and_then(Value::as_str).unwrap_or("").to_string() }
fn num(c: &Value, k: &str, d: f64, lo: f64, hi: f64) -> Result<f64, String> {
    let n = c.get(k).and_then(Value::as_f64).unwrap_or(d);
    if !n.is_finite() || n < lo || n > hi { return Err(format!("{k} must be between {lo} and {hi}")); }
    Ok(n)
}
fn nul(c: &Value, keys: &[&str]) -> Result<(), String> {
    if keys.iter().any(|k| raw(c,k).contains('\0')) { Err("Text settings must not contain NUL characters".into()) } else { Ok(()) }
}
fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c,'_'|'-'|'.'))
}
fn q(s: &str) -> String { format!("'{}'", s.replace('\0',"").replace('\'',"''")) }
fn condition(title: &str, icon: &str, desc: &str, fields: Vec<Value>, summary: &str, help: &str) -> Value {
    json!({"category":"condition","title":title,"icon":icon,"description":desc,"fields":fields,"summary":summary,"help":help})
}
fn action(title: &str, icon: &str, desc: &str, fields: Vec<Value>, summary: &str, help: &str) -> Value {
    json!({"category":"action","title":title,"icon":icon,"description":desc,"fields":fields,"summary":summary,"help":help})
}
fn wait_fields(mut fields: Vec<Value>) -> Vec<Value> {
    fields.retain(|f| f.get("preset").and_then(Value::as_str) != Some("onFail"));
    fields.extend(extra::behavior_fields());
    fields
}
fn field(spec: &Value, key: &str) -> Value {
    spec.get("fields").and_then(Value::as_array).and_then(|a| a.iter().find(|x| x.get("key").and_then(Value::as_str)==Some(key))).cloned().unwrap_or(Value::Null)
}

struct UiElementCondition;
fn ui_condition_spec() -> Value {
    let mut f = vec![
        json!({"key":"matchBy","label":"Match by","type":"select","default":"automationId","options":[["automationId","Automation ID"],["name","Accessible name"]]}),
        json!({"key":"query","label":"Element name / Automation ID","type":"text","required":true,"maxLen":512}),
        json!({"key":"controlType","label":"Control type (optional)","type":"select","default":"any","options":[["any","Any control"],["Button","Button"],["Edit","Text field"],["CheckBox","Checkbox"],["ComboBox","Combo box"],["ListItem","List item"],["MenuItem","Menu item"],["Text","Text"],["Window","Window"]]}),
        json!({"key":"state","label":"Condition","type":"select","default":"exists","options":[["exists","Exists"],["enabled","Is enabled"],["visible","Is visible"],["value_equals","Value equals"],["value_contains","Value contains"]]}),
        json!({"key":"expectedValue","label":"Expected value","type":"text","maxLen":2048,"show":{"key":"state","in":["value_equals","value_contains"]}}),
    ];
    f = wait_fields(f);
    condition("Wait for UI Element","app-window","Queries the Windows UI Automation tree by Automation ID or accessible name. Waits for an element state without relying on screen coordinates.",f,"UI element {query}","Requires the target application to expose the control through Windows UI Automation. Secure desktops and some custom-rendered apps cannot be inspected.")
}
async fn ui_probe(cx:&NodeCx, by:&str, query:&str, control_type:&str, state:&str, expected:&str)->Result<bool,String>{
    let by = q(by); let query = cx.expand_template(query)?; let query=q(&query);
    let ct=q(control_type); let state=q(state); let expected=cx.expand_template(expected)?; let expected=q(&expected);
    let script=format!(r#"$ErrorActionPreference='Stop'
[void][Reflection.Assembly]::LoadWithPartialName('UIAutomationClient')
[void][Reflection.Assembly]::LoadWithPartialName('UIAutomationTypes')
$by={by};$q={query};$ct={ct};$state={state};$expected={expected}
$root=[System.Windows.Automation.AutomationElement]::RootElement
if($by -eq 'automationId'){{$prop=[System.Windows.Automation.AutomationElement]::AutomationIdProperty}}else{{$prop=[System.Windows.Automation.AutomationElement]::NameProperty}}
$cond=New-Object System.Windows.Automation.PropertyCondition($prop,$q,[System.Windows.Automation.PropertyConditionFlags]::IgnoreCase)
$all=$root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$cond)
$el=$null
foreach($x in $all){{if($ct -ne 'any' -and $x.Current.ControlType.ProgrammaticName -ne ('ControlType.'+$ct)){{continue}};$el=$x;break}}
$ok=$false
if($null -ne $el){{
 switch($state){{
  'exists'{{$ok=$true}}
  'enabled'{{$ok=[bool]$el.Current.IsEnabled}}
  'visible'{{$ok=(-not [bool]$el.Current.IsOffscreen)}}
  'value_equals'{{$v='';try{{$v=$el.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value}}catch{{try{{$v=$el.Current.Name}}catch{{}}}};$ok=([string]$v -ceq $expected)}}
  'value_contains'{{$v='';try{{$v=$el.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value}}catch{{try{{$v=$el.Current.Name}}catch{{}}}};$ok=([string]$v).IndexOf($expected,[StringComparison]::OrdinalIgnoreCase) -ge 0}}
 }}
}}
[bool]$ok | ConvertTo-Json -Compress"#,by=by,query=query,ct=ct,state=state,expected=expected);
    let out=extra::powershell(cx,&script).await?; serde_json::from_str::<bool>(out.trim()).map_err(|e|format!("Could not parse UI Automation result: {e}"))
}
#[async_trait]
impl Block for UiElementCondition {
    fn title(&self)-> &'static str{"Wait for UI Element"} fn category(&self)->Cat{Cat::Condition}
    fn supports_timeout(&self)->bool{true} fn spec(&self)->Option<Value>{Some(ui_condition_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{
        spec::validate(&ui_condition_spec(),c)?;nul(c,&["query","expectedValue"])?;
        if raw(c,"query").trim().is_empty(){return Err("Element query is required".into())}
        if !matches!(text(c,"matchBy").as_str(),"automationId"|"name"){return Err("Unknown UI element match mode".into())}
        extra::wait_settings(c).map(|_|())
    }
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let by=text(&cx.cfg,"matchBy");let query=text(&cx.cfg,"query");let ct=text(&cx.cfg,"controlType");let state=text(&cx.cfg,"state");let expected=raw(&cx.cfg,"expectedValue");
        let (behavior,poll,stable,timeout)=extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx,&behavior,poll,stable,timeout,||{
            let by=by.clone();let query=query.clone();let ct=ct.clone();let state=state.clone();let expected=expected.clone();
            Box::pin(async move{ui_probe(cx,&by,&query,&ct,&state,&expected).await}) as extra::Probe<'_>
        }).await
    }
}

struct UiElementAction;
fn ui_action_spec()->Value{
 action("Interact with UI Element","mouse-pointer-2","Finds a UI Automation control and invokes it, sets its value, toggles it, selects it, or focuses it.",vec![
  json!({"key":"matchBy","label":"Match by","type":"select","default":"automationId","options":[["automationId","Automation ID"],["name","Accessible name"]]}),
  json!({"key":"query","label":"Element name / Automation ID","type":"text","required":true,"maxLen":512}),
  json!({"key":"controlType","label":"Control type (optional)","type":"select","default":"any","options":[["any","Any control"],["Button","Button"],["Edit","Text field"],["CheckBox","Checkbox"],["ComboBox","Combo box"],["ListItem","List item"],["MenuItem","Menu item"],["Window","Window"]]}),
  json!({"key":"operation","label":"Operation","type":"select","default":"invoke","options":[["invoke","Invoke / click"],["set_value","Set text value"],["select","Select item"],["toggle","Toggle checkbox"],["focus","Focus control"]]}),
  json!({"key":"value","label":"Value (where required)","type":"textarea","rows":2,"maxLen":4096,"show":{"key":"operation","in":["set_value","select"]}}),
  json!({"preset":"timeout","default":10}),json!({"preset":"onFail"})
 ],"UI {operation} {query}","UI Automation uses accessible control identifiers; it does not send blind coordinate clicks. Target application and current user must have a compatible interactive desktop.")
}
#[async_trait]
impl Block for UiElementAction{
 fn title(&self)-> &'static str{"Interact with UI Element"} fn category(&self)->Cat{Cat::Action} fn supports_timeout(&self)->bool{true} fn risky(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(ui_action_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{
  spec::validate(&ui_action_spec(),c)?;nul(c,&["query","value"])?;
  if raw(c,"query").trim().is_empty(){return Err("Element query is required".into())}
  if !matches!(text(c,"operation").as_str(),"invoke"|"set_value"|"select"|"toggle"|"focus"){return Err("Unknown UI element operation".into())}
  if text(c,"operation")=="set_value" && raw(c,"value").is_empty(){return Err("Value is required".into())}
  Ok(())
 }
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let by=q(&text(&cx.cfg,"matchBy"));let query=q(&cx.expand_template(&text(&cx.cfg,"query"))?);let ct=q(&text(&cx.cfg,"controlType"));let op=q(&text(&cx.cfg,"operation"));let value=q(&cx.expand_template(&raw(&cx.cfg,"value"))?);
  let script=format!(r#"$ErrorActionPreference='Stop'
[void][Reflection.Assembly]::LoadWithPartialName('UIAutomationClient')
[void][Reflection.Assembly]::LoadWithPartialName('UIAutomationTypes')
$by={by};$q={query};$ct={ct};$op={op};$value={value}
$root=[System.Windows.Automation.AutomationElement]::RootElement
if($by -eq 'automationId'){{$prop=[System.Windows.Automation.AutomationElement]::AutomationIdProperty}}else{{$prop=[System.Windows.Automation.AutomationElement]::NameProperty}}
$cond=New-Object System.Windows.Automation.PropertyCondition($prop,$q,[System.Windows.Automation.PropertyConditionFlags]::IgnoreCase)
$all=$root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$cond);$el=$null
foreach($x in $all){{if($ct -ne 'any' -and $x.Current.ControlType.ProgrammaticName -ne ('ControlType.'+$ct)){{continue}};$el=$x;break}}
if($null -eq $el){{throw 'UI Automation element was not found'}}
$el.SetFocus()
switch($op){{
 'focus'{{}}
 'invoke'{{$el.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()}}
 'set_value'{{$el.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($value)}}
 'toggle'{{$el.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()}}
 'select'{{$el.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()}}
}}
Write-Output 'ok'"#,by=by,query=query,ct=ct,op=op,value=value);
  extra::powershell(cx,&script).await?;Ok(Outcome::Satisfied)
 }
}

// Waits for a single loopback HTTP POST. It is deliberately bound to 127.0.0.1 and capped to 64 KiB.
struct WebhookTrigger;
fn webhook_spec()->Value{
 condition("Local Webhook Trigger","network","Waits for a POST request on a loopback-only listener. Best used in an auto-start automation; stores the bounded request body in a run variable.",vec![
  json!({"key":"port","label":"Local port","type":"number","min":1024,"max":65535,"step":1,"default":47847}),
  json!({"key":"path","label":"Request path","type":"text","default":"/trigger","required":true,"maxLen":256}),
  json!({"key":"token","label":"Shared token (optional)","type":"password","maxLen":512}),
  json!({"key":"saveBodyAs","label":"Save JSON/text body to variable","type":"text","default":"webhook_body","required":true,"maxLen":64}),
  json!({"preset":"timeout","default":0}),json!({"preset":"onFail"})
 ],"Webhook on port {port}","Only localhost can connect directly. A reverse proxy can change the exposure boundary, so do not expose the service publicly without a trusted proxy and strong token. Maximum body size is 64 KiB.")
}
#[async_trait]
impl Block for WebhookTrigger{
 fn title(&self)-> &'static str{"Local Webhook Trigger"} fn category(&self)->Cat{Cat::Condition} fn supports_timeout(&self)->bool{true} fn spec(&self)->Option<Value>{Some(webhook_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{
  spec::validate(&webhook_spec(),c)?;nul(c,&["path","token","saveBodyAs"])?;
  let p=num(c,"port",47847.0,1024.0,65535.0)?;if p.fract()!=0.0{return Err("Port must be a whole number".into())}
  let path=text(c,"path");if !path.starts_with('/')||path.contains(' ')||path.contains('#'){return Err("Request path must start with / and contain no spaces or fragments".into())}
  if !valid_name(&text(c,"saveBodyAs")){return Err("Output variable name is invalid".into())}
  Ok(())
 }
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let port=num(&cx.cfg,"port",47847.0,1024.0,65535.0)? as u16;let path=text(&cx.cfg,"path");let token=cx.expand_template(&raw(&cx.cfg,"token"))?;let save=text(&cx.cfg,"saveBodyAs");
  let listener=TcpListener::bind(("127.0.0.1",port)).await.map_err(|e|format!("Could not bind local webhook port {port}: {e}"))?;
  (cx.report)("waiting",None,format!("Listening on http://127.0.0.1:{port}{path}"));
  loop {
   let (mut stream,_)=tokio::select!{_=cx.token.cancelled()=>return Err("cancelled".into()),r=listener.accept()=>r.map_err(|e|format!("Webhook accept failed: {e}"))?};
   let request=async{
    let mut data=Vec::with_capacity(4096);let mut buf=[0u8;4096];let mut target_len:Option<usize>=None;
    loop{
     let n=stream.read(&mut buf).await.map_err(|e|e.to_string())?;if n==0{break}data.extend_from_slice(&buf[..n]);
     if data.len()>65536{return Err("Webhook request exceeded 64 KiB".to_string())}
     if target_len.is_none(){if let Some(pos)=data.windows(4).position(|w|w==b"\r\n\r\n"){let headers=String::from_utf8_lossy(&data[..pos]);let mut len=0usize;for h in headers.lines().skip(1){if let Some((k,v))=h.split_once(':'){if k.eq_ignore_ascii_case("Content-Length"){len=v.trim().parse::<usize>().map_err(|_|"Invalid Content-Length".to_string())?}}}target_len=Some(pos+4+len);}}
     if target_len.is_some_and(|end|data.len()>=end){break}
    }
    let text=String::from_utf8_lossy(&data).to_string();let header_end=text.find("\r\n\r\n").ok_or_else(||"Malformed HTTP request".to_string())?;
    let headers=&text[..header_end];let first=headers.lines().next().unwrap_or("");let mut parts=first.split_whitespace();let method=parts.next().unwrap_or("");let target=parts.next().unwrap_or("");let actual_path=target.split('?').next().unwrap_or("");
    let mut received_token="";let mut content_len=0usize;
    for h in headers.lines().skip(1){if let Some((k,v))=h.split_once(':'){if k.eq_ignore_ascii_case("X-00047-Token"){received_token=v.trim()}if k.eq_ignore_ascii_case("Content-Length"){content_len=v.trim().parse::<usize>().unwrap_or(0)}}}
    let body_start=header_end+4;let body_end=(body_start+content_len).min(data.len());let body=String::from_utf8_lossy(&data[body_start..body_end]).to_string();
    Ok::<_,String>((method.to_string(),actual_path.to_string(),received_token.to_string(),body))
   };
   let request=tokio::select!{_=cx.token.cancelled()=>return Err("cancelled".into()),r=tokio::time::timeout(Duration::from_secs(3),request)=>match r{Ok(v)=>v,Err(_)=>Err("Timed out reading webhook request".into())}};
   let (method,got_path,got_token,body)=match request{Ok(v)=>v,Err(e)=>{let _=stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await; (cx.log)("warn",format!("Rejected webhook request: {e}"));continue}};
   let valid=method=="POST"&&got_path==path&&(token.is_empty()||got_token==token);
   if !valid{let (code,msg)=if !token.is_empty()&&got_token!=token{("403 Forbidden","Forbidden")}else{("404 Not Found","Not found")};let response=format!("HTTP/1.1 {code}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{msg}",msg.len());let _=stream.write_all(response.as_bytes()).await;continue}
   cx.set_variable(save.clone(),Value::String(body.clone()))?;
   let _=stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK").await;
   (cx.log)("info",format!("Webhook received ({} bytes); body stored in variable {save}",body.len()));
   return Ok(Outcome::Satisfied)
  }
 }
}

struct HotkeyTrigger;
fn hotkey_spec()->Value{
 let keys:Vec<Value>=(b'A'..=b'Z').map(|x|json!([format!("{}",x as char),format!("{}",x as char)])).chain((1..=12).map(|n|json!([format!("F{n}"),format!("F{n}")]))).collect();
 condition("Global Hotkey Trigger","keyboard","Waits for a registered Windows global shortcut and follows IF when it is pressed.",vec![
  json!({"key":"key","label":"Key","type":"select","default":"F8","options":keys}),
  json!({"key":"ctrl","label":"Ctrl","type":"bool","default":true}),
  json!({"key":"alt","label":"Alt","type":"bool","default":true}),
  json!({"key":"shift","label":"Shift","type":"bool","default":false}),
  json!({"key":"win","label":"Windows key","type":"bool","default":false}),
  json!({"preset":"timeout","default":0}),json!({"preset":"onFail"})
 ],"Hotkey {key}","Runs under the current interactive Windows session. Choose a shortcut not already claimed by another application. The OS releases the registration when the waiting block ends.")
}
#[async_trait]
impl Block for HotkeyTrigger{
 fn title(&self)-> &'static str{"Global Hotkey Trigger"} fn category(&self)->Cat{Cat::Condition} fn supports_timeout(&self)->bool{true} fn spec(&self)->Option<Value>{Some(hotkey_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&hotkey_spec(),c)?;if !["A","B","C","D","E","F","G","H","I","J","K","L","M","N","O","P","Q","R","S","T","U","V","W","X","Y","Z","F1","F2","F3","F4","F5","F6","F7","F8","F9","F10","F11","F12"].contains(&text(c,"key").as_str()){return Err("Unsupported hotkey key".into())}if !["ctrl","alt","shift","win"].iter().any(|k|c.get(k).and_then(Value::as_bool)==Some(true)){return Err("Select at least one modifier".into())}Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;
  #[cfg(not(windows))]{let _=cx;return Err("Global Hotkey Trigger is supported only on Windows.".into())}
  #[cfg(windows)]{
   let key=text(&cx.cfg,"key");let vk=if key.starts_with('F'){0x70+key[1..].parse::<u32>().unwrap_or(8)-1}else{key.as_bytes()[0] as u32};
   let mut mods=0u32;if cx.cfg.get("alt").and_then(Value::as_bool)==Some(true){mods|=1}if cx.cfg.get("ctrl").and_then(Value::as_bool)==Some(true){mods|=2}if cx.cfg.get("shift").and_then(Value::as_bool)==Some(true){mods|=4}if cx.cfg.get("win").and_then(Value::as_bool)==Some(true){mods|=8}
   let script=format!(r#"$ErrorActionPreference='Stop'
$source=@'
using System;
using System.Runtime.InteropServices;
public static class WmHotKeyWaiter {{
 [StructLayout(LayoutKind.Sequential)] public struct POINT {{ public int x; public int y; }}
 [StructLayout(LayoutKind.Sequential)] public struct MSG {{ public IntPtr hwnd; public uint message; public UIntPtr wParam; public IntPtr lParam; public uint time; public POINT pt; }}
 [DllImport("user32.dll",SetLastError=true)] static extern bool RegisterHotKey(IntPtr hWnd,int id,uint fsModifiers,uint vk);
 [DllImport("user32.dll")] static extern int GetMessage(out MSG msg,IntPtr hWnd,uint min,uint max);
 [DllImport("user32.dll")] static extern bool UnregisterHotKey(IntPtr hWnd,int id);
 public static bool Wait(uint modifiers,uint key) {{ const int id=0x47047; if(!RegisterHotKey(IntPtr.Zero,id,modifiers,key)) throw new Exception("Could not register hotkey. It may already be in use."); try {{ MSG msg; while(GetMessage(out msg,IntPtr.Zero,0,0)>0) if(msg.message==0x0312 && msg.wParam.ToUInt64()==id) return true; return false; }} finally {{ UnregisterHotKey(IntPtr.Zero,id); }} }}
}}
'@
Add-Type -TypeDefinition $source
[WmHotKeyWaiter]::Wait([uint32]{mods},[uint32]{vk}) | ConvertTo-Json -Compress"#,mods=mods,vk=vk);
   let out=extra::powershell(cx,&script).await?;let ok=serde_json::from_str::<bool>(out.trim()).unwrap_or(false);Ok(if ok{Outcome::Satisfied}else{Outcome::NotSatisfied})
  }
 }
}

struct JsonExtract;
fn json_extract_spec()->Value{action("Extract JSON Value","file-json","Parses JSON text or reads an array/object variable and stores a nested property as a typed workflow value.",vec![
 json!({"key":"sourceVariable","label":"Source variable (optional)","type":"text","maxLen":64}),
 json!({"key":"jsonText","label":"JSON text","type":"textarea","rows":4,"maxLen":65536,"show":{"key":"sourceVariable","eq":""}}),
 json!({"key":"path","label":"Property path","type":"text","required":true,"maxLen":512,"placeholder":"data.items.0.name"}),
 json!({"key":"saveAs","label":"Save value as variable","type":"text","required":true,"maxLen":64}),
 json!({"preset":"onFail"})
],"JSON {path} → {saveAs}","Dot-separated paths may include numeric array indexes. The extracted value remains a typed string, number, boolean, array, object, or null.")
}
#[async_trait]
impl Block for JsonExtract{
 fn title(&self)-> &'static str{"Extract JSON Value"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(json_extract_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&json_extract_spec(),c)?;nul(c,&["sourceVariable","jsonText","path","saveAs"])?;if !raw(c,"sourceVariable").is_empty()&&!valid_name(&text(c,"sourceVariable")){return Err("Source variable name is invalid".into())}if !valid_name(&text(c,"saveAs")){return Err("Output variable name is invalid".into())}Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let source=raw(&cx.cfg,"sourceVariable");let value=if source.is_empty(){let raw=cx.expand_template(&raw(&cx.cfg,"jsonText"))?;serde_json::from_str::<Value>(&raw).map_err(|e|format!("JSON input is invalid: {e}"))?}else{cx.get_variable(&source).ok_or_else(||format!("Source variable {source} is not set"))?};
  let mut cur=&value;for part in text(&cx.cfg,"path").split('.') {cur=if let Ok(i)=part.parse::<usize>(){cur.get(i)}else{cur.get(part)}.ok_or_else(||format!("JSON property path not found: {}",text(&cx.cfg,"path")))?;}
  cx.set_variable(text(&cx.cfg,"saveAs"),cur.clone())?;Ok(Outcome::Satisfied)
 }
}

struct RegexExtract;
fn regex_spec()->Value{action("Regex Extract / Replace","regex","Captures a .NET regular-expression group or replaces matching text; expression execution is time-limited.",vec![
 json!({"key":"mode","label":"Operation","type":"select","default":"extract","options":[["extract","Extract first match"],["replace","Replace matches"]]}),
 json!({"key":"input","label":"Input text","type":"textarea","rows":4,"required":true,"maxLen":65536}),
 json!({"key":"pattern","label":"Regular expression","type":"textarea","rows":3,"required":true,"maxLen":4096}),
 json!({"key":"group","label":"Capture group number","type":"number","min":0,"max":99,"step":1,"default":1,"show":{"key":"mode","eq":"extract"}}),
 json!({"key":"replacement","label":"Replacement text","type":"text","maxLen":4096,"show":{"key":"mode","eq":"replace"}}),
 json!({"key":"saveAs","label":"Save result as variable","type":"text","required":true,"maxLen":64}),
 json!({"preset":"onFail"})
],"Regex {mode} → {saveAs}","Uses .NET regex syntax and enforces a short regex timeout to reduce catastrophic backtracking. Output is saved as a string.")
}
#[async_trait]
impl Block for RegexExtract{
 fn title(&self)-> &'static str{"Regex Extract / Replace"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{false}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(regex_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&regex_spec(),c)?;nul(c,&["input","pattern","replacement","saveAs"])?;if raw(c,"pattern").is_empty(){return Err("Regular expression is required".into())}if !valid_name(&text(c,"saveAs")){return Err("Output variable name is invalid".into())}num(c,"group",1.0,0.0,99.0)?;Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let input=cx.expand_template(&raw(&cx.cfg,"input"))?;let pattern=raw(&cx.cfg,"pattern");let mode=text(&cx.cfg,"mode");let replacement=cx.expand_template(&raw(&cx.cfg,"replacement"))?;let group=num(&cx.cfg,"group",1.0,0.0,99.0)? as usize;
  let script=format!(r#"$ErrorActionPreference='Stop';$inputText={};$pattern={};$replacement={};$mode={};$group={};$rx=[regex]::new($pattern,[System.Text.RegularExpressions.RegexOptions]::CultureInvariant,[TimeSpan]::FromMilliseconds(250));if($mode -eq 'replace'){{$out=$rx.Replace($inputText,$replacement)}}else{{$m=$rx.Match($inputText);if(-not $m.Success){{throw 'No regex match found'}};if($group -ge $m.Groups.Count){{throw 'Capture group index does not exist'}};$out=$m.Groups[$group].Value}};[Console]::Out.Write($out)"#,q(&input),q(&pattern),q(&replacement),q(&mode),group);
  let out=extra::powershell(cx,&script).await?;cx.set_variable(text(&cx.cfg,"saveAs"),Value::String(out))?;Ok(Outcome::Satisfied)
 }
}

struct TextTransform;
fn transform_spec()->Value{action("Transform Text / Value","variable","Performs common string transforms and safe arithmetic before saving the result to a variable.",vec![
 json!({"key":"operation","label":"Operation","type":"select","default":"trim","options":[["trim","Trim whitespace"],["upper","Uppercase"],["lower","Lowercase"],["replace","Replace literal text"],["split","Split into JSON array"],["number_add","Add to number"],["number_multiply","Multiply number"]]}),
 json!({"key":"input","label":"Input text / number","type":"textarea","rows":3,"required":true,"maxLen":65536}),
 json!({"key":"find","label":"Find text","type":"text","maxLen":2048,"show":{"key":"operation","eq":"replace"}}),
 json!({"key":"replacement","label":"Replace with","type":"text","maxLen":2048,"show":{"key":"operation","eq":"replace"}}),
 json!({"key":"separator","label":"Split separator","type":"text","default":",","maxLen":128,"show":{"key":"operation","eq":"split"}}),
 json!({"key":"operand","label":"Number operand","type":"number","min":-1000000000000,"max":1000000000000,"step":0.1,"default":0,"show":{"key":"operation","in":["number_add","number_multiply"]}}),
 json!({"key":"saveAs","label":"Save result as variable","type":"text","required":true,"maxLen":64}),
 json!({"preset":"onFail"})
],"Transform {operation} → {saveAs}","Split outputs a JSON array. Numeric transformations reject nonnumeric and non-finite inputs.")
}
#[async_trait]
impl Block for TextTransform{
 fn title(&self)-> &'static str{"Transform Text / Value"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(transform_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&transform_spec(),c)?;nul(c,&["input","find","replacement","separator","saveAs"])?;if !valid_name(&text(c,"saveAs")){return Err("Output variable name is invalid".into())}if text(c,"operation")=="replace"&&raw(c,"find").is_empty(){return Err("Find text is required".into())}if text(c,"operation")=="split"&&raw(c,"separator").is_empty(){return Err("Split separator is required".into())}Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let input=cx.expand_template(&raw(&cx.cfg,"input"))?;let op=text(&cx.cfg,"operation");
  let result=match op.as_str(){
   "trim"=>Value::String(input.trim().to_string()),"upper"=>Value::String(input.to_uppercase()),"lower"=>Value::String(input.to_lowercase()),
   "replace"=>Value::String(input.replace(&raw(&cx.cfg,"find"),&cx.expand_template(&raw(&cx.cfg,"replacement"))?)),
   "split"=>Value::Array(input.split(&raw(&cx.cfg,"separator")).map(|x|Value::String(x.to_string())).collect()),
   "number_add"|"number_multiply"=>{let n=input.trim().parse::<f64>().map_err(|_|"Input is not a number")?;let operand=num(&cx.cfg,"operand",0.0,-1e12,1e12)?;let v=if op=="number_add"{n+operand}else{n*operand};if !v.is_finite(){return Err("Numeric result is not finite".into())}json!(v)},
   _=>return Err("Unknown text/value transformation".into())
  };
  cx.set_variable(text(&cx.cfg,"saveAs"),result)?;Ok(Outcome::Satisfied)
 }
}

struct CsvRead;
fn csv_read_spec()->Value{action("Read CSV File","file-spreadsheet","Reads a bounded UTF-8 CSV file and stores rows as an array of arrays.",vec![
 json!({"key":"path","label":"CSV file path","type":"text","required":true,"maxLen":2048}),
 json!({"key":"saveAs","label":"Save rows as variable","type":"text","required":true,"maxLen":64}),
 json!({"key":"maxRows","label":"Maximum rows","type":"number","min":1,"max":100000,"step":1,"default":10000}),
 json!({"preset":"onFail"})
],"Read CSV → {saveAs}","CSV parsing supports quoted fields, escaped double quotes, CRLF/LF line endings, and commas/newlines inside quoted cells. The file is capped at 16 MiB.")
}
#[async_trait]
impl Block for CsvRead{
 fn title(&self)-> &'static str{"Read CSV File"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(csv_read_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_read_spec(),c)?;nul(c,&["path","saveAs"])?;if !valid_name(&text(c,"saveAs")){return Err("Output variable name is invalid".into())}num(c,"maxRows",10000.0,1.0,100000.0)?;Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let path=cx.expand_template(&text(&cx.cfg,"path"))?;let meta=fs::metadata(&path).map_err(|e|format!("CSV file: {e}"))?;if meta.len()>16*1024*1024{return Err("CSV file exceeds 16 MiB".into())}
  let bytes=fs::read(&path).map_err(|e|format!("CSV file: {e}"))?;let source=String::from_utf8(bytes).map_err(|_|"CSV must be UTF-8")?;let rows=parse_csv(&source)?;let limit=num(&cx.cfg,"maxRows",10000.0,1.0,100000.0)? as usize;if rows.len()>limit{return Err(format!("CSV row count {} exceeds configured limit {limit}",rows.len()))}
  cx.set_variable(text(&cx.cfg,"saveAs"),json!(rows))?;Ok(Outcome::Satisfied)
 }
}
fn parse_csv(input:&str)->Result<Vec<Vec<String>>,String>{
 let bytes=input.as_bytes();let mut rows=Vec::new();let mut row=Vec::new();let mut cell=String::new();let mut i=0;let mut quoted=false;let mut closed=false;
 while i<bytes.len(){let b=bytes[i];if quoted{if b==b'"'{if i+1<bytes.len()&&bytes[i+1]==b'"'{cell.push('"');i+=2;continue}else{quoted=false;closed=true;i+=1;continue}}let ch=input[i..].chars().next().ok_or("Invalid CSV encoding")?;cell.push(ch);i+=ch.len_utf8();continue}
  if closed&&b!=b','&&b!=b'\r'&&b!=b'\n'&&b!=b' '&&b!=b'\t'{return Err("Unexpected characters after a quoted CSV field".into())}
  match b{b'"' if cell.is_empty()&&!closed=>{quoted=true;i+=1},b','=>{row.push(std::mem::take(&mut cell));closed=false;i+=1},b'\r'|b'\n'=>{row.push(std::mem::take(&mut cell));rows.push(std::mem::take(&mut row));closed=false;if b==b'\r'&&i+1<bytes.len()&&bytes[i+1]==b'\n'{i+=2}else{i+=1}},_=>{let ch=input[i..].chars().next().ok_or("Invalid CSV encoding")?;cell.push(ch);i+=ch.len_utf8()}}
 }
 if quoted{return Err("CSV contains an unclosed quoted field".into())}
 if !cell.is_empty()||!row.is_empty()||closed{row.push(cell);rows.push(row)}
 Ok(rows)
}
fn quote_csv_cell(cell:&str)->String{if cell.contains([',','"','\r','\n']){format!("\"{}\"",cell.replace('"',"\"\""))}else{cell.to_string()}}
fn rows_from_value(v:Value)->Result<Vec<Vec<String>>,String>{
 let rows=v.as_array().ok_or("CSV rows variable must be an array")?;if rows.len()>100000{return Err("CSV rows variable exceeds 100,000 rows".into())}
 rows.iter().map(|row|{let cells=row.as_array().ok_or("Each CSV row must be an array")?;if cells.len()>1000{return Err("CSV row has too many columns".into())}Ok(cells.iter().map(|v|match v{Value::String(s)=>s.clone(),Value::Null=>String::new(),other=>other.to_string()}).collect())}).collect()
}
struct CsvFilter;
fn csv_filter_spec()->Value{action("Filter CSV Rows","filter","Filters an in-memory CSV row array by zero-based column and literal comparison.",vec![
 json!({"key":"sourceVariable","label":"Rows variable","type":"text","required":true,"maxLen":64}),
 json!({"key":"column","label":"Column index (zero-based)","type":"number","min":0,"max":999,"step":1,"default":0}),
 json!({"key":"operation","label":"Comparison","type":"select","default":"equals","options":[["equals","Equals"],["not_equals","Does not equal"],["contains","Contains"],["starts_with","Starts with"]]}),
 json!({"key":"value","label":"Compare with","type":"text","required":true,"maxLen":4096}),
 json!({"key":"saveAs","label":"Save filtered rows as variable","type":"text","required":true,"maxLen":64}),
 json!({"preset":"onFail"})
],"Filter rows → {saveAs}","Filter is case-insensitive for string comparisons and preserves the original row order.")
}
#[async_trait]
impl Block for CsvFilter{
 fn title(&self)-> &'static str{"Filter CSV Rows"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(csv_filter_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_filter_spec(),c)?;nul(c,&["sourceVariable","value","saveAs"])?;if !valid_name(&text(c,"sourceVariable"))||!valid_name(&text(c,"saveAs")){return Err("Variable name is invalid".into())}num(c,"column",0.0,0.0,999.0)?;Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let src=text(&cx.cfg,"sourceVariable");let v=cx.get_variable(&src).ok_or_else(||format!("Variable {src} is not set"))?;let rows=rows_from_value(v)?;let col=num(&cx.cfg,"column",0.0,0.0,999.0)? as usize;let op=text(&cx.cfg,"operation");let value=cx.expand_template(&raw(&cx.cfg,"value"))?.to_lowercase();
  let filtered:Vec<Vec<String>>=rows.into_iter().filter(|row|row.get(col).map(|x|{let x=x.to_lowercase();match op.as_str(){"equals"=>x==value,"not_equals"=>x!=value,"contains"=>x.contains(&value),"starts_with"=>x.starts_with(&value),_=>false}}).unwrap_or(false)).collect();
  cx.set_variable(text(&cx.cfg,"saveAs"),json!(filtered))?;Ok(Outcome::Satisfied)
 }
}
struct CsvWrite;
fn csv_write_spec()->Value{action("Write CSV File","file-pen","Serializes a row array variable to a CSV file using a temporary file before replacing the destination.",vec![
 json!({"key":"sourceVariable","label":"Rows variable","type":"text","required":true,"maxLen":64}),
 json!({"key":"path","label":"Destination CSV path","type":"text","required":true,"maxLen":2048}),
 json!({"key":"overwrite","label":"If destination exists","type":"select","default":"replace","options":[["replace","Replace destination"],["fail","Fail if it exists"]]}),
 json!({"preset":"onFail"})
],"Write CSV → {path}","The action validates the rows array and writes through a temporary file. Existing destination behavior is explicit.")
}
#[async_trait]
impl Block for CsvWrite{
 fn title(&self)-> &'static str{"Write CSV File"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(csv_write_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_write_spec(),c)?;nul(c,&["sourceVariable","path"])?;if !valid_name(&text(c,"sourceVariable")){return Err("Source variable name is invalid".into())}Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let v=cx.get_variable(&text(&cx.cfg,"sourceVariable")).ok_or_else(||"CSV rows variable is not set".to_string())?;let rows=rows_from_value(v)?;
  let path=cx.expand_template(&text(&cx.cfg,"path"))?;if Path::new(&path).exists()&&text(&cx.cfg,"overwrite")=="fail"{return Err("CSV destination already exists".into())}
  let mut csv=String::new();for row in rows{csv.push_str(&row.iter().map(|x|quote_csv_cell(x)).collect::<Vec<_>>().join(","));csv.push_str("\r\n");}
  let dest=Path::new(&path);let parent=dest.parent().filter(|x|!x.as_os_str().is_empty()).unwrap_or(Path::new("."));fs::create_dir_all(parent).map_err(|e|e.to_string())?;
  let tmp=dest.with_extension(format!("00047-{}.tmp",std::process::id()));fs::write(&tmp,csv.as_bytes()).map_err(|e|e.to_string())?;
  if text(&cx.cfg,"overwrite")=="replace"&&dest.exists(){fs::remove_file(dest).map_err(|e|{let _=fs::remove_file(&tmp);e.to_string()})?}
  fs::rename(&tmp,dest).map_err(|e|{let _=fs::remove_file(&tmp);format!("Could not install CSV file: {e}")})?;Ok(Outcome::Satisfied)
 }
}

struct ForEach;
fn foreach_spec()->Value{action("For Each Item","repeat","Iterates an array from a workflow variable. Connect IF to the loop body and wire the body's final block back into this block; AFTER exits when all items are processed.",vec![
 json!({"key":"sourceVariable","label":"Array variable","type":"text","required":true,"maxLen":64}),
 json!({"key":"itemVariable","label":"Current item variable","type":"text","default":"item","required":true,"maxLen":64}),
 json!({"key":"indexVariable","label":"Zero-based index variable","type":"text","default":"index","required":true,"maxLen":64}),
 json!({"preset":"onFail"})
],"For each {sourceVariable}","The engine advances one item each time the loop-back wire returns here. An empty array routes directly to AFTER. Avoid connecting multiple concurrent IF branches as the loop body.")
}
#[async_trait]
impl Block for ForEach{
 fn title(&self)-> &'static str{"For Each Item"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(foreach_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&foreach_spec(),c)?;nul(c,&["sourceVariable","itemVariable","indexVariable"])?;for k in ["sourceVariable","itemVariable","indexVariable"]{if !valid_name(&text(c,k)){return Err(format!("{k} is not a valid variable name"))}}if text(c,"itemVariable")==text(c,"indexVariable"){return Err("Item and index variables must be different".into())}Ok(())}
 async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Err("For Each Item is a control-flow block and must be dispatched by the workflow engine".into())}
}
struct Switch;
fn switch_spec()->Value{condition("Switch / Case","layers","Routes IF when a variable meets the selected comparison and ELSE otherwise. Add separate comparisons as chained Switch blocks for more than two branches.",vec![
 json!({"key":"sourceVariable","label":"Variable to compare","type":"text","required":true,"maxLen":64}),
 json!({"key":"operation","label":"Comparison","type":"select","default":"equals","options":[["equals","Equals"],["not_equals","Does not equal"],["contains","Contains"],["starts_with","Starts with"],["is_empty","Is empty"],["greater","Greater than (number)"],["less","Less than (number)"]]}),
 json!({"key":"compareTo","label":"Compare to","type":"text","maxLen":4096,"show":{"key":"operation","ne":"is_empty"}}),
 json!({"preset":"onFail"})
],"Switch {sourceVariable}","This is a two-route decision block; connect IF for a match and ELSE for the fallback case.")
}
#[async_trait]
impl Block for Switch{
 fn title(&self)-> &'static str{"Switch / Case"}fn category(&self)->Cat{Cat::Condition}fn spec(&self)->Option<Value>{Some(switch_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&switch_spec(),c)?;nul(c,&["sourceVariable","compareTo"])?;if !valid_name(&text(c,"sourceVariable")){return Err("Source variable name is invalid".into())}Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;let src=text(&cx.cfg,"sourceVariable");let value=cx.get_variable(&src).ok_or_else(||format!("Variable {src} is not set"))?;let s=match &value{Value::String(s)=>s.clone(),v=>v.to_string()};let op=text(&cx.cfg,"operation");let cmp=cx.expand_template(&raw(&cx.cfg,"compareTo"))?;let yes=match op.as_str(){"equals"=>s.eq_ignore_ascii_case(&cmp),"not_equals"=>!s.eq_ignore_ascii_case(&cmp),"contains"=>s.to_lowercase().contains(&cmp.to_lowercase()),"starts_with"=>s.to_lowercase().starts_with(&cmp.to_lowercase()),"is_empty"=>s.trim().is_empty(),"greater"|"less"=>{let a=s.trim().parse::<f64>().map_err(|_|"Source variable is not numeric")?;let b=cmp.trim().parse::<f64>().map_err(|_|"Comparison value is not numeric")?;if op=="greater"{a>b}else{a<b}},_=>false};Ok(if yes{Outcome::Satisfied}else{Outcome::NotSatisfied})
 }
}

struct CallSubflow;
fn call_spec()->Value{action("Call Subflow","workflow","Runs another saved automation as a child flow with the current run's variables shared in scope. Use Return Value in the called automation to supply an optional result.",vec![
 json!({"key":"automationId","label":"Automation ID","type":"text","required":true,"maxLen":128}),
 json!({"key":"returnAs","label":"Save return value to variable (optional)","type":"text","maxLen":64}),
 json!({"preset":"timeout","default":0}),json!({"preset":"onFail"})
],"Call subflow {automationId}","Choose the target automation's ID from the Automations list. Recursive calls are blocked; each call gets its own node runtime state while sharing the parent run's variable store.")
}
#[async_trait]
impl Block for CallSubflow{
 fn title(&self)-> &'static str{"Call Subflow"}fn category(&self)->Cat{Cat::Action}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(call_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&call_spec(),c)?;nul(c,&["automationId","returnAs"])?;if text(c,"automationId").trim().is_empty(){return Err("Automation ID is required".into())}if !raw(c,"returnAs").is_empty()&&!valid_name(&text(c,"returnAs")){return Err("Return variable name is invalid".into())}Ok(())}
 async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Err("Call Subflow is a control-flow block and must be dispatched by the workflow engine".into())}
}
struct ReturnValue;
fn return_spec()->Value{action("Return Value","corner-up-left","Returns from the current subflow and makes a typed value available to the calling block.",vec![
 json!({"key":"value","label":"Return value","type":"textarea","rows":3,"maxLen":16384}),
 json!({"preset":"onFail"})
],"Return value","Store structured JSON text as a typed value when it parses as JSON; otherwise return the configured text as a string.")
}
#[async_trait]
impl Block for ReturnValue{
 fn title(&self)-> &'static str{"Return Value"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(return_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&return_spec(),c)?;nul(c,&["value"])}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let s=cx.expand_template(&raw(&cx.cfg,"value"))?;let v=serde_json::from_str(&s).unwrap_or(Value::String(s));cx.set_variable("subflow.return".into(),v)?;Ok(Outcome::Satisfied)}
}

struct GetSecret;
fn secret_spec()->Value{action("Get Secret","lock","Retrieves a named user-protected secret into a workflow variable. Secret values are not exposed to the editor or logs.",vec![
 json!({"key":"name","label":"Secret name","type":"text","required":true,"maxLen":128}),
 json!({"key":"saveAs","label":"Save to variable","type":"text","required":true,"maxLen":64}),
 json!({"preset":"onFail"})
],"Get secret {name}","Create or update secrets in Settings → Secrets. Secret values are encrypted with Windows DPAPI for the current user; exported automations contain only the secret name.")
}
#[async_trait]
impl Block for GetSecret{
 fn title(&self)-> &'static str{"Get Secret"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(secret_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&secret_spec(),c)?;nul(c,&["name","saveAs"])?;if text(c,"name").trim().is_empty()||!valid_name(&text(c,"saveAs")){return Err("Secret name and output variable are required".into())}Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let secret=cx.read_secret(&text(&cx.cfg,"name"))?;cx.set_variable(text(&cx.cfg,"saveAs"),Value::String(secret))?;Ok(Outcome::Satisfied)}
}

pub(super) fn register_into(m:&mut std::collections::HashMap<&'static str,Box<dyn Block>>){
 m.insert("cond.ui_element",Box::new(UiElementCondition));
 m.insert("act.ui_element",Box::new(UiElementAction));
 m.insert("cond.webhook_trigger",Box::new(WebhookTrigger));
 m.insert("cond.hotkey_trigger",Box::new(HotkeyTrigger));
 m.insert("act.json_extract",Box::new(JsonExtract));
 m.insert("act.regex",Box::new(RegexExtract));
 m.insert("act.transform",Box::new(TextTransform));
 m.insert("act.csv_read",Box::new(CsvRead));
 m.insert("act.csv_filter",Box::new(CsvFilter));
 m.insert("act.csv_write",Box::new(CsvWrite));
 m.insert("act.for_each",Box::new(ForEach));
 m.insert("cond.switch",Box::new(Switch));
 m.insert("act.call_subflow",Box::new(CallSubflow));
 m.insert("act.return_value",Box::new(ReturnValue));
 m.insert("act.get_secret",Box::new(GetSecret));
}

#[cfg(test)]
mod tests{
 use super::*;
 #[test]fn csv_parser_handles_quotes_newlines_and_empty_fields(){
  assert_eq!(parse_csv("id,name,note\r\n1,\"Doe, Jane\",\"said \"\"hi\"\"\"\r\n2,Bob,\r\n").unwrap(),vec![vec!["id","name","note"],vec!["1","Doe, Jane","said \"hi\""],vec!["2","Bob",""]]);
  assert_eq!(parse_csv("\"line1\nline2\",x").unwrap(),vec![vec!["line1\nline2","x"]]);
  assert!(parse_csv("\"unclosed").is_err());
 }
 #[test]fn csv_roundtrip_quotes_commas_newlines(){
  let rows=vec![vec!["a,b".to_string(),"say \"hi\"".to_string(),"two\nlines".to_string()]];
  let csv=rows.iter().map(|row|row.iter().map(|x|quote_csv_cell(x)).collect::<Vec<_>>().join(",")).collect::<Vec<_>>().join("\r\n")+"\r\n";
  assert_eq!(parse_csv(&csv).unwrap(),rows);
 }
 #[test]fn utility_specs_are_registered_and_validate(){
  assert!(ui_condition_spec().get("fields").unwrap().as_array().unwrap().iter().any(|f|f.get("key")==Some(&json!("behavior"))));
  assert!(ForEach.validate(&json!({"sourceVariable":"rows","itemVariable":"item","indexVariable":"index","onFail":"stop"})).is_ok());
  assert!(HotkeyTrigger.validate(&json!({"key":"F8","ctrl":true,"alt":true,"shift":false,"win":false,"timeoutSec":0,"onFail":"stop"})).is_ok());
  assert!(WebhookTrigger.validate(&json!({"port":47847,"path":"/hook","token":"","saveBodyAs":"body","timeoutSec":0,"onFail":"stop"})).is_ok());
  assert!(WebhookTrigger.validate(&json!({"port":80,"path":"hook","token":"","saveBodyAs":"body","timeoutSec":0,"onFail":"stop"})).is_err());
  assert!(JsonExtract.validate(&json!({"sourceVariable":"","jsonText":"{}","path":"x","saveAs":"value","onFail":"stop"})).is_ok());
 }
}
