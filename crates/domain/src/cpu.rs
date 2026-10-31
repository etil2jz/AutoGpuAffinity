use std::fmt;

use thiserror::Error;

/// Processor addressable by an interrupt affinity mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cpu(u8);

impl Cpu {
    /// A KAFFINITY mask only covers one group of 64 processors.
    pub const LIMIT: u32 = 64;

    pub fn new(index: u32) -> Option<Self> {
        if index < Self::LIMIT {
            u8::try_from(index).ok().map(Self)
        } else {
            None
        }
    }

    pub fn index(self) -> u32 {
        u32::from(self.0)
    }

    pub fn mask(self) -> u64 {
        1 << self.0
    }
}

impl fmt::Display for Cpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

pub fn join_cpus(cpus: &[Cpu]) -> String {
    cpus.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
}

/// Addressable CPUs among the first `available` ones.
pub fn all_cpus(available: u32) -> Vec<Cpu> {
    (0..available.min(Cpu::LIMIT)).filter_map(Cpu::new).collect()
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CpuListError {
    #[error("expected an array such as [0..7, 9], found `{0}`")]
    NotAnArray(String),
    #[error("`{0}` is not a CPU number")]
    NotANumber(String),
    #[error("CPU {0} is out of range, interrupt affinity only supports CPUs 0 to 63")]
    OutOfRange(u32),
    #[error("invalid range `{0}`, the lower bound must not exceed the upper bound")]
    ReversedRange(String),
}

/// Accepts `[0..7, 9, 11..13]`; the result is sorted and deduplicated.
pub fn parse_cpu_list(text: &str) -> Result<Vec<Cpu>, CpuListError> {
    let inner = text
        .trim()
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(|| CpuListError::NotAnArray(text.to_owned()))?;

    let mut cpus = Vec::new();
    for item in inner.split(',').map(str::trim).filter(|item| !item.is_empty()) {
        match item.split_once("..") {
            Some((lower, upper)) => {
                let (lower, upper) = (parse_cpu(lower)?, parse_cpu(upper)?);
                if lower > upper {
                    return Err(CpuListError::ReversedRange(item.to_owned()));
                }
                cpus.extend((lower.index()..=upper.index()).filter_map(Cpu::new));
            }
            None => cpus.push(parse_cpu(item)?),
        }
    }
    cpus.sort_unstable();
    cpus.dedup();
    Ok(cpus)
}

fn parse_cpu(text: &str) -> Result<Cpu, CpuListError> {
    let text = text.trim();
    let index: u32 = text.parse().map_err(|_| CpuListError::NotANumber(text.to_owned()))?;
    Cpu::new(index).ok_or(CpuListError::OutOfRange(index))
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid CPUs in custom_cpus: {} (this system has CPUs 0 to {})", join_cpus(.cpus), .available.saturating_sub(1))]
pub struct UnavailableCpus {
    pub cpus: Vec<Cpu>,
    pub available: u32,
}

/// An empty configured list means every CPU.
pub fn select_cpus(custom: &[Cpu], available: u32) -> Result<Vec<Cpu>, UnavailableCpus> {
    if custom.is_empty() {
        return Ok(all_cpus(available));
    }
    let unavailable: Vec<Cpu> = custom.iter().copied().filter(|cpu| cpu.index() >= available).collect();
    if unavailable.is_empty() {
        Ok(custom.to_vec())
    } else {
        Err(UnavailableCpus {
            cpus: unavailable,
            available,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpus(indices: &[u32]) -> Vec<Cpu> {
        indices.iter().map(|&index| Cpu::new(index).unwrap()).collect()
    }

    #[test]
    fn cpu_indices_are_bounded_by_the_mask_width() {
        assert_eq!(Cpu::new(63).map(Cpu::mask), Some(1 << 63));
        assert_eq!(Cpu::new(64), None);
        assert_eq!(all_cpus(200).len(), 64);
    }

    #[test]
    fn cpu_lists_accept_ranges_and_combinations() {
        assert_eq!(parse_cpu_list("[]").unwrap(), cpus(&[]));
        assert_eq!(parse_cpu_list("[ ]").unwrap(), cpus(&[]));
        assert_eq!(parse_cpu_list("[6,1,3]").unwrap(), cpus(&[1, 3, 6]));
        assert_eq!(
            parse_cpu_list("[0..3, 9, 11..13]").unwrap(),
            cpus(&[0, 1, 2, 3, 9, 11, 12, 13])
        );
        assert_eq!(parse_cpu_list("[2, 1..3, 2,]").unwrap(), cpus(&[1, 2, 3]));
    }

    #[test]
    fn cpu_lists_reject_invalid_input() {
        assert_eq!(parse_cpu_list("0,1"), Err(CpuListError::NotAnArray("0,1".into())));
        assert_eq!(parse_cpu_list("[a]"), Err(CpuListError::NotANumber("a".into())));
        assert_eq!(
            parse_cpu_list("[3..1]"),
            Err(CpuListError::ReversedRange("3..1".into()))
        );
        assert_eq!(parse_cpu_list("[64]"), Err(CpuListError::OutOfRange(64)));
        assert_eq!(parse_cpu_list("[0..100]"), Err(CpuListError::OutOfRange(100)));
        assert!(parse_cpu_list("[-1]").is_err());
        assert!(parse_cpu_list("[1..]").is_err());
    }

    #[test]
    fn all_cpus_are_selected_by_default() {
        assert_eq!(select_cpus(&[], 4).unwrap(), cpus(&[0, 1, 2, 3]));
        assert_eq!(select_cpus(&cpus(&[1, 3]), 4).unwrap(), cpus(&[1, 3]));
        let error = select_cpus(&cpus(&[1, 4, 7]), 4).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid CPUs in custom_cpus: 4,7 (this system has CPUs 0 to 3)"
        );
    }
}
