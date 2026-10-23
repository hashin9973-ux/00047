//! UI Automation and data-processing blocks.
use super::{extra, Block, Cat, NodeCx, Outcome};
use crate::spec;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

fn txt(c: &Value, key: &str) -> String { c.get(key).and_then(Value::as_str).unwrap_or("").to_string() }
fn templ(cx: &NodeCx, value: &str) -> Result<String, String> { cx.expand_template(value) }
fn ps_quote(value: &str) -> String { format!("'{}'", value.replace('\0', "").replace('\'', "''")) }
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64
        && name.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && name.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}
fn validate_variable(c: &Value, key: &str, required: bool) -> Result<(), String> {
    let name=txt(c,key);
    if required && name.is_empty(){return Err(format!("{key} is required"))}
    if !name.is_empty() && !valid_name(&name){return Err(format!("{key} must start with a letter or underscore and contain only letters, digits, _, -, or ."))}
    Ok(())
}
fn action_spec(title:&str,icon:&str,description:&str,fields:Vec<Value>,summary:&str,help:&str)->Value {
    json!({"category":"action","title":title,"icon":icon,"description":description,"fields":fields,"summary":summary,"help":help})
}
fn ui_selector_fields()->Vec<Value>{
    vec![
        json!({"key":"processName","label":"Application process name (optional)","type":"text","maxLen":128,"default":""}),
        json!({"key":"windowTitle","label":"Window title contains (optional)","type":"text","maxLen":256,"default":""}),
        json!({"key":"controlName","label":"Control name (optional)","type":"text","maxLen":256,"default":""}),
        json!({"key":"automationId","label":"Automation ID (optional)","type":"text","maxLen":256,"default":""}),
        json!({"key":"controlType","label":"Control type (optional)","type":"select","default":"","options":[["","Any"],["Button","Button"],["Edit","Text field"],["Text","Text"],["CheckBox","Checkbox"],["RadioButton","Radio button"],["ComboBox","Combo box"],["List","List"],["ListItem","List item"],["MenuItem","Menu item"],["TabItem","Tab"],["Window","Window"],["Hyperlink","Link"]]})
    ]
}
fn validate_ui_selector(c:&Value)->Result<(),String>{
    for k in ["processName","windowTitle","controlName","automationId"] {if txt(c,k).contains('\0'){return Err(format!("{k} contains a NUL character"))}}
    let has_window=!txt(c,"processName").trim().is_empty()||!txt(c,"windowTitle").trim().is_empty();
    let has_control=!txt(c,"controlName").trim().is_empty()||!txt(c,"automationId").trim().is_empty()||!txt(c,"controlType").trim().is_empty();
    if !has_window{return Err("Set Application process name or Window title to scope the search".into())}
    if !has_control{return Err("Set Control name, Automation ID, or Control type".into())}
    Ok(())
}
fn control_type_expr(name:&str)-> &'static str {
    match name {
        "Button"=>"[System.Windows.Automation.ControlType]::Button",
        "Edit"=>"[System.Windows.Automation.ControlType]::Edit",
        "Text"=>"[System.Windows.Automation.ControlType]::Text",
        "CheckBox"=>"[System.Windows.Automation.ControlType]::CheckBox",
        "RadioButton"=>"[System.Windows.Automation.ControlType]::RadioButton",
        "ComboBox"=>"[System.Windows.Automation.ControlType]::ComboBox",
        "List"=>"[System.Windows.Automation.ControlType]::List",
        "ListItem"=>"[System.Windows.Automation.ControlType]::ListItem",
        "MenuItem"=>"[System.Windows.Automation.ControlType]::MenuItem",
        "TabItem"=>"[System.Windows.Automation.ControlType]::TabItem",
        "Window"=>"[System.Windows.Automation.ControlType]::Window",
        "Hyperlink"=>"[System.Windows.Automation.ControlType]::Hyperlink",
        _=>"$null"
    }
}
fn ui_script(c:&Value,operation:&str,value:&str,condition_mode:&str,expected:&str)->String{
    let ct_expr=control_type_expr(&txt(c,"controlType"));
    format!(r#"
$ErrorActionPreference='Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$processFilter={process}
$titleFilter={title}
$nameFilter={name}
$idFilter={automationId}
$controlType={controlType}
$operation={operation}
$newValue={newValue}
$conditionMode={conditionMode}
$expected={expected}
$root=[System.Windows.Automation.AutomationElement]::RootElement
$windows=$root.FindAll([System.Windows.Automation.TreeScope]::Children,[System.Windows.Automation.Condition]::TrueCondition)
$window=$null
foreach($candidate in $windows) {{
  $current=$candidate.Current
  $ok=$true
  if($titleFilter.Length -gt 0 -and $current.Name.IndexOf($titleFilter,[StringComparison]::OrdinalIgnoreCase) -lt 0) {{$ok=$false}}
  if($processFilter.Length -gt 0) {{
    $p=Get-Process -Id $current.ProcessId -ErrorAction SilentlyContinue
    $expectedProc=[regex]::Replace($processFilter,'(?i)\.exe$','')
    if($null -eq $p -or $p.ProcessName -ine $expectedProc) {{$ok=$false}}
  }}
  if($ok) {{$window=$candidate;break}}
}}
if($null -eq $window) {{
  $windowMatches=($conditionMode -eq 'missing')
  [pscustomobject]@{{found=[bool]$windowMatches;name='';value='';automationId='';controlType='';enabled=$false;x=0;y=0;width=0;height=0}} | ConvertTo-Json -Compress
  exit 0
}}
$conditions=New-Object 'System.Collections.Generic.List[System.Windows.Automation.Condition]'
if($nameFilter.Length -gt 0) {{$conditions.Add([System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty,$nameFilter))}}
if($idFilter.Length -gt 0) {{$conditions.Add([System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty,$idFilter))}}
if($null -ne $controlType) {{$conditions.Add([System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,$controlType))}}
if($conditions.Count -eq 1) {{$condition=$conditions[0]}} else {{$condition=[System.Windows.Automation.AndCondition]::new($conditions.ToArray())}}
$elements=$window.FindAll([System.Windows.Automation.TreeScope]::Descendants,$condition)
$found=($elements.Count -gt 0)
if(-not $found) {{
  $missingMatches=($conditionMode -eq 'missing')
  [pscustomobject]@{{found=[bool]$missingMatches;name='';value='';automationId='';controlType='';enabled=$false;x=0;y=0;width=0;height=0}} | ConvertTo-Json -Compress
  exit 0
}}
$element=$elements[0]
$current=$element.Current
$valueText=[string]$current.Name
try {{$vp=$element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern);$valueText=[string]$vp.Current.Value}} catch {{}}
switch($operation) {{
  'click' {{
    if(-not $current.IsEnabled) {{throw 'Matching UI element is disabled'}}
    try {{$element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()}}
    catch {{try {{$element.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()}} catch {{throw 'Matching UI element supports neither Invoke nor Select'}}}}
  }}
  'set_value' {{
    if(-not $current.IsEnabled) {{throw 'Matching UI element is disabled'}}
    try {{$element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($newValue)}}
    catch {{throw 'Matching UI element does not support ValuePattern; select a supported control'}}
  }}
  'select' {{
    if(-not $current.IsEnabled) {{throw 'Matching UI element is disabled'}}
    try {{$element.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()}}
    catch {{throw 'Matching UI element does not support SelectionItemPattern'}}
  }}
  default {{}}
}}
$matched=$found
if($conditionMode -eq 'missing') {{$matched=(-not $found)}}
elseif($conditionMode -eq 'value_equals') {{$matched=($found -and $valueText -ceq $expected)}}
elseif($conditionMode -eq 'value_contains') {{$matched=($found -and $valueText.IndexOf($expected,[StringComparison]::OrdinalIgnoreCase) -ge 0)}}
$rect=$current.BoundingRectangle
[pscustomobject]@{{found=[bool]$matched;name=[string]$current.Name;value=[string]$valueText;automationId=[string]$current.AutomationId;controlType=[string]$current.ControlType.ProgrammaticName;enabled=[bool]$current.IsEnabled;x=[double]$rect.X;y=[double]$rect.Y;width=[double]$rect.Width;height=[double]$rect.Height}} | ConvertTo-Json -Compress -Depth 3
"#,
        process=ps_quote(&txt(c,"processName")),title=ps_quote(&txt(c,"windowTitle")),
        name=ps_quote(&txt(c,"controlName")),automationId=ps_quote(&txt(c,"automationId")),
        controlType=ct_expr,operation=ps_quote(operation),newValue=ps_quote(value),
        conditionMode=ps_quote(condition_mode),expected=ps_quote(expected))
}
async fn ui_probe(cx:&NodeCx,c:&Value,mode:&str,expected:&str)->Result<Value,String>{
    validate_ui_selector(c)?;
    let script=ui_script(c,"probe","",mode,expected);
    let out=extra::powershell(cx,&script).await?;
    serde_json::from_str(&out).map_err(|e|format!("Could not parse UI Automation result: {e}"))
}

