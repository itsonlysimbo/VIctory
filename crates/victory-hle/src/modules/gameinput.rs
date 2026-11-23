//! `gameinput.dll`: input devices. For now nothing is ever connected:
//! readings and device lookups fail as they do with no device plugged in,
//! callbacks are accepted and never called.
#![allow(non_snake_case)]
// Title code calls these through the object, passing pointers the real API
// would trust.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::xasync::{E_INVALIDARG, E_NOTIMPL};
use super::xgameruntime::{HRESULT, S_OK};
use crate::{HleExport, HleModule};

/// The documented "no such reading" and "no such device" errors.
pub const READING_NOT_FOUND: HRESULT = 0x838A_0003_u32 as i32;
pub const DEVICE_NOT_FOUND: HRESULT = 0x838A_0002_u32 as i32;
const E_NOINTERFACE: HRESULT = 0x8000_4002_u32 as i32;

#[repr(C)]
pub struct Object {
    vtable: &'static Vtable,
}

#[repr(C)]
struct Vtable([*const (); 22]);
// SAFETY: function addresses only.
unsafe impl Sync for Vtable {}

static OBJECT: Object = Object { vtable: &VTABLE };

type This = *const Object;
type Out = *mut *const c_void;

/// The DLL's only export (ordinal 1): the input object, static and shared.
pub extern "system" fn GameInputCreate(out: *mut This) -> HRESULT {
    tracing::info!("GameInputCreate: no input devices");
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: checked non-null; the title passes room for a pointer.
    unsafe { out.write(&OBJECT) };
    S_OK
}

/// Clears a COM out pointer and returns `hr`.
fn fail(out: Out, hr: HRESULT) -> HRESULT {
    if !out.is_null() {
        // SAFETY: checked non-null; the title passes room for a pointer.
        unsafe { out.write(std::ptr::null()) };
    }
    hr
}

extern "system" fn query_interface(_: This, _iid: *const c_void, out: Out) -> HRESULT {
    tracing::warn!("input QueryInterface not implemented");
    fail(out, E_NOINTERFACE)
}
extern "system" fn add_ref(_: This) -> u32 {
    1
}

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Microseconds; only ever compared with readings, and there are none.
extern "system" fn timestamp(_: This) -> u64 {
    START.elapsed().as_micros() as u64
}

extern "system" fn current_reading(
    _: This,
    _kind: u32,
    _device: *const c_void,
    out: Out,
) -> HRESULT {
    fail(out, READING_NOT_FOUND)
}
extern "system" fn relative_reading(
    _: This,
    _reference: *const c_void,
    _kind: u32,
    _device: *const c_void,
    out: Out,
) -> HRESULT {
    fail(out, READING_NOT_FOUND)
}
extern "system" fn temporal_reading(
    _: This,
    _time: u64,
    _device: *const c_void,
    out: Out,
) -> HRESULT {
    fail(out, READING_NOT_FOUND)
}

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn token(out: *mut u64) -> HRESULT {
    if !out.is_null() {
        // SAFETY: checked non-null.
        unsafe { out.write(NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)) };
    }
    S_OK
}

extern "system" fn register_reading(
    _: This,
    _device: *const c_void,
    _kind: u32,
    _threshold: f32,
    _context: *mut c_void,
    _callback: *const c_void,
    out: *mut u64,
) -> HRESULT {
    token(out)
}
/// Enumerating existing devices finds none, so the callback never runs.
extern "system" fn register_device(
    _: This,
    _device: *const c_void,
    kind: u32,
    _status: u32,
    _enumeration: u32,
    _context: *mut c_void,
    _callback: *const c_void,
    out: *mut u64,
) -> HRESULT {
    tracing::info!(
        kind = format_args!("{kind:#x}"),
        "input device callback: no devices"
    );
    token(out)
}
/// Guide button and keyboard layout callbacks.
extern "system" fn register_other(
    _: This,
    _device: *const c_void,
    _context: *mut c_void,
    _callback: *const c_void,
    out: *mut u64,
) -> HRESULT {
    token(out)
}
extern "system" fn stop_callback(_: This, _token: u64) {}
extern "system" fn unregister_callback(_: This, _token: u64, _timeout: u64) -> bool {
    true
}

