//! Command allocators and command lists.
//!
//! The title's inlined code writes packets into a list only while its put
//! pointer is below a limit, and calls the list's method otherwise. Our
//! lists keep the pointer and every limit at zero, so every command reaches
//! the table as a call.
//!
//! shortcut: the methods record nothing yet; unwritten ones log once, so a
//! run lists what to translate. Translate them onto host command lists
//! (TODO.md, GPU).
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::mem::offset_of;

use crate::object::{
    self, ALLOCATOR, E_INVALIDARG, HRESULT, Iid, LIST, Object, S_OK, Table, This, guid,
};
use crate::resource::PAGEABLE_IIDS;

const ALLOCATOR_IIDS: [Iid; 4] = [
    guid(
        0x6102dee4,
        0xaf59,
        0x4b09,
        [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24],
    ),
    PAGEABLE_IIDS[0],
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

/// The list interfaces whose ids are the same as on PC, then the device
/// child's.
const LIST_IIDS: [Iid; 8] = [
    guid(
        0x7116d91c,
        0xe7e4,
        0x47ce,
        [0xb8, 0xc6, 0xec, 0x81, 0x68, 0xf4, 0x37, 0xe5],
    ),
    guid(
        0x5b160d0f,
        0xac1b,
        0x4185,
        [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55],
    ),
    guid(
        0x553103fb,
        0x1fe7,
        0x4557,
        [0xbb, 0x38, 0x94, 0x6d, 0x7d, 0x0e, 0x7c, 0xa7],
    ),
    guid(
        0x38c3e585,
        0xff17,
        0x412c,
        [0x91, 0x50, 0x4f, 0xc6, 0xf9, 0xd7, 0x2a, 0x28],
    ),
    guid(
        0x6fda83a7,
        0xb84c,
        0x4e38,
        [0x9a, 0xc8, 0xc7, 0xbd, 0x22, 0x01, 0x6b, 0x3d],
    ),
    guid(
        0x8754318e,
        0xd3a9,
        0x4541,
        [0x98, 0xcf, 0x64, 0x5b, 0x50, 0xdc, 0x48, 0x74],
    ),
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

#[repr(C)]
struct Allocator {
    object: Object,
    kind: u32,
}

/// The layout the title's inlined code reads.
#[repr(C)]
struct List {
    object: Object,
    kind: u32,
    /// The put pointer, then the plain, draw, API and unbiased API limits:
    /// all zero, so every inline check fails.
    put: [u64; 5],
    reserved: [u32; 64],
    /// State the inline code keeps for building packets; unused by us.
    graphics_root_headers: [u32; 128],
    compute_root_headers: [u32; 128],
    pipeline_shadow: [u32; 8],
    /// Ours, after the console layout: the host list the calls record
    /// into (decision 0007).
    #[cfg(windows)]
    host: Option<crate::host::HostList>,
}
const _: () = assert!(offset_of!(List, put) == 0x18);
const _: () = assert!(offset_of!(List, pipeline_shadow) + 32 == 0x560);

/// The host list behind a title list, if there is a host GPU.
#[cfg(windows)]
pub(crate) fn host_list<'a>(this: This) -> Option<&'a crate::host::HostList> {
    if this.is_null() {
        return None;
    }
    // SAFETY: the title passes one of our lists.
    unsafe { (*this.cast::<List>()).host.as_ref() }
}

pub(crate) extern "system" fn create_allocator(
    _: This,
    kind: u32,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    let allocator = Box::new(Allocator {
        object: Object::new(&ALLOCATOR_FUNCTIONS),
        kind,
    });
    object::hand_out(ALLOCATOR, allocator, iid, &ALLOCATOR_IIDS, out)
}

/// Lists start open, as in D3D12.
pub(crate) extern "system" fn create_list(
    _: This,
    _nodes: u32,
    kind: u32,
    allocator: This,
    _pipeline: This,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    new_list(kind, allocator, iid, out)
}

/// The console's list creation: type, flags, node mask and barrier batch
/// size in a description.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct ListDesc {
    kind: u32,
    flags: u32,
    nodes: u32,
    barrier_batch: u32,
}

pub(crate) extern "system" fn create_list_x(
    _: This,
    desc: *const ListDesc,
    allocator: This,
    _pipeline: This,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    tracing::debug!(?desc, "console command list");
    new_list(desc.kind, allocator, iid, out)
}

fn new_list(kind: u32, allocator: This, iid: *const Iid, out: *mut This) -> HRESULT {
    if allocator.is_null() {
        return E_INVALIDARG;
    }
    tracing::debug!(kind, "command list");
    let list = Box::new(List {
        object: Object::new(&LIST_FUNCTIONS),
        kind,
        put: [0; 5],
        reserved: [0; 64],
        graphics_root_headers: [0; 128],
        compute_root_headers: [0; 128],
        pipeline_shadow: [0; 8],
        #[cfg(windows)]
        host: crate::host::host().and_then(|h| match h.new_list() {
            Ok(list) => Some(list),
            Err(e) => {
                tracing::warn!(error = %e, "no host command list");
                None
            }
        }),
    });
    object::hand_out(LIST, list, iid, &LIST_IIDS, out)
}

extern "system" fn allocator_query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    let found = object::is(iid, &ALLOCATOR_IIDS).then_some(this);
    object::answer(ALLOCATOR, iid, found, out)
}
extern "system" fn allocator_release(this: This) -> u32 {
    object::release_boxed::<Allocator>(this)
}
/// Nothing is recorded, so there is nothing to free.
extern "system" fn allocator_reset(_: This) -> HRESULT {
    S_OK
}