struct UiElementCondition;
fn ui_condition_spec()->Value{
    let mut fields=ui_selector_fields();
    fields.push(json!({"key":"conditionMode","label":"Satisfied when","type":"select","default":"exists","options":[["exists","Element exists"],["missing","Element is missing"],["value_equals","Value equals"],["value_contains","Value contains"]]}));
    fields.push(json!({"key":"expectedValue","label":"Expected value","type":"text","maxLen":2048,"default":"","show":{"key":"conditionMode","in":["value_equals","value_contains"]}}));
    fields.extend(extra::behavior_fields());
    json!({"category":"condition","title":"Wait for UI Element","icon":"app-window","description":"Finds an application control with Windows UI Automation.","fields":fields,"summary":"UI element: {controlName}","help":"Requires an interactive Windows desktop. Some elevated, remote, canvas-rendered, or custom-drawn controls may not expose UI Automation patterns. Use Keyboard Shortcut/Mouse Automation as a fallback."})
}
#[async_trait]
impl Block for UiElementCondition{
    fn title(&self)-> &'static str {"Wait for UI Element"} fn category(&self)->Cat{Cat::Condition}
    fn supports_timeout(&self)->bool{true} fn spec(&self)->Option<Value>{Some(ui_condition_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{
        spec::validate(&ui_condition_spec(),c)?;validate_ui_selector(c)?;
        let mode=txt(c,"conditionMode");
        if matches!(mode.as_str(),"value_equals"|"value_contains")&&txt(c,"expectedValue").is_empty(){return Err("Expected value is required for this condition mode".into())}
        extra::wait_settings(c).map(|_|())
    }
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let (behavior,poll,stable,timeout)=extra::wait_settings(&cx.cfg)?;
        let cfg=cx.cfg.clone();
        extra::poll_condition(cx,&behavior,poll,stable,timeout,||{
            let cfg=cfg.clone();
            Box::pin(async move{
                let condition=txt(&cfg,"conditionMode");
                let expected=templ(cx,&txt(&cfg,"expectedValue"))?;
                let value=ui_probe(cx,&cfg,&condition,&expected).await?;
                let ok=value.get("found").and_then(Value::as_bool).unwrap_or(false);
                (cx.report)(if behavior=="check"{"running"}else{"waiting"},None,format!("UI element condition '{}': {}",condition,if ok{"satisfied"}else{"not satisfied"}));
                Ok(ok)
            }) as extra::Probe<'_>
        }).await
    }
}
fn ui_action_fields(include_value:bool,include_save_as:bool)->Vec<Value>{
    let mut fields=ui_selector_fields();
    if include_value{fields.push(json!({"key":"value","label":"Value to set","type":"textarea","rows":3,"required":true,"maxLen":8192}));}
    if include_save_as{fields.push(json!({"key":"saveAs","label":"Save element details to variable (optional)","type":"text","maxLen":64,"default":""}));}
    fields.push(json!({"preset":"timeout","default":15}));fields.push(json!({"preset":"onFail"}));fields
}
struct UiFind;
#[async_trait]
impl Block for UiFind{
    fn title(&self)-> &'static str {"Find UI Element"} fn category(&self)->Cat{Cat::Action}
    fn supports_timeout(&self)->bool{true}
    fn spec(&self)->Option<Value>{Some(action_spec("Find UI Element","app-window","Find a UI Automation control and optionally save its details as JSON in a workflow variable.",ui_action_fields(false,true),"Find UI element","Returns name, current value, Automation ID, type, enabled state and bounds."))}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&self.spec().unwrap(),c)?;validate_ui_selector(c)?;validate_variable(c,"saveAs",false)}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let script=ui_script(&cx.cfg,"probe","","exists","");
        let out=extra::powershell(cx,&script).await?;
        let result:Value=serde_json::from_str(&out).map_err(|e|format!("Could not parse UI Automation result: {e}"))?;
        if !result.get("found").and_then(Value::as_bool).unwrap_or(false){return Ok(Outcome::NotSatisfied)}
        let name=txt(&cx.cfg,"saveAs");if !name.is_empty(){cx.set_variable(name,result.clone())?;}
        (cx.log)("info",format!("Found UI element: {}",result.get("name").and_then(Value::as_str).unwrap_or("(unnamed)")));
        Ok(Outcome::Satisfied)
    }
}
struct UiClick;
#[async_trait]
impl Block for UiClick{
    fn title(&self)-> &'static str {"Click UI Element"} fn category(&self)->Cat{Cat::Action}
    fn supports_timeout(&self)->bool{true}
    fn spec(&self)->Option<Value>{Some(action_spec("Click UI Element","mouse-pointer-2","Invokes or selects a matching application control using Windows UI Automation.",ui_action_fields(false,false),"Click UI element","Prefer selectors over coordinates. Disabled controls and unsupported patterns return explicit errors."))}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&self.spec().unwrap(),c)?;validate_ui_selector(c)}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let out=extra::powershell(cx,&ui_script(&cx.cfg,"click","","exists","")).await?;
        let result:Value=serde_json::from_str(&out).map_err(|e|format!("Could not parse UI Automation result: {e}"))?;
        if !result.get("found").and_then(Value::as_bool).unwrap_or(false){return Err("No UI element matched the configured selectors".into())}
        (cx.log)("info",format!("Clicked UI element: {}",result.get("name").and_then(Value::as_str).unwrap_or("(unnamed)")));
        Ok(Outcome::Satisfied)
    }
}
struct UiSetValue;
#[async_trait]
impl Block for UiSetValue{
    fn title(&self)-> &'static str {"Set UI Element Value"} fn category(&self)->Cat{Cat::Action}
    fn supports_timeout(&self)->bool{true}
    fn spec(&self)->Option<Value>{Some(action_spec("Set UI Element Value","file-pen","Sets a control's value through the UI Automation ValuePattern.",ui_action_fields(true,false),"Set UI value","Works with controls that expose ValuePattern. Does not type by simulating keystrokes."))}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&self.spec().unwrap(),c)?;validate_ui_selector(c)}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let value=templ(cx,&txt(&cx.cfg,"value"))?;
        let out=extra::powershell(cx,&ui_script(&cx.cfg,"set_value",&value,"exists","")).await?;
        let result:Value=serde_json::from_str(&out).map_err(|e|format!("Could not parse UI Automation result: {e}"))?;
        if !result.get("found").and_then(Value::as_bool).unwrap_or(false){return Err("No UI element matched the configured selectors".into())}
        Ok(Outcome::Satisfied)
    }
}
struct UiSelectItem;
#[async_trait]
impl Block for UiSelectItem{
    fn title(&self)-> &'static str {"Select UI Item"} fn category(&self)->Cat{Cat::Action}
    fn supports_timeout(&self)->bool{true}
    fn spec(&self)->Option<Value>{
        let mut fields=ui_selector_fields();fields.extend([json!({"preset":"timeout","default":15}),json!({"preset":"onFail"})]);
        Some(action_spec("Select UI Item","layers","Selects a matching item with UI Automation SelectionItemPattern.",fields,"Select UI item","Targets a list item, tab, radio button, or similar selectable control."))
    }
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&self.spec().unwrap(),c)?;validate_ui_selector(c)}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let out=extra::powershell(cx,&ui_script(&cx.cfg,"select","","exists","")).await?;
        let result:Value=serde_json::from_str(&out).map_err(|e|format!("Could not parse UI Automation result: {e}"))?;
        if !result.get("found").and_then(Value::as_bool).unwrap_or(false){return Err("No UI element matched the configured selectors".into())}
        Ok(Outcome::Satisfied)
    }
}

