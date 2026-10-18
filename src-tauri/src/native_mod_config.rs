//! Settings and bounded log reader for native Windows mods.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::{HashMap, HashSet}, fs::{self, File}, io::{Read, Seek, SeekFrom}, path::{Path, PathBuf}};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SETTINGS_BYTES: u64 = 256 * 1024;
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SETTINGS: usize = 64;
const MAX_OPTIONS: usize = 100;
const MAX_SETTING_TEXT: usize = 4096;
const MAX_LOG_LINES: usize = 200;
const MAX_LOG_LINE_CHARS: usize = 4096;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingOption { pub value: String, pub label: String }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingSpec {
    pub id: String,
    pub label: String,
    #[serde(default)] pub description: String,
    #[serde(rename = "type")] pub kind: String,
    #[serde(default)] pub default: Value,
    #[serde(default)] pub min: Option<f64>,
    #[serde(default)] pub max: Option<f64>,
    #[serde(default)] pub step: Option<f64>,
    #[serde(default)] pub max_length: Option<usize>,
    #[serde(default)] pub options: Vec<SettingOption>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeModSettings { pub id: String, pub settings: Vec<SettingSpec>, pub values: HashMap<String, Value> }

fn valid_mod_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && !id.starts_with('.') && !id.ends_with('.')
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
}

