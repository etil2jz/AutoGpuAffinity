use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use autogpuaffinity_domain::config::{Api, Config};
use autogpuaffinity_domain::cpu::{self, Cpu};
use autogpuaffinity_domain::device::GraphicsCard;
use autogpuaffinity_domain::report;
use autogpuaffinity_domain::schedule::{self, AFTERBURNER_DELAY, LAUNCH_DELAY, SETTLE_DELAY};
use autogpuaffinity_windows::console;
use autogpuaffinity_windows::devices::DeviceManager;
use autogpuaffinity_windows::process::{self, OnInterrupt, Output, Subject};
use autogpuaffinity_windows::registry;
use chrono::{DateTime, Local, TimeDelta};

use crate::affinity::{self, AffinityGuard};
use crate::analysis;
use crate::output;

/// Time granted to PresentMon beyond the benchmark duration before it is considered stuck.
const PRESENTMON_GRACE: Duration = Duration::from_secs(60);
const XPERF_TIMEOUT: Duration = Duration::from_secs(600);
/// ERROR_WMI_INSTANCE_NOT_FOUND as an HRESULT: no trace was running.
const XPERF_NO_ACTIVE_SESSION: u32 = 0x8007_1069;

pub struct Environment<'a> {
    pub program_dir: &'a Path,
    pub devices: &'a DeviceManager,
    pub cards: &'a [GraphicsCard],
    pub cpu_count: u32,
    pub colors: bool,
    pub backup_path: PathBuf,
}

struct Plan<'a> {
    env: &'a Environment<'a>,
    config: &'a Config,
    subject_path: PathBuf,
    subject_name: String,
    subject_args: Vec<String>,
    presentmon_path: PathBuf,
    csv_dir: PathBuf,
    xperf_dir: PathBuf,
}

impl Plan<'_> {
    fn csv_path(&self, cpu: Cpu) -> PathBuf {
        self.csv_dir.join(report::capture_file_name(cpu))
    }

    fn etl_path(&self, cpu: Cpu) -> PathBuf {
        self.xperf_dir.join(format!("CPU-{cpu}.etl"))
    }

    fn report_path(&self, cpu: Cpu) -> PathBuf {
        self.xperf_dir.join(format!("CPU-{cpu}.txt"))
    }

    /// Kept partial results must only contain complete measurements.
    fn discard(&self, cpu: Cpu) {
        for path in [self.csv_path(cpu), self.etl_path(cpu), self.report_path(cpu)] {
            let _ = fs::remove_file(path);
        }
    }

    fn check_subject(&self, subject: &mut Subject) -> Result<()> {
        subject.ensure_running().map_err(|error| match self.config.api {
            Api::Liblava => anyhow!(
                "{}, see {} for details",
                output::error_chain(error),
                self.env.program_dir.join("lava.log").display()
            ),
            Api::D3D9 => error.into(),
        })
    }
}

pub fn run(env: &Environment, config: &Config) -> Result<()> {
    let cpus = cpu::select_cpus(&config.custom_cpus, env.cpu_count)?;

    let subject_path = env.program_dir.join(config.api.binary_path());
    // PresentMon 1.10 does not work on Windows Server editions.
    let presentmon_name = if registry::is_server()? {
        "PresentMon-1.6.0-x64.exe"
    } else {
        "PresentMon-1.10.0-x64.exe"
    };
    let presentmon_path = env.program_dir.join(r"bin\PresentMon").join(presentmon_name);
    for path in [&subject_path, &presentmon_path] {
        if !path.is_file() {
            bail!(
                "missing file {}, extract the full release archive again",
                path.display()
            );
        }
    }

    let started = Local::now();
    let session_dir = env
        .program_dir
        .join("captures")
        .join(format!("AutoGpuAffinity-{}", started.format("%d%m%y%H%M%S")));
    let plan = Plan {
        env,
        config,
        subject_name: subject_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        subject_path,
        subject_args: config.subject_args(),
        presentmon_path,
        csv_dir: session_dir.join("CSVs"),
        xperf_dir: session_dir.join("xperf"),
    };

    print_summary(&plan, &cpus, &session_dir, started);

    if !config.skip_confirmation {
        console::wait_for_enter("press enter to start benchmarking...")?;
    }

    if config.xperf_enabled {
        stop_existing_xperf_trace(config, env.program_dir)?;
    }
    process::kill_by_name(&[&plan.subject_name, presentmon_name])?;

    fs::create_dir_all(&plan.csv_dir).with_context(|| format!("unable to create {}", plan.csv_dir.display()))?;
    if config.xperf_enabled {
        fs::create_dir_all(&plan.xperf_dir)
            .with_context(|| format!("unable to create {}", plan.xperf_dir.display()))?;
    }

    let guard = AffinityGuard::arm(env.devices, env.cards, env.backup_path.clone())?;
    let outcome = benchmark_all(&plan, &cpus);
    let restored = guard.restore();
    if config.xperf_enabled {
        remove_kernel_etl(env.program_dir);
    }

    match outcome {
        Ok(()) => {
            println!();
            let displayed = analysis::analyze(&plan.csv_dir, env.colors);
            match (restored, displayed) {
                (Ok(()), displayed) => displayed,
                (Err(error), Ok(())) => Err(error),
                (Err(error), Err(display_error)) => {
                    output::error(format!("{display_error:#}"));
                    Err(error)
                }
            }
        }
        Err(error) => {
            if let Err(restore_error) = restored {
                output::error(format!("{restore_error:#}"));
            }
            keep_partial_results(&session_dir, &plan.csv_dir);
            Err(error)
        }
    }
}

