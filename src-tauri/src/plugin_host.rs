//! Out-of-process plugin host for application-level commands and background services.
//! External plugin programs run with the current user's permissions. This host uses argv
//! arrays rather than shell command strings and resolves entrypoints beneath installed packages.
//! Native in-process Windows mods are intentionally handled by a separate future subsystem.

use crate::{custom, spec};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    process::Stdio,
    time::Instant,
};
use tokio::{
    process::{Child, Command},
    sync::Mutex,
    time::{timeout, Duration},
};

#[derive(Serialize)]
pub struct PluginRunResult {
    pub plugin_id: String,
    pub command_id: String,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub elapsed_ms: u64,
}

struct ManagedService {
    child: Child,
    pid: Option<u32>,
}

#[derive(Default)]
pub struct PluginHost {
    services: Mutex<HashMap<String, ManagedService>>,
}

fn service_key(plugin_id: &str, service_id: &str) -> String {
    format!("{plugin_id}/{service_id}")
}

fn tail_text(bytes: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    let count = text.chars().count();
    if count > limit {
        format!("…{}", text.chars().skip(count - limit).collect::<String>())
    } else {
        text.to_string()
    }
}

fn substitute(template: &str, values: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in values {
        result = result.replace(&format!("{{{key}}}"), value);
    }
    result
}

fn merge_config(defaults: &Value, supplied: &Value) -> Result<Value, String> {
    let mut merged = defaults.as_object().cloned().unwrap_or_default();
    let incoming = supplied.as_object().ok_or("plugin configuration must be a JSON object")?;
    for (key, value) in incoming {
        merged.insert(key.clone(), value.clone());
    }
    Ok(Value::Object(merged))
}

/// Resolves a manifest-declared command or service and prepares it without invoking a shell.
fn prepare(
    plugin_id: &str,
    extension_type: &str,
    extension_id: &str,
    supplied_cfg: &Value,
) -> Result<(Command, u64), String> {
    let (plugin_dir, extension) = custom::runtime_extension(plugin_id, extension_type, extension_id)?;
    let cfg = merge_config(&extension["defaults"], supplied_cfg)?;
    let field_schema = json!({
        "fields": extension.get("fields").and_then(Value::as_array).cloned().unwrap_or_default()
    });
    spec::validate(&field_schema, &cfg)?;
    let mut values = spec::values(&field_schema, &cfg);
    let entrypoint = extension["entrypoint"].as_str().ok_or("extension entrypoint is missing")?;
    let full_entrypoint = plugin_dir.join(entrypoint);
    values.insert("pluginDir".into(), plugin_dir.to_string_lossy().to_string());
    values.insert("entrypoint".into(), full_entrypoint.to_string_lossy().to_string());
    values.insert("pluginId".into(), plugin_id.to_string());
    values.insert("extensionId".into(), extension_id.to_string());

    let runtime = &extension["runtime"];
    let program = substitute(runtime["command"].as_str().unwrap_or(""), &values);
    if program.trim().is_empty() {
        return Err("plugin runtime command resolved to an empty value".into());
    }
    let args: Vec<String> = runtime["args"].as_array()
        .ok_or("runtime.args must be an array")?
        .iter()
        .map(|arg| arg.as_str().map(|s| substitute(s, &values)).ok_or("runtime.args must contain only strings"))
        .collect::<Result<_, _>>()?;

    let mut command = Command::new(&program);
    command.args(&args)
        .current_dir(&plugin_dir)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for (key, value) in &values {
        if !matches!(key.as_str(), "pluginDir" | "entrypoint" | "pluginId" | "extensionId") {
            command.env(format!("FIELD_{}", key.to_uppercase()), value);
        }
    }
    command.env("PLUGIN_DIR", &plugin_dir)
        .env("PLUGIN_ENTRYPOINT", &full_entrypoint)
        .env("PLUGIN_ID", plugin_id)
        .env("PLUGIN_EXTENSION_ID", extension_id);
    let timeout_sec = extension["timeoutSec"].as_u64().unwrap_or(120).clamp(1, 86400);
    Ok((command, timeout_sec))
}

impl PluginHost {
    /// Execute an app-level command independently of the automation graph.
    pub async fn run_command(
        &self,
        plugin_id: String,
        command_id: String,
        cfg: Value,
    ) -> Result<PluginRunResult, String> {
        let (mut command, timeout_sec) = prepare(&plugin_id, "command", &command_id, &cfg)?;
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let started = Instant::now();
        let child = command.spawn()
            .map_err(|e| format!("could not start plugin command: {e}"))?;
        let output = match timeout(Duration::from_secs(timeout_sec), child.wait_with_output()).await {
            Ok(result) => result.map_err(|e| format!("plugin command failed while waiting: {e}"))?,
            Err(_) => return Err(format!("plugin command exceeded its {timeout_sec}s timeout and was terminated")),
        };
        let exit_code = output.status.code();
        Ok(PluginRunResult {
            plugin_id,
            command_id,
            exit_code,
            success: output.status.success(),
            stdout: tail_text(&output.stdout, 12000),
            stderr: tail_text(&output.stderr, 12000),
            elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        })
    }

