use std::fmt;

/// Device Manager problem code for a disabled device (CM_PROB_DISABLED).
const PROBLEM_DISABLED: u32 = 22;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsCard {
    pub instance_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Working,
    Disabled,
    Problem(u32),
    Unknown,
}

impl DeviceState {
    pub fn from_problem_code(code: u32) -> Self {
        match code {
            0 => Self::Working,
            PROBLEM_DISABLED => Self::Disabled,
            code => Self::Problem(code),
        }
    }
}

impl fmt::Display for DeviceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Working => f.write_str("working"),
            Self::Disabled => f.write_str("disabled"),
            Self::Problem(code) => write!(f, "reporting problem code {code}"),
            Self::Unknown => f.write_str("in an unknown state"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayAdapter {
    pub card: GraphicsCard,
    pub state: DeviceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    NotPci,
    NotWorking(DeviceState),
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPci => f.write_str("not a PCI device"),
            Self::NotWorking(state) => write!(f, "device is {state}"),
        }
    }
}

/// Virtual adapters have no interrupts, and restarting a disabled card would re-enable it.
pub fn select_cards(adapters: Vec<DisplayAdapter>) -> (Vec<GraphicsCard>, Vec<(GraphicsCard, SkipReason)>) {
    let mut selected = Vec::new();
    let mut skipped = Vec::new();
    for DisplayAdapter { card, state } in adapters {
        if !is_pci(&card.instance_id) {
            skipped.push((card, SkipReason::NotPci));
        } else if state == DeviceState::Working {
            selected.push(card);
        } else {
            skipped.push((card, SkipReason::NotWorking(state)));
        }
    }
    (selected, skipped)
}

fn is_pci(instance_id: &str) -> bool {
    instance_id
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("PCI\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(instance_id: &str, state: DeviceState) -> DisplayAdapter {
        DisplayAdapter {
            card: GraphicsCard {
                instance_id: instance_id.into(),
                name: instance_id.into(),
            },
            state,
        }
    }

    #[test]
    fn only_working_pci_cards_are_selected() {
        let (selected, skipped) = select_cards(vec![
            adapter(r"PCI\VEN_10DE&DEV_2684\4&2C2E7A3A&0&0019", DeviceState::Working),
            adapter(r"pci\ven_8086&dev_a780\3&11583659&0&10", DeviceState::Disabled),
            adapter(r"ROOT\BasicDisplay\0000", DeviceState::Working),
            adapter(r"SWD\REMOTEDISPLAYENUM\RDPIDD_INDIRECTDISPLAY", DeviceState::Working),
        ]);
        assert_eq!(selected.len(), 1);
        let reasons: Vec<SkipReason> = skipped.into_iter().map(|(_, reason)| reason).collect();
        assert_eq!(
            reasons,
            [
                SkipReason::NotWorking(DeviceState::Disabled),
                SkipReason::NotPci,
                SkipReason::NotPci
            ]
        );
    }

    #[test]
    fn problem_codes_map_to_states() {
        assert_eq!(DeviceState::from_problem_code(0), DeviceState::Working);
        assert_eq!(DeviceState::from_problem_code(22), DeviceState::Disabled);
        assert_eq!(DeviceState::from_problem_code(43), DeviceState::Problem(43));
    }
}
