use std::time::Duration;

use crate::config::Config;

/// Lets the driver settle after it has been restarted.
pub const SETTLE_DELAY: Duration = Duration::from_secs(5);
/// Lets the subject start before the cache phase.
pub const LAUNCH_DELAY: Duration = Duration::from_secs(5);
pub const AFTERBURNER_DELAY: Duration = Duration::from_secs(5);
const DRIVER_RESTART_ESTIMATE: Duration = Duration::from_secs(5);

/// Includes the final driver restart that restores the original policy.
pub fn estimate(config: &Config, cpu_count: usize, card_count: usize) -> Duration {
    let restarts = DRIVER_RESTART_ESTIMATE.saturating_mul(u32::try_from(card_count).unwrap_or(u32::MAX));
    let afterburner = if config.afterburner_profile > 0 {
        AFTERBURNER_DELAY
    } else {
        Duration::ZERO
    };
    let per_cpu = restarts
        .saturating_add(SETTLE_DELAY)
        .saturating_add(LAUNCH_DELAY)
        .saturating_add(afterburner)
        .saturating_add(Duration::from_secs(config.cache_duration.into()))
        .saturating_add(Duration::from_secs(config.benchmark_duration.into()));
    per_cpu
        .saturating_mul(u32::try_from(cpu_count).unwrap_or(u32::MAX))
        .saturating_add(restarts)
}

/// `H:MM:SS`, like Python's `timedelta`.
pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}:{:02}:{:02}", seconds / 3600, seconds % 3600 / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_formatted_like_timedelta() {
        assert_eq!(format_duration(Duration::from_secs(350)), "0:05:50");
        assert_eq!(format_duration(Duration::from_secs(3 * 3600 + 7)), "3:00:07");
    }

    #[test]
    fn estimate_accounts_for_every_step() {
        let (config, _) = Config::parse(include_str!("../../../AutoGpuAffinity/config.ini")).unwrap();
        // Per CPU: 5 s restart + 5 s settle + 5 s launch + 5 s cache + 30 s benchmark, plus one final restart.
        assert_eq!(estimate(&config, 2, 1), Duration::from_secs(2 * 50 + 5));
    }
}
