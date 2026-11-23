//! The console's audio hardware (`AcpHal.dll`, `xapu.dll`), enough for
//! middleware to start and then mix in software:
//!
//! * the APU heap: host memory with 32-bit device addresses;
//! * SHAPE context arrays allocated from it;
//! * the audio coprocessor's command and message queues, where every
//!   command completes at once and is acknowledged as the hardware would;
//! * the APU decoder's connections, which accept no work.
//!
//! What the hardware computes (XMA decoding, hardware rate conversion, EQ,
//! mixing, Opus decoding) is not emulated, so voices that need it stay
//! silent. Software-decoded audio plays through the host's WASAPI, which
//! middleware reaches through COM like on PC. fmod's console build sets
//! all of this up inside `System_Create` and waits there for its first
//! command to be acknowledged.
#![allow(non_snake_case)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::alloc::Layout;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use super::xasync::E_INVALIDARG;
use super::xgameruntime::{HRESULT, S_OK};
use crate::{HleModule, hle_export};

const E_FAIL: HRESULT = 0x8000_4005_u32 as i32;
const E_OUTOFMEMORY: HRESULT = 0x8007_000E_u32 as i32;
/// The decoder's "no result yet", which fmod checks for.
const PENDING_RESULTS: HRESULT = 0x8AC8_1001_u32 as i32;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// The APU heap.

struct Block {
    size: usize,
    apu: u32,
    layout: Layout,
}

struct Heap {
    /// By host address.
    blocks: BTreeMap<usize, Block>,
    next_apu: u32,
}

static HEAP: Mutex<Heap> = Mutex::new(Heap {
    blocks: BTreeMap::new(),
    next_apu: 0x0001_0000,
});

/// Zeroed host memory and its device address.
// shortcut: device addresses are handed out once and never reused, so the
// 32-bit space runs out after 4 GiB of allocations in total; reuse freed
// ranges if a title streams that much through the heap.
fn heap_alloc(size: usize, align: usize) -> Option<(*mut u8, u32)> {
    let layout = Layout::from_size_align(size.max(1), align.max(16)).ok()?;
    let mut heap = lock(&HEAP);
    let apu = heap.next_apu.checked_next_multiple_of(align as u32)?;
    let next = apu.checked_add(u32::try_from(layout.size()).ok()?)?;
    // SAFETY: non-zero size.
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        return None;
    }
    heap.next_apu = next;
    heap.blocks
        .insert(ptr as usize, Block { size, apu, layout });
    Some((ptr, apu))
}

fn heap_free(ptr: *mut u8) {
    let block = lock(&HEAP).blocks.remove(&(ptr as usize));
    if let Some(block) = block {
        // SAFETY: allocated by `heap_alloc` with this layout.
        unsafe { std::alloc::dealloc(ptr, block.layout) };
    }
}

/// The device address of any byte of a heap block, 0 elsewhere.
fn heap_map(ptr: usize) -> u32 {
    let heap = lock(&HEAP);
    match heap.blocks.range(..=ptr).next_back() {
        Some((&start, block)) if ptr < start + block.size.max(1) => {
            block.apu + (ptr - start) as u32
        }
        _ => 0,
    }
}

/// The host address of a device address, if a heap block holds it.
fn heap_unmap(apu: u32) -> Option<*mut u8> {
    let heap = lock(&HEAP);
    heap.blocks
        .iter()
        .find(|(_, b)| (b.apu..b.apu.saturating_add(b.size as u32)).contains(&apu))
        .map(|(&start, b)| (start + (apu - b.apu) as usize) as *mut u8)
}

pub extern "system" fn ApuAlloc(
    out: *mut *mut c_void,
    apu: *mut u32,
    size: u32,
    align: u32,
    _flags: u32,
) -> HRESULT {
    if out.is_null() || size == 0 || align < 4 || !align.is_power_of_two() {
        return E_INVALIDARG;
    }
    let Some((ptr, address)) = heap_alloc(size as usize, align as usize) else {
        return E_OUTOFMEMORY;
    };
    // SAFETY: checked non-null; optional out parameter checked below.
    unsafe {
        out.write(ptr.cast());
        if !apu.is_null() {
            apu.write(address);
        }
    }
    S_OK
}

pub extern "system" fn ApuFree(ptr: *mut c_void) {
    heap_free(ptr.cast());
}

pub extern "system" fn ApuMapVirtualAddress(ptr: *const c_void) -> u32 {
    heap_map(ptr as usize)
}

pub extern "system" fn ApuIsVirtualAddressValid(ptr: *const c_void, align: u32) -> bool {
    let apu = heap_map(ptr as usize);
    apu != 0 && (align == 0 || apu.is_multiple_of(align))
}

