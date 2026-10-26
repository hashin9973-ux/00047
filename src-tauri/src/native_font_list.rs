use std::ffi::c_void;

const MAX_FONT_FAMILIES: usize = 2048;

#[repr(C)]
#[derive(Default)]
struct LogFontW {
    lf_height: i32,
    lf_width: i32,
    lf_escapement: i32,
    lf_orientation: i32,
    lf_weight: i32,
    lf_italic: u8,
    lf_underline: u8,
    lf_strike_out: u8,
    lf_char_set: u8,
    lf_out_precision: u8,
    lf_clip_precision: u8,
    lf_quality: u8,
    lf_pitch_and_family: u8,
    lf_face_name: [u16; 32],
}

type FontEnumProc = unsafe extern "system" fn(
    *const LogFontW,
    *const c_void,
    u32,
    isize,
) -> i32;

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(hdc: *mut c_void) -> *mut c_void;
    fn DeleteDC(hdc: *mut c_void) -> i32;
    fn EnumFontFamiliesExW(
        hdc: *mut c_void,
        logfont: *const LogFontW,
        callback: Option<FontEnumProc>,
        lparam: isize,
        flags: u32,
    ) -> i32;
}

unsafe extern "system" fn collect_font_family(
    logfont: *const LogFontW,
    _metrics: *const c_void,
    _font_type: u32,
    parameter: isize,
) -> i32 {
    if logfont.is_null() || parameter == 0 {
        return 1;
    }
    let families = &mut *(parameter as *mut Vec<String>);
    if families.len() >= MAX_FONT_FAMILIES {
        return 0;
    }
    let units = &(*logfont).lf_face_name;
    let length = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
    if length == 0 {
        return 1;
    }
    let name = String::from_utf16_lossy(&units[..length]);
    let name = name.trim();
    if !name.is_empty()
        && name.chars().count() <= 100
        && !families.iter().any(|existing| existing.eq_ignore_ascii_case(name))
    {
        families.push(name.to_owned());
    }
    if families.len() >= MAX_FONT_FAMILIES { 0 } else { 1 }
}

pub fn list() -> Vec<String> {
    let mut families = Vec::new();
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            return families;
        }
        let mut query = LogFontW::default();
        // DEFAULT_CHARSET asks GDI to enumerate installed font families.
        query.lf_char_set = 1;
        let parameter = (&mut families as *mut Vec<String>) as isize;
        let _ = EnumFontFamiliesExW(
            dc,
            &query,
            Some(collect_font_family),
            parameter,
            0,
        );
        let _ = DeleteDC(dc);
    }
    families.sort_by_key(|name| name.to_lowercase());
    families
}
