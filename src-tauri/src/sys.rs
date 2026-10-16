#![allow(non_snake_case, non_camel_case_types)]
use serde::Serialize;
use std::{
    mem::{size_of, zeroed},
    time::Instant,
};

type HANDLE = isize;
const QUERY: u32 = 0x1000; // PROCESS_QUERY_LIMITED_INFORMATION
const SET_QUOTA: u32 = 0x0100; // PROCESS_SET_QUOTA

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct FILETIME {
    lo: u32,
    hi: u32,
}
impl FILETIME {
    fn v(&self) -> u64 {
        ((self.hi as u64) << 32) | self.lo as u64
    }
}

#[repr(C)]
struct PROCESS_MEMORY_COUNTERS {
    cb: u32,
    PageFaultCount: u32,
    PeakWorkingSetSize: usize,
    WorkingSetSize: usize,
    QuotaPeakPagedPoolUsage: usize,
    QuotaPagedPoolUsage: usize,
    QuotaPeakNonPagedPoolUsage: usize,
    QuotaNonPagedPoolUsage: usize,
    PagefileUsage: usize,
    PeakPagefileUsage: usize,
}

#[repr(C)]
struct MEMORYSTATUSEX {
    dwLength: u32,
    dwMemoryLoad: u32,
    ullTotalPhys: u64,
    ullAvailPhys: u64,
    ullTotalPageFile: u64,
    ullAvailPageFile: u64,
    ullTotalVirtual: u64,
    ullAvailVirtual: u64,
    ullAvailExtendedVirtual: u64,
}

#[repr(C)]
struct PROCESSENTRY32W {
    dwSize: u32,
    cntUsage: u32,
    th32ProcessID: u32,
    th32DefaultHeapID: usize,
    th32ModuleID: u32,
    cntThreads: u32,
    th32ParentProcessID: u32,
    pcPriClassBase: i32,
    dwFlags: u32,
    szExeFile: [u16; 260],
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> HANDLE;
    fn CloseHandle(h: HANDLE) -> i32;
    fn GetCurrentProcessId() -> u32;
    fn GetLastError() -> u32;
    fn K32GetProcessMemoryInfo(h: HANDLE, p: *mut PROCESS_MEMORY_COUNTERS, cb: u32) -> i32;
    fn GetProcessWorkingSetSizeEx(h: HANDLE, min: *mut usize, max: *mut usize, flags: *mut u32) -> i32;
    fn SetProcessWorkingSetSizeEx(h: HANDLE, min: usize, max: usize, flags: u32) -> i32;
    fn GetProcessTimes(h: HANDLE, c: *mut FILETIME, e: *mut FILETIME, k: *mut FILETIME, u: *mut FILETIME) -> i32;
    fn GlobalMemoryStatusEx(p: *mut MEMORYSTATUSEX) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> HANDLE;
    fn Process32FirstW(s: HANDLE, e: *mut PROCESSENTRY32W) -> i32;
    fn Process32NextW(s: HANDLE, e: *mut PROCESSENTRY32W) -> i32;
}
#[link(name = "user32")]
extern "system" {
    fn GetForegroundWindow() -> HANDLE;
    fn GetWindowThreadProcessId(h: HANDLE, pid: *mut u32) -> u32;
}
#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteW(hwnd: HANDLE, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> HANDLE;
}

fn err(what: &str) -> String {
    let c = unsafe { GetLastError() };
    if c == 5 {
        format!("{what}: access denied (protected or elevated process; try running 00047 as administrator)")
    } else {
        format!("{what} failed (Windows error {c})")
    }
}

struct Proc(HANDLE);
impl Proc {
    fn open(pid: u32, access: u32) -> Result<Proc, String> {
        let h = unsafe { OpenProcess(access, 0, pid) };
        if h == 0 {
            Err(err("OpenProcess"))
        } else {
            Ok(Proc(h))
        }
    }
}
impl Drop for Proc {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn ws_of(p: &Proc) -> Result<usize, String> {
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS = zeroed();
        c.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(p.0, &mut c, c.cb) == 0 {
            Err(err("GetProcessMemoryInfo"))
        } else {
            Ok(c.WorkingSetSize)
        }
    }
}

fn cpu_of(p: &Proc) -> u64 {
    unsafe {
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        if GetProcessTimes(p.0, &mut c, &mut e, &mut k, &mut u) == 0 {
            0
        } else {
            k.v() + u.v()
        }
    }
}

fn mb(b: usize) -> f64 {
    b as f64 / 1_048_576.0
}

pub fn own_pid() -> u32 {
    unsafe { GetCurrentProcessId() }
}

