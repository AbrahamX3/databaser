//! Service domain model, independent of any Windows API.

/// Which database product a discovered service belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ServiceKind {
    MySql,
    PostgreSql,
    SqlServer,
}

impl ServiceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::MySql => "MySQL",
            Self::PostgreSql => "PostgreSQL",
            Self::SqlServer => "SQL Server",
        }
    }
}

/// Lifecycle state of a Windows service, mirroring the SCM states we care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
}

impl ServiceState {
    pub fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }

    /// True while the SCM is still moving the service between states.
    pub fn is_transitioning(self) -> bool {
        matches!(
            self,
            Self::StartPending | Self::StopPending | Self::ContinuePending | Self::PausePending
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::StartPending => "Starting\u{2026}",
            Self::StopPending => "Stopping\u{2026}",
            Self::Running => "Running",
            Self::ContinuePending => "Resuming\u{2026}",
            Self::PausePending => "Pausing\u{2026}",
            Self::Paused => "Paused",
        }
    }
}

/// How Windows should start the service.
///
/// The third state, `Disabled`, matters: Windows refuses to start a disabled
/// service (error 1058), so it is the difference between "does not start at
/// boot" and "cannot be started at all".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartType {
    Automatic,
    Manual,
    Disabled,
}

impl StartType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Manual => "Manual",
            Self::Disabled => "Disabled",
        }
    }

    /// One-line consequence of the setting, used to confirm a change.
    pub fn effect(self) -> &'static str {
        match self {
            Self::Automatic => "starts automatically with Windows",
            Self::Manual => "starts only when you start it",
            Self::Disabled => "will not start at all until it is re-enabled",
        }
    }

    /// Lower-case verb used on the `--ctl` command line.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Manual => "manual",
            Self::Disabled => "disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Automatic),
            "manual" => Some(Self::Manual),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

/// A discovered database service and its last known state.
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    /// SCM service name, e.g. `MySQL80` or `postgresql-x64-18`.
    pub name: String,
    /// Human friendly display name shown in the UI.
    pub display_name: String,
    /// Path to the service binary, as recorded in the SCM configuration.
    pub executable_path: String,
    pub kind: ServiceKind,
    pub state: ServiceState,
    /// How Windows starts this service at boot.
    pub start_type: StartType,
}

/// An operation the user can request against a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    /// Change how Windows starts the service.
    SetStartType(StartType),
}

impl ServiceAction {
    /// Lower-case verb used on the `--ctl` command line.
    ///
    /// A start-type change is spelled by its target, so the existing
    /// `<verb> <service>` command line keeps working unchanged.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::SetStartType(start) => start.verb(),
        }
    }

    /// Past-tense verb for notifications, e.g. "Started".
    pub fn done_label(self) -> &'static str {
        match self {
            Self::Start => "Started",
            Self::Stop => "Stopped",
            Self::Restart => "Restarted",
            Self::SetStartType(_) => "Start-up updated for",
        }
    }

    /// Present-continuous verb for progress text, e.g. "Starting".
    pub fn ing_label(self) -> &'static str {
        match self {
            Self::Start => "Starting",
            Self::Stop => "Stopping",
            Self::Restart => "Restarting",
            Self::SetStartType(_) => "Changing start-up",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            _ => StartType::parse(s).map(Self::SetStartType),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ServiceAction, StartType};

    #[test]
    fn parse_returns_start_for_start() {
        assert_eq!(ServiceAction::parse("start"), Some(ServiceAction::Start));
    }

    #[test]
    fn parse_returns_stop_for_stop() {
        assert_eq!(ServiceAction::parse("stop"), Some(ServiceAction::Stop));
    }

    #[test]
    fn parse_returns_restart_for_restart() {
        assert_eq!(ServiceAction::parse("restart"), Some(ServiceAction::Restart));
    }

    #[test]
    fn parse_returns_none_for_unknown_verb() {
        assert_eq!(ServiceAction::parse("pause"), None);
    }

    #[test]
    fn parse_returns_set_start_type_for_each_start_verb() {
        assert_eq!(
            ServiceAction::parse("auto"),
            Some(ServiceAction::SetStartType(StartType::Automatic))
        );
        assert_eq!(
            ServiceAction::parse("manual"),
            Some(ServiceAction::SetStartType(StartType::Manual))
        );
        assert_eq!(
            ServiceAction::parse("disabled"),
            Some(ServiceAction::SetStartType(StartType::Disabled))
        );
    }

    /// The CLI round-trips the action through `verb` and `parse`, so the two
    /// must agree for every action the UI can request.
    #[test]
    fn every_action_survives_a_verb_round_trip() {
        for action in [
            ServiceAction::Start,
            ServiceAction::Stop,
            ServiceAction::Restart,
            ServiceAction::SetStartType(StartType::Automatic),
            ServiceAction::SetStartType(StartType::Manual),
            ServiceAction::SetStartType(StartType::Disabled),
        ] {
            assert_eq!(ServiceAction::parse(action.verb()), Some(action));
        }
    }
}
