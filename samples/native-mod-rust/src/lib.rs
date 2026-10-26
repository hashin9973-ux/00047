use std::sync::atomic::{AtomicBool, Ordering};

static INITIALIZED: AtomicBool = AtomicBool::new(false);

#[no_mangle]
pub extern "system" fn WmodGetApiVersion() -> u32 {
    2
}

#[no_mangle]
pub extern "system" fn WmodInitializeV1() -> i32 {
    INITIALIZED.store(true, Ordering::Release);
    1
}

#[no_mangle]
pub extern "system" fn WmodShutdownV1() {
    INITIALIZED.store(false, Ordering::Release);
}

#[no_mangle]
pub extern "system" fn WmodIsInitializedV1() -> i32 {
    if INITIALIZED.load(Ordering::Acquire) { 1 } else { 0 }
}

#[no_mangle]
pub extern "system" fn WmodCanUnloadV1() -> i32 {
    if !INITIALIZED.load(Ordering::Acquire) { 1 } else { 0 }
}
