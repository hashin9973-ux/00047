//! Data-processing and workflow-composition blocks.
use super::{extra, spec, Block, Cat, NodeCx, Outcome};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::{fs, path::Path};

const MAX_TEXT: usize = 1_048_576;
const MAX_CSV_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROWS: usize = 100_000;

fn txt(c: &Value, k: &str) -> String {
    c.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}
fn raw(c: &Value, k: &str) -> String { c.get(k).and_then(Value::as_str).unwrap_or("").to_string() }
fn int(c: &Value, k: &str, default: u64, min: u64, max: u64) -> Result<u64, String> {
    let n = c.get(k).and_then(Value::as_u64).unwrap_or(default);
    if n < min || n > max { return Err(format!("{k} must be between {min} and {max}")); }
    Ok(n)
}
fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 &&
        s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') &&
        s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}
fn no_nul(c: &Value, keys: &[&str]) -> Result<(), String> {
    for k in keys { if raw(c,k).contains('\0') { return Err(format!("{k} must not contain NUL characters")); } }
    Ok(())
}
fn value_path(root: &Value, path: &str) -> Option<Value> {
    if path.trim().is_empty() { return Some(root.clone()); }
    let mut cur = root;
    for part in path.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(part)?,
            Value::Array(a) => a.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur.clone())
}
fn var_value(cx: &NodeCx, name: &str) -> Result<Value, String> {
    cx.get_variable(name).ok_or_else(|| format!("workflow variable '{name}' is not set"))
}
fn source_value(cx: &NodeCx, c: &Value) -> Result<Value, String> {
    let name = txt(c, "sourceVar");
    if !name.is_empty() { return var_value(cx, &name); }
    let text = cx.expand_template(&raw(c, "sourceText"))?;
    if text.len() > MAX_TEXT { return Err("Source text exceeds 1 MiB".into()); }
    Ok(Value::String(text))
}
fn source_text(cx: &NodeCx, c: &Value) -> Result<String, String> {
    let v = source_value(cx, c)?;
    Ok(match v { Value::String(s) => s, other => other.to_string() })
}
fn check_output(c: &Value, key: &str) -> Result<String, String> {
    let name = txt(c,key);
    if !valid_name(&name) { return Err(format!("{key} must be a valid variable name")); }
    Ok(name)
}
fn spec_action(title: &str, icon: &str, desc: &str, fields: Vec<Value>, summary: &str, help: &str) -> Value {
    json!({"category":"action","title":title,"icon":icon,"description":desc,"fields":fields,"summary":summary,"help":help})
}
fn data_source_fields() -> Vec<Value> {
    vec![
        json!({"key":"sourceVar","label":"Source variable (takes precedence)","type":"text","maxLen":64}),
        json!({"key":"sourceText","label":"Or literal text","type":"textarea","rows":4,"maxLen":1048576}),
    ]
}

struct ParseJson;
fn parse_json_spec() -> Value {
    let mut f = data_source_fields();
    f.push(json!({"key":"path","label":"Optional dot path / array index","type":"text","maxLen":1000}));
    f.push(json!({"key":"saveAs","label":"Save JSON value as variable","type":"text","required":true,"maxLen":64}));
    f.push(json!({"preset":"onFail"}));
    spec_action("Parse JSON", "file-json", "Parses JSON from a variable or text and optionally selects a nested value.", f, "Parse JSON → {saveAs}", "Object keys use dots; array elements use numeric segments, such as data.items.0.id.")
}
#[async_trait]
impl Block for ParseJson {
    fn title(&self)-> &'static str{"Parse JSON"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(parse_json_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&parse_json_spec(),c)?;no_nul(c,&["sourceVar","sourceText","path","saveAs"])?;check_output(c,"saveAs").map(|_|())}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let source=source_text(cx,&cx.cfg)?;
        let root:Value=serde_json::from_str(&source).map_err(|e|format!("Invalid JSON: {e}"))?;
        let path=txt(&cx.cfg,"path");
        let value=value_path(&root,&path).ok_or_else(||format!("JSON path '{path}' was not found"))?;
        cx.set_variable(check_output(&cx.cfg,"saveAs")?,value)?;
        Ok(Outcome::Satisfied)
    }
}

struct JsonGet;
fn json_get_spec()->Value{
    let mut f=data_source_fields();
    f.push(json!({"key":"path","label":"Dot path / array index","type":"text","required":true,"maxLen":1000}));
    f.push(json!({"key":"saveAs","label":"Save extracted value as","type":"text","required":true,"maxLen":64}));
    f.push(json!({"preset":"onFail"}));
    spec_action("Extract JSON Value","file-json","Extracts a nested JSON value from a variable or literal.",f,"JSON {path} → {saveAs}","The extracted value retains its JSON type (string, number, boolean, array, object, or null).")
}
#[async_trait]
impl Block for JsonGet{
    fn title(&self)-> &'static str{"Extract JSON Value"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(json_get_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&json_get_spec(),c)?;no_nul(c,&["sourceVar","sourceText","path","saveAs"])?;check_output(c,"saveAs").map(|_|())}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let root=source_value(cx,&cx.cfg)?;
        let path=txt(&cx.cfg,"path");
        let v=value_path(&root,&path).ok_or_else(||format!("JSON path '{path}' was not found"))?;
        cx.set_variable(check_output(&cx.cfg,"saveAs")?,v)?;
        Ok(Outcome::Satisfied)
    }
}