extern "system" fn list_query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    object::answer(LIST, iid, object::is(iid, &LIST_IIDS).then_some(this), out)
}
extern "system" fn list_release(this: This) -> u32 {
    object::release_boxed::<List>(this)
}
extern "system" fn list_type(this: This) -> u32 {
    // SAFETY: `this` is one of our lists.
    unsafe { (*this.cast::<List>()).kind }
}
extern "system" fn close(this: This) -> HRESULT {
    #[cfg(windows)]
    if let Some(host) = host_list(this) {
        host.close();
    }
    S_OK
}
extern "system" fn reset(this: This, allocator: This, _pipeline: This) -> HRESULT {
    if allocator.is_null() {
        return E_INVALIDARG;
    }
    #[cfg(windows)]
    if let Some(host) = host_list(this) {
        host.reset();
    }
    S_OK
}

/// A resource barrier as titles pass it: type, flags, then a resource and
/// either a subresource and two states (transition) or a second resource
/// (aliasing). The same layout as the host's.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Barrier {
    kind: u32,
    flags: u32,
    first: This,
    second: u64,
    last: u64,
}

/// The barrier on host resources, or `None` if it names a resource the host
/// GPU does not have (buffers so far).
#[cfg(windows)]
fn host_barrier(b: &Barrier) -> Option<Barrier> {
    use windows::core::Interface;
    let host = |r: This| crate::resource::host_resource(r).map(|h| h.as_raw().cast_const().cast());
    match b.kind {
        0 => {
            // A split barrier's first half: its second half transitions.
            const BEGIN_ONLY: u32 = 1;
            if b.flags & BEGIN_ONLY != 0 {
                return None;
            }
            // shortcut: whole resources, from the state we tracked (see
            // `host_transition`); subresources are not told apart.
            let wanted = crate::host::texture_state(b.last as u32);
            let (resource, before) = crate::resource::host_transition(b.first, wanted)?;
            Some(Barrier {
                kind: 0,
                flags: 0,
                first: resource.cast_const().cast(),
                second: 0xffff_ffff | (u64::from(before) << 32),
                last: u64::from(wanted),
            })
        }
        1 => Some(Barrier {
            first: if b.first.is_null() {
                b.first
            } else {
                host(b.first)?
            },
            second: if b.second == 0 {
                0
            } else {
                host(b.second as This)? as u64
            },
            ..*b
        }),
        2 => Some(Barrier {
            first: if b.first.is_null() {
                b.first
            } else {
                host(b.first)?
            },
            ..*b
        }),
        _ => None,
    }
}

