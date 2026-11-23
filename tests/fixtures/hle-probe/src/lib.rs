//! A stand-in for a title, written by us. It imports from DLLs that do not
//! exist on a PC, which is exactly the situation victory-loader handles.
//! Loaded normally, this DLL would fail with "module not found".

#[link(name = "xgameruntime", kind = "raw-dylib")]
unsafe extern "system" {
    fn XGameRuntimeInitialize() -> i32;
}

#[link(name = "victory-probe-missing", kind = "raw-dylib")]
unsafe extern "system" {
    fn VIctoryProbeNotImplemented(x: u64) -> u64;
}

// A DLL the "title" ships; the test supplies a renamed copy of version.dll.
#[link(name = "victory-probe-title", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetFileVersionInfoSizeW(path: *const u16, handle: *mut u32) -> u32;
}

// Delay-loaded (see build.rs): bound through the delay-load table.
#[link(name = "victory-probe-delayed", kind = "raw-dylib")]
unsafe extern "system" {
    fn VIctoryProbeDelayed() -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetTickCount() -> u32;
    fn GetCurrentProcessId() -> u32;
}

/// Console-only API, answered by the resolver's HLE implementation.
#[unsafe(no_mangle)]
pub extern "system" fn probe_xgameruntime_init() -> i32 {
    unsafe { XGameRuntimeInitialize() }
}

/// Exists on the host too, but the resolver overrides it.
#[unsafe(no_mangle)]
pub extern "system" fn probe_tick() -> u32 {
    unsafe { GetTickCount() }
}

/// Exists on the host and is passed through to it.
#[unsafe(no_mangle)]
pub extern "system" fn probe_pid() -> u32 {
    unsafe { GetCurrentProcessId() }
}

/// An export of a title-shipped DLL: the version-info size of kernel32.dll.
#[unsafe(no_mangle)]
pub extern "system" fn probe_title() -> u32 {
    let path: Vec<u16> = r"C:\Windows\System32\kernel32.dll"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut handle = 0;
    unsafe { GetFileVersionInfoSizeW(path.as_ptr(), &mut handle) }
}

/// A delay-loaded console API.
#[unsafe(no_mangle)]
pub extern "system" fn probe_delayed() -> u32 {
    unsafe { VIctoryProbeDelayed() }
}

/// Nobody implements this one: it must land in an unimplemented-import stub.
#[unsafe(no_mangle)]
pub extern "system" fn probe_missing() -> u64 {
    unsafe { VIctoryProbeNotImplemented(42) }
}
