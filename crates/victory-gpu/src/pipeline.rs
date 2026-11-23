//! Pipeline states. Both descriptions start as on PC: the root signature,
//! then the shader stages (five for graphics, one for compute); only those
//! are read so far.
//!
//! shortcut: the console shader containers are kept, not translated, and
//! the rest of the description is ignored. Translate through the cache
//! here (TODO.md, GPU: pipeline creation, decision 0003). The 32-byte
//! packet after the header stays zero: titles only copy it inline, which
//! our null command-list limits keep from running.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::mem::offset_of;

use crate::object::{self, E_INVALIDARG, HRESULT, Iid, Object, PIPELINE, Table, This, guid};
use crate::resource::PAGEABLE_IIDS;

const PIPELINE_IIDS: [Iid; 4] = [
    guid(
        0x765a30f3,
        0xf624,
        0x4c6f,
        [0xa8, 0x28, 0xac, 0xe9, 0x48, 0x62, 0x24, 0x45],
    ),
    PAGEABLE_IIDS[0],
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

#[repr(C)]
pub(crate) struct Bytecode {
    code: *const u8,
    size: usize,
}

#[repr(C)]
pub(crate) struct GraphicsHead {
    root: This,
    /// Vertex, pixel, domain, hull, geometry.
    stages: [Bytecode; 5],
}

#[repr(C)]
pub(crate) struct ComputeHead {
    root: This,
    stage: Bytecode,
}

#[repr(C)]
struct Pipeline {
    object: Object,
    packet: [u32; 8],
    /// Each stage's container, empty when the stage is unused.
    shaders: Vec<Vec<u8>>,
}
const _: () = assert!(offset_of!(Pipeline, packet) == 0x10);

fn copy(stage: &Bytecode) -> Vec<u8> {
    if stage.code.is_null() || stage.size == 0 {
        return Vec::new();
    }
    // SAFETY: the title passes `size` bytes of shader code.
    unsafe { std::slice::from_raw_parts(stage.code, stage.size) }.to_vec()
}

fn hand_out(shaders: Vec<Vec<u8>>, iid: *const Iid, out: *mut This) -> HRESULT {
    let sizes: Vec<usize> = shaders.iter().map(Vec::len).collect();
    tracing::debug!(?sizes, "pipeline state");
    let pipeline = Box::new(Pipeline {
        object: Object::new(&FUNCTIONS),
        packet: [0; 8],
        shaders,
    });
    object::hand_out(PIPELINE, pipeline, iid, &PIPELINE_IIDS, out)
}

/// Device method.
pub(crate) extern "system" fn create_graphics(
    _: This,
    desc: *const GraphicsHead,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    hand_out(desc.stages.iter().map(copy).collect(), iid, out)
}

/// Device method.
pub(crate) extern "system" fn create_compute(
    _: This,
    desc: *const ComputeHead,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    hand_out(vec![copy(&desc.stage)], iid, out)
}

extern "system" fn query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    let found = object::is(iid, &PIPELINE_IIDS).then_some(this);
    object::answer(PIPELINE, iid, found, out)
}
extern "system" fn release(this: This) -> u32 {
    object::release_boxed::<Pipeline>(this)
}

/// IUnknown 3, object 4, device child 1, pipeline 3.
static FUNCTIONS: Table<11> = {
    let mut f = object::d3d12_object(object::missing_table::<PIPELINE, 11>());
    f[0] = query as *const ();
    f[2] = release as *const ();
    f[7] = object::parent_device as *const (); // 0x38
    Table(f)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{S_OK, slot};

    #[test]
    fn pipelines_keep_their_shaders() {
        let (vs, ps) = ([1u8, 2, 3], [4u8; 5]);
        let none = || Bytecode {
            code: std::ptr::null(),
            size: 0,
        };
        let desc = GraphicsHead {
            root: std::ptr::null(),
            stages: [
                Bytecode {
                    code: vs.as_ptr(),
                    size: vs.len(),
                },
                Bytecode {
                    code: ps.as_ptr(),
                    size: ps.len(),
                },
                none(),
                none(),
                none(),
            ],
        };
        let mut p = std::ptr::null();
        assert_eq!(
            create_graphics(std::ptr::null(), &desc, &PIPELINE_IIDS[0], &mut p),
            S_OK
        );
        // SAFETY: one of our pipelines.
        let shaders = unsafe { &(*p.cast::<Pipeline>()).shaders };
        assert_eq!(shaders[0], vs);
        assert_eq!(shaders[1], ps);
        assert!(shaders[2].is_empty());
        assert_eq!(slot::<extern "system" fn(This) -> u32>(p, 0x10)(p), 0);
    }
}
