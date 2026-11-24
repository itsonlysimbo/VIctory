//! Host descriptor heaps behind the title's shader-visible heaps (decision
//! 0007, step 3). Translated shaders index the heaps directly at the
//! title's descriptor positions, so each host heap keeps the title heap's
//! order: slot `i` is made from what the title wrote at slot `i`, when it
//! writes it (view creation and descriptor copies) and again when a draw
//! reads it through a table and finds the slot changed.

use std::sync::Mutex;

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::{VIEW_TAG, View, view_at};
use crate::host::host;

pub(crate) struct HostHeap {
    pub(crate) heap: ID3D12DescriptorHeap,
    cpu: usize,
    step: u32,
    /// Slots mirroring the title's; view heaps have [`DYNAMIC`] more, then
    /// their guard slot ([`HostHeap::guard_slot`]).
    count: u32,
    sampler: bool,
    /// The record (view index and generation) each slot was last made from.
    made: Mutex<Vec<u64>>,
    /// The host resource each slot views, held while the slot does: shaders
    /// index heaps directly, so a slot the title has not rewritten since it
    /// released the resource may still be read, and a freed host resource
    /// read there is a GPU page fault (the console reads stale memory).
    kept: Mutex<Vec<Option<ID3D12Resource>>>,
    /// Number of dynamic slots appended past `count` (0 for sampler heaps).
    dynamic: u32,
    /// Where the search for free dynamic slots starts, and which are taken
    /// (by lists not yet reset).
    next: Mutex<(u32, Vec<bool>)>,
    /// Per-slot element byte stride (`u32`) for structured buffers whose
    /// stride shader analysis could not recover statically.
    strides: Option<StrideTable>,
}

struct StrideTable {
    _resource: ID3D12Resource,
    ptr: usize,
    gpu: u64,
    len: u32,
}

impl StrideTable {
    fn new(device: &ID3D12Device, len: u32) -> Option<Self> {
        // [`SLOT_DWORDS`] dwords a slot.
        let bytes = (u64::from(len) * 4 * SLOT_DWORDS).max(4);
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: bytes,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut resource: Option<ID3D12Resource> = None;
        let mut p = std::ptr::null_mut();
        // SAFETY: plain committed upload buffer creation and persistent CPU map.
        let gpu = unsafe {
            device
                .CreateCommittedResource(
                    &D3D12_HEAP_PROPERTIES {
                        Type: D3D12_HEAP_TYPE_UPLOAD,
                        ..Default::default()
                    },
                    D3D12_HEAP_FLAG_NONE,
                    &desc,
                    D3D12_RESOURCE_STATE_GENERIC_READ,
                    None,
                    &mut resource,
                )
                .ok()?;
            let res = resource.as_ref()?;
            res.Map(0, None, Some(&mut p)).ok()?;
            std::ptr::write_bytes(p.cast::<u8>(), 0, bytes as usize);
            res.GetGPUVirtualAddress()
        };
        Some(Self {
            _resource: resource?,
            ptr: p as usize,
            gpu,
            len,
        })
    }
}

impl Drop for HostHeap {
    fn drop(&mut self) {
        if let Ok(mut kept) = self.kept.lock() {
            for res in kept.drain(..).flatten() {
                crate::host::retire(res);
            }
        }
    }
}

/// Host heaps' GPU ranges (start, bytes, bytes per slot, sampler), for
/// device-removed reports: a shader indexing a heap past its end faults.
static GPU_RANGES: Mutex<Vec<(u64, u64, u64, bool)>> = Mutex::new(Vec::new());

/// For device-removed reports: where host GPU address `va` lies against
/// the host descriptor heaps.
pub(crate) fn near_heaps(va: u64) -> String {
    let ranges = GPU_RANGES.lock().unwrap_or_else(|e| e.into_inner());
    let mut text = String::new();
    for &(start, bytes, step, sampler) in ranges.iter() {
        let kind = if sampler { "sampler" } else { "view" };
        let place = if va < start {
            format!("{:#x} below", start - va)
        } else if va < start + bytes {
            format!("inside, slot {}", (va - start) / step)
        } else {
            format!(
                "{:#x} past its end (slot {})",
                va - start - bytes,
                (va - start) / step
            )
        };
        text.push_str(&format!("; {kind} heap {start:#x}+{bytes:#x}: {place}"));
    }
    text
}

/// The host's limit for shader-visible view heaps on resource binding
/// tiers 1 and 2; tier 3 hardware may take more.
const PORTABLE_VIEWS: u32 = 1_000_000;

/// Slots past the title's in a view heap, for tables a draw needs made
/// afresh (constant buffers copied per draw). A list holds the slots it
/// took until it is reset (its submissions have run) or released.
const DYNAMIC: u32 = 1 << 19;
/// Smaller dynamic reserve when the host caps the whole view heap at
/// [`PORTABLE_VIEWS`] (NVIDIA). World frames ran out of 2^16 there; 2^18
/// leaves 737,856 title slots below the cap (tables past them are made
/// afresh too).
const DYNAMIC_CAPPED: u32 = 1 << 18;

