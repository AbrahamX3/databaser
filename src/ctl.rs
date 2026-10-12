//! Headless entry points for the privileged helper processes.
//!
//! * `databaser.exe --ctl <action> <service> --out <file>` — the per-action UAC
//!   fallback. Performs the action and writes a JSON result the GUI reads.
//! * `databaser.exe --ctl-from-task` — run by the scheduled task. Reads the
//!   request file, performs the action, and writes the response file.

use std::io::Write;

use crate::helper_task;
use crate::services::{run_action, ServiceAction};

pub fn run(args: &[String]) -> i32 {
    let mut action: Option<ServiceAction> = None;
    let mut service: Option<String> = None;
    let mut out: Option<String> = None;

    let mut i = 2; // skip exe and "--ctl"
    while i < args.len() {
        let arg = args[i].as_str();
        if let Some(parsed) = ServiceAction::parse(arg) {
            action = Some(parsed);
            i += 1;
        } else if arg == "--out" {
            out = args.get(i + 1).cloned();
            i += 2;
        } else {
            if service.is_none() {
                service = Some(arg.to_string());
            }
            i += 1;
        }
    }

    let (Some(action), Some(service)) = (action, service) else {
        write_out(out.as_deref(), false, "Invalid arguments for --ctl.");
        return 2;
    };

    match run_action(action, &service) {
        Ok(()) => {
            write_out(out.as_deref(), true, "");
            0
        }
        Err(error) => {
            write_out(out.as_deref(), false, &error.to_string());
            1
        }
    }
}

/// Entry point used by the scheduled task.
pub fn run_from_task() -> i32 {
    let Some((action, service)) = helper_task::read_request() else {
        helper_task::write_response(false, "The elevated helper received an invalid request.");
        return 2;
    };

    match run_action(action, &service) {
        Ok(()) => {
            helper_task::write_response(true, "");
            0
        }
        Err(error) => {
            helper_task::write_response(false, &error.to_string());
            1
        }
    }
}

fn write_out(path: Option<&str>, ok: bool, error: &str) {
    let Some(path) = path else { return };
    let payload = if ok {
        serde_json::json!({ "ok": true })
    } else {
        serde_json::json!({ "ok": false, "error": error })
    };
    if let Ok(mut file) = std::fs::File::create(path) {
        let _ = file.write_all(payload.to_string().as_bytes());
    }
}
