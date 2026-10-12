//! Best-effort product versions, read from the registry or the data directory.
//!
//! No new dependencies: `winreg` is already used to enumerate services. Product
//! releases keep this in different places, and none of them is authoritative, so
//! every lookup is optional and a miss simply hides the row.

use super::model::ServiceKind;
use std::path::Path;
use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

/// The installed version, or `None` when it cannot be determined.
pub fn read(kind: ServiceKind, service_name: &str, executable_path: &str) -> Option<String> {
    match kind {
        ServiceKind::MySql => mysql(executable_path),
        ServiceKind::PostgreSql => postgres(executable_path),
        ServiceKind::SqlServer => sql_server(service_name),
    }
}

/// MySQL keeps one `MySQL Server <x.y>` key per installed server under
/// `SOFTWARE\MySQL AB`, so match the service's binary against each key's
/// `Location`.
fn mysql(executable_path: &str) -> Option<String> {
    let ab = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\MySQL AB")
        .ok()?;
    let binary = executable_path.trim_matches('"').to_lowercase();

    let mut fallback = None;
    for name in ab.enum_keys().flatten() {
        // `SOFTWARE\MySQL AB` also holds keys for tools such as MySQL
        // Workbench, whose version would be plain wrong for a server.
        if !name.starts_with("MySQL Server") {
            continue;
        }
        let Ok(key) = ab.open_subkey(&name) else {
            continue;
        };
        let Ok(version) = key.get_value::<String, _>("Version") else {
            continue;
        };
        let location = key
            .get_value::<String, _>("Location")
            .unwrap_or_default()
            .trim_end_matches(['\\', '/'])
            .to_lowercase();

        if !location.is_empty() && binary.starts_with(&location) {
            return Some(version);
        }
        fallback = Some(version);
    }

    // A single installation is unambiguous even if `Location` was missing.
    fallback
}

/// PostgreSQL's service command line carries its data directory as `-D`, and
/// the data directory holds a `PG_VERSION` file with the major version.
fn postgres(executable_path: &str) -> Option<String> {
    let data_dir = postgres_data_dir(executable_path)?;
    let version = std::fs::read_to_string(Path::new(&data_dir).join("PG_VERSION")).ok()?;
    let version = version.trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// The argument after `-D` in a service command line, unquoted.
fn postgres_data_dir(command: &str) -> Option<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;

    for ch in command.chars() {
        match ch {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("-D"))
        .and_then(|index| tokens.get(index + 1))
        .cloned()
}

/// SQL Server maps the instance name to an internal id under
/// `Instance Names\SQL`, and that id's key carries `CurrentVersion`.
fn sql_server(service_name: &str) -> Option<String> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let base = r"SOFTWARE\Microsoft\Microsoft SQL Server";

    let instance_id: String = hklm
        .open_subkey(format!(r"{base}\Instance Names\SQL"))
        .ok()?
        .get_value(sql_server_instance(service_name))
        .ok()?;

    hklm.open_subkey(format!(r"{base}\{instance_id}\MSSQLServer\CurrentVersion"))
        .ok()?
        .get_value::<String, _>("CurrentVersion")
        .ok()
}

/// `MSSQL$SQLEXPRESS` is the `SQLEXPRESS` instance; `MSSQLSERVER` is default.
fn sql_server_instance(service_name: &str) -> &str {
    service_name.strip_prefix("MSSQL$").unwrap_or("MSSQLSERVER")
}

#[cfg(test)]
mod tests {
    use super::{postgres_data_dir, sql_server_instance};

    #[test]
    fn postgres_data_dir_reads_a_quoted_dash_d_argument() {
        let command = r#""C:\Program Files\PostgreSQL\18\bin\pg_ctl.exe" runservice -N "postgresql-x64-18" -D "C:\Program Files\PostgreSQL\18\data""#;
        assert_eq!(
            postgres_data_dir(command).as_deref(),
            Some(r"C:\Program Files\PostgreSQL\18\data")
        );
    }

    #[test]
    fn postgres_data_dir_accepts_an_unquoted_path() {
        let command = r#"C:\pgsql\bin\pg_ctl.exe runservice -D C:\pgsql\data"#;
        assert_eq!(postgres_data_dir(command).as_deref(), Some(r"C:\pgsql\data"));
    }

    #[test]
    fn postgres_data_dir_is_none_without_the_argument() {
        assert_eq!(postgres_data_dir(r#""C:\pg\pg_ctl.exe" runservice"#), None);
    }

    #[test]
    fn named_sql_server_instances_drop_the_service_prefix() {
        assert_eq!(sql_server_instance("MSSQL$SQLEXPRESS"), "SQLEXPRESS");
    }

    #[test]
    fn the_default_sql_server_service_maps_to_mssqlserver() {
        assert_eq!(sql_server_instance("MSSQLSERVER"), "MSSQLSERVER");
    }
}
