//! UI automation, event notifications, data processing, and subflow primitives.
use super::{extra, rng, spec, text, Block, Cat, NodeCx, Outcome};
use async_trait::async_trait;
use regex::{Regex, RegexBuilder};
use serde_json::{json, Map, Value};
use std::{fs, path::Path};

fn raw(c:&Value,k:&str)->String{c.get(k).and_then(Value::as_str).unwrap_or("").to_string()}
fn tpl(cx:&NodeCx,k:&str)->Result<String,String>{cx.expand_template(&raw(&cx.cfg,k))}
fn valid_name(s:&str)->bool{!s.is_empty()&&s.len()<=64&&s.chars().next().is_some_and(|c|c.is_ascii_alphabetic()||c=='_')&&s.chars().all(|c|c.is_ascii_alphanumeric()||matches!(c,'_'|'-'|'.'))}
fn vname(c:&Value,k:&str)->Result<String,String>{let n=text(c,k);if valid_name(&n){Ok(n)}else{Err(format!("{k} must be a valid workflow-variable name"))}}
fn no_nul(c:&Value,keys:&[&str])->Result<(),String>{for k in keys{if raw(c,k).contains('\0'){return Err(format!("{k} must not contain NUL"))}}Ok(())}
fn action_spec(title:&str,icon:&str,description:&str,fields:Vec<Value>,summary:&str,help:&str)->Value{json!({"category":"action","title":title,"icon":icon,"description":description,"fields":fields,"summary":summary,"help":help})}
fn cond_spec(title:&str,icon:&str,description:&str,fields:Vec<Value>,summary:&str,help:&str)->Value{json!({"category":"condition","title":title,"icon":icon,"description":description,"fields":fields,"summary":summary,"help":help})}
fn onfail()->Value{json!({"preset":"onFail"})}
fn to_text(v:&Value)->String{match v{Value::String(s)=>s.clone(),Value::Null=>String::new(),_=>v.to_string()}}
fn path_get<'a>(v:&'a Value,p:&str)->Option<&'a Value>{let mut cur=v;if p.is_empty(){return Some(cur)}for part in p.split('.') {cur=if let Ok(i)=part.parse::<usize>(){cur.get(i)?}else{cur.get(part)?}}Some(cur)}

// CSV parser with quoted fields, escaped quotes, limits, and no external code execution.
fn csv_parse(input:&str,delim:char,max:usize)->Result<Vec<Vec<String>>,String>{
 let mut rows=Vec::new();let mut row=Vec::new();let mut cell=String::new();let mut chars=input.chars().peekable();let mut quoted=false;
 while let Some(ch)=chars.next(){if quoted{if ch=='"'{if chars.peek()==Some(&'"'){chars.next();cell.push('"')}else{quoted=false}}else{cell.push(ch)}}else{match ch{
 '"' if cell.is_empty()=>quoted=true,c if c==delim=>row.push(std::mem::take(&mut cell)),
 '\n'|'\r'=>{if ch=='\r'&&chars.peek()==Some(&'\n'){chars.next();}row.push(std::mem::take(&mut cell));if row.iter().any(|x|!x.is_empty()){rows.push(std::mem::take(&mut row))}else{row.clear()}if rows.len()>max{return Err(format!("CSV exceeds {max} rows"))}},
 _=>cell.push(ch)}}}
 if quoted{return Err("CSV has an unterminated quoted field".into())}
 if !cell.is_empty()||!row.is_empty(){row.push(cell);rows.push(row)}
 if rows.len()>max{return Err(format!("CSV exceeds {max} rows"))}Ok(rows)
}
fn csv_quote(s:&str,d:char)->String{if s.contains(d)||s.contains('"')||s.contains('\r')||s.contains('\n'){format!("\"{}\"",s.replace('"',"\"\""))}else{s.to_string()}}
fn csv_rows(rows:Vec<Vec<String>>,header:bool)->Value{
 if rows.is_empty(){return Value::Array(vec![])}let width=rows.iter().map(Vec::len).max().unwrap_or(0);
 let (names,data)=if header{let mut i=rows.into_iter();(i.next().unwrap_or_default(),i.collect::<Vec<_>>())}else{((0..width).map(|i|format!("column_{}",i+1)).collect(),rows)};
 let mut seen=std::collections::HashMap::<String,usize>::new();
 let names:Vec<String>=(0..width).map(|i|{let base=names.get(i).filter(|s|!s.trim().is_empty()).cloned().unwrap_or_else(||format!("column_{}",i+1));let n=seen.entry(base.clone()).or_insert(0);*n+=1;if *n==1{base}else{format!("{base}_{}",*n)}}).collect();
 Value::Array(data.into_iter().map(|r|{let mut m=Map::new();for(i,n)in names.iter().enumerate(){m.insert(n.clone(),json!(r.get(i).cloned().unwrap_or_default()));}Value::Object(m)}).collect())
}

struct JsonParse;
fn json_parse_spec()->Value{action_spec("Parse JSON","file-json","Parses text into a typed workflow variable.",vec![
 json!({"key":"jsonText","label":"JSON text / template","type":"textarea","rows":4,"required":true,"maxLen":1000000}),
 json!({"key":"saveAs","label":"Save parsed value as","type":"text","required":true,"maxLen":64}),onfail()
],"Parse JSON → {saveAs}","Preserves JSON types. Content is not written to logs.")}
#[async_trait] impl Block for JsonParse{
 fn title(&self)-> &'static str{"Parse JSON"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(json_parse_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&json_parse_spec(),c)?;no_nul(c,&["jsonText","saveAs"])?;vname(c,"saveAs").map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let s=tpl(cx,"jsonText")?;let v=serde_json::from_str(&s).map_err(|e|format!("Invalid JSON: {e}"))?;cx.set_variable(vname(&cx.cfg,"saveAs")?,v)?;Ok(Outcome::Satisfied)}
}

