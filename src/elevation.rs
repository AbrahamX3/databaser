//! Choosing *how* to run a privileged action, and the per-action UAC fallback.
//!
//! Three modes, in order of preference:
//!  * [`ControlMode::Elevated`] — this process is already an administrator, so
//!    the action runs in-process with no prompt.
//!  * [`ControlMode::Task`] — a one-time-installed scheduled task performs the
//!    action, again with no prompt.
//!  * [`ControlMode::Uac`] — fall back to elevating a short-lived helper
//!    (`--ctl`) per action, which prompts once per click.

use std::io;
use std::path::PathBuf;

use crate::helper_task::{self, TaskError};
use crate::services::{self, ServiceAction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMode {
    Elevated,
    Task,
    Uac,
}

/// Result of a requested action, phrased for the UI.
#[derive(Debug)]
pub enum ActionOutcome {
    Done,
    Cancelled,
    Failed(String),
}

/// Errors from the per-action UAC fallback.
#[derive(Debug, thiserror::Error)]
pub enum ElevationError {
    /// The user dismissed the UAC prompt.
    #[error("elevation was cancelled")]
    Cancelled,

    /// The elevated process could not be started at all.
    #[error("{0}")]
    Spawn(#[source] io::Error),

    /// The helper ran but reported a problem, or gave no result.
    #[error("{0}")]
    Helper(String),
}

/// Work out which control mode is available. Runs off the UI thread because it
/// may spawn `schtasks.exe`.
pub fn detect_mode() -> ControlMode {
    if is_elevated() {
        return ControlMode::Elevated;
    }
    if helper_task::is_usable() {
        ControlMode::Task
    } else {
        ControlMode::Uac
    }
}

/// Whether this process already holds administrator rights. Opening the SCM
/// with create-service access fails for a standard user, which is a cheap and
/// reliable probe.
pub fn is_elevated() -> bool {
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE).is_ok()
}

/// Run an action using the best available mode.
pub fn perform(mode: ControlMode, action: ServiceAction, service: &str) -> ActionOutcome {
    match mode {
        ControlMode::Elevated => match services::run_action(action, service) {
            Ok(()) => ActionOutcome::Done,
            Err(error) => ActionOutcome::Failed(error.to_string()),
        },
        ControlMode::Task => match helper_task::run_via_task(action, service) {
            Ok(()) => ActionOutcome::Done,
            Err(TaskError::Cancelled) => ActionOutcome::Cancelled,
            Err(error) => ActionOutcome::Failed(error.to_string()),
        },
        ControlMode::Uac => match run_elevated(action, service) {
            Ok(()) => ActionOutcome::Done,
            Err(ElevationError::Cancelled) => ActionOutcome::Cancelled,
            Err(error) => ActionOutcome::Failed(error.to_string()),
        },
    }
}

pub fn run_elevated(action: ServiceAction, service: &str) -> Result<(), ElevationError> {
    let exe = std::env::current_exe().map_err(ElevationError::Spawn)?;

    let out_path = temp_out_path();
    let _ = std::fs::remove_file(&out_path);

    let args: Vec<String> = vec![
        "--ctl".into(),
        action.verb().into(),
        service.into(),
        "--out".into(),
        out_path.to_string_lossy().into_owned(),
    ];

    match runas::Command::new(exe).args(&args).status() {
        Ok(_status) => {}
        Err(err) => {
            if is_uac_cancelled(&err) {
                return Err(ElevationError::Cancelled);
            }
            return Err(ElevationError::Spawn(err));
        }
    }

    read_result(&out_path)
}

fn read_result(out_path: &PathBuf) -> Result<(), ElevationError> {
    let Ok(text) = std::fs::read_to_string(out_path) else {
        return Err(ElevationError::Helper(
            "The elevated helper did not report a result.".into(),
        ));
    };
    let _ = std::fs::remove_file(out_path);

    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err(ElevationError::Helper(
            value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("The operation failed.")
                .to_string(),
        ))
    }
}

fn temp_out_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    std::env::temp_dir().join(format!("databaser-ctl-{}-{nanos}.json", std::process::id()))
}

/// ERROR_CANCELLED (1223) means the user clicked "No" on the UAC prompt.
fn is_uac_cancelled(err: &io::Error) -> bool {
    err.raw_os_error() == Some(1223) || err.to_string().to_lowercase().contains("cancel")
}