fn end_time(started: DateTime<Local>, duration: Duration) -> String {
    let Some(end) = TimeDelta::from_std(duration)
        .ok()
        .and_then(|delta| started.checked_add_signed(delta))
    else {
        return "unknown".to_owned();
    };
    let days = (end.date_naive() - started.date_naive()).num_days();
    let clock = end.format("%H:%M:%S");
    if days == 0 {
        clock.to_string()
    } else {
        format!("{clock} (+{days}d)")
    }
}

fn print_summary(plan: &Plan, cpus: &[Cpu], session_dir: &Path, started: DateTime<Local>) {
    let config = plan.config;
    let estimated = schedule::estimate(config, cpus.len(), plan.env.cards.len());
    let benchmark_cpus = if config.custom_cpus.is_empty() {
        "All".to_owned()
    } else {
        cpu::join_cpus(cpus)
    };
    let cards = plan
        .env
        .cards
        .iter()
        .map(|card| card.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let rows = [
        ("Session Directory", session_dir.display().to_string()),
        ("Graphics Cards", cards),
        ("Cache Duration", config.cache_duration.to_string()),
        ("Benchmark Duration", config.benchmark_duration.to_string()),
        ("Benchmark CPUs", benchmark_cpus),
        (
            "Subject",
            plan.subject_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        ("Estimated Time", schedule::format_duration(estimated)),
        ("Estimated End Time", end_time(started, estimated)),
        ("Load Afterburner", (config.afterburner_profile > 0).to_string()),
        ("DPC/ISR Logging", config.xperf_enabled.to_string()),
        ("Save ETLs", config.xperf_save_etls.to_string()),
        ("Sync Affinity", config.sync_driver_affinity.to_string()),
    ];
    for (label, value) in rows {
        println!("{label:<25}{value}");
    }
    println!();
}

fn benchmark_all(plan: &Plan, cpus: &[Cpu]) -> Result<()> {
    for (index, &cpu) in cpus.iter().enumerate() {
        console::check_interrupted()?;
        output::info(format!("benchmarking CPU {cpu} ({}/{})", index + 1, cpus.len()));
        if let Err(error) = benchmark_cpu(plan, cpu) {
            plan.discard(cpu);
            return Err(error.context(format!("benchmark of CPU {cpu} failed")));
        }
    }
    Ok(())
}

fn benchmark_cpu(plan: &Plan, cpu: Cpu) -> Result<()> {
    let config = plan.config;

    affinity::apply(plan.env.devices, plan.env.cards, cpu)?;
    console::sleep(SETTLE_DELAY)?;

    if config.afterburner_profile > 0 {
        load_afterburner_profile(&config.afterburner_location, config.afterburner_profile)?;
    }

    let mask = config.sync_driver_affinity.then(|| cpu.mask());
    let mut subject = Subject::launch(&plan.subject_path, &plan.subject_args, plan.env.program_dir, mask)?;
    console::sleep(LAUNCH_DELAY + Duration::from_secs(config.cache_duration.into()))?;
    plan.check_subject(&mut subject)?;

    let trace = if config.xperf_enabled {
        Some(XperfTrace::start(&config.xperf_location, plan.env.program_dir)?)
    } else {
        None
    };

    let csv_path = plan.csv_path(cpu);
    let duration = config.benchmark_duration.to_string();
    let output = process::run(
        Command::new(&plan.presentmon_path)
            .args([
                "-stop_existing_session",
                "-no_top",
                "-timed",
                &duration,
                "-process_name",
                subject.name(),
            ])
            .arg("-output_file")
            .arg(&csv_path)
            .arg("-terminate_after_timed")
            .current_dir(plan.env.program_dir),
        Duration::from_secs(config.benchmark_duration.into()) + PRESENTMON_GRACE,
        OnInterrupt::Abort,
    )?;
    // PresentMon may have stopped on Ctrl+C with a partial capture.
    console::check_interrupted()?;
    if !output.status.success() {
        bail!("PresentMon failed: {}", output.failure_details());
    }
    // A capture taken while the subject was exiting would be meaningless.
    plan.check_subject(&mut subject)?;

    if !csv_path.is_file() {
        bail!("csv log unsuccessful, this may be due to a missing dependency or windows component");
    }
    analysis::load_capture(&csv_path)?;

    if let Some(trace) = trace {
        trace.finish(&plan.etl_path(cpu), &plan.report_path(cpu), config.xperf_save_etls)?;
    }
    Ok(())
}

fn load_afterburner_profile(location: &Path, profile: u32) -> Result<()> {
    let mut afterburner = process::spawn(Command::new(location).arg(format!("/Profile{profile}")).arg("/Q"))?;
    let waited = console::sleep(AFTERBURNER_DELAY);
    process::terminate(&mut afterburner);
    Ok(waited?)
}

fn run_xperf(xperf: &Path, directory: &Path, args: &[&OsStr], on_interrupt: OnInterrupt) -> Result<Output> {
    Ok(process::run(
        Command::new(xperf).args(args).current_dir(directory),
        XPERF_TIMEOUT,
        on_interrupt,
    )?)
}

fn stop_existing_xperf_trace(config: &Config, directory: &Path) -> Result<()> {
    let output = run_xperf(
        &config.xperf_location,
        directory,
        &["-stop".as_ref()],
        OnInterrupt::Abort,
    )?;
    match output.exit_code() {
        Some(0 | XPERF_NO_ACTIVE_SESSION) => Ok(()),
        _ => bail!("unable to stop the existing xperf trace: {}", output.failure_details()),
    }
}

/// Kernel trace stopped on drop unless it was finished.
struct XperfTrace<'a> {
    xperf: &'a Path,
    directory: &'a Path,
    active: bool,
}

impl<'a> XperfTrace<'a> {
    fn start(xperf: &'a Path, directory: &'a Path) -> Result<Self> {
        // Armed before `-on` so that a trace started by an interrupted xperf is still stopped.
        let trace = Self {
            xperf,
            directory,
            active: true,
        };
        let output = run_xperf(
            xperf,
            directory,
            &["-on".as_ref(), "base+interrupt+dpc".as_ref()],
            OnInterrupt::Abort,
        )?;
        if !output.status.success() {
            bail!("unable to start the xperf trace: {}", output.failure_details());
        }
        Ok(trace)
    }

    fn finish(mut self, etl: &Path, report: &Path, keep_etl: bool) -> Result<()> {
        let output = run_xperf(
            self.xperf,
            self.directory,
            &["-d".as_ref(), etl.as_os_str()],
            OnInterrupt::Abort,
        )?;
        if !output.status.success() {
            bail!("unable to stop the xperf trace: {}", output.failure_details());
        }
        self.active = false;

        let args = [
            "-quiet".as_ref(),
            "-i".as_ref(),
            etl.as_os_str(),
            "-o".as_ref(),
            report.as_os_str(),
            "-a".as_ref(),
            "dpcisr".as_ref(),
        ];
        let output = run_xperf(self.xperf, self.directory, &args, OnInterrupt::Abort)?;
        if !output.status.success() {
            bail!("unable to generate dpcisr report: {}", output.failure_details());
        }

        if !keep_etl && let Err(error) = fs::remove_file(etl) {
            output::warn(format!("unable to delete {}: {error}", etl.display()));
        }
        Ok(())
    }
}

impl Drop for XperfTrace<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = run_xperf(self.xperf, self.directory, &["-stop".as_ref()], OnInterrupt::Continue);
        }
    }
}