struct JsonGet;
fn json_get_spec()->Value{action_spec("Get JSON Value","file-json","Extracts a property or array index from a JSON variable.",vec![
 json!({"key":"sourceVariable","label":"Source JSON variable","type":"text","required":true,"maxLen":64}),
 json!({"key":"path","label":"Dot-separated path","type":"text","placeholder":"data.items.0.name","maxLen":1000}),
 json!({"key":"saveAs","label":"Save result as","type":"text","required":true,"maxLen":64}),onfail()
],"Get {path} → {saveAs}","Leave the path empty to copy the whole value. Numeric path parts select array items.")}
#[async_trait]impl Block for JsonGet{
 fn title(&self)-> &'static str{"Get JSON Value"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(json_get_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&json_get_spec(),c)?;vname(c,"sourceVariable")?;vname(c,"saveAs")?;no_nul(c,&["path"])}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let n=vname(&cx.cfg,"sourceVariable")?;let v=cx.get_variable(&n).ok_or_else(||format!("Variable '{n}' is not set"))?;let p=tpl(cx,"path")?;let v=path_get(&v,&p).cloned().ok_or_else(||format!("JSON path '{p}' not found"))?;cx.set_variable(vname(&cx.cfg,"saveAs")?,v)?;Ok(Outcome::Satisfied)}
}

struct RegexExtract;
fn rx_extract_spec()->Value{action_spec("Regex Extract","regex","Captures text using a linear-time regular expression.",vec![
 json!({"key":"source","label":"Source text","type":"textarea","rows":4,"required":true,"maxLen":500000}),
 json!({"key":"pattern","label":"Regex pattern","type":"text","required":true,"maxLen":4000}),
 json!({"key":"group","label":"Capture group index (0 = entire match)","type":"number","min":0,"max":100,"step":1,"default":0}),
 json!({"key":"mode","label":"Result","type":"select","default":"first","options":[["first","First match"],["all","All matches"]]}),
 json!({"key":"saveAs","label":"Save result as","type":"text","required":true,"maxLen":64}),onfail()
],"Regex Extract → {saveAs}","All mode stores a JSON array of capture strings. Look-around and backreferences are not supported.")}
#[async_trait]impl Block for RegexExtract{
 fn title(&self)-> &'static str{"Regex Extract"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(rx_extract_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&rx_extract_spec(),c)?;no_nul(c,&["source","pattern"])?;vname(c,"saveAs")?;let g=rng(c,"group",0.0,0.0,100.0,"Capture group")?;if g.fract()!=0.0{return Err("Capture group must be a whole number".into())};if !["first","all"].contains(&text(c,"mode").as_str()){return Err("Unknown result mode".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let s=tpl(cx,"source")?;let p=tpl(cx,"pattern")?;let g=rng(&cx.cfg,"group",0.0,0.0,100.0,"Capture group")? as usize;let re=RegexBuilder::new(&p).size_limit(1<<20).build().map_err(|e|format!("Invalid regex: {e}"))?;if g>=re.captures_len(){return Err("Capture group does not exist".into())};let out=if text(&cx.cfg,"mode")=="all"{json!(re.captures_iter(&s).take(10000).map(|c|c.get(g).map(|m|m.as_str()).unwrap_or("").to_string()).collect::<Vec<_>>())}else{json!(re.captures(&s).and_then(|c|c.get(g)).map(|m|m.as_str()).unwrap_or(""))};cx.set_variable(vname(&cx.cfg,"saveAs")?,out)?;Ok(Outcome::Satisfied)}
}
struct RegexReplace;
fn rx_replace_spec()->Value{action_spec("Regex Replace","regex","Replaces text using Rust's linear-time regular expression engine.",vec![
 json!({"key":"source","label":"Source text","type":"textarea","rows":4,"required":true,"maxLen":500000}),
 json!({"key":"pattern","label":"Regex pattern","type":"text","required":true,"maxLen":4000}),
 json!({"key":"replacement","label":"Replacement","type":"text","maxLen":10000}),
 json!({"key":"limit","label":"Replacement limit (0 = all)","type":"number","min":0,"max":100000,"step":1,"default":0}),
 json!({"key":"saveAs","label":"Save result as","type":"text","required":true,"maxLen":64}),onfail()
],"Regex Replace → {saveAs}","Supports numbered capture references like $1. Avoids catastrophic backtracking.")}
#[async_trait]impl Block for RegexReplace{
 fn title(&self)-> &'static str{"Regex Replace"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(rx_replace_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&rx_replace_spec(),c)?;no_nul(c,&["source","pattern","replacement"])?;vname(c,"saveAs")?;let n=rng(c,"limit",0.0,0.0,100000.0,"Replacement limit")?;if n.fract()!=0.0{return Err("Limit must be a whole number".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let s=tpl(cx,"source")?;let p=tpl(cx,"pattern")?;let rep=tpl(cx,"replacement")?;let lim=rng(&cx.cfg,"limit",0.0,0.0,100000.0,"Replacement limit")? as usize;let re=RegexBuilder::new(&p).size_limit(1<<20).build().map_err(|e|format!("Invalid regex: {e}"))?;let v=if lim==0{re.replace_all(&s,rep.as_str()).into_owned()}else{re.replacen(&s,lim,rep.as_str()).into_owned()};cx.set_variable(vname(&cx.cfg,"saveAs")?,json!(v))?;Ok(Outcome::Satisfied)}
}