struct JsonExtract;
fn json_extract_spec()->Value{
    action_spec("Extract JSON Value","file-json","Parses JSON and stores a selected property or array item in a workflow variable.",vec![
        json!({"key":"jsonText","label":"JSON text or variable template","type":"textarea","rows":5,"required":true,"maxLen":32768}),
        json!({"key":"jsonPath","label":"Property path (dot-separated; numeric parts index arrays; blank selects whole JSON)","type":"text","maxLen":1024,"default":""}),
        json!({"key":"saveAs","label":"Save result to variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})
    ],"Extract JSON value","Typed values are preserved. Example path: items.0.name.");
}
#[async_trait]
impl Block for JsonExtract{
    fn title(&self)-> &'static str {"Extract JSON Value"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(json_extract_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&json_extract_spec(),c)?;validate_variable(c,"saveAs",true)?;if txt(c,"jsonPath").contains('\0'){return Err("JSON path contains a NUL character".into())}Ok(())}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let raw=templ(cx,&txt(&cx.cfg,"jsonText"))?;
        let value:Value=serde_json::from_str(&raw).map_err(|e|format!("Invalid JSON input: {e}"))?;
        let mut selected=&value;
        let path=txt(&cx.cfg,"jsonPath");
        if !path.trim().is_empty(){for part in path.split('.'){
            selected=match selected{
                Value::Object(map)=>map.get(part).ok_or_else(||format!("JSON property '{part}' was not found"))?,
                Value::Array(items)=>items.get(part.parse::<usize>().map_err(|_|format!("'{part}' is not an array index"))?).ok_or_else(||format!("JSON array index '{part}' is out of range"))?,
                _=>return Err(format!("Cannot read '{part}' from a non-object/non-array JSON value"))
            };
        }}
        cx.set_variable(txt(&cx.cfg,"saveAs"),selected.clone())?;
        Ok(Outcome::Satisfied)
    }
}
struct RegexExtract;
fn regex_spec()->Value{
    action_spec("Regex Extract","regex","Extracts a named or numbered capture group with a time-limited .NET regular expression.",vec![
        json!({"key":"input","label":"Input text or variable template","type":"textarea","rows":4,"required":true,"maxLen":32768}),
        json!({"key":"pattern","label":"Regular expression","type":"text","required":true,"maxLen":2048}),
        json!({"key":"group","label":"Capture group index","type":"number","min":0,"max":100,"step":1,"default":0}),
        json!({"key":"ignoreCase","label":"Ignore case","type":"bool","default":false}),
        json!({"key":"saveAs","label":"Save capture to variable","type":"text","required":true,"maxLen":64}),
        json!({"preset":"timeout","default":5}),json!({"preset":"onFail"})
    ],"Extract regex capture","Regular expressions are limited to a 2-second match timeout to avoid runaway patterns. A missing match returns Not Satisfied; invalid patterns return an explicit error.");
}
#[async_trait]
impl Block for RegexExtract{
    fn title(&self)-> &'static str {"Regex Extract"} fn category(&self)->Cat{Cat::Action}
    fn supports_timeout(&self)->bool{true} fn spec(&self)->Option<Value>{Some(regex_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&regex_spec(),c)?;validate_variable(c,"saveAs",true)?;if txt(c,"pattern").contains('\0'){return Err("Regular expression contains a NUL character".into())}Ok(())}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let input=templ(cx,&txt(&cx.cfg,"input"))?;let pattern=templ(cx,&txt(&cx.cfg,"pattern"))?;
        let group=cx.cfg.get("group").and_then(Value::as_u64).unwrap_or(0).min(100);
        let ignore=if cx.cfg.get("ignoreCase").and_then(Value::as_bool).unwrap_or(false){"IgnoreCase"}else{"None"};
        let script=format!(r#"
$ErrorActionPreference='Stop'
$text={input}
$pattern={pattern}
$options=[System.Text.RegularExpressions.RegexOptions]::{ignore}
$match=[regex]::Match($text,$pattern,$options,[TimeSpan]::FromSeconds(2))
$value=''
if($match.Success){{if($match.Groups.Count -le {group}){{throw 'Capture group index does not exist'}};$value=$match.Groups[{group}].Value}}
[pscustomobject]@{{matched=[bool]$match.Success;value=[string]$value}} | ConvertTo-Json -Compress
"#,input=ps_quote(&input),pattern=ps_quote(&pattern),ignore=ignore,group=group);
        let out=extra::powershell(cx,&script).await?;
        let result:Value=serde_json::from_str(&out).map_err(|e|format!("Could not parse regex result: {e}"))?;
        if !result.get("matched").and_then(Value::as_bool).unwrap_or(false){return Ok(Outcome::NotSatisfied)}
        cx.set_variable(txt(&cx.cfg,"saveAs"),Value::String(result.get("value").and_then(Value::as_str).unwrap_or("").to_string()))?;
        Ok(Outcome::Satisfied)
    }
}

fn parse_csv(input:&str)->Result<Vec<Vec<String>>,String>{
    if input.is_empty(){return Ok(Vec::new())}
    let chars:Vec<char>=input.chars().collect();let mut rows:Vec<Vec<String>>=Vec::new();let mut row:Vec<String>=Vec::new();
    let mut cell=String::new();let mut quoted=false;let mut closed_quote=false;let mut i=0;
    while i<chars.len(){
        let ch=chars[i];
        if quoted {if ch=='"' {if i+1<chars.len()&&chars[i+1]=='"' {cell.push('"');i+=2;continue}quoted=false;closed_quote=true;i+=1;continue}cell.push(ch);i+=1;continue}
        match ch{
            '"' if cell.is_empty()&&!closed_quote=>{quoted=true;i+=1}
            '"'=>return Err("Unexpected quote in unquoted CSV field".into()),
            ','=>{row.push(std::mem::take(&mut cell));closed_quote=false;i+=1}
            '\n'=>{row.push(std::mem::take(&mut cell));rows.push(std::mem::take(&mut row));closed_quote=false;i+=1}
            '\r' if i+1<chars.len()&&chars[i+1]=='\n'=>{row.push(std::mem::take(&mut cell));rows.push(std::mem::take(&mut row));closed_quote=false;i+=2}
            '\r'=>{row.push(std::mem::take(&mut cell));rows.push(std::mem::take(&mut row));closed_quote=false;i+=1}
            _ if closed_quote&&!ch.is_whitespace()=>return Err("Unexpected character after closing CSV quote".into()),
            _=>{cell.push(ch);i+=1}
        }
    }
    if quoted{return Err("CSV input ended inside a quoted field".into())}
    if !cell.is_empty()||!row.is_empty()||!closed_quote{row.push(cell);rows.push(row)}
    if rows.last().is_some_and(|r|r.len()==1&&r[0].is_empty()){rows.pop();}
    Ok(rows)
}
fn csv_escape(value:&str)->String{
    if value.chars().any(|c|matches!(c,','|'"'|'\r'|'\n')){format!("\"{}\"",value.replace('"',"\"\""))}else{value.to_string()}
}
struct CsvToJson;
fn csv_to_json_spec()->Value{
    action_spec("CSV to JSON","file-spreadsheet","Parses CSV with the first row as headers and saves an array of objects.",vec![
        json!({"key":"csvText","label":"CSV text or variable template","type":"textarea","rows":6,"required":true,"maxLen":32768}),
        json!({"key":"saveAs","label":"Save array to variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})
    ],"CSV to JSON","Supports quoted commas, escaped quotes and embedded newlines. Duplicate/empty headers and inconsistent row widths are rejected.");
}
#[async_trait]
impl Block for CsvToJson{
    fn title(&self)-> &'static str {"CSV to JSON"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(csv_to_json_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_to_json_spec(),c)?;validate_variable(c,"saveAs",true)}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;let csv=templ(cx,&txt(&cx.cfg,"csvText"))?;let rows=parse_csv(&csv)?;
        if rows.is_empty(){cx.set_variable(txt(&cx.cfg,"saveAs"),Value::Array(Vec::new()))?;return Ok(Outcome::Satisfied)}
        let headers=rows[0].iter().map(|h|h.trim().to_string()).collect::<Vec<_>>();
        if headers.iter().any(|h|h.is_empty()){return Err("CSV headers must not be empty".into())}
        let mut seen=BTreeSet::new();if headers.iter().any(|h|!seen.insert(h.clone())){return Err("CSV headers must be unique".into())}
        let mut values=Vec::new();
        for (index,row) in rows.iter().enumerate().skip(1){
            if row.len()!=headers.len(){return Err(format!("CSV row {} has {} columns; expected {}",index+1,row.len(),headers.len()))}
            let mut obj=Map::new();for(key,val)in headers.iter().zip(row){obj.insert(key.clone(),Value::String(val.clone()));}values.push(Value::Object(obj))
        }
        cx.set_variable(txt(&cx.cfg,"saveAs"),Value::Array(values))?;(cx.log)("info",format!("Parsed {} CSV data rows",rows.len().saturating_sub(1)));Ok(Outcome::Satisfied)
    }
}
struct JsonToCsv;
fn json_to_csv_spec()->Value{
    action_spec("JSON to CSV","file-spreadsheet","Converts JSON objects into CSV text stored in a workflow variable.",vec![
        json!({"key":"jsonText","label":"JSON array or variable template","type":"textarea","rows":6,"required":true,"maxLen":32768}),
        json!({"key":"saveAs","label":"Save CSV text to variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})
    ],"JSON to CSV","The header is the sorted union of object keys. Missing properties are empty cells; nested values are JSON-encoded.");
}
#[async_trait]
impl Block for JsonToCsv{
    fn title(&self)-> &'static str {"JSON to CSV"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(json_to_csv_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&json_to_csv_spec(),c)?;validate_variable(c,"saveAs",true)}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;let src=templ(cx,&txt(&cx.cfg,"jsonText"))?;
        let arr:Vec<Value>=serde_json::from_str(&src).map_err(|e|format!("JSON input must be an array: {e}"))?;
        let mut headers=BTreeSet::new();for(i,item)in arr.iter().enumerate(){let map=item.as_object().ok_or_else(||format!("JSON row {} is not an object",i+1))?;headers.extend(map.keys().cloned())}
        let headers:Vec<String>=headers.into_iter().collect();let mut lines=Vec::new();
        if !headers.is_empty(){lines.push(headers.iter().map(|h|csv_escape(h)).collect::<Vec<_>>().join(","))}
        for item in &arr{let map=item.as_object().expect("validated as object");let line=headers.iter().map(|h|{let s=match map.get(h){None|Some(Value::Null)=>String::new(),Some(Value::String(s))=>s.clone(),Some(v)=>v.to_string()};csv_escape(&s)}).collect::<Vec<_>>().join(",");lines.push(line)}
        cx.set_variable(txt(&cx.cfg,"saveAs"),Value::String(lines.join("\r\n")))?;(cx.log)("info",format!("Converted {} JSON objects to CSV",arr.len()));Ok(Outcome::Satisfied)
    }
}
struct TransformValue;
fn transform_spec()->Value{
    action_spec("Transform Value","variable","Performs a common string, JSON or numeric transformation and saves the result.",vec![
        json!({"key":"operation","label":"Operation","type":"select","default":"trim","options":[["trim","Trim whitespace"],["upper","Uppercase"],["lower","Lowercase"],["replace","Replace literal text"],["split","Split into a JSON array"],["number_add","Add number"],["number_subtract","Subtract number"],["number_multiply","Multiply by number"],["number_divide","Divide by number"],["round","Round number"],["json_parse","Parse JSON into typed value"],["unix_now","Current Unix timestamp in seconds"]]}),
        json!({"key":"input","label":"Input or variable template","type":"textarea","rows":4,"maxLen":32768,"default":"","show":{"key":"operation","ne":"unix_now"}}),
        json!({"key":"find","label":"Text to find","type":"text","maxLen":8192,"default":"","show":{"key":"operation","eq":"replace"}}),
        json!({"key":"replacement","label":"Replacement","type":"text","maxLen":8192,"default":"","show":{"key":"operation","eq":"replace"}}),
        json!({"key":"separator","label":"Separator","type":"text","maxLen":256,"default":",","show":{"key":"operation","eq":"split"}}),
        json!({"key":"operand","label":"Number","type":"number","min":-1000000000000.0,"max":1000000000000.0,"step":0.1,"default":1,"show":{"key":"operation","in":["number_add","number_subtract","number_multiply","number_divide"]}}),
        json!({"key":"digits","label":"Decimal places","type":"number","min":0,"max":12,"step":1,"default":2,"show":{"key":"operation","eq":"round"}}),
        json!({"key":"saveAs","label":"Save result to variable","type":"text","required":true,"maxLen":64}),json!({"preset":"onFail"})
    ],"Transform value","Split produces an array, numeric operations produce a JSON number, JSON parsing preserves typed values, and unix_now returns epoch seconds.");
}
#[async_trait]
impl Block for TransformValue{
    fn title(&self)-> &'static str {"Transform Value"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(transform_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{
        spec::validate(&transform_spec(),c)?;validate_variable(c,"saveAs",true)?;
        let op=txt(c,"operation");if op=="replace"&&txt(c,"find").is_empty(){return Err("Text to find is required".into())}
        if op=="split"&&txt(c,"separator").is_empty(){return Err("Separator must not be empty".into())}
        if op=="number_divide"&&c.get("operand").and_then(Value::as_f64)==Some(0.0){return Err("Cannot divide by zero".into())}
        Ok(())
    }
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;let op=txt(&cx.cfg,"operation");
        let result=if op=="unix_now"{use std::time::{SystemTime,UNIX_EPOCH};Value::from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())}else{
            let input=templ(cx,&txt(&cx.cfg,"input"))?;
            match op.as_str(){
                "trim"=>Value::String(input.trim().to_string()),"upper"=>Value::String(input.to_uppercase()),"lower"=>Value::String(input.to_lowercase()),
                "replace"=>Value::String(input.replace(&txt(&cx.cfg,"find"),&txt(&cx.cfg,"replacement"))),
                "split"=>Value::Array(input.split(&txt(&cx.cfg,"separator")).map(|s|Value::String(s.to_string())).collect()),
                "json_parse"=>serde_json::from_str::<Value>(&input).map_err(|e|format!("Invalid JSON input: {e}"))?,
                "number_add"|"number_subtract"|"number_multiply"|"number_divide"|"round"=>{
                    let n=input.trim().parse::<f64>().map_err(|_|"Input must be a number".to_string())?;
                    let operand=cx.cfg.get("operand").and_then(Value::as_f64).unwrap_or(1.0);
                    let val=match op.as_str(){"number_add"=>n+operand,"number_subtract"=>n-operand,"number_multiply"=>n*operand,"number_divide"=>n/operand,_=>{let digits=cx.cfg.get("digits").and_then(Value::as_u64).unwrap_or(2).min(12) as i32;let scale=10_f64.powi(digits);(n*scale).round()/scale}};
                    if !val.is_finite(){return Err("Numeric transform produced a non-finite value".into())}
                    serde_json::Number::from_f64(val).map(Value::Number).ok_or_else(||"Could not represent numeric result".to_string())?
                }
                other=>return Err(format!("Unsupported transform operation: {other}"))
            }
        };
        cx.set_variable(txt(&cx.cfg,"saveAs"),result)?;Ok(Outcome::Satisfied)
    }
}

