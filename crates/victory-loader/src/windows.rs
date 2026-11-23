//! Loading a prepared image as a real Windows module and binding it.

use std::ffi::{CString, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

use anyhow::{Context, Result, bail};
use victory_pe::{ImageInfo, ImportSymbol};
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW};
use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};

use crate::thunks::{ImportSite, ThunkBlock, UnimplementedPolicy};
use crate::{DLL_PROCESS_ATTACH, ImportResolver, PreparedImage, Provenance, Resolution};

/// A title image mapped into this process.
#[derive(Debug)]
pub struct LoadedImage {
    module: HMODULE,
    path: PathBuf,
    info: ImageInfo,
}

// SAFETY: HMODULE is a process-wide handle (the base address); using it from
// any thread is fine.
unsafe impl Send for LoadedImage {}
// SAFETY: as above; `LoadedImage` has no interior mutability.
unsafe impl Sync for LoadedImage {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingImport {
    pub dll: String,
    pub symbol: String,
}

/// Outcome of [`LoadedImage::bind_imports`].
#[derive(Debug, Default, Clone)]
pub struct BindReport {
    pub hle: usize,
    pub host: usize,
    pub missing: Vec<MissingImport>,
}

/// Writes the prepared copy into `dir` and maps it with the Windows loader.
///
/// `dir` receives derived game data, so it must be outside the repository.
pub fn load(prepared: &PreparedImage, dir: &Path) -> Result<LoadedImage> {
    victory_core::ensure_outside_repo(dir)?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(prepared.file_name());
    write_if_changed(&path, &prepared.bytes)?;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated path. The image has no entry point
    // and no imports, so loading it runs no title code.
    let module = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if module.is_null() {
        bail!(
            "LoadLibraryExW({}) failed: {}{}",
            path.display(),
            std::io::Error::last_os_error(),
            if prepared.info.has_relocations {
                ""
            } else {
                " (image has no relocations; its preferred base may be taken)"
            }
        );
    }
    let loaded = LoadedImage {
        module,
        path,
        info: prepared.info.clone(),
    };
    tracing::info!(
        base = format_args!("{:#x}", loaded.base()),
        preferred = format_args!("{:#x}", loaded.info.image_base),
        path = %loaded.path.display(),
        "image mapped"
    );
    Ok(loaded)
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    if std::fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

impl LoadedImage {
    pub fn base(&self) -> usize {
        self.module as usize
    }

    /// Parsed from the original image (imports, entry point, TLS callbacks).
    pub fn info(&self) -> &ImageInfo {
        &self.info
    }

    /// Address of an export of the image, if it has one by that name.
    pub fn export(&self, name: &str) -> Option<usize> {
        let name = CString::new(name).ok()?;
        // SAFETY: valid module handle and NUL-terminated name.
        unsafe { GetProcAddress(self.module, name.as_ptr().cast()) }
            .map(|f| f as *const () as usize)
    }

    /// Fills every IAT slot of the image.
    ///
    /// Each import goes to `resolver`; whatever it cannot resolve gets an
    /// unimplemented-import stub that behaves according to `policy`.
    pub fn bind_imports(
        &self,
        resolver: &dyn ImportResolver,
        policy: UnimplementedPolicy,
    ) -> Result<BindReport> {
        let mut report = BindReport::default();
        let mut slots: Vec<(u32, usize)> = Vec::with_capacity(self.info.import_count());
        let mut unresolved: Vec<(u32, &str, &ImportSymbol)> = Vec::new();

        // Delay-load slots are bound eagerly too, so the title's own
        // delay-load helper (which would ask the host) never runs.
        for module in self.info.imports.iter().chain(&self.info.delay_imports) {
            for entry in &module.entries {
                match resolver.resolve(&module.dll, &entry.symbol) {
                    Resolution::Found {
                        address,
                        provenance,
                    } => {
                        match provenance {
                            Provenance::Hle => report.hle += 1,
                            Provenance::Host => report.host += 1,
                        }
                        slots.push((entry.iat_slot_rva, address));
                    }
                    Resolution::Missing => {
                        unresolved.push((entry.iat_slot_rva, &module.dll, &entry.symbol));
                    }
                }
            }
        }

        if !unresolved.is_empty() {
            let mut block = ThunkBlock::new(unresolved.len())?;
            for (slot, dll, symbol) in unresolved {
                let site: &'static ImportSite = Box::leak(Box::new(ImportSite {
                    dll: dll.to_owned(),
                    symbol: symbol.to_string(),
                    policy,
                    calls: AtomicU64::new(0),
                }));
                slots.push((slot, block.emit(site)?));
                report.missing.push(MissingImport {
                    dll: site.dll.clone(),
                    symbol: site.symbol.clone(),
                });
            }
            block.seal()?;
        }