struct CsvRead;
fn csv_read_spec()->Value{action_spec("Read CSV","file-spreadsheet","Reads bounded UTF-8 CSV into an array of objects.",vec![
 json!({"key":"path","label":"CSV file","type":"file","required":true,"maxLen":4000}),
 json!({"key":"delimiter","label":"Delimiter","type":"select","default":",","options":[[",","Comma"],[";","Semicolon"],["\\t","Tab"]]}),
 json!({"key":"hasHeader","label":"First row is header","type":"bool","default":true}),
 json!({"key":"maxRows","label":"Maximum data rows","type":"number","min":1,"max":100000,"step":1,"default":10000}),
 json!({"key":"saveAs","label":"Save rows as","type":"text","required":true,"maxLen":64}),onfail()
],"Read CSV → {saveAs}","CSV file cap 16 MiB, 100,000 rows, and 512 columns. UTF-8 required.")}
#[async_trait]impl Block for CsvRead{
 fn title(&self)-> &'static str{"Read CSV"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(csv_read_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_read_spec(),c)?;no_nul(c,&["path"])?;vname(c,"saveAs")?;let n=rng(c,"maxRows",10000.0,1.0,100000.0,"Maximum rows")?;if n.fract()!=0.0{return Err("Maximum rows must be a whole number".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let p=tpl(cx,"path")?;let b=fs::read(&p).map_err(|e|format!("Read CSV: {e}"))?;if b.len()>16*1024*1024{return Err("CSV exceeds 16 MiB".into())};let s=String::from_utf8(b).map_err(|_|"CSV must be UTF-8")?;let d=text(&cx.cfg,"delimiter");let del=if d=="\\t"||d=="\t"{'\t'}else{d.chars().next().ok_or("Delimiter required")?};let max=rng(&cx.cfg,"maxRows",10000.0,1.0,100000.0,"Maximum rows")? as usize;let rows=csv_parse(&s,del,max+2)?;if rows.iter().any(|r|r.len()>512){return Err("CSV exceeds 512 columns".into())};let val=csv_rows(rows,cx.cfg.get("hasHeader").and_then(Value::as_bool).unwrap_or(true));if val.as_array().is_some_and(|a|a.len()>max){return Err(format!("CSV exceeds {max} data rows"))};cx.set_variable(vname(&cx.cfg,"saveAs")?,val)?;Ok(Outcome::Satisfied)}
}
struct CsvWrite;
fn csv_write_spec()->Value{action_spec("Write CSV","file-spreadsheet","Writes JSON-object arrays as CSV with explicit collision policy.",vec![
 json!({"key":"sourceVariable","label":"Rows variable (array of objects)","type":"text","required":true,"maxLen":64}),
 json!({"key":"path","label":"Destination CSV","type":"file","required":true,"maxLen":4000}),
 json!({"key":"collision","label":"If file exists","type":"select","default":"fail","options":[["fail","Fail"],["replace","Replace atomically"],["append","Append rows"]]}),onfail()
],"Write CSV from {sourceVariable}","Object keys from the first row define a stable, alphabetically sorted header. Each row must be an object.")}
#[async_trait]impl Block for CsvWrite{
 fn title(&self)-> &'static str{"Write CSV"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(csv_write_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_write_spec(),c)?;no_nul(c,&["path"])?;vname(c,"sourceVariable")?;if !["fail","replace","append"].contains(&text(c,"collision").as_str()){return Err("Unknown collision policy".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let s=vname(&cx.cfg,"sourceVariable")?;let rows=cx.get_variable(&s).ok_or_else(||format!("Variable '{s}' is not set"))?;let arr=rows.as_array().ok_or("CSV input must be an array")?;if arr.len()>100000{return Err("CSV input exceeds 100,000 rows".into())};if arr.iter().any(|v|!v.is_object()){return Err("Every CSV row must be an object".into())};let mut headers=arr.first().and_then(Value::as_object).map(|o|o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();headers.sort();let p=tpl(cx,"path")?;let exists=Path::new(&p).exists();let collision=text(&cx.cfg,"collision");if exists&&collision=="fail"{return Err("Destination already exists".into())};let mut out=String::new();if !(exists&&collision=="append"){out.push_str(&headers.iter().map(|h|csv_quote(h,',')).collect::<Vec<_>>().join(","));out.push_str("\r\n")};for r in arr{let o=r.as_object().unwrap();out.push_str(&headers.iter().map(|h|csv_quote(&to_text(o.get(h).unwrap_or(&Value::Null)),',')).collect::<Vec<_>>().join(","));out.push_str("\r\n")};if collision=="append"&&exists{if fs::metadata(&p).map_err(|e|e.to_string())?.len()>32*1024*1024{return Err("Existing CSV exceeds 32 MiB".into())};use std::io::Write;let mut f=fs::OpenOptions::new().append(true).open(&p).map_err(|e|e.to_string())?;f.write_all(out.as_bytes()).map_err(|e|format!("Append CSV: {e}"))?;}else{let path=Path::new(&p);if let Some(parent)=path.parent(){fs::create_dir_all(parent).map_err(|e|e.to_string())?};let tmp=path.with_extension("csv.tmp");fs::write(&tmp,out.as_bytes()).map_err(|e|e.to_string())?;if exists{let bak=path.with_extension("csv.bak");if bak.exists(){fs::remove_file(&bak).map_err(|e|e.to_string())?};fs::rename(path,&bak).map_err(|e|e.to_string())?;if let Err(e)=fs::rename(&tmp,path){let _=fs::rename(&bak,path);return Err(format!("CSV replacement failed; prior file restored: {e}"))};let _=fs::remove_file(bak);}else if let Err(e)=fs::rename(&tmp,path){let _=fs::remove_file(tmp);return Err(e.to_string())}}
