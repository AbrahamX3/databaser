//! A one-time-installed Windows Scheduled Task that performs privileged
//! service actions without a UAC prompt on every click.
//!
//! Setup (a single UAC prompt) registers a task marked *Run with highest
//! privileges* whose action is `databaser.exe --ctl-from-task`. Afterwards the
//! unelevated GUI drops a request file, starts the task, and reads the helper's
//! response — no elevation prompt.
//!
//! The task has no trigger, so it only ever runs when we ask it to.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::services::ServiceAction;

/// Name of the scheduled task, and the folder it is registered under.
const TASK_NAME: &str = "DatabaserElevatedControl";

/// Windows `CREATE_NO_WINDOW`: keeps the console helpers from flashing a window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("could not run schtasks: {0}")]
    Io(#[source] std::io::Error),

    #[error("the elevated helper did not respond (was the task removed?)")]
    NoResponse,

    #[error("{0}")]
    Helper(String),

    #[error("setting up elevated control was cancelled")]
    Cancelled,

    #[error("setting up elevated control failed: {0}")]
    Setup(String),
}

/// Request file the GUI writes and the task's helper reads.
pub fn request_path() -> PathBuf {
    state_dir().join("ctl-request.json")
}

/// Response file the helper writes and the GUI reads.
fn response_path() -> PathBuf {
    state_dir().join("ctl-response.json")
}

fn state_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Databaser")
}

/// True when the scheduled task exists *and* runs the given executable (so a
/// moved or rebuilt binary is detected and setup is offered again).
pub fn is_registered_for(exe: &Path) -> bool {
    let Ok(output) = system_tool("schtasks.exe")
        .args(["/query", "/tn", TASK_NAME, "/xml"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let xml = String::from_utf8_lossy(&output.stdout).to_lowercase();
    xml.contains(&exe.to_string_lossy().to_lowercase())
}

/// Register (or re-register) the highest-privileges task. Prompts for UAC once.
pub fn setup(exe: &Path) -> Result<(), TaskError> {
    let script_path = std::env::temp_dir().join("databaser-setup.ps1");
    std::fs::write(&script_path, setup_script(exe)).map_err(TaskError::Io)?;

    let result = runas::Command::new(powershell_path())
        .args(&[
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
        ])
        .arg(&script_path)
        .status();

    let _ = std::fs::remove_file(&script_path);

    match result {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(TaskError::Setup(format!(
            "the task could not be registered (exit code {})",
            status.code().unwrap_or(-1)
        ))),
        Err(err) if is_uac_cancelled(&err) => Err(TaskError::Cancelled),
        Err(err) => Err(TaskError::Setup(err.to_string())),
    }
}

/// Run one action through the scheduled task and wait for its result.
pub fn run_via_task(action: ServiceAction, service: &str) -> Result<(), TaskError> {
    let request = request_path();
    let response = response_path();
    if let Some(dir) = request.parent() {
        std::fs::create_dir_all(dir).map_err(TaskError::Io)?;
    }

    let _ = std::fs::remove_file(&response);
    let payload = serde_json::json!({ "action": action.verb(), "service": service });
    std::fs::write(&request, payload.to_string()).map_err(TaskError::Io)?;

    let status = system_tool("schtasks.exe")
        .args(["/run", "/tn", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(TaskError::Io)?;
    if !status.success() {
        let _ = std::fs::remove_file(&request);
        return Err(TaskError::Helper(
            "could not start the elevated task".to_string(),
        ));
    }

    wait_for_response(&request, &response)
}

fn wait_for_response(request: &Path, response: &Path) -> Result<(), TaskError> {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        if let Ok(text) = std::fs::read_to_string(response) {
            let _ = std::fs::remove_file(response);
            let _ = std::fs::remove_file(request);

            let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            return if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
                Ok(())
            } else {
                Err(TaskError::Helper(
                    value
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("the operation failed")
                        .to_string(),
                ))
            };
        }

        if Instant::now() >= deadline {
            let _ = std::fs::remove_file(request);
            return Err(TaskError::NoResponse);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Read the request written by the GUI. Used by the task's helper process.
pub fn read_request() -> Option<(ServiceAction, String)> {
    let text = std::fs::read_to_string(request_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let action = value.get("action").and_then(|v| v.as_str())?;
    let service = value.get("service").and_then(|v| v.as_str())?;
    Some((ServiceAction::parse(action)?, service.to_string()))
}

/// Write the helper's result atomically (temp file + rename) so the GUI never
/// reads a half-written file.
pub fn write_response(ok: bool, error: &str) {
    let response = response_path();
    if let Some(dir) = response.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let payload = if ok {
        serde_json::json!({ "ok": true })
    } else {
        serde_json::json!({ "ok": false, "error": error })
    };

    let tmp = response.with_extension("json.tmp");
    if std::fs::write(&tmp, payload.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &response);
    }
}

fn setup_script(exe: &Path) -> String {
    // Single-quote the path for PowerShell and escape embedded quotes.
    let exe = exe.to_string_lossy().replace('\'', "''");
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $me = \"$env:USERDOMAIN\\$env:USERNAME\"\n\
         $action = New-ScheduledTaskAction -Execute '{exe}' -Argument '--ctl-from-task'\n\
         $principal = New-ScheduledTaskPrincipal -UserId $me -LogonType Interactive -RunLevel Highest\n\
         $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes 5)\n\
         Register-ScheduledTask -TaskName '{TASK_NAME}' -Action $action -Principal $principal -Settings $settings -Force | Out-Null\n"
    )
}

fn system_tool(name: &str) -> std::process::Command {
    std::process::Command::new(system32().join(name))
}

fn powershell_path() -> PathBuf {
    system32()
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

fn system32() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
}

fn is_uac_cancelled(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(1223) || err.to_string().to_lowercase().contains("cancel")
}
