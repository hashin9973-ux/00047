//! UI Automation, structured data, CSV, event, secrets and reusable workflow blocks.
use super::{extra, spec, Block, Cat, NodeCx, Outcome};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::{fs, path::Path, time::Duration};

fn txt(c: &Value, k: &str) -> String { c.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string() }
fn raw(c: &Value, k: &str) -> String { c.get(k).and_then(Value::as_str).unwrap_or("").to_string() }
fn no_nul(c: &Value, keys: &[&str]) -> Result<(), String> {
    for key in keys { if raw(c, key).contains('\0') { return Err(format!("{key} must not contain NUL characters")); } }
    Ok(())
}
fn valid_var(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64
        && s.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && s.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}
fn require_var(c: &Value, key: &str) -> Result<(), String> {
    let name = txt(c, key);
    if valid_var(&name) { Ok(()) } else { Err(format!("{key} must be a valid variable name")) }
}
fn action_spec(title: &str, icon: &str, description: &str, fields: Vec<Value>, summary: &str, help: &str) -> Value {
    json!({"category":"action","title":title,"icon":icon,"description":description,"fields":fields,"summary":summary,"help":help})
}
fn condition_spec(title: &str, icon: &str, description: &str, mut fields: Vec<Value>, summary: &str, help: &str) -> Value {
    fields.extend(extra::behavior_fields());
    json!({"category":"condition","title":title,"icon":icon,"description":description,"fields":fields,"summary":summary,"help":help})
}
fn condition_ok(v: bool) -> Outcome { if v { Outcome::Satisfied } else { Outcome::NotSatisfied } }

