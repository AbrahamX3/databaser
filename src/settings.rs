//! Persisted application settings, stored per-user under `%APPDATA%\Databaser`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// How long a cached update check stays fresh.
const UPDATE_CHECK_INTERVAL_SECS: u64 = 60 * 60 * 24;

/// Seconds since the Unix epoch, or 0 if the clock is before it.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Three-way theme choice. `System` follows the operating system appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub theme: ThemePreference,
    /// Unix time of the last update check (successful or not), so launch checks
    /// are throttled instead of hitting GitHub on every start.
    #[serde(default)]
    pub last_update_check: Option<u64>,
    /// A version the user dismissed, so the banner does not reappear for it.
    #[serde(default)]
    pub skipped_version: Option<String>,
}

impl Settings {
    fn dir() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("Databaser"))
    }

    fn file() -> Option<PathBuf> {
        Self::dir().map(|dir| dir.join("settings.json"))
    }

    /// Load settings, falling back to defaults if the file is missing or bad.
    pub fn load() -> Self {
        let Some(path) = Self::file() else {
            return Self::default();
        };
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Best-effort persist; failures are ignored so a read-only profile never
    /// crashes the app.
    pub fn save(&self) {
        let Some(dir) = Self::dir() else { return };
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(dir.join("settings.json"), text);
        }
    }

    /// True when the cached update check is missing or older than a day.
    pub fn update_check_due(&self) -> bool {
        match self.last_update_check {
            Some(at) => now_unix().saturating_sub(at) >= UPDATE_CHECK_INTERVAL_SECS,
            None => true,
        }
    }

    /// Record that an update check was just attempted.
    pub fn record_update_check(&mut self) {
        self.last_update_check = Some(now_unix());
    }
}

#[cfg(test)]
mod tests {
    use super::{Settings, ThemePreference};

    #[test]
    fn theme_preference_defaults_to_system() {
        assert_eq!(ThemePreference::default(), ThemePreference::System);
    }

    #[test]
    fn theme_preference_deserializes_from_lowercase() {
        let parsed: ThemePreference = serde_json::from_str("\"dark\"").unwrap();
        assert_eq!(parsed, ThemePreference::Dark);
    }

    #[test]
    fn update_check_is_due_when_never_checked() {
        assert!(Settings::default().update_check_due());
    }

    #[test]
    fn update_check_is_not_due_when_checked_just_now() {
        let mut settings = Settings::default();
        settings.record_update_check();
        assert!(!settings.update_check_due());
    }

    #[test]
    fn update_check_is_due_when_cached_result_is_a_day_old() {
        let settings = Settings {
            last_update_check: Some(super::now_unix().saturating_sub(60 * 60 * 25)),
            ..Settings::default()
        };
        assert!(settings.update_check_due());
    }
}
