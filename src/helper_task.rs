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

    #[error("removing elevated control was cancelled")]
    RemoveCancelled,

    #[error("removing elevated control failed: {0}")]
    Teardown(String),
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

/// Whether no-prompt control is set up and still usable on this machine.
///
/// The task performs its actions through the shared request and response files,
/// so *which copy* of the executable it launches does not matter as long as that
/// copy is still there. Requiring an exact match with the running binary made
/// the toggle silently read as off whenever the app was started from a different
/// build of itself - a debug build beside the installed one, say - which is not
/// what "no-prompt control is on" means.
pub fn is_usable() -> bool {
    registered_command().is_some_and(|command| Path::new(&command).is_file())
}

/// The program the task launches, or `None` when there is no such task.
fn registered_command() -> Option<String> {
    let output = system_tool("schtasks.exe")
        .args(["/query", "/tn", TASK_NAME, "/xml"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    extract_command(&String::from_utf8_lossy(&output.stdout))
}

/// Pull `<Command>` out of the task definition.
///
/// The value is XML-escaped, so a path containing `&` arrives as `&amp;`.
fn extract_command(xml: &str) -> Option<String> {
    const OPEN: &str = "<Command>";
    let start = xml.find(OPEN)? + OPEN.len();
    let end = start + xml[start..].find("</Command>")?;
    let command = xml[start..end].trim();

    (!command.is_empty()).then(|| {
        command
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&")
    })
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

/// Delete the scheduled task, reverting to the per-action UAC prompt.
///
/// The task is registered *Run with highest privileges*, so changing it needs
/// administrator rights as well: deleting it unelevated fails with "Access is
/// denied", regardless of the account that registered it. That is why this
/// elevates, and why it can prompt once — exactly like setup, and for the same
/// reason.
pub fn remove() -> Result<(), TaskError> {
    let script_path = std::env::temp_dir().join("databaser-teardown.ps1");
    std::fs::write(&script_path, teardown_script()).map_err(TaskError::Io)?;

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
        // Confirm rather than assume: a refused deletion must not be reported
        // as success, or the toggle would be a one-way door again.
        Ok(status) if status.success() && !is_registered() => Ok(()),
        Ok(status) => Err(TaskError::Teardown(format!(
            "the task is still registered (exit code {})",
            status.code().unwrap_or(-1)
        ))),
        Err(err) if is_uac_cancelled(&err) => Err(TaskError::RemoveCancelled),
        Err(err) => Err(TaskError::Teardown(err.to_string())),
    }
}

/// Whether the task exists, whatever it points at.
fn is_registered() -> bool {
    system_tool("schtasks.exe")
        .args(["/query", "/tn", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|output| output.status.success())
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

fn teardown_script() -> String {
    // `schtasks` is a native command, so `$ErrorActionPreference` does not turn
    // a failure into a script failure. Propagate its exit code instead, or the
    // caller would read a refused deletion as success.
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         schtasks.exe /delete /tn '{TASK_NAME}' /f | Out-Null\n\
         if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}\n"
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

#[cfg(test)]
mod tests {
    use super::extract_command;

    #[test]
    fn reads_the_command_out_of_a_task_definition() {
        let xml = r#"<Task><Actions><Exec>
            <Command>C:\Users\Abraham\AppData\Local\Programs\Databaser\databaser.exe</Command>
            <Arguments>--ctl-from-task</Arguments>
        </Exec></Actions></Task>"#;
        assert_eq!(
            extract_command(xml).as_deref(),
            Some(r"C:\Users\Abraham\AppData\Local\Programs\Databaser\databaser.exe")
        );
    }

    #[test]
    fn unescapes_an_ampersand_in_the_path() {
        let xml = r"<Command>C:\Tools &amp; Bits\databaser.exe</Command>";
        assert_eq!(
            extract_command(xml).as_deref(),
            Some(r"C:\Tools & Bits\databaser.exe")
        );
    }

    #[test]
    fn a_task_without_a_command_reads_as_none() {
        assert_eq!(extract_command("<Task></Task>"), None);
    }
}