    /// Start a long-lived worker process declared in the package manifest.
    pub async fn start_service(
        &self,
        plugin_id: String,
        service_id: String,
        cfg: Value,
    ) -> Result<Value, String> {
        let (mut command, _) = prepare(&plugin_id, "service", &service_id, &cfg)?;
        command.stdout(Stdio::null()).stderr(Stdio::null());
        let key = service_key(&plugin_id, &service_id);
        let mut services = self.services.lock().await;
        let existing_status = match services.get_mut(&key) {
            Some(existing) => Some(existing.child.try_wait()),
            None => None,
        };
        if let Some(status) = existing_status {
            match status {
                Ok(None) => return Err(format!("service '{key}' is already running")),
                Ok(Some(_)) => { services.remove(&key); }
                Err(e) => return Err(format!("could not inspect existing service '{key}': {e}")),
            }
        }
        let child = command.spawn()
            .map_err(|e| format!("could not start plugin service: {e}"))?;
        let pid = child.id();
        services.insert(key, ManagedService { child, pid });
        Ok(json!({"pluginId": plugin_id, "serviceId": service_id, "running": true, "pid": pid}))
    }

    pub async fn stop_service(&self, plugin_id: String, service_id: String) -> Result<Value, String> {
        let key = service_key(&plugin_id, &service_id);
        let service = self.services.lock().await.remove(&key);
        let Some(mut service) = service else {
            return Ok(json!({"pluginId": plugin_id, "serviceId": service_id, "running": false}));
        };
        match service.child.try_wait() {
            Ok(None) => {
                service.child.kill().await
                    .map_err(|e| format!("could not stop plugin service '{key}': {e}"))?;
                let _ = service.child.wait().await;
            }
            Ok(Some(_)) => {}
            Err(e) => return Err(format!("could not inspect plugin service '{key}': {e}")),
        }
        Ok(json!({"pluginId": plugin_id, "serviceId": service_id, "running": false}))
    }

    /// Stops every process managed by this host. Called during app shutdown/restart.
    pub async fn stop_all(&self) {
        let services = {
            let mut guard = self.services.lock().await;
            std::mem::take(&mut *guard)
        };
        for (_key, mut service) in services {
            if matches!(service.child.try_wait(), Ok(None)) {
                let _ = service.child.kill().await;
                let _ = service.child.wait().await;
            }
        }
    }

    /// Starts services explicitly marked autoStart in installed manifests.
    pub async fn start_auto_services(&self) -> Vec<String> {
        let listing = custom::list_external_plugins();
        let mut errors = Vec::new();
        let plugins = listing["plugins"].as_array().cloned().unwrap_or_default();
        for plugin in plugins {
            let plugin_id = plugin["id"].as_str().unwrap_or("").to_string();
            let services = plugin["services"].as_array().cloned().unwrap_or_default();
            for service in services {
                if service["autoStart"].as_bool() != Some(true) {
                    continue;
                }
                let service_id = service["id"].as_str().unwrap_or("").to_string();
                match self.start_service(plugin_id.clone(), service_id.clone(), json!({})).await {
                    Ok(_) => {}
                    Err(e) => errors.push(format!("{plugin_id}/{service_id}: {e}")),
                }
            }
        }
        errors
    }

    /// Returns package metadata with live service status for the Plugins page.
    pub async fn list_plugins(&self) -> Value {
        let statuses: HashMap<String, Option<u32>> = {
            let mut services = self.services.lock().await;
            let mut result = HashMap::new();
            let mut exited = Vec::new();
            for (key, service) in services.iter_mut() {
                match service.child.try_wait() {
                    Ok(None) => { result.insert(key.clone(), service.pid); }
                    Ok(Some(_)) => exited.push(key.clone()),
                    Err(_) => exited.push(key.clone()),
                }
            }
            for key in exited {
                services.remove(&key);
            }
            result
        };
        let mut listing = custom::list_external_plugins();
        if let Some(plugins) = listing["plugins"].as_array_mut() {
            for plugin in plugins {
                let plugin_id = plugin["id"].as_str().unwrap_or("").to_string();
                if let Some(services) = plugin["services"].as_array_mut() {
                    for service in services {
                        let service_id = service["id"].as_str().unwrap_or("").to_string();
                        let key = service_key(&plugin_id, &service_id);
                        let running = statuses.contains_key(&key);
                        service["running"] = json!(running);
                        service["pid"] = statuses.get(&key).and_then(|pid| *pid).map(|pid| json!(pid)).unwrap_or(Value::Null);
                    }
                }
            }
        }
        listing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_substitution_does_not_join_arguments_into_a_shell() {
        let mut values = HashMap::new();
        values.insert("path".into(), r"C:\My Files\sample.txt".into());
        assert_eq!(substitute("--path={path}", &values), r"--path=C:\My Files\sample.txt");
    }

    #[test]
    fn service_keys_are_scoped_to_plugin_and_service() {
        assert_ne!(service_key("a", "bc"), service_key("ab", "c"));
    }
}
