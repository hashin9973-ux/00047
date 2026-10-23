//! Per-user encrypted secret vault backed by Windows DPAPI.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Default)]
pub struct SecretVault {
    path: PathBuf,
    gate: Mutex<()>,
}

#[derive(Serialize, Deserialize, Default)]
struct EncryptedVault {
    version: u32,
    secrets: HashMap<String, String>,
}

impl SecretVault {
    pub fn new(path: PathBuf) -> Self {
        Self { path, gate: Mutex::new(()) }
    }

    fn read_ciphertext(&self) -> Result<EncryptedVault, String> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                let vault: EncryptedVault = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("Secret vault file is invalid: {e}"))?;
                if vault.version != 1 {
                    return Err(format!("Unsupported secret vault version {}", vault.version));
                }
                Ok(vault)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(EncryptedVault { version: 1, secrets: HashMap::new() }),
            Err(e) => Err(format!("Could not read secret vault: {e}")),
        }
    }

    fn write_ciphertext(&self, vault: &EncryptedVault) -> Result<(), String> {
        let parent = self.path.parent().ok_or("Secret vault path has no parent directory")?;
        fs::create_dir_all(parent).map_err(|e| format!("Could not create secret vault directory: {e}"))?;
        let bytes = serde_json::to_vec_pretty(vault).map_err(|e| format!("Could not encode secret vault: {e}"))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, bytes).map_err(|e| format!("Could not write temporary secret vault: {e}"))?;
        if let Err(e) = replace_file(&tmp, &self.path) {
            let _ = fs::remove_file(&tmp);
            return Err(format!("Could not atomically replace secret vault: {e}"));
        }
        Ok(())
    }

    pub fn list_names(&self) -> Result<Vec<String>, String> {
        let _guard = self.gate.lock().map_err(|e| e.to_string())?;
        let mut names: Vec<_> = self.read_ciphertext()?.secrets.into_keys().collect();
        names.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
        Ok(names)
    }

    pub fn save(&self, name: &str, value: &str) -> Result<(), String> {
        validate_name(name)?;
        if value.is_empty() { return Err("Secret value must not be empty".into()); }
        if value.len() > 16 * 1024 { return Err("Secret value is too large (maximum 16 KiB)".into()); }
        if value.contains('\0') { return Err("Secret value contains a NUL character".into()); }
        let _guard = self.gate.lock().map_err(|e| e.to_string())?;
        let mut vault = self.read_ciphertext()?;
        vault.version = 1;
        vault.secrets.insert(name.to_string(), to_hex(&protect(value.as_bytes())?));
        self.write_ciphertext(&vault)
    }

    pub fn delete(&self, name: &str) -> Result<bool, String> {
        validate_name(name)?;
        let _guard = self.gate.lock().map_err(|e| e.to_string())?;
        let mut vault = self.read_ciphertext()?;
        if vault.secrets.remove(name).is_none() { return Ok(false); }
        self.write_ciphertext(&vault)?;
        Ok(true)
    }

    /// Loads decrypted secrets for the current automation run. Callers keep values in
    /// the per-run variable store and must never log or persist those plaintext values.
    pub fn load_for_run(&self) -> Result<HashMap<String, String>, String> {
        let _guard = self.gate.lock().map_err(|e| e.to_string())?;
        let vault = self.read_ciphertext()?;
        let mut values = HashMap::new();
        for (name, ciphertext) in vault.secrets {
            let bytes = from_hex(&ciphertext)?;
            let plaintext = unprotect(&bytes)?;
            let value = String::from_utf8(plaintext)
                .map_err(|_| format!("Secret '{name}' is not valid UTF-8"))?;
            values.insert(format!("secret.{name}"), value);
        }
        Ok(values)
    }
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64
        || !name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err("Secret name must start with a letter, digit, or underscore and contain only letters, digits, _, -, or . (maximum 64 characters)".into());
    }
    Ok(())
}
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
fn from_hex(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 2 != 0 { return Err("Secret vault ciphertext has invalid hex length".into()); }
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks_exact(2) {
        fn nibble(ch: u8) -> Option<u8> {
            match ch { b'0'..=b'9' => Some(ch - b'0'), b'a'..=b'f' => Some(ch - b'a' + 10), b'A'..=b'F' => Some(ch - b'A' + 10), _ => None }
        }
        let hi = nibble(pair[0]).ok_or("Secret vault ciphertext contains invalid hex")?;
        let lo = nibble(pair[1]).ok_or("Secret vault ciphertext contains invalid hex")?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

#[cfg(windows)]
#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}
#[cfg(windows)]
#[link(name = "Crypt32")]
extern "system" {
    fn CryptProtectData(input: *const DataBlob, description: *const u16, entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void, prompt: *const std::ffi::c_void, flags: u32, output: *mut DataBlob) -> i32;
    fn CryptUnprotectData(input: *const DataBlob, description: *mut *mut u16, entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void, prompt: *const std::ffi::c_void, flags: u32, output: *mut DataBlob) -> i32;
}
#[cfg(windows)]
#[link(name = "Kernel32")]
extern "system" {
    fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
}

#[cfg(windows)]
fn protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use std::{ffi::c_void, ptr};
    let len = u32::try_from(plaintext.len()).map_err(|_| "Secret value is too large")?;
    let input = DataBlob { cb_data: len, pb_data: plaintext.as_ptr() as *mut u8 };
    let mut output = DataBlob { cb_data: 0, pb_data: ptr::null_mut() };
    let ok = unsafe { CryptProtectData(&input, ptr::null(), ptr::null(), ptr::null_mut(), ptr::null(), 1, &mut output) };
    if ok == 0 { return Err(format!("Windows DPAPI encryption failed: {}", std::io::Error::last_os_error())); }
    let result = unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize).to_vec() };
    unsafe { let _ = LocalFree(output.pb_data as *mut c_void); }
    Ok(result)
}
#[cfg(not(windows))]
fn protect(_: &[u8]) -> Result<Vec<u8>, String> {
    Err("The Secret Vault requires Windows DPAPI and is unavailable on this platform".into())
}

