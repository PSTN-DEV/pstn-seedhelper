use anyhow::{bail, Context, Result};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

use crate::app::LogSender;
use crate::config::{self, Config};

const SQUAD_APP_ID: &str = "393380";

// ── Config validation ─────────────────────────────────────────────────────────

pub fn validate_config(cfg: &Config) -> Result<()> {
    if cfg.eco_mode {
        if !config::game_settings_path().exists() {
            bail!(
                "GameUserSettings.ini не найден: {:?}",
                config::game_settings_path()
            );
        }
        if find_squad_launcher().is_none() {
            bail!("squad_launcher.exe не найден — укажите путь в настройках или установите Squad через Steam");
        }
    }
    if cfg.steam_id.len() != 17 || !cfg.steam_id.chars().all(|c| c.is_ascii_digit()) {
        bail!("SteamID должен состоять из 17 цифр");
    }
    Ok(())
}

// ── INI management ────────────────────────────────────────────────────────────

fn settings_path() -> PathBuf {
    config::game_settings_path()
}

/// Write or remove the two FPS keys independently.
/// `None` removes the key (Squad resets to its own default); `Some(n)` writes `n.000000`.
pub fn write_fps_keys(fps: Option<u32>, menu_fps: Option<u32>) -> Result<()> {
    let path = settings_path();
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&path).context("Чтение INI")?;
    let content = content.trim_start_matches('\u{feff}');

    const TARGET_SECTION: &str = "[/Script/Squad.SQGameUserSettings]";
    let keys: [(&str, Option<u32>); 2] =
        [("FrameRateLimit", fps), ("MenuFrameRateLimit", menu_fps)];

    let mut out = Vec::<String>::new();
    let mut in_section = false;
    let mut found = std::collections::HashSet::new();
    let mut section_end_idx: Option<usize> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == TARGET_SECTION {
            in_section = true;
            out.push(line.to_owned());
            continue;
        }
        if in_section && trimmed.starts_with('[') && trimmed != TARGET_SECTION {
            in_section = false;
            section_end_idx = Some(out.len());
        }
        if in_section {
            if let Some(&(key, val)) = keys
                .iter()
                .find(|(k, _)| trimmed.starts_with(&format!("{k}=")))
            {
                if let Some(n) = val {
                    out.push(format!("{key}={:.6}", n as f64));
                    found.insert(key);
                }
                // None = skip (remove the key)
                continue;
            }
        }
        out.push(line.to_owned());
    }

    if in_section {
        section_end_idx = Some(out.len());
    }

    if let Some(idx) = section_end_idx {
        let missing: Vec<String> = keys
            .iter()
            .filter(|(k, v)| v.is_some() && !found.contains(k))
            .map(|(k, v)| format!("{k}={:.6}", v.unwrap() as f64))
            .collect();
        for (i, line) in missing.into_iter().enumerate() {
            out.insert(idx + i, line);
        }
    }

    std::fs::write(&path, out.join("\n")).context("Запись INI")?;
    Ok(())
}