fn snapshot() -> Vec<(u32, u32, String)> {
    let mut out = vec![];
    unsafe {
        let s = CreateToolhelp32Snapshot(0x2, 0); // TH32CS_SNAPPROCESS
        if s == -1 || s == 0 {
            return out;
        }
        let mut e: PROCESSENTRY32W = zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(s, &mut e);
        while ok != 0 {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
            out.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
            ok = Process32NextW(s, &mut e);
        }
        CloseHandle(s);
    }
    out
}

pub fn process_name(pid: u32) -> String {
    snapshot().into_iter().find(|p| p.0 == pid).map(|p| p.2).unwrap_or_else(|| format!("pid {pid}"))
}

/// Finds a running process by exe name (with or without .exe); picks the one with the largest working set.
pub fn find_pid_by_name(name: &str) -> Result<u32, String> {
    let want = name.trim().to_lowercase();
    let want_exe = if want.ends_with(".exe") { want.clone() } else { format!("{want}.exe") };
    let mut best: Option<(u32, usize)> = None;
    for (pid, _, n) in snapshot() {
        let n = n.to_lowercase();
        if n == want || n == want_exe {
            let ws = Proc::open(pid, QUERY).and_then(|p| ws_of(&p)).unwrap_or(0);
            if best.map(|b| ws >= b.1).unwrap_or(true) {
                best = Some((pid, ws));
            }
        }
    }
    best.map(|b| b.0).ok_or_else(|| format!("process '{}' is not running", name.trim()))
}

pub fn foreground_pid() -> Result<u32, String> {
    unsafe {
        let h = GetForegroundWindow();
        if h == 0 {
            return Err("there is no foreground window right now".into());
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, &mut pid);
        if pid == 0 {
            Err("could not read the foreground window's process".into())
        } else {
            Ok(pid)
        }
    }
}

pub fn working_set_bytes(pid: u32) -> Result<usize, String> {
    ws_of(&Proc::open(pid, QUERY)?)
}

pub fn system_used_mb() -> Result<f64, String> {
    unsafe {
        let mut m: MEMORYSTATUSEX = zeroed();
        m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut m) == 0 {
            return Err(err("GlobalMemoryStatusEx"));
        }
        Ok((m.ullTotalPhys - m.ullAvailPhys) as f64 / 1_048_576.0)
    }
}

/// Working-set trimming. percent = share of the CURRENT working set to release.
/// 100 = full trim (EmptyWorkingSet equivalent). 1..99 = temporarily lower the soft maximum to
/// (100-percent)% of the current working set, then restore the original limits so the process is
/// never left capped. Pages move to the standby/modified lists (soft trim) and the process can
/// fault them back in. The real before/after sizes are reported.
pub fn trim_working_set(pid: u32, percent: f64) -> Result<String, String> {
    if !(0.0..=100.0).contains(&percent) {
        return Err("percent must be between 0 and 100".into());
    }
    let p = Proc::open(pid, QUERY | SET_QUOTA)?;
    let before = ws_of(&p)?;
    if percent <= 0.0 {
        return Ok(format!("0% requested, nothing to do ({:.0} MB)", mb(before)));
    }
    unsafe {
        if percent >= 100.0 {
            if SetProcessWorkingSetSizeEx(p.0, usize::MAX, usize::MAX, 0) == 0 {
                return Err(err("SetProcessWorkingSetSizeEx"));
            }
        } else {
            let (mut mn, mut mx, mut fl) = (0usize, 0usize, 0u32);
            if GetProcessWorkingSetSizeEx(p.0, &mut mn, &mut mx, &mut fl) == 0 {
                return Err(err("GetProcessWorkingSetSizeEx"));
            }
            let target = (((before as f64) * (1.0 - percent / 100.0)) as usize).max(1 << 20);
            if target >= before {
                return Ok(format!("already at or below the target ({:.0} MB)", mb(before)));
            }
            if SetProcessWorkingSetSizeEx(p.0, mn.min(target), target, 0) == 0 {
                return Err(err("SetProcessWorkingSetSizeEx"));
            }
            SetProcessWorkingSetSizeEx(p.0, mn, mx, fl); // restore original limits
        }
    }
    let after = ws_of(&p).unwrap_or(0);
    Ok(format!("working set {:.0} MB -> {:.0} MB (requested {percent:.0}% release)", mb(before), mb(after)))
}

pub fn start_file(path: &str, args: &str) -> Result<(), String> {
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (op, f, a) = (w("open"), w(path), w(args));
    let r = unsafe {
        ShellExecuteW(0, op.as_ptr(), f.as_ptr(), if args.is_empty() { std::ptr::null() } else { a.as_ptr() }, std::ptr::null(), 1)
    };
    if r > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecute failed (code {r})"))
    }
}