/// xperf writes its raw trace to `\kernel.etl` at the root of the current drive.
fn remove_kernel_etl(program_dir: &Path) {
    let mut candidates = vec![PathBuf::from(r"C:\kernel.etl")];
    if let Some(root) = program_dir.ancestors().last() {
        let path = root.join("kernel.etl");
        if !candidates
            .iter()
            .any(|candidate| candidate.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
        {
            candidates.push(path);
        }
    }
    for path in candidates.iter().filter(|path| path.is_file()) {
        if let Err(error) = fs::remove_file(path) {
            output::warn(format!("unable to delete {}: {error}", path.display()));
        }
    }
}

fn keep_partial_results(session_dir: &Path, csv_dir: &Path) {
    if analysis::find_captures(csv_dir).is_ok_and(|captures| !captures.is_empty()) {
        output::info(format!(
            "completed measurements were kept, analyze them with: AutoGpuAffinity --analyze \"{}\"",
            csv_dir.display()
        ));
    } else if session_dir.exists()
        && let Err(error) = fs::remove_dir_all(session_dir)
    {
        output::warn(format!("unable to delete {}: {error}", session_dir.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn end_time_wraps_past_midnight() {
        let started = Local.with_ymd_and_hms(2026, 3, 7, 23, 59, 0).unwrap();
        assert_eq!(end_time(started, Duration::from_secs(30)), "23:59:30");
        assert_eq!(end_time(started, Duration::from_secs(120)), "00:01:00 (+1d)");
    }
}
