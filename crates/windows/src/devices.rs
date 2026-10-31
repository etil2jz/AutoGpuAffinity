use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use autogpuaffinity_domain::device::{DeviceState, DisplayAdapter, GraphicsCard};
use serde::Deserialize;
use thiserror::Error;
use wmi::{WMIConnection, WMIError};

const STATE_CHANGE_DELAY: Duration = Duration::from_secs(2);
const START_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const ENABLE_ATTEMPTS: u32 = 3;

#[derive(Debug, Deserialize)]
#[serde(rename = "Win32_PnPEntity")]
struct PnpEntity {
    #[serde(rename = "__Path")]
    path: String,
    #[serde(rename = "DeviceID")]
    device_id: String,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "ConfigManagerErrorCode")]
    problem_code: Option<u32>,
    #[serde(rename = "Present")]
    present: Option<bool>,
}

impl PnpEntity {
    fn is_present(&self) -> bool {
        self.present != Some(false)
    }

    fn state(&self) -> DeviceState {
        self.problem_code
            .map_or(DeviceState::Unknown, DeviceState::from_problem_code)
    }

    fn into_adapter(self) -> DisplayAdapter {
        let state = self.state();
        let name = self
            .name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.device_id.clone());
        DisplayAdapter {
            card: GraphicsCard {
                instance_id: self.device_id,
                name,
            },
            state,
        }
    }
}

#[derive(Debug, Deserialize)]
struct StateChangeOutput {
    #[serde(rename = "ReturnValue")]
    return_value: u32,
    #[serde(rename = "rebootNeeded")]
    reboot_needed: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateChange {
    Enable,
    Disable,
}

impl StateChange {
    fn method(self) -> &'static str {
        match self {
            Self::Enable => "Enable",
            Self::Disable => "Disable",
        }
    }
}

impl fmt::Display for StateChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
        })
    }
}

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("unable to connect to WMI")]
    Connect(#[source] WMIError),
    #[error("unable to query display adapters")]
    Query(#[source] WMIError),
    #[error("{0} is no longer present")]
    Absent(String),
    #[error("unable to {change} {device}")]
    Call {
        change: StateChange,
        device: String,
        #[source]
        source: WMIError,
    },
    #[error("unable to {change} {device} (error code {code})")]
    Rejected {
        change: StateChange,
        device: String,
        code: u32,
    },
    #[error("the driver of {0} can not be restarted without rebooting the system")]
    RebootRequired(String),
    #[error("{device} did not start within {} s (device is {state})", START_TIMEOUT.as_secs())]
    StartTimeout { device: String, state: DeviceState },
}

/// Plug and Play access through `Win32_PnPEntity`, whose Enable and Disable methods mirror Device Manager.
pub struct DeviceManager {
    wmi: WMIConnection,
}

impl DeviceManager {
    pub fn connect() -> Result<Self, DeviceError> {
        WMIConnection::new()
            .map(|wmi| Self { wmi })
            .map_err(DeviceError::Connect)
    }

    pub fn display_adapters(&self) -> Result<Vec<DisplayAdapter>, DeviceError> {
        let entities: Vec<PnpEntity> = self
            .wmi
            .raw_query("SELECT * FROM Win32_PnPEntity WHERE PNPClass = 'Display'")
            .map_err(DeviceError::Query)?;
        Ok(entities
            .into_iter()
            .filter(PnpEntity::is_present)
            .map(PnpEntity::into_adapter)
            .collect())
    }

    fn find(&self, card: &GraphicsCard) -> Result<Option<PnpEntity>, DeviceError> {
        let query = format!(
            "SELECT * FROM Win32_PnPEntity WHERE DeviceID = '{}'",
            wql_string(&card.instance_id)
        );
        let entities: Vec<PnpEntity> = self.wmi.raw_query(query).map_err(DeviceError::Query)?;
        Ok(entities.into_iter().find(PnpEntity::is_present))
    }

    /// `None` when the device is no longer present.
    pub fn state(&self, card: &GraphicsCard) -> Result<Option<DeviceState>, DeviceError> {
        Ok(self.find(card)?.map(|entity| entity.state()))
    }

    /// Returns whether Windows needs a reboot to apply the change.
    fn change_state(&self, card: &GraphicsCard, change: StateChange) -> Result<bool, DeviceError> {
        let entity = self.find(card)?.ok_or_else(|| DeviceError::Absent(card.name.clone()))?;
        let output: StateChangeOutput = self
            .wmi
            .exec_instance_method::<PnpEntity, _>(&entity.path, change.method(), ())
            .map_err(|source| DeviceError::Call {
                change,
                device: card.name.clone(),
                source,
            })?;
        if output.return_value != 0 {
            return Err(DeviceError::Rejected {
                change,
                device: card.name.clone(),
                code: output.return_value,
            });
        }
        Ok(output.reboot_needed.unwrap_or(false))
    }

    fn wait_until_working(&self, card: &GraphicsCard) -> Result<(), DeviceError> {
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            let state = self
                .state(card)?
                .ok_or_else(|| DeviceError::Absent(card.name.clone()))?;
            if state == DeviceState::Working {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(DeviceError::StartTimeout {
                    device: card.name.clone(),
                    state,
                });
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn enable(&self, card: &GraphicsCard) -> Result<(), DeviceError> {
        let mut attempt = 1;
        loop {
            match self.change_state(card, StateChange::Enable) {
                Ok(false) => return self.wait_until_working(card),
                Ok(true) => return Err(DeviceError::RebootRequired(card.name.clone())),
                Err(DeviceError::Absent(device)) => return Err(DeviceError::Absent(device)),
                Err(_) if attempt < ENABLE_ATTEMPTS => {
                    attempt += 1;
                    thread::sleep(Duration::from_secs(1));
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Makes the driver read its affinity policy again; a card is never left disabled on failure.
    pub fn restart(&self, card: &GraphicsCard) -> Result<(), DeviceError> {
        match self.change_state(card, StateChange::Disable) {
            Ok(false) => {}
            Ok(true) => {
                // Cancels the disable that Windows scheduled for the next boot.
                self.enable(card)?;
                return Err(DeviceError::RebootRequired(card.name.clone()));
            }
            Err(error) => {
                let _ = self.enable(card);
                return Err(error);
            }
        }
        thread::sleep(STATE_CHANGE_DELAY);
        self.enable(card)?;
        thread::sleep(STATE_CHANGE_DELAY);
        Ok(())
    }

    /// Re-enables a card left disabled by an abrupt exit mid-restart, otherwise restarts it.
    pub fn reload(&self, card: &GraphicsCard) -> Result<(), DeviceError> {
        match self.state(card)? {
            None => Err(DeviceError::Absent(card.name.clone())),
            Some(DeviceState::Disabled) => self.enable(card),
            Some(_) => self.restart(card),
        }
    }
}

fn wql_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wql_strings_are_escaped() {
        assert_eq!(
            wql_string(r"PCI\VEN_10DE&DEV_2482\4&D0BDF66&0&0009"),
            r"PCI\\VEN_10DE&DEV_2482\\4&D0BDF66&0&0009"
        );
        assert_eq!(wql_string("it's"), r"it\'s");
    }

    #[test]
    #[ignore = "reads the real hardware configuration"]
    fn lists_real_display_adapters() {
        let devices = DeviceManager::connect().unwrap();
        for adapter in devices.display_adapters().unwrap() {
            let state = devices.state(&adapter.card).unwrap();
            println!(
                "{} [{}] -> {:?} / {state:?}",
                adapter.card.name, adapter.card.instance_id, adapter.state
            );
        }
    }
}
