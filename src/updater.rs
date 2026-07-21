use std::sync::Arc;
use crate::app::AppState;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Called once on startup. Sets the update notice in the UI if a newer version is available.
pub async fn check(state: &Arc<AppState>) {
    match state.api.get_latest_version().await {
        Ok(remote) if is_newer(&remote, CURRENT_VERSION) => {
            let notice = format!("Доступно обновление v{remote}  (текущая v{CURRENT_VERSION})");
            let _ = state.window.upgrade_in_event_loop(move |w| {
                w.set_update_notice(notice.into());
            });
        }
        Ok(_) => {} // already up to date
        Err(e) => log::warn!("Не удалось проверить обновление: {e}"),
    }
}

/// Download and self-replace. Spawns a helper bat then exits.
pub async fn apply(state: &Arc<AppState>) {
    let _ = state.log.send("[info] Скачиваем обновление...".into());

    let bytes: Vec<u8> = match state.api.download_update().await {
        Ok(b) => b,
        Err(e) => {
            let _ = state.log.send(format!("[error] Ошибка скачивания: {e}"));
            return;
        }
    };

    let tmp = std::env::temp_dir().join("seed_helper_update.exe");
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        let _ = state.log.send(format!("[error] Ошибка записи обновления: {e}"));
        return;
    }

    // Write a bat that waits 2s, replaces the exe, then launches the new version
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => { let _ = state.log.send(format!("[error] {e}")); return; }
    };

    let bat = std::env::temp_dir().join("seed_helper_update.bat");
    let bat_content = format!(
        "@echo off\ntimeout /t 2 /nobreak >nul\nmove /y \"{}\" \"{}\"\nstart \"\" \"{}\"\ndel \"%~f0\"\n",
        tmp.display(), current_exe.display(), current_exe.display()
    );

    if let Err(e) = std::fs::write(&bat, bat_content) {
        let _ = state.log.send(format!("[error] Ошибка записи bat: {e}"));
        return;
    }

    let _ = state.log.send("[info] Перезапускаем для применения обновления...".into());
    let _ = std::process::Command::new("cmd")
        .args(["/c", &bat.to_string_lossy()])
        .spawn();

    std::process::exit(0);
}

/// Simple semver comparison: returns true if `remote` > `current`.
fn is_newer(remote: &str, current: &str) -> bool {
    fn parse(s: &str) -> (u32, u32, u32) {
        let mut parts = s.trim_start_matches('v').splitn(3, '.');
        let a: u32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let b: u32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let c: u32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        (a, b, c)
    }
    parse(remote) > parse(current)
}
