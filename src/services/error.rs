//! Errors produced while discovering or controlling Windows services.

use std::io;

/// Anything that can go wrong while reading or controlling services.
///
/// Uses `thiserror` so each variant carries a human-readable `Display` message
/// and keeps the underlying Win32 error as its `source`. The `reason` field is
/// the *deepest* cause (e.g. "Access is denied. (os error 5)") because
/// `windows_service::Error`'s own `Display` is uninformative.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("Could not open the Windows service manager: {reason}")]
    ServiceManager {
        reason: String,
        #[source]
        source: windows_service::Error,
    },

    #[error("Could not read the service registry: {0}")]
    Registry(#[source] io::Error),

    #[error("Could not open service `{name}` (try running as administrator): {reason}")]
    OpenService {
        name: String,
        reason: String,
        #[source]
        source: windows_service::Error,
    },

    #[error("Could not {verb} `{name}`: {reason}")]
    Control {
        verb: &'static str,
        name: String,
        reason: String,
        #[source]
        source: windows_service::Error,
    },

    #[error("`{name}` stopped again before it finished starting")]
    StoppedWhileStarting { name: String },

    #[error("Timed out waiting for `{name}` to finish {verb}")]
    Timeout { name: String, verb: &'static str },
}

impl ServiceError {
    pub(crate) fn service_manager(source: windows_service::Error) -> Self {
        Self::ServiceManager {
            reason: describe(&source),
            source,
        }
    }

    pub(crate) fn open_service(name: &str, source: windows_service::Error) -> Self {
        Self::OpenService {
            name: name.to_string(),
            reason: describe(&source),
            source,
        }
    }

    pub(crate) fn control(
        verb: &'static str,
        name: &str,
        source: windows_service::Error,
    ) -> Self {
        Self::Control {
            verb,
            name: name.to_string(),
            reason: describe(&source),
            source,
        }
    }
}

/// Walk an error's `source` chain to its deepest cause and stringify that.
fn describe(error: &windows_service::Error) -> String {
    let mut current: &dyn std::error::Error = error;
    while let Some(source) = current.source() {
        current = source;
    }
    current.to_string()
}
