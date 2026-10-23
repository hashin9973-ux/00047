//! Per-Windows-user secret vault. Values are encrypted with Windows DPAPI through
//! PowerShell's ConvertFrom-SecureString default, never stored as plaintext files.
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
};

static DIR: OnceLock<PathBuf> = OnceLock::new();
static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
fn cache()-> &'static Mutex<HashMap<String,String>>{CACHE.get_or_init(||Mutex::new(HashMap::new()))}
pub fn init(path:PathBuf)->Result<(),String>{
    fs::create_dir_all(&path).map_err(|e|format!("create secrets directory: {e}"))?;
    DIR.set(path).map_err(|_|"Secret vault has already been initialized".to_string())?;
    Ok(())
}
fn dir()->Result<&'static Path, String>{DIR.get().map(|p|p.as_path()).ok_or_else(||"Secret vault has not been initialized".into())}
fn validate_name(name:&str)->Result<(),String>{
    if name.is_empty()||name.len()>64||!name.chars().next().is_some_and(|c|c.is_ascii_alphanumeric())||
      !name.chars().all(|c|c.is_ascii_alphanumeric()||matches!(c,'_'|'-')){
        return Err("Secret names must start with a letter or number and use only letters, numbers, _ or - (max 64 characters)".into())
    }
    Ok(())
}
fn file(name:&str)->Result<PathBuf,String>{validate_name(name)?;Ok(dir()?.join(format!("{name}.dpapi")))}
fn hex(bytes:&[u8])->String{bytes.iter().map(|b|format!("{b:02X}")).collect()}
fn unhex(s:&str)->Result<Vec<u8>,String>{
    let s=s.trim();if s.len()%2!=0||s.len()>2*1024*1024{return Err("Invalid encrypted secret encoding".into())}
    (0..s.len()).step_by(2).map(|i|u8::from_str_radix(&s[i..i+2],16).map_err(|_|"Invalid encrypted secret encoding".into())).collect()
}
#[cfg(windows)]
fn powershell(script:&str, stdin:Option<&[u8]>)->Result<String,String>{
    use std::os::windows::process::CommandExt;
    let mut cmd=Command::new("powershell.exe");
    cmd.args(["-NoLogo","-NoProfile","-NonInteractive","-ExecutionPolicy","Bypass","-Command",script])
        .stdin(if stdin.is_some(){Stdio::piped()}else{Stdio::null()})
        .stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(0x0800_0000);
    let mut child=cmd.spawn().map_err(|e|format!("Could not start Windows secret vault helper: {e}"))?;
    if let Some(bytes)=stdin {
        let mut input=child.stdin.take().ok_or("Secret vault input pipe unavailable")?;
        input.write_all(bytes).map_err(|e|format!("write secret helper input: {e}"))?;
        drop(input);
    }
    let out=child.wait_with_output().map_err(|e|format!("Wait for secret helper: {e}"))?;
    if !out.status.success(){
        return Err(format!("Windows secret-vault operation failed: {}",String::from_utf8_lossy(&out.stderr).chars().take(400).collect::<String>()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
#[cfg(not(windows))]
fn powershell(_script:&str,_stdin:Option<&[u8]>)->Result<String,String>{Err("The secret vault is supported only on Windows.".into())}

pub fn set_secret(name:&str,value:&str)->Result<(),String>{
    validate_name(name)?;
    if value.len()>32768{return Err("Secret values are limited to 32 KiB".into())}
    if value.contains('\0'){return Err("Secret value must not contain NUL characters".into())}
    let target=file(name)?;
    let tmp=target.with_extension(format!("dpapi.{}.tmp",std::process::id()));
    let backup=target.with_extension(format!("dpapi.{}.bak",std::process::id()));
    // Hex input keeps plaintext out of process arguments and avoids console code-page loss.
    let encoded=hex(value.as_bytes());
    let script="$ErrorActionPreference='Stop'; $hex=[Console]::In.ReadToEnd().Trim(); if(($hex.Length % 2) -ne 0){throw 'Invalid UTF8 hex input'}; $bytes=New-Object byte[] ($hex.Length / 2); for($i=0;$i -lt $bytes.Length;$i++){ $bytes[$i]=[Convert]::ToByte($hex.Substring($i*2,2),16) }; $plain=[Text.Encoding]::UTF8.GetString($bytes); $secure=ConvertTo-SecureString -String $plain -AsPlainText -Force; $enc=ConvertFrom-SecureString -SecureString $secure; [Console]::Out.Write($enc); $plain=$null; $secure.Dispose();";
    let encrypted=powershell(script,Some(encoded.as_bytes()))?;
    if encrypted.is_empty(){return Err("Windows did not return encrypted secret data".into())}
    fs::write(&tmp,encrypted.as_bytes()).map_err(|e|format!("write encrypted secret: {e}"))?;
    let had_old=target.exists();
    if had_old {
        if backup.exists(){fs::remove_file(&backup).map_err(|e|format!("remove stale secret backup: {e}"))?;}
        if let Err(e)=fs::rename(&target,&backup){let _=fs::remove_file(&tmp);return Err(format!("stage previous encrypted secret: {e}"));}
    }
    if let Err(e)=fs::rename(&tmp,&target){
        let _=fs::remove_file(&tmp);
        let restored=if had_old {fs::rename(&backup,&target).is_ok()} else {true};
        return Err(format!("install encrypted secret: {e}; previous secret restoration was {}",if restored{"successful"}else{"unsuccessful"}));
    }
    if had_old {let _=fs::remove_file(&backup);}
    if let Ok(mut c)=cache().lock(){c.remove(name);}
    Ok(())
}
pub fn get_secret(name:&str)->Result<String,String>{
    validate_name(name)?;
    if let Some(v)=cache().lock().map_err(|e|e.to_string())?.get(name).cloned(){return Ok(v)}
    let path=file(name)?;
    let enc=fs::read_to_string(&path).map_err(|e|format!("Could not read secret '{name}': {e}"))?;
    let p=path.to_string_lossy().replace("'","''");
    let script=format!("$ErrorActionPreference='Stop'; $enc=Get-Content -LiteralPath '{}' -Raw; $secure=ConvertTo-SecureString -String $enc; $b=[Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure); try{{$plain=[Runtime.InteropServices.Marshal]::PtrToStringBSTR($b);$bytes=[Text.Encoding]::UTF8.GetBytes($plain);[Console]::Out.Write([BitConverter]::ToString($bytes).Replace('-',''))}}finally{{[Runtime.InteropServices.Marshal]::ZeroFreeBSTR($b);$secure.Dispose()}}",$p);
    let plain=String::from_utf8(unhex(&powershell(&script,None)?)?).map_err(|e|format!("Secret is not UTF-8: {e}"))?;
    if let Ok(mut c)=cache().lock(){c.insert(name.to_string(),plain.clone());}
    Ok(plain)
}
pub fn list_secrets()->Result<Vec<String>,String>{
    let mut names=Vec::new();
    for item in fs::read_dir(dir()?).map_err(|e|format!("list secrets: {e}"))?{
        let item=item.map_err(|e|e.to_string())?;let path=item.path();
        if path.is_file()&&path.extension().and_then(|x|x.to_str())==Some("dpapi"){
            if let Some(stem)=path.file_stem().and_then(|s|s.to_str()){
                if validate_name(stem).is_ok(){names.push(stem.to_string())}
            }
        }
    }
    names.sort();Ok(names)
}
pub fn delete_secret(name:&str)->Result<(),String>{
    let path=file(name)?;
    match fs::remove_file(path){Ok(())=>{},Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Err(format!("Secret '{name}' was not found")),Err(e)=>return Err(format!("Delete secret: {e}"))}
    if let Ok(mut c)=cache().lock(){c.remove(name);}
    Ok(())
}

#[cfg(test)]
mod tests{
 use super::*;
 #[test]fn names_are_safe_for_file_storage(){
  assert!(validate_name("api-key_1").is_ok());
  assert!(validate_name("../secret").is_err());
  assert!(validate_name("folder/name").is_err());
  assert!(validate_name(".").is_err());
  assert!(validate_name("has space").is_err());
 }
 #[test]fn hex_roundtrips_utf8(){let s="hello 🔒 café";assert_eq!(String::from_utf8(unhex(&hex(s.as_bytes())).unwrap()).unwrap(),s);}
}
