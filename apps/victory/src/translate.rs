//! The offline batch: every shader found goes through the translator, and
//! what it cannot handle yet is tallied (decision 0003).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use victory_shader::structure::{self, Atom, LaneExit};
use victory_shader::{Container, FourCC, ShaderError, ir, native_programs};

use crate::shader::{MAX_FILE, Source, sources};

#[derive(Default)]
struct Tally {
    containers: usize,
    duplicates: usize,
    programs: usize,
    translated: usize,
    instructions: usize,
    structured: usize,
    vector: usize,
    guarded: usize,
    /// Guards that read a mask the pass cannot see into.
    opaque: usize,
    lane_branches: usize,
    uniform_branches: usize,
    /// Reason → programs stopped by it, and the first one.
    failed: BTreeMap<String, (usize, String)>,
}

impl Tally {
    fn fail(&mut self, reason: String, example: impl FnOnce() -> String) {
        self.failed
            .entry(reason)
            .or_insert_with(|| (0, example()))
            .0 += 1;
    }

    fn merge(&mut self, other: Tally) {
        self.containers += other.containers;
        self.duplicates += other.duplicates;
        self.programs += other.programs;
        self.translated += other.translated;
        self.instructions += other.instructions;
        self.structured += other.structured;
        self.vector += other.vector;
        self.guarded += other.guarded;
        self.opaque += other.opaque;
        self.lane_branches += other.lane_branches;
        self.uniform_branches += other.uniform_branches;
        for (reason, (n, example)) in other.failed {
            self.failed.entry(reason).or_insert((0, example)).0 += n;
        }
    }
}

pub fn run(paths: Vec<PathBuf>) -> Result<()> {
    let paths = crate::default_paths(paths)?;
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let seen = Mutex::new(HashSet::new());
    let total = Mutex::new(Tally::default());
    for root in &paths {
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() || entry.metadata().map_or(0, |m| m.len()) > MAX_FILE {
                continue;
            }
            let Ok(data) = std::fs::read(entry.path()) else {
                continue;
            };
            let jobs = sources(&data);
            let next = AtomicUsize::new(0);
            std::thread::scope(|s| {
                for _ in 0..threads.min(jobs.len()) {
                    s.spawn(|| {
                        let mut t = Tally::default();
                        while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                            translate(job, entry.path(), &seen, &mut t);
                        }
                        lock(&total).merge(t);
                    });
                }
            });
        }
    }
    let t = total.into_inner().unwrap_or_else(|e| e.into_inner());
    println!(
        "containers   {} ({} duplicates skipped)",
        t.containers, t.duplicates
    );
    println!(
        "programs     {} translated of {} ({} IR instructions)",
        t.translated, t.programs, t.instructions
    );
    println!(
        "structured   {} of {} ({} vector instructions, {} guarded, {} on opaque masks; \
         branches {} per lane, {} uniform)",
        t.structured,
        t.translated,
        t.vector,
        t.guarded,
        t.opaque,
        t.lane_branches,
        t.uniform_branches
    );
    let mut failed: Vec<_> = t.failed.into_iter().collect();
    failed.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    for (reason, (n, example)) in failed {
        println!("  {n:>7}  {reason}  (e.g. {example})");
    }
    Ok(())
}

fn translate(source: &Source, path: &Path, seen: &Mutex<HashSet<blake3::Hash>>, t: &mut Tally) {
    let example = || format!("{}{}", path.display(), source.label());
    let Some(bytes) = source.bytes() else {
        return t.fail("corrupt record".into(), example);
    };
    t.containers += 1;
    let Some(dxil) = Container::parse(&bytes)
        .ok()
        .and_then(|c| c.part(FourCC::DXIL).map(|p| p.data))
    else {
        return t.fail("no DXIL part (SM5 or older)".into(), example);
    };
    if !lock(seen).insert(blake3::hash(dxil)) {
        t.duplicates += 1;
        return;
    }
    let programs = match native_programs(dxil) {
        Ok(p) => p,
        Err(e) => return t.fail(e.to_string(), example),
    };
    for p in programs {
        t.programs += 1;
        match ir::build(&p.code, p.wave64, p.vgprs) {
            Ok(program) => {
                t.translated += 1;
                t.instructions += program.blocks.iter().map(|b| b.insts.len()).sum::<usize>();
                match structure::structure(&program) {
                    Ok(s) => tally_structured(&program, &s, t),
                    Err(e) => {
                        let reason = match e {
                            ShaderError::Unsupported { what, .. } => format!("structure: {what}"),
                            ShaderError::Isa { reason, .. } => format!("structure: {reason}"),
                            e => e.to_string(),
                        };
                        t.fail(reason, || format!("{} {:?}", example(), p.stage));
                    }
                }
            }
            Err(e) => {
                let reason = match e {
                    ShaderError::Unsupported { what, .. } => format!("unsupported: {what}"),
                    e => e.to_string(),
                };
                t.fail(reason, || format!("{} {:?}", example(), p.stage));
            }
        }
    }
}

fn tally_structured(program: &ir::Program, s: &structure::Structured, t: &mut Tally) {
    t.structured += 1;
    for (block, guards) in program.blocks.iter().zip(&s.guards) {
        t.vector += block
            .insts
            .iter()
            .filter(|i| i.read(ir::Reg::S(ir::EXEC)).is_some())
            .count();
        for &g in guards.iter().flatten() {
            t.guarded += 1;
            let support = s.bools.support(g);
            t.opaque += usize::from(support.iter().any(|a| matches!(a, Atom::Opaque(_))));
        }
    }
    for exit in &s.exits {
        if let LaneExit::Branch { when, .. } = *exit {
            if s.bools
                .support(when)
                .iter()
                .all(|a| matches!(a, Atom::Uniform(_)))
            {
                t.uniform_branches += 1;
            } else {
                t.lane_branches += 1;
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