struct RegexExtract;
fn regex_extract_spec()->Value{
    let mut f=data_source_fields();
    f.extend([
        json!({"key":"pattern","label":"Regex pattern","type":"text","required":true,"maxLen":4096}),
        json!({"key":"group","label":"Capture group number (0 = full match)","type":"number","min":0,"max":100,"default":1}),
        json!({"key":"saveAs","label":"Save match as","type":"text","required":true,"maxLen":64}),
        json!({"preset":"onFail"}),
    ]);
    spec_action("Regex Extract","regex","Extracts a regular-expression match or capture group.",f,"Regex extract → {saveAs}","Rust regex syntax is used (no look-around/backreferences). Inputs are bounded to 1 MiB and patterns to 4096 characters.")
}
#[async_trait]
impl Block for RegexExtract{
    fn title(&self)-> &'static str{"Regex Extract"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(regex_extract_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{
        spec::validate(&regex_extract_spec(),c)?;no_nul(c,&["sourceVar","sourceText","pattern","saveAs"])?;
        Regex::new(&txt(c,"pattern")).map_err(|e|format!("Invalid regex: {e}"))?;
        check_output(c,"saveAs").map(|_|())
    }
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let input=source_text(cx,&cx.cfg)?;
        if input.len()>MAX_TEXT{return Err("Input exceeds 1 MiB".into())}
        let re=Regex::new(&txt(&cx.cfg,"pattern")).map_err(|e|e.to_string())?;
        let group=int(&cx.cfg,"group",1,0,100)? as usize;
        let cap=re.captures(&input).ok_or("Regex did not match")?;
        let v=cap.get(group).ok_or_else(||format!("Capture group {group} does not exist or did not match"))?.as_str().to_string();
        cx.set_variable(check_output(&cx.cfg,"saveAs")?,Value::String(v))?;
        Ok(Outcome::Satisfied)
    }
}

struct RegexReplace;
fn regex_replace_spec()->Value{
    let mut f=data_source_fields();
    f.extend([
        json!({"key":"pattern","label":"Regex pattern","type":"text","required":true,"maxLen":4096}),
        json!({"key":"replacement","label":"Replacement text","type":"textarea","rows":3,"maxLen":8192}),
        json!({"key":"replaceAll","label":"Replace every match","type":"bool","default":true}),
        json!({"key":"saveAs","label":"Save output as","type":"text","required":true,"maxLen":64}),
        json!({"preset":"onFail"}),
    ]);
    spec_action("Regex Replace","regex","Replaces regex matches and saves the resulting text.",f,"Regex replace → {saveAs}","Uses Rust regex syntax. Output is capped at 1 MiB; this block does not modify source files.")
}
#[async_trait]
impl Block for RegexReplace{
    fn title(&self)-> &'static str{"Regex Replace"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(regex_replace_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&regex_replace_spec(),c)?;no_nul(c,&["sourceVar","sourceText","pattern","replacement","saveAs"])?;Regex::new(&txt(c,"pattern")).map_err(|e|format!("Invalid regex: {e}"))?;check_output(c,"saveAs").map(|_|())}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let input=source_text(cx,&cx.cfg)?;
        if input.len()>MAX_TEXT{return Err("Input exceeds 1 MiB".into())}
        let re=Regex::new(&txt(&cx.cfg,"pattern")).map_err(|e|e.to_string())?;
        let replacement=raw(&cx.cfg,"replacement");
        let out=if cx.cfg.get("replaceAll").and_then(Value::as_bool).unwrap_or(true){re.replace_all(&input,replacement.as_str()).into_owned()}else{re.replace(&input,replacement.as_str()).into_owned()};
        if out.len()>MAX_TEXT{return Err("Output exceeds 1 MiB".into())}
        cx.set_variable(check_output(&cx.cfg,"saveAs")?,Value::String(out))?;
        Ok(Outcome::Satisfied)
    }
}

