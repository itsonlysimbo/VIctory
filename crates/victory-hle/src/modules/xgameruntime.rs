//! `xgameruntime.dll`: the console game runtime (XGame*, XUser*, XTaskQueue*...).
//!
//! The DLL names titles actually import these from are confirmed by running
//! `victory census` on real titles; extend `dlls` accordingly.
#![allow(non_snake_case)]

use crate::{HleModule, hle_export};

pub type HRESULT = i32;
pub const S_OK: HRESULT = 0;

pub extern "system" fn XGameRuntimeInitialize() -> HRESULT {
    tracing::info!("XGameRuntimeInitialize");
    S_OK
}

pub extern "system" fn XGameRuntimeUninitialize() {
    tracing::info!("XGameRuntimeUninitialize");
}

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "xgameruntime",
        dlls: &["xgameruntime.dll"],
        exports: vec![
            hle_export!(XGameRuntimeInitialize),
            hle_export!(XGameRuntimeUninitialize),
        ],
    }
}
