//! Native GFX10 programs that the console compiler stores in a `DXIL` part.
//!
//! An `XDAT` index after the program header lists blobs stored around the
//! bitcode; one or more of them hold native programs, zlib-packed (a vertex
//! shader can carry two: compiled for use with and without a geometry
//! shader). Layout as observed in shipped titles (index offsets are relative
//! to the part data):
//!
//! ```text
//! +0x18 "XDAT"  u32 entry count, u32 (0x24 seen), then per entry:
//!               u32 kind, u32 offset, u32 size
//! kind 0x0d     "XBCH" u32 version (3), u64 packed size, u64 unpacked size,
//!               zlib stream
//! unpacked      "FSSM" u32 version (0x101), u64 payload size, payload
//! payload +0x00 u64 graphics slot count (6)
//!         +0x08 u64 record offsets: LS HS ES GS VS PS (0x08..0x30),
//!               mesh 0x48, compute 0x60; unused ones are 0xDEADBEEF_DEADBEEF
//! record  +0x00 u64 code offset, +0x08 u64 code size (payload-relative),
//!         +0x14 u32 flags: bit 5 wave32 (else wave64)
//!         +0x50 u32 bits 0-9 VGPRs used, bits 20-27 SGPRs used
//! ```
//!
//! Other kinds seen: 0x07 program header + bitcode (or a stub when stripped),
//! 0x0a `xbop` (compiler options), 0x0e a table not decoded yet, 0x10 and
//! 0x12 16-byte blobs (0x10 is the shader hash).
//! Unknown versions are rejected so a format change shows up as an error.

use crate::ShaderError::{self, Malformed};
use crate::dxbc::{u32_at, u64_at};

const XDAT_OFFSET: usize = 0x18;
const MAX_XDAT_ENTRIES: usize = 64;
const KIND_NATIVE: u32 = 0x0d;
const XBCH_VERSION: u32 = 3;
const XBCH_HEADER: usize = 0x18;
const FSSM_VERSION: u32 = 0x101;
const FSSM_HEADER: usize = 0x10;
const GRAPHICS_SLOT_COUNT: u64 = 6;
/// Where each stage's record offset sits in the FSSM payload.
const SLOTS: [(HwStage, usize); 8] = [
    (HwStage::Ls, 0x08),
    (HwStage::Hs, 0x10),
    (HwStage::Es, 0x18),
    (HwStage::Gs, 0x20),
    (HwStage::Vs, 0x28),
    (HwStage::Ps, 0x30),
    (HwStage::Ms, 0x48),
    (HwStage::Cs, 0x60),
];
const ABSENT: u64 = 0xDEAD_BEEF_DEAD_BEEF;
const RECORD_SIZE: u64 = 0x54;
const RECORD_FLAGS: usize = 0x14;
const FLAG_WAVE32: u32 = 1 << 5;
const RECORD_REGS: usize = 0x50;
/// Unpacked blobs are a few KiB; refuse to allocate for absurd sizes.
const MAX_UNPACKED: u64 = 16 << 20;

/// Hardware shader stage a native program runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HwStage {
    Ls,
    Hs,
    Es,
    Gs,
    Vs,
    Ps,
    /// Mesh shader.
    Ms,
    Cs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeProgram {
    pub stage: HwStage,
    /// Runs as 64 lanes per wave (else 32); sizes lane masks.
    pub wave64: bool,
    /// VGPRs the program uses (v0 up to this), the bound of indexed access.
    pub vgprs: u16,
    /// GFX10 machine code.
    pub code: Vec<u8>,
}

/// The native programs in a `DXIL` part's data; empty for the PC layout.
pub fn native_programs(part: &[u8]) -> Result<Vec<NativeProgram>, ShaderError> {
    if part.get(XDAT_OFFSET..XDAT_OFFSET + 4) != Some(b"XDAT") {
        return Ok(Vec::new());
    }
    let count = u32_at(part, XDAT_OFFSET + 4)? as usize;
    if count > MAX_XDAT_ENTRIES {
        return Err(Malformed("implausible XDAT entry count"));
    }
    let mut out = Vec::new();
    for i in 0..count {
        let at = XDAT_OFFSET + 12 + 12 * i;
        if u32_at(part, at)? != KIND_NATIVE {
            continue;
        }
        let offset = u32_at(part, at + 4)?.into();
        let size = u32_at(part, at + 8)?.into();
        let blob = bytes(part, offset, size).ok_or(Malformed("XDAT entry out of bounds"))?;
        out.extend(programs(&unpack(blob)?)?);
    }
    Ok(out)
}