impl HostHeap {
    pub(crate) fn new(sampler: bool, count: u32) -> Option<Self> {
        let host = host()?;
        let kind = if sampler {
            D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER
        } else {
            D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV
        };
        let make = |count: u32| -> windows::core::Result<ID3D12DescriptorHeap> {
            // SAFETY: plain heap creation.
            unsafe {
                host.device
                    .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: kind,
                        NumDescriptors: count,
                        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                        NodeMask: 0,
                    })
            }
        };
        // A view heap ends with its guard slot (see [`HostHeap::guard_slot`]),
        // one past its dynamic slots.
        let extra = if sampler { 0 } else { DYNAMIC };
        let guard = u32::from(!sampler);
        let (heap, count, extra) = match make(count + extra + guard) {
            Ok(heap) => (heap, count, extra),
            Err(e) if !sampler && count + extra + guard > PORTABLE_VIEWS => {
                // shortcut: descriptors past the host's limit are left out;
                // a title that reaches them needs remapping.
                tracing::warn!(error = %e, count, "host view heap capped at {PORTABLE_VIEWS}");
                (
                    make(PORTABLE_VIEWS).ok()?,
                    PORTABLE_VIEWS - DYNAMIC_CAPPED - 1,
                    DYNAMIC_CAPPED,
                )
            }
            Err(e) => {
                tracing::warn!(error = %e, count, sampler, "no host descriptor heap");
                return None;
            }
        };
        // SAFETY: a live heap.
        let (cpu, step) = unsafe {
            (
                heap.GetCPUDescriptorHandleForHeapStart().ptr,
                host.device.GetDescriptorHandleIncrementSize(kind),
            )
        };
        tracing::debug!(count, extra, sampler, "host descriptor heap");
        // SAFETY: a live shader-visible heap.
        let gpu = unsafe { heap.GetGPUDescriptorHandleForHeapStart().ptr };
        GPU_RANGES.lock().unwrap_or_else(|e| e.into_inner()).push((
            gpu,
            u64::from(count + extra + guard) * u64::from(step),
            u64::from(step),
            sampler,
        ));
        let strides = if sampler {
            None
        } else {
            // Every slot a shader may index, the guard slot included.
            StrideTable::new(&host.device, count + extra + guard)
        };
        let h = HostHeap {
            heap,
            cpu,
            step,
            count,
            sampler,
            made: Mutex::new(vec![0; count as usize]),
            kept: Mutex::new(vec![None; (count + extra + guard) as usize]),
            dynamic: extra,
            next: Mutex::new((0, vec![false; extra as usize])),
            strides,
        };
        // A slot never written is undefined: shaders reading one (a slot
        // the title wrote in a way we do not know, or a table entry no
        // used-resource entry names) read a wild address. Every slot starts
        // null instead.
        let started = std::time::Instant::now();
        if let Err(e) = h.fill_null(kind, count + extra + guard) {
            tracing::warn!(error = %e, "host descriptor heap not made null");
        }
        tracing::debug!(
            ms = started.elapsed().as_millis(),
            "host descriptor heap made null"
        );
        Some(h)
    }

    /// Makes the first `total` slots null: one null slot in a CPU-side heap,
    /// doubled by copies to a block, the block copied over the heap.
    fn fill_null(&self, kind: D3D12_DESCRIPTOR_HEAP_TYPE, total: u32) -> windows::core::Result<()> {
        const BLOCK: u32 = 1 << 16;
        let Some(host) = host() else { return Ok(()) };
        // SAFETY: plain heap creation and copies inside live heaps.
        unsafe {
            let block: ID3D12DescriptorHeap =
                host.device
                    .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: kind,
                        NumDescriptors: BLOCK,
                        ..Default::default()
                    })?;
            let start = block.GetCPUDescriptorHandleForHeapStart();
            let at = |i: u32| D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: start.ptr + (i * self.step) as usize,
            };
            if self.sampler {
                null_sampler(start);
            } else {
                null_view(start);
            }
            let mut filled = 1;
            while filled < BLOCK {
                let n = filled.min(BLOCK - filled);
                host.device
                    .CopyDescriptorsSimple(n, at(filled), start, kind);
                filled += n;
            }
            let mut done = 0;
            while done < total {
                let n = BLOCK.min(total - done);
                host.device
                    .CopyDescriptorsSimple(n, self.cpu(done), start, kind);
                done += n;
            }
        }
        Ok(())
    }

    /// A plain point sampler at slot `index`, for sampler slots without a
    /// view.
    fn null_sampler(&self, index: u32) {
        null_sampler(self.cpu(index));
    }
}

/// A plain point sampler at `at`.
fn null_sampler(at: D3D12_CPU_DESCRIPTOR_HANDLE) {
    let Some(host) = host() else { return };
    let desc = D3D12_SAMPLER_DESC {
        Filter: D3D12_FILTER_MIN_MAG_MIP_POINT,
        AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
        ..Default::default()
    };
    // SAFETY: a slot of a sampler heap of ours.
    unsafe { host.device.CreateSampler(&desc, at) };
}

impl HostHeap {
    /// Whether this heap holds samplers rather than CBV/SRV/UAV views.
    pub(crate) fn is_sampler(&self) -> bool {
        self.sampler
    }

    /// Whether title slot `index` fits in the mirrored part of the host heap.
    pub(crate) fn in_range(&self, index: u32) -> bool {
        index < self.count
    }