fn valid_setting_key(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64
        && id.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn package_directory(root: &Path, id: &str) -> Result<PathBuf, String> {
    if !valid_mod_id(id) { return Err("Invalid native mod ID.".into()); }
    let packages = root.join("packages").canonicalize()
        .map_err(|e| format!("Cannot resolve native mod package directory: {e}"))?;
    let folder = packages.join(id).canonicalize()
        .map_err(|e| format!("Cannot resolve installed native mod '{id}': {e}"))?;
    if !folder.starts_with(&packages) { return Err("Native mod package path escapes the managed package directory.".into()); }
    let manifest_path = folder.join("manifest.json");
    let meta = fs::symlink_metadata(&manifest_path).map_err(|e| format!("Cannot inspect native mod manifest: {e}"))?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_MANIFEST_BYTES {
        return Err("Native mod manifest must be a regular file no larger than 1 MB.".into());
    }
    let content = fs::read_to_string(&manifest_path).map_err(|e| format!("Cannot read native mod manifest: {e}"))?;
    let manifest: Value = serde_json::from_str(content.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("Invalid native mod manifest: {e}"))?;
    if manifest.get("id").and_then(Value::as_str) != Some(id) {
        return Err("Native mod ID does not match the installed package manifest.".into());
    }
    Ok(folder)
}

fn load_specs(folder: &Path) -> Result<Vec<SettingSpec>, String> {
    let content = fs::read_to_string(folder.join("manifest.json"))
        .map_err(|e| format!("Cannot read native mod manifest: {e}"))?;
    if content.len() as u64 > MAX_MANIFEST_BYTES { return Err("Native mod manifest exceeds the 1 MB limit.".into()); }
    let manifest: Value = serde_json::from_str(content.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("Invalid native mod manifest: {e}"))?;
    let raw = manifest.get("settings").cloned().unwrap_or_else(|| Value::Array(Vec::new()));
    let specs: Vec<SettingSpec> = serde_json::from_value(raw)
        .map_err(|e| format!("Invalid native mod settings schema: {e}"))?;
    validate_specs(&specs)?;
    Ok(specs)
}

fn validate_specs(specs: &[SettingSpec]) -> Result<(), String> {
    if specs.len() > MAX_SETTINGS { return Err(format!("A native mod may declare at most {MAX_SETTINGS} settings.")); }
    let mut ids = HashSet::new();
    for s in specs {
        if !valid_setting_key(&s.id) || !ids.insert(s.id.clone()) { return Err(format!("Setting ID '{}' is invalid or duplicated.", s.id)); }
        if s.label.trim().is_empty() || s.label.chars().count() > 100 { return Err(format!("Setting '{}': label is required (max 100 characters).", s.id)); }
        if s.description.chars().count() > 500 { return Err(format!("Setting '{}': description exceeds 500 characters.", s.id)); }
        if s.min.is_some_and(|v| !v.is_finite()) || s.max.is_some_and(|v| !v.is_finite()) { return Err(format!("Setting '{}': numeric bounds must be finite.", s.id)); }
        if matches!((s.min, s.max), (Some(min), Some(max)) if min > max) { return Err(format!("Setting '{}': minimum exceeds maximum.", s.id)); }
        if s.step.is_some_and(|n| !n.is_finite() || n <= 0.0) { return Err(format!("Setting '{}': step must be positive and finite.", s.id)); }
        if s.max_length.is_some_and(|n| n > MAX_SETTING_TEXT) { return Err(format!("Setting '{}': maxLength may not exceed {MAX_SETTING_TEXT}.", s.id)); }
        if s.options.len() > MAX_OPTIONS { return Err(format!("Setting '{}': too many select options.", s.id)); }
        if s.kind == "select" {
            if s.options.is_empty() { return Err(format!("Setting '{}': select settings need options.", s.id)); }
            let mut choices = HashSet::new();
            for o in &s.options {
                if o.value.is_empty() || o.value.len() > 100 || o.label.trim().is_empty()
                    || o.label.chars().count() > 100 || !choices.insert(o.value.clone()) {
                    return Err(format!("Setting '{}': select options must have unique values and non-empty labels.", s.id));
                }
            }
        } else if !s.options.is_empty() {
            return Err(format!("Setting '{}': options are only valid for select settings.", s.id));
        }
        validate_value(s, &s.default).map_err(|e| format!("Setting '{}': invalid default value: {e}", s.id))?;
    }
    Ok(())
}

fn validate_value(s: &SettingSpec, v: &Value) -> Result<(), String> {
    match s.kind.as_str() {
        "boolean" => if !v.is_boolean() { return Err("expected true or false".into()); },
        "string" => {
            let text = v.as_str().ok_or("expected text")?;
            let max = s.max_length.unwrap_or(1024);
            if text.chars().count() > max { return Err(format!("text exceeds {max} characters")); }
        }
        "integer" => {
            let n = v.as_i64().ok_or("expected an integer")? as f64;
            validate_number(s, n)?;
        }
        "number" => {
            let n = v.as_f64().ok_or("expected a number")?;
            if !n.is_finite() { return Err("expected a finite number".into()); }
            validate_number(s, n)?;
        }
        "select" => {
            let selected = v.as_str().ok_or("expected an option value")?;
            if !s.options.iter().any(|o| o.value == selected) { return Err("value is not one of the declared options".into()); }
        }
        other => return Err(format!("unsupported setting type '{other}'")),
    }
    Ok(())
}

fn validate_number(s: &SettingSpec, n: f64) -> Result<(), String> {
    if let Some(min) = s.min { if n < min { return Err(format!("value is below minimum {min}")); } }
    if let Some(max) = s.max { if n > max { return Err(format!("value is above maximum {max}")); } }
    if let Some(step) = s.step {
        let ticks = (n - s.min.unwrap_or(0.0)) / step;
        if (ticks - ticks.round()).abs() > 1e-7 { return Err(format!("value does not match step {step}")); }
    }
    Ok(())
}

fn stored_values(folder: &Path, specs: &[SettingSpec]) -> Result<HashMap<String, Value>, String> {
    let path = folder.join("settings.json");
    let stored: Value = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(serde_json::Map::new()),
        Err(e) => return Err(format!("Cannot inspect settings.json: {e}")),
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_SETTINGS_BYTES {
                return Err("settings.json must be a regular file no larger than 256 KB.".into());
            }
            let content = fs::read_to_string(&path).map_err(|e| format!("Cannot read settings.json: {e}"))?;
            serde_json::from_str(&content).map_err(|e| format!("Invalid settings.json: {e}"))?
        }
    };
    let object = stored.as_object().ok_or("settings.json must contain a JSON object")?;
    let mut values = HashMap::new();
    for s in specs {
        let v = object.get(&s.id).filter(|v| validate_value(s, v).is_ok()).cloned().unwrap_or_else(|| s.default.clone());
        values.insert(s.id.clone(), v);
    }
    Ok(values)
}

