//! Names the faulting code when title code crashes.
//!
//! A vectored handler sees every exception first, so it only logs the kinds
//! that end a process (access violations and the like) and leaves handling
//! to the title's own handlers.

use windows_sys::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_INT_DIVIDE_BY_ZERO,
    EXCEPTION_PRIV_INSTRUCTION, EXCEPTION_STACK_OVERFLOW, HMODULE,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_POINTERS,
};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
/// Stack slots scanned for return addresses into a module.
const STACK_SCAN: usize = 64;

/// Installs the handler; call once before running title code.
pub fn report_crashes() {
    // SAFETY: registers a handler that only reads the exception it is given.
    unsafe { AddVectoredExceptionHandler(0, Some(handler)) };
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes valid exception pointers.
    let (record, context) = unsafe { (&*(*info).ExceptionRecord, &*(*info).ContextRecord) };
    let code = record.ExceptionCode;
    if ![
        EXCEPTION_ACCESS_VIOLATION,
        EXCEPTION_ILLEGAL_INSTRUCTION,
        EXCEPTION_PRIV_INSTRUCTION,
        EXCEPTION_INT_DIVIDE_BY_ZERO,
        EXCEPTION_STACK_OVERFLOW,
    ]
    .contains(&code)
    {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let at = record.ExceptionAddress as usize;
    let access = if code == EXCEPTION_ACCESS_VIOLATION {
        let kind = match record.ExceptionInformation[0] {
            0 => "read",
            1 => "write",
            _ => "execute",
        };
        format!(" ({kind} {:#x})", record.ExceptionInformation[1])
    } else {
        String::new()
    };
    eprintln!(
        "VIctory: exception {:#010x} at {}{access}",
        code as u32,
        locate(at)
    );
    eprintln!(
        "  rax {:#x} rbx {:#x} rcx {:#x} rdx {:#x} rsi {:#x} rdi {:#x} rsp {:#x}",
        context.Rax, context.Rbx, context.Rcx, context.Rdx, context.Rsi, context.Rdi, context.Rsp
    );
    // shortcut: a stack scan, not an unwind; it lists stale return
    // addresses too. Use RtlVirtualUnwind if this misleads.
    let stack = context.Rsp as *const usize;
    for i in 0..STACK_SCAN {
        // SAFETY: slots just above the faulting RSP are committed stack.
        let value = unsafe { stack.add(i).read_unaligned() };
        if let Some(name) = module_of(value) {
            eprintln!("  [rsp+{:#x}] {name}", i * 8);
        }
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// `module+rva`, or the bare address.
pub fn locate(address: usize) -> String {
    module_of(address).unwrap_or_else(|| format!("{address:#x}"))
}

fn module_of(address: usize) -> Option<String> {
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: looks up the module containing `address`; no reference taken.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as *const u16,
            &mut module,
        )
    };
    if found == 0 {
        return None;
    }
    let mut path = [0u16; 260];
    // SAFETY: `module` is a loaded module; the buffer length is passed.
    let len = unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) };
    let path = String::from_utf16_lossy(&path[..len as usize]);
    let name = path.rsplit('\\').next().unwrap_or(&path);
    Some(format!("{name}+{:#x}", address - module as usize))
}