Ok(Outcome::Satisfied)}
}

struct Transform;
fn transform_spec()->Value{action_spec("Transform Value","settings","Transforms strings, JSON lists, and numeric values without evaluating scripts.",vec![
 json!({"key":"sourceVariable","label":"Source variable (optional)","type":"text","maxLen":64}),
 json!({"key":"value","label":"Literal or template","type":"textarea","rows":2,"maxLen":50000}),
 json!({"key":"operation","label":"Operation","type":"select","default":"trim","options":[["trim","Trim"],["lower","Lowercase"],["upper","Uppercase"],["split","Split to array"],["join","Join array"],["number","Parse number"],["add","Add"],["subtract","Subtract"],["multiply","Multiply"],["divide","Divide"]]}),
 json!({"key":"argument","label":"Delimiter / second number","type":"text","maxLen":1000}),
 json!({"key":"saveAs","label":"Save result as","type":"text","required":true,"maxLen":64}),onfail()
],"Transform → {saveAs}","No expression evaluation. Division by zero and non-finite results are rejected.")}
#[async_trait]impl Block for Transform{
 fn title(&self)-> &'static str{"Transform Value"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(transform_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&transform_spec(),c)?;no_nul(c,&["sourceVariable","value","argument"])?;if !text(c,"sourceVariable").is_empty(){vname(c,"sourceVariable")?};vname(c,"saveAs")?;if !["trim","lower","upper","split","join","number","add","subtract","multiply","divide"].contains(&text(c,"operation").as_str()){return Err("Unknown transform".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let src=text(&cx.cfg,"sourceVariable");let v=if src.is_empty(){json!(tpl(cx,"value")?)}else{cx.get_variable(&src).ok_or_else(||format!("Variable '{src}' is not set"))?};let op=text(&cx.cfg,"operation");let a=tpl(cx,"argument")?;let out=match op.as_str(){"trim"=>json!(to_text(&v).trim()),"lower"=>json!(to_text(&v).to_lowercase()),"upper"=>json!(to_text(&v).to_uppercase()),"split"=>json!(to_text(&v).split(&a).map(str::to_string).collect::<Vec<_>>()),"join"=>json!(v.as_array().ok_or("Join expects an array variable")?.iter().map(to_text).collect::<Vec<_>>().join(&a)),"number"=>json!(to_text(&v).parse::<f64>().map_err(|_|"Value is not numeric")?),"add"|"subtract"|"multiply"|"divide"=>{let x=v.as_f64().or_else(||to_text(&v).parse::<f64>().ok()).ok_or("Source value is not numeric")?;let y=a.parse::<f64>().map_err(|_|"Numeric argument is invalid")?;let z=match op.as_str(){"add"=>x+y,"subtract"=>x-y,"multiply"=>x*y,"divide" if y!=0.0=>x/y,"divide"=>return Err("Division by zero".into()),_=>x};if !z.is_finite(){return Err("Numeric result is not finite".into())};json!(z)},_=>return Err("Unknown transform".into())};cx.set_variable(vname(&cx.cfg,"saveAs")?,out)?;Ok(Outcome::Satisfied)}
}

struct SwitchCase;
fn switch_spec()->Value{cond_spec("Switch / Case","layers","Compares a workflow variable to a configured case.",vec![
 json!({"key":"sourceVariable","label":"Variable","type":"text","required":true,"maxLen":64}),
 json!({"key":"mode","label":"Match","type":"select","default":"equals","options":[["equals","Equals"],["not_equals","Does not equal"],["contains","Contains"],["starts_with","Starts with"],["ends_with","Ends with"]]}),
 json!({"key":"expected","label":"Expected value","type":"text","maxLen":10000}),onfail()
],"Case {expected}","Use several Switch / Case conditions against the same variable. Matching is case-sensitive.")}
#[async_trait]impl Block for SwitchCase{
 fn title(&self)-> &'static str{"Switch / Case"}fn category(&self)->Cat{Cat::Condition}fn spec(&self)->Option<Value>{Some(switch_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&switch_spec(),c)?;vname(c,"sourceVariable")?;no_nul(c,&["expected"])?;Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let n=vname(&cx.cfg,"sourceVariable")?;let v=cx.get_variable(&n).ok_or_else(||format!("Variable '{n}' is not set"))?;let a=to_text(&v);let b=tpl(cx,"expected")?;let yes=match text(&cx.cfg,"mode").as_str(){"equals"=>a==b,"not_equals"=>a!=b,"contains"=>a.contains(&b),"starts_with"=>a.starts_with(&b),"ends_with"=>a.ends_with(&b),_=>false};Ok(if yes{Outcome::Satisfied}else{Outcome::NotSatisfied})}
}

