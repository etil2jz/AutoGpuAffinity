use std::fmt::Write as _;

use thiserror::Error;

use crate::cpu::Cpu;
use crate::stats::FrameStats;

pub const METRIC_COUNT: usize = 12;
pub const METRIC_NAMES: [&str; METRIC_COUNT] = [
    "Max",
    "Avg",
    "Min",
    "STDEV",
    "1 %ile",
    "0.1 %ile",
    "0.01 %ile",
    "0.005 %ile",
    "1% Low",
    "0.1% Low",
    "0.01% Low",
    "0.005% Low",
];
const CPU_COLUMN_WIDTH: usize = 5;
const METRIC_COLUMN_WIDTH: usize = 12;
pub const TABLE_WIDTH: usize = CPU_COLUMN_WIDTH + METRIC_COLUMN_WIDTH * METRIC_COUNT;

/// The only metric where a lower value is better.
const STDEV: usize = 3;
const PERCENTS: [f64; 4] = [1.0, 0.1, 0.01, 0.005];
/// Green for the best value, yellow for the second best.
const RANK_COLORS: [&str; 2] = ["\x1b[92m", "\x1b[93m"];
const RESET: &str = "\x1b[0m";

pub type Metrics = [f64; METRIC_COUNT];
pub type Ranks = [Option<usize>; METRIC_COUNT];

