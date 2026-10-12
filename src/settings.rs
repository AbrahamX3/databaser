//! Persisted application settings, stored per-user under `%APPDATA%\Databaser`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
}

#[cfg(test)]
mod tests {
    use super::ThemePreference;

    #[test]
    fn theme_preference_defaults_to_system() {
        assert_eq!(ThemePreference::default(), ThemePreference::System);
    }

    #[test]
    fn theme_preference_deserializes_from_lowercase() {
        let parsed: ThemePreference = serde_json::from_str("\"dark\"").unwrap();
        assert_eq!(parsed, ThemePreference::Dark);
    }
}