struct ForEach;
fn foreach_spec()->Value{action_spec("For Each Item","repeat","Iterates a JSON array through a workflow-variable item and index.",vec![
 json!({"key":"sourceVariable","label":"Array variable","type":"text","required":true,"maxLen":64}),
 json!({"key":"itemVariable","label":"Item variable","type":"text","default":"item","required":true,"maxLen":64}),
 json!({"key":"indexVariable","label":"Index variable (0-based, optional)","type":"text","default":"index","maxLen":64}),onfail()
],"For Each {sourceVariable}","Wire IF to the loop body and route the body back to this block. Each visit exposes one item; once exhausted, ELSE and AFTER continue. Maximum 100,000 items.")}
#[async_trait]impl Block for ForEach{
 fn title(&self)-> &'static str{"For Each Item"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(foreach_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&foreach_spec(),c)?;vname(c,"sourceVariable")?;vname(c,"itemVariable")?;if !text(c,"indexVariable").is_empty(){vname(c,"indexVariable")?};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let n=vname(&cx.cfg,"sourceVariable")?;let v=cx.get_variable(&n).ok_or_else(||format!("Variable '{n}' is not set"))?;let a=v.as_array().ok_or("For Each requires a JSON array")?;if a.len()>100000{return Err("For Each is limited to 100,000 items".into())};let i=cx.visit.saturating_sub(1) as usize;if i>=a.len(){return Ok(Outcome::NotSatisfied)};cx.set_variable(vname(&cx.cfg,"itemVariable")?,a[i].clone())?;let ix=text(&cx.cfg,"indexVariable");if !ix.is_empty(){cx.set_variable(vname(&cx.cfg,"indexVariable")?,json!(i))?};Ok(Outcome::Satisfied)}
}

struct CallSubflow;
fn call_spec()->Value{action_spec("Call Subflow","workflow","Runs a saved automation as a reusable subflow using the current run's variable store.",vec![
 json!({"key":"automationId","label":"Saved automation ID","type":"text","required":true,"maxLen":100}),
 json!({"key":"timeoutSec","label":"Timeout (seconds; 0 = none)","type":"number","min":0,"max":86400,"step":1,"default":0}),onfail()
],"Call subflow {automationId}","Nested workflows share variables. Recursive calls are rejected and nesting is capped at 16 levels. Route ELSE to a handler to catch subflow failures.")}
#[async_trait]impl Block for CallSubflow{
 fn title(&self)-> &'static str{"Call Subflow"}fn category(&self)->Cat{Cat::Action}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(call_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&call_spec(),c)?;no_nul(c,&["automationId"])?;if text(c,"automationId").is_empty(){return Err("Automation ID required".into())};Ok(())}
 async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Ok(Outcome::Satisfied)}
}
struct ReturnValue;
fn return_spec()->Value{action_spec("Return Value","log-out","Optionally stores a subflow result and ends this path.",vec![
 json!({"key":"sourceVariable","label":"Source variable (optional)","type":"text","maxLen":64}),
 json!({"key":"value","label":"Literal/template when source is empty","type":"textarea","rows":2,"maxLen":50000}),
 json!({"key":"saveAs","label":"Result variable (optional)","type":"text","maxLen":64}),onfail()
],"Return value","Stores the result in the shared run-variable scope. Terminates this path of the current workflow/subflow.")}
#[async_trait]impl Block for ReturnValue{
 fn title(&self)-> &'static str{"Return Value"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(return_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&return_spec(),c)?;no_nul(c,&["sourceVariable","value","saveAs"])?;for k in ["sourceVariable","saveAs"]{if !text(c,k).is_empty(){vname(c,k)?}};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let src=text(&cx.cfg,"sourceVariable");let v=if src.is_empty(){json!(tpl(cx,"value")?)}else{cx.get_variable(&src).ok_or_else(||format!("Variable '{src}' is not set"))?};let dst=text(&cx.cfg,"saveAs");if !dst.is_empty(){cx.set_variable(vname(&cx.cfg,"saveAs")?,v)?};Ok(Outcome::Satisfied)}
}

