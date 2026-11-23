//! D3D12.X on host D3D12.
//!
//! D3D12.X records many commands by writing command-processor packets into
//! command lists from code inlined into the title itself, so no import-level
//! hook sees them. This crate decodes those packet streams at
//! `ExecuteCommandLists` time and replays them on host D3D12, and maps the
//! title's GPU virtual addresses (console memory is unified) to host
//! resources.
//!
//! Planned modules: `packets` (stream decoder), `memory` (title GPU
//! addresses to host heaps), `pipeline` (PSOs from `victory-shader` output),
//! `present` (swap chain, frame pacing).