    /// `n` consecutive free dynamic slots (view heaps only), taken until
    /// [`HostHeap::release`]: the first's index.
    pub(crate) fn dynamic(&self, n: u32) -> Option<u32> {
        if self.sampler || n == 0 || n > self.dynamic {
            return None;
        }
        let mut guard = self.next.lock().unwrap_or_else(|e| e.into_inner());
        let (next, taken) = &mut *guard;
        let cap = self.dynamic as usize;
        let n = n as usize;
        let Some(first) = free_table(taken, *next as usize, n) else {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                let (free, longest) = free_space(taken);
                tracing::warn!(
                    n,
                    cap = taken.len(),
                    free,
                    longest,
                    "no free dynamic descriptor slots: a table is not made"
                );
            });
            return None;
        };
        taken[first..first + n].fill(true);
        *next = ((first + n) % cap) as u32;
        Some(self.count + first as u32)
    }

    /// The title slots this heap has host slots for; fewer than the title's
    /// when the host caps its size.
    #[allow(dead_code)]
    pub(crate) fn count(&self) -> u32 {
        self.count
    }

    /// The slot translated shaders read in place of a computed heap index
    /// at or past it (a stale buffer can give any index, and one past the
    /// heap hangs some GPUs): in a view heap its last slot, after the
    /// dynamic ones, null from the start and never made; in a sampler heap
    /// its last slot (any sampler reads safely).
    pub(crate) fn guard_slot(&self) -> u32 {
        if self.sampler {
            self.count.saturating_sub(1)
        } else {
            self.count + self.dynamic
        }
    }

    /// Gives back `n` dynamic slots from `first` (an index [`dynamic`]
    /// returned).
    ///
    /// [`dynamic`]: HostHeap::dynamic
    pub(crate) fn release(&self, first: u32, n: u32) {
        let Some(at) = first.checked_sub(self.count) else {
            return;
        };
        let mut guard = self.next.lock().unwrap_or_else(|e| e.into_inner());
        let end = (at + n).min(self.dynamic) as usize;
        if let Some(slots) = guard.1.get_mut(at as usize..end) {
            slots.fill(false);
        }
    }

    /// Makes slot `index` (a dynamic one) from `view`; a null view if the
    /// host cannot.
    pub(crate) fn make_at(&self, index: u32, view: &View) {
        let kept = make(view, self.cpu(index));
        let ok = kept.is_some();
        if !ok {
            null_for(view, self.cpu(index));
        }
        self.keep(index, kept.flatten());
        self.set_slot(index, if ok { slot_words(view) } else { [0; SLOT_DWORDS as usize] });
    }

    /// Makes slot `index` read nothing: a null view, or a plain sampler.
    pub(crate) fn make_null(&self, index: u32) {
        if self.sampler {
            self.null_sampler(index);
        } else {
            null_view(self.cpu(index));
        }
        self.keep(index, None);
        self.set_slot(index, [0; SLOT_DWORDS as usize]);
    }

    /// Makes slot `index` a null view matching the resource kind a shader
    /// expects there (CBV, buffer SRV/UAV, or 2D texture).
    pub(crate) fn make_null_kind(&self, index: u32, kind: victory_shader::ResourceType, dwords: u8) {
        if self.sampler {
            self.null_sampler(index);
            self.keep(index, None);
            return;
        }
        let at = self.cpu(index);
        match kind {
            victory_shader::ResourceType::Cbv => {
                if let Some(host) = host() {
                    let desc = D3D12_CONSTANT_BUFFER_VIEW_DESC::default();
                    // SAFETY: a slot of our heap; zero location makes a null CBV.
                    unsafe { host.device.CreateConstantBufferView(Some(&desc), at) };
                }
            }
            victory_shader::ResourceType::Srv if dwords == 4 => null_buffer_view(at),
            victory_shader::ResourceType::Uav => {
                if let Some(host) = host() {
                    let desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
                        Format: DXGI_FORMAT_R32_UINT,
                        ViewDimension: if dwords == 4 {
                            D3D12_UAV_DIMENSION_BUFFER
                        } else {
                            D3D12_UAV_DIMENSION_TEXTURE2D
                        },
                        ..Default::default()
                    };
                    // SAFETY: a slot of our heap; null resource makes a null UAV.
                    unsafe {
                        host.device.CreateUnorderedAccessView(
                            None::<&ID3D12Resource>,
                            None::<&ID3D12Resource>,
                            Some(&desc),
                            at,
                        )
                    };
                }
            }
            _ => null_view(at),
        }
        self.keep(index, None);
    }

    /// Slot `index` now views `resource` (or nothing of the host's). A
    /// previously viewed resource is retired rather than dropped inline so an
    /// in-flight GPU submission reading that slot cannot fault on freed memory.
    fn keep(&self, index: u32, resource: Option<ID3D12Resource>) {
        let old = {
            let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
            kept.get_mut(index as usize)
                .and_then(|slot| std::mem::replace(slot, resource))
        };
        if let Some(old) = old {
            crate::host::retire(old);
        }
    }

    /// Slot `index`'s words in the table: its element stride and the
    /// NUM_RECORDS its console descriptor holds ([`slot_words`]).
    fn set_slot(&self, index: u32, words: [u32; SLOT_DWORDS as usize]) {
        if let Some(t) = &self.strides
            && index < t.len
        {
            // SAFETY: `t.ptr` points to a live mapped UPLOAD buffer of
            // `t.len` slots of two dwords.
            unsafe {
                let at = (t.ptr as *mut u32).add(index as usize * SLOT_DWORDS as usize);
                for (k, w) in words.into_iter().enumerate() {
                    at.add(k).write(w);
                }
            }
        }
    }

    /// GPU virtual address of the per-slot element byte stride table.
    pub(crate) fn stride_table_gpu(&self) -> Option<u64> {
        self.strides.as_ref().map(|t| t.gpu)
    }

    /// Makes slot `index` (a dynamic one) a constant buffer view of host
    /// memory (the list's own, which it holds).
    pub(crate) fn constant_at(&self, index: u32, gpu: u64, size: u32) {
        self.keep(index, None);
        self.set_slot(index, [0; SLOT_DWORDS as usize]);
        let Some(host) = host() else { return };
        let desc = D3D12_CONSTANT_BUFFER_VIEW_DESC {
            BufferLocation: gpu,
            SizeInBytes: size.next_multiple_of(256),
        };
        // SAFETY: a slot of our heap; the memory is the caller's.
        unsafe {
            host.device
                .CreateConstantBufferView(Some(&desc), self.cpu(index))
        };
    }

    fn cpu(&self, index: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: self.cpu + index as usize * self.step as usize,
        }
    }

    /// Makes host slot `index` from the record the title has at `handle`
    /// if it changed since, and returns the view recorded there. Buffer
    /// views are made only for draws (`for_draw`), which also copy their
    /// data: a host copy of a buffer is made on first use.
    pub(crate) fn mirror(&self, index: u32, handle: u64, for_draw: bool) -> Option<View> {
        if index >= self.count {
            // Diagnostic (NVIDIA heap cap): the highest title slot past the
            // host heap seen so far.
            static HIGHEST: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if HIGHEST.fetch_max(index, std::sync::atomic::Ordering::Relaxed) < index {
                tracing::warn!(
                    index,
                    count = self.count,
                    for_draw,
                    "title slot past the host heap"
                );
            }
            return None;
        }
        // SAFETY: the handle is a descriptor slot in title memory.
        let [tag, i, generation] = unsafe { (handle as *const [u32; 3]).read_unaligned() };
        // Generations start at 1, so a made slot's key is never 0.
        let key = u64::from(generation) << 32 | u64::from(i);
        let mut made = self.made.lock().unwrap_or_else(|e| e.into_inner());
        let view = (tag == VIEW_TAG).then(|| view_at(i, generation)).flatten();
        let Some(view) = view else {
            // Not a view of ours (never written, or of a resource the
            // title released): nothing to read.
            if made[index as usize] != 0 {
                made[index as usize] = 0;
                self.make_null(index);
            }
            return None;
        };
        if made[index as usize] != key {
            let later = !for_draw && view.is_buffer();
            made[index as usize] = if later { 0 } else { key };
            let kept = if later {
                None
            } else {
                make(&view, self.cpu(index))
            };
            let ok = kept.is_some();
            if !ok && !self.sampler {
                null_for(&view, self.cpu(index));
            }
            self.keep(index, kept.flatten());
            self.set_slot(index, if ok { slot_words(&view) } else { [0; SLOT_DWORDS as usize] });
        }
        Some(view)
    }

    /// The view host slot `index` was made from, in a few words, with the
    /// title and host addresses of a buffer view (diagnostics). "null" for
    /// a slot never made or left null.
    #[cfg(feature = "diagnostics")]
    pub(crate) fn describe_slot(&self, index: u32) -> Option<String> {
        let key = *self
            .made
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(index as usize)?;
        if key == 0 {
            return Some("null".into());
        }
        let view = view_at(key as u32, (key >> 32) as u32)?;
        let mut text: String = format!("{view:?}").chars().take(120).collect();
        if let Some((address, len)) = buffer_range(&view) {
            let host = crate::shadow::find(address, len.max(1)).map(|s| s.gpu(address));
            text.push_str(&format!(" title {address:#x}+{len:#x} host {host:#x?}"));
        }
        Some(text)
    }
}