fn unpack(blob: &[u8]) -> Result<Vec<u8>, ShaderError> {
    if blob.get(0..4) != Some(b"XBCH") {
        return Err(Malformed("native blob lacks XBCH magic"));
    }
    if u32_at(blob, 4)? != XBCH_VERSION {
        return Err(Malformed("unknown XBCH version"));
    }
    let packed = u64_at(blob, 8)?;
    let unpacked = u64_at(blob, 16)?;
    if unpacked > MAX_UNPACKED {
        return Err(Malformed("implausible XBCH unpacked size"));
    }
    let stream =
        bytes(blob, XBCH_HEADER as u64, packed).ok_or(Malformed("XBCH stream out of bounds"))?;
    let out = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(stream, unpacked as usize)
        .map_err(|_| Malformed("XBCH stream does not inflate to its stated size"))?;
    if out.len() as u64 != unpacked {
        return Err(Malformed("XBCH stream does not inflate to its stated size"));
    }
    Ok(out)
}

fn programs(fssm: &[u8]) -> Result<Vec<NativeProgram>, ShaderError> {
    if fssm.get(0..4) != Some(b"FSSM") {
        return Err(Malformed("native blob lacks FSSM magic"));
    }
    if u32_at(fssm, 4)? != FSSM_VERSION {
        return Err(Malformed("unknown FSSM version"));
    }
    let payload = &fssm[FSSM_HEADER.min(fssm.len())..];
    if u64_at(fssm, 8)? != payload.len() as u64 {
        return Err(Malformed("FSSM payload size disagrees"));
    }
    if u64_at(payload, 0)? != GRAPHICS_SLOT_COUNT {
        return Err(Malformed("unexpected FSSM slot count"));
    }
    let mut out = Vec::new();
    for (stage, at) in SLOTS {
        let record = u64_at(payload, at)?;
        if record == ABSENT {
            continue;
        }
        let record =
            bytes(payload, record, RECORD_SIZE).ok_or(Malformed("FSSM record out of bounds"))?;
        let code = bytes(payload, u64_at(record, 0)?, u64_at(record, 8)?)
            .ok_or(Malformed("native code out of bounds"))?;
        let vgprs = (u32_at(record, RECORD_REGS)? & 0x3FF) as u16;
        if vgprs > 256 {
            return Err(Malformed("implausible VGPR count"));
        }
        out.push(NativeProgram {
            stage,
            wave64: u32_at(record, RECORD_FLAGS)? & FLAG_WAVE32 == 0,
            vgprs,
            code: code.to_vec(),
        });
    }
    Ok(out)
}

