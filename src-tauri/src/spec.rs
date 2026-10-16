//! Declarative block specs. One JSON shape describes a block's UI fields and validation.
//! Used by native blocks (via `Block::spec`) and by user block packs (see custom.rs).
use serde_json::{json, Value};
use std::collections::HashMap;

fn merge(mut base: Value, over: &Value) -> Value {
    if let (Some(b), Some(o)) = (base.as_object_mut(), over.as_object()) {
        for (k, v) in o {
            if k != "preset" {
                b.insert(k.clone(), v.clone());
            }
        }
    }
    base
}

/// Expands `{"preset":"timeout"}` and `{"preset":"onFail"}` into full field definitions.
pub fn expand(f: &Value) -> Value {
    match f.get("preset").and_then(Value::as_str) {
        Some("timeout") => merge(
            json!({"key":"timeoutSec","label":"Timeout","type":"number","min":0,"max":86400,"step":1,"unit":"s","default":0,
                   "help":"0 = no timeout. When it expires the block is marked failed."}),
            f,
        ),
        Some("onFail") => merge(
            json!({"key":"onFail","label":"On failure","type":"select","default":"stop",
                   "options":[["stop","End this branch"],["else","Follow ELSE wires"],["after","Follow AFTER wires"],["abort","Abort automation"]]}),
            f,
        ),
        _ => f.clone(),
    }
}

pub fn fields(spec: &Value) -> Vec<Value> {
    spec.get("fields").and_then(Value::as_array).map(|a| a.iter().map(expand).collect()).unwrap_or_default()
}

fn flag(f: &Value, k: &str) -> bool {
    f.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn visible(f: &Value, cfg: &Value) -> bool {
    let Some(s) = f.get("show") else { return true };
    let v = cfg.get(s.get("key").and_then(Value::as_str).unwrap_or(""));
    if let Some(eq) = s.get("eq") {
        return v == Some(eq);
    }
    if let Some(ne) = s.get("ne") {
        return v != Some(ne);
    }
    if let Some(list) = s.get("in").and_then(Value::as_array) {
        return v.map(|x| list.contains(x)).unwrap_or(false);
    }
    true
}

fn current<'a>(f: &'a Value, cfg: &'a Value) -> Option<&'a Value> {
    let key = f.get("key")?.as_str()?;
    cfg.get(key).filter(|v| !v.is_null()).or_else(|| f.get("default"))
}

/// "HH:MM" -> minutes since midnight.
pub fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    if h < 24 && m < 60 {
        Some(h * 60 + m)
    } else {
        None
    }
}

/// Validates a block's saved settings against its spec. Hidden fields are skipped.
pub fn validate(spec: &Value, cfg: &Value) -> Result<(), String> {
    for f in fields(spec) {
        if !visible(&f, cfg) {
            continue;
        }
        let key = f.get("key").and_then(Value::as_str).unwrap_or("");
        let label = f.get("label").and_then(Value::as_str).unwrap_or(key);
        let v = current(&f, cfg);
        match f.get("type").and_then(Value::as_str).unwrap_or("text") {
            "number" => {
                let n = v.and_then(Value::as_f64).filter(|n| n.is_finite()).ok_or_else(|| format!("{label} must be a number"))?;
                if let Some(lo) = f.get("min").and_then(Value::as_f64) {
                    if n < lo {
                        return Err(format!("{label} must be at least {lo}"));
                    }
                }
                if let Some(hi) = f.get("max").and_then(Value::as_f64) {
                    if n > hi {
                        return Err(format!("{label} must be at most {hi}"));
                    }
                }
            }
            "time" => {
                if parse_hhmm(v.and_then(Value::as_str).unwrap_or("")).is_none() {
                    return Err(format!("{label} must be a time like 09:30"));
                }
            }
            "select" => {
                let s = v.and_then(Value::as_str).unwrap_or("");
                if let Some(opts) = f.get("options").and_then(Value::as_array) {
                    if !opts.iter().any(|o| o.get(0).and_then(Value::as_str) == Some(s)) {
                        return Err(format!("{label} has an unknown option"));
                    }
                }
            }
            "bool" => {
                if flag(&f, "required") && v.and_then(Value::as_bool) != Some(true) {
                    return Err(format!("{label} must be ticked"));
                }
            }
            _ => {
                let s = v.and_then(Value::as_str).unwrap_or("").trim();
                if flag(&f, "required") && s.is_empty() {
                    return Err(format!("{label} is required"));
                }
                if let Some(max) = f.get("maxLen").and_then(Value::as_u64) {
                    if s.chars().count() as u64 > max {
                        return Err(format!("{label} must be {max} characters or fewer"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Every field's current value (saved or default) as a string. Used for command templates and env vars.
pub fn values(spec: &Value, cfg: &Value) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for f in fields(spec) {
        let Some(key) = f.get("key").and_then(Value::as_str) else { continue };
        let s = match current(&f, cfg) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Bool(b)) => b.to_string(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        m.insert(key.to_string(), s);
    }
    m
}