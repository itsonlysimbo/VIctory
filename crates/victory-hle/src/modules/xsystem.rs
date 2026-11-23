//! XSystem: the console's identity and sandbox, and how analytics describe
//! the device. Fixed values: one offline retail Xbox Series X.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use super::xgameruntime::{
    Guid, HRESULT, Interface, Methods, Object, add_ref, missing, put_string,
};

pub const PROVIDER: Guid = Guid::new(
    0xe349_bd1a,
    0xfc20,
    0x4e40,
    [0xb9, 0x9c, 0x41, 0x78, 0xcc, 0x6b, 0x40, 0x9f],
);

/// The interface as Minecraft (2022) asks for it; an older table
/// ended before the device id.
pub const IID: Guid = Guid::new(
    0x6fd7_1f09,
    0x7513,
    0x49f0,
    [0x89, 0xbc, 0xbf, 0xaf, 0x5d, 0xf6, 0xf8, 0x52],
);

/// Analytics information; its interface id is the provider's.
pub const ANALYTICS: Guid = Guid::new(
    0xb884_675d,
    0xb738,
    0x4a9c,
    [0x81, 0x5d, 0x9a, 0x9a, 0x1e, 0x0c, 0x6c, 0x9b],
);

/// 38 characters (39 bytes with the NUL).
const CONSOLE_ID: &str = "00000000000000000000000000000000000001";
/// The retail sandbox.
const SANDBOX: &str = "RETAIL";
/// Base64 of 32 bytes: 44 characters (45 bytes with the NUL).
const DEVICE_ID: &str = "VkljdG9yeSBvZmZsaW5lIGRldmljZSBpZGVudGl0eSE=";

pub static OBJECT: Object = Object::new("system", &VTABLE);
pub static ANALYTICS_OBJECT: Object = Object::new("analytics", &ANALYTICS_VTABLE);

pub const INTERFACES: &[Interface] = &[
    (PROVIDER, IID, &OBJECT),
    (ANALYTICS, ANALYTICS, &ANALYTICS_OBJECT),
];

type This = *const Object;

extern "system" fn console_id(_: This, size: usize, buffer: *mut u8, used: *mut usize) -> HRESULT {
    put_string(CONSOLE_ID, size, buffer, used)
}
extern "system" fn sandbox_id(_: This, size: usize, buffer: *mut u8, used: *mut usize) -> HRESULT {
    put_string(SANDBOX, size, buffer, used)
}
/// The device id this title sees (titles see different ones).
extern "system" fn device_id(_: This, size: usize, buffer: *mut u8, used: *mut usize) -> HRESULT {
    put_string(DEVICE_ID, size, buffer, used)
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
    pub revision: u16,
}

#[repr(C)]
pub struct AnalyticsInfo {
    pub os: Version,
    pub hosting_os: Version,
    /// NUL-terminated.
    pub family: [u8; 64],
    pub form: [u8; 64],
}

const OS: Version = Version {
    major: 10,
    minor: 0,
    build: 22000,
    revision: 0,
};

fn text(s: &str) -> [u8; 64] {
    let mut out = [0; 64];
    out[..s.len()].copy_from_slice(s.as_bytes());
    out
}

/// A struct this large comes back through the caller's buffer, whose
/// address is also the return value.
extern "system" fn analytics_info(_: This, out: *mut AnalyticsInfo) -> *mut AnalyticsInfo {
    let info = AnalyticsInfo {
        os: OS,
        hosting_os: OS,
        family: text("Xbox"),
        form: text("Xbox Series X"),
    };
    // SAFETY: the caller passes room for the struct.
    unsafe { out.write(info) };
    out
}

static VTABLE: Methods<6> = Methods([
    missing::<0> as *const (),
    add_ref as *const (),
    add_ref as *const (),
    console_id as *const (), // 0x18, by the older table; titles seen do not call it
    sandbox_id as *const (), // 0x20
    device_id as *const (),  // 0x28
]);

static ANALYTICS_VTABLE: Methods<4> = Methods([
    missing::<0> as *const (),
    add_ref as *const (),
    add_ref as *const (),
    analytics_info as *const (), // 0x18
]);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::xgameruntime::{S_OK, method};

    #[test]
    fn identity_strings_fit_the_documented_sizes() {
        type Get = extern "system" fn(This, usize, *mut u8, *mut usize) -> HRESULT;
        let this: This = &OBJECT;
        let mut buffer = [0u8; 64];
        let mut used = 0;
        for (slot, size) in [(0x18, 39), (0x20, 16), (0x28, 45)] {
            let get = method::<Get>(&OBJECT, slot);
            assert_eq!(get(this, size, buffer.as_mut_ptr(), &mut used), S_OK);
            assert!(used <= size && buffer[used - 1] == 0, "slot {slot:#x}");
        }
        assert_eq!(&buffer[..7], b"VkljdG9");
    }

    #[test]
    fn analytics_info_comes_back_through_the_buffer() {
        type Get = extern "system" fn(This, *mut AnalyticsInfo) -> *mut AnalyticsInfo;
        let mut info = std::mem::MaybeUninit::<AnalyticsInfo>::uninit();
        let get = method::<Get>(&ANALYTICS_OBJECT, 0x18);
        let back = get(&ANALYTICS_OBJECT, info.as_mut_ptr());
        assert_eq!(back, info.as_mut_ptr());
        // SAFETY: written by the call.
        let info = unsafe { info.assume_init() };
        assert_eq!(size_of::<AnalyticsInfo>(), 0x90);
        assert_eq!(info.os.build, 22000);
        assert_eq!(&info.form[..14], b"Xbox Series X\0");
    }
}
