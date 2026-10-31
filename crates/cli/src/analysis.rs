use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use autogpuaffinity_domain::cpu::Cpu;
use autogpuaffinity_domain::report::{self, CpuResult, Metrics, TABLE_WIDTH};
use autogpuaffinity_windows::console;

use crate::output;

pub fn find_captures(dir: &Path) -> Result<Vec<(Cpu, PathBuf)>> {
    let entries = fs::read_dir(dir).with_context(|| format!("unable to read directory {}", dir.display()))?;
    let mut captures = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("unable to read directory {}", dir.display()))?;
        if let Some(cpu) = report::capture_cpu(&entry.file_name().to_string_lossy()) {
            captures.push((cpu, entry.path()));
        }
    }
    captures.sort_unstable_by_key(|(cpu, _)| *cpu);
    Ok(captures)
}

pub fn load_capture(path: &Path) -> Result<Metrics> {
    let csv = fs::read(path).with_context(|| format!("unable to read {}", path.display()))?;
    report::analyze_capture(&csv).with_context(|| format!("unusable capture {}", path.display()))
}

/// Also accepts a session directory, the parent of `CSVs`.
pub fn analyze(dir: &Path, colors: bool) -> Result<()> {
    let mut captures = find_captures(dir)?;
    if captures.is_empty() && dir.join("CSVs").is_dir() {
        captures = find_captures(&dir.join("CSVs"))?;
    }
    if captures.is_empty() {
        bail!("no CPU-<n>.csv file found in {}", dir.display());
    }

    let mut results = Vec::new();
    for (cpu, path) in captures {
        match load_capture(&path) {
            Ok(metrics) => results.push(CpuResult { cpu, metrics }),
            Err(error) => output::warn(format!("skipping CPU {cpu}: {error:#}")),
        }
    }
    if results.is_empty() {
        bail!("no usable capture found in {}", dir.display());
    }

    widen_console(colors);
    print!("{}", report::render_table(&results, &report::rank(&results), colors));
    println!();
    Ok(())
}

/// xterm resize sequence, ignored by terminals that do not support it.
fn widen_console(escape_sequences: bool) {
    if escape_sequences
        && let Some((columns, rows)) = console::window_size()
        && usize::from(columns) <= TABLE_WIDTH
    {
        print!("\x1b[8;{rows};{}t", TABLE_WIDTH + 1);
    }
}