// SHAPE context arrays.

/// Six counts (rate conversion, EQ/compression, filter/volume, DMA, XMA,
/// PCM), then the six arrays and their device addresses.
#[repr(C, packed)]
pub struct ShapeContexts {
    counts: [u32; 6],
    arrays: [*mut c_void; 6],
    apu: [u32; 6],
}

/// Bytes per context: rate conversion, DMA and XMA as fmod clears them;
/// the others (not seen yet) get a generous upper bound.
const CONTEXT_BYTES: [usize; 6] = [32, 256, 256, 16, 64, 256];

static CONTEXTS: Mutex<Option<[usize; 6]>> = Mutex::new(None);

pub extern "system" fn AcpHalAllocateShapeContexts(contexts: *mut ShapeContexts) -> HRESULT {
    if contexts.is_null() {
        return E_INVALIDARG;
    }
    let mut allocated = lock(&CONTEXTS);
    if allocated.is_some() {
        return E_FAIL;
    }
    // SAFETY: the title passes its request; packed fields are copied out.
    let counts = unsafe { std::ptr::addr_of!((*contexts).counts).read_unaligned() };
    let mut arrays: [*mut c_void; 6] = [std::ptr::null_mut(); 6];
    let mut addresses = [0u32; 6];
    for i in 0..6 {
        if counts[i] == 0 {
            continue;
        }
        let Some((ptr, apu)) = heap_alloc(counts[i] as usize * CONTEXT_BYTES[i], 256) else {
            arrays.iter().for_each(|&p| heap_free(p.cast()));
            return E_OUTOFMEMORY;
        };
        (arrays[i], addresses[i]) = (ptr.cast(), apu);
    }
    // SAFETY: as above.
    unsafe {
        std::ptr::addr_of_mut!((*contexts).arrays).write_unaligned(arrays);
        std::ptr::addr_of_mut!((*contexts).apu).write_unaligned(addresses);
    }
    *allocated = Some(arrays.map(|p| p as usize));
    S_OK
}

pub extern "system" fn AcpHalReleaseShapeContexts() -> HRESULT {
    match lock(&CONTEXTS).take() {
        Some(arrays) => {
            arrays.iter().for_each(|&p| heap_free(p as *mut u8));
            S_OK
        }
        None => E_FAIL,
    }
}

// The audio coprocessor.

const LOAD_FLOWGRAPH: u32 = 1;
const REGISTER_MESSAGES: u32 = 2;
const UNREGISTER_MESSAGES: u32 = 3;

const FLOWGRAPH_COMPLETED: u32 = 0x02;
const COMMAND_COMPLETED: u32 = 0x10;
const DISCONNECTED: u32 = 0x80;

/// Audio frames of 128 samples at 48 kHz.
const FRAMES_PER_SECOND: u64 = 375;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Message {
    kind: u32,
    dropped: u32,
    usec: u32,
    data: [u8; 16],
}

struct State {
    connected: bool,
    /// Message kinds the title registered for.
    mask: u32,
    messages: VecDeque<Message>,
    since: Instant,
}

impl State {
    fn usec(&self) -> u64 {
        self.since.elapsed().as_micros() as u64
    }

    fn frame(&self) -> u32 {
        (self.usec() * FRAMES_PER_SECOND / 1_000_000) as u32
    }

    // shortcut: the queue is unbounded and never reports dropped messages;
    // bound it at the size given to `connect` if a title relies on drops.
    fn post(&mut self, kind: u32, data: &[u8]) {
        if self.mask & kind == 0 {
            return;
        }
        let mut message = Message {
            kind,
            dropped: 0,
            usec: self.usec() as u32,
            data: [0; 16],
        };
        message.data[..data.len()].copy_from_slice(data);
        self.messages.push_back(message);
    }
}

/// The interface object: its method table, then our state.
#[repr(C)]
pub struct Acp {
    vtable: &'static AcpMethods,
    state: Mutex<State>,
}

#[repr(C)]
struct AcpMethods {
    connect: extern "system" fn(*mut Acp, u32, u32) -> HRESULT,
    disconnect: extern "system" fn(*mut Acp) -> HRESULT,
    submit: extern "system" fn(*mut Acp, u32, u64, u32, *const c_void, u32) -> HRESULT,
    pop_message: extern "system" fn(*mut Acp, *mut Message) -> bool,
    message_count: extern "system" fn(*mut Acp) -> u32,
    release: extern "system" fn(*mut Acp),
}

static ACP_METHODS: AcpMethods = AcpMethods {
    connect,
    disconnect,
    submit,
    pop_message,
    message_count,
    release,
};