fn delimiter(c:&Value)->Result<char,String>{match txt(c,"delimiter").as_str(){"comma"=>Ok(','),"semicolon"=>Ok(';'),"tab"=>Ok('\t'),_=>Err("Delimiter must be comma, semicolon, or tab".into())}}
fn parse_csv(input:&str, delim:char)->Result<Vec<Vec<String>>,String>{
    if input.len() as u64>MAX_CSV_BYTES{return Err("CSV exceeds the 16 MiB limit".into())}
    let mut rows=Vec::<Vec<String>>::new();let mut row=Vec::<String>::new();let mut cell=String::new();
    let mut chars=input.chars().peekable();let mut quoted=false;
    while let Some(ch)=chars.next(){
        if quoted {
            if ch=='"' { if chars.peek()==Some(&'"'){chars.next();cell.push('"')}else{quoted=false} }
            else { cell.push(ch); }
        } else { match ch {
            '"' if cell.is_empty()=>quoted=true,
            x if x==delim=>{row.push(std::mem::take(&mut cell));},
            '\n'=>{row.push(std::mem::take(&mut cell));rows.push(std::mem::take(&mut row));},
            '\r'=>{if chars.peek()==Some(&'\n'){chars.next();}row.push(std::mem::take(&mut cell));rows.push(std::mem::take(&mut row));},
            _=>cell.push(ch),
        }}
        if rows.len()>MAX_ROWS+1{return Err("CSV row count exceeds 100,000".into())}
    }
    if quoted{return Err("CSV ends inside a quoted field".into())}
    if !cell.is_empty()||!row.is_empty(){row.push(cell);rows.push(row);}
    Ok(rows)
}
fn csv_quote(s:&str, delim:char)->String {
    if s.contains(delim)||s.contains('"')||s.contains('\n')||s.contains('\r'){
        format!("\"{}\"",s.replace('"',"\"\""))
    } else {s.to_string()}
}
fn write_csv(path:&Path,rows:&[Value],delim:char,append:bool)->Result<(),String>{
    if rows.len()>MAX_ROWS{return Err("CSV output exceeds 100,000 rows".into())}
    let mut columns:Vec<String>=Vec::new();
    for r in rows {if let Some(m)=r.as_object(){for k in m.keys(){if !columns.contains(k){columns.push(k.clone())}}}}
    if columns.is_empty() && !rows.is_empty(){return Err("CSV rows must be JSON objects".into())}
    let parent=path.parent().ok_or("CSV output path has no parent")?;
    fs::create_dir_all(parent).map_err(|e|format!("create CSV directory: {e}"))?;
    let mut content=String::new();
    // A new append destination needs a header just like a replacement destination.
    let write_header = !append || !path.exists();
    if write_header && !columns.is_empty(){content.push_str(&columns.iter().map(|x|csv_quote(x,delim)).collect::<Vec<_>>().join(&delim.to_string()));content.push_str("\r\n");}
    for r in rows {
        let m=r.as_object().ok_or("CSV rows must be JSON objects")?;
        content.push_str(&columns.iter().map(|k|{
            let v=m.get(k).map(|x|match x{Value::String(s)=>s.clone(),Value::Null=>String::new(),other=>other.to_string()}).unwrap_or_default();
            csv_quote(&v,delim)
        }).collect::<Vec<_>>().join(&delim.to_string()));
        content.push_str("\r\n");
    }
    if content.len() as u64>MAX_CSV_BYTES{return Err("CSV output exceeds 16 MiB".into())}
    if append {
        use std::io::Write;
        let mut f=fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e|format!("open CSV output: {e}"))?;
        f.write_all(content.as_bytes()).map_err(|e|format!("write CSV output: {e}"))?;
    } else {
        let ext=path.extension().and_then(|x|x.to_str()).unwrap_or("csv");
        let tmp=path.with_extension(format!("{ext}.{}.tmp",std::process::id()));
        let backup=path.with_extension(format!("{ext}.{}.bak",std::process::id()));
        fs::write(&tmp,&content).map_err(|e|format!("write staged CSV: {e}"))?;
        let had_old=path.exists();
        if had_old {
            if backup.exists() { fs::remove_file(&backup).map_err(|e|format!("remove stale CSV backup: {e}"))?; }
            if let Err(e)=fs::rename(path,&backup) {
                let _=fs::remove_file(&tmp);
                return Err(format!("stage existing CSV for replacement: {e}"));
            }
        }
        if let Err(e)=fs::rename(&tmp,path) {
            let _=fs::remove_file(&tmp);
            if had_old {
                let _=fs::rename(&backup,path);
            }
            return Err(format!("install staged CSV: {e}; previous file restoration was {}",if had_old && path.exists(){"successful"}else if had_old{"unsuccessful"}else{"not needed"}));
        }
        if had_old { let _=fs::remove_file(&backup); }
    }
    Ok(())
}

