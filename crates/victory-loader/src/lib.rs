//! Runs a title's original x86-64 code natively, inside our process.
//!
//! Xbox Series X|S executables are x86-64 PE32+ images built for a Zen 2
//! CPU, so the machine code needs no translation. What differs is the
//! platform underneath, so the loader's job is to:
//!
//! 1. [`prepare`] a lightly patched copy of the image (pure, portable): mark
//!    it as a DLL, remove its entry point and import directory, and detach
//!    its TLS callbacks, so the Windows loader maps it, relocates it,
//!    allocates its static TLS and registers its unwind tables, but runs
//!    none of its code and resolves none of its imports;
//! 2. load that copy as a real module (debuggers, profilers, crash dumps
//!    and stack walks all see it); this step is Windows-only;
//! 3. bind every import ourselves through an [`ImportResolver`], our
//!    runtime (`victory-hle`) or the host, and route anything unresolved
//!    to a generated stub that reports exactly what was called;
//! 4. run TLS callbacks and the entry point on a thread with the stack size
//!    the image asks for.

pub mod prepare;
pub mod resolve;
#[cfg(windows)]
mod thunks;
#[cfg(windows)]
mod windows;

pub use prepare::{PrepareError, PreparedImage, prepare};
pub use resolve::{ImportResolver, Provenance, Resolution};
#[cfg(windows)]
pub use thunks::{UnimplementedPolicy, unimplemented_call_count};
#[cfg(windows)]
pub use windows::{BindReport, LoadedImage, MissingImport, load};

/// `DLL_PROCESS_ATTACH`, for TLS callbacks.
pub const DLL_PROCESS_ATTACH: u32 = 1;
