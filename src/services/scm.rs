//! Discovery and status queries against the Windows Service Control Manager.
//!
//! Reading is always allowed, so these calls run in the ordinary (unelevated)
//! process. Starting and stopping a service lives in [`super::control`] and is
//! executed by the elevated helper.

use super::error::ServiceError;
use super::model::{ServiceInfo, ServiceKind, ServiceState};
use windows_service::service::{ServiceAccess, ServiceState as WinState};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

/// Enumerate every installed service, keep the MySQL/MariaDB and PostgreSQL
/// ones, and read their current state.
pub fn discover() -> Result<Vec<ServiceInfo>, ServiceError> {
    let manager = open_manager()?;

    let mut found = Vec::new();
    for name in service_names()? {
        // Not every entry in the Services registry key is a real service
        // (driver services, leftovers), so opening may fail. Skip those.
        let Ok(service) =
            manager.open_service(&name, ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG)
        else {
            continue;
        };
        let Ok(config) = service.query_config() else {
            continue;
        };

        let executable = config.executable_path.to_string_lossy().to_lowercase();
        let display_name = config.display_name.to_string_lossy().to_string();
        let Some(kind) = classify(&name, &executable, &display_name) else {
            continue;
        };

        let state = service
            .query_status()
            .map_or(ServiceState::Stopped, |status| convert_state(status.current_state));

        found.push(ServiceInfo {
            display_name: if display_name.is_empty() {
                name.clone()
            } else {
                display_name
            },
            name,
            kind,
            state,
        });
    }

    found.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
    });
    Ok(found)
}

/// Read the state of the named services. Returns `None` for a name that cannot
/// be opened (for example if it was uninstalled since discovery).
pub fn query_states(names: &[String]) -> Result<Vec<Option<ServiceState>>, ServiceError> {
    let manager = open_manager()?;

    Ok(names
        .iter()
        .map(|name| {
            manager
                .open_service(name, ServiceAccess::QUERY_STATUS)
                .ok()
                .and_then(|service| service.query_status().ok())
                .map(|status| convert_state(status.current_state))
        })
        .collect())
}

fn open_manager() -> Result<ServiceManager, ServiceError> {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(ServiceError::service_manager)
}

/// Read the registry for the list of service key names. Enumeration itself has
/// no crate wrapper, so we read names here and use SCM for everything else.
fn service_names() -> Result<Vec<String>, ServiceError> {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let services = hklm
        .open_subkey(r"SYSTEM\CurrentControlSet\Services")
        .map_err(ServiceError::Registry)?;

    Ok(services.enum_keys().filter_map(Result::ok).collect())
}

/// Decide whether a service is a database service we care about. Matching on
/// the executable path is the most reliable signal; the name and display name
/// catch services whose path we could not read.
fn classify(name: &str, executable: &str, display_name: &str) -> Option<ServiceKind> {
    let name = name.to_lowercase();
    let display = display_name.to_lowercase();

    if executable.contains("mysqld")
        || executable.contains("mariadbd")
        || name.starts_with("mysql")
        || name.starts_with("maria")
        || display.contains("mysql")
        || display.contains("mariadb")
    {
        return Some(ServiceKind::MySql);
    }

    if executable.contains("pg_ctl")
        || executable.contains("postgres")
        || name.starts_with("postgresql")
        || name.starts_with("pgsql")
        || display.contains("postgresql")
    {
        return Some(ServiceKind::PostgreSql);
    }

    // SQL Server engine only. It runs `sqlservr.exe` and its display name is
    // "SQL Server (INSTANCE)", which cleanly excludes the Agent, Browser, CEIP
    // and VSS Writer helpers.
    if executable.contains("sqlservr.exe")
        || name == "mssqlserver"
        || name.starts_with("mssql$")
        || display.starts_with("sql server (")
    {
        return Some(ServiceKind::SqlServer);
    }

    None
}

