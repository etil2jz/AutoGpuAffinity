use std::borrow::Cow;
use std::io;

use autogpuaffinity_domain::policy::{ASSIGNMENT_SET_OVERRIDE, AffinityPolicy, DEVICE_POLICY, RawValue};
use thiserror::Error;
use winreg::RegKey;
use winreg::RegValue;
use winreg::enums::{
    HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_BINARY, REG_DWORD, REG_DWORD_BIG_ENDIAN,
    REG_EXPAND_SZ, REG_FULL_RESOURCE_DESCRIPTOR, REG_LINK, REG_MULTI_SZ, REG_NONE, REG_QWORD, REG_RESOURCE_LIST,
    REG_RESOURCE_REQUIREMENTS_LIST, REG_SZ, RegType,
};

const BASIC_DISPLAY_KEY: &str = r"SYSTEM\CurrentControlSet\Services\BasicDisplay";
const PRODUCT_OPTIONS_KEY: &str = r"SYSTEM\CurrentControlSet\Control\ProductOptions";

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("unable to open registry key {path}")]
    Open {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("unable to read {name} in {path}")]
    Read {
        path: String,
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("unable to write {name} in {path}")]
    Write {
        path: String,
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("unable to delete {name} in {path}")]
    Delete {
        path: String,
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("unsupported registry value type {kind} for {name}")]
    UnsupportedType { name: String, kind: u32 },
}

fn hklm() -> RegKey {
    RegKey::predef(HKEY_LOCAL_MACHINE)
}

/// `CurrentControlSet` rather than `ControlSet001`, which is not always the active one.
fn policy_path(instance_id: &str) -> String {
    format!(r"SYSTEM\CurrentControlSet\Enum\{instance_id}\Device Parameters\Interrupt Management\Affinity Policy")
}

fn open(path: &str, access: u32) -> Result<Option<RegKey>, RegistryError> {
    match hklm().open_subkey_with_flags(path, access | KEY_WOW64_64KEY) {
        Ok(key) => Ok(Some(key)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(RegistryError::Open {
            path: path.to_owned(),
            source,
        }),
    }
}

fn read_raw(key: &RegKey, path: &str, name: &str) -> Result<Option<RawValue>, RegistryError> {
    match key.get_raw_value(name) {
        Ok(value) => Ok(Some(RawValue {
            kind: value.vtype as u32,
            data: value.bytes.into_owned(),
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(RegistryError::Read {
            path: path.to_owned(),
            name: name.to_owned(),
            source,
        }),
    }
}

fn reg_type(kind: u32) -> Option<RegType> {
    [
        REG_NONE,
        REG_SZ,
        REG_EXPAND_SZ,
        REG_BINARY,
        REG_DWORD,
        REG_DWORD_BIG_ENDIAN,
        REG_LINK,
        REG_MULTI_SZ,
        REG_RESOURCE_LIST,
        REG_FULL_RESOURCE_DESCRIPTOR,
        REG_RESOURCE_REQUIREMENTS_LIST,
        REG_QWORD,
    ]
    .into_iter()
    .find(|candidate| candidate.clone() as u32 == kind)
}

pub fn read_policy(instance_id: &str) -> Result<AffinityPolicy, RegistryError> {
    let path = policy_path(instance_id);
    let Some(key) = open(&path, KEY_QUERY_VALUE)? else {
        return Ok(AffinityPolicy::default());
    };
    Ok(AffinityPolicy {
        device_policy: read_raw(&key, &path, DEVICE_POLICY)?,
        assignment_set_override: read_raw(&key, &path, ASSIGNMENT_SET_OVERRIDE)?,
    })
}

/// Present values are written and absent ones deleted, so the key ends up exactly as described.
pub fn write_policy(instance_id: &str, policy: &AffinityPolicy) -> Result<(), RegistryError> {
    let path = policy_path(instance_id);
    let key = if policy.is_empty() {
        match open(&path, KEY_SET_VALUE)? {
            Some(key) => key,
            None => return Ok(()),
        }
    } else {
        hklm()
            .create_subkey_with_flags(&path, KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY)
            .map(|(key, _)| key)
            .map_err(|source| RegistryError::Open {
                path: path.clone(),
                source,
            })?
    };

    for (name, value) in policy.values() {
        match value {
            Some(value) => {
                let vtype = reg_type(value.kind).ok_or_else(|| RegistryError::UnsupportedType {
                    name: name.to_owned(),
                    kind: value.kind,
                })?;
                key.set_raw_value(
                    name,
                    &RegValue {
                        bytes: Cow::Borrowed(&value.data),
                        vtype,
                    },
                )
                .map_err(|source| RegistryError::Write {
                    path: path.clone(),
                    name: name.to_owned(),
                    source,
                })?;
            }
            None => match key.delete_value(name) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    return Err(RegistryError::Delete {
                        path: path.clone(),
                        name: name.to_owned(),
                        source: error,
                    });
                }
                _ => {}
            },
        }
    }
    Ok(())
}

/// `None` when the service is not registered.
pub fn basic_display_start_type() -> Result<Option<u32>, RegistryError> {
    let Some(key) = open(BASIC_DISPLAY_KEY, KEY_QUERY_VALUE)? else {
        return Ok(None);
    };
    match key.get_value("Start") {
        Ok(start) => Ok(Some(start)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(RegistryError::Read {
            path: BASIC_DISPLAY_KEY.to_owned(),
            name: "Start".to_owned(),
            source,
        }),
    }
}

/// `ProductType` is `WinNT` on workstations, `ServerNT` or `LanmanNT` on servers.
pub fn is_server() -> Result<bool, RegistryError> {
    let read_error = |source| RegistryError::Read {
        path: PRODUCT_OPTIONS_KEY.to_owned(),
        name: "ProductType".to_owned(),
        source,
    };
    let key = open(PRODUCT_OPTIONS_KEY, KEY_QUERY_VALUE)?
        .ok_or_else(|| read_error(io::Error::from(io::ErrorKind::NotFound)))?;
    let product_type: String = key.get_value("ProductType").map_err(read_error)?;
    Ok(!product_type.eq_ignore_ascii_case("WinNT"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_types_round_trip() {
        for kind in [3, 4, 11] {
            assert_eq!(reg_type(kind).map(|vtype| vtype as u32), Some(kind));
        }
        assert!(reg_type(99).is_none());
    }

    #[test]
    #[ignore = "reads the real system configuration"]
    fn reads_system_values() {
        println!("BasicDisplay start type: {:?}", basic_display_start_type().unwrap());
        println!("server: {}", is_server().unwrap());
    }
}