struct CsvRead;
fn csv_read_spec()->Value{
    let mut f=vec![
        json!({"key":"path","label":"CSV file path","type":"text","required":true,"maxLen":4000}),
        json!({"key":"delimiter","label":"Delimiter","type":"select","default":"comma","options":[["comma","Comma"],["semicolon","Semicolon"],["tab","Tab"]]}),
        json!({"key":"hasHeader","label":"First row is header","type":"bool","default":true}),
        json!({"key":"maxRows","label":"Maximum rows","type":"number","min":1,"max":100000,"default":10000}),
        json!({"key":"saveAs","label":"Save rows as array variable","type":"text","required":true,"maxLen":64}),
        json!({"preset":"onFail"}),
    ];
    spec_action("Read CSV","file-spreadsheet","Reads a bounded CSV file into an array of JSON objects.",f,"Read CSV → {saveAs}","Only UTF-8/UTF-8-BOM CSV is accepted. Limit: 16 MiB and 100,000 data rows.")
}
#[async_trait]
impl Block for CsvRead{
    fn title(&self)-> &'static str{"Read CSV"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(csv_read_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_read_spec(),c)?;no_nul(c,&["path","saveAs"])?;check_output(c,"saveAs").map(|_|())?;delimiter(c).map(|_|())}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let path=cx.expand_template(&txt(&cx.cfg,"path"))?;
        let meta=fs::metadata(&path).map_err(|e|format!("read CSV metadata: {e}"))?;
        if !meta.is_file()||meta.len()>MAX_CSV_BYTES{return Err("CSV must be a file no larger than 16 MiB".into())}
        let data=fs::read_to_string(&path).map_err(|e|format!("read UTF-8 CSV: {e}"))?;
        let data=data.strip_prefix('\u{feff}').unwrap_or(&data);
        let d=delimiter(&cx.cfg)?;
        let mut rows=parse_csv(data,d)?;
        let max=int(&cx.cfg,"maxRows",10000,1,100000)? as usize;
        let has_header=cx.cfg.get("hasHeader").and_then(Value::as_bool).unwrap_or(true);
        let headers:Vec<String>=if has_header {
            if rows.is_empty(){vec![]} else {rows.remove(0).into_iter().enumerate().map(|(i,s)|if s.trim().is_empty(){format!("column{}",i+1)}else{s}).collect()}
        } else {
            rows.first().map(|r|(0..r.len()).map(|i|format!("column{}",i+1)).collect()).unwrap_or_default()
        };
        if rows.len()>max{return Err(format!("CSV has {} rows, exceeding the configured maximum {max}",rows.len()))}
        let mut out=Vec::with_capacity(rows.len());
        for row in rows {
            let mut obj=serde_json::Map::new();
            for (i,h) in headers.iter().enumerate(){obj.insert(h.clone(),Value::String(row.get(i).cloned().unwrap_or_default()));}
            out.push(Value::Object(obj));
        }
        cx.set_variable(check_output(&cx.cfg,"saveAs")?,Value::Array(out))?;
        Ok(Outcome::Satisfied)
    }
}

struct CsvFilter;
fn csv_filter_spec()->Value{
    let f=vec![
        json!({"key":"sourceVar","label":"Source array variable","type":"text","required":true,"maxLen":64}),
        json!({"key":"column","label":"Column name","type":"text","required":true,"maxLen":512}),
        json!({"key":"operator","label":"Filter","type":"select","default":"equals","options":[["equals","Equals"],["not_equals","Does not equal"],["contains","Contains"],["starts_with","Starts with"],["empty","Is empty"],["not_empty","Is not empty"]]}),
        json!({"key":"value","label":"Compare with","type":"text","maxLen":8192,"show":{"key":"operator","in":["equals","not_equals","contains","starts_with"]}}),
        json!({"key":"saveAs","label":"Save filtered array as","type":"text","required":true,"maxLen":64}),
        json!({"preset":"onFail"}),
    ];
    spec_action("Filter Array / CSV Rows","filter","Filters an array of objects by one field while preserving their original JSON values.",f,"Filter {sourceVar} → {saveAs}","Maximum input is 100,000 rows. String comparisons are case-insensitive.")
}
#[async_trait]
impl Block for CsvFilter{
    fn title(&self)-> &'static str{"Filter Array / CSV Rows"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(csv_filter_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_filter_spec(),c)?;no_nul(c,&["sourceVar","column","value","saveAs"])?;check_output(c,"saveAs").map(|_|())?;if !matches!(txt(c,"operator").as_str(),"equals"|"not_equals"|"contains"|"starts_with"|"empty"|"not_empty"){return Err("Unknown filter operator".into())}Ok(())}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let rows=var_value(cx,&txt(&cx.cfg,"sourceVar"))?;
        let rows=rows.as_array().ok_or("Source variable must be a JSON array")?;
        if rows.len()>MAX_ROWS{return Err("Input array exceeds 100,000 rows".into())}
        let column=txt(&cx.cfg,"column");let op=txt(&cx.cfg,"operator");let target=raw(&cx.cfg,"value").to_lowercase();
        let output:Vec<Value>=rows.iter().filter(|r|{
            let value=r.get(&column).map(|v|match v{Value::String(s)=>s.clone(),Value::Null=>String::new(),x=>x.to_string()}).unwrap_or_default();
            match op.as_str(){"equals"=>value.to_lowercase()==target,"not_equals"=>value.to_lowercase()!=target,"contains"=>value.to_lowercase().contains(&target),"starts_with"=>value.to_lowercase().starts_with(&target),"empty"=>value.trim().is_empty(),"not_empty"=>!value.trim().is_empty(),_=>false}
        }).cloned().collect();
        cx.set_variable(check_output(&cx.cfg,"saveAs")?,Value::Array(output))?;Ok(Outcome::Satisfied)
    }
}

