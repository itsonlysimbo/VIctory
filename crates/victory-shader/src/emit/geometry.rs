//! Geometry shaders the host adds: the console's rectangle lists.

use super::HostShader;
use super::container::SigElement;
use crate::dxbc::{Container, FourCC};

/// A geometry shader for the console's rectangle lists after `vs`: each
/// triangle of three corners `(v0, v1, v2)` becomes `(v0, v1, v2)` and
/// `(v0, v2, v3)` with `v3 = v0 + v2 - v1` in every float output (integer
/// outputs keep `v0`'s), as the hardware completes a rectangle from the
/// vertex shader's outputs.
pub fn rect_gs(vs: &HostShader) -> Result<HostShader, String> {
    use super::bitcode::{Attr, Bin, Term};
    let c = Container::parse(&vs.dxil).map_err(|e| e.to_string())?;
    let outputs = match c.part(FourCC::OSG1) {
        Some(p) => super::signature(p.data).map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    if outputs.is_empty() {
        return Err("a vertex shader without outputs".into());
    }
    if let Some(e) = outputs.iter().find(|e| !matches!(e.comp, 4 | 5 | 9)) {
        return Err(format!("an output of component type {}", e.comp));
    }
    let outputs: Vec<SigElement> = outputs
        .into_iter()
        .map(|mut e| {
            e.usage = (1 << e.cols) - 1;
            e
        })
        .collect();
    let mut s = super::shader::Shader::new(false);
    s.geometry = Some(6);
    let b = s.entry;
    // Every output of every corner, then the fourth.
    let mut corners: Vec<Vec<Vec<super::bitcode::V>>> = Vec::new();
    for v in 0..3u32 {
        let mut els = Vec::new();
        for (id, e) in outputs.iter().enumerate() {
            let (suffix, ty) = if e.comp == 9 {
                (".f32", s.t.f32)
            } else {
                (".i32", s.t.i32)
            };
            let mut cols = Vec::new();
            for col in 0..e.cols {
                let args = [s.m.i32(id as u32), s.m.i32(0), s.m.i8(col), s.m.i32(v)];
                cols.push(s.op(
                    b,
                    &format!("loadInput{suffix}"),
                    Attr::ReadNone,
                    ty,
                    4,
                    &args,
                ));
            }
            els.push(cols);
        }
        corners.push(els);
    }
    let mut fourth = corners[0].clone();
    for (id, e) in outputs.iter().enumerate() {
        if e.comp != 9 {
            continue;
        }
        for col in 0..usize::from(e.cols) {
            let sum =
                s.m.bin(b, Bin::Add, corners[0][id][col], corners[2][id][col]);
            fourth[id][col] = s.m.bin(b, Bin::Sub, sum, corners[1][id][col]);
        }
    }
    corners.push(fourth);
    let zero = s.m.i8(0);
    for (k, &v) in [0usize, 1, 2, 0, 2, 3].iter().enumerate() {
        for (id, e) in outputs.iter().enumerate() {
            let suffix = if e.comp == 9 { ".f32" } else { ".i32" };
            for col in 0..e.cols {
                let x = corners[v][id][usize::from(col)];
                let args = [s.m.i32(id as u32), s.m.i32(0), s.m.i8(col), x];
                s.op(
                    b,
                    &format!("storeOutput{suffix}"),
                    Attr::NoUnwind,
                    s.t.void,
                    5,
                    &args,
                );
            }
        }
        s.op(b, "emitStream", Attr::NoUnwind, s.t.void, 97, &[zero]);
        if k == 2 {
            s.op(b, "cutStream", Attr::NoUnwind, s.t.void, 98, &[zero]);
        }
    }
    s.inputs = outputs.clone();
    s.outputs = outputs;
    s.m.terminate(b, Term::Ret);
    Ok(HostShader {
        stage: crate::native::HwStage::Gs,
        dxil: s.finish(),
        user_data: Vec::new(),
        samplers: Vec::new(),
        static_samplers: Vec::new(),
        root_cbvs: Vec::new(),
        root_srvs: Vec::new(),
        stride_table: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(name: &str, kind: u8, comp: u8, interp: u8, row: u32, cols: u8) -> SigElement {
        SigElement {
            name: name.into(),
            index: 0,
            kind,
            comp,
            interp,
            row: Some(row),
            col: 0,
            cols,
            usage: 0,
            half: false,
        }
    }

    /// A vertex shader writing zeros to `outputs`.
    fn vertex_shader(outputs: Vec<SigElement>) -> HostShader {
        use super::super::bitcode::{Attr, Term};
        let mut s = super::super::shader::Shader::new(false);
        let b = s.entry;
        for (id, e) in outputs.iter().enumerate() {
            for col in 0..e.cols {
                let (suffix, zero) = if e.comp == 9 {
                    (".f32", s.m.f32(0))
                } else {
                    (".i32", s.m.i32(0))
                };
                let args = [s.m.i32(id as u32), s.m.i32(0), s.m.i8(col), zero];
                s.op(
                    b,
                    &format!("storeOutput{suffix}"),
                    Attr::NoUnwind,
                    s.t.void,
                    5,
                    &args,
                );
            }
        }
        s.outputs = outputs
            .into_iter()
            .map(|mut e| {
                e.usage = (1 << e.cols) - 1;
                e
            })
            .collect();
        s.m.terminate(b, Term::Ret);
        HostShader {
            stage: crate::native::HwStage::Vs,
            dxil: s.finish(),
            user_data: Vec::new(),
            samplers: Vec::new(),
            static_samplers: Vec::new(),
            root_cbvs: Vec::new(),
            root_srvs: Vec::new(),
            stride_table: false,
        }
    }

    #[test]
    #[cfg(windows)]
    fn rectangle_geometry_shader_validates() {
        let vs = vertex_shader(vec![
            element("SV_Position", 3, 9, 4, 0, 4),
            element("TEXCOORD", 0, 9, 2, 1, 2),
            element("COLOR", 0, 5, 1, 2, 1),
        ]);
        let gs = rect_gs(&vs).unwrap();
        let path = super::super::validator::Validator::default_path();
        let Ok(mut v) = super::super::validator::Validator::load(path.as_deref()) else {
            eprintln!("no dxil.dll; skipped");
            return;
        };
        v.validate(&mut vs.dxil.clone()).unwrap();
        v.validate(&mut gs.dxil.clone()).unwrap();
    }
}