/// Write or remove the four resolution keys.
/// `None` removes the key; `Some(n)` writes the integer value.
pub fn write_resolution_keys(res_x: Option<u32>, res_y: Option<u32>) -> Result<()> {
    let path = settings_path();
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&path).context("Чтение INI")?;
    let content = content.trim_start_matches('\u{feff}');

    const TARGET_SECTION: &str = "[/Script/Squad.SQGameUserSettings]";
    // Integer keys: written as plain integers
    let int_keys: [(&str, Option<u32>); 8] = [
        ("ResolutionSizeX", res_x),
        ("ResolutionSizeY", res_y),
        ("LastUserConfirmedResolutionSizeX", res_x),
        ("LastUserConfirmedResolutionSizeY", res_y),
        ("DesiredScreenWidth", res_x),
        ("DesiredScreenHeight", res_y),
        ("LastUserConfirmedDesiredScreenWidth", res_x),
        ("LastUserConfirmedDesiredScreenHeight", res_y),
    ];
    // Float keys: always reset to -1.000000 (Squad's "no recommendation" sentinel)
    const FLOAT_KEYS: [&str; 2] = ["LastRecommendedScreenWidth", "LastRecommendedScreenHeight"];
    const RESET_FLOAT: &str = "-1.000000";

    let mut out = Vec::<String>::new();
    let mut in_section = false;
    let mut found_int = std::collections::HashSet::new();
    let mut found_float = std::collections::HashSet::new();
    let mut section_end_idx: Option<usize> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == TARGET_SECTION {
            in_section = true;
            out.push(line.to_owned());
            continue;
        }
        if in_section && trimmed.starts_with('[') && trimmed != TARGET_SECTION {
            in_section = false;
            section_end_idx = Some(out.len());
        }
        if in_section {
            if let Some(&(key, val)) = int_keys
                .iter()
                .find(|(k, _)| trimmed.starts_with(&format!("{k}=")))
            {
                if let Some(n) = val {
                    out.push(format!("{key}={n}"));
                    found_int.insert(key);
                }
                continue;
            }
            if let Some(&key) = FLOAT_KEYS
                .iter()
                .find(|&&k| trimmed.starts_with(&format!("{k}=")))
            {
                out.push(format!("{key}={RESET_FLOAT}"));
                found_float.insert(key);
                continue;
            }
        }
        out.push(line.to_owned());
    }

    if in_section {
        section_end_idx = Some(out.len());
    }

    if let Some(idx) = section_end_idx {
        let mut missing: Vec<String> = int_keys
            .iter()
            .filter(|(k, v)| v.is_some() && !found_int.contains(k))
            .map(|(k, v)| format!("{k}={}", v.unwrap()))
            .collect();
        for key in FLOAT_KEYS.iter().filter(|&&k| !found_float.contains(k)) {
            missing.push(format!("{key}={RESET_FLOAT}"));
        }
        for (i, line) in missing.into_iter().enumerate() {
            out.insert(idx + i, line);
        }
    }

    std::fs::write(&path, out.join("\n")).context("Запись INI")?;
    Ok(())
}

/// Restore preferred FPS + resolution keys. Squad rewrites these on every map
/// change / server join, so this must run after the game process is killed.
pub fn restore_ini_keys(cfg: &Config) {
    let _ = write_fps_keys(cfg.preferred_fps, cfg.preferred_menu_fps);
    let _ = write_resolution_keys(cfg.preferred_res_x, cfg.preferred_res_y);
}

// ── Squad install dir detection ───────────────────────────────────────────────

/// Find Squad install dir via Steam registry → libraryfolders.vdf.
pub fn find_squad_dir() -> Option<PathBuf> {
    detect_squad_via_steam()
}

/// Find squad_launcher.exe via Steam registry → libraryfolders.vdf.
pub fn find_squad_launcher() -> Option<PathBuf> {
    let squad_dir = detect_squad_via_steam()?;
    let exe = squad_dir.join("squad_launcher.exe");
    exe.exists().then_some(exe)
}

