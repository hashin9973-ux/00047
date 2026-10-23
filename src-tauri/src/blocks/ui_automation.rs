//! Windows UI Automation blocks. These query controls through UI Automation properties,
//! falling back to coordinate input only when explicitly chosen by the workflow author.
use super::{extra, spec, Block, Cat, NodeCx, Outcome};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

fn text(c:&Value,k:&str)->String{c.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()}
fn raw(c:&Value,k:&str)->String{c.get(k).and_then(Value::as_str).unwrap_or("").to_string()}
fn ps_quote(s:&str)->String{format!("'{}'",s.replace('\0',"").replace('\'',"''"))}
fn no_nul(c:&Value,keys:&[&str])->Result<(),String>{for k in keys{if raw(c,k).contains('\0'){return Err(format!("{k} must not contain NUL characters"))}}Ok(())}
fn ui_fields()->Vec<Value>{
    vec![
      json!({"key":"automationId","label":"Automation ID","type":"text","maxLen":1024}),
      json!({"key":"elementName","label":"Element name (exact)","type":"text","maxLen":1024}),
      json!({"key":"controlType","label":"Control type","type":"select","default":"Any","options":[["Any","Any control"],["Button","Button"],["Edit","Text input"],["Text","Text label"],["CheckBox","Checkbox"],["ComboBox","Combo box"],["ListItem","List item"],["MenuItem","Menu item"],["RadioButton","Radio button"],["Window","Window"],["TabItem","Tab item"]]}),
      json!({"key":"processName","label":"Owning process name (optional)","type":"text","maxLen":260,"placeholder":"notepad.exe"}),
      json!({"key":"windowTitle","label":"Containing window title contains","type":"text","maxLen":1024}),
      json!({"key":"enabledState","label":"Enabled state","type":"select","default":"enabled","options":[["enabled","Must be enabled"],["disabled","Must be disabled"],["any","Either state"]]}),
    ]
}
fn condition_spec()->Value{
 let mut f=ui_fields();
 f.extend(extra::behavior_fields());
 json!({"category":"condition","title":"UI Element Condition","icon":"scan-search","description":"Finds a desktop control by Automation ID, accessible name, control type, process, or containing window title.","fields":f,"summary":"UI element {elementName}","help":"Uses Windows UI Automation rather than screen coordinates. Supply at least one stable selector (Automation ID, element name, or control type plus a process/window constraint). Unavailable, elevated, remote, and custom-drawn controls may not expose UI Automation."})
}
fn action_spec()->Value{
 let mut f=ui_fields();
 f.insert(0,json!({"key":"operation","label":"Operation","type":"select","default":"invoke","options":[["invoke","Invoke / click"],["focus","Set keyboard focus"],["set_value","Set text value"],["toggle","Toggle checkbox"],["select","Select item"]]}));
 f.insert(4,json!({"key":"value","label":"Value to set (set text only)","type":"textarea","rows":2,"maxLen":8192,"show":{"key":"operation","eq":"set_value"}}));
 f.push(json!({"preset":"onFail"}));
 json!({"category":"action","title":"UI Automation Action","icon":"mouse-pointer-click","description":"Invokes or manipulates a control selected by UI Automation properties.","fields":f,"summary":"UI {operation}: {elementName}","help":"Control selection must match exactly one element to avoid interacting with the wrong window. This is more resilient than raw coordinates but depends on application accessibility support."})
}
fn selectors(c:&Value)->Result<(String,String,String,String,String,String),String>{
 let id=text(c,"automationId");let name=text(c,"elementName");let ctl=if text(c,"controlType").is_empty(){"Any".to_string()}else{text(c,"controlType")};let proc=text(c,"processName");let title=text(c,"windowTitle");let enabled=if text(c,"enabledState").is_empty(){"enabled".to_string()}else{text(c,"enabledState")};
 if id.is_empty()&&name.is_empty()&&ctl=="Any"&&proc.is_empty()&&title.is_empty(){return Err("Specify at least one UI selector".into())}
 if !proc.is_empty() && (proc.contains('/')||proc.contains('\\')) {return Err("Process name must be a name such as notepad.exe, not a path".into())}
 if !matches!(enabled.as_str(),"enabled"|"disabled"|"any"){return Err("Unknown enabled state filter".into())}
 Ok((id,name,ctl,proc,title,enabled))
}
fn make_query_script(c:&Value, operation:&str)->Result<String,String>{
 let (id,name,ctl,proc,title,enabled)=selectors(c)?;
 let value=raw(c,"value");
 let id=ps_quote(&id);let name=ps_quote(&name);let ctl=ps_quote(&ctl);let proc=ps_quote(&proc);let title=ps_quote(&title);let enabled=ps_quote(&enabled);let value=ps_quote(&value);let operation=ps_quote(operation);
 Ok(format!(r#"$ErrorActionPreference='Stop'; Add-Type -AssemblyName UIAutomationClient; Add-Type -AssemblyName UIAutomationTypes;
$aid={id};$ename={name};$ct={ctl};$pn={proc};$wt={title};$enabled={enabled};$op={operation};$val={value};
$conditions=New-Object 'System.Collections.Generic.List[System.Windows.Automation.Condition]';
if($aid -ne ''){{$conditions.Add(([System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty,$aid)))}}
if($ename -ne ''){{$conditions.Add(([System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty,$ename)))}}
if($ct -ne 'Any'){{$map=@{{Button=[System.Windows.Automation.ControlType]::Button;Edit=[System.Windows.Automation.ControlType]::Edit;Text=[System.Windows.Automation.ControlType]::Text;CheckBox=[System.Windows.Automation.ControlType]::CheckBox;ComboBox=[System.Windows.Automation.ControlType]::ComboBox;ListItem=[System.Windows.Automation.ControlType]::ListItem;MenuItem=[System.Windows.Automation.ControlType]::MenuItem;RadioButton=[System.Windows.Automation.ControlType]::RadioButton;Window=[System.Windows.Automation.ControlType]::Window;TabItem=[System.Windows.Automation.ControlType]::TabItem}};$conditions.Add(([System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,$map[$ct]))) }}
if($conditions.Count -eq 0){{$base=[System.Windows.Automation.Condition]::TrueCondition}}elseif($conditions.Count -eq 1){{$base=$conditions[0]}}else{{$base=[System.Windows.Automation.AndCondition]::new($conditions.ToArray())}}
$root=[System.Windows.Automation.AutomationElement]::RootElement;$all=$root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$base);$hits=New-Object System.Collections.Generic.List[System.Windows.Automation.AutomationElement];
foreach($el in $all){{try{{$cur=$el.Current;if($enabled -eq 'enabled' -and -not $cur.IsEnabled){{continue}};if($enabled -eq 'disabled' -and $cur.IsEnabled){{continue}};if($pn -ne ''){{$p=Get-Process -Id $cur.ProcessId -ErrorAction SilentlyContinue;if($null -eq $p -or $p.ProcessName -ine [IO.Path]::GetFileNameWithoutExtension($pn)){{continue}}}};
if($wt -ne ''){{$walker=[System.Windows.Automation.TreeWalker]::ControlViewWalker;$w=$el;$windowName='';for($i=0;$i -lt 32 -and $null -ne $w;$i++){{$iftype=$w.Current.ControlType;if($iftype -eq [System.Windows.Automation.ControlType]::Window){{$windowName=$w.Current.Name;break}};$w=$walker.GetParent($w)}};if($windowName -notlike ('*'+$wt+'*')){{continue}}}};
$hits.Add($el)}}catch{{}}}}
if($hits.Count -ne 1){{[pscustomobject]@{{ok=$false;count=$hits.Count;name='';value='';enabled=$false;processId=0}}|ConvertTo-Json -Compress;exit 0}}
$el=$hits[0];$cur=$el.Current;$ok=$true;try{{switch($op){{
'query'{{}}
'invoke'{{$pat=$el.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern);$pat.Invoke()}}
'focus'{{$el.SetFocus()}}
'set_value'{{$pat=$el.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern);$pat.SetValue($val)}}
'toggle'{{$pat=$el.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern);$pat.Toggle()}}
'select'{{$pat=$el.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern);$pat.Select()}}
default{{throw 'Unsupported UI operation'}}
}}}}catch{{$ok=$false;$err=$_.Exception.Message}}
[pscustomobject]@{{ok=$ok;count=$hits.Count;name=$cur.Name;value=$cur.AutomationId;enabled=$cur.IsEnabled;processId=$cur.ProcessId;error=$err}}|ConvertTo-Json -Compress"#,
 id=id,name=name,ctl=ctl,proc=proc,title=title,operation=operation,value=value))
}
fn parse_result(out:&str)->Result<Value,String>{serde_json::from_str(out.trim()).map_err(|e|format!("Could not parse UI Automation result: {e}"))}
struct UiElementCondition;
#[async_trait]
impl Block for UiElementCondition{
 fn title(&self)-> &'static str{"UI Element Condition"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(condition_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&condition_spec(),c)?;no_nul(c,&["automationId","elementName","processName","windowTitle"])?;selectors(c).map(|_|())?;extra::wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;
  let script=make_query_script(&cx.cfg,"query")?;
  let(behavior,poll,stable,timeout)=extra::wait_settings(&cx.cfg)?;
  extra::poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{
    let out=extra::powershell(cx,&script).await?;
    let v=parse_result(&out)?;
    let count=v.get("count").and_then(Value::as_u64).unwrap_or(0);
    (cx.report)("waiting",None,format!("UI selector matched {count} enabled control(s)"));
    Ok(v.get("ok").and_then(Value::as_bool).unwrap_or(false))
  }) as extra::Probe<'_>).await
 }
}
struct UiAutomationAction;
#[async_trait]
impl Block for UiAutomationAction{
 fn title(&self)-> &'static str{"UI Automation Action"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}
 fn spec(&self)->Option<Value>{Some(action_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&action_spec(),c)?;no_nul(c,&["automationId","elementName","processName","windowTitle","value"])?;selectors(c).map(|_|())?;let op=text(c,"operation");if !matches!(op.as_str(),"invoke"|"focus"|"set_value"|"toggle"|"select"){return Err("Unknown UI Automation operation".into())}if op=="set_value"&&raw(c,"value").is_empty(){return Err("Value is required for Set text value".into())}Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
  self.validate(&cx.cfg)?;
  let operation=text(&cx.cfg,"operation");let script=make_query_script(&cx.cfg,&operation)?;
  let out=extra::powershell(cx,&script).await?;let v=parse_result(&out)?;
  if v.get("count").and_then(Value::as_u64)!=Some(1){return Err("UI selector must match exactly one enabled control".into())}
  if !v.get("ok").and_then(Value::as_bool).unwrap_or(false){return Err(format!("UI Automation operation failed: {}",v.get("error").and_then(Value::as_str).unwrap_or("control pattern not supported")))}
  (cx.log)("info",format!("UI Automation {} completed for {}",operation,v.get("name").and_then(Value::as_str).unwrap_or("selected control")));
  Ok(Outcome::Satisfied)
 }
}
pub(super) fn register_into(m:&mut std::collections::HashMap<&'static str,Box<dyn Block>>){
 m.insert("cond.ui_element",Box::new(UiElementCondition));
 m.insert("act.ui_interact",Box::new(UiAutomationAction));
}
#[cfg(test)]
mod tests{
 use super::*;
 #[test]fn selector_requires_a_target(){assert!(selectors(&json!({"automationId":"saveBtn"})).is_ok());assert!(selectors(&json!({"processName":"notepad.exe"})).is_ok());assert!(selectors(&json!({})).is_err());}
 #[test]fn enabled_filter_is_validated(){assert!(selectors(&json!({"automationId":"saveBtn","enabledState":"disabled"})).is_ok());assert!(selectors(&json!({"automationId":"saveBtn","enabledState":"unknown"})).is_err());}
 #[test]fn process_selector_rejects_paths(){assert!(selectors(&json!({"processName":"notepad.exe"})).is_ok());assert!(selectors(&json!({"processName":"C:\\\\Windows\\\\notepad.exe"})).is_err());}
}
