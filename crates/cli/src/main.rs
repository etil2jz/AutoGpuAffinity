#![forbid(unsafe_code)]

mod affinity;
mod analysis;
mod benchmark;
mod output;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use autogpuaffinity_domain::config::Config;
use autogpuaffinity_domain::cpu::Cpu;
use autogpuaffinity_domain::device::{self, GraphicsCard};
use autogpuaffinity_domain::ini::decode_text;
use autogpuaffinity_windows::console::{self, Interrupted};
use autogpuaffinity_windows::devices::DeviceManager;
use autogpuaffinity_windows::process::{self, ProcessError};
use autogpuaffinity_windows::{registry, system};
use clap::Parser;

/// BasicDisplay start type meaning the service is disabled.
const SERVICE_DISABLED: u32 = 4;

#[derive(Parser)]
#[command(name = "AutoGpuAffinity", bin_name = "AutoGpuAffinity", version)]
struct Cli {
    /// path to config file
    #[arg(long, value_name = "config")]
    config: Option<PathBuf>,

    /// analyze csv files from a previous benchmark
    #[arg(long, value_name = "csv directory", conflicts_with = "apply_affinity")]
    analyze: Option<PathBuf>,

    /// assign a single core affinity to graphics drivers
    #[arg(long, value_name = "cpu")]
    apply_affinity: Option<u32>,
}

fn main() -> ExitCode {
    if let Err(error) = console::install_interrupt_handler() {
        output::warn(output::error_chain(error));
    }
    println!(
        "AutoGpuAffinity Version {} - GPLv3\nGitHub - https://github.com/valleyofdoom\n",
        env!("CARGO_PKG_VERSION")
    );

    // Restoration guards also run while a panic unwinds; catching it keeps the final pause.
    let code = match std::panic::catch_unwind(run) {
        Ok(Ok(code)) => code,
        Ok(Err(error)) => {
            report(&error);
            1
        }
        Err(_) => 101,
    };

    console::mark_shutdown_complete();
    console::pause_if_launched_from_explorer();
    ExitCode::from(code)
}

fn report(error: &anyhow::Error) {
    let interrupted = error.chain().any(|cause| {
        cause.is::<Interrupted>() || matches!(cause.downcast_ref::<ProcessError>(), Some(ProcessError::Interrupted))
    });
    if interrupted {
        output::error("interrupted by user");
    } else {
        output::error(format!("{error:#}"));
    }
}

fn run() -> Result<u8> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return Ok(if error.use_stderr() { 2 } else { 0 });
        }
    };
    let colors = console::enable_colors();
    let program_dir = program_dir()?;

    // Analysis does not touch the system: it needs neither elevation nor a graphics card.
    if let Some(dir) = &cli.analyze {
        analysis::analyze(&resolve_user_path(dir, &program_dir), colors)?;
        return Ok(0);
    }

    if !system::is_elevated()? {
        bail!("administrator privileges required");
    }
    if let Err(error) = process::kill_children_on_exit() {
        output::warn(format!(
            "{}, child processes may outlive AutoGpuAffinity",
            output::error_chain(error)
        ));
    }
    check_basic_display()?;

    let devices = DeviceManager::connect()?;
    let backup_path = program_dir.join(affinity::BACKUP_FILE_NAME);
    affinity::recover_previous_session(&devices, &backup_path)?;

    let cards = graphics_cards(&devices)?;
    let cpu_count = usable_cpu_count();

    if let Some(index) = cli.apply_affinity {
        let cpu = Cpu::new(index).filter(|cpu| cpu.index() < cpu_count).with_context(|| {
            format!(
                "invalid affinity specified: CPU {index} (valid range is 0 to {})",
                cpu_count.saturating_sub(1)
            )
        })?;
        affinity::apply(&devices, &cards, cpu)?;
        output::info(format!("set gpu driver affinity to: CPU {cpu}"));
        return Ok(0);
    }

    let config_path = match &cli.config {
        Some(path) => resolve_user_path(path, &program_dir),
        None => program_dir.join("config.ini"),
    };
    let config = load_config(&config_path)?;

    let env = benchmark::Environment {
        program_dir: &program_dir,
        devices: &devices,
        cards: &cards,
        cpu_count,
        colors,
        backup_path,
    };
    benchmark::run(&env, &config)?;
    Ok(0)
}

fn program_dir() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("unable to locate the executable")?;
    let dir = executable
        .parent()
        .context("unable to locate the program directory")?
        .to_path_buf();

    // `cargo run` executes from target\, while the resources live in the repository.
    #[cfg(debug_assertions)]
    if !dir.join("bin").is_dir() {
        let resources = Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\AutoGpuAffinity");
        if resources.join("bin").is_dir() {
            return Ok(resources);
        }
    }

    Ok(dir)
}

/// Falls back to the program directory, the working directory of the Python version.
fn resolve_user_path(path: &Path, program_dir: &Path) -> PathBuf {
    if path.is_absolute() || path.exists() {
        return path.to_path_buf();
    }
    let fallback = program_dir.join(path);
    if fallback.exists() {
        fallback
    } else {
        path.to_path_buf()
    }
}

fn load_config(path: &Path) -> Result<Config> {
    let bytes = std::fs::read(path).with_context(|| format!("unable to read config file {}", path.display()))?;
    let (config, warnings) =
        Config::parse(&decode_text(bytes)).with_context(|| format!("invalid config file {}", path.display()))?;
    for warning in warnings {
        output::warn(format!("{}: {warning}", path.display()));
    }
    if config.afterburner_profile > 0 && !config.afterburner_location.is_file() {
        bail!(
            "invalid MSI Afterburner path specified: {}",
            config.afterburner_location.display()
        );
    }
    if config.xperf_enabled && !config.xperf_location.is_file() {
        bail!("invalid xperf path specified: {}", config.xperf_location.display());
    }
    Ok(config)
}

/// The BasicDisplay driver is the fallback while the graphics driver restarts.
fn check_basic_display() -> Result<()> {
    match registry::basic_display_start_type()? {
        None => bail!("unable to get BasicDisplay start type"),
        Some(SERVICE_DISABLED) => {
            bail!("enable the BasicDisplay driver to prevent issues with restarting the GPU driver")
        }
        Some(_) => Ok(()),
    }
}

fn graphics_cards(devices: &DeviceManager) -> Result<Vec<GraphicsCard>> {
    let (cards, skipped) = device::select_cards(devices.display_adapters()?);
    for (card, reason) in skipped {
        output::info(format!("ignoring {}: {reason}", card.name));
    }
    if cards.is_empty() {
        bail!("no graphics cards found");
    }
    Ok(cards)
}

fn usable_cpu_count() -> u32 {
    let usable = system::usable_cpu_count();
    if system::logical_cpu_count().is_some_and(|logical| logical > usable as usize) {
        output::warn(format!(
            "only the first {usable} CPUs (processor group 0) can be benchmarked"
        ));
    }
    usable
}