struct CsvWrite;
fn csv_write_spec()->Value{
    let f=vec![
        json!({"key":"sourceVar","label":"Source array variable","type":"text","required":true,"maxLen":64}),
        json!({"key":"path","label":"Output path","type":"text","required":true,"maxLen":4000}),
        json!({"key":"delimiter","label":"Delimiter","type":"select","default":"comma","options":[["comma","Comma"],["semicolon","Semicolon"],["tab","Tab"]]}),
        json!({"key":"mode","label":"Write mode","type":"select","default":"replace","options":[["replace","Replace (staged file)"],["append","Append rows"]]}),
        json!({"preset":"onFail"}),
    ];
    spec_action("Write CSV","file-spreadsheet","Writes an array of objects to CSV with proper field escaping.",f,"Write {sourceVar} → CSV","Writes up to 100,000 rows and 16 MiB. Replace mode stages a temporary file before installing it; append mode preserves existing content.")
}
#[async_trait]
impl Block for CsvWrite{
    fn title(&self)-> &'static str{"Write CSV"} fn category(&self)->Cat{Cat::Action}
    fn risky(&self)->bool{true}
    fn spec(&self)->Option<Value>{Some(csv_write_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&csv_write_spec(),c)?;no_nul(c,&["sourceVar","path"])?;delimiter(c).map(|_|())?;if !matches!(txt(c,"mode").as_str(),"replace"|"append"){return Err("Unknown write mode".into())}Ok(())}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let source=var_value(cx,&txt(&cx.cfg,"sourceVar"))?;
        let rows=source.as_array().ok_or("Source variable must be a JSON array")?;
        if rows.len()>MAX_ROWS{return Err("Output array exceeds 100,000 rows".into())}
        let path=cx.expand_template(&txt(&cx.cfg,"path"))?;
        let p=Path::new(&path);let append=txt(&cx.cfg,"mode")=="append";
        if p.exists()&&!append&&!p.is_file(){return Err("CSV destination exists but is not a file".into())}
        write_csv(p,rows,delimiter(&cx.cfg)?,append)?;
        Ok(Outcome::Satisfied)
    }
}

struct TransformValue;
fn transform_spec()->Value{
    let f=vec![
        json!({"key":"sourceVar","label":"Source variable","type":"text","required":true,"maxLen":64}),
        json!({"key":"operation","label":"Transform","type":"select","default":"trim","options":[["trim","Trim string"],["upper","Uppercase"],["lower","Lowercase"],["length","String / array / object length"],["to_number","Parse number"],["add","Add operand"],["subtract","Subtract operand"],["multiply","Multiply operand"],["divide","Divide by operand"],["round","Round number"],["json_compact","Serialize as compact JSON"],["parse_json","Parse string as JSON"],["to_boolean","Parse boolean"]]}),
        json!({"key":"operand","label":"Numeric operand","type":"number","min":-1000000000000,"max":1000000000000,"default":1,"show":{"key":"operation","in":["add","subtract","multiply","divide"]}}),
        json!({"key":"decimals","label":"Decimal places","type":"number","min":0,"max":10,"default":0,"show":{"key":"operation","eq":"round"}}),
        json!({"key":"saveAs","label":"Save result as","type":"text","required":true,"maxLen":64}),
        json!({"preset":"onFail"}),
    ];
    spec_action("Transform Value","calculator","Performs bounded string, JSON, length, and numeric transformations.",f,"{operation} {sourceVar} → {saveAs}","Division by zero, invalid input types, non-finite numbers, and results outside representable JSON numbers are errors.")
}
#[async_trait]
impl Block for TransformValue{
    fn title(&self)-> &'static str{"Transform Value"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(transform_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&transform_spec(),c)?;no_nul(c,&["sourceVar","saveAs"])?;check_output(c,"saveAs").map(|_|())?;if !matches!(txt(c,"operation").as_str(),"trim"|"upper"|"lower"|"length"|"to_number"|"add"|"subtract"|"multiply"|"divide"|"round"|"json_compact"|"parse_json"|"to_boolean"){return Err("Unknown transform operation".into())}Ok(())}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let source=var_value(cx,&txt(&cx.cfg,"sourceVar"))?;let op=txt(&cx.cfg,"operation");
        let out=match op.as_str(){
            "trim"=>Value::String(source.as_str().ok_or("Trim requires a string")?.trim().to_string()),
            "upper"=>Value::String(source.as_str().ok_or("Uppercase requires a string")?.to_uppercase()),
            "lower"=>Value::String(source.as_str().ok_or("Lowercase requires a string")?.to_lowercase()),
            "length"=>{let n=match &source{Value::String(s)=>s.chars().count(),Value::Array(a)=>a.len(),Value::Object(o)=>o.len(),_=>return Err("Length requires a string, array, or object".into())};json!(n)},
            "to_number"=>{let n=match &source{Value::Number(n)=>n.as_f64().ok_or("Invalid number")?,Value::String(s)=>s.trim().parse::<f64>().map_err(|_|"Value is not numeric")?,_=>return Err("Parse number requires a number or string".into())};if !n.is_finite(){return Err("Number must be finite".into())}json!(n)},
            "add"|"subtract"|"multiply"|"divide"|"round"=>{
                let n=source.as_f64().or_else(||source.as_str().and_then(|s|s.parse().ok())).ok_or("Numeric transform requires a number or numeric string")?;
                let operand=cx.cfg.get("operand").and_then(Value::as_f64).unwrap_or(1.0);
                let result=match op.as_str(){"add"=>n+operand,"subtract"=>n-operand,"multiply"=>n*operand,"divide"=>{if operand==0.0{return Err("Cannot divide by zero".into())}n/operand},_=>{let places=int(&cx.cfg,"decimals",0,0,10)? as i32;let p=10f64.powi(places);(n*p).round()/p}};
                if !result.is_finite(){return Err("Numeric result is not finite".into())}json!(result)
            },
            "json_compact"=>Value::String(source.to_string()),
            "parse_json"=>serde_json::from_str(source.as_str().ok_or("Parse JSON requires a string")?).map_err(|e|format!("Invalid JSON: {e}"))?,
            "to_boolean"=>match source{Value::Bool(b)=>Value::Bool(b),Value::String(s) if s.eq_ignore_ascii_case("true")=>Value::Bool(true),Value::String(s) if s.eq_ignore_ascii_case("false")=>Value::Bool(false),_=>return Err("Value must be true or false".into())},
            _=>return Err("Unsupported transform operation".into()),
        };
        cx.set_variable(check_output(&cx.cfg,"saveAs")?,out)?;Ok(Outcome::Satisfied)
    }
}

