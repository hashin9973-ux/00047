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
    #[serde(default)] pub dynamic_select: bool,
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
    let mut specs: Vec<SettingSpec> = serde_json::from_value(raw)
        .map_err(|e| format!("Invalid native mod settings schema: {e}"))?;
    merge_dynamic_select_options(folder, &mut specs)?;
    validate_specs(&specs)?;
    Ok(specs)
}

fn read_profile_text(path: &Path) -> Result<Option<String>, String> {
    let meta = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Cannot inspect native mod values file: {e}")),
        Ok(meta) => meta,
    };
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > 512 * 1024 {
        return Err("wh_values.ini must be a regular file no larger than 512 KB.".into());
    }
    let bytes = fs::read(path).map_err(|e| format!("Cannot read native mod values file: {e}"))?;
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units = bytes[2..].chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]])).collect::<Vec<_>>();
        return Ok(Some(String::from_utf16_lossy(&units)));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units = bytes[2..].chunks_exact(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]])).collect::<Vec<_>>();
        return Ok(Some(String::from_utf16_lossy(&units)));
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
}

fn dynamic_select_entries(folder: &Path) -> Result<Vec<(String, String, String)>, String> {
    let Some(content) = read_profile_text(&folder.join("wh_values.ini"))? else {
        return Ok(Vec::new());
    };
    let mut in_values = false;
    let mut entries = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_values = line[1..line.len() - 1].trim().eq_ignore_ascii_case("values");
            continue;
        }
        if !in_values {
            continue;
        }
        let Some((key, label)) = line.split_once('=') else {
            continue;
        };
        let Some(suffix) = key.strip_prefix("::wh_select_option::") else {
            continue;
        };
        let Some((setting_path, value)) = suffix.split_once("::") else {
            continue;
        };
        if !valid_setting_key(setting_path) || value.chars().count() > 100
            || label.chars().count() > 100 {
            continue;
        }
        entries.push((setting_path.to_owned(), value.to_owned(), label.to_owned()));
    }
    Ok(entries)
}

fn merge_dynamic_select_options(folder: &Path, specs: &mut [SettingSpec]) -> Result<(), String> {
    let entries = dynamic_select_entries(folder)?;
    for (setting_path, value, label) in entries {
        let Some(spec) = specs.iter_mut().find(|s| s.id == setting_path && s.kind == "select" && s.dynamic_select) else {
            continue;
        };
        if let Some(existing) = spec.options.iter_mut().find(|o| o.value == value) {
            // Runtime options can relabel a static option without changing its stored value.
            existing.label = label;
        } else if spec.options.len() < MAX_OPTIONS {
            spec.options.push(SettingOption { value, label });
        }
    }
    Ok(())
}

