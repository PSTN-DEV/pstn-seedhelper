use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── Enums ────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub enum AfterSeedAction {
    #[default]
    #[serde(rename = "Ничего")]
    Nothing,
    #[serde(rename = "Закрыть игру и Выйти")]
    CloseAndExit,
    #[serde(rename = "Завершение Работы")]
    Shutdown,
    #[serde(rename = "Спящий Режим")]
    Sleep,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

// ── Config ───────────────────────────────────────────────────────────────────

const CONFIG_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub config_version: u32,
    pub steam_id: String,

    pub desired_players: u32,
    pub checkup_interval: u64,
    pub start_on_startup: bool,
    pub auto_start_seeding: bool,
    pub startup_wait_minutes: u32,
    pub game_launch_delay: u32,
    pub time_limit_hour: u32,
    pub after_seed_action: AfterSeedAction,
    pub stop_after_server: u8,
    pub time_limit_minute: u32,
    #[serde(default = "default_true")]
    pub time_limit_enabled: bool,
    pub preferred_fps: Option<u32>,
    pub preferred_menu_fps: Option<u32>,
    pub preferred_res_x: Option<u32>,
    pub preferred_res_y: Option<u32>,
    pub render_toggle: bool,
    pub auto_create_squad: bool,
    #[serde(default = "default_true")]
    pub disable_sound: bool,
    #[serde(default = "default_true")]
    pub delete_startup_video: bool,
    pub eco_mode: bool,
    #[serde(default = "default_true")]
    pub auto_update: bool,
    pub theme: Theme,
    pub night_mode_enabled: bool,
    pub night_start_hour: u32,
    pub night_start_minute: u32,
    pub night_end_hour: u32,
    pub night_end_minute: u32,
    pub night_after_action: AfterSeedAction,

    pub seed_period_start_hour: u32,
    pub seed_period_start_minute: u32,

    pub launcher_path: Option<String>,

    // None = disabled, Some("HH:MM") = scheduled shutdown
    // Migration: old Python config stores "" for disabled
    #[serde(deserialize_with = "de_optional_string")]
    pub scheduled_shutdown: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            steam_id: String::new(),
            desired_players: 65,
            checkup_interval: 60,
            start_on_startup: false,
            auto_start_seeding: false,
            startup_wait_minutes: 2,
            game_launch_delay: 3,
            time_limit_hour: 17,
            time_limit_minute: 0,
            time_limit_enabled: true,
            after_seed_action: AfterSeedAction::Nothing,
            stop_after_server: 0,
            preferred_fps: None,
            preferred_menu_fps: None,
            preferred_res_x: None,
            preferred_res_y: None,
            render_toggle: false,
            auto_create_squad: false,
            disable_sound: true,
            delete_startup_video: true,
            eco_mode: false,
            auto_update: true,
            theme: Theme::Dark,
            scheduled_shutdown: None,
            night_mode_enabled: false,
            night_start_hour: 23,
            night_start_minute: 0,
            night_end_hour: 5,
            night_end_minute: 0,
            night_after_action: AfterSeedAction::Nothing,
            seed_period_start_hour: 5,
            seed_period_start_minute: 0,
            launcher_path: None,
        }
    }
}

fn default_true() -> bool { true }

fn de_optional_string<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Accept both null/missing and an empty string "" for "disabled"
    let opt: Option<String> = Option::deserialize(de)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

// ── Paths ────────────────────────────────────────────────────────────────────

pub fn config_dir() -> PathBuf {
    #[cfg(debug_assertions)]
    {
        // ponytail: use cwd in debug so cargo run picks up local config.json
        return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    }
    #[cfg(not(debug_assertions))]
    {
        std::env::var("APPDATA")
            .map(|p| PathBuf::from(p).join(".SeedHelper"))
            .unwrap_or_else(|_| PathBuf::from(".SeedHelper"))
    }
}

#[cfg(not(debug_assertions))]
fn legacy_config_path() -> Option<PathBuf> {
    let p = std::env::var("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("Temp").join("sqseeder").join("config.json"))
        .ok()?;
    p.exists().then_some(p)
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn log_dir() -> PathBuf {
    config_dir().join("logs")
}

/// Today's log file. Path is date-stamped, so the append-writer rolls to a new
/// file automatically at midnight — no rotation logic needed.
pub fn log_path() -> PathBuf {
    let day = chrono::Local::now().format("%Y-%m-%d");
    log_dir().join(format!("seed_{day}.log"))
}

/// Create the logs dir and delete anything older than 7 days. Called once at startup.
pub fn setup_logs() {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    // Pre-4.5 single log file, now superseded by per-day files in logs/.
    let _ = std::fs::remove_file(config_dir().join("seed_debug.log"));
    let cutoff = chrono::Local::now().date_naive() - chrono::Duration::days(7);
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        // seed_YYYY-MM-DD.log
        let Some(date) = name.strip_prefix("seed_").and_then(|s| s.strip_suffix(".log")) else {
            continue;
        };
        if let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
            if d < cutoff {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

pub fn game_settings_path() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(|p| {
            PathBuf::from(p)
                .join("SquadGame")
                .join("Saved")
                .join("Config")
                .join("Windows")
                .join("GameUserSettings.ini")
        })
        .unwrap_or_else(|_| PathBuf::from("GameUserSettings.ini"))
}

// ── Init ─────────────────────────────────────────────────────────────────────

pub fn ensure_config_dir() {
    let dir = config_dir();
    if dir.exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("Failed to create config dir: {e}");
        return;
    }
    #[cfg(windows)]
    hide_dir(&dir);
}

#[cfg(windows)]
fn hide_dir(path: &std::path::Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN};
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = SetFileAttributesW(
            windows::core::PCWSTR(wide.as_ptr()),
            FILE_ATTRIBUTE_HIDDEN,
        );
    }
}

// ── Load / Save ──────────────────────────────────────────────────────────────

pub fn load() -> Config {
    ensure_config_dir();
    setup_logs();
    let path = config_path();

    // First run after path change: pull config from old location
    #[cfg(not(debug_assertions))]
    if !path.exists() {
        if let Some(legacy) = legacy_config_path() {
            let _ = std::fs::copy(&legacy, &path);
        }
    }

    if !path.exists() {
        let cfg = Config::default();
        save(&cfg);
        return cfg;
    }

    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            eprintln!("Config parse error: {e}, using defaults");
            Config::default()
        }),
        Err(e) => {
            eprintln!("Config read error: {e}, using defaults");
            Config::default()
        }
    };

    let mut dirty = cfg.config_version < CONFIG_VERSION;
    if dirty {
        cfg.config_version = CONFIG_VERSION;
    }

    if cfg.preferred_res_x.is_none() || cfg.preferred_res_y.is_none() {
        let (w, h) = crate::platform::primary_monitor_resolution();
        cfg.preferred_res_x = Some(w);
        cfg.preferred_res_y = Some(h);
        dirty = true;
    }

    if dirty {
        save(&cfg);
    }

    cfg
}

pub fn save(cfg: &Config) {
    match serde_json::to_string_pretty(cfg) {
        Ok(s) => {
            if let Err(e) = std::fs::write(config_path(), s) {
                eprintln!("Config save error: {e}");
            }
        }
        Err(e) => eprintln!("Config serialize error: {e}"),
    }
}