#[derive(Debug, Clone, PartialEq)]
pub struct CpuResult {
    pub cpu: Cpu,
    pub metrics: Metrics,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CaptureError {
    #[error("column msBetweenPresents not found")]
    MissingFrametimeColumn,
    #[error("malformed record at line {line}")]
    MalformedRecord { line: u64 },
    #[error("no frame recorded")]
    NoFrames,
}

pub fn capture_file_name(cpu: Cpu) -> String {
    format!("CPU-{cpu}.csv")
}

pub fn capture_cpu(file_name: &str) -> Option<Cpu> {
    let name = file_name.to_ascii_lowercase();
    let index = name.strip_prefix("cpu-")?.strip_suffix(".csv")?.parse().ok()?;
    Cpu::new(index)
}

/// The column name changed case across PresentMon versions; non-numeric values are skipped.
pub fn parse_frametimes(csv: &[u8]) -> Result<Vec<f64>, CaptureError> {
    let malformed = |error: csv::Error| CaptureError::MalformedRecord {
        line: error.position().map_or(0, csv::Position::line),
    };
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(csv);
    let column = reader
        .headers()
        .map_err(malformed)?
        .iter()
        .position(|header| header.eq_ignore_ascii_case("msBetweenPresents"))
        .ok_or(CaptureError::MissingFrametimeColumn)?;

    let mut frametimes = Vec::new();
    for record in reader.records() {
        if let Some(value) = record
            .map_err(malformed)?
            .get(column)
            .and_then(|value| value.parse().ok())
        {
            frametimes.push(value);
        }
    }
    Ok(frametimes)
}

pub fn metrics(stats: &FrameStats) -> Metrics {
    let [p1, p01, p001, p0005] = PERCENTS.map(|percent| stats.percentile(percent));
    let [l1, l01, l001, l0005] = PERCENTS.map(|percent| stats.low(percent));
    [
        stats.maximum(),
        stats.average(),
        stats.minimum(),
        stats.stdev(),
        p1,
        p01,
        p001,
        p0005,
        l1,
        l01,
        l001,
        l0005,
    ]
}

pub fn analyze_capture(csv: &[u8]) -> Result<Metrics, CaptureError> {
    let frametimes = parse_frametimes(csv)?;
    FrameStats::new(&frametimes)
        .map(|stats| metrics(&stats))
        .ok_or(CaptureError::NoFrames)
}

/// Rounded like the displayed value so that the ranking matches what is visible.
fn hundredths(value: f64) -> i64 {
    format!("{value:.2}").replace('.', "").parse().unwrap_or(0)
}

fn ranking_keys(metrics: &Metrics) -> [i64; METRIC_COUNT] {
    let mut keys = metrics.map(hundredths);
    if let Some(stdev) = keys.get_mut(STDEV) {
        *stdev = -*stdev;
    }
    keys
}

/// Last place is never highlighted, as in the Python version.
pub fn rank(results: &[CpuResult]) -> Vec<Ranks> {
    let highlighted = results.len().saturating_sub(1).min(RANK_COLORS.len());
    let keys: Vec<[i64; METRIC_COUNT]> = results.iter().map(|result| ranking_keys(&result.metrics)).collect();
    let best: [Vec<i64>; METRIC_COUNT] = std::array::from_fn(|metric| {
        let mut column: Vec<i64> = keys.iter().filter_map(|row| row.get(metric).copied()).collect();
        column.sort_unstable_by(|a, b| b.cmp(a));
        column.dedup();
        column.truncate(highlighted);
        column
    });
    keys.iter()
        .map(|row| {
            std::array::from_fn(|metric| {
                let key = row.get(metric)?;
                best.get(metric)?.iter().position(|candidate| candidate == key)
            })
        })
        .collect()
}

pub fn render_table(results: &[CpuResult], ranks: &[Ranks], colors: bool) -> String {
    let mut table = format!("{:<CPU_COLUMN_WIDTH$}", "CPU");
    for name in METRIC_NAMES {
        let _ = write!(table, "{name:<METRIC_COLUMN_WIDTH$}");
    }
    table.push('\n');

    for (result, row_ranks) in results.iter().zip(ranks) {
        let _ = write!(table, "{:<CPU_COLUMN_WIDTH$}", result.cpu);
        for (value, rank) in result.metrics.iter().zip(row_ranks) {
            let text = format!("{:.2}", value.abs());
            let padding = METRIC_COLUMN_WIDTH.saturating_sub(text.len());
            match rank.and_then(|rank| RANK_COLORS.get(rank)).filter(|_| colors) {
                Some(color) => {
                    let _ = write!(table, "{color}{text}{RESET}{:padding$}", "");
                }
                None => {
                    let _ = write!(table, "{text}{:padding$}", "");
                }
            }
        }
        table.push('\n');
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(cpu: u32, max: f64, stdev: f64) -> CpuResult {
        let mut metrics = [10.0; METRIC_COUNT];
        metrics[0] = max;
        metrics[STDEV] = stdev;
        CpuResult {
            cpu: Cpu::new(cpu).unwrap(),
            metrics,
        }
    }

    #[test]
    fn reads_both_presentmon_column_spellings() {
        let legacy = b"Application,ProcessID,msBetweenPresents\nlava.exe,1,16.6\nlava.exe,1,8.3\n";
        assert_eq!(parse_frametimes(legacy), Ok(vec![16.6, 8.3]));

        let modern = b"Application,MsBetweenPresents\nlava.exe,4.0\nlava.exe,NA\nlava.exe,\nlava.exe,5.0\n";
        assert_eq!(parse_frametimes(modern), Ok(vec![4.0, 5.0]));

        assert_eq!(
            parse_frametimes(b"Application,Other\nx,1\n"),
            Err(CaptureError::MissingFrametimeColumn)
        );
        assert_eq!(
            analyze_capture(b"Application,msBetweenPresents\n"),
            Err(CaptureError::NoFrames)
        );
    }

    #[test]
    fn capture_file_names_round_trip() {
        let cpu = Cpu::new(12).unwrap();
        assert_eq!(capture_cpu(&capture_file_name(cpu)), Some(cpu));
        assert_eq!(capture_cpu("cpu-0.CSV"), Cpu::new(0));
        for name in ["desktop.ini", "CPU-x.csv", "notes.csv", "CPU-64.csv"] {
            assert_eq!(capture_cpu(name), None, "{name}");
        }
    }

    #[test]
    fn ranking_highlights_the_two_best_values() {
        let results = [
            result(0, 100.0, 5.0),
            result(1, 300.0, 1.0),
            result(2, 200.0, 3.0),
            result(3, 300.004, 9.0),
        ];
        let ranks = rank(&results);
        // 300.00 is shared by CPU 1 and 3 once rounded.
        assert_eq!(
            ranks.iter().map(|r| r[0]).collect::<Vec<_>>(),
            [None, Some(0), Some(1), Some(0)]
        );
        assert_eq!(
            ranks.iter().map(|r| r[STDEV]).collect::<Vec<_>>(),
            [None, Some(0), Some(1), None]
        );
        assert!(ranks.iter().all(|r| r[1] == Some(0)));
    }

    #[test]
    fn last_place_is_never_highlighted() {
        assert_eq!(rank(&[result(0, 1.0, 1.0)])[0], [None; METRIC_COUNT]);
        let ranks = rank(&[result(0, 1.0, 1.0), result(1, 2.0, 2.0)]);
        assert_eq!((ranks[0][0], ranks[1][0]), (None, Some(0)));
        assert_eq!((ranks[0][STDEV], ranks[1][STDEV]), (Some(0), None));
    }

    #[test]
    fn table_layout_is_stable() {
        let results = [result(0, 100.0, 5.0), result(1, 300.0, 1.0)];
        let plain = render_table(&results, &rank(&results), false);
        let lines: Vec<&str> = plain.lines().collect();
        assert!(lines[0].starts_with("CPU  Max         Avg         Min         STDEV       1 %ile"));
        assert!(lines[1].starts_with("0    100.00      10.00       10.00       5.00        10.00"));
        assert_eq!(lines[1].len(), TABLE_WIDTH);

        let colored = render_table(&results, &rank(&results), true);
        assert!(colored.contains("\x1b[92m300.00\x1b[0m      "));
    }
}
