//! Implementations, one module per area of the console API.
//!
//! Adding an API: write an `extern "system"` function named exactly like
//! the import, then list it with `hle_export!` in the module's `module()`.
//! Run `victory census` over your titles to see what is still missing.
//!
//! Planned areas (driven by census results, not by completeness):
//! `xmem` (console memory types, GPU-visible allocations), `file` (package
//! paths), `thread` (TLS callbacks for title threads), `d3d12x` (bridge to
//! `victory-gpu`), `xaudio2`, `gameinput`, `xuser`/`xstore` stubs.

pub mod kernel;
pub mod xgameruntime;

use crate::HleModule;

pub(crate) fn all() -> Vec<HleModule> {
    vec![kernel::module(), xgameruntime::module()]
}