#[derive(Default)]
pub struct CpuState {
    last: Option<(Instant, u64)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStats {
    pub cpu: f64,
    pub ram_mb: f64,
    pub procs: usize,
}

/// CPU % and working set of 00047 plus its child processes (the WebView2 helpers).
pub fn app_stats(st: &mut CpuState) -> AppStats {
    let me = own_pid();
    let list = snapshot();
    let mut set = vec![me];
    loop {
        let before = set.len();
        for (pid, ppid, _) in &list {
            if set.contains(ppid) && !set.contains(pid) {
                set.push(*pid);
            }
        }
        if set.len() == before {
            break;
        }
    }
    let (mut ram, mut ticks, mut n) = (0usize, 0u64, 0usize);
    for pid in set {
        if let Ok(p) = Proc::open(pid, QUERY) {
            ram += ws_of(&p).unwrap_or(0);
            ticks += cpu_of(&p);
            n += 1;
        }
    }
    let now = Instant::now();
    let cores = std::thread::available_parallelism().map(|c| c.get()).unwrap_or(1) as f64;
    let cpu = match st.last {
        Some((t, c)) => {
            let wall = now.duration_since(t).as_secs_f64() * 1e7;
            if wall > 0.0 {
                (ticks.saturating_sub(c) as f64 / wall / cores * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            }
        }
        None => 0.0,
    };
    st.last = Some((now, ticks));
    AppStats { cpu, ram_mb: mb(ram), procs: n }
}


/// Sends WM_CLOSE to a process's top-level windows, then force-terminates it after 5 seconds.
#[cfg(windows)]
pub fn stop_process(pid: u32, force: bool) -> Result<String, String> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
    use windows::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };

    unsafe extern "system" fn close_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let mut window_pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut window_pid));
        if window_pid == lp.0 as u32 {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        BOOL(1)
    }

    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
        .map_err(|e| format!("open process {pid}: {e}"))?;
    let result = (|| -> Result<String, String> {
        if !force {
            unsafe { let _ = EnumWindows(Some(close_cb), LPARAM(pid as isize)); }
            if unsafe { WaitForSingleObject(handle, 5000) } == WAIT_OBJECT_0 {
                return Ok("closed gracefully".to_string());
            }
        }
        unsafe { TerminateProcess(handle, 1) }.map_err(|e| format!("TerminateProcess: {e}"))?;
        unsafe { WaitForSingleObject(handle, 3000); }
        Ok(if force { "force-killed".to_string() }
            else { "did not close within 5 s; force-killed".to_string() })
    })();
    unsafe { let _ = CloseHandle(handle); }
    result
}

/// Displays a Windows toast using the built-in PowerShell AppUserModelID.
/// Toast delivery for unpackaged desktop apps depends on Windows notification registration.
#[cfg(windows)]
pub fn show_notification(title: &str, body: &str, sound: bool) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
        .map_err(|e| format!("RoInitialize: {e}"))?;
    let result = (|| -> Result<(), String> {
        let esc = |s: &str| {
            s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
                .replace('"', "&quot;").replace('\'', "&apos;")
        };
        let audio = if sound { "" } else { "<audio silent=\"true\"/>" };
        let xml = format!(
            "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{}</toast>",
            esc(title), esc(body), audio
        );
        let doc = XmlDocument::new().map_err(|e| format!("XmlDocument: {e}"))?;
        doc.LoadXml(&HSTRING::from(xml)).map_err(|e| format!("LoadXml: {e}"))?;
        let toast = ToastNotification::CreateToastNotification(&doc).map_err(|e| format!("toast: {e}"))?;
        let app_id = HSTRING::from(
            r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe",
        );
        ToastNotificationManager::CreateToastNotifierWithId(&app_id)
            .map_err(|e| format!("notifier: {e}"))?
            .Show(&toast)
            .map_err(|e| format!("show: {e}"))
    })();
    unsafe { RoUninitialize() };
    result
}


#[repr(C)]
struct SYSTEMTIME {
    wYear: u16,
    wMonth: u16,
    wDayOfWeek: u16,
    wDay: u16,
    wHour: u16,
    wMinute: u16,
    wSecond: u16,
    wMilliseconds: u16,
}
#[link(name = "kernel32")]
extern "system" {
    fn GetLocalTime(t: *mut SYSTEMTIME);
}

/// (weekday 0 = Sunday .. 6 = Saturday, minutes since local midnight)
pub fn local_weekday_minutes() -> (u16, u32) {
    unsafe {
        let mut t: SYSTEMTIME = zeroed();
        GetLocalTime(&mut t);
        (t.wDayOfWeek, t.wHour as u32 * 60 + t.wMinute as u32)
    }
}