// PowerShell UI Automation helper. Matching is scoped to the selected process window when provided.
fn uia_prefix(proc:&str,id:&str,nm:&str)->String{let p=extra::ps_quote(proc);let a=extra::ps_quote(id);let n=extra::ps_quote(nm);
format!("$ErrorActionPreference='Stop'; Add-Type -AssemblyName UIAutomationClient; Add-Type -AssemblyName UIAutomationTypes; $pn={p}; $aid={a}; $nm={n}; $root=[System.Windows.Automation.AutomationElement]::RootElement; if($pn){{$p=Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($pn)) -ErrorAction SilentlyContinue|Select-Object -First 1;if($null -eq $p){{$root=$null}}else{{$pc=New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty,$p.Id);$root=$root.FindFirst([System.Windows.Automation.TreeScope]::Children,$pc)}}}}; $el=$null; if($null -ne $root){{$list=New-Object 'System.Collections.Generic.List[System.Windows.Automation.Condition]';if($aid){{$list.Add((New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::AutomationIdProperty,$aid)))}};if($nm){{$list.Add((New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty,$nm)))}};if($list.Count -eq 1){{$q=$list[0]}}elseif($list.Count -gt 1){{$q=New-Object System.Windows.Automation.AndCondition($list.ToArray())}};if($list.Count -gt 0){{$el=$root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$q)}}}}")}
struct UiElement;
fn ui_cond_spec()->Value{let mut fields=vec![
 json!({"key":"processName","label":"Application process (optional)","type":"text","placeholder":"notepad.exe","maxLen":260}),
 json!({"key":"automationId","label":"Automation ID","type":"text","maxLen":512}),
 json!({"key":"elementName","label":"Accessible element name","type":"text","required":true,"maxLen":512}),
 json!({"key":"state","label":"State","type":"select","default":"exists","options":[["exists","Exists"],["absent","Absent"],["enabled","Enabled"],["disabled","Disabled"]]})
];fields.extend(extra::behavior_fields());cond_spec("UI Element State","app-window","Finds controls by accessible name and Automation ID rather than screen coordinates.",fields,"UI element {elementName}","Works when the target app exposes UI Automation properties. Elevated/protected/custom-rendered controls may not be accessible.")}
#[async_trait]impl Block for UiElement{
 fn title(&self)-> &'static str{"UI Element State"}fn category(&self)->Cat{Cat::Condition}fn supports_timeout(&self)->bool{true}fn spec(&self)->Option<Value>{Some(ui_cond_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&ui_cond_spec(),c)?;no_nul(c,&["processName","automationId","elementName"])?;if text(c,"automationId").is_empty()&&text(c,"elementName").is_empty(){return Err("Set an Automation ID or element name".into())};extra::wait_settings(c).map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let s=text(&cx.cfg,"state");let prefix=uia_prefix(&text(&cx.cfg,"processName"),&text(&cx.cfg,"automationId"),&text(&cx.cfg,"elementName"));let(b,p,stable,t)=extra::wait_settings(&cx.cfg)?;extra::poll_condition(cx,&b,p,stable,t,||{let s=s.clone();Box::pin(async{let out=extra::powershell(cx,&format!("{prefix};if($null -eq $el){{[Console]::Out.Write('false')}}else{{[Console]::Out.Write([string]([bool]$el.Current.IsEnabled))}}")).await?;Ok(match s.as_str(){"exists"=>out.trim()=="True"||out.trim()=="true","absent"=>!matches!(out.trim(),"True"|"true"),"enabled"=>out.trim().eq_ignore_ascii_case("true"),"disabled"=>out.trim().eq_ignore_ascii_case("false"),_=>false})}) as extra::Probe<'_>}).await}
}
struct UiClick;
fn ui_click_spec()->Value{action_spec("Click UI Element","mouse-pointer-2","Invokes a matched UI Automation control.",vec![
 json!({"key":"processName","label":"Application process (optional)","type":"text","maxLen":260}),
 json!({"key":"automationId","label":"Automation ID","type":"text","maxLen":512}),
 json!({"key":"elementName","label":"Accessible element name","type":"text","required":true,"maxLen":512}),onfail()
],"Click {elementName}","Tries Invoke, SelectionItem, and Toggle patterns. No coordinate click fallback is performed.")}
#[async_trait]impl Block for UiClick{
 fn title(&self)-> &'static str{"Click UI Element"}fn category(&self)->Cat{Cat::Action}fn risky(&self)->bool{true}fn spec(&self)->Option<Value>{Some(ui_click_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&ui_click_spec(),c)?;no_nul(c,&["processName","automationId","elementName"])?;if text(c,"automationId").is_empty()&&text(c,"elementName").is_empty(){return Err("Set an Automation ID or element name".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let prefix=uia_prefix(&text(&cx.cfg,"processName"),&text(&cx.cfg,"automationId"),&text(&cx.cfg,"elementName"));let script=format!("{prefix};if($null -eq $el){{throw 'UI element not found'}};$done=$false;foreach($patType in @([System.Windows.Automation.InvokePattern]::Pattern,[System.Windows.Automation.SelectionItemPattern]::Pattern,[System.Windows.Automation.TogglePattern]::Pattern)){{$pat=$null;if($el.TryGetCurrentPattern($patType,[ref]$pat)){{if($patType -eq [System.Windows.Automation.InvokePattern]::Pattern){{$pat.Invoke()}}elseif($patType -eq [System.Windows.Automation.SelectionItemPattern]::Pattern){{$pat.Select()}}else{{$pat.Toggle()}};$done=$true;break}}}};if(-not $done){{throw 'Control supports no Invoke, SelectionItem, or Toggle pattern'}};[Console]::Out.Write('ok')");let out=extra::powershell(cx,&script).await?;if out.trim()!="ok"{return Err("UI element invocation failed".into())};Ok(Outcome::Satisfied)}
}
struct UiSetValue;
fn ui_set_spec()->Value{action_spec("Set UI Element Value","app-window","Sets text using the UI Automation Value pattern.",vec![
 json!({"key":"processName","label":"Application process (optional)","type":"text","maxLen":260}),
 json!({"key":"automationId","label":"Automation ID","type":"text","maxLen":512}),
 json!({"key":"elementName","label":"Accessible element name","type":"text","required":true,"maxLen":512}),
 json!({"key":"value","label":"Text to set","type":"textarea","rows":3,"required":true,"maxLen":10000}),onfail()
],"Set {elementName}","Requires an enabled control exposing a writable Value pattern. Use Type Text for controls that do not.")}
#[async_trait]impl Block for UiSetValue{
 fn title(&self)-> &'static str{"Set UI Element Value"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(ui_set_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&ui_set_spec(),c)?;no_nul(c,&["processName","automationId","elementName","value"])?;vname(c,"elementName").map(|_|())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let prefix=uia_prefix(&text(&cx.cfg,"processName"),&text(&cx.cfg,"automationId"),&text(&cx.cfg,"elementName"));let val=tpl(cx,"value")?;let script=format!("{prefix};if($null -eq $el){{throw 'UI element not found'}};if(-not $el.Current.IsEnabled){{throw 'UI element is disabled'}};$pat=$null;if(-not $el.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern,[ref]$pat)){{throw 'UI element has no Value pattern'}};$pat.SetValue({});[Console]::Out.Write('ok')",extra::ps_quote(&val));let out=extra::powershell(cx,&script).await?;if out.trim()!="ok"{return Err("Set UI element value failed".into())};Ok(Outcome::Satisfied)}
}