fn detect_squad_via_steam() -> Option<PathBuf> {
    let out = {
        let mut cmd = std::process::Command::new("reg");
        cmd.args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"]);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        cmd.output().ok()?
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let steam_path = stdout
        .lines()
        .find(|l| l.contains("SteamPath"))?
        .split("REG_SZ")
        .nth(1)?
        .trim()
        .to_owned();

    let default_squad = PathBuf::from(&steam_path)
        .join("steamapps")
        .join("common")
        .join("Squad");
    if default_squad.exists() {
        return Some(default_squad);
    }

    let vdf = std::fs::read_to_string(
        PathBuf::from(&steam_path)
            .join("steamapps")
            .join("libraryfolders.vdf"),
    )
    .ok()?;
    for line in vdf.lines() {
        let t = line.trim();
        if t.starts_with("\"path\"") {
            if let Some(p) = t.split('"').nth(3) {
                let squad = PathBuf::from(p)
                    .join("steamapps")
                    .join("common")
                    .join("Squad");
                if squad.exists() {
                    return Some(squad);
                }
            }
        }
    }
    None
}

// ── Welcome video ─────────────────────────────────────────────────────────────

pub fn remove_welcome_video(log: &LogSender) {
    let squad_dir = match find_squad_dir() {
        Some(d) => d,
        None => {
            let _ = log.send("Не удалось найти папку Squad для удаления видео".into());
            return;
        }
    };
    let video = squad_dir
        .join("SquadGame")
        .join("Content")
        .join("Movies")
        .join("welcome_to_squad.mp4");

    if video.exists() {
        if let Err(e) = std::fs::remove_file(&video) {
            let _ = log.send(format!("Не удалось удалить видео: {e}"));
        } else {
            let _ = log.send("Удалено welcome_to_squad.mp4".into());
        }
    }
}

// ── Game launch ───────────────────────────────────────────────────────────────

/// Build steam://run/<id>//<space-separated args>/ URL.
fn steam_url(args: &[&str]) -> String {
    if args.is_empty() {
        format!("steam://run/{SQUAD_APP_ID}//")
    } else {
        format!("steam://run/{SQUAD_APP_ID}//{}/", args.join("%20"))
    }
}

/// Eco-mode: launch via squad_launcher.exe directly (avoids Steam confirmation dialog),
/// set fps=6 before launch, restore preferred fps after 10 s, then show toast.
pub async fn launch_game_eco(
    cfg: &Config,
    token: &CancellationToken,
    log: &LogSender,
) -> Result<()> {
    let mut args: Vec<&str> = Vec::new();

    if cfg.render_toggle {
        args.extend([
            "-nullrhi",
            "-NoAsyncPostLoad",
            "-noshaderworker",
            "-norenderthread",
            "-NoShaderCompile",
            "-log",
            "-nosplash",
        ]);
        let _ = log.send("Squad запущен без рендера. ОПАСНО!".into());
    } else {
        args.extend(["-windowed", "-ResX=1", "-ResY=1"]);
        let _ = log.send("Squad запущен в эко режиме (окно 1×1)".into());
    }
    if cfg.disable_sound {
        args.push("-nosound");
    }

    let launcher = find_squad_launcher()
        .context("squad_launcher.exe не найден — укажите путь в настройках")?;

    if !cfg.render_toggle {
        write_fps_keys(Some(6), Some(100))?;
    }
    std::process::Command::new(&launcher)
        .args(&args)
        .spawn()
        .context("Ошибка запуска Squad")?;

    if !cfg.render_toggle {
        let _ = log.send("Ждём 10 секунд — игра читает настройки...".into());
        tokio::select! {
            _ = sleep(Duration::from_secs(10)) => {}
            _ = token.cancelled() => {
                let _ = write_fps_keys(cfg.preferred_fps, cfg.preferred_menu_fps);
                let _ = write_resolution_keys(cfg.preferred_res_x, cfg.preferred_res_y);
                let _ = log.send("\x00restore_toast".into());
                return Ok(());
            }
        }

        write_fps_keys(cfg.preferred_fps, cfg.preferred_menu_fps)?;
        write_resolution_keys(cfg.preferred_res_x, cfg.preferred_res_y)?;
        let _ = log.send("\x00restore_toast".into());
    }

    let delay_secs = (cfg.game_launch_delay as u64)
        .saturating_mul(60)
        .saturating_sub(10);
    if delay_secs > 0 {
        let _ = log.send(format!("Ждём загрузки игры (ещё {delay_secs} сек)..."));
        tokio::select! {
            _ = sleep(Duration::from_secs(delay_secs)) => {}
            _ = token.cancelled() => {}
        }
    }

    Ok(())
}

/// Non-eco: launch Squad via Steam browser protocol, or via squad_launcher when -nosound is needed.
pub async fn launch_game_steam(
    cfg: &Config,
    token: &CancellationToken,
    log: &LogSender,
) -> Result<()> {
    if cfg.disable_sound {
        // Steam URL with launch args triggers a confirmation dialog; use launcher directly instead.
        let launcher = find_squad_launcher()
            .context("squad_launcher.exe не найден — укажите путь в настройках")?;
        std::process::Command::new(&launcher)
            .arg("-nosound")
            .spawn()
            .context("Ошибка запуска Squad")?;
        let _ = log.send("Squad запущен с -nosound".into());
    } else {
        open_steam_url(&steam_url(&[]))?;
        let _ = log.send("Squad запущен через Steam".into());
    }

    let delay_secs = cfg.game_launch_delay as u64 * 60;
    if delay_secs > 0 {
        let _ = log.send(format!("Ждём загрузки игры ({delay_secs} сек)..."));
        tokio::select! {
            _ = sleep(Duration::from_secs(delay_secs)) => {}
            _ = token.cancelled() => {}
        }
    }

    Ok(())
}

// ── Steam URL ─────────────────────────────────────────────────────────────────

pub fn open_steam_url(url: &str) -> Result<()> {
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/c", "start", "", url]);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    cmd.spawn().context("Ошибка открытия steam URL")?;
    Ok(())
}