struct VariableCompare;
fn variable_compare_spec()->Value{
    let mut f=vec![
        json!({"key":"variable","label":"Variable name","type":"text","required":true,"maxLen":64}),
        json!({"key":"operator","label":"Comparison","type":"select","default":"equals","options":[["equals","Equals"],["not_equals","Does not equal"],["contains","Contains"],["greater","Greater than"],["at_least","Greater than or equal"],["less","Less than"],["at_most","Less than or equal"],["exists","Is set"],["truthy","Is truthy"]]}),
        json!({"key":"expected","label":"Expected value (string / number)","type":"text","maxLen":8192,"show":{"key":"operator","not_equals":"exists","ne":"truthy"}}),
    ];
    f.extend(extra::behavior_fields());
    json!({"category":"condition","title":"Variable Condition","icon":"variable","description":"Routes workflow flow based on a variable's value or existence.","fields":f,"summary":"Variable {variable} {operator}","help":"Useful for branching, sequential Switch/Case patterns, and checking results of earlier blocks. State-change mode waits until the comparison result flips."})
}
#[async_trait]
impl Block for VariableCompare{
    fn title(&self)-> &'static str{"Variable Condition"} fn category(&self)->Cat{Cat::Condition}
    fn supports_timeout(&self)->bool{true}
    fn spec(&self)->Option<Value>{Some(variable_compare_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&variable_compare_spec(),c)?;no_nul(c,&["variable","expected"])?;if !valid_name(&txt(c,"variable")){return Err("Variable name is invalid".into())}if !matches!(txt(c,"operator").as_str(),"equals"|"not_equals"|"contains"|"greater"|"at_least"|"less"|"at_most"|"exists"|"truthy"){return Err("Unknown comparison".into())}extra::wait_settings(c).map(|_|())}
    async fn run(&self,cx:&NodeCx)->Result<Outcome,String>{
        self.validate(&cx.cfg)?;
        let name=txt(&cx.cfg,"variable");let op=txt(&cx.cfg,"operator");let expected=raw(&cx.cfg,"expected");
        let (behavior,poll,stable,timeout)=extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx,&behavior,poll,stable,timeout,||Box::pin(async{
            let actual=cx.get_variable(&name);
            let result=match op.as_str(){
                "exists"=>actual.is_some(),
                "truthy"=>match actual.as_ref(){Some(Value::Bool(b))=>*b,Some(Value::Number(n))=>n.as_f64().unwrap_or(0.0)!=0.0,Some(Value::String(s))=>!s.is_empty(),Some(Value::Array(a))=>!a.is_empty(),Some(Value::Object(o))=>!o.is_empty(),_=>false},
                "equals"|"not_equals"|"contains"=>{let a=actual.as_ref().map(|v|v.as_str().map(str::to_string).unwrap_or_else(||v.to_string())).unwrap_or_default();let eq=a.eq_ignore_ascii_case(&expected);match op.as_str(){"equals"=>eq,"not_equals"=>!eq,_=>a.to_lowercase().contains(&expected.to_lowercase())}},
                _=>{let a=actual.as_ref().and_then(|v|v.as_f64().or_else(||v.as_str().and_then(|s|s.parse().ok())));let b=expected.parse::<f64>().ok();match (a,b,op.as_str()){(Some(a),Some(b),"greater")=>a>b,(Some(a),Some(b),"at_least")=>a>=b,(Some(a),Some(b),"less")=>a<b,(Some(a),Some(b),"at_most")=>a<=b,_=>false}}
            };
            (cx.report)("waiting",None,format!("Variable {name}: {}",if result{"comparison satisfied"}else{"not satisfied"}));
            Ok(result)
        }) as extra::Probe<'_>).await
    }
}