/// Total free slots and the longest contiguous run in the physical heap.
fn free_space(taken: &[bool]) -> (usize, usize) {
    let mut free = 0;
    let mut longest = 0;
    for gap in taken.split(|&held| held) {
        free += gap.len();
        longest = longest.max(gap.len());
    }
    (free, longest)
}

/// Try each possible table start from the cursor, then from slot zero.
/// A table may cross the cursor on the second pass, but must fit in the heap.
fn free_table(taken: &[bool], cursor: usize, n: usize) -> Option<usize> {
    let end = taken.len().checked_sub(n)? + 1;
    let split = cursor.min(end);
    for starts in [split..end, 0..split] {
        let mut first = starts.start;
        while first < starts.end {
            match taken[first..first + n].iter().position(|&held| held) {
                Some(offset) => first += offset + 1,
                None => return Some(first),
            }
        }
    }
    None
}

/// Logs `what` the first time it happens.
macro_rules! once {
    ($($arg:tt)*) => {{
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::warn!($($arg)*));
    }};
}

/// A view description from its words, with the format as the host has it.
fn srv_desc(words: &[u32; 10]) -> D3D12_SHADER_RESOURCE_VIEW_DESC {
    let mut words = *words;
    words[0] = crate::resource::pc_format(words[0]);
    // SAFETY: 40 bytes in the host's layout.
    unsafe {
        std::ptr::from_ref(&words)
            .cast::<D3D12_SHADER_RESOURCE_VIEW_DESC>()
            .read_unaligned()
    }
}

fn uav_desc(words: &[u32; 10]) -> D3D12_UNORDERED_ACCESS_VIEW_DESC {
    let mut words = *words;
    words[0] = crate::resource::pc_format(words[0]);
    // SAFETY: 40 bytes in the host's layout.
    unsafe {
        std::ptr::from_ref(&words)
            .cast::<D3D12_UNORDERED_ACCESS_VIEW_DESC>()
            .read_unaligned()
    }
}

/// A buffer view of title memory at `address` on its host copy: the first
/// element moves by the copy's offset.
/// Bytes per element of a buffer view.
fn element_size(desc: &D3D12_SHADER_RESOURCE_VIEW_DESC) -> u64 {
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { desc.Anonymous.Buffer };
    if buffer.StructureByteStride > 0 {
        u64::from(buffer.StructureByteStride)
    } else if buffer.Flags.0 & D3D12_BUFFER_SRV_FLAG_RAW.0 != 0 {
        4
    } else {
        crate::resource::element(desc.Format.0 as u32).map_or(0, |(bytes, _)| bytes)
    }
}

/// The title memory a buffer shader or unordered access view reads: its
/// address and length.
pub(crate) fn buffer_range(view: &View) -> Option<(u64, u64)> {
    let (base, words): (u64, &[u32; 10]) = match view {
        View::Shader(r, Some(desc)) => (
            crate::resource::buffer_bytes(*r as crate::object::This)?.0 as u64,
            desc,
        ),
        View::Placed(false, resource, desc) if resource[0] == 1 => (
            u64::from(desc[10]) | u64::from(desc[11]) << 32,
            desc[..10].try_into().ok()?,
        ),
        View::Unordered(r, _, Some(desc)) => {
            let base = crate::resource::buffer_bytes(*r as crate::object::This)?.0 as u64;
            return unordered_range(base, desc);
        }
        View::Placed(true, resource, desc) if resource[0] == 1 => {
            let base = u64::from(desc[10]) | u64::from(desc[11]) << 32;
            return unordered_range(base, desc[..10].try_into().ok()?);
        }
        _ => return None,
    };
    let desc = srv_desc(words);
    let element = element_size(&desc);
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { desc.Anonymous.Buffer };
    Some((
        base + buffer.FirstElement * element,
        u64::from(buffer.NumElements) * element,
    ))
}

fn uav_element_size(desc: &D3D12_UNORDERED_ACCESS_VIEW_DESC) -> Option<u64> {
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { desc.Anonymous.Buffer };
    if buffer.StructureByteStride > 0 {
        Some(u64::from(buffer.StructureByteStride))
    } else if buffer.Flags.0 & D3D12_BUFFER_UAV_FLAG_RAW.0 != 0 {
        Some(4)
    } else {
        crate::resource::element(desc.Format.0 as u32).map(|(bytes, _)| bytes)
    }
}