// ---------------- Native Windows UI Automation ----------------
fn ui_fields() -> Vec<Value> {
    vec![
        json!({"key":"windowTitle","label":"Window title contains","type":"text","default":"","maxLen":256}),
        json!({"key":"elementName","label":"Control name contains","type":"text","default":"","maxLen":256}),
        json!({"key":"automationId","label":"Automation ID (optional)","type":"text","default":"","maxLen":256}),
        json!({"key":"controlType","label":"Control type","type":"select","default":"","options":[["","Any control"],["Button","Button"],["Edit","Text field"],["Text","Text label"],["CheckBox","Checkbox"],["ComboBox","Combo box"],["ListItem","List item"],["MenuItem","Menu item"],["TabItem","Tab"],["Hyperlink","Link"],["Window","Window"]]}),
    ]
}
fn ui_condition_spec() -> Value {
    let mut f = vec![json!({"key":"operation","label":"Satisfied when","type":"select","default":"exists","options":[["exists","Control exists"],["enabled","Control is enabled"],["visible","Control is not off-screen"],["name_contains","Name contains expected text"],["value_equals","Value equals expected text"]]})];
    f.extend(ui_fields());
    f.push(json!({"key":"expectedValue","label":"Expected name/value","type":"text","default":"","maxLen":2048,"show":{"key":"operation","in":["name_contains","value_equals"]}}));
    condition_spec("UI Element State", "app-window", "Finds a Windows UI Automation control by its containing window and control properties. Does not rely on screen coordinates.", f, "UI element {operation}", "Uses the Windows UI Automation tree. Some applications (including certain elevated, game, and custom-rendered apps) may not expose accessible controls.")
}
fn ui_action_spec(title: &str, icon: &str, action: &str) -> Value {
    let mut f = ui_fields();
    f.push(json!({"key":"timeoutSec","label":"Find timeout","type":"number","min":0,"max":120,"step":1,"default":10,"unit":"s"}));
    if action == "set" { f.push(json!({"key":"value","label":"Value to set","type":"textarea","rows":2,"required":true,"maxLen":4000})); }
    if action == "get" { f.push(json!({"key":"saveAs","label":"Store value in variable","type":"text","required":true,"maxLen":64})); }
    action_spec(title, icon, "Uses Windows UI Automation patterns instead of coordinate-based input when the target application exposes accessible controls.", f, title, "UI Automation is subject to Windows integrity levels and application accessibility support. It cannot reliably interact with every custom-rendered surface.")
}
fn ui_script(c: &Value, action: &str) -> Result<String, String> {
    let wt = extra::ps_quote(&c.get("__window").and_then(Value::as_str).unwrap_or("").to_string());
    let name = extra::ps_quote(&c.get("__name").and_then(Value::as_str).unwrap_or("").to_string());
    let aid = extra::ps_quote(&c.get("__automationId").and_then(Value::as_str).unwrap_or("").to_string());
    let ty = extra::ps_quote(&c.get("__controlType").and_then(Value::as_str).unwrap_or("").to_string());
    let val = extra::ps_quote(&c.get("__value").and_then(Value::as_str).unwrap_or("").to_string());
    let code = r#"
$ErrorActionPreference='Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$wt=__WINDOW__; $needle=__NAME__; $aid=__AID__; $type=__TYPE__; $setValue=__VALUE__
$root=[System.Windows.Automation.AutomationElement]::RootElement
$container=$root
if($wt.Length -gt 0) {
  $windows=$root.FindAll([System.Windows.Automation.TreeScope]::Children,[System.Windows.Automation.Condition]::TrueCondition)
  $container=$null
  foreach($w in $windows) { try { if($w.Current.Name.IndexOf($wt,[StringComparison]::OrdinalIgnoreCase) -ge 0) {$container=$w; break} } catch {} }
  if($null -eq $container) { [pscustomobject]@{found=$false;name='';enabled=$false;visible=$false;value=''}|ConvertTo-Json -Compress; exit 0 }
}
$items=$container.FindAll([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.Condition]::TrueCondition)
$found=$null
foreach($el in $items) {
  try {
    if($needle.Length -gt 0 -and $el.Current.Name.IndexOf($needle,[StringComparison]::OrdinalIgnoreCase) -lt 0) {continue}
    if($aid.Length -gt 0 -and $el.Current.AutomationId -ne $aid) {continue}
    if($type.Length -gt 0 -and ($el.Current.ControlType.ProgrammaticName -replace '^ControlType\.','') -ne $type) {continue}
    $found=$el; break
  } catch {}
}
if($null -eq $found) { [pscustomobject]@{found=$false;name='';enabled=$false;visible=$false;value=''}|ConvertTo-Json -Compress; exit 0 }
if('__ACTION__' -eq 'click') {
  if(-not $found.Current.IsEnabled) {throw 'UI element is disabled'}
  $pattern=$found.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
  $pattern.Invoke()
}
elseif('__ACTION__' -eq 'set') {
  if(-not $found.Current.IsEnabled) {throw 'UI element is disabled'}
  $pattern=$found.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
  $pattern.SetValue($setValue)
}
$value=''
try {$pattern=$found.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern);$value=[string]$pattern.Current.Value} catch {}
[pscustomobject]@{found=$true;name=[string]$found.Current.Name;enabled=[bool]$found.Current.IsEnabled;visible=(-not [bool]$found.Current.IsOffscreen);value=$value}|ConvertTo-Json -Compress
"#;
    Ok(code.replace("__WINDOW__", &wt).replace("__NAME__", &name).replace("__AID__", &aid)
       .replace("__TYPE__", &ty).replace("__VALUE__", &val).replace("__ACTION__", action))
}
fn ui_cfg(cx: &NodeCx) -> Result<Value, String> {
    let mut c = cx.cfg.clone();
    for key in ["windowTitle","elementName","automationId","controlType","value"] {
        if let Some(v)=c.get(key).and_then(Value::as_str) { c[key]=Value::String(cx.expand_template(v)?); }
    }
    c["__window"]=json!(txt(&c,"windowTitle"));
    c["__name"]=json!(txt(&c,"elementName"));
    c["__automationId"]=json!(txt(&c,"automationId"));
    c["__controlType"]=json!(txt(&c,"controlType"));
    c["__value"]=json!(raw(&c,"value"));
    Ok(c)
}
#[derive(Default)]
struct UiSnapshot { found: bool, enabled: bool, visible: bool, name: String, value: String }
async fn ui_probe(cx: &NodeCx, c: &Value, action: &str) -> Result<UiSnapshot,String> {
    let script=ui_script(c,action)?;
    let out=extra::powershell(cx,&script).await?;
    let v:Value=serde_json::from_str(&out).map_err(|e|format!("Could not parse UI Automation result: {e}"))?;
    Ok(UiSnapshot{found:v["found"].as_bool().unwrap_or(false),enabled:v["enabled"].as_bool().unwrap_or(false),visible:v["visible"].as_bool().unwrap_or(false),name:v["name"].as_str().unwrap_or("").into(),value:v["value"].as_str().unwrap_or("").into()})
}
struct UiElementState;
#[async_trait]
impl Block for UiElementState {
 fn title(&self)->&'static str{"UI Element State"} fn category(&self)->Cat{Cat::Condition} fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(ui_condition_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&ui_condition_spec(),c)?;no_nul(c,&["windowTitle","elementName","automationId","expectedValue"])?;if txt(c,"windowTitle").is_empty()&&txt(c,"elementName").is_empty()&&txt(c,"automationId").is_empty(){return Err("Specify at least a window title, control name, or Automation ID".into())};if txt(c,"operation")=="value_equals"&&txt(c,"expectedValue").is_empty(){return Err("Expected value is required".into())};extra::wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let c=ui_cfg(cx)?;let op=txt(&cx.cfg,"operation");let expected=cx.expand_template(&raw(&cx.cfg,"expectedValue"))?;let(behavior,poll,stable,timeout)=extra::wait_settings(&cx.cfg)?;extra::poll_condition(cx,&behavior,poll,stable,timeout,||{let c=c.clone();let op=op.clone();let expected=expected.clone();Box::pin(async move{let s=ui_probe(cx,&c,"probe").await?;Ok(match op.as_str(){"enabled"=>s.found&&s.enabled,"visible"=>s.found&&s.visible,"name_contains"=>s.found&&s.name.to_lowercase().contains(&expected.to_lowercase()),"value_equals"=>s.found&&s.value==expected,_=>s.found})}) as extra::Probe<'_>}).await}
}
struct UiClick;
#[async_trait]
impl Block for UiClick {
 fn title(&self)->&'static str{"Click UI Element"} fn category(&self)->Cat{Cat::Action} fn supports_timeout(&self)->bool{true} fn risky(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(ui_action_spec("Click UI Element","mouse-pointer-2","click"))}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&ui_action_spec("Click UI Element","mouse-pointer-2","click"),c)?;no_nul(c,&["windowTitle","elementName","automationId"])?;if ["windowTitle","elementName","automationId"].iter().all(|k|txt(c,k).is_empty()){return Err("Specify at least one UI selector".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let c=ui_cfg(cx)?;let t=cx.cfg["timeoutSec"].as_f64().unwrap_or(10.0);let start=std::time::Instant::now();loop{match ui_probe(cx,&c,"click").await{Ok(s) if s.found=>{(cx.log)("info",format!("Invoked UI control '{}'",s.name));return Ok(Outcome::Satisfied)},Ok(_)=>{},Err(e) if e.contains("Could not find")=>{},Err(e)=>return Err(e)}if t<=0.0||start.elapsed().as_secs_f64()>=t{return Err("UI element was not found before timeout".into())}tokio::select!{_=tokio::time::sleep(Duration::from_millis(250))=>{},_=cx.token.cancelled()=>return Err("cancelled".into())}}}
}
struct UiSetValue;
#[async_trait]
impl Block for UiSetValue {
 fn title(&self)->&'static str{"Set UI Element Value"} fn category(&self)->Cat{Cat::Action} fn supports_timeout(&self)->bool{true} fn risky(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(ui_action_spec("Set UI Element Value","app-window","set"))}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&ui_action_spec("Set UI Element Value","app-window","set"),c)?;no_nul(c,&["windowTitle","elementName","automationId","value"])?;if ["windowTitle","elementName","automationId"].iter().all(|k|txt(c,k).is_empty()){return Err("Specify at least one UI selector".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let c=ui_cfg(cx)?;let t=cx.cfg["timeoutSec"].as_f64().unwrap_or(10.0);let start=std::time::Instant::now();loop{match ui_probe(cx,&c,"set").await{Ok(s) if s.found=>{(cx.log)("info",format!("Set value on UI control '{}'",s.name));return Ok(Outcome::Satisfied)},Ok(_)=>{},Err(e)=>return Err(e)}if t<=0.0||start.elapsed().as_secs_f64()>=t{return Err("UI element was not found before timeout".into())}tokio::select!{_=tokio::time::sleep(Duration::from_millis(250))=>{},_=cx.token.cancelled()=>return Err("cancelled".into())}}}
}
struct UiGetValue;
#[async_trait]
impl Block for UiGetValue {
 fn title(&self)->&'static str{"Read UI Element Value"} fn category(&self)->Cat{Cat::Action} fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(ui_action_spec("Read UI Element Value","app-window","get"))}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&ui_action_spec("Read UI Element Value","app-window","get"),c)?;no_nul(c,&["windowTitle","elementName","automationId","saveAs"])?;if ["windowTitle","elementName","automationId"].iter().all(|k|txt(c,k).is_empty()){return Err("Specify at least one UI selector".into())};require_var(c,"saveAs")}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let c=ui_cfg(cx)?;let s=ui_probe(cx,&c,"probe").await?;if !s.found{return Err("UI element was not found".into())};cx.set_variable(txt(&cx.cfg,"saveAs"),Value::String(if s.value.is_empty(){s.name}else{s.value}))?;Ok(Outcome::Satisfied)}
}