struct FileEvent;
fn file_event_spec()->Value{cond_spec("Wait for File Event","folder-open","Uses Windows FileSystemWatcher notifications instead of polling directory snapshots.",vec![
 json!({"key":"path","label":"Directory","type":"text","required":true,"maxLen":4000}),
 json!({"key":"pattern","label":"Name pattern","type":"text","default":"*","maxLen":512}),
 json!({"key":"recursive","label":"Watch subdirectories","type":"bool","default":false}),
 json!({"key":"event","label":"Event","type":"select","default":"any","options":[["any","Any file change"],["created","Created"],["changed","Changed"],["deleted","Deleted"],["renamed","Renamed"]]}),
 json!({"key":"timeoutSec","label":"Wait timeout (seconds; 0 = indefinite)","type":"number","min":0,"max":86400,"step":1,"default":0}),onfail()
],"Wait for file event in {path}","One-shot native filesystem notification. Loop back to this block to wait for the next event. Cancellation stops the watcher subprocess.")}
#[async_trait]impl Block for FileEvent{
 fn title(&self)-> &'static str{"Wait for File Event"}fn category(&self)->Cat{Cat::Condition}fn spec(&self)->Option<Value>{Some(file_event_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&file_event_spec(),c)?;no_nul(c,&["path","pattern"])?;if text(c,"path").is_empty(){return Err("Directory required".into())};if !["any","created","changed","deleted","renamed"].contains(&text(c,"event").as_str()){return Err("Unknown event type".into())};let t=rng(c,"timeoutSec",0.0,0.0,86400.0,"Timeout")?;if t.fract()!=0.0{return Err("Timeout must be a whole number".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let p=tpl(cx,"path")?;let pat=tpl(cx,"pattern")?;let recursive=cx.cfg.get("recursive").and_then(Value::as_bool).unwrap_or(false);let mode=text(&cx.cfg,"event");let timeout=rng(&cx.cfg,"timeoutSec",0.0,0.0,86400.0,"Timeout")? as i32;let script=format!("$ErrorActionPreference='Stop';$p={};$filter={};if(-not(Test-Path -LiteralPath $p -PathType Container)){{throw 'Directory does not exist'}};$w=New-Object IO.FileSystemWatcher;$w.Path=(Resolve-Path -LiteralPath $p).Path;$w.Filter=$filter;$w.IncludeSubdirectories={};$w.NotifyFilter=[IO.NotifyFilters]'FileName,DirectoryName,LastWrite,Size';$w.EnableRaisingEvents=$true;try{{$ms=if({}>0){{{}*1000}}else{{-1}};$c=$w.WaitForChanged([IO.WatcherChangeTypes]::All,$ms);if($c.TimedOut){{throw 'Filesystem event wait timed out'}};$actual=$c.ChangeType.ToString().ToLowerInvariant();if({} -ne 'any' -and $actual -ne {}){{[Console]::Out.Write('0')}}else{{[Console]::Out.Write('1')}}}}finally{{$w.EnableRaisingEvents=$false;$w.Dispose()}}",extra::ps_quote(&p),extra::ps_quote(&pat),if recursive{"$true"}else{"$false"},timeout,timeout,extra::ps_quote(&mode),extra::ps_quote(&mode));let out=extra::powershell(cx,&script).await?;match out.trim(){"1"=>Ok(Outcome::Satisfied),"0"=>Ok(Outcome::NotSatisfied),_=>Err(format!("Unexpected FileSystemWatcher result: {}",out.chars().take(200).collect::<String>()))}}
}

struct Breakpoint;
fn breakpoint_spec()->Value{action_spec("Breakpoint","pause-circle","Pauses the automation at this point until the debugger resumes it.",vec![],"Breakpoint","When reached, the engine pauses before the next block. Use Resume to continue or Step to execute one block at a time.")}
#[async_trait]impl Block for Breakpoint{
 fn title(&self)-> &'static str{"Breakpoint"}fn category(&self)->Cat{Cat::Action}fn spec(&self)->Option<Value>{Some(breakpoint_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&breakpoint_spec(),c)}
 async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Ok(Outcome::Satisfied)}
}