/// Element byte stride of `view` for the runtime stride table (`0` for
/// non-buffer views or descriptors without a known element size).
fn view_stride(view: &View) -> u32 {
    let bytes = match view {
        View::Shader(_, Some(desc)) => element_size(&srv_desc(desc)),
        View::Placed(false, resource, desc) if resource[0] == 1 => desc[..10]
            .try_into()
            .ok()
            .map_or(0, |w: &[u32; 10]| element_size(&srv_desc(w))),
        View::Unordered(_, _, Some(desc)) => uav_element_size(&uav_desc(desc)).unwrap_or(0),
        View::Placed(true, resource, desc) if resource[0] == 1 => desc[..10]
            .try_into()
            .ok()
            .and_then(|w: &[u32; 10]| uav_element_size(&uav_desc(w)))
            .unwrap_or(0),
        _ => 0,
    };
    u32::try_from(bytes).unwrap_or(0)
}

/// Dwords per slot in the runtime table: the element stride
/// ([`view_stride`]), then the console descriptor's NUM_RECORDS
/// ([`view_records`]). The translator reads them at these offsets.
const SLOT_DWORDS: u64 = victory_shader::emit::SLOT_TABLE_DWORDS as u64;

/// A slot's words in the runtime table.
fn slot_words(view: &View) -> [u32; SLOT_DWORDS as usize] {
    let mut w = [0; SLOT_DWORDS as usize];
    w[..4].copy_from_slice(&[view_stride(view), view_records(view), view_dword1(view), 0]);
    let at = victory_shader::emit::SLOT_CONSOLE_WORDS as usize;
    w[at..at + 8].copy_from_slice(&console_words(view));
    w
}

/// The console descriptor (GFX10 V# or T#, eight dwords) the title would
/// hold for `view`, as far as the host view tells it: a buffer's title
/// address, stride field, NUM_RECORDS and identity channel order; an
/// image's title address (views of placed memory only) and its type.
/// Code that reads a descriptor's fields as data (XCL's model: console
/// descriptors are memory shaders read) gets these.
// shortcut: formats, tiling, sizes of images and the metadata words read
// 0; the translator rebuilds image sizes from the host view itself.
fn console_words(view: &View) -> [u32; 8] {
    /// DST_SEL x, y, z, w = X, Y, Z, W (dword 3 bits 0..11).
    const IDENTITY: u32 = 4 | 5 << 3 | 6 << 6 | 7 << 9;
    let buffer = |address: u64, records: u32, dword1: u32| {
        [
            address as u32,
            (address >> 32) as u32 & 0xFFFF | dword1 & 0x3FFF_0000,
            records,
            IDENTITY,
            0,
            0,
            0,
            0,
        ]
    };
    match view {
        View::Constant(address, size) => buffer(*address, *size, 0),
        View::Shader(..) | View::Unordered(..) | View::Placed(..)
            if view_records(view) != 0 =>
        {
            let address = buffer_range(view).map_or(0, |(a, _)| a);
            buffer(address, view_records(view), view_dword1(view))
        }
        View::Placed(_, _, desc) => {
            let address = u64::from(desc[10]) | u64::from(desc[11]) << 32;
            [
                (address >> 8) as u32,
                (address >> 40) as u32 & 0xFF,
                0,
                IDENTITY,
                0,
                0,
                0,
                0,
            ]
        }
        _ => [0; 8],
    }
}

/// Dword 1 of the buffer descriptor the console holds for `view`, its
/// stride field (bits 16..30) alone: the structure stride, a typed view's
/// element size, 0 for raw views (whose NUM_RECORDS counts bytes). The
/// address bits are the console's and mean nothing on the host.
fn view_dword1(view: &View) -> u32 {
    let raw = match view {
        View::Shader(_, Some(desc)) => {
            // SAFETY: a buffer view's union member.
            let b = unsafe { srv_desc(desc).Anonymous.Buffer };
            b.StructureByteStride == 0 && b.Flags.0 & D3D12_BUFFER_SRV_FLAG_RAW.0 != 0
        }
        View::Unordered(_, _, Some(desc)) => {
            // SAFETY: as above.
            let b = unsafe { uav_desc(desc).Anonymous.Buffer };
            b.StructureByteStride == 0 && b.Flags.0 & D3D12_BUFFER_UAV_FLAG_RAW.0 != 0
        }
        View::Placed(uav, resource, desc) if resource[0] == 1 => {
            let Ok(w) = <&[u32; 10]>::try_from(&desc[..10]) else {
                return 0;
            };
            if *uav {
                // SAFETY: as above.
                let b = unsafe { uav_desc(w).Anonymous.Buffer };
                b.StructureByteStride == 0 && b.Flags.0 & D3D12_BUFFER_UAV_FLAG_RAW.0 != 0
            } else {
                // SAFETY: as above.
                let b = unsafe { srv_desc(w).Anonymous.Buffer };
                b.StructureByteStride == 0 && b.Flags.0 & D3D12_BUFFER_SRV_FLAG_RAW.0 != 0
            }
        }
        _ => return 0,
    };
    if raw || view_records(view) == 0 {
        return 0;
    }
    (view_stride(view) & 0x3FFF) << 16
}