pub fn read_settings(root: &Path, id: &str) -> Result<NativeModSettings, String> {
    let folder = package_directory(root, id)?;
    let settings = load_specs(&folder)?;
    let values = stored_values(&folder, &settings)?;
    Ok(NativeModSettings { id: id.into(), settings, values })
}

pub fn write_settings(root: &Path, id: &str, values: Value) -> Result<NativeModSettings, String> {
    let folder = package_directory(root, id)?;
    let settings = load_specs(&folder)?;
    let object = values.as_object().ok_or("Settings must be submitted as a JSON object")?;
    if serde_json::to_vec(&values).map_err(|e| e.to_string())?.len() > MAX_SETTINGS_BYTES {
        return Err("Settings object exceeds the 256 KB limit.".into());
    }
    let allowed: HashSet<&str> = settings.iter().map(|s| s.id.as_str()).collect();
    if let Some(key) = object.keys().find(|key| !allowed.contains(key.as_str())) { return Err(format!("Unknown setting '{key}'.")); }
    let mut checked = HashMap::new();
    for s in &settings {
        let v = object.get(&s.id).ok_or_else(|| format!("Missing setting '{}'.", s.id))?;
        validate_value(s, v).map_err(|e| format!("Setting '{}': {e}", s.id))?;
        checked.insert(s.id.clone(), v.clone());
    }
    let path = folder.join("settings.json");
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() || !meta.is_file() { return Err("Refusing to overwrite a non-regular settings.json file.".into()); }
    }
    fs::write(&path, serde_json::to_vec_pretty(&checked).map_err(|e| e.to_string())?)
        .map_err(|e| format!("Cannot save settings.json: {e}"))?;
    Ok(NativeModSettings { id: id.into(), settings, values: checked })
}

pub fn read_logs(root: &Path, id: &str) -> Result<Vec<String>, String> {
    let folder = package_directory(root, id)?;
    let path = folder.join("mod.log");
    let metadata = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Cannot inspect mod.log: {e}")),
        Ok(m) => m,
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() { return Err("mod.log must be a regular file.".into()); }
    let mut file = File::open(&path).map_err(|e| format!("Cannot read mod.log: {e}"))?;
    let start = metadata.len().saturating_sub(MAX_LOG_BYTES);
    if start > 0 { file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?; }
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let content = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = content.lines().rev().take(MAX_LOG_LINES)
        .map(|line| line.chars().take(MAX_LOG_LINE_CHARS).collect::<String>()).collect();
    lines.reverse();
    if start > 0 && !lines.is_empty() { lines.remove(0); }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Vec<SettingSpec> {
        serde_json::from_str(r#"[
          {"id":"enabled","label":"Enabled","type":"boolean","default":true},
          {"id":"opacity","label":"Opacity","type":"number","default":0.5,"min":0,"max":1,"step":0.1},
          {"id":"style","label":"Style","type":"select","default":"mica","options":[{"value":"mica","label":"Mica"},{"value":"acrylic","label":"Acrylic"}]},
          {"id":"label","label":"Label","type":"string","default":"Example","maxLength":40}
        ]"#).unwrap()
    }

    #[test]
    fn schema_accepts_supported_setting_types() {
        let s = schema();
        assert!(validate_specs(&s).is_ok());
        assert!(validate_value(&s[0], &Value::Bool(false)).is_ok());
        assert!(validate_value(&s[1], &serde_json::json!(0.7)).is_ok());
        assert!(validate_value(&s[2], &Value::String("acrylic".into())).is_ok());
        assert!(validate_value(&s[3], &Value::String("A label".into())).is_ok());
    }

    #[test]
    fn schema_rejects_invalid_values() {
        let s = schema();
        assert!(validate_value(&s[1], &serde_json::json!(1.2)).is_err());
        assert!(validate_value(&s[1], &serde_json::json!(0.55)).is_err());
        assert!(validate_value(&s[2], &Value::String("unsupported".into())).is_err());
        assert!(validate_value(&s[0], &Value::String("true".into())).is_err());
    }

    #[test]
    fn schema_rejects_duplicate_and_invalid_ids() {
        let mut s = schema();
        s.push(s[0].clone());
        assert!(validate_specs(&s).unwrap_err().contains("duplicated"));
        assert!(!valid_mod_id("../escape"));
        assert!(!valid_setting_key("not valid"));
    }
}
