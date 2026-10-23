use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};

pub struct SecretVault {
    path: PathBuf,
    entries: Mutex<HashMap<String, String>>,
}

impl SecretVault {
    pub fn new(path: PathBuf) -> Self {
        let entries = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<HashMap<String, String>>(&bytes).ok())
            .unwrap_or_default();
        Self { path, entries: Mutex::new(entries) }
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.entries.lock().unwrap().keys().cloned().collect();
        names.sort();
        names
    }

    pub fn set(&self, name: &str, value: &str) -> Result<(), String> {
        if !valid_name(name) { return Err("Secret name must start with a letter or underscore and contain only letters, numbers, dash, underscore, or dot (maximum 64 characters).".into()); }
        if value.is_empty() { return Err("Secret value must not be empty.".into()); }
        if value.len() > 16384 || value.contains('\0') { return Err("Secret value exceeds 16 KiB or contains a NUL character.".into()); }
        let protected = protect(value)?;
        let mut guard = self.entries.lock().map_err(|_| "Secret vault is unavailable".to_string())?;
        let mut next = guard.clone();
        next.insert(name.to_string(), protected);
        self.persist(&next)?;
        *guard = next;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Result<String, String> {
        let protected = self.entries.lock().map_err(|_| "Secret vault is unavailable".to_string())?
            .get(name).cloned().ok_or_else(|| format!("Secret '{name}' was not found. Add it under Settings → Secrets."))?;
        unprotect(&protected)
    }

    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let mut guard = self.entries.lock().map_err(|_| "Secret vault is unavailable".to_string())?;
        let mut next = guard.clone();
        let existed = next.remove(name).is_some();
        if existed { self.persist(&next)?; *guard = next; }
        Ok(existed)
    }

    fn persist(&self, values: &HashMap<String, String>) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(values).map_err(|e| e.to_string())?;
        if let Some(parent) = self.path.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
        let tmp = self.path.with_extension("json.tmp");
        let mut f = fs::File::create(&tmp).map_err(|e| format!("Could not create vault temp file: {e}"))?;
        f.write_all(&bytes).map_err(|e| format!("Could not write vault: {e}"))?;
        f.sync_all().map_err(|e| format!("Could not flush vault: {e}"))?;
        drop(f);
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(|e| format!("Could not replace vault: {e}"))?;
        }
        fs::rename(&tmp, &self.path).map_err(|e| format!("Could not install vault: {e}"))
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}
fn ps_quote(s: &str) -> String { format!("'{}'", s.replace('\0', "").replace('\'', "''")) }

#[cfg(windows)]
fn ps(script: &str) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let mut child = Command::new("powershell.exe");
    child.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", "-"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .creation_flags(0x0800_0000);
    let mut child = child.spawn().map_err(|e| format!("Could not start Windows protected-secret service: {e}"))?;
    use std::io::Write as _;
    child.stdin.take().ok_or("Secret service stdin unavailable")?.write_all(script.as_bytes()).map_err(|e| e.to_string())?;
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Windows secret protection failed: {}", detail.chars().take(300).collect::<String>()));
    }
    String::from_utf8(output.stdout).map(|s| s.trim_end_matches(['\r','\n']).to_string()).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn protect(value: &str) -> Result<String, String> {
    let script = format!("$ErrorActionPreference='Stop'; $s=ConvertTo-SecureString -String {} -AsPlainText -Force; ConvertFrom-SecureString -SecureString $s", ps_quote(value));
    ps(&script)
}
#[cfg(windows)]
fn unprotect(value: &str) -> Result<String, String> {
    let script = format!("$ErrorActionPreference='Stop'; $s=ConvertTo-SecureString -String {} -ErrorAction Stop; [Console]::Out.Write([System.Net.NetworkCredential]::new('', $s).Password)", ps_quote(value));
    ps(&script)
}
#[cfg(not(windows))]
fn protect(_value: &str) -> Result<String, String> { Err("The encrypted secret vault is supported on Windows only.".into()) }
#[cfg(not(windows))]
fn unprotect(_value: &str) -> Result<String, String> { Err("The encrypted secret vault is supported on Windows only.".into()) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn name_validation_rejects_invalid_secret_names() {
        assert!(valid_name("API_KEY"));
        assert!(valid_name("db.password"));
        assert!(!valid_name("1bad"));
        assert!(!valid_name("name with spaces"));
        assert!(!valid_name(""));
    }
    #[test]
    fn vault_lists_only_names_and_deletes_atomically() {
        let dir = std::env::temp_dir().join(format!("00047-vault-test-{}", std::process::id()));
        let path = dir.join("secrets.json");
        let v = SecretVault::new(path.clone());
        {
            let mut guard = v.entries.lock().unwrap();
            guard.insert("API_KEY".into(), "ciphertext-not-plaintext".into());
            v.persist(&guard).unwrap();
        }
        assert_eq!(v.names(), vec!["API_KEY"]);
        let parsed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed["API_KEY"], "ciphertext-not-plaintext");
        assert!(v.delete("API_KEY").unwrap());
        assert!(!v.delete("API_KEY").unwrap());
        let _ = fs::remove_dir_all(dir);
    }
}
