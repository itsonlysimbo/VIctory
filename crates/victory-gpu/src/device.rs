//! The device titles get from device creation, laid out as their inlined
//! code expects: the object header, then device fields. Its table starts
//! like the PC device interface (IUnknown, the object methods, then the
//! device methods in PC order); console additions follow. Methods we have
//! not written log their table offset and fail, so a run shows what to
//! write next.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;

use crate::object::{
    self, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL, HRESULT, Iid, Object, S_FALSE, S_OK, Table, This,
    guid, guid_text,
};
use crate::{
    descriptor, dxgi, fence, frame, list, pipeline, query, queue, resource, root, signature,
};

/// What D3D returns when none of the requested feature levels is supported.
const UNSUPPORTED: HRESULT = 0x887A_0004_u32 as i32;

/// The device interface id (the same as on PC). Titles also import it as
/// data.
pub static IID: Iid = guid(
    0x189819f1,
    0x1db6,
    0x4b57,
    [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7],
);

/// The interface version these layouts were checked against (2.4, what
/// Minecraft passes). Newer titles may lay objects out differently.
pub const CHECKED_VERSION: u32 = 0x0002_0004;

/// Feature level 12_2, what the console hardware supports.
const HIGHEST_FEATURE_LEVEL: u32 = 0xc200;

#[repr(C)]
pub struct Device {
    object: Object,
    /// GPU address of a null constant buffer; none yet.
    null_constant_buffer: u64,
    /// Byte 0 non-zero: calls the title would otherwise inline come to the
    /// table. The rest is reserved.
    out_of_line: [u32; 16],
}
const _: () = assert!(size_of::<Device>() == 0x58);

