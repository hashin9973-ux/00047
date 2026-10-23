//! Per-user DPAPI-protected secrets. Plaintext is never written to disk.
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io::Write, path::{Path,PathBuf}, process::{Command,Stdio}, sync::{Mutex,OnceLock}};

struct Vault { path:PathBuf, encrypted:BTreeMap<String,String> }
static VAULT:OnceLock<Mutex<Vault>>=OnceLock::new();

fn valid_key(s:&str)->bool{!s.is_empty()&&s.len()<=64&&s.chars().next().is_some_and(|c|c.is_ascii_alphanumeric()||c=='_')&&s.chars().all(|c|c.is_ascii_alphanumeric()||matches!(c,'_'|'-'|'.'))}
fn quote(s:&str)->String{format!("'{}'",s.replace('\0',"").replace('\'',"''"))}
fn ps(script:&str,input:&str)->Result<String,String>{
 #[cfg(not(windows))]{let _=(script,input);return Err("Secret storage is available only on Windows.".into())}
 #[cfg(windows)]{
  let mut cmd=Command::new("powershell.exe");
  cmd.args(["-NoLogo","-NoProfile","-NonInteractive","-Command",script]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(false);
  use std::os::windows::process::CommandExt;cmd.creation_flags(0x08000000);
  let mut child=cmd.spawn().map_err(|e|format!("Could not start DPAPI helper: {e}"))?;
  if let Some(mut stdin)=child.stdin.take(){stdin.write_all(input.as_bytes()).map_err(|e|e.to_string())?;stdin.write_all(b"\n").map_err(|e|e.to_string())?;}
  let out=child.wait_with_output().map_err(|e|e.to_string())?;
  if !out.status.success(){return Err(format!("DPAPI helper failed: {}",String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()))}
  Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
 }
}
fn protect(value:&str)->Result<String,String>{
 let script="$ErrorActionPreference='Stop';$b=[Console]::In.ReadLine();$v=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b));$s=ConvertTo-SecureString -String $v -AsPlainText -Force;ConvertFrom-SecureString -SecureString $s";
 ps(script,&base64(value.as_bytes()))
}
fn unprotect(cipher:&str)->Result<String,String>{
 let script="$ErrorActionPreference='Stop';$c=[Console]::In.ReadLine();$s=ConvertTo-SecureString -String $c;$p=[Runtime.InteropServices.Marshal]::SecureStringToBSTR($s);try{$v=[Runtime.InteropServices.Marshal]::PtrToStringBSTR($p);[Console]::Out.Write([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($v)))}finally{[Runtime.InteropServices.Marshal]::ZeroFreeBSTR($p)}";
 let b=ps(script,cipher)?;let bytes=decode64(&b)?;String::from_utf8(bytes).map_err(|_|"Stored secret is not valid UTF-8".into())
}
fn base64(v:&[u8])->String{
 const T:&[u8;64]=b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
 let mut out=String::new();for c in v.chunks(3){let n=((c[0] as u32)<<16)|(((*c.get(1).unwrap_or(&0)) as u32)<<8)|(*c.get(2).unwrap_or(&0) as u32);out.push(T[((n>>18)&63)as usize]as char);out.push(T[((n>>12)&63)as usize]as char);out.push(if c.len()>1{T[((n>>6)&63)as usize]as char}else{'='});out.push(if c.len()>2{T[(n&63)as usize]as char}else{'='});}out
}
fn decode64(s:&str)->Result<Vec<u8>,String>{
 let mut out=Vec::new();let mut n=0u32;let mut bits=0;
 for c in s.bytes(){if c==b'=' {break}let v=match c{b'A'..=b'Z'=>(c-b'A')as u32,b'a'..=b'z'=>(c-b'a'+26)as u32,b'0'..=b'9'=>(c-b'0'+52)as u32,b'+'=>62,b'/'=>63,_=>continue};n=(n<<6)|v;bits+=6;if bits>=8{bits-=8;out.push(((n>>bits)&255)as u8)}}
 Ok(out)
}
impl Vault {
 fn flush(&self)->Result<(),String>{let data=serde_json::to_vec_pretty(&self.encrypted).map_err(|e|e.to_string())?;let tmp=self.path.with_extension("json.tmp");fs::write(&tmp,&data).map_err(|e|format!("Write encrypted vault: {e}"))?;let f=fs::OpenOptions::new().read(true).open(&tmp).map_err(|e|e.to_string())?;f.sync_all().map_err(|e|e.to_string())?;drop(f);if self.path.exists(){let backup=self.path.with_extension("json.bak");if backup.exists(){let _=fs::remove_file(&backup)};fs::rename(&self.path,&backup).map_err(|e|e.to_string())?;if let Err(e)=fs::rename(&tmp,&self.path){let _=fs::rename(&backup,&self.path);return Err(format!("Replace vault failed: {e}"))};let _=fs::remove_file(backup);}else{fs::rename(tmp,&self.path).map_err(|e|e.to_string())?}Ok(())}
}
pub fn init(path:impl AsRef<Path>)->Result<(),String>{
 let path=path.as_ref().to_path_buf();
 let mut warning=None;
 let encrypted=match fs::read(&path){
  Ok(b)=>match serde_json::from_slice::<BTreeMap<String,String>>(&b){Ok(v)=>v,Err(e)=>{let bad=path.with_file_name("secrets.corrupt-backup.json");let _=fs::rename(&path,&bad);warning=Some(format!("Encrypted secrets vault could not be parsed ({e}); the unreadable file was moved to {} and a new empty vault was created.",bad.display()));BTreeMap::new()}},
  Err(e) if e.kind()==std::io::ErrorKind::NotFound=>BTreeMap::new(),
  Err(e)=>return Err(format!("Read secrets vault: {e}"))
 };
 let _=VAULT.set(Mutex::new(Vault{path,encrypted}));
 if let Some(w)=warning{Err(w)}else{Ok(())}
}
pub fn list()->Result<Vec<String>,String>{let v=VAULT.get().ok_or("Secrets vault is not initialized")?;Ok(v.lock().map_err(|e|e.to_string())?.encrypted.keys().cloned().collect())}
pub fn get(name:&str)->Result<String,String>{if !valid_key(name){return Err("Secret name is invalid".into())};let v=VAULT.get().ok_or("Secrets vault is not initialized")?;let cipher=v.lock().map_err(|e|e.to_string())?.encrypted.get(name).cloned().ok_or_else(||format!("Secret '{name}' is not stored"))?;unprotect(&cipher)}
pub fn set(name:&str,value:&str)->Result<(),String>{if !valid_key(name){return Err("Secret name must be 1-64 ASCII letters, digits, underscore, dash, or dot".into())};if value.len()>16384{return Err("Secret values are limited to 16 KiB".into())};if value.contains('\0'){return Err("Secret value must not contain NUL".into())};let cipher=protect(value)?;let v=VAULT.get().ok_or("Secrets vault is not initialized")?;let mut v=v.lock().map_err(|e|e.to_string())?;let old=v.encrypted.insert(name.to_string(),cipher);if let Err(e)=v.flush(){if let Some(x)=old{v.encrypted.insert(name.to_string(),x);}else{v.encrypted.remove(name);}return Err(e)}Ok(())}
pub fn delete(name:&str)->Result<bool,String>{if !valid_key(name){return Err("Secret name is invalid".into())};let v=VAULT.get().ok_or("Secrets vault is not initialized")?;let mut v=v.lock().map_err(|e|e.to_string())?;let old=v.encrypted.remove(name);if old.is_none(){return Ok(false)}if let Err(e)=v.flush(){v.encrypted.insert(name.to_string(),old.unwrap());return Err(e)}Ok(true)}
#[cfg(test)]mod tests{use super::*;#[test]fn name_validation(){assert!(valid_key("token.prod_1"));assert!(!valid_key("../token"));assert!(!valid_key(""));assert!(!valid_key("a".repeat(65).as_str()))}#[test]fn base64_round_trip(){for x in [b"".as_slice(),b"abc",b"a\0b", "東京".as_bytes()]{assert_eq!(decode64(&base64(x)).unwrap(),x)}}}