/// Returns the console's barrier "actions" (none).
extern "system" fn barrier(this: This, count: u32, barriers: *const Barrier) -> u32 {
    #[cfg(windows)]
    if let Some(list) = host_list(this)
        && !barriers.is_null()
    {
        // SAFETY: the title passes `count` barriers.
        let barriers = unsafe { std::slice::from_raw_parts(barriers, count as usize) };
        let host: Vec<Barrier> = barriers.iter().filter_map(host_barrier).collect();
        if !host.is_empty() {
            use windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_BARRIER;
            const _: () = assert!(size_of::<Barrier>() == size_of::<D3D12_RESOURCE_BARRIER>());
            // SAFETY: the same layout; borrowed resource pointers, never
            // released through this slice.
            let host = unsafe {
                std::slice::from_raw_parts(
                    host.as_ptr().cast::<D3D12_RESOURCE_BARRIER>(),
                    host.len(),
                )
            };
            // SAFETY: an open list and live resources.
            unsafe { list.list.ResourceBarrier(host) };
        }
    }
    0
}

/// Moves a title texture's host copy to `state` in the list, if needed.
#[cfg(windows)]
fn ensure_state(list: &crate::host::HostList, resource: This, state: u32) {
    use windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES as State;
    if let Some(texture) = crate::resource::host_resource(resource)
        && let Some((_, before)) = crate::resource::host_transition(resource, state)
    {
        let barrier = crate::host::transition(texture, State(before as i32), State(state as i32));
        // SAFETY: an open list and a live texture.
        unsafe { list.list.ResourceBarrier(&[barrier]) };
    }
}

#[cfg(windows)]
const COPY_DEST: u32 = 0x400;
#[cfg(windows)]
const COPY_SOURCE: u32 = 0x800;

/// `len` bytes at `offset` of a title buffer, if they are inside it.
#[cfg(windows)]
fn buffer_range(buffer: This, offset: u64, len: u64) -> Option<*mut u8> {
    let (data, size) = crate::resource::buffer_bytes(buffer)?;
    let end = offset.checked_add(len)?;
    if end > size {
        tracing::warn!(offset, len, size, "copy outside its buffer");
        return None;
    }
    // SAFETY: inside the buffer, checked above.
    Some(unsafe { data.add(offset as usize) })
}

/// Buffer to buffer: both are host memory, copied at submission.
extern "system" fn copy_buffer_region(
    this: This,
    dst: This,
    dst_offset: u64,
    src: This,
    src_offset: u64,
    len: u64,
) {
    #[cfg(windows)]
    if let Some(list) = host_list(this)
        && let Some(to) = buffer_range(dst, dst_offset, len)
        && let Some(from) = buffer_range(src, src_offset, len)
    {
        list.defer_copy(to, from, len as usize);
    }
    #[cfg(not(windows))]
    let _ = (this, dst, dst_offset, src, src_offset, len);
}

extern "system" fn copy_resource(this: This, dst: This, src: This) {
    #[cfg(windows)]
    {
        let Some(list) = host_list(this) else {
            return;
        };
        if let (Some(to), Some(from)) = (
            crate::resource::host_resource(dst),
            crate::resource::host_resource(src),
        ) {
            ensure_state(list, dst, COPY_DEST);
            ensure_state(list, src, COPY_SOURCE);
            // SAFETY: an open list and live textures.
            unsafe { list.list.CopyResource(to, from) };
        } else if let (Some((to, a)), Some((from, b))) = (
            crate::resource::buffer_bytes(dst),
            crate::resource::buffer_bytes(src),
        ) {
            list.defer_copy(to, from, a.min(b) as usize);
        }
    }
    #[cfg(not(windows))]
    let _ = (this, dst, src);
}

extern "system" fn copy_resource_x(this: This, dst: This, src: This, _flags: u32) {
    copy_resource(this, dst, src);
}

/// A copy location as titles pass it, laid out as the host's: a resource,
/// then a subresource index (type 0) or a placed footprint (type 1).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CopyLocation {
    resource: This,
    kind: u32,
    footprint: crate::resource::PlacedFootprint,
}

