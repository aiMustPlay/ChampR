use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[allow(dead_code)]
pub struct Settings {
    /// Source identifiers the user has checked (e.g. ["op.gg", "u.gg"])
    #[serde(default)]
    pub selected_sources: Vec<String>,
    /// Which source to show runes from in the overlay window
    #[serde(default)]
    pub rune_source: String,
    #[serde(default)]
    pub tts_rate: i32,
    #[serde(default = "default_tts_volume")]
    pub tts_volume: i32,
    #[serde(default = "default_tts_voice")]
    pub tts_voice: String,
    #[serde(default = "default_lol_launcher_path")]
    pub lol_launcher_path: String,
    #[serde(default)]
    pub deepseek_api_key: String,
    #[serde(default = "default_deepseek_base_url")]
    pub deepseek_base_url: String,
    #[serde(default = "default_deepseek_model")]
    pub deepseek_model: String,
    #[serde(default)]
    pub deepseek_thinking: bool,
    #[serde(default)]
    pub deepseek_stream: bool,
    #[serde(default = "default_deepseek_reasoning_effort")]
    pub deepseek_reasoning_effort: String,
}

fn default_tts_volume() -> i32 {
    100
}

fn default_tts_voice() -> String {
    "Microsoft Huihui".to_string()
}

fn default_lol_launcher_path() -> String {
    r"C:\WeGameApps\英雄联盟（含经典模式）\WeGameLauncher\launcher.exe".to_string()
}

fn default_deepseek_base_url() -> String {
    "https://api.deepseek.com".to_string()
}

fn default_deepseek_model() -> String {
    "deepseek-v4-flash".to_string()
}

fn default_deepseek_reasoning_effort() -> String {
    "high".to_string()
}

fn settings_path() -> PathBuf {
    let mut dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push("champr");
    dir.push("settings.toml");
    dir
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let mut settings: Self = toml::from_str(&contents).unwrap_or_default();
                settings.normalize_defaults();
                settings
            }
            Err(_) => Self {
                tts_rate: 0,
                tts_volume: default_tts_volume(),
                tts_voice: default_tts_voice(),
                lol_launcher_path: default_lol_launcher_path(),
                ..Self::default()
            },
        }
    }

    fn normalize_defaults(&mut self) {
        if self.tts_voice.is_empty() {
            self.tts_voice = default_tts_voice();
        }
        if self.lol_launcher_path.is_empty() {
            self.lol_launcher_path = default_lol_launcher_path();
        }
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(contents) = toml::to_string_pretty(self) {
            let _ = fs::write(&path, contents);
        }
    }
}