struct ProcessEvent;
fn process_event_spec()->Value{cond_spec("Wait for Process Event","proc","Waits for process-start or process-stop WMI notifications without polling process lists.",vec![
 json!({"key":"processName","label":"Process name (blank = any)","type":"text","placeholder":"notepad.exe","maxLen":260}),
 json!({"key":"event","label":"Event","type":"select","default":"started","options":[["started","Process started"],["stopped","Process stopped"]]}),
 json!({"key":"timeoutSec","label":"Wait timeout (seconds; 0 = indefinite)","type":"number","min":0,"max":86400,"step":1,"default":0}),onfail()
],"Wait for process {event}: {processName}","One-shot WMI process event wait. Loop back to wait for more events; cancellation terminates the subscriber subprocess.")}
#[async_trait]impl Block for ProcessEvent{
 fn title(&self)-> &'static str{"Wait for Process Event"}fn category(&self)->Cat{Cat::Condition}fn spec(&self)->Option<Value>{Some(process_event_spec())}
 fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&process_event_spec(),c)?;no_nul(c,&["processName"])?;if !["started","stopped"].contains(&text(c,"event").as_str()){return Err("Unknown process event".into())};let t=rng(c,"timeoutSec",0.0,0.0,86400.0,"Timeout")?;if t.fract()!=0.0{return Err("Timeout must be a whole number".into())};Ok(())}
 async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{self.validate(&cx.cfg)?;let n=tpl(cx,"processName")?;let t=rng(&cx.cfg,"timeoutSec",0.0,0.0,86400.0,"Timeout")?;let class=if text(&cx.cfg,"event")=="started"{"Win32_ProcessStartTrace"}else{"Win32_ProcessStopTrace"};let script=format!("$ErrorActionPreference='Stop';$source='00047_'+[guid]::NewGuid().ToString('N');$needle={};$limit={};$sw=[Diagnostics.Stopwatch]::StartNew();Register-WmiEvent -Class {} -SourceIdentifier $source|Out-Null;try{{while($true){{$left=if($limit -le 0){{0}}else{{[Math]::Max(0.1,$limit-$sw.Elapsed.TotalSeconds)}};if($limit -gt 0 -and $left -le 0){{throw 'Process event wait timed out'}};$ev=if($limit -le 0){{Wait-Event -SourceIdentifier $source}}else{{Wait-Event -SourceIdentifier $source -Timeout $left}};if($null -eq $ev){{throw 'Process event wait timed out'}};$p=[string]$ev.SourceEventArgs.NewEvent.ProcessName;Remove-Event -EventIdentifier $ev.EventIdentifier;if((-not $needle)-or $p -ieq $needle -or [IO.Path]::GetFileNameWithoutExtension($p) -ieq [IO.Path]::GetFileNameWithoutExtension($needle)){{[Console]::Out.Write('1');break}}}}}}finally{{Unregister-Event -SourceIdentifier $source -ErrorAction SilentlyContinue}}",extra::ps_quote(&n),t,class);let o=extra::powershell(cx,&script).await?;if o.trim()=="1"{Ok(Outcome::Satisfied)}else{Err("Process event wait returned no match".into())}}
}

pub(super) fn register_into(m:&mut std::collections::HashMap<&'static str,Box<dyn Block>>){
 m.insert("act.json_parse",Box::new(JsonParse));m.insert("act.json_get",Box::new(JsonGet));
 m.insert("act.regex_extract",Box::new(RegexExtract));m.insert("act.regex_replace",Box::new(RegexReplace));
 m.insert("act.csv_read",Box::new(CsvRead));m.insert("act.csv_write",Box::new(CsvWrite));m.insert("act.transform_value",Box::new(Transform));
 m.insert("cond.switch_case",Box::new(SwitchCase));m.insert("act.for_each",Box::new(ForEach));
 m.insert("act.call_subflow",Box::new(CallSubflow));m.insert("flow.return_value",Box::new(ReturnValue));
 m.insert("cond.ui_element",Box::new(UiElement));m.insert("act.ui_click",Box::new(UiClick));m.insert("act.ui_set_value",Box::new(UiSetValue));
 m.insert("cond.file_event",Box::new(FileEvent));m.insert("cond.process_event",Box::new(ProcessEvent));m.insert("flow.breakpoint",Box::new(Breakpoint));
}
#[cfg(test)]mod tests{use super::*;#[test]fn csv_quotes_and_escaped_quotes(){let r=csv_parse("a,b\r\nx,\"hello, world\"\r\ny,\"say \"\"yes\"\"\"\r\n",',',10).unwrap();assert_eq!(r[1][1],"hello, world");assert_eq!(r[2][1],"say \"yes\"")}#[test]fn csv_rejects_unclosed_quotes_and_limits(){assert!(csv_parse("a\n\"oops",',',20).is_err());assert!(csv_parse("a\nb\nc\n",',',1).is_err())}#[test]fn json_path_array_indices(){let v=json!({"a":{"items":[{"n":"x"},{"n":"y"}]}});assert_eq!(path_get(&v,"a.items.1.n"),Some(&json!("y")));assert!(path_get(&v,"a.items.9.n").is_none())}#[test]fn regex_capture_replace(){let r=Regex::new(r"([A-Z]+)-(\d+)").unwrap();assert_eq!(r.captures("ID-42").unwrap().get(2).unwrap().as_str(),"42");assert_eq!(r.replace("ID-42","$1:$2"),"ID:42")}#[test]fn nextgen_registry_specs(){let mut m=std::collections::HashMap::new();register_into(&mut m);assert_eq!(m.len(),17);for(k,b)in m{assert!(b.spec().is_some(),"{k} no spec")}}}
