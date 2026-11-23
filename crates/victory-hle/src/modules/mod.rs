//! Implementations, one module per area of the console API.
//!
//! Adding an API: write an `extern "system"` function named exactly like
//! the import, then list it with `hle_export!` in the module's `module()`.
//! Run `victory census` over your titles to see what is still missing.
//!
//! Planned areas (driven by census results, not by completeness):
//! `file` (package paths), `thread` (TLS callbacks for title threads),
//! `xaudio2`, `xstore` stubs.

pub mod audio;
pub mod d3d12x;
#[cfg(windows)]
pub mod files;
pub mod gameinput;
pub mod kernel;
#[cfg(windows)]
pub mod lifecycle;
pub mod storage;
pub mod xasync;
pub mod xgame;
pub mod xgameruntime;
#[cfg(windows)]
pub mod xmem;
pub mod xsystem;
pub mod xuser;

use crate::HleModule;

pub(crate) fn all() -> Vec<HleModule> {
    let mut all = vec![
        kernel::module(),
        xgameruntime::module(),
        gameinput::module(),
        d3d12x::module(),
        audio::module(),
    ];
    #[cfg(windows)]
    all.push(files::module());
    #[cfg(windows)]
    all.push(lifecycle::module());
    #[cfg(windows)]
    all.push(xmem::module());
    all
}
