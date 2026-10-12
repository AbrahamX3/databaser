//! Everything about discovering and controlling local database services.

mod control;
mod error;
mod model;
mod scm;

pub use control::run_action;
pub use model::{ServiceAction, ServiceInfo, ServiceKind, ServiceState};
pub use scm::{discover, query_states};
