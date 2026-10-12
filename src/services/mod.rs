//! Everything about discovering and controlling local database services.

mod control;
mod error;
mod model;
mod ports;
mod scm;
mod version;

pub use control::run_action;
pub use model::{ServiceAction, ServiceInfo, ServiceKind, ServiceState, StartType};
pub use ports::listening_ports;
pub use scm::{discover, process_id, query_states};