/// NUM_RECORDS (dword 2) of the buffer descriptor the console holds for
/// `view`: elements for structured and typed views, bytes for raw ones, as
/// `GetDimensions` of the host view gives them. 0 for other views.
fn view_records(view: &View) -> u32 {
    fn records(elements: u32, structured: bool, raw: bool) -> u32 {
        if !structured && raw {
            elements.saturating_mul(4)
        } else {
            elements
        }
    }
    let srv = |w: &[u32; 10]| {
        let d = srv_desc(w);
        if d.ViewDimension != D3D12_SRV_DIMENSION_BUFFER {
            return 0;
        }
        // SAFETY: a buffer view's union member.
        let b = unsafe { d.Anonymous.Buffer };
        records(
            b.NumElements,
            b.StructureByteStride > 0,
            b.Flags.0 & D3D12_BUFFER_SRV_FLAG_RAW.0 != 0,
        )
    };
    let uav = |w: &[u32; 10]| {
        let d = uav_desc(w);
        if d.ViewDimension != D3D12_UAV_DIMENSION_BUFFER {
            return 0;
        }
        // SAFETY: a buffer view's union member.
        let b = unsafe { d.Anonymous.Buffer };
        records(
            b.NumElements,
            b.StructureByteStride > 0,
            b.Flags.0 & D3D12_BUFFER_UAV_FLAG_RAW.0 != 0,
        )
    };
    match view {
        View::Shader(_, Some(desc)) => srv(desc),
        View::Placed(false, resource, desc) if resource[0] == 1 => {
            desc[..10].try_into().map_or(0, |w: &[u32; 10]| srv(w))
        }
        View::Unordered(_, _, Some(desc)) => uav(desc),
        View::Placed(true, resource, desc) if resource[0] == 1 => {
            desc[..10].try_into().map_or(0, |w: &[u32; 10]| uav(w))
        }
        _ => 0,
    }
}

/// [`buffer_range`] of an unordered access view of memory at `base`.
fn unordered_range(base: u64, words: &[u32; 10]) -> Option<(u64, u64)> {
    let desc = uav_desc(words);
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { desc.Anonymous.Buffer };
    let element = uav_element_size(&desc)?;
    Some((
        base + buffer.FirstElement * element,
        u64::from(buffer.NumElements) * element,
    ))
}

/// The host resource a slot made by [`make`] views, if any.
type Kept = Option<ID3D12Resource>;

fn buffer_srv(
    address: u64,
    len: u64,
    mut desc: D3D12_SHADER_RESOURCE_VIEW_DESC,
    at: D3D12_CPU_DESCRIPTOR_HANDLE,
) -> Option<Kept> {
    let shadow = crate::shadow::find(address, len)?;
    let element = element_size(&desc);
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { &mut desc.Anonymous.Buffer };
    let offset = shadow.offset(address);
    if element == 0 || offset % element != 0 {
        once!(
            offset,
            element,
            "buffer view not on an element of its host copy"
        );
        return None;
    }
    buffer.FirstElement += offset / element;
    let host = host()?;
    // SAFETY: a live buffer and a slot of our heap.
    unsafe {
        host.device
            .CreateShaderResourceView(&shadow.buffer, Some(&desc), at)
    };
    Some(Some(shadow.buffer.clone()))
}

/// An unordered access view of title buffer memory at `address` (`len`
/// bytes) on its host copy, as [`buffer_srv`]; false if the copy cannot be
/// written by shaders (upload memory).
/// shortcut: what shaders write stays in the host copy; title memory (and
/// so the CPU, and copies run on the CPU) does not see it.
fn buffer_uav(
    address: u64,
    len: u64,
    mut desc: D3D12_UNORDERED_ACCESS_VIEW_DESC,
    at: D3D12_CPU_DESCRIPTOR_HANDLE,
) -> Option<Kept> {
    let shadow = crate::shadow::find(address, len)?;
    // SAFETY: a live buffer.
    let flags = unsafe { shadow.buffer.GetDesc() }.Flags;
    if flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 == 0 {
        once!("unordered access to a buffer whose host copy shaders cannot write");
        return None;
    }
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { &mut desc.Anonymous.Buffer };
    let element = if buffer.StructureByteStride > 0 {
        u64::from(buffer.StructureByteStride)
    } else if buffer.Flags.0 & D3D12_BUFFER_UAV_FLAG_RAW.0 != 0 {
        4
    } else {
        crate::resource::element(desc.Format.0 as u32).map_or(0, |(bytes, _)| bytes)
    };
    let offset = shadow.offset(address);
    if element == 0 || offset % element != 0 {
        once!(
            offset,
            element,
            "unordered buffer view not on an element of its host copy"
        );
        return None;
    }
    buffer.FirstElement += offset / element;
    buffer.CounterOffsetInBytes = 0;
    let host = host()?;
    // SAFETY: a live buffer and a slot of our heap.
    unsafe {
        host.device.CreateUnorderedAccessView(
            &shadow.buffer,
            None::<&ID3D12Resource>,
            Some(&desc),
            at,
        )
    };
    Some(Some(shadow.buffer.clone()))
}

/// Whether the host can make a shader (or unordered access) view of
/// `texture` in `format` (none: the texture's own). The console allows any
/// view; the host refuses (and removes the device for) views without the
/// resource's permission, of typed depth, and in another format than a
/// typed resource's. A view of a typed texture in another format of its
/// family takes the texture's format.
/// shortcut: such a view (sRGB of UNORM, say) reads without its
/// conversion; cast with castable formats if colors come out wrong.
fn fits(texture: &ID3D12Resource, mut format: Option<&mut DXGI_FORMAT>, unordered: bool) -> bool {
    use crate::resource::{DEPTH_FORMATS, family};
    // SAFETY: a live resource.
    let d = unsafe { texture.GetDesc() };
    let allowed = if unordered {
        d.Flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 != 0
    } else {
        d.Flags.0 & D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE.0 == 0
    };
    let own = d.Format.0 as u32;
    if !allowed || DEPTH_FORMATS.iter().any(|&(typed, _)| typed == own) {
        once!(
            unordered,
            format = own,
            flags = d.Flags.0,
            "view the host does not allow"
        );
        return false;
    }
    // Every family has more than one format, its typeless one first.
    let typeless = family(own) == own && family(own + 1) == own;
    if let Some(format) = format.as_deref_mut()
        && unordered
        && let Some(&(_, linear)) = crate::resource::SRGB
            .iter()
            .find(|&&(s, _)| s == format.0 as u32)
    {
        format.0 = linear as i32;
    }
    let view = format.as_ref().map_or(0, |f| f.0 as u32);
    if view == 0 {
        return !typeless;
    }
    // A texture whose console format PC forbids as a render target or UAV
    // was substituted (`resource::target_format`): views name its host format.
    if view != own && crate::resource::target_format(view) == own {
        if let Some(format) = format {
            *format = d.Format;
        }
        return true;
    }
    if family(view) != family(own) {
        once!(view, own, "view format of another family");
        return false;
    }
    if let Some(format) = format
        && !typeless
    {
        *format = d.Format;
    }
    true
}

