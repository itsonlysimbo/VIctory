//! Root signatures.
//!
//! Serializing goes to the host's `D3D12SerializeRootSignature`: the
//! description is laid out as on PC, and the blob it returns works as the
//! console's, whose two methods sit in the same slots. The console's own
//! flags (bit 24 and up) are dropped first.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::sync::OnceLock;

use crate::object::{
    self, E_INVALIDARG, E_NOTIMPL, HRESULT, Iid, Object, ROOT_SIGNATURE, Table, This, guid,
};
use crate::resource::PAGEABLE_IIDS;

const ROOT_SIGNATURE_IIDS: [Iid; 3] = [
    guid(
        0xc54a6b66,
        0x72df,
        0x4ee8,
        [0x8b, 0xe5, 0xa9, 0x46, 0xa1, 0x42, 0x92, 0x14],
    ),
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

const CONSOLE_FLAGS: u32 = 0xff00_0000;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Desc {
    parameter_count: u32,
    parameters: *const c_void,
    sampler_count: u32,
    samplers: *const c_void,
    flags: u32,
}

type Serialize =
    unsafe extern "system" fn(*const Desc, u32, *mut *mut c_void, *mut *mut c_void) -> HRESULT;

/// The host's serializer, from `d3d12.dll` in System32.
fn host_serializer() -> Option<Serialize> {
    static FUNCTION: OnceLock<Option<usize>> = OnceLock::new();
    let address = *FUNCTION.get_or_init(|| {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::LibraryLoader::{
                GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
            };
            let name: Vec<u16> = "d3d12.dll".encode_utf16().chain([0]).collect();
            // SAFETY: a NUL-terminated name; System32 only.
            let module = unsafe {
                LoadLibraryExW(
                    name.as_ptr(),
                    std::ptr::null_mut(),
                    LOAD_LIBRARY_SEARCH_SYSTEM32,
                )
            };
            if module.is_null() {
                return None;
            }
            // SAFETY: a loaded module and a NUL-terminated name.
            unsafe { GetProcAddress(module, c"D3D12SerializeRootSignature".as_ptr().cast()) }
                .map(|f| f as usize)
        }
        #[cfg(not(windows))]
        None
    });
    // SAFETY: the export has this signature.
    address.map(|a| unsafe { std::mem::transmute::<usize, Serialize>(a) })
}

/// `d3d12_xs!D3D12SerializeRootSignature`.
pub extern "system" fn serialize(
    desc: *const Desc,
    version: u32,
    blob: *mut *mut c_void,
    error: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(&desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    if desc.flags & CONSOLE_FLAGS != 0 {
        tracing::debug!(
            flags = format_args!("{:#x}", desc.flags),
            "console root signature flags dropped"
        );
    }
    let desc = Desc {
        flags: desc.flags & !CONSOLE_FLAGS,
        ..desc
    };
    let Some(serialize) = host_serializer() else {
        tracing::warn!("no host d3d12.dll to serialize root signatures");
        return E_NOTIMPL;
    };
    // SAFETY: the host function takes the same arguments the title passed.
    unsafe { serialize(&desc, version, blob, error) }
}

#[repr(C)]
struct RootSignature {
    object: Object,
    /// The serialized form, kept for translating pipelines later.
    blob: Vec<u8>,
}

/// Device method.
pub(crate) extern "system" fn create(
    _: This,
    _nodes: u32,
    blob: *const u8,
    size: usize,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    if blob.is_null() || size == 0 {
        return E_INVALIDARG;
    }
    // SAFETY: the title passes `size` bytes.
    let blob = unsafe { std::slice::from_raw_parts(blob, size) }.to_vec();
    tracing::debug!(size, "root signature");
    let root = Box::new(RootSignature {
        object: Object::new(&FUNCTIONS),
        blob,
    });
    object::hand_out(ROOT_SIGNATURE, root, iid, &ROOT_SIGNATURE_IIDS, out)
}

extern "system" fn query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    let found = object::is(iid, &ROOT_SIGNATURE_IIDS).then_some(this);
    object::answer(ROOT_SIGNATURE, iid, found, out)
}
extern "system" fn release(this: This) -> u32 {
    object::release_boxed::<RootSignature>(this)
}

/// IUnknown 3, object 4, device child 1.
static FUNCTIONS: Table<8> = {
    let mut f = object::d3d12_object(object::missing_table::<ROOT_SIGNATURE, 8>());
    f[0] = query as *const ();
    f[2] = release as *const ();
    f[7] = object::parent_device as *const (); // 0x38
    Table(f)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{S_OK, slot};

    /// One 32-bit constant at b0, serialized by the host, then created.
    #[cfg(windows)]
    #[test]
    fn host_serialized_blobs_create_root_signatures() {
        #[repr(C)]
        struct Constants {
            kind: u32,
            register: u32,
            space: u32,
            count: u32,
            visibility: u32,
        }
        let parameter = Constants {
            kind: 1,
            register: 0,
            space: 0,
            count: 1,
            visibility: 0,
        };
        let desc = Desc {
            parameter_count: 1,
            parameters: (&raw const parameter).cast(),
            sampler_count: 0,
            samplers: std::ptr::null(),
            flags: 0x0100_0001,
        };
        let (mut blob, mut error) = (std::ptr::null_mut(), std::ptr::null_mut());
        assert_eq!(serialize(&desc, 1, &mut blob, &mut error), S_OK);
        let blob = blob.cast_const().cast::<Object>();
        let data = slot::<extern "system" fn(This) -> *const u8>(blob, 0x18)(blob);
        let size = slot::<extern "system" fn(This) -> usize>(blob, 0x20)(blob);
        assert!(size > 0);
        let null = std::ptr::null();
        let mut root = null;
        assert_eq!(
            create(null, 0, data, size, &ROOT_SIGNATURE_IIDS[0], &mut root),
            S_OK
        );
        assert_eq!(slot::<extern "system" fn(This) -> u32>(root, 0x10)(root), 0);
        slot::<extern "system" fn(This) -> u32>(blob, 0x10)(blob);
    }
}
