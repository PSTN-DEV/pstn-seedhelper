use std::sync::Arc;
use crate::app::AppState;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Called once on startup. Auto-applies if enabled, otherwise sets the UI update notice.
pub async fn check(state: &Arc<AppState>) {
    match state.api.get_latest_version().await {
        Ok(remote) if is_newer(&remote, CURRENT_VERSION) => {
            let auto = state.config.lock().unwrap().auto_update;
            if auto {
                let _ = state.log.send(format!("Авто-обновление до v{remote}..."));
                apply(state, true).await;
                return;
            }
            let notice = format!("Доступно обновление v{remote}  (текущая v{CURRENT_VERSION})");
            let _ = state.window.upgrade_in_event_loop(move |w| {
                w.set_update_notice(notice.into());
            });
        }
        Ok(_) => {} // already up to date
        Err(e) => { let _ = state.log.send(format!("Не удалось проверить обновление: {e}")); }
    }
}

/// Download the installer and launch it, then exit.
/// `silent = true`  → passes /VERYSILENT (auto-update, no UI, app relaunches after install).
/// `silent = false` → no flags (user sees the normal install wizard).
pub async fn apply(state: &Arc<AppState>, silent: bool) {
    let _ = state.log.send("Скачиваем установщик обновления...".into());

    let bytes: Vec<u8> = match state.api.download_update().await {
        Ok(b) => b,
        Err(e) => {
            let _ = state.log.send(format!("Ошибка скачивания: {e}"));
            return;
        }
    };

    let installer = std::env::temp_dir().join("pstn-seedhelper-setup.exe");
    if let Err(e) = std::fs::write(&installer, &bytes) {
        let _ = state.log.send(format!("Ошибка записи установщика: {e}"));
        return;
    }

    let _ = state.log.send("Запускаем установщик...".into());
    let mut cmd = std::process::Command::new(&installer);
    if silent { cmd.arg("/VERYSILENT"); }
    match cmd.spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => { let _ = state.log.send(format!("Не удалось запустить установщик: {e}")); }
    }
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