fn state(this: *mut Acp) -> MutexGuard<'static, State> {
    // SAFETY: title code calls methods on the object `AcpHalCreate` made,
    // which lives until `release`.
    lock(unsafe { &(*this).state })
}

extern "system" fn connect(this: *mut Acp, commands: u32, _messages: u32) -> HRESULT {
    if commands == 0 {
        return E_INVALIDARG;
    }
    state(this).connected = true;
    S_OK
}

extern "system" fn disconnect(this: *mut Acp) -> HRESULT {
    let mut state = state(this);
    if !state.connected {
        return E_FAIL;
    }
    state.post(DISCONNECTED, &[]);
    state.connected = false;
    S_OK
}

/// Done at once: message registrations take effect, a flowgraph completes
/// unprocessed, a command with an id is acknowledged, and its notification
/// word is set.
extern "system" fn submit(
    this: *mut Acp,
    command: u32,
    id: u64,
    _frame: u32,
    data: *const c_void,
    notification: u32,
) -> HRESULT {
    static SEEN: Mutex<u64> = Mutex::new(0);
    if command < 64 && *lock(&SEEN) & 1 << command == 0 {
        *lock(&SEEN) |= 1 << command;
        tracing::info!(command, "first coprocessor command of this kind");
    }
    let mut state = state(this);
    let word = |at: usize| -> Option<u32> {
        // SAFETY: the command's data starts with these u32 fields.
        (!data.is_null()).then(|| unsafe { data.cast::<u32>().add(at).read_unaligned() })
    };
    match command {
        REGISTER_MESSAGES => state.mask |= word(0).unwrap_or(0),
        UNREGISTER_MESSAGES => state.mask &= !word(0).unwrap_or(0),
        LOAD_FLOWGRAPH => {
            let flowgraph = word(1).unwrap_or(0);
            state.post(FLOWGRAPH_COMPLETED, &flowgraph.to_le_bytes());
        }
        _ => {}
    }
    if id != 0 {
        let mut done = [0u8; 16];
        done[..4].copy_from_slice(&command.to_le_bytes());
        done[4..12].copy_from_slice(&id.to_le_bytes());
        done[12..].copy_from_slice(&state.frame().to_le_bytes());
        state.post(COMMAND_COMPLETED, &done);
    }
    if notification != 0
        && let Some(word) = heap_unmap(notification)
    {
        // SAFETY: a u32 in a heap block, which the title reads as volatile.
        unsafe { word.cast::<u32>().write_volatile(1) };
    }
    S_OK
}

extern "system" fn pop_message(this: *mut Acp, out: *mut Message) -> bool {
    let Some(message) = state(this).messages.pop_front() else {
        return false;
    };
    if !out.is_null() {
        // SAFETY: the title passes room for one message.
        unsafe { out.write_unaligned(message) };
    }
    true
}

extern "system" fn message_count(this: *mut Acp) -> u32 {
    state(this).messages.len() as u32
}

extern "system" fn release(this: *mut Acp) {
    // SAFETY: made by `AcpHalCreate` with `Box::into_raw`; released once.
    drop(unsafe { Box::from_raw(this) });
}

pub extern "system" fn AcpHalCreate(out: *mut *mut Acp) -> HRESULT {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let acp = Box::new(Acp {
        vtable: &ACP_METHODS,
        state: Mutex::new(State {
            connected: false,
            mask: 0,
            messages: VecDeque::new(),
            since: Instant::now(),
        }),
    });
    // SAFETY: checked non-null.
    unsafe { out.write(Box::into_raw(acp)) };
    S_OK
}

// The APU decoder: connections get their memory, commands are refused.

#[repr(C)]
pub struct ConnectInput {
    process: u32,
    streams: u32,
    commands_per_stream: u32,
    memory: u32,
    frames_per_output: u32,
    options: u32,
}

#[repr(C)]
pub struct ConnectOutput {
    memory: *mut c_void,
    result_event: *mut c_void,
}

/// The memory a connection hands out.
pub struct Connection(*mut u8, Layout);

pub extern "system" fn XApuConnect(
    input: *const ConnectInput,
    output: *mut ConnectOutput,
    handle: *mut *mut Connection,
) -> HRESULT {
    const RESULT_READY_EVENT: u32 = 8;
    if input.is_null() || output.is_null() || handle.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: checked non-null.
    let input = unsafe { &*input };
    if input.options & RESULT_READY_EVENT != 0 {
        // shortcut: no event to signal, since no result ever comes; create
        // one if a title asks for it.
        return E_FAIL;
    }
    let Ok(layout) = Layout::from_size_align(input.memory.max(16) as usize, 16) else {
        return E_INVALIDARG;
    };
    // SAFETY: non-zero size.
    let memory = unsafe { std::alloc::alloc_zeroed(layout) };
    if memory.is_null() {
        return E_OUTOFMEMORY;
    }
    // SAFETY: checked non-null.
    unsafe {
        output.write(ConnectOutput {
            memory: memory.cast(),
            result_event: std::ptr::null_mut(),
        });
        handle.write(Box::into_raw(Box::new(Connection(memory, layout))));
    }
    S_OK
}

