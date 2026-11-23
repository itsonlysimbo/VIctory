//! The title's own id (from its manifest), which runtime areas exist, and
//! game invites and events. Offline no invite arrives and events go
//! nowhere.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{CStr, c_char, c_void};

use super::xgameruntime::{
    Guid, HRESULT, Interface, Methods, Object, S_OK, add_ref, missing, put, token,
};
use crate::runtime;

/// Title id and launching another title.
pub const TITLE_PROVIDER: Guid = Guid::new(
    0x973a_344e,
    0x24bf,
    0x4d0f,
    [0x84, 0x57, 0x56, 0xc5, 0x34, 0x89, 0x2b, 0x29],
);
/// As Minecraft (2022) asks for it.
pub const TITLE_IID: Guid = Guid::new(
    0x2549_f142,
    0x6419,
    0x4a06,
    [0x97, 0xb5, 0x93, 0x1a, 0xab, 0x7c, 0x2f, 0x34],
);
/// Feature checks; the interface id is the provider's, as for the next two.
pub const FEATURES: Guid = Guid::new(
    0x8836_fe87,
    0xedb9,
    0x4fe3,
    [0x8d, 0xad, 0x05, 0xf0, 0xd2, 0xcd, 0x5b, 0x40],
);
pub const INVITES: Guid = Guid::new(
    0x0651_aae2,
    0x4012,
    0x4077,
    [0xbf, 0x84, 0x8b, 0x90, 0x97, 0x09, 0x0e, 0x2c],
);
pub const EVENTS: Guid = Guid::new(
    0xbbfb_dcc7,
    0xbfe7,
    0x409b,
    [0xa5, 0xca, 0xed, 0xf0, 0x54, 0x96, 0x0b, 0x4d],
);

pub static TITLE_OBJECT: Object = Object::new("title", &TITLE_VTABLE);
pub static FEATURES_OBJECT: Object = Object::new("features", &FEATURES_VTABLE);
pub static INVITES_OBJECT: Object = Object::new("invites", &INVITES_VTABLE);
pub static EVENTS_OBJECT: Object = Object::new("events", &EVENTS_VTABLE);

pub const INTERFACES: &[Interface] = &[
    (TITLE_PROVIDER, TITLE_IID, &TITLE_OBJECT),
    (FEATURES, FEATURES, &FEATURES_OBJECT),
    (INVITES, INVITES, &INVITES_OBJECT),
    (EVENTS, EVENTS, &EVENTS_OBJECT),
];

type This = *const Object;

extern "system" fn title_id(_: This, out: *mut u32) -> HRESULT {
    put(out, runtime::title().id)
}

/// Runtime areas by number, in the public feature list's order, that we
/// serve: async calls and providers (2, 3), game (5), invites (6),
/// persistent local storage (12), system (15), task queues (16), users
/// (18), error reports (19), game events (20). Not the others: store, game
/// saves, UI, networking, packages, streaming, threads...
const AVAILABLE: [u32; 10] = [2, 3, 5, 6, 12, 15, 16, 18, 19, 20];

extern "system" fn is_feature_available(_: This, feature: u32) -> bool {
    AVAILABLE.contains(&feature)
}

/// Nothing raises these events offline, so the callback never runs.
extern "system" fn register(
    _: This,
    _queue: *const c_void,
    _context: *mut c_void,
    _callback: *const c_void,
    out: *mut u64,
) -> HRESULT {
    put(out, token())
}
extern "system" fn unregister(_: This, _token: u64, _wait: bool) -> bool {
    true
}

/// Accepted and dropped; the name goes to the debug log.
extern "system" fn write_event(
    _: This,
    _user: *const c_void,
    _service_config: *const c_char,
    _session: *const c_char,
    name: *const c_char,
    _dimensions: *const c_char,
    _measurements: *const c_char,
) -> HRESULT {
    if !name.is_null() {
        // SAFETY: a non-null name is a NUL-terminated string.
        let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
        tracing::debug!(%name, "game event dropped");
    }
    S_OK
}

static TITLE_VTABLE: Methods<5> = Methods([
    missing::<0> as *const (),
    add_ref as *const (),
    add_ref as *const (),
    title_id as *const (),     // 0x18
    missing::<4> as *const (), // 0x20 launch another title (returns nothing)
]);

static FEATURES_VTABLE: Methods<4> = Methods([
    missing::<0> as *const (),
    add_ref as *const (),
    add_ref as *const (),
    is_feature_available as *const (), // 0x18
]);

static INVITES_VTABLE: Methods<5> = Methods([
    missing::<0> as *const (),
    add_ref as *const (),
    add_ref as *const (),
    register as *const (),   // 0x18
    unregister as *const (), // 0x20
]);

static EVENTS_VTABLE: Methods<4> = Methods([
    missing::<0> as *const (),
    add_ref as *const (),
    add_ref as *const (),
    write_event as *const (), // 0x18
]);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::xgameruntime::method;

    #[test]
    fn served_areas_are_available_others_not() {
        type Check = extern "system" fn(This, u32) -> bool;
        let check = method::<Check>(&FEATURES_OBJECT, 0x18);
        assert!(check(&FEATURES_OBJECT, 18), "users");
        assert!(!check(&FEATURES_OBJECT, 14), "store");

        type Register = extern "system" fn(
            This,
            *const c_void,
            *mut c_void,
            *const c_void,
            *mut u64,
        ) -> HRESULT;
        type Unregister = extern "system" fn(This, u64, bool) -> bool;
        let null = std::ptr::null();
        let mut token = 0;
        let register = method::<Register>(&INVITES_OBJECT, 0x18);
        assert_eq!(
            register(&INVITES_OBJECT, null, null as _, null, &mut token),
            S_OK
        );
        assert_ne!(token, 0);
        assert!(method::<Unregister>(&INVITES_OBJECT, 0x20)(
            &INVITES_OBJECT,
            token,
            true
        ));
    }
}
