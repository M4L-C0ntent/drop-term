use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub window_width: u32,
    pub window_height: u32,
    #[serde(default = "default_user_host_color")]
    pub user_host_color: (u8, u8, u8),
    #[serde(default = "default_directory_color")]
    pub directory_color: (u8, u8, u8),
}

// Matches the conventional ANSI green/blue used for user@host and cwd in
// the vast majority of default Debian/Ubuntu-based bash prompts.
fn default_user_host_color() -> (u8, u8, u8) {
    (0, 205, 0)
}

fn default_directory_color() -> (u8, u8, u8) {
    (0, 0, 238)
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            window_width: 900,
            window_height: 500,
            user_host_color: default_user_host_color(),
            directory_color: default_directory_color(),
        }
    }
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("drop-term")
        .join("config.ron")
}

pub fn load() -> AppConfig {
    let path = config_path();
    if let Ok(contents) = std::fs::read_to_string(&path) {
        if let Ok(mut cfg) = ron::from_str::<AppConfig>(&contents) {
            cfg.window_width = cfg.window_width.max(500);
            cfg.window_height = cfg.window_height.max(300);
            return cfg;
        }
    }
    let default = AppConfig::default();
    save(&default);
    default
}

pub fn save(config: &AppConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(serialized) = ron::ser::to_string_pretty(config, ron::ser::PrettyConfig::default()) {
        let _ = std::fs::write(path, serialized);
    }
}