/// Texture to texture on the GPU; buffer to texture through an upload
/// buffer filled at submission. Texture to buffer (read back) is not done
/// yet.
extern "system" fn copy_texture_region(
    this: This,
    dst: *const CopyLocation,
    x: u32,
    y: u32,
    z: u32,
    src: *const CopyLocation,
    source_box: *const [u32; 6],
) {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Direct3D12::{D3D12_BOX, D3D12_TEXTURE_COPY_LOCATION};
        use windows::core::Interface;
        const FOOTPRINT: u32 = 1;
        // SAFETY: the title passes both locations (null is tolerated).
        let (Some(list), Some(&dst), Some(&src)) =
            (host_list(this), unsafe { dst.as_ref() }, unsafe {
                src.as_ref()
            })
        else {
            return;
        };
        let Some(to) = crate::resource::host_resource(dst.resource) else {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::warn!("texture to buffer copies not implemented"));
            return;
        };
        let mut from = src;
        let _upload; // keeps the source alive while this call records it
        if src.kind == FOOTPRINT {
            let (Some(host), Some(bytes)) = (crate::host::host(), src.footprint.bytes()) else {
                return;
            };
            let Some(data) = buffer_range(src.resource, src.footprint.offset, bytes) else {
                return;
            };
            let Some((buffer, staging)) = list.upload(host, bytes) else {
                return;
            };
            list.defer_copy(staging, data, bytes as usize);
            from.resource = buffer.as_raw().cast_const().cast();
            from.footprint.offset = 0;
            _upload = buffer;
        } else {
            let Some(texture) = crate::resource::host_resource(src.resource) else {
                return;
            };
            ensure_state(list, src.resource, COPY_SOURCE);
            from.resource = texture.as_raw().cast_const().cast();
        }
        ensure_state(list, dst.resource, COPY_DEST);
        let mut to_location = dst;
        to_location.resource = to.as_raw().cast_const().cast();
        let host_box = (!source_box.is_null()).then(|| source_box.cast::<D3D12_BOX>());
        const _: () =
            assert!(size_of::<CopyLocation>() == size_of::<D3D12_TEXTURE_COPY_LOCATION>());
        // SAFETY: an open list; both locations in the host's layout, naming
        // live resources; the box is the title's (same layout).
        unsafe {
            list.list.CopyTextureRegion(
                (&raw const to_location).cast(),
                x,
                y,
                z,
                (&raw const from).cast(),
                host_box,
            )
        };
    }
    #[cfg(not(windows))]
    let _ = (this, dst, x, y, z, src, source_box);
}

#[allow(clippy::too_many_arguments)] // the method's own arguments
extern "system" fn copy_texture_region_x(
    this: This,
    dst: *const CopyLocation,
    x: u32,
    y: u32,
    z: u32,
    src: *const CopyLocation,
    source_box: *const [u32; 6],
    _flags: u32,
) {
    copy_texture_region(this, dst, x, y, z, src, source_box);
}

extern "system" fn resolve(
    this: This,
    dst: This,
    dst_sub: u32,
    src: This,
    src_sub: u32,
    format: u32,
) {
    #[cfg(windows)]
    if let Some(list) = host_list(this)
        && let (Some(to), Some(from)) = (
            crate::resource::host_resource(dst),
            crate::resource::host_resource(src),
        )
    {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::Graphics::Direct3D12::{
            D3D12_RESOLVE_MODE_AVERAGE, ID3D12GraphicsCommandList1,
        };
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
        use windows::core::Interface;
        const RESOLVE_DEST: u32 = 0x1000;
        const RESOLVE_SOURCE: u32 = 0x2000;
        ensure_state(list, dst, RESOLVE_DEST);
        ensure_state(list, src, RESOLVE_SOURCE);
        // SAFETY: an open list and live textures; the region is inside both.
        unsafe {
            let (a, b) = (to.GetDesc(), from.GetDesc());
            let format = DXGI_FORMAT(format as i32);
            if (a.Width, a.Height) == (b.Width, b.Height) {
                list.list
                    .ResolveSubresource(to, dst_sub, from, src_sub, format);
            } else if let Ok(list1) = list.list.cast::<ID3D12GraphicsCommandList1>() {
                // The console clips a larger source to the destination.
                let rect = RECT {
                    left: 0,
                    top: 0,
                    right: a.Width.min(b.Width) as i32,
                    bottom: a.Height.min(b.Height) as i32,
                };
                list1.ResolveSubresourceRegion(
                    to,
                    dst_sub,
                    0,
                    0,
                    from,
                    src_sub,
                    Some(&rect),
                    format,
                    D3D12_RESOLVE_MODE_AVERAGE,
                );
            }
        }
    }
    #[cfg(not(windows))]
    let _ = (this, dst, dst_sub, src, src_sub, format);
}

#[repr(C)]
pub(crate) struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