/// One device per process, as on the console.
pub(crate) static DEVICE: Device = Device {
    object: Object::new(&FUNCTIONS),
    null_constant_buffer: 0,
    out_of_line: [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
};

pub(crate) fn device() -> This {
    (&raw const DEVICE).cast()
}

/// Device creation: `(adapter, parameters, iid, out)`. The parameters start
/// with the interface version; the rest (ring and scratch sizes, features
/// to leave out) does not matter to us yet.
pub extern "system" fn create(
    _adapter: *const c_void,
    parameters: *const u32,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    if parameters.is_null() || iid.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: checked non-null; the title passes its parameters and an id.
    let (version, iid) = unsafe { (parameters.read(), &*iid) };
    tracing::info!(version = format_args!("{version:#x}"), "D3D12.X device");
    if version != CHECKED_VERSION {
        tracing::warn!(
            version = format_args!("{version:#x}"),
            "device layouts were checked against {CHECKED_VERSION:#x} only"
        );
    }
    if *iid != IID {
        tracing::warn!(iid = %guid_text(iid), "device created as an unknown interface");
        return E_NOINTERFACE;
    }
    if out.is_null() {
        return S_FALSE;
    }
    object::answer(object::DEVICE, iid, Some(device()), out)
}

/// The device, or its DXGI side.
extern "system" fn query_interface(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    let found = if object::is(iid, &[IID]) {
        Some(this)
    } else if object::is(iid, &dxgi::DEVICE_IIDS) {
        Some(dxgi::device())
    } else {
        None
    };
    object::answer(object::DEVICE, iid, found, out)
}

extern "system" fn node_count(_: This) -> u32 {
    1
}

/// Feature query 2: the highest of the requested feature levels we support.
#[repr(C)]
struct FeatureLevels {
    count: u32,
    requested: *const u32,
    highest: u32,
}

/// Feature query 0, what an RDNA 2 GPU reports. The console's layout has
/// one field more than PC's (index 11).
const OPTIONS: [u32; 16] = [
    1,  // double-precision shader operations
    1,  // output-merger logic operations
    2,  // minimum precision: 16-bit
    3,  // tiled resources tier
    3,  // resource binding tier
    1,  // stencil reference from pixel shaders
    1,  // typed UAV loads of more formats
    1,  // rasterizer-ordered views
    3,  // conservative rasterization tier
    40, // GPU address bits per resource
    0,  // 64 KiB standard swizzle
    0,  // console-only field
    0,  // cross-node sharing tier
    0,  // cross-adapter row-major textures
    1,  // viewport and render target index from any stage
    2,  // resource heap tier
];

extern "system" fn check_feature_support(
    _: This,
    feature: u32,
    data: *mut c_void,
    size: u32,
) -> HRESULT {
    const OPTIONS_QUERY: u32 = 0;
    const ARCHITECTURE: u32 = 1;
    const FEATURE_LEVELS: u32 = 2;
    const FORMAT_SUPPORT: u32 = 3;
    const MULTISAMPLE: u32 = 4;
    let expected = match feature {
        OPTIONS_QUERY => size_of_val(&OPTIONS),
        ARCHITECTURE => 16,
        FEATURE_LEVELS => size_of::<FeatureLevels>(),
        FORMAT_SUPPORT => 12,
        MULTISAMPLE => 16,
        _ => {
            tracing::warn!(feature, size, "device feature query not implemented");
            return E_NOTIMPL;
        }
    };
    if data.is_null() || size as usize != expected {
        return E_INVALIDARG;
    }
    match feature {
        OPTIONS_QUERY => {
            // SAFETY: checked size and non-null.
            unsafe { data.cast::<[u32; 16]>().write_unaligned(OPTIONS) };
            S_OK
        }
        ARCHITECTURE => {
            // SAFETY: checked size and non-null.
            let a = unsafe { &mut *data.cast::<[u32; 4]>() };
            if a[0] != 0 {
                return E_INVALIDARG; // one node only
            }
            // Not tile-based; unified, cache-coherent memory.
            a[1..].copy_from_slice(&[0, 1, 1]);
            S_OK
        }
        FORMAT_SUPPORT => {
            // SAFETY: checked size and non-null.
            let q = unsafe { &mut *data.cast::<[u32; 3]>() };
            // shortcut: every use of every format; answer from the host
            // device once there is one.
            q[1] = 0x7fff_ffff;
            q[2] = 0xffff;
            S_OK
        }
        MULTISAMPLE => {
            // SAFETY: checked size and non-null.
            let q = unsafe { &mut *data.cast::<[u32; 4]>() };
            // Format, sample count, flags in; one quality level out for the
            // counts the hardware has.
            q[3] = u32::from(matches!(q[1], 1 | 2 | 4 | 8));
            S_OK
        }
        _ => feature_levels(data),
    }
}

fn feature_levels(data: *mut c_void) -> HRESULT {
    // SAFETY: the caller checked size and non-null.
    let query = unsafe { &mut *data.cast::<FeatureLevels>() };
    if query.requested.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the title passes `count` levels.
    let requested = unsafe { std::slice::from_raw_parts(query.requested, query.count as usize) };
    match requested
        .iter()
        .copied()
        .filter(|&l| l <= HIGHEST_FEATURE_LEVEL)
        .max()
    {
        Some(level) => {
            query.highest = level;
            S_OK
        }
        None => UNSUPPORTED,
    }
}

/// IUnknown 3, object methods 4, device methods 76.
static FUNCTIONS: Table<83> = {
    let mut f = object::d3d12_object(object::missing_table::<{ object::DEVICE }, 83>());
    f[0] = query_interface as *const (); // 0x00
    f[7] = node_count as *const (); // 0x38
    f[8] = queue::create as *const (); // 0x40
    f[9] = list::create_allocator as *const (); // 0x48
    f[12] = list::create_list as *const (); // 0x60
    f[10] = pipeline::create_graphics as *const (); // 0x50
    f[11] = pipeline::create_compute as *const (); // 0x58
    f[13] = check_feature_support as *const (); // 0x68
    f[14] = descriptor::create_heap as *const (); // 0x70
    f[15] = descriptor::increment_size as *const (); // 0x78
    f[16] = root::create as *const (); // 0x80
    f[20] = descriptor::create_rtv as *const (); // 0xa0
    f[25] = resource::allocation_info as *const (); // 0xc8
    f[27] = resource::create_committed as *const (); // 0xd8
    f[28] = resource::create_heap as *const (); // 0xe0
    f[29] = resource::create_placed as *const (); // 0xe8
    f[36] = fence::create as *const (); // 0x120
    f[38] = resource::copyable_footprints as *const (); // 0x130
    f[39] = query::create as *const (); // 0x138
    f[41] = signature::create as *const (); // 0x148
    f[69] = object::ignore as *const (); // 0x228 debug error filter
    f[73] = list::create_list_x as *const (); // 0x248
    f[78] = frame::set_interval as *const (); // 0x270
    f[79] = frame::schedule as *const (); // 0x278
    f[80] = frame::wait as *const (); // 0x280
    f[81] = frame::statistics as *const (); // 0x288
    Table(f)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::slot;

    fn create_as(iid: &Iid) -> (HRESULT, This) {
        let version = CHECKED_VERSION;
        let mut device = std::ptr::null();
        let hr = create(std::ptr::null(), &version, iid, &mut device);
        (hr, device)
    }

    #[test]
    fn creation_hands_out_the_device() {
        let (hr, device) = create_as(&IID);
        assert_eq!(hr, S_OK);
        assert_eq!(device, super::device());
        // The title tests byte 0x18 before inlining a device call.
        // SAFETY: the device is 0x58 bytes.
        assert_ne!(unsafe { device.cast::<u8>().add(0x18).read() }, 0);
        assert_eq!(create_as(&[0; 16]).0, E_NOINTERFACE);
        assert_eq!(
            slot::<extern "system" fn(This) -> u32>(device, 0x38)(device),
            1
        );
        let missing = slot::<extern "system" fn() -> HRESULT>(device, 0x88);
        assert_eq!(missing(), E_NOTIMPL);
    }

    #[test]
    fn feature_levels_pick_the_highest_supported() {
        type Check = extern "system" fn(This, u32, *mut c_void, u32) -> HRESULT;
        let device = device();
        let check = slot::<Check>(device, 0x68);
        let query = |levels: &[u32], size| {
            let mut query = FeatureLevels {
                count: levels.len() as u32,
                requested: levels.as_ptr(),
                highest: 0,
            };
            let hr = check(device, 2, (&raw mut query).cast(), size);
            (hr, query.highest)
        };
        let levels = [0xb000, 0xb100, 0xc000, 0xc100, 0xd000];
        assert_eq!(query(&levels, 0x18), (S_OK, 0xc100));
        assert_eq!(query(&levels, 0x10).0, E_INVALIDARG);
        assert_eq!(query(&levels[4..], 0x18).0, UNSUPPORTED);
    }
}
