use std::fmt::Write as _;

use thiserror::Error;

use crate::cpu::Cpu;
use crate::device::GraphicsCard;
use crate::ini::{Ini, IniError};

pub const DEVICE_POLICY: &str = "DevicePolicy";
pub const ASSIGNMENT_SET_OVERRIDE: &str = "AssignmentSetOverride";
pub const REG_BINARY: u32 = 3;
pub const REG_DWORD: u32 = 4;

const IRQ_POLICY_SPECIFIED_PROCESSORS: u32 = 4;
const ABSENT: &str = "absent";
const BACKUP_HEADER: &str = "\
# AutoGpuAffinity - original interrupt affinity policy of the graphics cards.
# Written before benchmarking and deleted once the policy has been restored.
# If this file still exists, the next launch of AutoGpuAffinity restores the policy from it.
# Do not edit.
";

/// Raw registry value, kept as is so it can be restored exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawValue {
    pub kind: u32,
    pub data: Vec<u8>,
}

/// `None` means the value is absent from the registry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AffinityPolicy {
    pub device_policy: Option<RawValue>,
    pub assignment_set_override: Option<RawValue>,
}

impl AffinityPolicy {
    pub fn pinned_to(cpu: Cpu) -> Self {
        Self {
            device_policy: Some(RawValue {
                kind: REG_DWORD,
                data: IRQ_POLICY_SPECIFIED_PROCESSORS.to_le_bytes().to_vec(),
            }),
            assignment_set_override: Some(RawValue {
                kind: REG_BINARY,
                data: mask_bytes(cpu),
            }),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.device_policy.is_none() && self.assignment_set_override.is_none()
    }

    pub fn values(&self) -> [(&'static str, Option<&RawValue>); 2] {
        [
            (DEVICE_POLICY, self.device_policy.as_ref()),
            (ASSIGNMENT_SET_OVERRIDE, self.assignment_set_override.as_ref()),
        ]
    }
}

/// Legacy encoding: little-endian KAFFINITY without the trailing zero bytes.
pub fn mask_bytes(cpu: Cpu) -> Vec<u8> {
    let mut bytes = cpu.mask().to_le_bytes().to_vec();
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    bytes
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    pub card: GraphicsCard,
    pub policy: AffinityPolicy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyBackup {
    pub entries: Vec<BackupEntry>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BackupError {
    #[error(transparent)]
    Syntax(#[from] IniError),
    #[error("missing `{key}` for device {device}")]
    MissingValue { device: String, key: String },
    #[error("line {line}: invalid value `{value}`")]
    InvalidValue { line: usize, value: String },
}

impl PolicyBackup {
    pub fn serialize(&self) -> String {
        let mut text = BACKUP_HEADER.to_owned();
        for BackupEntry { card, policy } in &self.entries {
            let _ = write!(text, "\n[{}]\nname={}\n", card.instance_id, card.name);
            for (name, value) in policy.values() {
                let _ = writeln!(text, "{name}={}", encode_value(value));
            }
        }
        text
    }

    pub fn parse(text: &str) -> Result<Self, BackupError> {
        let ini = Ini::parse(text)?;
        let entries = ini
            .sections
            .iter()
            .map(|section| {
                let value = |key: &str| {
                    let entry = section.get(key).ok_or_else(|| BackupError::MissingValue {
                        device: section.name.clone(),
                        key: key.to_owned(),
                    })?;
                    decode_value(&entry.value).ok_or_else(|| BackupError::InvalidValue {
                        line: entry.line,
                        value: entry.value.clone(),
                    })
                };
                Ok(BackupEntry {
                    card: GraphicsCard {
                        instance_id: section.name.clone(),
                        name: section
                            .get("name")
                            .map_or_else(|| section.name.clone(), |entry| entry.value.clone()),
                    },
                    policy: AffinityPolicy {
                        device_policy: value(DEVICE_POLICY)?,
                        assignment_set_override: value(ASSIGNMENT_SET_OVERRIDE)?,
                    },
                })
            })
            .collect::<Result<_, BackupError>>()?;
        Ok(Self { entries })
    }
}

fn encode_value(value: Option<&RawValue>) -> String {
    match value {
        None => ABSENT.to_owned(),
        Some(RawValue { kind, data }) => data.iter().fold(format!("{kind}:"), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        }),
    }
}

/// `Some(None)` means an absent value, `None` an invalid text.
fn decode_value(text: &str) -> Option<Option<RawValue>> {
    if text == ABSENT {
        return Some(None);
    }
    let (kind, hex) = text.split_once(':')?;
    let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let data = pairs
        .iter()
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect::<Option<Vec<u8>>>()?;
    Some(Some(RawValue {
        kind: kind.parse().ok()?,
        data,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(index: u32) -> Cpu {
        Cpu::new(index).unwrap()
    }

    #[test]
    fn mask_bytes_match_the_legacy_encoding() {
        assert_eq!(mask_bytes(cpu(0)), [0x01]);
        assert_eq!(mask_bytes(cpu(7)), [0x80]);
        assert_eq!(mask_bytes(cpu(8)), [0x00, 0x01]);
        assert_eq!(mask_bytes(cpu(63)), [0, 0, 0, 0, 0, 0, 0, 0x80]);
    }

    #[test]
    fn pinned_policy_targets_specified_processors() {
        let policy = AffinityPolicy::pinned_to(cpu(8));
        assert_eq!(
            policy.device_policy,
            Some(RawValue {
                kind: REG_DWORD,
                data: vec![4, 0, 0, 0]
            })
        );
        assert_eq!(
            policy.assignment_set_override,
            Some(RawValue {
                kind: REG_BINARY,
                data: vec![0, 1]
            })
        );
    }

    #[test]
    fn backup_round_trips() {
        let backup = PolicyBackup {
            entries: vec![
                BackupEntry {
                    card: GraphicsCard {
                        instance_id: r"PCI\VEN_10DE&DEV_2684&SUBSYS_16F110DE&REV_A1\4&2C2E7A3A&0&0019".into(),
                        name: "NVIDIA GeForce RTX 4090".into(),
                    },
                    policy: AffinityPolicy::pinned_to(cpu(8)),
                },
                BackupEntry {
                    card: GraphicsCard {
                        instance_id: r"PCI\VEN_8086&DEV_A780\3&1&0&10".into(),
                        name: "Intel".into(),
                    },
                    policy: AffinityPolicy {
                        device_policy: None,
                        assignment_set_override: Some(RawValue {
                            kind: REG_BINARY,
                            data: Vec::new(),
                        }),
                    },
                },
            ],
        };
        assert_eq!(PolicyBackup::parse(&backup.serialize()), Ok(backup));
    }

    #[test]
    fn corrupted_backups_are_rejected() {
        assert_eq!(
            PolicyBackup::parse("[PCI\\X]\nDevicePolicy=4:0\nAssignmentSetOverride=absent\n"),
            Err(BackupError::InvalidValue {
                line: 2,
                value: "4:0".into()
            })
        );
        assert_eq!(
            PolicyBackup::parse("[PCI\\X]\nDevicePolicy=absent\n"),
            Err(BackupError::MissingValue {
                device: "PCI\\X".into(),
                key: ASSIGNMENT_SET_OVERRIDE.into()
            })
        );
    }
}