fn append_stored_dynamic_select_values(specs: &mut [SettingSpec], values: &HashMap<String, Value>) {
    for spec in specs.iter_mut().filter(|s| s.kind == "select" && s.dynamic_select) {
        let Some(selected) = values.get(&spec.id).and_then(Value::as_str) else {
            continue;
        };
        if selected.chars().count() > 100 || spec.options.iter().any(|o| o.value == selected)
            || spec.options.len() >= MAX_OPTIONS {
            continue;
        }
        spec.options.push(SettingOption {
            value: selected.to_owned(),
            // Keep a disconnected/renamed current selection visible until the user changes it.
            label: selected.to_owned(),
        });
    }
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
        if s.dynamic_select && s.kind != "select" {
            return Err(format!("Setting '{}': dynamicSelect is only valid for select settings.", s.id));
        }
        if s.kind == "select" {
            if s.options.is_empty() && !s.dynamic_select {
                return Err(format!("Setting '{}': select settings need options or dynamicSelect:true.", s.id));
            }
            let mut choices = HashSet::new();
            for o in &s.options {
                if o.value.len() > 100 || o.label.chars().count() > 100 || !choices.insert(o.value.clone()) {
                    return Err(format!("Setting '{}': select option values must be unique and values/labels may not exceed 100 characters.", s.id));
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
            if s.dynamic_select {
                if selected.chars().count() > 100 { return Err("dynamic select value exceeds 100 characters".into()); }
            } else if !s.options.iter().any(|o| o.value == selected) {
                return Err("value is not one of the declared options".into());
            }
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
    let mut settings = load_specs(&folder)?;
    let values = stored_values(&folder, &settings)?;
    append_stored_dynamic_select_values(&mut settings, &values);
    Ok(NativeModSettings { id: id.into(), settings, values })
}

pub fn write_settings(root: &Path, id: &str, values: Value) -> Result<NativeModSettings, String> {
    let folder = package_directory(root, id)?;
    let mut settings = load_specs(&folder)?;
    let previously_stored = stored_values(&folder, &settings)?;
    append_stored_dynamic_select_values(&mut settings, &previously_stored);
    let object = values.as_object().ok_or("Settings must be submitted as a JSON object")?;
    if serde_json::to_vec(&values).map_err(|e| e.to_string())?.len() > MAX_SETTINGS_BYTES as usize {
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
    append_stored_dynamic_select_values(&mut settings, &checked);
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
    fn dynamic_select_options_merge_static_labels_and_keep_disconnected_selection() {
        let temp = TestRoot::new();
        let package = temp.path.join("packages").join("com.example.test-mod");

        let manifest_path = package.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["settings"].as_array_mut().unwrap().push(serde_json::json!({
            "id": "device",
            "label": "Device",
            "type": "select",
            "dynamicSelect": true,
            "default": "",
            "options": [{ "value": "static", "label": "Static" }]
        }));
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        fs::write(package.join("wh_values.ini"),
            "[values]\n::wh_select_option::device::speaker=Speakers\n::wh_select_option::device::static=Runtime static label\n::wh_select_option::other::ignored=Ignore me\n"
        ).unwrap();
        fs::write(package.join("settings.json"), serde_json::to_vec(&serde_json::json!({
            "device": "offline-device"
        })).unwrap()).unwrap();

        let result = read_settings(&temp.path, "com.example.test-mod").unwrap();
        let spec = result.settings.iter().find(|s| s.id == "device").unwrap();
        assert!(spec.dynamic_select);
        assert_eq!(spec.options.iter().find(|o| o.value == "static").unwrap().label, "Runtime static label");
        assert_eq!(spec.options.iter().find(|o| o.value == "speaker").unwrap().label, "Speakers");
        assert_eq!(spec.options.iter().find(|o| o.value == "offline-device").unwrap().label, "offline-device");
        assert_eq!(result.values.get("device"), Some(&Value::String("offline-device".into())));

        let mut updated = result.values.clone();
        updated.insert("device".into(), Value::String("speaker".into()));
        let saved = write_settings(&temp.path, "com.example.test-mod", serde_json::to_value(updated).unwrap()).unwrap();
        assert_eq!(saved.values.get("device"), Some(&Value::String("speaker".into())));
        let reread = read_settings(&temp.path, "com.example.test-mod").unwrap();
        assert_eq!(reread.values.get("device"), Some(&Value::String("speaker".into())));
    }

    #[test]
    fn dynamic_select_reader_accepts_utf16_windows_ini_files() {
        let temp = TestRoot::new();
        let package = temp.path.join("packages").join("com.example.test-mod");
        let text = "[values]\n::wh_select_option::device::speaker=Speakers\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(package.join("wh_values.ini"), bytes).unwrap();

        let entries = dynamic_select_entries(&package).unwrap();
        assert_eq!(entries, vec![("device".into(), "speaker".into(), "Speakers".into())]);
    }

    #[test]
    fn dynamic_select_schema_can_start_without_options() {
        let spec: SettingSpec = serde_json::from_value(serde_json::json!({
            "id": "device",
            "label": "Device",
            "type": "select",
            "dynamicSelect": true,
            "default": ""
        })).unwrap();
        assert!(validate_specs(&[spec]).is_ok());
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

    
    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn new() -> Self {
            use std::time::{SystemTime, UNIX_EPOCH};
            let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!(
                "00047-native-mod-config-{}-{nonce}",
                std::process::id()
            ));
            let package = root.join("packages").join("com.example.test-mod");
            fs::create_dir_all(&package).unwrap();
            let manifest = serde_json::json!({
                "id": "com.example.test-mod",
                "settings": [
                    { "id": "enabled", "label": "Enabled", "type": "boolean", "default": true },
                    { "id": "opacity", "label": "Opacity", "type": "number", "default": 0.5, "min": 0.0, "max": 1.0, "step": 0.1 },
                    { "id": "style", "label": "Style", "type": "select", "default": "mica",
                      "options": [{ "value": "mica", "label": "Mica" }, { "value": "acrylic", "label": "Acrylic" }] },
                    { "id": "label", "label": "Label", "type": "string", "default": "Example", "maxLength": 40 }
                ]
            });
            fs::write(
                package.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).unwrap(),
            ).unwrap();
            Self { path: root }
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn settings_round_trip_persists_only_valid_declared_values() {
        let temp = TestRoot::new();
        let id = "com.example.test-mod";

        let initial = read_settings(&temp.path, id).unwrap();
        assert_eq!(initial.values.get("enabled"), Some(&Value::Bool(true)));
        assert_eq!(initial.values.get("opacity"), Some(&serde_json::json!(0.5)));
        assert_eq!(initial.values.get("style"), Some(&Value::String("mica".into())));

        let values = serde_json::json!({
            "enabled": false,
            "opacity": 0.7,
            "style": "acrylic",
            "label": "persisted"
        });
        let saved = write_settings(&temp.path, id, values.clone()).unwrap();
        assert_eq!(saved.values.get("enabled"), Some(&Value::Bool(false)));
        assert_eq!(saved.values.get("label"), Some(&Value::String("persisted".into())));
        assert_eq!(read_settings(&temp.path, id).unwrap().values, saved.values);

        assert!(write_settings(&temp.path, id, serde_json::json!({
            "enabled": true,
            "opacity": 0.7,
            "style": "acrylic",
            "label": "persisted",
            "unknown": "not declared"
        })).unwrap_err().contains("Unknown setting"));
        assert!(write_settings(&temp.path, id, serde_json::json!({
            "enabled": true,
            "opacity": 0.75,
            "style": "acrylic",
            "label": "persisted"
        })).unwrap_err().contains("step"));
        assert_eq!(read_settings(&temp.path, id).unwrap().values, saved.values);
    }

    #[test]
    fn invalid_persisted_values_fall_back_to_schema_defaults() {
        let temp = TestRoot::new();
        let package = temp.path.join("packages").join("com.example.test-mod");
        fs::write(package.join("settings.json"), serde_json::to_vec(&serde_json::json!({
            "enabled": "yes",
            "opacity": 0.55,
            "style": "unknown",
            "label": "kept"
        })).unwrap()).unwrap();

        let values = read_settings(&temp.path, "com.example.test-mod").unwrap().values;
        assert_eq!(values.get("enabled"), Some(&Value::Bool(true)));
        assert_eq!(values.get("opacity"), Some(&serde_json::json!(0.5)));
        assert_eq!(values.get("style"), Some(&Value::String("mica".into())));
        assert_eq!(values.get("label"), Some(&Value::String("kept".into())));
    }

    #[test]
    fn log_reader_returns_only_the_most_recent_bounded_lines() {
        let temp = TestRoot::new();
        let package = temp.path.join("packages").join("com.example.test-mod");
        let content = (0..250).map(|n| format!("log-{n:03}\n")).collect::<String>();
        fs::write(package.join("mod.log"), content).unwrap();

        let lines = read_logs(&temp.path, "com.example.test-mod").unwrap();
        assert_eq!(lines.len(), MAX_LOG_LINES);
        assert_eq!(lines.first().map(String::as_str), Some("log-050"));
        assert_eq!(lines.last().map(String::as_str), Some("log-249"));
    }

    #[test]
    fn missing_log_is_an_empty_list() {
        let temp = TestRoot::new();
        assert!(read_logs(&temp.path, "com.example.test-mod").unwrap().is_empty());
    }

}
