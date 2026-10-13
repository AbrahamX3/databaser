//! Start / stop / restart a service, and change how Windows starts it. These
//! calls require administrator rights, so this module only ever runs inside the
//! elevated helper process (see [`crate::ctl`]).

use super::error::ServiceError;
use super::model::{ServiceAction, StartType};
use std::ffi::OsStr;
use std::io;
use std::thread::sleep;
use std::time::{Duration, Instant};
use windows_service::service::{
    ServiceAccess, Service as WinService, ServiceStartType as WinStartType,
    ServiceState as WinState,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_sys::Win32::System::Services::{ChangeServiceConfigW, SERVICE_NO_CHANGE};

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
        ServiceAction::SetStartType(start_type) => set_start_type(name, start_type),
    }
}

/// Change only how Windows starts the service, leaving every other SCM setting
/// exactly as it was.
///
/// [`WinService::change_config`] cannot be used for this. It writes the whole
/// configuration back, and `query_config` hands out `lpBinaryPathName` verbatim
/// (for PostgreSQL that is `pg_ctl.exe` followed by its arguments), whereas
/// `change_config` re-escapes whatever it is given as one executable path.
/// Feeding the two together would rewrite the binary path into nonsense.
/// `ChangeServiceConfigW` with [`SERVICE_NO_CHANGE`] for every other field
/// changes one thing and nothing else.
///
/// [`WinService::change_config`]: windows_service::service::Service::change_config
#[expect(
    unsafe_code,
    reason = "ChangeServiceConfigW has no safe wrapper for a start-type-only update"
)]
fn set_start_type(name: &str, start_type: StartType) -> Result<(), ServiceError> {
    let service = open(name, ServiceAccess::CHANGE_CONFIG | ServiceAccess::QUERY_CONFIG)?;

    let win_start_type = match start_type {
        StartType::Automatic => WinStartType::AutoStart,
        StartType::Manual => WinStartType::OnDemand,
        StartType::Disabled => WinStartType::Disabled,
    };

    // Safety: `service` keeps a valid handle alive across the call, and every
    // pointer argument is NULL, which the API documents as "leave unchanged".
    let changed = unsafe {
        ChangeServiceConfigW(
            service.raw_handle(),
            SERVICE_NO_CHANGE,
            win_start_type.to_raw(),
            SERVICE_NO_CHANGE,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };

    if changed == 0 {
        return Err(ServiceError::control(
            "change the start-up type of",
            name,
            windows_service::Error::Winapi(io::Error::last_os_error()),
        ));
    }

    // Read it back rather than trusting the return value: the UI must never
    // report a start type that the Service Control Manager did not take.
    let applied = service
        .query_config()
        .ok()
        .map(|config| config.start_type.to_raw());
    if applied != Some(win_start_type.to_raw()) {
        return Err(ServiceError::control(
            "confirm the start-up type of",
            name,
            windows_service::Error::Winapi(io::Error::other(
                "the Service Control Manager did not apply the change",
            )),
        ));
    }

    Ok(())
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