/// Dispatcher and aggregate device: not needed while nothing is connected.
extern "system" fn create_dispatcher(_: This, out: Out) -> HRESULT {
    tracing::warn!("input dispatcher not implemented");
    fail(out, E_NOTIMPL)
}
extern "system" fn create_aggregate(_: This, _kind: u32, out: Out) -> HRESULT {
    tracing::warn!("aggregate input device not implemented");
    fail(out, E_NOTIMPL)
}
/// Lookup by id, object, platform handle or platform string.
extern "system" fn find_device(_: This, _value: *const c_void, out: Out) -> HRESULT {
    fail(out, DEVICE_NOT_FOUND)
}
extern "system" fn enable_oem_device(
    _: This,
    _vendor: u16,
    _product: u16,
    _interface: u8,
    _collection: u8,
) -> HRESULT {
    S_OK
}
extern "system" fn set_focus_policy(_: This, _policy: u32) {}

static VTABLE: Vtable = Vtable([
    query_interface as *const (),     // 0x00
    add_ref as *const (),             // 0x08 AddRef
    add_ref as *const (),             // 0x10 Release: the object is static
    timestamp as *const (),           // 0x18
    current_reading as *const (),     // 0x20
    relative_reading as *const (),    // 0x28 next
    relative_reading as *const (),    // 0x30 previous
    temporal_reading as *const (),    // 0x38
    register_reading as *const (),    // 0x40
    register_device as *const (),     // 0x48
    register_other as *const (),      // 0x50 guide button
    register_other as *const (),      // 0x58 keyboard layout
    stop_callback as *const (),       // 0x60
    unregister_callback as *const (), // 0x68
    create_dispatcher as *const (),   // 0x70
    create_aggregate as *const (),    // 0x78
    find_device as *const (),         // 0x80 by id
    find_device as *const (),         // 0x88 by object
    find_device as *const (),         // 0x90 by platform handle
    find_device as *const (),         // 0x98 by platform string
    enable_oem_device as *const (),   // 0xa0
    set_focus_policy as *const (),    // 0xa8
]);

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "gameinput",
        dlls: &["gameinput.dll"],
        // Titles import it by ordinal only.
        exports: vec![HleExport {
            name: "#1",
            address: GameInputCreate as *const () as usize,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HleResolver;
    use crate::HostPolicy;
    use victory_loader::Provenance;
    use victory_pe::ImportSymbol;

    fn slot<F: Copy>(object: This, offset: usize) -> F {
        // SAFETY: `object` is ours.
        let f = unsafe { (*object).vtable.0[offset / 8] };
        // SAFETY: the caller names the slot's signature.
        unsafe { std::mem::transmute_copy(&f) }
    }

    #[test]
    fn ordinal_one_resolves_to_create() {
        let r = HleResolver::new(HostPolicy::none());
        assert_eq!(
            r.classify("GameInput.dll", &ImportSymbol::Ordinal(1)),
            Some(Provenance::Hle)
        );
        assert_eq!(r.classify("GameInput.dll", &ImportSymbol::Ordinal(2)), None);
    }

    #[test]
    fn startup_sees_no_devices() {
        type Register = extern "system" fn(
            This,
            *const c_void,
            u32,
            u32,
            u32,
            *mut c_void,
            *const c_void,
            *mut u64,
        ) -> HRESULT;
        type Reading = extern "system" fn(This, u32, *const c_void, Out) -> HRESULT;
        type Find = extern "system" fn(This, *const c_void, Out) -> HRESULT;

        let mut input = std::ptr::null();
        assert_eq!(GameInputCreate(&mut input), S_OK);
        assert!(!input.is_null());

        let mut token = 0;
        let register = slot::<Register>(input, 0x48);
        let null = std::ptr::null_mut();
        assert_eq!(
            register(input, null, 0x40130, 1, 1, null, null, &mut token),
            S_OK
        );
        assert_ne!(token, 0);

        let mut out = 0x1234 as *const c_void;
        let reading = slot::<Reading>(input, 0x20);
        assert_eq!(reading(input, 4, null, &mut out), READING_NOT_FOUND);
        assert!(out.is_null());
        out = 0x1234 as *const c_void;
        let find = slot::<Find>(input, 0x80);
        assert_eq!(find(input, null, &mut out), DEVICE_NOT_FOUND);
        assert!(out.is_null());
    }
}
