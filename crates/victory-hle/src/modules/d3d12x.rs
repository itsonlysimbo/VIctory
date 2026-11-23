//! `d3d12_xs.dll`: device creation. The device and everything it creates
//! live in `victory-gpu`.

use victory_gpu::device;

use crate::{HleExport, HleModule};

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "d3d12x",
        dlls: &["d3d12_xs.dll"],
        exports: vec![
            HleExport {
                name: "D3D12XboxCreateDevice",
                address: device::create as *const () as usize,
            },
            HleExport {
                name: "D3D12SerializeRootSignature",
                address: victory_gpu::root::serialize as *const () as usize,
            },
            // A data import: the title reads the id at this address.
            HleExport {
                name: "IID_ID3D12Device",
                address: (&raw const device::IID) as usize,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HleResolver, HostPolicy};
    use victory_loader::ImportResolver;
    use victory_loader::Resolution;
    use victory_pe::ImportSymbol;

    #[test]
    fn the_interface_id_import_is_bound_to_data() {
        let symbol = ImportSymbol::Name {
            hint: 0,
            name: "IID_ID3D12Device".into(),
        };
        let r = HleResolver::new(HostPolicy::none());
        let Resolution::Found { address, .. } = r.resolve("d3d12_xs.dll", &symbol) else {
            panic!("not bound");
        };
        // SAFETY: the address is our static id.
        assert_eq!(unsafe { *(address as *const [u8; 16]) }, device::IID);
    }
}
