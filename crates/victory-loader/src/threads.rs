//! Thread notices for title images. The prepared copies have no entry point
//! and no TLS callbacks, so the Windows loader tells them nothing when a
//! thread starts or ends. Our own TLS callback, which Windows calls on every
//! thread of the process whoever created it (title, host CRT or our
//! runtime), passes each notice on to every attached image: its TLS
//! callbacks, then a DLL's `DllMain`.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use crate::LoadedImage;

const DLL_THREAD_ATTACH: u32 = 2;
const DLL_THREAD_DETACH: u32 = 3;

struct Mapped {
    image: Arc<LoadedImage>,
    /// Its `DLL_PROCESS_ATTACH` is done; only then does it get thread notices.
    attached: bool,
    /// `DisableThreadLibraryCalls`: no more `DllMain` thread notices.
    quiet: bool,
}

/// Every title image we mapped, in load order.
static IMAGES: Mutex<Vec<Mapped>> = Mutex::new(Vec::new());

fn images() -> std::sync::MutexGuard<'static, Vec<Mapped>> {
    IMAGES.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn mapped(image: &LoadedImage) {
    // SAFETY: reads our own static, so the linker keeps it (and its section).
    unsafe { std::ptr::from_ref(&ON_THREAD).read_volatile() };
    let image = Arc::new(image.clone());
    let mut images = images();
    images.retain(|m| m.image.base() != image.base());
    images.push(Mapped {
        image,
        attached: false,
        quiet: false,
    });
}

pub(crate) fn attached(base: usize) {
    if let Some(m) = images().iter_mut().find(|m| m.image.base() == base) {
        m.attached = true;
    }
}

pub(crate) fn unmapped(base: usize) {
    images().retain(|m| m.image.base() != base);
}

/// `DisableThreadLibraryCalls` for a title image; `None` if `base` is not
/// one. Fails, as on Windows, for an image with static TLS.
pub fn disable_thread_calls(base: usize) -> Option<bool> {
    let mut images = images();
    let m = images.iter_mut().find(|m| m.image.base() == base)?;
    if m.image.info().tls.is_some() {
        return Some(false);
    }
    m.quiet = true;
    Some(true)
}

/// Windows calls every pointer between the CRT's markers `.CRT$XLA` and
/// `.CRT$XLZ`; `XLAV` sorts ahead of the usual `XLB`, so ours runs before
/// any other callback linked into the exe.
#[used]
#[unsafe(link_section = ".CRT$XLAV")]
static ON_THREAD: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) = on_thread;

unsafe extern "system" fn on_thread(_: *mut c_void, reason: u32, _: *mut c_void) {
    if reason != DLL_THREAD_ATTACH && reason != DLL_THREAD_DETACH {
        return;
    }
    // A snapshot: title code may map and attach another DLL from here.
    let notify: Vec<_> = images()
        .iter()
        .filter(|m| m.attached)
        .map(|m| (m.image.clone(), !m.quiet))
        .collect();
    // Starts in load order, ends in reverse, as the Windows loader does.
    let notify_one = |(image, dll_main): &(Arc<LoadedImage>, bool)| {
        // SAFETY: attached images have their imports bound.
        unsafe { image.notify(reason, *dll_main) };
    };
    if reason == DLL_THREAD_ATTACH {
        notify.iter().for_each(notify_one);
    } else {
        notify.iter().rev().for_each(notify_one);
    }
}