fn convert_state(state: WinState) -> ServiceState {
    match state {
        WinState::Stopped => ServiceState::Stopped,
        WinState::StartPending => ServiceState::StartPending,
        WinState::StopPending => ServiceState::StopPending,
        WinState::Running => ServiceState::Running,
        WinState::ContinuePending => ServiceState::ContinuePending,
        WinState::PausePending => ServiceState::PausePending,
        WinState::Paused => ServiceState::Paused,
    }
}

#[cfg(test)]
mod tests {
    use super::{classify, ServiceKind};

    #[test]
    fn classify_matches_mysql_from_executable() {
        let kind = classify("MySQL80", r"c:\program files\mysql\bin\mysqld.exe", "MySQL80");
        assert_eq!(kind, Some(ServiceKind::MySql));
    }

    #[test]
    fn classify_matches_postgres_from_executable() {
        let kind = classify(
            "postgresql-x64-18",
            r"c:\program files\postgresql\18\bin\pg_ctl.exe",
            "",
        );
        assert_eq!(kind, Some(ServiceKind::PostgreSql));
    }

    #[test]
    fn classify_matches_mariadb_by_service_name() {
        let kind = classify("MariaDB", "", "");
        assert_eq!(kind, Some(ServiceKind::MySql));
    }

    #[test]
    fn classify_matches_postgres_by_display_name() {
        let kind = classify("svc_a1b2", "", "PostgreSQL Server 16");
        assert_eq!(kind, Some(ServiceKind::PostgreSql));
    }

    #[test]
    fn classify_ignores_unrelated_service() {
        let kind = classify("Spooler", r"c:\windows\system32\spoolsv.exe", "Print Spooler");
        assert_eq!(kind, None);
    }

    #[test]
    fn classify_matches_sql_server_engine_from_executable() {
        let kind = classify(
            "MSSQL$SQLEXPRESS",
            r"c:\program files\microsoft sql server\mssql15.sqlexpress\mssql\binn\sqlservr.exe",
            "SQL Server (SQLEXPRESS)",
        );
        assert_eq!(kind, Some(ServiceKind::SqlServer));
    }

    #[test]
    fn classify_matches_sql_server_engine_by_display_name() {
        let kind = classify("svc_unknown", "", "SQL Server (SQLEXPRESS)");
        assert_eq!(kind, Some(ServiceKind::SqlServer));
    }

    #[test]
    fn classify_matches_default_sql_server_instance_by_name() {
        let kind = classify("MSSQLSERVER", "", "");
        assert_eq!(kind, Some(ServiceKind::SqlServer));
    }

    #[test]
    fn classify_ignores_sql_server_agent() {
        let kind = classify(
            "SQLAgent$SQLEXPRESS",
            r"c:\program files\microsoft sql server\mssql15.sqlexpress\mssql\binn\sqlagent.exe",
            "SQL Server Agent (SQLEXPRESS)",
        );
        assert_eq!(kind, None);
    }

    #[test]
    fn classify_ignores_sql_server_browser() {
        let kind = classify(
            "SQLBrowser",
            r"c:\program files (x86)\microsoft sql server\90\shared\sqlbrowser.exe",
            "SQL Server Browser",
        );
        assert_eq!(kind, None);
    }

    #[test]
    fn classify_ignores_sql_server_telemetry_and_writer() {
        let telemetry = classify(
            "SQLTELEMETRY$SQLEXPRESS",
            r"c:\program files\microsoft sql server\mssql15.sqlexpress\mssql\binn\sqlceip.exe",
            "SQL Server CEIP service (SQLEXPRESS)",
        );
        let writer = classify(
            "SQLWriter",
            r"c:\program files (x86)\microsoft sql server\90\shared\sqlwriter.exe",
            "SQL Server VSS Writer",
        );
        assert_eq!(telemetry, None);
        assert_eq!(writer, None);
    }
}