/// `data[offset..offset + len]` with untrusted 64-bit bounds.
fn bytes(data: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    data.get(start..start.checked_add(usize::try_from(len).ok()?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VS_CODE: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    const PS_CODE: [u8; 4] = [9, 10, 11, 12];
    const VGPRS: u32 = 28;

    fn slot(stage: HwStage) -> usize {
        SLOTS.iter().find(|s| s.0 == stage).unwrap().1
    }

    /// FSSM blob with `programs` as (stage, code) pairs.
    fn fssm(programs: &[(HwStage, &[u8])]) -> Vec<u8> {
        let mut payload = GRAPHICS_SLOT_COUNT.to_le_bytes().to_vec();
        payload.resize(slot(HwStage::Cs) + 8, 0);
        for (_, at) in SLOTS {
            payload[at..at + 8].copy_from_slice(&ABSENT.to_le_bytes());
        }
        for &(stage, code) in programs {
            let slot = slot(stage);
            let record = payload.len() as u64;
            payload[slot..slot + 8].copy_from_slice(&record.to_le_bytes());
            payload.extend_from_slice(&(record + RECORD_SIZE).to_le_bytes());
            payload.extend_from_slice(&(code.len() as u64).to_le_bytes());
            let flags = if stage == HwStage::Ps { FLAG_WAVE32 } else { 0 };
            payload.extend_from_slice(&[0; 4]);
            payload.extend_from_slice(&flags.to_le_bytes());
            payload.resize(record as usize + RECORD_REGS, 0);
            payload.extend_from_slice(&(VGPRS | 31 << 20).to_le_bytes());
            payload.extend_from_slice(code);
        }
        let mut out = b"FSSM".to_vec();
        out.extend_from_slice(&FSSM_VERSION.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        out.extend_from_slice(&payload);
        out
    }

    fn xbch(unpacked: &[u8]) -> Vec<u8> {
        let stream = miniz_oxide::deflate::compress_to_vec_zlib(unpacked, 6);
        let mut out = b"XBCH".to_vec();
        out.extend_from_slice(&XBCH_VERSION.to_le_bytes());
        out.extend_from_slice(&(stream.len() as u64).to_le_bytes());
        out.extend_from_slice(&(unpacked.len() as u64).to_le_bytes());
        out.extend_from_slice(&stream);
        out
    }

    /// `DXIL` part data: program header, XDAT index, then the blobs.
    fn part(blobs: &[(u32, &[u8])]) -> Vec<u8> {
        let mut out = vec![0u8; XDAT_OFFSET];
        out.extend_from_slice(b"XDAT");
        out.extend_from_slice(&(blobs.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x24u32.to_le_bytes());
        let mut offset = out.len() + 12 * blobs.len();
        for (kind, blob) in blobs {
            for v in [*kind, offset as u32, blob.len() as u32] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            offset += blob.len();
        }
        for (_, blob) in blobs {
            out.extend_from_slice(blob);
        }
        out
    }

    #[test]
    fn finds_graphics_programs() {
        let native = xbch(&fssm(&[(HwStage::Vs, &VS_CODE), (HwStage::Ps, &PS_CODE)]));
        let data = part(&[(0x0a, b"xbop"), (KIND_NATIVE, &native)]);
        let found = native_programs(&data).unwrap();
        assert_eq!(
            found,
            [
                NativeProgram {
                    stage: HwStage::Vs,
                    wave64: true,
                    vgprs: 28,
                    code: VS_CODE.to_vec()
                },
                NativeProgram {
                    stage: HwStage::Ps,
                    wave64: false,
                    vgprs: 28,
                    code: PS_CODE.to_vec()
                },
            ]
        );
    }

    #[test]
    fn finds_mesh_and_compute_programs() {
        for stage in [HwStage::Ms, HwStage::Cs] {
            let data = part(&[(KIND_NATIVE, &xbch(&fssm(&[(stage, &VS_CODE)])))]);
            let found = native_programs(&data).unwrap();
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].stage, stage);
        }
    }

    #[test]
    fn reads_every_native_blob() {
        let es = xbch(&fssm(&[(HwStage::Es, &VS_CODE)]));
        let vs = xbch(&fssm(&[(HwStage::Vs, &VS_CODE)]));
        let data = part(&[(0x12, &[0; 16]), (KIND_NATIVE, &es), (KIND_NATIVE, &vs)]);
        let stages: Vec<_> = native_programs(&data)
            .unwrap()
            .iter()
            .map(|p| p.stage)
            .collect();
        assert_eq!(stages, [HwStage::Es, HwStage::Vs]);
    }

    #[test]
    fn pc_layout_has_none() {
        assert!(native_programs(&[0u8; 64]).unwrap().is_empty());
        assert!(
            native_programs(&part(&[(0x0a, b"xbop")]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_corrupt_blobs() {
        let good = xbch(&fssm(&[(HwStage::Ls, &VS_CODE)]));
        let corrupt = |at: usize, byte: u8| {
            let mut blob = good.clone();
            blob[at] = byte;
            native_programs(&part(&[(KIND_NATIVE, &blob)]))
        };
        assert!(corrupt(4, 9).is_err(), "unknown version");
        assert!(corrupt(16, 0xFF).is_err(), "stated size");
        assert!(corrupt(XBCH_HEADER + 2, 0xFF).is_err(), "zlib stream");
        assert!(native_programs(&part(&[(KIND_NATIVE, &good[..20])])).is_err());

        let mut bad_record = fssm(&[(HwStage::Ls, &VS_CODE)]);
        let at = FSSM_HEADER + slot(HwStage::Ls);
        bad_record[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(native_programs(&part(&[(KIND_NATIVE, &xbch(&bad_record))])).is_err());
    }
}
