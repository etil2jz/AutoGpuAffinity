use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

use crate::cpu::{Cpu, CpuListError, parse_cpu_list};
use crate::ini::{Entry, Ini, IniError};

const MAX_AFTERBURNER_PROFILE: u32 = 5;

const KNOWN_KEYS: &[(&str, &[&str])] = &[
    (
        "settings",
        &[
            "cache_duration",
            "benchmark_duration",
            "custom_cpus",
            "api",
            "sync_driver_affinity",
            "skip_confirmation",
        ],
    ),
    ("MSI Afterburner", &["profile", "location"]),
    ("xperf", &["enabled", "location", "save_etls"]),
    (
        "liblava",
        &[
            "fullscreen",
            "x_resolution",
            "y_resolution",
            "fps_cap",
            "triple_buffering",
        ],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    Liblava,
    D3D9,
}

impl Api {
    pub fn binary_path(self) -> &'static str {
        match self {
            Self::Liblava => r"bin\liblava\lava-triangle.exe",
            Self::D3D9 => r"bin\D3D9-benchmark.exe",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Liblava {
    pub fullscreen: bool,
    pub x_resolution: u32,
    pub y_resolution: u32,
    pub fps_cap: u32,
    pub triple_buffering: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub cache_duration: u32,
    pub benchmark_duration: u32,
    pub custom_cpus: Vec<Cpu>,
    pub api: Api,
    pub sync_driver_affinity: bool,
    pub skip_confirmation: bool,
    pub afterburner_profile: u32,
    pub afterburner_location: PathBuf,
    pub xperf_enabled: bool,
    pub xperf_location: PathBuf,
    pub xperf_save_etls: bool,
    pub liblava: Liblava,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error(transparent)]
    Syntax(#[from] IniError),
    #[error("missing section [{0}]")]
    MissingSection(String),
    #[error("missing `{key}` in section [{section}]")]
    MissingKey { section: String, key: String },
    #[error("line {line}: `{key}` must be {expected}, found `{found}`")]
    InvalidValue {
        line: usize,
        key: String,
        expected: &'static str,
        found: String,
    },
    #[error("line {line}: invalid `{key}`")]
    InvalidCpuList {
        line: usize,
        key: String,
        #[source]
        source: CpuListError,
    },
    #[error("`benchmark_duration` must be greater than 0")]
    ZeroBenchmarkDuration,
    #[error("MSI Afterburner `profile` must be between 0 and {MAX_AFTERBURNER_PROFILE}")]
    AfterburnerProfileOutOfRange,
    #[error("liblava resolution must be greater than 0 in windowed mode")]
    ZeroResolution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarning {
    UnknownSection { line: usize, name: String },
    UnknownKey { line: usize, key: String, section: String },
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSection { line, name } => write!(f, "line {line}: unknown section [{name}] ignored"),
            Self::UnknownKey { line, key, section } => {
                write!(f, "line {line}: unknown key `{key}` in section [{section}] ignored")
            }
        }
    }
}

struct Reader<'a> {
    ini: &'a Ini,
}

impl Reader<'_> {
    fn entry(&self, section: &str, key: &str) -> Result<&Entry, ConfigError> {
        self.ini
            .section(section)
            .ok_or_else(|| ConfigError::MissingSection(section.to_owned()))?
            .get(key)
            .ok_or_else(|| ConfigError::MissingKey {
                section: section.to_owned(),
                key: key.to_owned(),
            })
    }

    fn invalid(entry: &Entry, expected: &'static str) -> ConfigError {
        ConfigError::InvalidValue {
            line: entry.line,
            key: entry.key.clone(),
            expected,
            found: entry.value.clone(),
        }
    }

    fn u32(&self, section: &str, key: &str) -> Result<u32, ConfigError> {
        let entry = self.entry(section, key)?;
        entry
            .value
            .parse()
            .map_err(|_| Self::invalid(entry, "a non-negative integer"))
    }

    fn bool(&self, section: &str, key: &str) -> Result<bool, ConfigError> {
        let entry = self.entry(section, key)?;
        match entry.value.to_ascii_lowercase().as_str() {
            "1" | "yes" | "true" | "on" => Ok(true),
            "0" | "no" | "false" | "off" => Ok(false),
            _ => Err(Self::invalid(entry, "true or false")),
        }
    }

    fn path(&self, section: &str, key: &str) -> Result<PathBuf, ConfigError> {
        let value = &self.entry(section, key)?.value;
        // Quotes are tolerated even though the file asks to omit them.
        let unquoted = value
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(value);
        Ok(PathBuf::from(unquoted.trim()))
    }

    fn cpus(&self, section: &str, key: &str) -> Result<Vec<Cpu>, ConfigError> {
        let entry = self.entry(section, key)?;
        parse_cpu_list(&entry.value).map_err(|source| ConfigError::InvalidCpuList {
            line: entry.line,
            key: entry.key.clone(),
            source,
        })
    }

    fn api(&self) -> Result<Api, ConfigError> {
        let entry = self.entry("settings", "api")?;
        match entry.value.as_str() {
            "1" => Ok(Api::Liblava),
            "2" => Ok(Api::D3D9),
            _ => Err(Self::invalid(entry, "1 (liblava) or 2 (D3D9)")),
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<(Self, Vec<ConfigWarning>), ConfigError> {
        let ini = Ini::parse(text)?;
        let reader = Reader { ini: &ini };

        let config = Self {
            cache_duration: reader.u32("settings", "cache_duration")?,
            benchmark_duration: reader.u32("settings", "benchmark_duration")?,
            custom_cpus: reader.cpus("settings", "custom_cpus")?,
            api: reader.api()?,
            sync_driver_affinity: reader.bool("settings", "sync_driver_affinity")?,
            skip_confirmation: reader.bool("settings", "skip_confirmation")?,
            afterburner_profile: reader.u32("MSI Afterburner", "profile")?,
            afterburner_location: reader.path("MSI Afterburner", "location")?,
            xperf_enabled: reader.bool("xperf", "enabled")?,
            xperf_location: reader.path("xperf", "location")?,
            xperf_save_etls: reader.bool("xperf", "save_etls")?,
            liblava: Liblava {
                fullscreen: reader.bool("liblava", "fullscreen")?,
                x_resolution: reader.u32("liblava", "x_resolution")?,
                y_resolution: reader.u32("liblava", "y_resolution")?,
                fps_cap: reader.u32("liblava", "fps_cap")?,
                triple_buffering: reader.bool("liblava", "triple_buffering")?,
            },
        };

        if config.benchmark_duration == 0 {
            return Err(ConfigError::ZeroBenchmarkDuration);
        }
        if config.afterburner_profile > MAX_AFTERBURNER_PROFILE {
            return Err(ConfigError::AfterburnerProfileOutOfRange);
        }
        let lava = &config.liblava;
        if config.api == Api::Liblava && !lava.fullscreen && (lava.x_resolution == 0 || lava.y_resolution == 0) {
            return Err(ConfigError::ZeroResolution);
        }

        Ok((config, unknown_keys(&ini)))
    }

    pub fn subject_args(&self) -> Vec<String> {
        match self.api {
            Api::Liblava => {
                let lava = &self.liblava;
                vec![
                    format!("--fullscreen={}", u8::from(lava.fullscreen)),
                    format!("--width={}", lava.x_resolution),
                    format!("--height={}", lava.y_resolution),
                    format!("--fps_cap={}", lava.fps_cap),
                    format!("--triple_buffering={}", u8::from(lava.triple_buffering)),
                ]
            }
            Api::D3D9 => Vec::new(),
        }
    }
}

fn unknown_keys(ini: &Ini) -> Vec<ConfigWarning> {
    let mut warnings = Vec::new();
    for section in &ini.sections {
        match KNOWN_KEYS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&section.name))
        {
            None => warnings.push(ConfigWarning::UnknownSection {
                line: section.line,
                name: section.name.clone(),
            }),
            Some((_, keys)) => warnings.extend(
                section
                    .entries
                    .iter()
                    .filter(|entry| !keys.contains(&entry.key.as_str()))
                    .map(|entry| ConfigWarning::UnknownKey {
                        line: entry.line,
                        key: entry.key.clone(),
                        section: section.name.clone(),
                    }),
            ),
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED_CONFIG: &str = include_str!("../../../AutoGpuAffinity/config.ini");

    #[test]
    fn shipped_config_is_valid() {
        let (config, warnings) = Config::parse(SHIPPED_CONFIG).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(config.cache_duration, 5);
        assert_eq!(config.benchmark_duration, 30);
        assert!(config.custom_cpus.is_empty());
        assert_eq!(config.api, Api::Liblava);
        assert!(config.sync_driver_affinity);
        assert_eq!(config.afterburner_profile, 0);
        assert_eq!(
            config.xperf_location,
            PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\Windows Performance Toolkit\xperf.exe")
        );
        assert_eq!(config.liblava.x_resolution, 640);
    }

    #[test]
    fn liblava_arguments_match_the_legacy_command_line() {
        let (config, _) = Config::parse(SHIPPED_CONFIG).unwrap();
        assert_eq!(
            config.subject_args(),
            [
                "--fullscreen=1",
                "--width=640",
                "--height=480",
                "--fps_cap=0",
                "--triple_buffering=0"
            ]
        );
    }

    #[test]
    fn reports_missing_keys_and_bad_values() {
        let parse = |from: &str, to: &str| Config::parse(&SHIPPED_CONFIG.replace(from, to)).map(|_| ());

        assert_eq!(
            parse("api=1", ""),
            Err(ConfigError::MissingKey {
                section: "settings".into(),
                key: "api".into()
            })
        );
        assert!(matches!(parse("api=1", "api=3"), Err(ConfigError::InvalidValue { key, .. }) if key == "api"));
        assert!(
            matches!(parse("fullscreen=true", "fullscreen=maybe"), Err(ConfigError::InvalidValue { key, .. }) if key == "fullscreen")
        );
        assert!(matches!(
            parse("cache_duration=5", "cache_duration=-5"),
            Err(ConfigError::InvalidValue { .. })
        ));
        assert!(matches!(
            parse("custom_cpus=[]", "custom_cpus=[99]"),
            Err(ConfigError::InvalidCpuList {
                source: CpuListError::OutOfRange(99),
                ..
            })
        ));
        assert_eq!(
            parse("benchmark_duration=30", "benchmark_duration=0"),
            Err(ConfigError::ZeroBenchmarkDuration)
        );
        assert_eq!(
            parse("profile=0", "profile=6"),
            Err(ConfigError::AfterburnerProfileOutOfRange)
        );
    }

    #[test]
    fn warns_about_typos() {
        let (_, warnings) =
            Config::parse(&SHIPPED_CONFIG.replace("custom_cpus=[]", "custom_cpus=[]\ncustom_cores=[1]")).unwrap();
        assert!(matches!(warnings.as_slice(), [ConfigWarning::UnknownKey { key, .. }] if key == "custom_cores"));
    }

    #[test]
    fn paths_may_be_quoted() {
        let path = r"C:\Program Files (x86)\MSI Afterburner\MSIAfterburner.exe";
        let quoted = SHIPPED_CONFIG.replace(&format!("location={path}"), &format!("location=\"{path}\""));
        let (config, _) = Config::parse(&quoted).unwrap();
        assert_eq!(config.afterburner_location, PathBuf::from(path));
    }
}