pub(super) fn register_into(m:&mut std::collections::HashMap<&'static str,Box<dyn Block>>){
    m.insert("cond.ui_element",Box::new(UiElementCondition));
    m.insert("act.ui_find",Box::new(UiFind));m.insert("act.ui_click",Box::new(UiClick));
    m.insert("act.ui_set_value",Box::new(UiSetValue));m.insert("act.ui_select_item",Box::new(UiSelectItem));
    m.insert("act.json_extract",Box::new(JsonExtract));m.insert("act.regex_extract",Box::new(RegexExtract));
    m.insert("act.csv_to_json",Box::new(CsvToJson));m.insert("act.json_to_csv",Box::new(JsonToCsv));
    m.insert("act.transform_value",Box::new(TransformValue));
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn csv_parser_supports_quotes_newlines_and_crlf(){
        assert_eq!(parse_csv("name,value\r\nalice,\"hello, world\"\r\nbob,\"two\nlines\"\r\n").unwrap(),vec![vec!["name","value"],vec!["alice","hello, world"],vec!["bob","two\nlines"]]);
        assert_eq!(csv_escape("a,\"b"),"\"a,\"\"b\"");
    }
    #[test]
    fn csv_parser_rejects_malformed_quotes(){
        assert!(parse_csv("a,b\n\"not closed,x").is_err());assert!(parse_csv("a,b\nab\"cd,ef").is_err());
    }
    #[test]
    fn ui_selector_requires_a_window_and_a_control(){
        assert!(validate_ui_selector(&json!({"processName":"notepad","controlName":"Save"})).is_ok());
        assert!(validate_ui_selector(&json!({"processName":"notepad"})).is_err());
        assert!(validate_ui_selector(&json!({"controlName":"Save"})).is_err());
    }
    #[test]
    fn valid_variable_names_are_bounded_and_explicit(){
        assert!(valid_name("result_1"));assert!(valid_name("file.path"));assert!(!valid_name("1bad"));assert!(!valid_name("bad name"));
    }
}