struct ForEach;
fn foreach_spec()->Value{
    spec_action("For Each Item","repeat","Runs the IF-connected body once per item in a JSON array variable.",vec![
        json!({"key":"itemsVar","label":"Array variable","type":"text","required":true,"maxLen":64}),
        json!({"key":"itemVar","label":"Current item variable","type":"text","default":"item","required":true,"maxLen":64}),
        json!({"key":"indexVar","label":"Zero-based index variable","type":"text","default":"index","required":true,"maxLen":64}),
        json!({"preset":"timeout"}),
        json!({"preset":"onFail"}),
    ],"For each {itemsVar}","Connect the IF output to the loop body and terminate each path with For Each End. After runs after every item is processed. Body execution is sequential.")
}
#[async_trait]
impl Block for ForEach{
    fn title(&self)-> &'static str{"For Each Item"} fn category(&self)->Cat{Cat::Action} fn supports_timeout(&self)->bool{true}
    fn spec(&self)->Option<Value>{Some(foreach_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{spec::validate(&foreach_spec(),c)?;no_nul(c,&["itemsVar","itemVar","indexVar"])?;for k in ["itemsVar","itemVar","indexVar"]{if !valid_name(&txt(c,k)){return Err(format!("{k} must be a valid variable name"))}}if txt(c,"itemVar")==txt(c,"indexVar"){return Err("Item and index variables must be different".into())}Ok(())}
    async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Err("For Each Item is executed by the workflow engine.".into())}
}
struct ForEachEnd;
fn foreach_end_spec()->Value{spec_action("For Each End","corner-down-left","Marks the end of a For Each Item loop body.",vec![],"End loop body","This marker stops the current loop-body path. The loop continues with the next item; workflow AFTER wires on For Each run after the array is exhausted.")}
#[async_trait]
impl Block for ForEachEnd{
    fn title(&self)-> &'static str{"For Each End"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(foreach_end_spec())}
    fn validate(&self,_:&Value)->Result<(),String>{Ok(())}
    async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Ok(Outcome::Satisfied)}
}

struct TryCatch;
fn try_catch_spec()->Value {
    spec_action("Try / Catch Subflows","git-branch","Runs one saved automation and invokes a recovery subflow if it fails.",vec![
        json!({"key":"tryAutomationId","label":"Try subflow automation ID","type":"text","required":true,"maxLen":128}),
        json!({"key":"catchAutomationId","label":"Catch subflow automation ID","type":"text","required":true,"maxLen":128}),
        json!({"key":"errorVar","label":"Save error message as variable","type":"text","default":"errorMessage","required":true,"maxLen":64}),
        json!({"preset":"timeout"}),
        json!({"preset":"onFail"}),
    ],"Try {tryAutomationId} / catch {catchAutomationId}","If the try subflow fails, the error message is stored in the named variable and the catch subflow runs. IF means try succeeded; ELSE means the catch handled the failure. If catch also fails, normal On failure handling applies.")
}
#[async_trait]
impl Block for TryCatch {
    fn title(&self)-> &'static str{"Try / Catch Subflows"} fn category(&self)->Cat{Cat::Action}
    fn supports_timeout(&self)->bool{true}
    fn spec(&self)->Option<Value>{Some(try_catch_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{
        spec::validate(&try_catch_spec(),c)?;no_nul(c,&["tryAutomationId","catchAutomationId","errorVar"])?;
        let a=txt(c,"tryAutomationId");let b=txt(c,"catchAutomationId");
        if a.is_empty()||b.is_empty(){return Err("Try and catch subflow IDs are required".into())}
        if a==b{return Err("Try and catch must reference different automations".into())}
        if !valid_name(&txt(c,"errorVar")){return Err("Error variable name is invalid".into())}
        Ok(())
    }
    async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Err("Try / Catch is executed by the workflow engine.".into())}
}

struct ForEachContinue;
struct ForEachBreak;
fn loop_marker_spec(title:&str,icon:&str,desc:&str)->Value{
    spec_action(title,icon,desc,vec![],title,"Only valid inside a For Each Item body. The marker is intercepted by the workflow engine.")
}
#[async_trait]
impl Block for ForEachContinue {
    fn title(&self)-> &'static str{"For Each Continue"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(loop_marker_spec("For Each Continue","skip-forward","Skips the rest of this loop-body path and continues with the next array item."))}
    fn validate(&self,_:&Value)->Result<(),String>{Ok(())}
    async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Ok(Outcome::Satisfied)}
}
#[async_trait]
impl Block for ForEachBreak {
    fn title(&self)-> &'static str{"For Each Break"} fn category(&self)->Cat{Cat::Action}
    fn spec(&self)->Option<Value>{Some(loop_marker_spec("For Each Break","circle-stop","Stops the current For Each loop and continues after the loop."))}
    fn validate(&self,_:&Value)->Result<(),String>{Ok(())}
    async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Ok(Outcome::Satisfied)}
}