        self.write_slots(&slots)?;
        tracing::info!(
            hle = report.hle,
            host = report.host,
            missing = report.missing.len(),
            "imports bound"
        );
        Ok(report)
    }

    /// Protects and restores each slot on its own: import tables can sit in
    /// sections with different protections (delay-load tables in `.didat`).
    fn write_slots(&self, slots: &[(u32, usize)]) -> Result<()> {
        for &(rva, address) in slots {
            let slot = (self.base() + rva as usize) as *mut u64;
            let mut old = 0;
            // SAFETY: the parser only yields slots inside the image.
            if unsafe { VirtualProtect(slot as *const c_void, 8, PAGE_READWRITE, &mut old) } == 0 {
                bail!(
                    "VirtualProtect(IAT slot {rva:#x}) failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            // SAFETY: inside the image, now writable, 8-byte aligned by the PE format.
            unsafe { slot.write_volatile(address as u64) };
            let mut ignored = 0;
            // SAFETY: restoring the protection we just changed.
            unsafe { VirtualProtect(slot as *const c_void, 8, old, &mut ignored) };
        }
        Ok(())
    }

    /// Calls the image's TLS callbacks with `reason` (e.g. `DLL_PROCESS_ATTACH`).
    ///
    /// # Safety
    /// Runs title code. Imports must be bound first.
    pub unsafe fn run_tls_callbacks(&self, reason: u32) {
        let Some(tls) = &self.info.tls else { return };
        for &rva in &tls.callback_rvas {
            type TlsCallback = unsafe extern "system" fn(*mut c_void, u32, *mut c_void);
            // SAFETY: the RVA came from the image's own TLS directory.
            let callback: TlsCallback = unsafe { std::mem::transmute(self.base() + rva as usize) };
            // SAFETY: the caller guarantees imports are bound.
            unsafe { callback(self.module, reason, std::ptr::null_mut()) };
        }
    }

    /// Starts the title: on a new thread with the stack size from the image
    /// header, runs the TLS callbacks (`DLL_PROCESS_ATTACH`), then the
    /// original entry point. Returns the thread so the caller can join it.
    ///
    /// # Safety
    /// Runs title code. Imports must be bound first.
    pub unsafe fn spawn_main(self: std::sync::Arc<Self>) -> Result<std::thread::JoinHandle<u32>> {
        if self.info.entry_rva == 0 {
            bail!("image has no entry point");
        }
        let stack = usize::try_from(self.info.stack_reserve)
            .unwrap_or(usize::MAX)
            .max(1 << 20);
        std::thread::Builder::new()
            .name("title-main".into())
            .stack_size(stack)
            .spawn(move || {
                // SAFETY: forwarded from the caller of `spawn_main`.
                unsafe { self.run_tls_callbacks(DLL_PROCESS_ATTACH) };
                type Entry = unsafe extern "system" fn(*mut c_void) -> u32;
                // SAFETY: the entry RVA came from the original image header.
                let entry: Entry =
                    unsafe { std::mem::transmute(self.base() + self.info.entry_rva as usize) };
                // SAFETY: forwarded from the caller of `spawn_main`.
                unsafe { entry(std::ptr::null_mut()) }
            })
            .context("spawning title main thread")
    }

    /// Unmaps the image.
    ///
    /// # Safety
    /// No title code may be running or run later, and no pointer into the
    /// image may be used afterwards.
    pub unsafe fn unload(self) {
        // SAFETY: guaranteed by the caller.
        unsafe { FreeLibrary(self.module) };
    }
}
