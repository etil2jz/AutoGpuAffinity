use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use autogpuaffinity_domain::cpu::Cpu;
use autogpuaffinity_domain::device::GraphicsCard;
use autogpuaffinity_domain::ini::decode_text;
use autogpuaffinity_domain::policy::{AffinityPolicy, BackupEntry, PolicyBackup};
use autogpuaffinity_windows::devices::{DeviceError, DeviceManager};
use autogpuaffinity_windows::registry;

use crate::output;

pub const BACKUP_FILE_NAME: &str = "affinity-backup.ini";

pub fn apply(devices: &DeviceManager, cards: &[GraphicsCard], cpu: Cpu) -> Result<()> {
    let policy = AffinityPolicy::pinned_to(cpu);
    for card in cards {
        registry::write_policy(&card.instance_id, &policy)?;
        devices.restart(card)?;
    }
    Ok(())
}

fn capture(cards: &[GraphicsCard]) -> Result<PolicyBackup> {
    let entries = cards
        .iter()
        .map(|card| {
            Ok(BackupEntry {
                card: card.clone(),
                policy: registry::read_policy(&card.instance_id)?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(PolicyBackup { entries })
}

/// Atomic and flushed to disk: the backup must survive a system crash during the benchmark.
fn save(backup: &PolicyBackup, path: &Path) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(backup.serialize().as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    };
    write().with_context(|| format!("unable to write the affinity backup {}", path.display()))
}

fn load(path: &Path) -> Result<PolicyBackup> {
    let bytes = fs::read(path).with_context(|| format!("unable to read {}", path.display()))?;
    PolicyBackup::parse(&decode_text(bytes)).with_context(|| format!("invalid affinity backup {}", path.display()))
}

/// Keeps going after a failure to restore as much as possible.
fn restore(devices: &DeviceManager, backup: &PolicyBackup) -> Result<()> {
    let mut failures = Vec::new();
    // Registry first: it is fast, and a reboot is enough if the process dies while the drivers reload.
    for entry in &backup.entries {
        if let Err(error) = registry::write_policy(&entry.card.instance_id, &entry.policy) {
            failures.push(output::error_chain(error));
        }
    }
    for entry in &backup.entries {
        match devices.reload(&entry.card) {
            Ok(()) => {}
            // Reporting these as failures would retry the restoration on every launch.
            Err(error @ DeviceError::RebootRequired(_)) => {
                output::warn(format!("{error}, reboot to apply the restored affinity policy"));
            }
            Err(error @ DeviceError::Absent(_)) => output::warn(error),
            Err(error) => failures.push(output::error_chain(error)),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!(failures.join("; "))
    }
}

fn restore_and_forget(devices: &DeviceManager, backup: &PolicyBackup, path: &Path) -> Result<()> {
    restore(devices, backup).with_context(|| {
        format!(
            "unable to restore the original affinity policy, it will be retried on next launch (backup kept in {})",
            path.display()
        )
    })?;
    if let Err(error) = fs::remove_file(path) {
        output::warn(format!("unable to delete {}: {error}", path.display()));
    }
    Ok(())
}

/// Restores the policy left by a session that did not exit cleanly (killed, driver or system crash...).
pub fn recover_previous_session(devices: &DeviceManager, path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    output::warn("a previous benchmark did not exit cleanly, restoring the original affinity policy");
    restore_and_forget(devices, &load(path)?, path)?;
    output::info("original affinity policy restored");
    Ok(())
}

/// Brings the original policy back explicitly with [`AffinityGuard::restore`], or on drop during a panic.
pub struct AffinityGuard<'a> {
    devices: &'a DeviceManager,
    backup: PolicyBackup,
    path: PathBuf,
    armed: bool,
}

impl<'a> AffinityGuard<'a> {
    pub fn arm(devices: &'a DeviceManager, cards: &[GraphicsCard], path: PathBuf) -> Result<Self> {
        let backup = capture(cards)?;
        save(&backup, &path)?;
        Ok(Self {
            devices,
            backup,
            path,
            armed: true,
        })
    }

    pub fn restore(mut self) -> Result<()> {
        self.armed = false;
        output::info("restoring the original affinity policy");
        restore_and_forget(self.devices, &self.backup, &self.path)
    }
}

impl Drop for AffinityGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            output::warn("restoring the original affinity policy");
            if let Err(error) = restore_and_forget(self.devices, &self.backup, &self.path) {
                output::error(format!("{error:#}"));
            }
        }
    }
}