struct CallSubflow;
fn subflow_spec()->Value{
    spec_action("Call Subflow","workflow","Runs another saved automation as a nested workflow; its variables are shared within the parent run.",vec![
        json!({"key":"automationId","label":"Automation ID to call","type":"text","required":true,"maxLen":128}),
        json!({"key":"inputMapJson","label":"Input mapping JSON (child variable → parent variable)","type":"textarea","rows":3,"default":"{}","maxLen":8000}),
        json!({"key":"outputMapJson","label":"Output mapping JSON (child variable → parent variable)","type":"textarea","rows":3,"default":"{}","maxLen":8000}),
        json!({"preset":"timeout"}),
        json!({"preset":"onFail"}),
    ],"Call workflow {automationId}","Subflows share the current run's variable store. Maps must be JSON objects of variable-name pairs. A nested failure follows this block's On failure route.")
}
#[async_trait]
impl Block for CallSubflow{
    fn title(&self)-> &'static str{"Call Subflow"} fn category(&self)->Cat{Cat::Action} fn supports_timeout(&self)->bool{true}
    fn spec(&self)->Option<Value>{Some(subflow_spec())}
    fn validate(&self,c:&Value)->Result<(),String>{
        spec::validate(&subflow_spec(),c)?;no_nul(c,&["automationId","inputMapJson","outputMapJson"])?;
        if txt(c,"automationId").is_empty(){return Err("Automation ID is required".into())}
        for key in ["inputMapJson","outputMapJson"]{let v:Value=serde_json::from_str(&raw(c,key)).map_err(|e|format!("{key} must be valid JSON: {e}"))?;if !v.is_object(){return Err(format!("{key} must be a JSON object"))}for (a,b) in v.as_object().unwrap(){if !valid_name(a)||!b.as_str().is_some_and(valid_name){return Err(format!("{key} must map valid variable names to variable names"))}}}
        Ok(())
    }
    async fn run(&self,_:&NodeCx)->Result<Outcome,String>{Err("Call Subflow is executed by the workflow engine.".into())}
}

pub(super) fn register_into(m:&mut std::collections::HashMap<&'static str,Box<dyn Block>>){
    m.insert("act.parse_json",Box::new(ParseJson));
    m.insert("act.json_get",Box::new(JsonGet));
    m.insert("act.regex_extract",Box::new(RegexExtract));
    m.insert("act.regex_replace",Box::new(RegexReplace));
    m.insert("act.csv_read",Box::new(CsvRead));
    m.insert("act.csv_filter",Box::new(CsvFilter));
    m.insert("act.csv_write",Box::new(CsvWrite));
    m.insert("act.transform_value",Box::new(TransformValue));
    m.insert("cond.variable_compare",Box::new(VariableCompare));
    m.insert("act.for_each",Box::new(ForEach));
    m.insert("act.for_each_end",Box::new(ForEachEnd));
    m.insert("act.for_each_continue",Box::new(ForEachContinue));
    m.insert("act.for_each_break",Box::new(ForEachBreak));
    m.insert("act.call_subflow",Box::new(CallSubflow));
    m.insert("act.try_catch",Box::new(TryCatch));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csv_parser_handles_quotes_delimiters_and_newlines(){
        let r=parse_csv("name,value\r\n\"Hana, Inc\",2\r\n\"a \"\"quote\"\"\",3\n",',').unwrap();
        assert_eq!(r,vec![vec!["name","value"],vec!["Hana, Inc","2"],vec!["a \"quote\"","3"]]);
    }
    #[test]
    fn csv_writer_creates_header_on_first_append_and_preserves_existing_file_on_stage_failure(){
        use std::{env,fs,time::{SystemTime,UNIX_EPOCH}};
        let root=env::temp_dir().join(format!("00047-csv-test-{}",SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&root).unwrap();
        let path=root.join("out.csv");
        write_csv(&path,&[json!({"name":"first","value":1})],',',true).unwrap();
        let first=fs::read_to_string(&path).unwrap();
        assert!(first.starts_with("name,value\r\n"));
        write_csv(&path,&[json!({"name":"second","value":2})],',',true).unwrap();
        let appended=fs::read_to_string(&path).unwrap();
        assert_eq!(appended.matches("name,value").count(),1);
        assert!(appended.contains("second,2"));
        // Force the staged write to fail by providing a directory as the file destination.
        let impossible=root.join("destination.csv");
        fs::create_dir(&impossible).unwrap();
        assert!(write_csv(&impossible,&[json!({"name":"third"})],',',false).is_err());
        let backup_or_dir=fs::metadata(&impossible).unwrap();
        assert!(backup_or_dir.is_dir());
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn csv_parser_rejects_unclosed_quotes(){
        assert!(parse_csv("a,b\n\"unfinished",',').is_err());
    }
    #[test]
    fn json_path_supports_objects_and_arrays(){
        let v=json!({"data":{"items":[{"id":12}] }});
        assert_eq!(value_path(&v,"data.items.0.id"),Some(json!(12)));
        assert_eq!(value_path(&v,"data.items.4.id"),None);
    }
    #[test]
    fn reusable_flow_blocks_validate_safe_references(){
        assert!(CallSubflow.validate(&json!({"automationId":"abc","inputMapJson":"{\"param\":\"source\"}","outputMapJson":"{\"result\":\"target\"}","timeoutSec":30,"onFail":"stop"})).is_ok());
        assert!(TryCatch.validate(&json!({"tryAutomationId":"try-id","catchAutomationId":"catch-id","errorVar":"err","timeoutSec":60,"onFail":"stop"})).is_ok());
        assert!(TryCatch.validate(&json!({"tryAutomationId":"same","catchAutomationId":"same","errorVar":"err","timeoutSec":60,"onFail":"stop"})).is_err());
    }
    #[test]
    fn output_variable_names_are_validated(){
        assert!(valid_name("output-1"));assert!(!valid_name("1bad"));assert!(!valid_name("bad name"));
    }
}