/// Makes the host descriptor for `view` at `at`: the host resource it
/// views, or `None` if it cannot.
fn make(view: &View, at: D3D12_CPU_DESCRIPTOR_HANDLE) -> Option<Kept> {
    let host = host()?;
    let device = &host.device;
    match view {
        &View::Constant(address, size) => {
            let shadow = crate::shadow::find(address, u64::from(size))?;
            let gpu = shadow.gpu(address);
            if gpu % 256 != 0 {
                once!(
                    address = format_args!("{address:#x}"),
                    "constant buffer not 256-byte aligned on the host"
                );
                return None;
            }
            let desc = D3D12_CONSTANT_BUFFER_VIEW_DESC {
                BufferLocation: gpu,
                SizeInBytes: size.next_multiple_of(256),
            };
            // SAFETY: a slot of our heap; the range lies in the host copy.
            unsafe { device.CreateConstantBufferView(Some(&desc), at) };
            Some(Some(shadow.buffer.clone()))
        }
        View::Shader(resource, desc) => {
            let resource = *resource as crate::object::This;
            if let Some(texture) = crate::resource::host_resource(resource) {
                let mut desc = desc.as_ref().map(srv_desc);
                if !fits(texture, desc.as_mut().map(|d| &mut d.Format), false) {
                    return None;
                }
                // SAFETY: a live texture and a slot of our heap.
                unsafe {
                    device.CreateShaderResourceView(
                        texture,
                        desc.as_ref().map(std::ptr::from_ref),
                        at,
                    )
                };
                Some(Some(texture.clone()))
            } else if let (Some((data, len)), Some(desc)) =
                (crate::resource::buffer_bytes(resource), desc)
            {
                buffer_srv(data as u64, len, srv_desc(desc), at)
            } else {
                None
            }
        }
        View::Unordered(resource, _counter, desc) => {
            let resource = *resource as crate::object::This;
            let Some(texture) = crate::resource::host_resource(resource) else {
                return match (crate::resource::buffer_bytes(resource), desc) {
                    (Some((data, len)), Some(desc)) => {
                        buffer_uav(data as u64, len, uav_desc(desc), at)
                    }
                    _ => None,
                };
            };
            let mut desc = desc.as_ref().map(uav_desc);
            if !fits(texture, desc.as_mut().map(|d| &mut d.Format), true) {
                return None;
            }
            // SAFETY: a live texture and a slot of our heap.
            unsafe {
                device.CreateUnorderedAccessView(
                    texture,
                    None::<&ID3D12Resource>,
                    desc.as_ref().map(std::ptr::from_ref),
                    at,
                )
            };
            Some(Some(texture.clone()))
        }
        View::Sampler(words) => {
            // SAFETY: 13 words in the host's layout.
            let desc = unsafe {
                std::ptr::from_ref(words)
                    .cast::<D3D12_SAMPLER_DESC>()
                    .read_unaligned()
            };
            // SAFETY: a slot of our sampler heap.
            unsafe { device.CreateSampler(&desc, at) };
            Some(None)
        }
        View::ConsoleSampler(words) => {
            let desc = crate::sampler::console(words);
            // SAFETY: a slot of our sampler heap.
            unsafe { device.CreateSampler(&desc, at) };
            Some(None)
        }
        View::Placed(unordered, resource, desc) => {
            if let Some(view) = placed_texture(view) {
                return make(&view, at);
            }
            const BUFFER: u32 = 1;
            if resource[0] != BUFFER {
                // shortcut: unordered access to a texture the title placed
                // itself is not mirrored; give it a resource as shader
                // views of such textures get when a dispatch writes one.
                once!(
                    dimension = resource[0],
                    "placed unordered texture view not mirrored"
                );
                return None;
            }
            // The resource description's width, after the alignment.
            let len = u64::from(resource[4]) | u64::from(resource[5]) << 32;
            let address = u64::from(desc[10]) | u64::from(desc[11]) << 32;
            let words: &[u32; 10] = desc[..10].try_into().unwrap_or(&[0; 10]);
            if *unordered {
                buffer_uav(address, len, uav_desc(words), at)
            } else {
                buffer_srv(address, len, srv_desc(words), at)
            }
        }
    }
}

/// A shader view of a texture the title placed in memory itself, as a view
/// of the resource made for that memory (on first use).
pub(crate) fn placed_texture(view: &View) -> Option<View> {
    const BUFFER: u32 = 1;
    let View::Placed(false, resource, desc) = view else {
        return None;
    };
    if resource[0] == BUFFER {
        return None;
    }
    let address = u64::from(desc[10]) | u64::from(desc[11]) << 32;
    let texture = crate::resource::texture_at(address, resource)?;
    Some(View::Shader(texture as usize, desc[..10].try_into().ok()))
}

/// A null view of the kind `view` is, for a slot whose view the host
/// cannot make: unordered access (to a buffer or a 2D texture) or a shader
/// view. Writes to it are dropped, reads give zeros.
fn null_for(view: &View, at: D3D12_CPU_DESCRIPTOR_HANDLE) {
    let Some(host) = host() else { return };
    let is_buffer = |r: &usize| crate::resource::buffer_bytes(*r as crate::object::This).is_some();
    let buffer = match view {
        View::Unordered(r, _, desc) => is_buffer(r) || desc.is_some_and(|d| d[1] == 1),
        View::Placed(true, resource, _) => resource[0] == 1,
        // Shader views of buffers read nothing through a null buffer view:
        // a null 2D view where the shader expects a buffer is a mismatch
        // the hardware does not survive in general (GPU-based validation
        // reports it).
        View::Shader(r, desc) if is_buffer(r) || desc.is_some_and(|d| d[1] == 1) => {
            return null_buffer_view(at);
        }
        View::Placed(false, resource, _) if resource[0] == 1 => return null_buffer_view(at),
        // Textures: a null view of the title's dimension (1D, arrays, 3D,
        // cubes), not always 2D.
        View::Shader(_, Some(desc)) => return null_like(desc, at),
        View::Placed(false, _, desc) => {
            return null_like(desc[..10].try_into().unwrap_or(&[0; 10]), at);
        }
        View::Constant(..) => {
            let desc = D3D12_CONSTANT_BUFFER_VIEW_DESC::default();
            // SAFETY: a slot of our heap; a zero location makes a null view.
            return unsafe { host.device.CreateConstantBufferView(Some(&desc), at) };
        }
        _ => return null_view(at),
    };
    let desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
        Format: DXGI_FORMAT_R32_UINT,
        ViewDimension: if buffer {
            D3D12_UAV_DIMENSION_BUFFER
        } else {
            D3D12_UAV_DIMENSION_TEXTURE2D
        },
        ..Default::default()
    };
    // SAFETY: a slot of our heap; a null resource is allowed with a desc.
    unsafe {
        host.device.CreateUnorderedAccessView(
            None::<&ID3D12Resource>,
            None::<&ID3D12Resource>,
            Some(&desc),
            at,
        )
    };
}

