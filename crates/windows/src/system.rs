use std::io;
use std::num::NonZero;
use std::thread;

use autogpuaffinity_domain::cpu::Cpu;
use sysinfo::{CpuRefreshKind, RefreshKind, System};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("unable to query the process elevation")]
pub struct ElevationError(#[source] io::Error);

pub fn is_elevated() -> Result<bool, ElevationError> {
    check_elevation::is_elevated().map_err(|code| ElevationError(io::Error::from_raw_os_error(code.cast_signed())))
}

/// CPUs of the current processor group, the only ones an affinity mask can address.
pub fn usable_cpu_count() -> u32 {
    let count = thread::available_parallelism().map_or(1, NonZero::get);
    u32::try_from(count).unwrap_or(u32::MAX).min(Cpu::LIMIT)
}

/// Logical processors across every processor group.
pub fn logical_cpu_count() -> Option<usize> {
    System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing()))
        .ok()
        .map(|system| system.cpus().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_counts_are_consistent() {
        let usable = usable_cpu_count();
        assert!((1..=Cpu::LIMIT).contains(&usable));
        assert!(logical_cpu_count().unwrap() >= usable as usize);
    }
}