// ---------------- Structured data and CSV ----------------
fn csv_parse(input:&str)->Result<Vec<Vec<String>>,String>{
 let s=input.trim_start_matches('\u{feff}');let mut rows=Vec::new();let mut row=Vec::new();let mut field=String::new();let mut it=s.chars().peekable();let mut quoted=false;
 while let Some(ch)=it.next(){if quoted {if ch=='"' {if it.peek()==Some(&'"'){it.next();field.push('"')}else{quoted=false}}else{field.push(ch)}}else{match ch{','=>row.push(std::mem::take(&mut field)),'\r'=>{if it.peek()==Some(&'\n'){it.next();}row.push(std::mem::take(&mut field));if !(row.len()==1&&row[0].is_empty()){rows.push(std::mem::take(&mut row));}},'\n'=>{row.push(std::mem::take(&mut field));if !(row.len()==1&&row[0].is_empty()){rows.push(std::mem::take(&mut row));}},'"' if field.is_empty()=>quoted=true,'"'=>return Err("Unexpected quote inside unquoted CSV field".into()),_=>field.push(ch)}}}
 if quoted{return Err("CSV has an unclosed quoted field".into())}if !field.is_empty()||!row.is_empty(){row.push(field);rows.push(row)}if rows.len()>100_001{return Err("CSV exceeds 100,000 data rows".into())}Ok(rows)
}
fn csv_encode(s:&str)->String { if s.contains([',','"','\n','\r']){format!("\"{}\"",s.replace('"',"\"\""))}else{s.to_string()} }
fn value_string(v:&Value)->String { match v {Value::Null=>String::new(),Value::String(s)=>s.clone(),_=>v.to_string()} }
struct CsvRead;
fn csv_read_spec()->Value{action_spec("Read CSV into Variable","file-spreadsheet","Reads a bounded CSV file into an array of objects keyed by the header row.",vec![json!({"key":"path","label":"CSV path","type":"text","required":true,"maxLen":1000}),json!({"key":"saveAs","label":"Array variable","type":"text","required":true,"maxLen":64}),json!({"key":"maxBytes","label":"Maximum file size","type":"number","min":1,"max":10485760,"default":2097152,"unit":"bytes"}),json!({"preset":"onFail"})],"Read CSV → {saveAs}","CSV is parsed in-process. File size and row count are bounded to avoid loading unbounded inputs into memory.")}
#[async_trait] impl Block for CsvRead{
 fn title(&self)->&'static str{"Read CSV into Variable"} fn category(&self)->Cat{Cat::Action} fn spec(&self)->Option<Value>{Some(csv_read_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_read_spec(),c)?;no_nul(c,&["path","saveAs"])?;require_var(c,"saveAs")?;let p=txt(c,"path");if p.is_empty(){return Err("CSV path is required".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let path=cx.expand_template(&txt(&cx.cfg,"path"))?;let max=cx.cfg["maxBytes"].as_u64().unwrap_or(2097152).min(10*1024*1024) as usize;let bytes=fs::read(&path).map_err(|e|format!("Could not read CSV: {e}"))?;if bytes.len()>max{return Err(format!("CSV is larger than configured limit ({max} bytes)"))};let input=String::from_utf8(bytes).map_err(|_|"CSV must be UTF-8".to_string())?;let mut rows=csv_parse(&input)?;if rows.is_empty(){return Err("CSV is empty".into())};let headers=rows.remove(0);if headers.is_empty()||headers.iter().any(|h|h.trim().is_empty()){return Err("CSV header names must be nonempty".into())};let mut seen=std::collections::HashSet::new();if headers.iter().any(|h|!seen.insert(h.clone())){return Err("CSV header names must be unique".into())};let mut values=Vec::new();for row in rows{if row.len()!=headers.len(){return Err(format!("CSV row has {} cells but header has {}",row.len(),headers.len()))};let object=headers.iter().cloned().zip(row).map(|(k,v)|(k,Value::String(v))).collect::<Map<String,Value>>();values.push(Value::Object(object));}cx.set_variable(txt(&cx.cfg,"saveAs"),Value::Array(values.clone()))?;(cx.log)("info",format!("Loaded {} CSV rows into variable '{}'",values.len(),txt(&cx.cfg,"saveAs")));Ok(Outcome::Satisfied)}
}
struct CsvWrite;
fn csv_write_spec()->Value{action_spec("Write CSV from Variable","file-spreadsheet","Writes an array of JSON objects to CSV using first-row key order as headers.",vec![json!({"key":"sourceVariable","label":"Array variable","type":"text","required":true,"maxLen":64}),json!({"key":"path","label":"Output path","type":"text","required":true,"maxLen":1000}),json!({"key":"collision","label":"If file exists","type":"select","default":"fail","options":[["fail","Fail without overwriting"],["replace","Replace existing file"]]}),json!({"preset":"onFail"})],"Write CSV → {path}","Output is staged beside the target and renamed into place. Values are written as strings; nested JSON values are serialized compactly.")}
#[async_trait] impl Block for CsvWrite{
 fn title(&self)->&'static str{"Write CSV from Variable"} fn category(&self)->Cat{Cat::Action} fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(csv_write_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_write_spec(),c)?;require_var(c,"sourceVariable")?;no_nul(c,&["path"])?;if txt(c,"path").is_empty(){return Err("Output path is required".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let items=cx.get_variable(&txt(&cx.cfg,"sourceVariable")).ok_or("CSV source variable is not set")?;let arr=items.as_array().ok_or("CSV source variable must contain a JSON array")?;if arr.len()>100000{return Err("CSV output exceeds 100,000 rows".into())};let headers:Vec<String>=arr.first().and_then(Value::as_object).map(|m|m.keys().cloned().collect()).unwrap_or_default();if headers.is_empty()&&!arr.is_empty(){return Err("CSV rows must be JSON objects".into())};if arr.iter().any(|v|!v.is_object()){return Err("Every CSV row must be a JSON object".into())};let mut out=String::new();out.push_str(&headers.iter().map(|h|csv_encode(h)).collect::<Vec<_>>().join(","));out.push_str("\r\n");for item in arr{let object=item.as_object().unwrap();out.push_str(&headers.iter().map(|h|csv_encode(&value_string(&object.get(h).cloned().unwrap_or(Value::Null))).replace("\r","")).collect::<Vec<_>>().join(","));out.push_str("\r\n")};let path=cx.expand_template(&txt(&cx.cfg,"path"))?;let target=Path::new(&path);if target.exists()&&txt(&cx.cfg,"collision")!="replace"{return Err("CSV output already exists".into())};let parent=target.parent().filter(|p|!p.as_os_str().is_empty()).unwrap_or(Path::new("."));if !parent.is_dir(){return Err("CSV output directory does not exist".into())};let mut temp=target.to_path_buf();let ext=target.extension().and_then(|x|x.to_str()).unwrap_or("csv");temp.set_extension(format!("{ext}.00047-tmp"));fs::write(&temp,out).map_err(|e|format!("Could not stage CSV: {e}"))?;if target.exists(){let mut backup=target.to_path_buf();backup.set_extension(format!("{ext}.00047-backup"));let _=fs::remove_file(&backup);fs::rename(target,&backup).map_err(|e|format!("Could not back up existing CSV: {e}"))?;if let Err(e)=fs::rename(&temp,target){let _=fs::rename(&backup,target);let _=fs::remove_file(&temp);return Err(format!("Could not install CSV: {e}"))};let _=fs::remove_file(backup);}else{fs::rename(&temp,target).map_err(|e|{let _=fs::remove_file(&temp);format!("Could not install CSV: {e}")})?};Ok(Outcome::Satisfied)}
}
struct JsonParse;
fn json_parse_spec()->Value{action_spec("Parse JSON","file-json","Parses JSON text into a structured workflow variable, optionally selecting a nested property.",vec![json!({"key":"jsonText","label":"JSON text","type":"textarea","rows":4,"required":true,"maxLen":1048576}),json!({"key":"propertyPath","label":"Property path (optional)","type":"text","maxLen":512,"default":""}),json!({"key":"saveAs","label":"Output variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})],"Parse JSON → {saveAs}","Nested paths use dot-separated object properties or array indices.")}
#[async_trait] impl Block for JsonParse{
 fn title(&self)->&'static str{"Parse JSON"} fn category(&self)->Cat{Cat::Action} fn spec(&self)->Option<Value>{Some(json_parse_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&json_parse_spec(),c)?;require_var(c,"saveAs")?;no_nul(c,&["jsonText","propertyPath"])?;Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let s=cx.expand_template(&raw(&cx.cfg,"jsonText"))?;if s.len()>1048576{return Err("JSON input is too large".into())};let mut v:Value=serde_json::from_str(&s).map_err(|e|format!("Invalid JSON: {e}"))?;let path=txt(&cx.cfg,"propertyPath");if !path.is_empty(){for part in path.split('.') {v=if let Some(a)=v.as_array(){let idx=part.parse::<usize>().map_err(|_|format!("'{part}' is not an array index"))?;a.get(idx).cloned().ok_or_else(||format!("JSON array index {idx} does not exist"))?}else{v.get(part).cloned().ok_or_else(||format!("JSON property '{part}' does not exist"))?};}}cx.set_variable(txt(&cx.cfg,"saveAs"),v)?;Ok(Outcome::Satisfied)}
}
struct JsonGet;
fn json_get_spec()->Value{action_spec("Get JSON Property","file-json","Gets a property from a JSON variable and saves it in another variable.",vec![json!({"key":"sourceVariable","label":"Source JSON variable","type":"text","required":true,"maxLen":64}),json!({"key":"propertyPath","label":"Property path","type":"text","required":true,"maxLen":512}),json!({"key":"saveAs","label":"Output variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})],"Get {propertyPath} → {saveAs}","Use dot-separated property names and numeric array indexes.")}
#[async_trait] impl Block for JsonGet{
 fn title(&self)->&'static str{"Get JSON Property"} fn category(&self)->Cat{Cat::Action} fn spec(&self)->Option<Value>{Some(json_get_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&json_get_spec(),c)?;require_var(c,"sourceVariable")?;require_var(c,"saveAs")?;Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let mut v=cx.get_variable(&txt(&cx.cfg,"sourceVariable")).ok_or("Source variable is not set")?;for part in txt(&cx.cfg,"propertyPath").split('.') {v=if let Some(a)=v.as_array(){let n=part.parse::<usize>().map_err(|_|"Array path component must be a number")?;a.get(n).cloned().ok_or("JSON array index is out of range")?}else{v.get(part).cloned().ok_or_else(||format!("JSON property '{part}' does not exist"))?};};cx.set_variable(txt(&cx.cfg,"saveAs"),v)?;Ok(Outcome::Satisfied)}
}
struct Transform;
fn transform_spec()->Value{action_spec("Transform Value","variable","Performs a bounded, explicit string or numeric transformation and stores the result.",vec![json!({"key":"operation","label":"Operation","type":"select","default":"to_string","options":[["to_string","Convert to text"],["trim","Trim whitespace"],["upper","Uppercase"],["lower","Lowercase"],["length","Text / array length"],["to_number","Parse number"],["add","Add number"],["subtract","Subtract number"],["multiply","Multiply number"],["divide","Divide number"],["replace","Replace text"],["split","Split into array"]]}),json!({"key":"sourceVariable","label":"Source variable","type":"text","required":true,"maxLen":64}),json!({"key":"operand","label":"Operand / replacement","type":"text","maxLen":2048,"default":""}),json!({"key":"saveAs","label":"Output variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})],"Transform {sourceVariable} → {saveAs}","Arithmetic is numeric-only, division by zero is rejected, and no user expression is evaluated as code.")}
#[async_trait] impl Block for Transform{
 fn title(&self)->&'static str{"Transform Value"} fn category(&self)->Cat{Cat::Action} fn spec(&self)->Option<Value>{Some(transform_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&transform_spec(),c)?;require_var(c,"sourceVariable")?;require_var(c,"saveAs")?;no_nul(c,&["operand"])?;if !matches!(txt(c,"operation").as_str(),"to_string"|"trim"|"upper"|"lower"|"length"|"to_number"|"add"|"subtract"|"multiply"|"divide"|"replace"|"split"){return Err("Unknown transformation".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let v=cx.get_variable(&txt(&cx.cfg,"sourceVariable")).ok_or("Source variable is not set")?;let s=value_string(&v);let operand=cx.expand_template(&raw(&cx.cfg,"operand"))?;let result=match txt(&cx.cfg,"operation").as_str(){
 "to_string"=>Value::String(s),"trim"=>Value::String(s.trim().into()),"upper"=>Value::String(s.to_uppercase()),"lower"=>Value::String(s.to_lowercase()),"length"=>Value::Number((v.as_array().map(|x|x.len()).unwrap_or_else(||s.chars().count()) as u64).into()),
 "to_number"=>Value::from(s.trim().parse::<f64>().map_err(|_|"Source value is not numeric")?).as_number().cloned().map(Value::Number).ok_or("Number is not finite")?,
 "add"|"subtract"|"multiply"|"divide"=>{let n=v.as_f64().or_else(||s.trim().parse().ok()).ok_or("Source value is not numeric")?;let m=operand.trim().parse::<f64>().map_err(|_|"Operand must be numeric")?;let x=match txt(&cx.cfg,"operation").as_str(){"add"=>n+m,"subtract"=>n-m,"multiply"=>n*m,_=>{if m==0.0{return Err("Division by zero is not allowed".into())};n/m}};if !x.is_finite(){return Err("Numeric result is not finite".into())};json!(x)},
 "replace"=>{let (a,b)=operand.split_once("=>").ok_or("Replace operand must be search=>replacement")?;if a.is_empty(){return Err("Search text cannot be empty".into())};Value::String(s.replace(a,b))},
 "split"=>Value::Array(s.split(&operand).map(|x|Value::String(x.into())).collect()),
 _=>return Err("Unknown transformation".into())};cx.set_variable(txt(&cx.cfg,"saveAs"),result)?;Ok(Outcome::Satisfied)}
}

// Regex is delegated to .NET Regex through PowerShell so it uses the platform's regex engine.
struct RegexExtract;
fn regex_extract_spec()->Value{action_spec("Regex Extract","regex","Captures a regular expression group from input text into a workflow variable.",vec![json!({"key":"inputText","label":"Input text","type":"textarea","rows":4,"required":true,"maxLen":65536}),json!({"key":"pattern","label":"Regular expression","type":"text","required":true,"maxLen":2048}),json!({"key":"group","label":"Capture group (name or index)","type":"text","default":"1","maxLen":64}),json!({"key":"saveAs","label":"Output variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})],"Extract regex group → {saveAs}","The first match is used; no match follows the block's failure route.")}
#[async_trait] impl Block for RegexExtract{
 fn title(&self)->&'static str{"Regex Extract"} fn category(&self)->Cat{Cat::Action} fn spec(&self)->Option<Value>{Some(regex_extract_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&regex_extract_spec(),c)?;require_var(c,"saveAs")?;no_nul(c,&["inputText","pattern","group"])?;Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let input=cx.expand_template(&raw(&cx.cfg,"inputText"))?;let pattern=cx.expand_template(&raw(&cx.cfg,"pattern"))?;let group=txt(&cx.cfg,"group");let script=r#"$ErrorActionPreference='Stop';$inputText=__INPUT__;$pattern=__PATTERN__;$group=__GROUP__;try{$m=[regex]::Match($inputText,$pattern,[System.Text.RegularExpressions.RegexOptions]::None,[TimeSpan]::FromMilliseconds(500));if(-not $m.Success){throw 'No regular-expression match'};if($group -match '^\d+$'){$g=$m.Groups[[int]$group]}else{$g=$m.Groups[$group]};if(-not $g.Success){throw 'Capture group does not exist'};[Console]::Out.Write($g.Value)}catch{[Console]::Error.WriteLine($_.Exception.Message);exit 1}"#.replace("__INPUT__",&extra::ps_quote(&input)).replace("__PATTERN__",&extra::ps_quote(&pattern)).replace("__GROUP__",&extra::ps_quote(&group));let out=extra::powershell(cx,&script).await?;cx.set_variable(txt(&cx.cfg,"saveAs"),json!(out))?;Ok(Outcome::Satisfied)}
}
struct RegexReplace;
fn regex_replace_spec()->Value{action_spec("Regex Replace","regex","Applies a bounded .NET regular-expression replacement and stores the transformed text.",vec![json!({"key":"inputText","label":"Input text","type":"textarea","rows":4,"required":true,"maxLen":65536}),json!({"key":"pattern","label":"Regular expression","type":"text","required":true,"maxLen":2048}),json!({"key":"replacement","label":"Replacement","type":"text","maxLen":4096,"default":""}),json!({"key":"saveAs","label":"Output variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})],"Regex replace → {saveAs}","Uses .NET Regex with a 500 ms timeout to avoid pathological expressions.")}
#[async_trait] impl Block for RegexReplace{
 fn title(&self)->&'static str{"Regex Replace"} fn category(&self)->Cat{Cat::Action} fn spec(&self)->Option<Value>{Some(regex_replace_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&regex_replace_spec(),c)?;require_var(c,"saveAs")?;no_nul(c,&["inputText","pattern","replacement"])?;Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let input=cx.expand_template(&raw(&cx.cfg,"inputText"))?;let pattern=cx.expand_template(&raw(&cx.cfg,"pattern"))?;let replacement=cx.expand_template(&raw(&cx.cfg,"replacement"))?;let script=r#"$ErrorActionPreference='Stop';$inputText=__INPUT__;$pattern=__PATTERN__;$replacement=__REPLACEMENT__;try{$r=[regex]::Replace($inputText,$pattern,$replacement,[System.Text.RegularExpressions.RegexOptions]::None,[TimeSpan]::FromMilliseconds(500));[Console]::Out.Write($r)}catch{[Console]::Error.WriteLine($_.Exception.Message);exit 1}"#.replace("__INPUT__",&extra::ps_quote(&input)).replace("__PATTERN__",&extra::ps_quote(&pattern)).replace("__REPLACEMENT__",&extra::ps_quote(&replacement));let out=extra::powershell(cx,&script).await?;cx.set_variable(txt(&cx.cfg,"saveAs"),json!(out))?;Ok(Outcome::Satisfied)}
}
struct ForEachItem;
fn foreach_spec()->Value{action_spec("For Each Item","repeat","Iterates an array variable. Connect the loop body back to this block; ELSE exits after the final item.",vec![json!({"key":"itemsVariable","label":"Array variable","type":"text","required":true,"maxLen":64}),json!({"key":"itemAs","label":"Current item variable","type":"text","default":"item","required":true,"maxLen":64}),json!({"key":"indexAs","label":"Index variable (optional)","type":"text","default":"index","maxLen":64}),json!({"key":"maxItems","label":"Maximum items","type":"number","min":1,"max":10000,"step":1,"default":1000}),json!({"preset":"onFail"})],"For each {itemsVariable}","The current item and zero-based index are stored per automation run. Empty arrays immediately follow ELSE.")}
#[async_trait] impl Block for ForEachItem{
 fn title(&self)->&'static str{"For Each Item"} fn category(&self)->Cat{Cat::Action} fn spec(&self)->Option<Value>{Some(foreach_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&foreach_spec(),c)?;require_var(c,"itemsVariable")?;require_var(c,"itemAs")?;if !txt(c,"indexAs").is_empty(){require_var(c,"indexAs")?};let n=c["maxItems"].as_u64().unwrap_or(1000);if !(1..=10000).contains(&n){return Err("Maximum items must be between 1 and 10000".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let items=cx.get_variable(&txt(&cx.cfg,"itemsVariable")).ok_or("Array variable is not set")?;let a=items.as_array().ok_or("For Each Item expects a JSON array")?;let max=cx.cfg["maxItems"].as_u64().unwrap_or(1000) as usize;if a.len()>max{return Err(format!("Array has {} items; maximum is {max}",a.len()))};let idx=cx.visit.saturating_sub(1) as usize;if idx<a.len(){cx.set_variable(txt(&cx.cfg,"itemAs"),a[idx].clone())?;if !txt(&cx.cfg,"indexAs").is_empty(){cx.set_variable(txt(&cx.cfg,"indexAs"),json!(idx))?};Ok(Outcome::Satisfied)}else{Ok(Outcome::NotSatisfied)}}
}
struct CsvFilter;
fn csv_filter_spec()->Value{action_spec("Filter Array Rows","layers","Filters a JSON array of objects by equality, containment, or numeric comparison and writes the matching rows to another variable.",vec![json!({"key":"sourceVariable","label":"Source array variable","type":"text","required":true,"maxLen":64}),json!({"key":"property","label":"Property name","type":"text","required":true,"maxLen":128}),json!({"key":"operation","label":"Match","type":"select","default":"equals","options":[["equals","Equals text"],["contains","Contains text"],["not_equals","Does not equal"],["greater","Number greater than"],["less","Number less than"],["empty","Is empty"]]}),json!({"key":"expected","label":"Expected value","type":"text","maxLen":2048,"default":"","show":{"key":"operation","ne":"empty"}}),json!({"key":"saveAs","label":"Output array variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})],"Filter {sourceVariable}","Comparisons use ordinal-insensitive text matching; numeric operations require finite numbers.")}
#[async_trait] impl Block for CsvFilter{
 fn title(&self)->&'static str{"Filter Array Rows"} fn category(&self)->Cat{Cat::Action} fn spec(&self)->Option<Value>{Some(csv_filter_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_filter_spec(),c)?;require_var(c,"sourceVariable")?;require_var(c,"saveAs")?;no_nul(c,&["property","expected"])?;Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let source=cx.get_variable(&txt(&cx.cfg,"sourceVariable")).ok_or("Source array variable is not set")?;let arr=source.as_array().ok_or("Source variable must contain a JSON array")?;if arr.len()>100000{return Err("Array is too large to filter".into())};let prop=txt(&cx.cfg,"property");let op=txt(&cx.cfg,"operation");let expected=cx.expand_template(&raw(&cx.cfg,"expected"))?;let expn=expected.parse::<f64>().ok();let mut out=Vec::new();for item in arr{let value=item.get(&prop);let s=value.map(value_string).unwrap_or_default();let ok=match op.as_str(){"contains"=>s.to_lowercase().contains(&expected.to_lowercase()),"not_equals"=>!s.eq_ignore_ascii_case(&expected),"greater"=>value.and_then(Value::as_f64).or_else(||s.parse().ok()).zip(expn).is_some_and(|(a,b)|a>b),"less"=>value.and_then(Value::as_f64).or_else(||s.parse().ok()).zip(expn).is_some_and(|(a,b)|a<b),"empty"=>value.is_none_or(|x|x.is_null()||s.is_empty()),_=>s.eq_ignore_ascii_case(&expected)};if ok{out.push(item.clone())}}cx.set_variable(txt(&cx.cfg,"saveAs"),json!(out))?;Ok(Outcome::Satisfied)}
}

// ---------------- File system events ----------------
struct WaitFileEvent;
fn file_event_spec()->Value{action_spec("Wait for File Event","activity","Waits for a native filesystem notification and stores the event path. This is a one-event wait step and can be used as an automation entry trigger.",vec![json!({"key":"folder","label":"Folder to watch","type":"text","required":true,"maxLen":1000}),json!({"key":"pattern","label":"File pattern","type":"text","default":"*","maxLen":256}),json!({"key":"event","label":"Event","type":"select","default":"created","options":[["created","Created"],["changed","Changed"],["deleted","Deleted"],["renamed","Renamed"],["any","Any supported event"]]}),json!({"key":"recursive","label":"Include subfolders","type":"bool","default":false}),json!({"key":"waitSec","label":"Wait duration","type":"number","min":0.1,"max":86400,"step":0.5,"default":60,"unit":"s"}),json!({"key":"savePathAs","label":"Store changed path (optional)","type":"text","maxLen":64}),json!({"preset":"onFail"})],"Wait for {event} in {folder}","Uses FileSystemWatcher instead of repeatedly scanning the whole directory. One block execution consumes one event; reconnect the workflow to loop for recurring events.")}
#[async_trait] impl Block for WaitFileEvent{
 fn title(&self)->&'static str{"Wait for File Event"} fn category(&self)->Cat{Cat::Action} fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(file_event_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&file_event_spec(),c)?;no_nul(c,&["folder","pattern","savePathAs"])?;if !txt(c,"savePathAs").is_empty(){require_var(c,"savePathAs")?};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let folder=cx.expand_template(&txt(&cx.cfg,"folder"))?;let pattern=txt(&cx.cfg,"pattern");let event=txt(&cx.cfg,"event");let recursive=cx.cfg["recursive"].as_bool().unwrap_or(false);let secs=cx.cfg["waitSec"].as_f64().unwrap_or(60.0);if !Path::new(&folder).is_dir(){return Err("Watch folder does not exist".into())};let f=extra::ps_quote(&folder);let p=extra::ps_quote(&pattern);let script=r#"$ErrorActionPreference='Stop';$folder=__FOLDER__;$filter=__PATTERN__;$recursive=__RECURSIVE__;$seconds=__SECONDS__;$eventKind=__EVENT__;$mask=if($eventKind -eq 'any'){15}else{switch($eventKind){'created'{1}'changed'{4}'deleted'{2}'renamed'{8}default{1}}};$w=New-Object System.IO.FileSystemWatcher;$w.Path=$folder;$w.Filter=$filter;$w.IncludeSubdirectories=$recursive;$w.NotifyFilter=[IO.NotifyFilters]'FileName,DirectoryName,LastWrite,Size,CreationTime';$w.EnableRaisingEvents=$true;try{$ch=$w.WaitForChanged([IO.WatcherChangeTypes]$mask,[int]($seconds*1000));if($ch.TimedOut){[pscustomobject]@{timedOut=$true;name='';change=''}|ConvertTo-Json -Compress}else{[pscustomobject]@{timedOut=$false;name=[string]$ch.Name;change=[string]$ch.ChangeType}|ConvertTo-Json -Compress}}finally{$w.EnableRaisingEvents=$false;$w.Dispose()}"#
 .replace("__FOLDER__",&f).replace("__PATTERN__",&p).replace("__RECURSIVE__",if recursive{"$true"}else{"$false"}).replace("__SECONDS__",&secs.to_string()).replace("__EVENT__",&extra::ps_quote(&event));
 let out=extra::powershell(cx,&script).await?;let v:Value=serde_json::from_str(&out).map_err(|e|format!("Could not parse file event: {e}"))?;if v["timedOut"].as_bool().unwrap_or(true){return Ok(Outcome::NotSatisfied)};let name=v["name"].as_str().unwrap_or("");let full=Path::new(&folder).join(name).to_string_lossy().into_owned();if !txt(&cx.cfg,"savePathAs").is_empty(){cx.set_variable(txt(&cx.cfg,"savePathAs"),json!(full.clone()))?};(cx.log)("info",format!("Filesystem event {}: {}",v["change"].as_str().unwrap_or("changed"),full));Ok(Outcome::Satisfied)}
}

// ---------------- Per-user DPAPI credential vault ----------------
fn secret_key(c:&Value)->String{txt(c,"key")}
fn secret_spec(title:&str,desc:&str,fields:Vec<Value>)->Value{action_spec(title,"lock",desc,fields,title,"Vault entries are protected with Windows DPAPI for the current Windows user. Secrets are not written to automation fields or normal logs.")}
fn secret_script(body:&str,key:&str)->String{let key=extra::ps_quote(key);let prefix=r#"$ErrorActionPreference='Stop';$root=Join-Path $env:LOCALAPPDATA '00047';$path=Join-Path $root 'secrets.json';[void][IO.Directory]::CreateDirectory($root);$store=@{};if(Test-Path -LiteralPath $path){$loaded=Get-Content -LiteralPath $path -Raw -ErrorAction Stop|ConvertFrom-Json; if($null -ne $loaded){$loaded.PSObject.Properties|ForEach-Object{$store[$_.Name]=$_.Value}}};$key=__KEY__;"#;format!("{}{}",$prefix.replace("__KEY__",&key),body)}
struct SecretPrompt;
fn secret_prompt_spec()->Value{secret_spec("Save Secret in Vault","Prompts for a secret using a masked field and protects it for the current Windows user.",vec![json!({"key":"key","label":"Secret name","type":"text","required":true,"maxLen":128}),json!({"key":"description","label":"Prompt label","type":"text","default":"Enter secret","maxLen":128}),json!({"preset":"onFail"})])}
#[async_trait] impl Block for SecretPrompt{
 fn title(&self)->&'static str{"Save Secret in Vault"} fn category(&self)->Cat{Cat::Action} fn risky(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(secret_prompt_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&secret_prompt_spec(),c)?;no_nul(c,&["key","description"])?;if secret_key(c).is_empty()||secret_key(c).len()>128{return Err("Secret name must be 1–128 characters".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let key=secret_key(&cx.cfg);let label=txt(&cx.cfg,"description");let mut script=secret_script(r#"
Add-Type -AssemblyName System.Windows.Forms
[void][System.Windows.Forms.Application]::EnableVisualStyles()
$form=New-Object Windows.Forms.Form;$form.Text='00047 · Secret Vault';$form.Width=440;$form.Height=170;$form.StartPosition='CenterScreen';$form.TopMost=$true
$label=New-Object Windows.Forms.Label;$label.Left=12;$label.Top=12;$label.Width=400;$label.Text=__LABEL__; $form.Controls.Add($label)
$box=New-Object Windows.Forms.TextBox;$box.Left=12;$box.Top=40;$box.Width=400;$box.UseSystemPasswordChar=$true;$form.Controls.Add($box)
$save=New-Object Windows.Forms.Button;$save.Text='Save securely';$save.Left=230;$save.Top=78;$save.Width=100;$save.DialogResult=[Windows.Forms.DialogResult]::OK;$form.Controls.Add($save)
$cancel=New-Object Windows.Forms.Button;$cancel.Text='Cancel';$cancel.Left=340;$cancel.Top=78;$cancel.Width=72;$cancel.DialogResult=[Windows.Forms.DialogResult]::Cancel;$form.Controls.Add($cancel);$form.AcceptButton=$save;$form.CancelButton=$cancel
$result=$form.ShowDialog();$secret=$box.Text;$form.Dispose()
if($result -ne [Windows.Forms.DialogResult]::OK -or [string]::IsNullOrEmpty($secret)){Write-Output 'cancelled';exit 0}
$secure=ConvertTo-SecureString $secret -AsPlainText -Force;$store[$key]=ConvertFrom-SecureString $secure
$tmp=$path+'.tmp';$store|ConvertTo-Json -Compress|Set-Content -LiteralPath $tmp -Encoding UTF8;Move-Item -LiteralPath $tmp -Destination $path -Force
Write-Output 'saved'
"#,&key).replace("__LABEL__",&extra::ps_quote(&label));let out=extra::powershell(cx,&script).await?;if out.trim()=="cancelled"{Ok(Outcome::NotSatisfied)}else if out.trim()=="saved"{Ok(Outcome::Satisfied)}else{Err("Secret vault did not confirm the save".into())}}
}
struct SecretGet;
fn secret_get_spec()->Value{action_spec("Get Secret from Vault","lock","Decrypts a named DPAPI-protected secret into a run-scoped variable without logging its value.",vec![json!({"key":"key","label":"Secret name","type":"text","required":true,"maxLen":128}),json!({"key":"saveAs","label":"Secret variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})],"Get secret {key} → {saveAs}","The secret is decrypted for the current Windows user and is only stored in this run's variable store. Avoid writing secret variables to logs or files.")}
#[async_trait] impl Block for SecretGet{
 fn title(&self)->&'static str{"Get Secret from Vault"} fn category(&self)->Cat{Cat::Action}
 fn spec(&self)->Option<Value>{Some(secret_get_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&secret_get_spec(),c)?;no_nul(c,&["key","saveAs"])?;if secret_key(c).is_empty(){return Err("Secret name is required".into())};require_var(c,"saveAs")}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let key=secret_key(&cx.cfg);let s=secret_script(r#"if(-not $store.ContainsKey($key)){throw 'Secret was not found'};$secure=ConvertTo-SecureString ([string]$store[$key]);$ptr=[Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure);try{$plain=[Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr);[Console]::Out.Write($plain)}finally{[Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr)}"#,&key);let out=extra::powershell(cx,&s).await?;if out.len()>65536{return Err("Secret exceeds the supported 64 KiB limit".into())};cx.set_variable(txt(&cx.cfg,"saveAs"),json!(out))?;Ok(Outcome::Satisfied)}
}
struct SecretDelete;
fn secret_delete_spec()->Value{secret_spec("Delete Secret from Vault","Removes a named DPAPI-protected secret.",vec![json!({"key":"key","label":"Secret name","type":"text","required":true,"maxLen":128}),json!({"preset":"onFail"})])}
#[async_trait] impl Block for SecretDelete{
 fn title(&self)->&'static str{"Delete Secret from Vault"} fn category(&self)->Cat{Cat::Action} fn risky(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(secret_delete_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&secret_delete_spec(),c)?;no_nul(c,&["key"])?;if secret_key(c).is_empty(){return Err("Secret name is required".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let s=secret_script(r#"if(-not $store.ContainsKey($key)){Write-Output 'missing';exit 0};$store.Remove($key);$tmp=$path+'.tmp';$store|ConvertTo-Json -Compress|Set-Content -LiteralPath $tmp -Encoding UTF8;Move-Item -LiteralPath $tmp -Destination $path -Force;Write-Output 'deleted'"#,&secret_key(&cx.cfg));let out=extra::powershell(cx,&s).await?;if out.trim()=="missing"{Ok(Outcome::NotSatisfied)}else{Ok(Outcome::Satisfied)}}
}
struct SecretExists;
fn secret_exists_spec()->Value{let f=vec![json!({"key":"key","label":"Secret name","type":"text","required":true,"maxLen":128})];condition_spec("Secret Exists","lock","Checks whether a named encrypted vault entry exists without revealing its value.",f,"Secret exists: {key}","Only checks whether the entry exists; it never decrypts or logs the secret.")}
#[async_trait] impl Block for SecretExists{
 fn title(&self)->&'static str{"Secret Exists"} fn category(&self)->Cat{Cat::Condition}
 fn spec(&self)->Option<Value>{Some(secret_exists_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&secret_exists_spec(),c)?;no_nul(c,&["key"])?;if secret_key(c).is_empty(){return Err("Secret name is required".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let s=secret_script(r#"Write-Output ([bool]$store.ContainsKey($key)|ConvertTo-Json -Compress)"#,&secret_key(&cx.cfg));let out=extra::powershell(cx,&s).await?;let exists=serde_json::from_str::<bool>(out.trim()).map_err(|e|e.to_string())?;Ok(condition_ok(exists))}
}

// ---------------- Reusable workflows (engine executes this marker inline) ----------------
struct CallSubflow;
fn call_subflow_spec()->Value{action_spec("Call Subflow","layers","Runs another saved automation inline using this run's variables, then resumes the caller when the subflow finishes.",vec![json!({"key":"automationId","label":"Automation ID","type":"text","required":true,"maxLen":128}),json!({"preset":"timeout","default":0}),json!({"preset":"onFail"})],"Call automation {automationId}","Subflows run sequentially in the caller's run context. Avoid direct or indirect recursive calls; the engine enforces a maximum nesting depth.")}
#[async_trait] impl Block for CallSubflow{
 fn title(&self)->&'static str{"Call Subflow"} fn category(&self)->Cat{Cat::Action} fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(call_subflow_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&call_subflow_spec(),c)?;no_nul(c,&["automationId"])?;if txt(c,"automationId").is_empty(){return Err("Automation ID is required".into())};Ok(())}
 async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Ok(Outcome::Satisfied)}
}

// Engine recognizes this block and executes its referenced graph inline after validation.
pub(super) fn is_call_subflow(kind:&str)->bool{kind=="act.call_subflow"}
pub(super) fn subflow_id(cfg:&Value)->String{txt(cfg,"automationId")}

// ---------------- Debug breakpoint marker ----------------
struct Breakpoint;
fn breakpoint_spec()->Value{action_spec("Debugger Breakpoint","bug","Pauses this automation at this node when execution debug mode is active. Resume or step from the debugger panel.",vec![json!({"key":"enabled","label":"Pause here","type":"bool","default":true}),json!({"preset":"onFail"})],"Breakpoint","Ignored unless debug mode is enabled for the running automation.")}
#[async_trait] impl Block for Breakpoint{
 fn title(&self)->&'static str{"Debugger Breakpoint"} fn category(&self)->Cat{Cat::Action}
 fn spec(&self)->Option<Value>{Some(breakpoint_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&breakpoint_spec(),c)}
 async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Ok(Outcome::Satisfied)}
}
pub(super) fn is_breakpoint(kind:&str,cfg:&Value)->bool{kind=="act.debug_breakpoint"&&cfg.get("enabled").and_then(Value::as_bool).unwrap_or(true)}

// ---------------- Registry ----------------
pub(super) fn register_into(m:&mut std::collections::HashMap<&'static str,Box<dyn Block>>){
 m.insert("cond.ui_element",Box::new(UiElementState));
 m.insert("act.ui_click",Box::new(UiClick));
 m.insert("act.ui_set_value",Box::new(UiSetValue));
 m.insert("act.ui_get_value",Box::new(UiGetValue));
 m.insert("act.csv_read",Box::new(CsvRead));
 m.insert("act.csv_write",Box::new(CsvWrite));
 m.insert("act.json_parse",Box::new(JsonParse));
 m.insert("act.json_get",Box::new(JsonGet));
 m.insert("act.transform_value",Box::new(Transform));
 m.insert("act.regex_extract",Box::new(RegexExtract));
 m.insert("act.regex_replace",Box::new(RegexReplace));
 m.insert("act.for_each",Box::new(ForEachItem));
 m.insert("act.filter_array",Box::new(CsvFilter));
 m.insert("act.wait_file_event",Box::new(WaitFileEvent));
 m.insert("act.secret_prompt",Box::new(SecretPrompt));
 m.insert("act.secret_get",Box::new(SecretGet));
 m.insert("act.secret_delete",Box::new(SecretDelete));
 m.insert("cond.secret_exists",Box::new(SecretExists));
 m.insert("act.call_subflow",Box::new(CallSubflow));
 m.insert("act.debug_breakpoint",Box::new(Breakpoint));
}

#[cfg(test)]
mod tests {
 use super::*;
 #[test] fn csv_parser_handles_quotes_and_newlines(){
  let rows=csv_parse("name,note\r\nAda,\"likes, commas\"\r\nGrace,\"line 1\nline 2\"\r\n").unwrap();
  assert_eq!(rows.len(),3);assert_eq!(rows[1][1],"likes, commas");assert_eq!(rows[2][1],"line 1\nline 2");
 }
 #[test] fn csv_parser_rejects_malformed_rows(){
  assert!(csv_parse("a,b\n1").is_ok());
  assert!(csv_parse("a,b\n\"bad").is_err());
  assert!(csv_parse("a,b\nx\"y,z").is_err());
 }
 #[test] fn variable_name_validation_is_strict(){
  assert!(valid_var("_item-1"));assert!(valid_var("secret.token"));assert!(!valid_var("1bad"));assert!(!valid_var("has space"));
 }
}
