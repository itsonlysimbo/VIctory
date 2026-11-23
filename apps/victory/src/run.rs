use std::path::Path;

use anyhow::{Context, Result};

use crate::Unimplemented;

#[cfg(windows)]
pub fn run(exe: &Path, unimplemented: Unimplemented, strict: bool, execute: bool) -> Result<()> {
    use std::sync::Arc;
    use victory_hle::{HleResolver, HostPolicy};
    use victory_loader::UnimplementedPolicy;

    let cfg = victory_core::Config::load()?;
    let original = std::fs::read(exe).with_context(|| format!("reading {}", exe.display()))?;
    let prepared = victory_loader::prepare(&original)?;
    let hash = prepared.original_hash.to_hex();
    match victory_game::identify(&hash) {
        Some(build) => println!("build          {} ({hash})", build.label),
        None => println!("build          unknown ({hash})"),
    }

    let image = victory_loader::load(&prepared, &cfg.cache_subdir("images")?)?;
    victory_hle::runtime::set_main_image(image.base());
    let resolver = HleResolver::new(if strict {
        HostPolicy::none()
    } else {
        HostPolicy::default()
    });
    let policy = match unimplemented {
        Unimplemented::Trap => UnimplementedPolicy::Trap,
        Unimplemented::Zero => UnimplementedPolicy::ReturnZero,
    };
    let report = image.bind_imports(&resolver, policy)?;
    println!(
        "mapped at      {:#x} (preferred {:#x})",
        image.base(),
        image.info().image_base
    );
    println!(
        "bound          {} hle, {} host, {} missing",
        report.hle,
        report.host,
        report.missing.len()
    );
    for m in &report.missing {
        println!("  missing      {}!{}", m.dll, m.symbol);
    }
    if !execute {
        println!("not executing (pass --execute to start the title)");
        return Ok(());
    }
    // SAFETY: imports are bound; from here on we run title code.
    let main = unsafe { Arc::new(image).spawn_main()? };
    let code = main
        .join()
        .map_err(|_| anyhow::anyhow!("title main thread panicked"))?;
    println!("title exited with {code}");
    Ok(())
}

#[cfg(not(windows))]
pub fn run(_: &Path, _: Unimplemented, _: bool, _: bool) -> Result<()> {
    anyhow::bail!("running titles requires Windows")
}