#[cfg(windows)]
fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    use std::{ffi::c_void, ptr};
    let len = u32::try_from(ciphertext.len()).map_err(|_| "Secret ciphertext is too large")?;
    let input = DataBlob { cb_data: len, pb_data: ciphertext.as_ptr() as *mut u8 };
    let mut output = DataBlob { cb_data: 0, pb_data: ptr::null_mut() };
    let ok = unsafe { CryptUnprotectData(&input, ptr::null_mut(), ptr::null(), ptr::null_mut(), ptr::null(), 1, &mut output) };
    if ok == 0 { return Err(format!("Windows DPAPI decryption failed: {}", std::io::Error::last_os_error())); }
    let result = unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize).to_vec() };
    unsafe { let _ = LocalFree(output.pb_data as *mut c_void); }
    Ok(result)
}
#[cfg(not(windows))]
fn unprotect(_: &[u8]) -> Result<Vec<u8>, String> {
    Err("The Secret Vault requires Windows DPAPI and is unavailable on this platform".into())
}

#[cfg(windows)]
fn replace_file(src: &Path, dst: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<u16> = src.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let target: Vec<u16> = dst.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let ok = unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 0x1 | 0x8) };
    if ok == 0 { Err(std::io::Error::last_os_error().to_string()) } else { Ok(()) }
}
#[cfg(not(windows))]
fn replace_file(src: &Path, dst: &Path) -> Result<(), String> {
    fs::rename(src, dst).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_secret_names_and_hex_roundtrips() {
        assert!(validate_name("my_api_key-1").is_ok());
        assert!(validate_name("team.token.prod").is_ok());
        assert!(validate_name("../unsafe").is_err());
        assert!(validate_name("bad name").is_err());
        assert_eq!(from_hex(&to_hex(&[0, 1, 127, 128, 255])).unwrap(), vec![0, 1, 127, 128, 255]);
        assert!(from_hex("abc").is_err());
        assert!(from_hex("gg").is_err());
    }
}
