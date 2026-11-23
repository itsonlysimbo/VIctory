//! Overrides of shared Windows APIs whose console meaning differs.
//!
//! Only the differences live here; everything else passes through to the
//! host (see `HostPolicy`).
#![allow(non_snake_case)]

use std::ffi::c_void;

use crate::{HleModule, hle_export, runtime};

/// `GetModuleHandleW(NULL)` must return the title's image, not our exe.
pub extern "system" fn GetModuleHandleW(name: *const u16) -> *mut c_void {
    if name.is_null() {
        return runtime::main_image().unwrap_or(0) as *mut c_void;
    }
    host::get_module_handle_w(name)
}

/// `GetModuleHandleA(NULL)` must return the title's image, not our exe.
pub extern "system" fn GetModuleHandleA(name: *const u8) -> *mut c_void {
    if name.is_null() {
        return runtime::main_image().unwrap_or(0) as *mut c_void;
    }
    host::get_module_handle_a(name)
}

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "kernel",
        dlls: &[
            "kernel32.dll",
            "kernelbase.dll",
            "api-ms-win-core-libraryloader-*",
        ],
        exports: vec![hle_export!(GetModuleHandleW), hle_export!(GetModuleHandleA)],
    }
}

#[cfg(windows)]
mod host {
    use std::ffi::c_void;

    pub(super) fn get_module_handle_w(name: *const u16) -> *mut c_void {
        // SAFETY: forwarding the title's own argument to the host API.
        unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(name) }
    }

    pub(super) fn get_module_handle_a(name: *const u8) -> *mut c_void {
        // SAFETY: forwarding the title's own argument to the host API.
        unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleA(name) }
    }
}

#[cfg(not(windows))]
mod host {
    use std::ffi::c_void;

    pub(super) fn get_module_handle_w(_: *const u16) -> *mut c_void {
        std::ptr::null_mut()
    }

    pub(super) fn get_module_handle_a(_: *const u8) -> *mut c_void {
        std::ptr::null_mut()
    }
}