extern "system" fn clear_rtv(
    this: This,
    handle: u64,
    color: *const [f32; 4],
    count: u32,
    rects: *const Rect,
) {
    #[cfg(windows)]
    {
        let (Some(list), Some(record)) = (host_list(this), crate::descriptor::rtv_record(handle))
        else {
            return;
        };
        let (Some(texture), Some(host)) = (
            crate::resource::host_resource(record.resource),
            crate::host::host(),
        ) else {
            return;
        };
        // SAFETY: the title passes four floats (null is tolerated).
        let Some(color) = (unsafe { color.as_ref() }) else {
            return;
        };
        let rects = (count > 0 && !rects.is_null()).then(|| {
            // SAFETY: the title passes `count` rectangles, laid out as the host's.
            unsafe { std::slice::from_raw_parts(rects.cast(), count as usize) }
        });
        const RENDER_TARGET: u32 = 0x4;
        let rtv = host.rtv(texture, record.desc().as_ref());
        // SAFETY: an open list, a fresh view of a live texture.
        unsafe {
            if let Some((_, before)) =
                crate::resource::host_transition(record.resource, RENDER_TARGET)
            {
                use windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES as State;
                list.list.ResourceBarrier(&[crate::host::transition(
                    texture,
                    State(before as i32),
                    State(RENDER_TARGET as i32),
                )]);
            }
            list.list.ClearRenderTargetView(rtv, color, rects);
        }
    }
    #[cfg(not(windows))]
    let _ = (this, handle, color, count, rects);
}

/// IUnknown 3, object 4, device child 1, allocator 2.
static ALLOCATOR_FUNCTIONS: Table<10> = {
    let mut f = object::d3d12_object(object::missing_table::<ALLOCATOR, 10>());
    f[0] = allocator_query as *const ();
    f[2] = allocator_release as *const ();
    f[7] = object::parent_device as *const (); // 0x38
    f[8] = allocator_reset as *const (); // 0x40
    f[9] = object::ignore as *const (); // 0x48 allocation callback
    Table(f)
};

/// IUnknown 3, object 4, device child 1, list 1, graphics list 96 (59 as
/// on PC, then console ones), then versions 1-6 (6 + 1 + 1 + 11 + 3 + 13).
static LIST_FUNCTIONS: Table<140> = {
    let mut f = object::d3d12_object(object::missing_table::<LIST, 140>());
    f[0] = list_query as *const ();
    f[2] = list_release as *const ();
    f[7] = object::parent_device as *const (); // 0x38
    f[8] = list_type as *const (); // 0x40
    f[9] = close as *const (); // 0x48
    f[10] = reset as *const (); // 0x50
    f[15] = copy_buffer_region as *const (); // 0x78
    f[16] = copy_texture_region as *const (); // 0x80
    f[17] = copy_resource as *const (); // 0x88
    f[19] = resolve as *const (); // 0x98
    f[26] = barrier as *const (); // 0xd0
    f[83] = copy_texture_region_x as *const (); // 0x298
    f[84] = copy_resource_x as *const (); // 0x2a0
    f[52] = clear_rtv as *const (); // 0x1a0
    Table(f)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::slot;

    #[test]
    fn every_inline_command_falls_back_to_the_table() {
        let null = std::ptr::null();
        let mut allocator = null;
        assert_eq!(
            create_allocator(null, 0, &ALLOCATOR_IIDS[0], &mut allocator),
            S_OK
        );
        let mut list = null;
        let hr = create_list(null, 0, 0, allocator, null, &LIST_IIDS[1], &mut list);
        assert_eq!(hr, S_OK);
        // What the title's inline draw checks: put < draw limit.
        // SAFETY: the list is 0x560 bytes.
        let (put, draw_limit, api_limit) = unsafe {
            let at = |o: usize| list.cast::<u8>().add(o).cast::<u64>().read();
            (at(0x18), at(0x28), at(0x30))
        };
        assert!(put >= draw_limit && put >= api_limit);
        assert_eq!(slot::<extern "system" fn(This) -> u32>(list, 0x40)(list), 0);
        assert_eq!(
            slot::<extern "system" fn(This) -> HRESULT>(list, 0x48)(list),
            S_OK
        );
        let release = |o: This| slot::<extern "system" fn(This) -> u32>(o, 0x10)(o);
        assert_eq!(release(list), 0);
        assert_eq!(release(allocator), 0);
    }
}
