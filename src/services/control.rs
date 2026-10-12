//! Start / stop / restart a service. These calls require administrator rights,
//! so this module only ever runs inside the elevated helper process (see
//! [`crate::ctl`]).

use super::error::ServiceError;
use super::model::ServiceAction;
use std::ffi::OsStr;
use std::thread::sleep;
use std::time::{Duration, Instant};
use windows_service::service::{ServiceAccess, Service as WinService, ServiceState as WinState};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const STEP_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(300);

pub fn run_action(action: ServiceAction, name: &str) -> Result<(), ServiceError> {
    match action {
        ServiceAction::Start => start(name),
        ServiceAction::Stop => stop(name),
        ServiceAction::Restart => {
            // Best-effort stop, then start. If it was already stopped we simply
            // start it.
            let _ = stop(name);
            start(name)
        }
    }
}

fn start(name: &str) -> Result<(), ServiceError> {
    let service = open(name, ServiceAccess::START | ServiceAccess::QUERY_STATUS)?;

    if current_state(&service) == Some(WinState::Running) {
        return Ok(());
    }

    let no_args: [&OsStr; 0] = [];
    service
        .start(&no_args)
        .map_err(|source| ServiceError::control("start", name, source))?;

    wait_for(&service, WinState::Running, name, "starting")
}

fn stop(name: &str) -> Result<(), ServiceError> {
    let service = open(name, ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)?;

    if current_state(&service) == Some(WinState::Stopped) {
        return Ok(());
    }

    service
        .stop()
        .map_err(|source| ServiceError::control("stop", name, source))?;

    wait_for(&service, WinState::Stopped, name, "stopping")
}

fn open(name: &str, access: ServiceAccess) -> Result<WinService, ServiceError> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(ServiceError::service_manager)?;
    manager
        .open_service(name, access)
        .map_err(|source| ServiceError::open_service(name, source))
}

fn current_state(service: &WinService) -> Option<WinState> {
    service.query_status().ok().map(|status| status.current_state)
}

fn wait_for(
    service: &WinService,
    target: WinState,
    name: &str,
    verb: &'static str,
) -> Result<(), ServiceError> {
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        match current_state(service) {
            Some(state) if state == target => return Ok(()),
            Some(WinState::Stopped) if target == WinState::Running => {
                return Err(ServiceError::StoppedWhileStarting {
                    name: name.to_string(),
                });
            }
            _ => {}
        }

        if Instant::now() >= deadline {
            return Err(ServiceError::Timeout {
                name: name.to_string(),
                verb,
            });
        }
        sleep(POLL_INTERVAL);
    }
}
