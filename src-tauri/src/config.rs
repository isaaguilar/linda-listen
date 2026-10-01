use crate::error::{AppError, AppResult};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

const SETTINGS_FILE: &str = "settings.json";
const KEY_SERVICE: &str = "linda-listen";
const KEY_ACCOUNT: &str = "gemini-api-key";

pub const DEFAULT_SHORTCUT: &str = "Option+Space";
const LEGACY_DEFAULT_SHORTCUT: &str = "CommandOrControl+Shift+Space";
pub const DEFAULT_GEMINI_MODEL: &str = "gemini-2.5-flash";
pub const DEFAULT_WHISPER_LANGUAGE: &str = "";
pub const DEFAULT_GEMINI_PROMPT: &str = "\
Clean this transcript:\n\
\n\
1. Fix spelling, capitalization, and punctuation errors\n\
2. Convert number words to digits (twenty-five → 25, ten percent → 10%, five dollars → $5)\n\
3. Replace spoken punctuation with symbols (period → ., comma → ,, question mark → ?)\n\
4. Remove filler words (um, uh, like as filler)\n\
5. Keep the language in the original version (if it was french, keep it in french for example)\n\
\n\
It is ok to paraphrase for clarification but keep the order of the content.\n\
\n\
Return only the cleaned transcript.";
const CONFIG_VERSION: u8 = 3;

pub(crate) fn normalize_shortcut(shortcut: &str) -> String {
    shortcut
        .trim()
        .replace("CmdOrControl", "CommandOrControl")
        .replace("CmdOrCtrl", "CommandOrControl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub config_version: u8,
    pub gemini_enabled: bool,
    pub gemini_model: String,
    pub gemini_prompt: String,
    pub whisper_model_path: String,
    pub whisper_language: String,
    pub shortcut: String,
    pub auto_paste: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            gemini_enabled: false,
            gemini_model: DEFAULT_GEMINI_MODEL.to_owned(),
            gemini_prompt: DEFAULT_GEMINI_PROMPT.to_owned(),
            whisper_model_path: String::new(),
            whisper_language: DEFAULT_WHISPER_LANGUAGE.to_owned(),
            shortcut: DEFAULT_SHORTCUT.to_owned(),
            auto_paste: true,
        }
    }
}

fn migrate_to_local_first(config: &mut AppConfig) {
    if config.config_version == 0 {
        config.gemini_enabled = false;
        config.config_version = CONFIG_VERSION;
    }
}

fn migrate_default_shortcut(config: &mut AppConfig) {
    let normalized_shortcut = normalize_shortcut(&config.shortcut);
    if normalized_shortcut.is_empty() || normalized_shortcut == LEGACY_DEFAULT_SHORTCUT {
        config.shortcut = DEFAULT_SHORTCUT.to_owned();
    } else if normalized_shortcut != config.shortcut {
        config.shortcut = normalized_shortcut;
    }
}

#[derive(Debug)]
pub struct ConfigStore {
    config_path: PathBuf,
    keyring: keyring_core::Entry,
}

impl ConfigStore {
    pub fn load() -> AppResult<(Self, AppConfig)> {
        let dirs = ProjectDirs::from("com", "galleybytes", "linda-listen")
            .ok_or(AppError::ConfigDirUnavailable)?;
        let config_dir = dirs.config_dir();
        fs::create_dir_all(config_dir)?;

        keyring::use_native_store(false)?;

        let config_path = config_dir.join(SETTINGS_FILE);
        let keyring = keyring_core::Entry::new(KEY_SERVICE, KEY_ACCOUNT)?;
        let config = if config_path.exists() {
            let mut config = Self::read_config(&config_path)?;
            migrate_to_local_first(&mut config);
            migrate_default_shortcut(&mut config);
            if config.config_version != CONFIG_VERSION {
                config.config_version = CONFIG_VERSION;
            }
            config
        } else {
            AppConfig::default()
        };

        let store = Self {
            config_path,
            keyring,
        };
        store.save_config(&config)?;

        Ok((store, config))
    }

    fn read_config(path: &Path) -> AppResult<AppConfig> {
        let bytes = fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn save_config(&self, config: &AppConfig) -> AppResult<()> {
        let text = serde_json::to_string_pretty(config)?;
        fs::write(&self.config_path, text)?;
        Ok(())
    }

    pub fn read_api_key(&self) -> AppResult<Option<String>> {
        match self.keyring.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub fn set_api_key(&self, api_key: Option<&str>) -> AppResult<()> {
        match api_key.map(str::trim).filter(|value| !value.is_empty()) {
            Some(password) => {
                self.keyring.set_password(password)?;
            }
            None => match self.keyring.delete_credential() {
                Ok(()) | Err(keyring_core::Error::NoEntry) => {}
                Err(err) => return Err(err.into()),
            },
        }

        Ok(())
    }

    pub fn config_dir(&self) -> &Path {
        self.config_path.parent().unwrap_or(&self.config_path)
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_starts_local_first() {
        let config = AppConfig::default();

        assert_eq!(config.config_version, CONFIG_VERSION);
        assert!(!config.gemini_enabled);
    }

    #[test]
    fn legacy_config_is_migrated_to_local_first() {
        let mut config = AppConfig {
            config_version: 0,
            gemini_enabled: true,
            gemini_model: DEFAULT_GEMINI_MODEL.to_owned(),
            gemini_prompt: DEFAULT_GEMINI_PROMPT.to_owned(),
            whisper_model_path: String::new(),
            whisper_language: DEFAULT_WHISPER_LANGUAGE.to_owned(),
            shortcut: DEFAULT_SHORTCUT.to_owned(),
            auto_paste: true,
        };

        migrate_to_local_first(&mut config);

        assert_eq!(config.config_version, CONFIG_VERSION);
        assert!(!config.gemini_enabled);
    }

    #[test]
    fn modern_config_keeps_optional_gemini_enabled() {
        let mut config = AppConfig {
            config_version: CONFIG_VERSION,
            gemini_enabled: true,
            gemini_model: DEFAULT_GEMINI_MODEL.to_owned(),
            gemini_prompt: DEFAULT_GEMINI_PROMPT.to_owned(),
            whisper_model_path: String::new(),
            whisper_language: DEFAULT_WHISPER_LANGUAGE.to_owned(),
            shortcut: DEFAULT_SHORTCUT.to_owned(),
            auto_paste: true,
        };

        migrate_to_local_first(&mut config);

        assert_eq!(config.config_version, CONFIG_VERSION);
        assert!(config.gemini_enabled);
    }

    #[test]
    fn shortcut_is_normalized_to_command_or_control() {
        assert_eq!(
            normalize_shortcut("CmdOrCtrl+Shift+Space"),
            "CommandOrControl+Shift+Space"
        );
        assert_eq!(
            normalize_shortcut("CmdOrControl+Shift+Space"),
            "CommandOrControl+Shift+Space"
        );
    }

    #[test]
    fn legacy_default_shortcut_is_migrated_to_option_space() {
        let mut config = AppConfig {
            config_version: 2,
            gemini_enabled: false,
            gemini_model: DEFAULT_GEMINI_MODEL.to_owned(),
            gemini_prompt: DEFAULT_GEMINI_PROMPT.to_owned(),
            whisper_model_path: String::new(),
            whisper_language: DEFAULT_WHISPER_LANGUAGE.to_owned(),
            shortcut: LEGACY_DEFAULT_SHORTCUT.to_owned(),
            auto_paste: true,
        };

        migrate_default_shortcut(&mut config);

        assert_eq!(config.shortcut, DEFAULT_SHORTCUT);
    }
}