/// Nothing is decoded: the stream fails instead of waiting forever.
pub extern "system" fn XApuEnqueueCommand() -> HRESULT {
    E_FAIL
}

pub extern "system" fn XApuDequeueResult() -> HRESULT {
    PENDING_RESULTS
}

pub extern "system" fn XApuDisconnect(handle: *mut Connection) -> HRESULT {
    if handle.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: made by `XApuConnect`; disconnected once.
    let Connection(memory, layout) = *unsafe { Box::from_raw(handle) };
    // SAFETY: allocated in `XApuConnect` with this layout.
    unsafe { std::alloc::dealloc(memory, layout) };
    S_OK
}

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "audio",
        dlls: &["acphal.dll", "xapu.dll"],
        exports: vec![
            hle_export!(AcpHalAllocateShapeContexts),
            hle_export!(AcpHalReleaseShapeContexts),
            hle_export!(AcpHalCreate),
            hle_export!(ApuAlloc),
            hle_export!(ApuFree),
            hle_export!(ApuIsVirtualAddressValid),
            hle_export!(ApuMapVirtualAddress),
            hle_export!(XApuConnect),
            hle_export!(XApuEnqueueCommand),
            hle_export!(XApuDequeueResult),
            hle_export!(XApuDisconnect),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heap_maps_every_byte_of_a_block() {
        let (mut ptr, mut apu) = (std::ptr::null_mut(), 0);
        assert_eq!(ApuAlloc(&mut ptr, &mut apu, 0x100, 128, 1), S_OK);
        assert!(apu != 0 && apu.is_multiple_of(128));
        let inner = ptr.cast::<u8>().wrapping_add(0x40).cast_const().cast();
        assert_eq!(ApuMapVirtualAddress(inner), apu + 0x40);
        assert!(ApuIsVirtualAddressValid(ptr, 128));
        assert_eq!(heap_unmap(apu + 4), Some(ptr.cast::<u8>().wrapping_add(4)));
        ApuFree(ptr);
        assert_eq!(ApuMapVirtualAddress(ptr), 0);
        assert_eq!(ApuAlloc(&mut ptr, &mut apu, 0x100, 6, 1), E_INVALIDARG);
    }

    #[test]
    fn startup_as_fmod_does_it() {
        // Context arrays: rate conversion, DMA and XMA.
        let mut request = ShapeContexts {
            counts: [0x200, 0, 0, 0x400, 0x200, 0],
            arrays: [std::ptr::null_mut(); 6],
            apu: [0; 6],
        };
        assert_eq!(size_of::<ShapeContexts>(), 0x60);
        assert_eq!(AcpHalAllocateShapeContexts(&mut request), S_OK);
        let arrays = request.arrays;
        assert!(!arrays[0].is_null() && !arrays[3].is_null() && !arrays[4].is_null());
        assert!(arrays[1].is_null());
        // SAFETY: 0x200 rate-conversion contexts of 32 bytes were allocated.
        unsafe { arrays[0].cast::<u8>().write_bytes(0, 0x200 << 5) };

        // Connect, register every message but frame starts, then wait for
        // the registration to be acknowledged.
        let mut acp = std::ptr::null_mut();
        assert_eq!(AcpHalCreate(&mut acp), S_OK);
        // SAFETY: just created.
        let m = unsafe { (*acp).vtable };
        assert_eq!((m.connect)(acp, 0x804, 0x804), S_OK);
        let mask = 0xfe_u32;
        let data = (&raw const mask).cast();
        assert_eq!(
            (m.submit)(acp, REGISTER_MESSAGES, 1, u32::MAX, data, 0),
            S_OK
        );
        let mut message = Message {
            kind: 0,
            dropped: 0,
            usec: 0,
            data: [0; 16],
        };
        assert!((m.pop_message)(acp, &mut message));
        let (kind, dropped) = (message.kind, message.dropped);
        assert_eq!((kind, dropped), (COMMAND_COMPLETED, 0));
        assert_eq!(
            u64::from_le_bytes(message.data[4..12].try_into().unwrap()),
            1
        );
        assert!(!(m.pop_message)(acp, &mut message));
        assert_eq!((m.disconnect)(acp), S_OK);
        assert_eq!((m.message_count)(acp), 1, "disconnected");
        (m.release)(acp);
        assert_eq!(AcpHalReleaseShapeContexts(), S_OK);
    }
}
