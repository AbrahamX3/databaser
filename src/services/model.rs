//! Service domain model, independent of any Windows API.

/// Which database product a discovered service belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ServiceKind {
    MySql,
    PostgreSql,
}

impl ServiceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::MySql => "MySQL",
            Self::PostgreSql => "PostgreSQL",
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

/// A discovered database service and its last known state.
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    /// SCM service name, e.g. `MySQL80` or `postgresql-x64-18`.
    pub name: String,
    /// Human friendly display name shown in the UI.
    pub display_name: String,
    pub kind: ServiceKind,
    pub state: ServiceState,
}

/// An operation the user can request against a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

impl ServiceAction {
    /// Lower-case verb used on the `--ctl` command line.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }

    /// Past-tense verb for notifications, e.g. "Started".
    pub fn done_label(self) -> &'static str {
        match self {
            Self::Start => "Started",
            Self::Stop => "Stopped",
            Self::Restart => "Restarted",
        }
    }

    /// Present-continuous verb for progress text, e.g. "Starting".
    pub fn ing_label(self) -> &'static str {
        match self {
            Self::Start => "Starting",
            Self::Stop => "Stopping",
            Self::Restart => "Restarting",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ServiceAction;

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
}