/// A buffer view that reads zeros (raw, as translated shaders mostly read
/// buffers).
fn null_buffer_view(at: D3D12_CPU_DESCRIPTOR_HANDLE) {
    let Some(host) = host() else { return };
    let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R32_TYPELESS,
        ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Buffer: D3D12_BUFFER_SRV {
                NumElements: 1,
                Flags: D3D12_BUFFER_SRV_FLAG_RAW,
                ..Default::default()
            },
        },
    };
    // SAFETY: a slot of our heap; a null resource is allowed with a desc.
    unsafe {
        host.device
            .CreateShaderResourceView(None::<&ID3D12Resource>, Some(&desc), at)
    };
}

/// A texture view that reads zeros with the dimension (and mip and array
/// counts) of the title's description `words`; 2D if it names none.
fn null_like(words: &[u32; 10], at: D3D12_CPU_DESCRIPTOR_HANDLE) {
    const TEXTURES: std::ops::RangeInclusive<u32> = 2..=10;
    if !TEXTURES.contains(&words[1]) {
        return null_view(at);
    }
    let Some(host) = host() else { return };
    // One mip, one slice or cube, plane 0: fields a null view still checks.
    let mut w = [0u32; 10];
    w[1] = words[1];
    match words[1] {
        6 => {}
        7 => w[5] = 1,
        3 | 5 | 10 => (w[5], w[7]) = (1, 1),
        _ => w[5] = 1,
    }
    let mut desc = srv_desc(&w);
    desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    desc.Shader4ComponentMapping = D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING;
    // SAFETY: a slot of our heap; a null resource is allowed with a desc.
    unsafe {
        host.device
            .CreateShaderResourceView(None::<&ID3D12Resource>, Some(&desc), at)
    };
}

/// A view that reads zeros, for slots whose view the host cannot make.
/// shortcut: slots with no view at all (kind unknown) get this null 2D
/// view; null buffer views there did not stop the world's low-address
/// page faults (2026-11-24, run w10), so they are not the cause.
fn null_view(at: D3D12_CPU_DESCRIPTOR_HANDLE) {
    let Some(host) = host() else { return };
    let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D12_TEX2D_SRV {
                MipLevels: 1,
                ..Default::default()
            },
        },
    };
    // SAFETY: a slot of our heap; a null resource is allowed with a desc.
    unsafe {
        host.device
            .CreateShaderResourceView(None::<&ID3D12Resource>, Some(&desc), at)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_counts_separate_runs_at_the_heap_ends() {
        assert_eq!(free_space(&[true; 8]), (0, 0));
        assert_eq!(free_space(&[false; 8]), (8, 8));
        let fragmented = [false, false, true, false, true, false, false, false];
        assert_eq!(free_space(&fragmented), (6, 3));
    }

    #[test]
    fn free_table_chooses_the_first_contiguous_fit_in_cursor_order() {
        for mask in 0u16..256 {
            let taken: [bool; 8] = std::array::from_fn(|slot| mask & (1 << slot) != 0);
            for cursor in 0..taken.len() {
                for n in 1..=taken.len() {
                    let expected = (cursor..taken.len()).chain(0..cursor).find(|&start| {
                        taken
                            .get(start..start + n)
                            .is_some_and(|table| table.iter().all(|&held| !held))
                    });
                    assert_eq!(
                        free_table(&taken, cursor, n),
                        expected,
                        "{mask:#x} {cursor} {n}"
                    );
                }
            }
        }
    }

    /// Dynamic slots stay taken until given back, and runs do not wrap.
    #[test]
    fn dynamic_slots_are_held_until_released() {
        let Some(heap) = HostHeap::new(false, 4) else {
            return; // no host GPU
        };
        let a = heap.dynamic(DYNAMIC - 100).unwrap();
        let b = heap.dynamic(60).unwrap();
        assert_eq!((a, b), (4, 4 + DYNAMIC - 100));
        // 40 left at the end: 50 do not fit until something is given back.
        assert_eq!(heap.dynamic(50), None);
        heap.release(a, 50);
        assert_eq!(heap.dynamic(50), Some(4));
        assert_eq!(heap.dynamic(40), Some(4 + DYNAMIC - 40));
    }

    #[test]
    fn a_free_table_can_straddle_the_search_cursor() {
        let Some(heap) = HostHeap::new(false, 4) else {
            return;
        };
        let left = heap.dynamic(4).unwrap();
        let right = heap.dynamic(5).unwrap();
        let tail = heap.dynamic(DYNAMIC - 9).unwrap();

        heap.release(left, 4);
        assert_eq!(heap.dynamic(4), Some(left)); // cursor now separates the two tables
        heap.release(right, 5);
        heap.release(left, 4);
        assert_eq!(heap.dynamic(9), Some(left));
        assert_eq!(heap.dynamic(1), None, "the tail is still held");

        heap.release(tail, DYNAMIC - 9);
        assert_eq!(heap.dynamic(DYNAMIC - 9), Some(tail));
    }
}
